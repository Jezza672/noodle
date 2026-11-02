//! Recording automation: moves of an armed Automation node, or a signal
//! wired into its `record` input, written into its lane.
//!
//! The recorder is plain data fed with ticks, so it needs no clock. The
//! session feeds it while the transport plays: every move of an armed node's
//! `value`, the playhead each frame, and the reading of a wired `record`
//! input. Nothing touches the project until the Pass ends (the transport
//! stops); then [`Recorder::finish`] makes one `Command::Batch` of lanes, so
//! the whole Pass, however many nodes wrote, is one undo step.

use std::collections::{BTreeMap, BTreeSet};

use noodle_core::{
    AutomationLane, AutomationPoint, Command, Curve, Endpoint, NodeId, Project, TICKS_PER_QUARTER,
    Tick,
};
use serde::{Deserialize, Serialize};

/// The parameter a lane automates and a move records.
pub const VALUE: &str = noodle_nodes::AUTOMATION_VALUE;
/// The input a wired signal is recorded from.
pub const RECORD: &str = noodle_nodes::AUTOMATION_RECORD;

/// How a Pass treats moves. One mode for every node, saved as a preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutomationMode {
    /// Plays the lane back; nothing is written, armed or not.
    Read,
    /// Writes while the node is being moved, and goes back to the lane when
    /// it is let go.
    #[default]
    Touch,
    /// Writes from the first move to the end of the Pass, holding the last
    /// value once it is let go.
    Latch,
}

impl AutomationMode {
    pub const ALL: [Self; 3] = [Self::Read, Self::Touch, Self::Latch];

    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Touch => "Touch",
            Self::Latch => "Latch",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Read => "Automation plays back; armed nodes write nothing",
            Self::Touch => {
                "Armed nodes write while you move them, and go back to the lane when you let go"
            }
            Self::Latch => {
                "Armed nodes write from your first move to the end, holding the last value"
            }
        }
    }
}

/// Recorded points closer than this to the line between their neighbours
/// (in the lane's 0 to 1) are dropped.
pub const TOLERANCE: f32 = 0.004;

/// A wired signal that has not changed for this long is no longer "moved"
/// (Touch lets go).
const STILL: i64 = TICKS_PER_QUARTER / 2;

/// A playhead this far back is a loop wrap or a seek.
const WRAP: i64 = TICKS_PER_QUARTER / 4;

/// A stretch of one node's lane being written.
#[derive(Clone, Debug)]
struct Segment {
    start: Tick,
    end: Tick,
    points: Vec<(Tick, f32)>,
}

#[derive(Debug, Default)]
struct NodePass {
    segments: Vec<Segment>,
    open: Option<Segment>,
    /// The last value written, held while a segment stays open.
    held: Option<f32>,
    /// Latch has been triggered.
    latched: bool,
    /// A gesture (a drag, a slider) is moving the node.
    touching: bool,
    /// The last value the `record` input had, and when it last changed.
    signal: Option<(f32, Tick)>,
}

impl NodePass {
    fn write(&mut self, tick: Tick, value: f32) {
        self.held = Some(value);
        let segment = self.open.get_or_insert_with(|| Segment {
            start: tick,
            end: tick,
            points: Vec::new(),
        });
        segment.end = tick;
        segment.points.push((tick, value));
    }

    fn close(&mut self, tick: Tick) {
        if let (Some(mut segment), Some(held)) = (self.open.take(), self.held) {
            if tick > segment.end {
                segment.points.push((tick, held));
                segment.end = tick;
            }
            self.segments.push(segment);
        }
    }
}

/// The arming of nodes, and the Pass in progress.
#[derive(Debug, Default)]
pub struct Recorder {
    armed: BTreeSet<NodeId>,
    nodes: BTreeMap<NodeId, NodePass>,
    last: Option<Tick>,
}

impl Recorder {
    pub fn is_armed(&self, node: NodeId) -> bool {
        self.armed.contains(&node)
    }

    pub fn armed(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.armed.iter().copied()
    }

    pub fn arm(&mut self, node: NodeId, on: bool) {
        if on {
            self.armed.insert(node);
        } else {
            self.armed.remove(&node);
            self.nodes.remove(&node);
        }
    }

    /// Forgets nodes that no longer exist.
    pub fn retain(&mut self, mut exists: impl FnMut(NodeId) -> bool) {
        self.armed.retain(|&n| exists(n));
        self.nodes.retain(|&n, _| exists(n));
    }

    /// Whether a Pass has written anything yet.
    pub fn has_pass(&self) -> bool {
        !self.nodes.is_empty()
    }

    /// The value an armed node is being held at, if a Pass is writing it.
    pub fn held(&self, node: NodeId) -> Option<f32> {
        self.nodes
            .get(&node)
            .filter(|n| n.open.is_some())
            .and_then(|n| n.held)
    }

    /// A move of an armed node's `value` to `value` (0 to 1) at `tick`.
    /// Says whether the recorder took it: when it did, the move is not
    /// applied to the node.
    pub fn moved(&mut self, mode: AutomationMode, node: NodeId, value: f32, tick: Tick) -> bool {
        if mode == AutomationMode::Read || !self.is_armed(node) {
            return false;
        }
        let value = unit(value);
        let pass = self.nodes.entry(node).or_default();
        pass.touching = true;
        pass.latched |= mode == AutomationMode::Latch;
        pass.write(tick, value);
        true
    }

    /// The gesture moving the nodes was let go.
    pub fn released(&mut self, mode: AutomationMode, tick: Tick) {
        for pass in self.nodes.values_mut() {
            if std::mem::take(&mut pass.touching) && mode != AutomationMode::Latch {
                pass.close(tick);
            }
        }
    }

    /// The reading of a wired `record` input at `tick`.
    pub fn signal(&mut self, mode: AutomationMode, node: NodeId, value: f32, tick: Tick) {
        if mode == AutomationMode::Read || !self.is_armed(node) || !value.is_finite() {
            return;
        }
        let value = unit(value);
        let pass = self.nodes.entry(node).or_default();
        match pass.signal {
            Some((seen, _)) if seen == value => {
                // Still: Touch lets go after a while.
                if let Some((_, since)) = pass.signal
                    && mode == AutomationMode::Touch
                    && !pass.touching
                    && tick.0 - since.0 > STILL
                {
                    pass.close(tick);
                }
            }
            Some(_) => {
                pass.signal = Some((value, tick));
                pass.latched |= mode == AutomationMode::Latch;
                pass.write(tick, value);
            }
            // The first reading is where the signal is, not a move.
            None => pass.signal = Some((value, tick)),
        }
    }

    /// The playhead, once a frame while the transport plays. Keeps held
    /// values written, and closes the stretch being written at a loop wrap
    /// or a seek (Latch and a held gesture carry on from the new place).
    pub fn advance(&mut self, tick: Tick) {
        let wrapped = self.last.is_some_and(|last| tick.0 + WRAP < last.0);
        let last = self.last.unwrap_or(tick);
        self.last = Some(tick);
        for pass in self.nodes.values_mut() {
            if wrapped {
                pass.close(last);
            }
            let carry = pass.latched || pass.touching;
            let writing = pass.open.is_some() || (wrapped && carry);
            if let (true, Some(held)) = (writing, pass.held) {
                pass.write(tick, held);
            }
        }
    }

    /// Ends the Pass at `tick`, and gives the one command that writes it
    /// into the lanes, if anything was written.
    pub fn finish(&mut self, project: &Project, tick: Tick) -> Option<Command> {
        let mut commands = Vec::new();
        let mut fresh = project.next_lane_id();
        for (&node, pass) in &mut self.nodes {
            pass.close(tick);
            pass.touching = false;
            pass.latched = false;
            pass.held = None;
            pass.signal = None;
            if pass.segments.is_empty() {
                continue;
            }
            let target = Endpoint::new(node, VALUE);
            let existing = project.lane_for(&target);
            let mut points = existing.map_or_else(Vec::new, |(_, l)| l.points.clone());
            for segment in std::mem::take(&mut pass.segments) {
                points.retain(|p| p.tick < segment.start || p.tick > segment.end);
                points.extend(thin(&segment.points).into_iter().map(|(tick, value)| {
                    AutomationPoint {
                        tick,
                        value,
                        curve: Curve::Linear,
                    }
                }));
                points.sort_by_key(|p| p.tick);
            }
            match existing {
                Some((id, lane)) => commands.push(Command::SetLane {
                    id,
                    lane: AutomationLane::new(lane.target.clone(), points),
                }),
                None => {
                    commands.push(Command::AddLane {
                        id: fresh,
                        lane: AutomationLane::new(target, points),
                    });
                    fresh.0 += 1;
                }
            }
        }
        self.nodes.clear();
        self.last = None;
        match commands.len() {
            0 => None,
            1 => commands.pop(),
            _ => Some(Command::Batch(commands)),
        }
    }
}

fn unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// `points` (in time order, ticks may repeat) cut down to those that matter:
/// one per tick (the last), then every point within [`TOLERANCE`] of the
/// straight line between the points kept either side dropped.
fn thin(points: &[(Tick, f32)]) -> Vec<(Tick, f32)> {
    let mut unique: Vec<(Tick, f32)> = Vec::with_capacity(points.len());
    for &(tick, value) in points {
        match unique.last_mut() {
            Some(last) if last.0 == tick => last.1 = value,
            _ => unique.push((tick, value)),
        }
    }
    if unique.len() < 3 {
        return unique;
    }
    let mut keep = vec![false; unique.len()];
    keep[0] = true;
    keep[unique.len() - 1] = true;
    let mut stack = vec![(0, unique.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (ta, va) = (unique[a].0.0 as f64, f64::from(unique[a].1));
        let (tb, vb) = (unique[b].0.0 as f64, f64::from(unique[b].1));
        let mut worst = (0.0_f64, None);
        for (i, &(tick, value)) in unique.iter().enumerate().take(b).skip(a + 1) {
            let along = (tick.0 as f64 - ta) / (tb - ta);
            let off = (f64::from(value) - (va + (vb - va) * along)).abs();
            if off > worst.0 {
                worst = (off, Some(i));
            }
        }
        if let (off, Some(i)) = worst
            && off > f64::from(TOLERANCE)
        {
            keep[i] = true;
            stack.push((a, i));
            stack.push((i, b));
        }
    }
    unique
        .into_iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_core::Node;

    fn setup(points: &[(i64, f32)]) -> (Session, NodeId) {
        let mut session = Session::new(Nodes::all());
        let node = session.new_node_id();
        let made = Node::new(noodle_nodes::AUTOMATION_NODE_ID);
        session.edit([Edit::Apply(Command::AddNode {
            id: node,
            node: made,
        })]);
        let id = session.project().next_lane_id();
        let points = points
            .iter()
            .map(|&(t, v)| AutomationPoint {
                tick: Tick(t),
                value: v,
                curve: Curve::Linear,
            })
            .collect();
        let lane = AutomationLane::new(Endpoint::new(node, VALUE), points);
        session.edit([Edit::Apply(Command::AddLane { id, lane })]);
        (session, node)
    }

    fn finish(rec: &mut Recorder, session: &mut Session, tick: i64) {
        let command = rec.finish(session.project(), Tick(tick)).unwrap();
        session.edit([Edit::Apply(command)]);
    }

    fn lane_points(session: &Session, n: NodeId) -> Vec<(i64, f32)> {
        let (_, lane) = session
            .project()
            .lane_for(&Endpoint::new(n, VALUE))
            .unwrap();
        lane.points.iter().map(|p| (p.tick.0, p.value)).collect()
    }

    #[test]
    fn thinning_drops_points_on_the_line_and_keeps_corners() {
        let line: Vec<_> = (0..10).map(|i| (Tick(i * 10), i as f32 / 10.0)).collect();
        assert_eq!(thin(&line).len(), 2);
        let mut corner = line.clone();
        corner.extend((10..20).map(|i| (Tick(i * 10), 0.9 - (i - 10) as f32 / 10.0)));
        let kept = thin(&corner);
        assert!(kept.len() >= 3 && kept.len() <= 4, "{kept:?}");
        // Repeated ticks collapse to the last value.
        assert_eq!(
            thin(&[(Tick(5), 0.1), (Tick(5), 0.7)]),
            vec![(Tick(5), 0.7)]
        );
    }

    #[test]
    fn touch_writes_while_moved_and_leaves_the_rest_of_the_lane() {
        let (mut session, n) = setup(&[(0, 0.2), (1000, 0.2)]);
        let mut rec = Recorder::default();
        rec.arm(n, true);
        let mode = AutomationMode::Touch;
        for (t, v) in [(100, 0.5), (200, 0.8), (300, 0.8)] {
            assert!(rec.moved(mode, n, v, Tick(t)));
            rec.advance(Tick(t));
        }
        rec.released(mode, Tick(300));
        rec.advance(Tick(500));
        finish(&mut rec, &mut session, 600);
        let points = lane_points(&session, n);
        assert_eq!(points.first(), Some(&(0, 0.2)));
        assert_eq!(points.last(), Some(&(1000, 0.2)));
        assert!(points.contains(&(100, 0.5)), "{points:?}");
        assert!(points.contains(&(200, 0.8)), "{points:?}");
        // Let go at 300: nothing written after it.
        assert!(points.iter().all(|&(t, _)| t <= 300 || t == 1000));
    }

    #[test]
    fn latch_holds_the_last_value_to_the_end_of_the_pass() {
        let (mut session, n) = setup(&[(0, 0.2), (1000, 0.2)]);
        let mut rec = Recorder::default();
        rec.arm(n, true);
        let mode = AutomationMode::Latch;
        assert!(rec.moved(mode, n, 0.9, Tick(100)));
        rec.released(mode, Tick(150));
        rec.advance(Tick(400));
        finish(&mut rec, &mut session, 700);
        let points = lane_points(&session, n);
        assert!(points.contains(&(100, 0.9)));
        assert!(points.contains(&(700, 0.9)), "{points:?}");
        // The old point at 1000 is outside the pass and stays.
        assert_eq!(points.last(), Some(&(1000, 0.2)));
    }

    #[test]
    fn read_mode_and_unarmed_nodes_write_nothing() {
        let (session, n) = setup(&[(0, 0.2)]);
        let mut rec = Recorder::default();
        rec.arm(n, true);
        assert!(!rec.moved(AutomationMode::Read, n, 0.5, Tick(10)));
        rec.arm(n, false);
        assert!(!rec.moved(AutomationMode::Touch, n, 0.5, Tick(10)));
        assert!(rec.finish(session.project(), Tick(100)).is_none());
    }

    #[test]
    fn a_loop_wrap_keeps_latching_and_a_later_cycle_overwrites() {
        let (mut session, n) = setup(&[(0, 0.0)]);
        let mut rec = Recorder::default();
        rec.arm(n, true);
        let mode = AutomationMode::Latch;
        rec.moved(mode, n, 0.4, Tick(100));
        rec.advance(Tick(400));
        // The loop wraps from 400 back to 0, and the value is moved again.
        rec.advance(Tick(0));
        rec.moved(mode, n, 0.9, Tick(50));
        rec.advance(Tick(400));
        finish(&mut rec, &mut session, 400);
        let points = lane_points(&session, n);
        assert!(!points.contains(&(100, 0.4)), "overwritten: {points:?}");
        assert!(points.contains(&(50, 0.9)) && points.contains(&(400, 0.9)));
    }

    #[test]
    fn a_wired_signal_is_recorded_when_it_changes_and_touch_lets_go_when_still() {
        let (mut session, n) = setup(&[(0, 0.5)]);
        let mut rec = Recorder::default();
        rec.arm(n, true);
        let mode = AutomationMode::Touch;
        // The first reading is only noted.
        rec.signal(mode, n, 0.5, Tick(0));
        assert!(rec.held(n).is_none());
        for (i, v) in [0.6, 0.8, 1.0].into_iter().enumerate() {
            rec.signal(mode, n, v, Tick(100 * (i as i64 + 1)));
        }
        // Held still past the limit: let go, then nothing more is written.
        rec.signal(mode, n, 1.0, Tick(300 + STILL + 10));
        rec.signal(mode, n, 1.0, Tick(2000));
        finish(&mut rec, &mut session, 2000);
        let points = lane_points(&session, n);
        assert!(points.contains(&(300, 1.0)), "{points:?}");
        assert!(
            points.iter().all(|&(t, _)| t <= 300 + STILL + 10),
            "{points:?}"
        );
    }

    #[test]
    fn a_node_without_a_lane_gets_one_and_several_nodes_are_one_command() {
        let (session, n) = setup(&[(0, 0.2)]);
        let mut rec = Recorder::default();
        let other = NodeId(9_999);
        rec.arm(n, true);
        rec.arm(other, true);
        rec.moved(AutomationMode::Touch, n, 0.5, Tick(10));
        rec.moved(AutomationMode::Touch, other, 0.5, Tick(10));
        let Some(Command::Batch(commands)) = rec.finish(session.project(), Tick(20)) else {
            panic!("one batch")
        };
        assert!(matches!(commands[0], Command::SetLane { .. }));
        assert!(matches!(commands[1], Command::AddLane { .. }));
    }
}

/// The Pass through a running session, on the virtual output.
#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_core::{Node, spare};
    use noodle_engine::{OUTPUT_ID, Registry, Settings};
    use noodle_nodes::{LFO_ID, SVF_ID};
    use std::time::Duration;

    fn add(session: &mut Session, node: Node) -> NodeId {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        id
    }

    fn wire(session: &mut Session, from: (NodeId, &str), to: (NodeId, &str)) {
        let wire = spare::wire(Endpoint::new(from.0, from.1), Endpoint::new(to.0, to.1));
        session.edit([Edit::Apply(wire)]);
    }

    /// A saw through a filter to the output; returns the cutoff made
    /// automatable, with its Automation node.
    fn automated_filter(session: &mut Session) -> NodeId {
        let saw = add(session, Node::new("noodle.osc.saw"));
        let filter = add(session, Node::new(SVF_ID).with_param("cutoff", 400.0));
        let output = add(session, Node::new(OUTPUT_ID));
        wire(session, (saw, "out"), (filter, "in"));
        wire(session, (filter, "low"), (output, "in"));
        session.edit([Edit::Automate(Endpoint::new(filter, "cutoff"))]);
        session
            .project()
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == noodle_nodes::AUTOMATION_NODE_ID)
            .map(|(id, _)| id)
            .unwrap()
    }

    fn level(session: &Session, from: usize) -> f32 {
        let mut registry = Registry::with_builtins();
        let _library = noodle_nodes::register_all(&mut registry);
        let settings = Settings {
            sample_rate: 48_000.0,
            max_frames: 512,
            channels: 2,
        };
        let rendered =
            noodle_engine::render_project(session.project(), &registry, settings, 96_000).unwrap();
        let tail = &rendered.samples[from..];
        (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
    }

    #[test]
    fn riding_an_armed_node_while_playing_writes_the_lane_in_one_undo_step() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(crate::testkit::virtual_audio());
        let node = automated_filter(&mut session);
        let filter = session
            .project()
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == noodle_nodes::SVF_ID)
            .map(|(id, _)| id)
            .unwrap();
        let before = level(&session, 48_000);
        session.set_automation_mode(AutomationMode::Touch);
        session.edit([Edit::ArmAutomation { node, on: true }]);
        session.play();
        assert!(session.transport_running(), "{:?}", session.message());

        // Ride the value up to the top: a drag, a frame at a time.
        let steps = [0.6, 0.8, 1.0, 1.0];
        for value in steps {
            session.edit([Edit::Drag(Command::SetParam {
                node,
                key: VALUE.into(),
                value: Some(value),
            })]);
            std::thread::sleep(Duration::from_millis(60));
            session.maintain();
        }
        // Mid-pass the filter's cutoff signal follows the held value, not
        // the lane (which still holds 0.5, an offset of 0: 400 Hz): the move is heard.
        std::thread::sleep(Duration::from_millis(100));
        session.maintain();
        let cutoff = session
            .telemetry()
            .read_param(filter, "cutoff")
            .expect("the cutoff is wired and reporting");
        assert!(cutoff.value > 600.0, "{cutoff:?}");
        session.edit([Edit::EndDrag]);
        let history = session.can_undo();
        session.set_transport_running(false);

        let (_, lane) = session
            .project()
            .lane_for(&Endpoint::new(node, VALUE))
            .unwrap();
        assert!(lane.points.len() >= 3, "{:?}", lane.points);
        assert!(lane.points.iter().any(|p| p.value == 1.0));
        assert!(lane.points.windows(2).all(|w| w[0].tick < w[1].tick));

        // The cutoff offset rides to the top of its travel: brighter.
        let after = level(&session, 48_000);
        assert!(after > before * 1.05, "{before} -> {after}");

        // The whole Pass is one undo step.
        assert!(history);
        session.undo();
        let (_, lane) = session
            .project()
            .lane_for(&Endpoint::new(node, VALUE))
            .unwrap();
        assert_eq!(lane.points.len(), 1);
        assert_eq!(level(&session, 48_000), before);
    }

    #[test]
    fn a_wired_lfo_is_printed_into_the_lane_through_record() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(crate::testkit::virtual_audio());
        let node = automated_filter(&mut session);
        // A fast sine, centred at one half and a half deep: 0 to 1.
        let lfo = add(
            &mut session,
            Node::new(LFO_ID)
                .with_param("rate", 4.0)
                .with_param("depth", 0.5)
                .with_param("offset", 0.5),
        );
        wire(&mut session, (lfo, "out"), (node, RECORD));
        session.set_automation_mode(AutomationMode::Latch);
        session.edit([Edit::ArmAutomation { node, on: true }]);
        session.play();
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(25));
            session.maintain();
        }
        session.set_transport_running(false);
        let (_, lane) = session
            .project()
            .lane_for(&Endpoint::new(node, VALUE))
            .unwrap();
        let values: Vec<f32> = lane.points.iter().map(|p| p.value).collect();
        assert!(values.len() > 4, "{values:?}");
        let (low, high) = values
            .iter()
            .fold((1.0_f32, 0.0_f32), |(l, h), &v| (l.min(v), h.max(v)));
        assert!(high - low > 0.5, "the sine swings: {low}..{high}");
    }
}
