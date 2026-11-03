//! Sections: the span between two markers (or any range picked on the ruler),
//! across all tracks. A section is copied, cut and pasted like clips are, and
//! time is inserted into the arrangement or deleted from it. A copy brings
//! clips and automation lanes only, and the tempo, which the arrangement shows
//! as an automation lane too. Copies are not linked to what they came from.
//!
//! Everything here works out a [`Command`] from the project as it is, so one
//! gesture is one undo.

use std::collections::{BTreeMap, BTreeSet};

use noodle_core::{
    AutomationLane, AutomationPoint, Clip, ClipId, ClipWarp, Command, Endpoint, Marker, Project,
    SignatureChange, TempoChange, TempoMap, Tick, WarpId,
};
use serde::{Deserialize, Serialize};

use super::{Copied, clips, paste, warp_of};

/// A span of the timeline, `start` before `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: Tick,
    pub end: Tick,
}

impl Range {
    pub fn new(a: Tick, b: Tick) -> Option<Self> {
        let (start, end) = (a.min(b), a.max(b));
        (start < end).then_some(Self { start, end })
    }

    pub fn len(self) -> i64 {
        self.end.0 - self.start.0
    }

    pub fn contains(self, tick: Tick) -> bool {
        self.start <= tick && tick < self.end
    }
}

/// The range from the marker at `index` to the next one. The last marker's
/// section ends where the last clip or automation point does.
pub fn between_markers(
    project: &Project,
    index: usize,
    rate: &dyn Fn(&Clip) -> u32,
) -> Option<Range> {
    let markers = project.markers();
    let from = markers.get(index)?.at;
    let to = match markers.get(index + 1) {
        Some(next) => next.at,
        None => content_end(project, rate)?,
    };
    Range::new(from, to)
}

/// Where the last clip or automation point ends.
fn content_end(project: &Project, rate: &dyn Fn(&Clip) -> u32) -> Option<Tick> {
    let map = project.tempo_map();
    let clips = project
        .clips()
        .map(|(_, clip)| clips::clip_end(map, clip, rate(clip), warp_of(project, clip)));
    let points = project
        .lanes()
        .filter_map(|(_, lane)| lane.points.last().map(|p| p.tick));
    clips.chain(points).max()
}

/// A section as copied: everything is relative to the start of the range.
#[derive(Default, Serialize, Deserialize)]
pub struct Section {
    pub length: Tick,
    clips: Vec<Clip>,
    warps: Vec<(WarpId, ClipWarp)>,
    lanes: Vec<(Endpoint, Vec<AutomationPoint>)>,
    /// The tempo changes, the first at 0: the tempo the range started in.
    tempos: Vec<TempoChange>,
}

/// What a clip becomes when a range is cut out of it.
struct Carved {
    /// The part before the range (it keeps the clip's ID).
    left: Option<Clip>,
    /// The part inside it.
    mid: Option<Clip>,
    /// The part after it.
    right: Option<Clip>,
}

/// Cuts `clip` at the range's edges. A clip that can't be cut (a warped one
/// the warp can't split) goes whole to the side its start is on.
fn carve(map: &TempoMap, clip: &Clip, warp: Option<&ClipWarp>, rate: u32, range: Range) -> Carved {
    let end = clips::clip_end(map, clip, rate, warp);
    let mut out = Carved {
        left: None,
        mid: None,
        right: None,
    };
    if end <= range.start {
        out.left = Some(clip.clone());
        return out;
    }
    if clip.start >= range.end {
        out.right = Some(clip.clone());
        return out;
    }
    let mut rest = clip.clone();
    if clip.start < range.start {
        match clips::split(map, clip, range.start, rate, warp) {
            Some((left, right)) => {
                out.left = Some(left);
                rest = right;
            }
            None => {
                out.left = Some(clip.clone());
                return out;
            }
        }
    }
    if clips::clip_end(map, &rest, rate, warp) > range.end {
        match clips::split(map, &rest, range.end, rate, warp) {
            Some((mid, right)) => {
                out.mid = Some(mid);
                out.right = Some(right);
            }
            None => out.mid = Some(rest),
        }
    } else {
        out.mid = Some(rest);
    }
    out
}

/// A point at the start of `lane`'s span that holds the value there, unless a
/// point is already there.
fn held_at(lane: &AutomationLane, at: Tick) -> Option<AutomationPoint> {
    if lane.points.iter().any(|p| p.tick == at) {
        return None;
    }
    Some(AutomationPoint {
        tick: at,
        value: lane.value_at(at.0 as f64)?,
        curve: Default::default(),
    })
}

/// Copies what is inside `range`, across all tracks.
pub fn copy(project: &Project, range: Range, rate: &dyn Fn(&Clip) -> u32) -> Section {
    let map = project.tempo_map();
    let mid: Vec<Clip> = project
        .clips()
        .filter_map(|(_, clip)| {
            let carved = carve(map, clip, warp_of(project, clip), rate(clip), range);
            carved.mid
        })
        .map(|clip| clips::moved(&clip, Tick(clip.start.0 - range.start.0)))
        .collect();
    let copied = Copied::of(project, mid.iter());
    let mut lanes = Vec::new();
    for (_, lane) in project.lanes() {
        let inside: Vec<AutomationPoint> = lane
            .points
            .iter()
            .filter(|p| range.contains(p.tick))
            .map(|p| AutomationPoint {
                tick: Tick(p.tick.0 - range.start.0),
                ..*p
            })
            .collect();
        if inside.is_empty() {
            continue;
        }
        let mut points = inside;
        if let Some(held) = held_at(lane, range.start) {
            points.insert(
                0,
                AutomationPoint {
                    tick: Tick::ZERO,
                    ..held
                },
            );
        }
        lanes.push((lane.target.clone(), points));
    }
    let mut tempos = vec![TempoChange {
        tick: Tick::ZERO,
        bpm: map.bpm_at(range.start),
    }];
    tempos.extend(
        map.tempos()
            .iter()
            .filter(|t| t.tick > range.start && t.tick < range.end)
            .map(|t| TempoChange {
                tick: Tick(t.tick.0 - range.start.0),
                bpm: t.bpm,
            }),
    );
    Section {
        length: Tick(range.len()),
        clips: copied.clips,
        warps: copied.warps,
        lanes,
        tempos,
    }
}

/// The tempo map with tempo changes in `[from, to)` dropped, `copied` (placed
/// at `from`) put in their place and the tempo from `to` on left as it was.
fn tempo_over(map: &TempoMap, from: Tick, to: Tick, copied: &[TempoChange]) -> Option<TempoMap> {
    let after = map.bpm_at(to);
    let mut tempos: Vec<TempoChange> = map
        .tempos()
        .iter()
        .filter(|t| t.tick < from || t.tick >= to)
        .copied()
        .collect();
    // A change at `to` already says what comes after.
    if !tempos.iter().any(|t| t.tick == to) {
        tempos.push(TempoChange {
            tick: to,
            bpm: after,
        });
    }
    for change in copied {
        let tick = Tick(from.0 + change.tick.0);
        tempos.retain(|t| t.tick != tick);
        tempos.push(TempoChange {
            tick,
            bpm: change.bpm,
        });
    }
    normalised(map, tempos)
}

/// The tempos in order, without a change that repeats the tempo before it.
fn normalised(map: &TempoMap, mut tempos: Vec<TempoChange>) -> Option<TempoMap> {
    tempos.sort_by_key(|t| t.tick);
    let mut kept: Vec<TempoChange> = Vec::new();
    for change in tempos {
        if kept
            .last()
            .is_some_and(|last| (last.bpm - change.bpm).abs() < 1e-9)
        {
            continue;
        }
        kept.push(change);
    }
    TempoMap::new(kept, map.signatures().to_vec()).ok()
}

/// The command that puts a copy of `section` at `at`, and the clips it adds.
/// Clips are added; automation and tempo inside the span are replaced, and
/// what follows the span keeps its values. `None` if there is nothing to do.
pub fn paste_section(
    project: &Project,
    section: &Section,
    at: Tick,
) -> (Option<Command>, BTreeSet<ClipId>) {
    let mut commands = Vec::new();
    let mut pasted = BTreeSet::new();
    let end = Tick(at.0 + section.length.0);
    if let Some(first) = section.clips.iter().map(|clip| clip.start.0).min() {
        let copied = Copied {
            clips: section.clips.clone(),
            warps: section.warps.clone(),
        };
        // `paste` puts the earliest clip at the tick it is given.
        let (command, ids) = paste(project, &copied, Tick(at.0 + first), None);
        commands.extend(command);
        pasted = ids;
    }
    let mut next_lane = project.next_lane_id();
    for (target, points) in &section.lanes {
        if project.graph().node(target.node).is_none() {
            continue;
        }
        let shifted = points.iter().map(|p| AutomationPoint {
            tick: Tick(at.0 + p.tick.0),
            ..*p
        });
        match project.lane_for(target) {
            Some((id, lane)) => {
                let mut kept: Vec<AutomationPoint> = lane
                    .points
                    .iter()
                    .filter(|p| p.tick < at || p.tick >= end)
                    .copied()
                    .collect();
                // What follows keeps the value it had.
                if let Some(held) = held_at(lane, end) {
                    kept.push(held);
                }
                kept.retain(|p| !(at <= p.tick && p.tick < end));
                kept.extend(shifted);
                kept.sort_by_key(|p| p.tick);
                kept.dedup_by_key(|p| p.tick);
                commands.push(Command::SetLane {
                    id,
                    lane: AutomationLane::new(target.clone(), kept),
                });
            }
            None => {
                commands.push(Command::AddLane {
                    id: next_lane,
                    lane: AutomationLane::new(target.clone(), shifted.collect()),
                });
                next_lane.0 += 1;
            }
        }
    }
    let map = project.tempo_map();
    if let Some(new) = tempo_over(map, at, end, &section.tempos).filter(|new| new != map) {
        commands.push(Command::SetTempoMap(new));
    }
    (
        (!commands.is_empty()).then_some(Command::Batch(commands)),
        pasted,
    )
}

/// How a range's clips are changed: the parts left of it stay, the parts
/// inside go, and the parts right of it move `shift` earlier.
fn cut_clips(
    project: &Project,
    range: Range,
    shift: i64,
    rate: &dyn Fn(&Clip) -> u32,
) -> Vec<Command> {
    let map = project.tempo_map();
    let mut next = project.next_clip_id();
    let mut commands = Vec::new();
    for (id, clip) in project.clips() {
        let carved = carve(map, clip, warp_of(project, clip), rate(clip), range);
        let right = carved
            .right
            .map(|right| clips::moved(&right, Tick(right.start.0 - shift)));
        match (carved.left, right) {
            (Some(left), right) => {
                if left != *clip {
                    commands.push(Command::SetClip { id, clip: left });
                }
                if let Some(right) = right {
                    commands.push(Command::AddClip {
                        id: next,
                        clip: right,
                    });
                    next.0 += 1;
                }
            }
            (None, Some(right)) => {
                if right != *clip {
                    commands.push(Command::SetClip { id, clip: right });
                }
            }
            (None, None) => commands.push(Command::RemoveClip { id }),
        }
    }
    commands
}

/// The command that empties `range` of clips and automation points, leaving
/// the time in place (what cutting a section does after copying it).
pub fn clear(project: &Project, range: Range, rate: &dyn Fn(&Clip) -> u32) -> Command {
    let mut commands = cut_clips(project, range, 0, rate);
    for (id, lane) in project.lanes() {
        let points: Vec<AutomationPoint> = lane
            .points
            .iter()
            .filter(|p| !range.contains(p.tick))
            .copied()
            .collect();
        if points.len() != lane.points.len() {
            commands.push(Command::SetLane {
                id,
                lane: AutomationLane::new(lane.target.clone(), points),
            });
        }
    }
    Command::Batch(commands)
}

/// The signature changes from `from` on, moved `delta` ticks (earlier if
/// negative), those starting before `drop_before` aside. Only whole bars can
/// move them, so a shift of anything else leaves them where they are.
fn signatures_moved(
    map: &TempoMap,
    from: Tick,
    delta: i64,
    drop_before: Tick,
) -> Vec<SignatureChange> {
    let bar_ticks = signature_bar_ticks(map, from);
    let whole = bar_ticks > 0 && delta % bar_ticks == 0;
    let bars = if whole { delta / bar_ticks } else { 0 };
    map.signatures()
        .iter()
        .filter_map(|change| {
            let start = map.bar_start(change.bar);
            if change.bar == 0 {
                return Some(*change);
            }
            // Changes inside a deleted range go with it.
            if start >= drop_before && start < from {
                return None;
            }
            if start < from {
                return Some(*change);
            }
            Some(SignatureChange {
                bar: (i64::from(change.bar) + bars).max(1) as u32,
                signature: change.signature,
            })
        })
        .collect()
}

/// How many ticks a bar is at `tick`.
fn signature_bar_ticks(map: &TempoMap, tick: Tick) -> i64 {
    let i = map
        .signatures()
        .partition_point(|s| map.bar_start(s.bar) <= tick)
        .saturating_sub(1);
    map.signatures()[i].signature.ticks_per_bar()
}

fn shifted_map(
    mut tempos: Vec<TempoChange>,
    mut signatures: Vec<SignatureChange>,
) -> Option<TempoMap> {
    tempos.sort_by_key(|t| t.tick);
    signatures.sort_by_key(|s| s.bar);
    signatures.dedup_by_key(|s| s.bar);
    TempoMap::new(tempos, signatures).ok()
}

fn markers_command(project: &Project, markers: Vec<Marker>) -> Option<Command> {
    (markers != project.markers()).then_some(Command::SetMarkers(markers))
}

/// Opens `len` ticks of empty time at `at`: everything from there on moves
/// later, a clip across `at` is cut in two, and an automation lane holds its
/// value across the gap.
pub fn insert_time(
    project: &Project,
    at: Tick,
    len: i64,
    rate: &dyn Fn(&Clip) -> u32,
) -> Option<Command> {
    if len <= 0 {
        return None;
    }
    let map = project.tempo_map();
    let mut commands = Vec::new();
    let mut next = project.next_clip_id();
    for (id, clip) in project.clips() {
        let warp = warp_of(project, clip);
        let end = clips::clip_end(map, clip, rate(clip), warp);
        if clip.start >= at {
            commands.push(Command::SetClip {
                id,
                clip: clips::moved(clip, Tick(clip.start.0 + len)),
            });
        } else if end > at
            && let Some((left, right)) = clips::split(map, clip, at, rate(clip), warp)
        {
            commands.push(Command::SetClip { id, clip: left });
            commands.push(Command::AddClip {
                id: next,
                clip: clips::moved(&right, Tick(at.0 + len)),
            });
            next.0 += 1;
        }
    }
    for (id, lane) in project.lanes() {
        let Some(split) = lane.points.iter().position(|p| p.tick >= at) else {
            continue;
        };
        let mut points: Vec<AutomationPoint> = lane.points[..split].to_vec();
        if points.last().is_some() && lane.points[split].tick == at {
            // The point at `at` moves on: hold its value across the gap.
            points.push(AutomationPoint {
                tick: at,
                value: lane.points[split].value,
                curve: noodle_core::Curve::Hold,
            });
        } else if let Some(before) = points.last().copied() {
            // Hold the value across the gap, then carry on the curve.
            points.push(AutomationPoint {
                tick: at,
                value: lane.value_at(at.0 as f64).unwrap_or(before.value),
                curve: noodle_core::Curve::Hold,
            });
            points.push(AutomationPoint {
                tick: Tick(at.0 + len),
                value: lane.value_at(at.0 as f64).unwrap_or(before.value),
                curve: before.curve,
            });
        }
        points.extend(lane.points[split..].iter().map(|p| AutomationPoint {
            tick: Tick(p.tick.0 + len),
            ..*p
        }));
        commands.push(Command::SetLane {
            id,
            lane: AutomationLane::new(lane.target.clone(), points),
        });
    }
    // Tempo changes after the point move; a change at the very start stays.
    let tempos: Vec<TempoChange> = map
        .tempos()
        .iter()
        .map(|t| {
            if t.tick >= at && t.tick > Tick::ZERO {
                TempoChange {
                    tick: Tick(t.tick.0 + len),
                    ..*t
                }
            } else {
                *t
            }
        })
        .collect();
    let signatures = signatures_moved(map, at, len, at);
    if let Some(new) = shifted_map(tempos, signatures).filter(|new| new != map) {
        commands.push(Command::SetTempoMap(new));
    }
    let markers = project
        .markers()
        .iter()
        .map(|m| Marker {
            name: m.name.clone(),
            at: if m.at >= at { Tick(m.at.0 + len) } else { m.at },
        })
        .collect();
    commands.extend(markers_command(project, markers));
    (!commands.is_empty()).then_some(Command::Batch(commands))
}

/// Takes `range` out of the arrangement: what is inside goes, and everything
/// after it moves earlier to close the gap.
pub fn delete_time(project: &Project, range: Range, rate: &dyn Fn(&Clip) -> u32) -> Command {
    let len = range.len();
    let map = project.tempo_map();
    let mut commands = cut_clips(project, range, len, rate);
    for (id, lane) in project.lanes() {
        if !lane.points.iter().any(|p| p.tick >= range.start) {
            continue;
        }
        let points: Vec<AutomationPoint> = lane
            .points
            .iter()
            .filter(|p| !range.contains(p.tick))
            .map(|p| {
                if p.tick >= range.end {
                    AutomationPoint {
                        tick: Tick(p.tick.0 - len),
                        ..*p
                    }
                } else {
                    *p
                }
            })
            .collect();
        commands.push(Command::SetLane {
            id,
            lane: AutomationLane::new(lane.target.clone(), points),
        });
    }
    // The tempo after the range goes on from where the range started.
    let mut tempos: Vec<TempoChange> = Vec::new();
    for t in map.tempos() {
        if t.tick < range.start {
            tempos.push(*t);
        } else if t.tick >= range.end {
            tempos.push(TempoChange {
                tick: Tick(t.tick.0 - len),
                ..*t
            });
        }
    }
    let after = map.bpm_at(range.end);
    if !tempos.iter().any(|t| t.tick == range.start) {
        tempos.push(TempoChange {
            tick: range.start,
            bpm: after,
        });
    }
    let signatures = signatures_moved(map, range.end, -len, range.start);
    if let Some(new) = shifted_map(tempos, signatures)
        .and_then(|new| normalised(&new, new.tempos().to_vec()))
        .filter(|new| new != map)
    {
        commands.push(Command::SetTempoMap(new));
    }
    let markers = project
        .markers()
        .iter()
        .filter(|m| !range.contains(m.at))
        .map(|m| Marker {
            name: m.name.clone(),
            at: if m.at >= range.end {
                Tick(m.at.0 - len)
            } else {
                m.at
            },
        })
        .collect();
    commands.extend(markers_command(project, markers));
    Command::Batch(commands)
}

/// A name for a new marker: "Marker 1", "Marker 2", the first not taken.
pub fn next_name(project: &Project) -> String {
    let taken: BTreeMap<&str, ()> = project
        .markers()
        .iter()
        .map(|m| (m.name.as_str(), ()))
        .collect();
    (1..)
        .map(|n| format!("Marker {n}"))
        .find(|name| !taken.contains_key(name.as_str()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_core::group::{GROUP_OUTPUT, TRACK_INPUT, create_track};
    use noodle_core::{Curve, Position};

    const BAR: i64 = 3840;

    fn rate(_: &Clip) -> u32 {
        48_000
    }

    fn range(from_bar: i64, to_bar: i64) -> Range {
        Range::new(Tick(from_bar * BAR), Tick(to_bar * BAR)).unwrap()
    }

    struct Song {
        session: Session,
        gain: Endpoint,
    }

    impl Song {
        /// A track with a MIDI clip a bar long at the start of each of bars
        /// 1 to 4, a gain lane with a point at the start of each bar, a
        /// faster tempo from bar 3 and a marker at bar 2.
        fn new() -> Self {
            let mut session = Session::new(Nodes::all());
            let (_, command) =
                create_track(None, Position { x: 0.0, y: 0.0 }, || session.new_node_id());
            session.edit([Edit::Apply(command)]);
            let find = |session: &Session, type_id: &str| {
                session
                    .project()
                    .graph()
                    .nodes()
                    .find(|(_, node)| node.type_id == type_id)
                    .map(|(id, _)| id)
                    .unwrap()
            };
            let input = find(&session, TRACK_INPUT);
            let output = find(&session, GROUP_OUTPUT);
            let gain = Endpoint::new(output, "gain.1");
            let mut song = Self { session, gain };
            for bar in 0..4 {
                let id = song.session.project().next_clip_id();
                song.apply(Command::AddClip {
                    id,
                    clip: Clip::midi(input, Tick(bar * BAR), Tick(BAR)),
                });
            }
            let points = (0..4)
                .map(|bar| AutomationPoint {
                    tick: Tick(bar * BAR),
                    value: bar as f32 / 10.0,
                    curve: Curve::Linear,
                })
                .collect();
            let id = song.session.project().next_lane_id();
            song.apply(Command::AddLane {
                id,
                lane: AutomationLane::new(song.gain.clone(), points),
            });
            let tempos = vec![
                TempoChange {
                    tick: Tick::ZERO,
                    bpm: 120.0,
                },
                TempoChange {
                    tick: Tick(2 * BAR),
                    bpm: 90.0,
                },
            ];
            let map = TempoMap::new(
                tempos,
                song.session.project().tempo_map().signatures().to_vec(),
            )
            .unwrap();
            song.apply(Command::SetTempoMap(map));
            song.apply(Command::SetMarkers(vec![Marker {
                name: "Chorus".into(),
                at: Tick(BAR),
            }]));
            song
        }

        fn apply(&mut self, command: Command) {
            self.session.edit([Edit::Apply(command)]);
        }

        fn project(&self) -> &Project {
            self.session.project()
        }

        fn starts(&self) -> Vec<i64> {
            let mut starts: Vec<i64> = self.project().clips().map(|(_, c)| c.start.0).collect();
            starts.sort();
            starts
        }

        fn lane(&self) -> Vec<(i64, f32)> {
            self.project()
                .lane_for(&self.gain)
                .unwrap()
                .1
                .points
                .iter()
                .map(|p| (p.tick.0, p.value))
                .collect()
        }

        fn tempos(&self) -> Vec<(i64, f64)> {
            self.project()
                .tempo_map()
                .tempos()
                .iter()
                .map(|t| (t.tick.0, t.bpm))
                .collect()
        }
    }

    #[test]
    fn a_copied_section_brings_clips_automation_and_tempo_and_nothing_links_it() {
        let mut song = Song::new();
        // Bars 3 and 4 (the faster part), pasted after the end of the song.
        let section = copy(song.project(), range(2, 4), &rate);
        assert_eq!(section.length, Tick(2 * BAR));
        let (command, pasted) = paste_section(song.project(), &section, Tick(4 * BAR));
        assert_eq!(pasted.len(), 2);
        song.apply(command.unwrap());
        assert_eq!(song.starts(), [0, BAR, 2 * BAR, 3 * BAR, 4 * BAR, 5 * BAR]);
        // The lane got the section's points, and a held value at its start.
        let lane = song.lane();
        assert!(lane.contains(&(4 * BAR, 0.2)) && lane.contains(&(5 * BAR, 0.3)));
        // The tempo at the section's start came along.
        assert_eq!(song.tempos(), [(0, 120.0), (2 * BAR, 90.0)]);
        // A section from the slower start pastes a tempo change.
        let section = copy(song.project(), range(0, 1), &rate);
        let (command, _) = paste_section(song.project(), &section, Tick(6 * BAR));
        song.apply(command.unwrap());
        assert_eq!(
            song.tempos(),
            [
                (0, 120.0),
                (2 * BAR, 90.0),
                (6 * BAR, 120.0),
                (7 * BAR, 90.0)
            ]
        );
        // No clip group ties the copies to the originals.
        assert!(
            song.project()
                .clips()
                .all(|(id, _)| song.project().group_of(id).is_none())
        );
    }

    #[test]
    fn inserting_time_moves_everything_after_it_and_holds_automation_across_the_gap() {
        let mut song = Song::new();
        song.apply(insert_time(song.project(), Tick(2 * BAR), BAR, &rate).unwrap());
        assert_eq!(song.starts(), [0, BAR, 3 * BAR, 4 * BAR]);
        // Hold the value from bar 2 over the new bar, then carry on.
        let lane = song.lane();
        assert_eq!(
            lane,
            [
                (0, 0.0),
                (BAR, 0.1),
                (2 * BAR, 0.2),
                (3 * BAR, 0.2),
                (4 * BAR, 0.3)
            ]
        );
        assert_eq!(song.tempos(), [(0, 120.0), (3 * BAR, 90.0)]);
        assert_eq!(song.project().markers()[0].at, Tick(BAR));
        // Before the marker, the marker moves too.
        song.apply(insert_time(song.project(), Tick(0), BAR, &rate).unwrap());
        assert_eq!(song.project().markers()[0].at, Tick(2 * BAR));
    }

    #[test]
    fn inserting_time_inside_a_clip_cuts_it_in_two() {
        let mut song = Song::new();
        song.apply(insert_time(song.project(), Tick(BAR + 960), 960, &rate).unwrap());
        assert_eq!(
            song.starts(),
            [0, BAR, BAR + 960 + 960, 2 * BAR + 960, 3 * BAR + 960]
        );
        let lengths: Vec<i64> = song
            .project()
            .clips()
            .filter_map(|(_, c)| Some(c.as_midi()?.length.0))
            .collect();
        assert_eq!(lengths.iter().sum::<i64>(), 4 * BAR);
    }

    #[test]
    fn deleting_time_removes_what_is_inside_and_closes_the_gap() {
        let mut song = Song::new();
        song.apply(delete_time(song.project(), range(1, 3), &rate));
        assert_eq!(song.starts(), [0, BAR]);
        assert_eq!(song.lane(), [(0, 0.0), (BAR, 0.3)]);
        // The slower tempo that began inside the range now starts at its end.
        assert_eq!(song.tempos(), [(0, 120.0), (BAR, 90.0)]);
        // The marker was inside the range.
        assert!(song.project().markers().is_empty());
    }

    #[test]
    fn cutting_a_section_empties_it_and_keeps_the_time() {
        let mut song = Song::new();
        song.apply(clear(song.project(), range(1, 3), &rate));
        assert_eq!(song.starts(), [0, 3 * BAR]);
        assert_eq!(song.lane(), [(0, 0.0), (3 * BAR, 0.3)]);
        assert_eq!(song.tempos(), [(0, 120.0), (2 * BAR, 90.0)]);
    }

    #[test]
    fn a_section_runs_from_one_marker_to_the_next_and_the_last_to_the_end() {
        let mut song = Song::new();
        let mut markers = song.project().markers().to_vec();
        markers.push(Marker {
            name: "Bridge".into(),
            at: Tick(3 * BAR),
        });
        song.apply(Command::SetMarkers(markers));
        assert_eq!(between_markers(song.project(), 0, &rate), Some(range(1, 3)));
        // The last clip ends at the end of bar 4.
        assert_eq!(between_markers(song.project(), 1, &rate), Some(range(3, 4)));
        assert_eq!(between_markers(song.project(), 2, &rate), None);
        assert_eq!(next_name(song.project()), "Marker 1");
    }

    #[test]
    fn undo_puts_a_whole_time_edit_back_in_one_step() {
        let mut song = Song::new();
        let before = song.project().clone();
        song.apply(delete_time(song.project(), range(1, 3), &rate));
        assert_ne!(*song.project(), before);
        song.session.undo();
        assert_eq!(*song.project(), before);
    }
}
