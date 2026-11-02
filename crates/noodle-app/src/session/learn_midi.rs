//! Learn MIDI: a graph shortcut, like Automate. Choose it on a parameter,
//! move a control, and a MIDI Control node set to that controller and port
//! appears in the parameter's group, wired into the parameter, as one undo
//! step. Nothing else links the control and the parameter: the mapping is
//! the node and its wire.
//!
//! On an Automation node the wire goes into its `record` input, so the knob
//! records into the lane when the node is armed.

use noodle_core::{Command, Config, Endpoint, Node, NodeId, Project, Value, spare};
use noodle_engine::{InputKind, Registry, Taper};
use noodle_io::Heard;
use noodle_nodes::{
    AUTOMATION_NODE_ID, AUTOMATION_RECORD, AUTOMATION_VALUE, CONTROL_CONTROL, CONTROL_MAX,
    CONTROL_MIN, CONTROL_PITCH_BEND, CONTROL_PRESSURE, CONTROL_START, CONTROL_TAPER,
    MIDI_CONTROL_ID, MIDI_PORT_KEY,
};

use super::Session;
use crate::widgets::param::LearnMidi;

/// How far left of its target the new node sits.
const OFFSET_X: f32 = 260.0;

/// The input a learned control is wired into: the parameter itself, or the
/// `record` input for an Automation node's `value`.
pub fn destination(project: &Project, target: &Endpoint) -> Endpoint {
    let is_automation = project
        .graph()
        .node(target.node)
        .is_some_and(|n| n.type_id == AUTOMATION_NODE_ID);
    if is_automation && target.port == AUTOMATION_VALUE {
        Endpoint::new(target.node, AUTOMATION_RECORD)
    } else {
        target.clone()
    }
}

/// Why Learn MIDI can't be used on `target`, for the menu entry's tooltip.
pub fn refusal(project: &Project, target: &Endpoint) -> Option<&'static str> {
    let destination = destination(project, target);
    if project.graph().source(&destination).is_some() {
        Some("This parameter already has a wire")
    } else if destination == *target && project.lane_for(target).is_some() {
        Some("This parameter already has an automation lane")
    } else {
        None
    }
}

/// The controller a message moves, as the MIDI Control node's `control`
/// number: a controller (the low half of a pair counts as its high half),
/// 128 for pitch bend, 129 for channel pressure. Notes, program changes and
/// the channel mode messages (120 and up) aren't controls.
pub fn control_of(message: [u8; 3]) -> Option<f32> {
    match message[0] & 0xF0 {
        0xB0 => match message[1] {
            index @ 0..=31 => Some(f32::from(index)),
            index @ 32..=63 => Some(f32::from(index - 32)),
            index @ 64..=119 => Some(f32::from(index)),
            _ => None,
        },
        0xE0 => Some(CONTROL_PITCH_BEND),
        0xD0 => Some(CONTROL_PRESSURE),
        _ => None,
    }
}

/// The command that learns `heard` onto `target`, with `node` as the new
/// node's ID.
pub fn command(
    project: &Project,
    registry: &Registry,
    target: &Endpoint,
    node: NodeId,
    heard: &Heard,
) -> Result<Command, String> {
    let target = destination(project, target);
    if let Some(reason) = refusal(project, &target) {
        return Err(reason.to_owned());
    }
    let control = control_of(heard.1).ok_or("that isn't a control")?;
    let graph = project.graph();
    let target_node = graph.node(target.node).ok_or("the node is gone")?;
    let layout = registry
        .get(&target_node.type_id)
        .ok_or("the node's type is unknown")?
        .layout(&target_node.config)
        .map_err(|e| e.to_string())?;
    let input = layout
        .inputs
        .iter()
        .find(|i| i.key == target.port)
        .ok_or("the parameter is gone")?;
    let InputKind::Param(info) = &input.kind else {
        return Err("only a parameter can be learned".to_owned());
    };
    let current = target_node
        .params
        .get(&target.port)
        .copied()
        .unwrap_or(info.default);
    let (min, max, log, start) = super::automate::range_and_start(info, current);
    debug_assert!(!log || info.taper == Taper::Log);

    let port = heard
        .0
        .clone()
        .unwrap_or_else(|| noodle_io::ALL_INPUTS.to_owned());
    let mut learned = Node::new(MIDI_CONTROL_ID)
        .at(target_node.position.x - OFFSET_X, target_node.position.y)
        .with_config(Config::new().with(MIDI_PORT_KEY, Value::Text(port)))
        .with_param(CONTROL_CONTROL, control);
    learned.parent = target_node.parent;
    // Parameters left at their defaults aren't stored.
    if min != 0.0 {
        learned = learned.with_param(CONTROL_MIN, min);
    }
    if max != 1.0 {
        learned = learned.with_param(CONTROL_MAX, max);
    }
    if log {
        learned = learned.with_param(CONTROL_TAPER, 1.0);
    }
    if start != 0.0 {
        learned = learned.with_param(CONTROL_START, start);
    }
    Ok(Command::Batch(vec![
        Command::AddNode {
            id: node,
            node: learned,
        },
        spare::wire(Endpoint::new(node, "out"), target),
    ]))
}

/// A parameter waiting for a control to be moved.
pub struct Learning {
    pub target: Endpoint,
    tap: noodle_io::MidiTap,
}

impl Session {
    /// What the Learn MIDI entry on `target` should show.
    pub fn learn_midi_menu(&self, target: &Endpoint) -> LearnMidi {
        let destination = destination(&self.project, target);
        if self
            .learning
            .as_ref()
            .is_some_and(|l| l.target == destination)
        {
            LearnMidi::Listening
        } else if let Some(why) = refusal(&self.project, target) {
            LearnMidi::Refused(why)
        } else {
            LearnMidi::Offered
        }
    }

    /// Whether a parameter is waiting for a control.
    pub fn learning_midi(&self) -> bool {
        self.learning.is_some()
    }

    /// Starts listening for the control to learn onto `target`, or stops if
    /// it already was.
    pub(super) fn learn_midi(&mut self, target: Endpoint) {
        let destination = destination(&self.project, &target);
        if self
            .learning
            .as_ref()
            .is_some_and(|l| l.target == destination)
        {
            self.learning = None;
            self.message = Some("Stopped learning MIDI".into());
            return;
        }
        if let Some(why) = refusal(&self.project, &target) {
            self.message = Some(format!("Couldn't learn MIDI: {why}"));
            return;
        }
        let hint = if self.audio_config.midi_inputs.is_empty() {
            " (no MIDI input is open: choose one in the audio settings)"
        } else {
            ""
        };
        self.learning = Some(Learning {
            target: destination,
            tap: self.midi.tap(),
        });
        self.message = Some(format!("Move a MIDI control to learn it{hint}"));
    }

    /// Takes the first control heard since Learn MIDI was chosen.
    pub(super) fn poll_learn_midi(&mut self) {
        let Some(learning) = &self.learning else {
            return;
        };
        let heard = std::iter::from_fn(|| learning.tap.next())
            .find(|(_, message)| control_of(*message).is_some());
        let Some(heard) = heard else {
            return;
        };
        let target = learning.target.clone();
        self.learning = None;
        self.history.end_group();
        let node = self.new_node_id();
        match command(&self.project, &self.registry, &target, node, &heard) {
            Ok(command) => {
                self.edit([super::Edit::Apply(command)]);
                let from = heard
                    .0
                    .map_or(String::new(), |port| format!(" from {port}"));
                let control = control_of(heard.1).unwrap_or_default();
                self.message = Some(format!("Learned control {control}{from}"));
            }
            Err(error) => self.message = Some(format!("Couldn't learn MIDI: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes};
    use noodle_core::group::{GROUP, GROUP_OUTPUT, ports_config};
    use noodle_engine::OUTPUT_ID;
    use noodle_nodes::SVF_ID;

    fn add(session: &mut Session, node: Node) -> NodeId {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        id
    }

    fn wire(session: &mut Session, from: (NodeId, &str), to: (NodeId, &str)) {
        let wire = spare::wire(Endpoint::new(from.0, from.1), Endpoint::new(to.0, to.1));
        session.edit([Edit::Apply(wire)]);
    }

    /// "Lead": a saw through a state-variable filter, in a group, to the
    /// output. Returns the group and the filter.
    fn lead(session: &mut Session) -> (NodeId, NodeId) {
        let group = add(session, Node::new(GROUP));
        let saw = add(session, Node::new("noodle.osc.saw").in_group(group));
        let filter = add(
            session,
            Node::new(SVF_ID)
                .with_param("cutoff", 800.0)
                .in_group(group),
        );
        let out = add(
            session,
            Node::new(GROUP_OUTPUT)
                .with_config(ports_config(&[(1, "out".into())]))
                .in_group(group),
        );
        wire(session, (saw, "out"), (filter, "in"));
        wire(session, (filter, "low"), (out, "p1"));
        let output = add(session, Node::new(OUTPUT_ID));
        wire(session, (group, "out"), (output, "in"));
        (group, filter)
    }

    fn controls(session: &Session) -> Vec<(NodeId, Node)> {
        let nodes = session.project().graph().nodes();
        let found = nodes.filter(|(_, n)| n.type_id == MIDI_CONTROL_ID);
        found.map(|(id, n)| (id, n.clone())).collect()
    }

    fn port(node: &Node) -> Option<&Value> {
        node.config.get(MIDI_PORT_KEY)
    }

    #[test]
    fn learning_a_synth_groups_cutoff_adds_a_wired_control_for_that_port_in_one_undo_step() {
        let mut session = Session::new(Nodes::all());
        let (group, filter) = lead(&mut session);
        let cutoff = Endpoint::new(filter, "cutoff");
        let before = session.project().graph().nodes().count();

        session.edit([Edit::LearnMidi(cutoff.clone())]);
        assert!(session.learning_midi());
        assert_eq!(session.learn_midi_menu(&cutoff), LearnMidi::Listening);
        // A note and a program change aren't controls; the controller is.
        let bus = session.midi_bus().clone();
        bus.send_from("Keystation", [0x90, 60, 100]);
        bus.send_from("Keystation", [0xC0, 5, 0]);
        session.maintain();
        assert!(session.learning_midi(), "still waiting");
        bus.send_from("Keystation", [0xB2, 74, 64]);
        session.maintain();
        assert!(!session.learning_midi());

        let [(node, made)] = &controls(&session)[..] else {
            panic!("one MIDI Control")
        };
        assert_eq!(made.parent, Some(group), "in the parameter's own group");
        assert_eq!(port(made), Some(&Value::Text("Keystation".into())));
        assert_eq!(made.params.get("control"), Some(&74.0));
        // A cutoff is offset by a wire: the control ranges over its travel
        // and starts in the middle, so the sound stays as it was.
        assert_eq!(made.params.get("min"), Some(&-1.0));
        assert_eq!(made.params.get("max"), None, "1 is the default");
        assert_eq!(made.params.get("start"), Some(&0.5));
        assert_eq!(
            session.project().graph().source(&cutoff),
            Some(&Endpoint::new(*node, "out"))
        );
        assert_eq!(session.project().graph().nodes().count(), before + 1);

        // Learned again, it is refused: the parameter has a wire.
        assert!(refusal(session.project(), &cutoff).is_some());
        session.edit([Edit::LearnMidi(cutoff)]);
        assert!(!session.learning_midi());

        session.undo();
        assert!(controls(&session).is_empty());
        assert_eq!(session.project().graph().nodes().count(), before);
    }

    #[test]
    fn a_pitch_bend_or_a_low_controller_half_is_learned_as_its_control() {
        assert_eq!(control_of([0xE1, 0, 64]), Some(128.0));
        assert_eq!(control_of([0xD0, 50, 0]), Some(129.0));
        assert_eq!(control_of([0xB0, 33, 5]), Some(1.0));
        assert_eq!(control_of([0xB0, 123, 0]), None);
        assert_eq!(control_of([0x90, 60, 100]), None);
    }

    #[test]
    fn learning_on_an_automation_node_wires_its_record_input() {
        let mut session = Session::new(Nodes::all());
        let node = add(&mut session, Node::new(AUTOMATION_NODE_ID));
        session.edit([Edit::ArmAutomation { node, on: true }]);
        let value = Endpoint::new(node, AUTOMATION_VALUE);
        session.edit([Edit::LearnMidi(value.clone())]);
        // The playing keyboard sends from no port: any port will do.
        session.midi_bus().send([0xB0, 21, 100]);
        session.maintain();
        let [(control, made)] = &controls(&session)[..] else {
            panic!("one MIDI Control")
        };
        assert_eq!(port(made), Some(&Value::Text("*".into())));
        assert_eq!(
            session
                .project()
                .graph()
                .source(&Endpoint::new(node, AUTOMATION_RECORD)),
            Some(&Endpoint::new(*control, "out"))
        );
        assert!(session.project().graph().source(&value).is_none());
        // Its record input is wired now, so learning again is refused.
        assert!(refusal(session.project(), &value).is_some());
    }

    #[test]
    fn learning_can_be_stopped_and_ignores_controls_moved_before_it_began() {
        let mut session = Session::new(Nodes::all());
        let (_, filter) = lead(&mut session);
        let cutoff = Endpoint::new(filter, "cutoff");
        session.midi_bus().send_from("Keys", [0xB0, 1, 10]);
        session.edit([Edit::LearnMidi(cutoff.clone())]);
        session.maintain();
        assert!(session.learning_midi());
        // The same entry again stops it, and so does Escape.
        session.edit([Edit::LearnMidi(cutoff.clone())]);
        assert!(!session.learning_midi());
        session.edit([Edit::LearnMidi(cutoff)]);
        session.edit([Edit::CancelLearnMidi]);
        session.midi_bus().send_from("Keys", [0xB0, 1, 10]);
        session.maintain();
        assert!(controls(&session).is_empty());
    }
}
