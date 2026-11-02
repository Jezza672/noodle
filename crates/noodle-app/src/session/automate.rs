//! The Automate shortcut: a graph edit, nothing more. It adds an Automation
//! node beside a parameter, a lane on the node's `value` holding one point,
//! and a wire from the node's `out` to the parameter, as one undo step. The
//! parameter is afterwards an ordinary wired parameter.

use noodle_core::group::{GROUP, PORT_NAME};
use noodle_core::{
    AutomationLane, AutomationPoint, Command, Config, Curve, Endpoint, LaneId, Node, NodeId,
    Project, Tick, Value, spare,
};
use noodle_engine::{InputKind, ParamInfo, Registry, Taper};
use noodle_nodes::{
    AUTOMATION_LABEL, AUTOMATION_MAX, AUTOMATION_MIN, AUTOMATION_NODE_ID, AUTOMATION_TAPER,
    AUTOMATION_VALUE,
};

/// How far left of its target the new node sits.
const OFFSET_X: f32 = 260.0;

/// Why Automate can't be used on `target`, for the menu entry's tooltip.
pub fn refusal(project: &Project, target: &Endpoint) -> Option<&'static str> {
    if project.graph().source(target).is_some() {
        Some("This parameter already has a wire")
    } else if project.lane_for(target).is_some() {
        Some("This parameter already has an automation lane")
    } else {
        None
    }
}

/// The command that automates `target`, with `node` and `lane` as the new
/// node's and lane's IDs.
pub fn command(
    project: &Project,
    registry: &Registry,
    target: &Endpoint,
    node: NodeId,
    lane: LaneId,
) -> Result<Command, String> {
    if let Some(reason) = refusal(project, target) {
        return Err(reason.to_owned());
    }
    let graph = project.graph();
    let target_node = graph.node(target.node).ok_or("the node is gone")?;
    let node_type = registry
        .get(&target_node.type_id)
        .ok_or("the node's type is unknown")?;
    let layout = node_type
        .layout(&target_node.config)
        .map_err(|e| e.to_string())?;
    let input = layout
        .inputs
        .iter()
        .find(|i| i.key == target.port)
        .ok_or("the parameter is gone")?;
    let InputKind::Param(info) = &input.kind else {
        return Err("only a parameter can be automated".to_owned());
    };
    let current = target_node
        .params
        .get(&target.port)
        .copied()
        .unwrap_or(info.default);

    let (min, max, log, start) = range_and_start(info, current);
    let owner = match target_node.parent.and_then(|g| graph.node(g)) {
        Some(group) if group.type_id == GROUP => match group.config.get(PORT_NAME) {
            Some(Value::Text(name)) if !name.is_empty() => name.clone(),
            _ => "Group".to_owned(),
        },
        _ => node_type.info().name.to_owned(),
    };
    let label = format!("{owner} › {}", input.name);

    let mut automation = Node::new(AUTOMATION_NODE_ID)
        .at(target_node.position.x - OFFSET_X, target_node.position.y)
        .with_config(Config::new().with(AUTOMATION_LABEL, Value::Text(label)));
    automation.parent = target_node.parent;
    // Parameters left at their defaults (0 to 1, linear) aren't stored.
    if min != 0.0 {
        automation = automation.with_param(AUTOMATION_MIN, min);
    }
    if max != 1.0 {
        automation = automation.with_param(AUTOMATION_MAX, max);
    }
    if log {
        automation = automation.with_param(AUTOMATION_TAPER, 1.0);
    }
    let point = AutomationPoint {
        tick: Tick::ZERO,
        value: start,
        curve: Curve::Linear,
    };
    Ok(Command::Batch(vec![
        Command::AddNode {
            id: node,
            node: automation,
        },
        Command::AddLane {
            id: lane,
            lane: AutomationLane::new(Endpoint::new(node, AUTOMATION_VALUE), vec![point]),
        },
        spare::wire(Endpoint::new(node, "out"), target.clone()),
    ]))
}

/// The Automation node's `min`, `max` and log taper, and the lane's first
/// value (0 to 1), that leave the sound as it is.
///
/// A parameter a wire *replaces* gets its own range and the position its
/// current value sits at. One a wire *offsets* (a filter's cutoff, say) takes
/// the signal as a move along its travel, with 0 meaning no move, so the node
/// ranges over -1 to 1 travel and starts in the middle: the parameter's own
/// value stays the base the lane moves around.
pub(super) fn range_and_start(info: &ParamInfo, current: f32) -> (f32, f32, bool, f32) {
    if info.is_offset() {
        (-1.0, 1.0, false, 0.5)
    } else {
        let log = info.taper == Taper::Log && info.min > 0.0 && info.max > info.min;
        (info.min, info.max, log, info.position(current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_core::group::{GROUP, GROUP_OUTPUT, ports_config};
    use noodle_engine::OUTPUT_ID;
    use noodle_engine::Settings;
    use noodle_nodes::SVF_ID;

    const SETTINGS: Settings = Settings {
        sample_rate: 48_000.0,
        max_frames: 512,
        channels: 2,
    };

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
    /// output. Returns the filter.
    fn lead(session: &mut Session) -> NodeId {
        let group = add(
            session,
            Node::new(GROUP).with_config(Config::new().with("name", Value::Text("Lead".into()))),
        );
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
        filter
    }

    fn render(session: &Session) -> Vec<f32> {
        let mut registry = Registry::with_builtins();
        let _library = noodle_nodes::register_all(&mut registry);
        let rendered =
            noodle_engine::render_project(session.project(), &registry, SETTINGS, 4_800).unwrap();
        assert!(
            rendered.diagnostics.is_empty(),
            "{:?}",
            rendered.diagnostics
        );
        assert!(rendered.samples.iter().any(|&x| x != 0.0), "silent");
        rendered.samples
    }

    fn automation_nodes(session: &Session) -> Vec<NodeId> {
        let nodes = session.project().graph().nodes();
        let found = nodes.filter(|(_, n)| n.type_id == AUTOMATION_NODE_ID);
        found.map(|(id, _)| id).collect()
    }

    /// The effects of Automate on a parameter a wire replaces: the node takes
    /// the parameter's range and taper, and the lane starts at its value.
    #[test]
    fn automating_a_replaced_parameter_copies_its_range_and_value() {
        let mut session = Session::new(Nodes::all());
        lead(&mut session);
        let saw = session
            .project()
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == "noodle.osc.saw")
            .map(|(id, _)| id)
            .unwrap();
        let target = Endpoint::new(saw, "frequency");
        let before = render(&session);
        session.edit([Edit::Automate(target.clone())]);
        let [node] = automation_nodes(&session)[..] else {
            panic!("one Automation node")
        };
        let project = session.project();
        let made = project.graph().node(node).unwrap();
        assert_eq!(made.parent, project.graph().node(saw).unwrap().parent);
        assert_eq!(project.graph().source(&target).unwrap().node, node);
        let (_, lane) = project
            .lane_for(&Endpoint::new(node, AUTOMATION_VALUE))
            .unwrap();
        assert_eq!(lane.points.len(), 1);
        assert_eq!(
            made.config.get(AUTOMATION_LABEL),
            Some(&Value::Text("Lead › Frequency".into()))
        );
        // The same sound, to a tolerance of the exp/log round trip.
        let after = render(&session);
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(&after) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    /// The issue's case: a filter cutoff inside a synth group.
    #[test]
    fn automating_a_filter_cutoff_in_a_group_is_one_undo_step_and_sounds_the_same() {
        let mut session = Session::new(Nodes::all());
        let filter = lead(&mut session);
        let cutoff = Endpoint::new(filter, "cutoff");
        let before = render(&session);
        let nodes = session.project().graph().nodes().count();

        session.edit([Edit::Automate(cutoff.clone())]);
        let [node] = automation_nodes(&session)[..] else {
            panic!("one Automation node")
        };
        let project = session.project();
        let made = project.graph().node(node).unwrap();
        let group = project.graph().node(filter).unwrap().parent;
        assert!(group.is_some());
        assert_eq!(made.parent, group, "in the parameter's own group");
        assert_eq!(
            project.graph().source(&cutoff),
            Some(&Endpoint::new(node, "out"))
        );
        assert_eq!(
            made.config.get(AUTOMATION_LABEL),
            Some(&Value::Text("Lead › Cutoff".into()))
        );
        let (_, lane) = project
            .lane_for(&Endpoint::new(node, AUTOMATION_VALUE))
            .expect("a lane on the node's value");
        assert_eq!(lane.points.len(), 1);
        assert_eq!(lane.points[0].tick, Tick::ZERO);

        // The sound doesn't change.
        let after = render(&session);
        assert_eq!(before.len(), after.len());
        let worst = before
            .iter()
            .zip(&after)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-3, "worst difference {worst}");

        // A wired parameter can't be automated again.
        assert!(refusal(session.project(), &cutoff).is_some());
        session.edit([Edit::Automate(cutoff)]);
        assert_eq!(automation_nodes(&session).len(), 1);

        // One undo takes the node, the lane and the wire away.
        session.undo();
        assert!(automation_nodes(&session).is_empty());
        assert_eq!(session.project().graph().nodes().count(), nodes);
        assert_eq!(session.project().lanes().count(), 0);
        assert_eq!(render(&session), before);
    }

    #[test]
    fn a_replaced_log_parameter_gets_its_range_and_a_log_taper() {
        let info = ParamInfo::new(20.0, 20_000.0, 1_000.0).log();
        let (min, max, log, start) = range_and_start(&info, 632.455_5);
        assert_eq!((min, max, log), (20.0, 20_000.0, true));
        assert!((start - 0.5).abs() < 1e-4, "{start}");
        let linear = ParamInfo::new(-60.0, 12.0, 0.0);
        assert_eq!(range_and_start(&linear, -24.0), (-60.0, 12.0, false, 0.5));
        // An offsetting parameter keeps its own value as the base.
        assert_eq!(
            range_and_start(&info.offset(), 3_000.0),
            (-1.0, 1.0, false, 0.5)
        );
    }
}
