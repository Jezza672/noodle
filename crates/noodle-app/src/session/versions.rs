//! Vary per version: a graph edit, nothing more. It adds a Version Value node
//! beside a parameter, every slot holding the parameter's current value, and
//! a wire from the node's `out` to the parameter, as one undo step. If the
//! project has no versions yet it also starts the list, so there is
//! something to vary. The parameter is afterwards an ordinary wired
//! parameter.

use noodle_core::{
    Command, Config, Endpoint, Node, NodeId, Project, VERSION_COUNT_KEY, VERSION_VALUE_ID, Value,
    spare, version_slot_key,
};
use noodle_engine::{InputKind, Registry};
use noodle_nodes::{VERSION_MAX, VERSION_MIN, VERSION_TAPER};

/// How far left of its target the new node sits.
const OFFSET_X: f32 = 260.0;

/// The versions a project starts with when the first parameter is varied.
pub const STARTING_VERSIONS: [&str; 2] = ["Main", "Alt"];

/// Why Vary per version can't be used on `target`, for the menu entry's
/// tooltip.
pub fn refusal(project: &Project, target: &Endpoint) -> Option<&'static str> {
    if project.graph().source(target).is_some() {
        Some("This parameter already has a wire")
    } else if project.lane_for(target).is_some() {
        Some("This parameter already has an automation lane")
    } else {
        None
    }
}

/// The command that varies `target` per version, with `node` as the new
/// node's ID.
pub fn command(
    project: &Project,
    registry: &Registry,
    target: &Endpoint,
    node: NodeId,
) -> Result<Command, String> {
    if let Some(reason) = refusal(project, target) {
        return Err(reason.to_owned());
    }
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
        return Err("only a parameter can vary per version".to_owned());
    };
    let current = target_node
        .params
        .get(&target.port)
        .copied()
        .unwrap_or(info.default);
    let (min, max, log, start) = super::automate::range_and_start(info, current);

    let mut commands = Vec::new();
    let count = if project.versions().is_empty() {
        commands.push(Command::SetVersions {
            names: STARTING_VERSIONS.iter().map(|s| (*s).to_owned()).collect(),
            active: 0,
        });
        STARTING_VERSIONS.len()
    } else {
        project.versions().len()
    };
    let mut varied = Node::new(VERSION_VALUE_ID)
        .at(target_node.position.x - OFFSET_X, target_node.position.y)
        .with_config(Config::new().with(VERSION_COUNT_KEY, Value::Int(count as i64)));
    varied.parent = target_node.parent;
    // Parameters left at their defaults (0 to 1, linear) aren't stored.
    if min != 0.0 {
        varied = varied.with_param(VERSION_MIN, min);
    }
    if max != 1.0 {
        varied = varied.with_param(VERSION_MAX, max);
    }
    if log {
        varied = varied.with_param(VERSION_TAPER, 1.0);
    }
    if start != 0.0 {
        for slot in 0..count {
            varied = varied.with_param(version_slot_key(slot), start);
        }
    }
    commands.push(Command::AddNode {
        id: node,
        node: varied,
    });
    commands.push(spare::wire(Endpoint::new(node, "out"), target.clone()));
    Ok(Command::Batch(commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_nodes::SVF_ID;

    fn session_with_filter() -> (Session, NodeId) {
        let mut session = Session::new(Nodes::all());
        let filter = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id: filter,
            node: Node::new(SVF_ID).with_param("cutoff", 1000.0),
        })]);
        (session, filter)
    }

    #[test]
    fn it_starts_the_list_and_wires_a_node_holding_the_current_value_in_every_slot() {
        let (mut session, filter) = session_with_filter();
        let cutoff = Endpoint::new(filter, "cutoff");
        session.edit([Edit::VaryPerVersion(cutoff.clone())]);
        let project = session.project();
        assert_eq!(project.versions(), ["Main", "Alt"]);
        let source = project.graph().source(&cutoff).expect("wired").clone();
        let node = project.graph().node(source.node).unwrap();
        assert_eq!(node.type_id, VERSION_VALUE_ID);
        // Both slots sit where the cutoff was, so nothing changes by itself.
        assert_eq!(node.params.get("v1"), node.params.get("v2"));
        assert!(node.params.get("v1").is_some_and(|v| *v > 0.0));
        // One undo step takes the node, the wire and the list away.
        session.undo();
        assert!(session.project().versions().is_empty());
        assert!(session.project().graph().source(&cutoff).is_none());
    }

    #[test]
    fn a_second_parameter_joins_the_existing_list() {
        let (mut session, filter) = session_with_filter();
        session.edit([Edit::VaryPerVersion(Endpoint::new(filter, "cutoff"))]);
        let add = session.project().add_version_command("TV").unwrap();
        session.edit([Edit::Apply(add)]);
        session.edit([Edit::VaryPerVersion(Endpoint::new(filter, "resonance"))]);
        let project = session.project();
        assert_eq!(project.versions().len(), 3);
        let source = project
            .graph()
            .source(&Endpoint::new(filter, "resonance"))
            .unwrap();
        let node = project.graph().node(source.node).unwrap();
        assert_eq!(noodle_core::version_slots(node), 3);
    }

    #[test]
    fn a_wired_parameter_cannot_vary() {
        let (mut session, filter) = session_with_filter();
        let cutoff = Endpoint::new(filter, "cutoff");
        session.edit([Edit::VaryPerVersion(cutoff.clone())]);
        assert!(refusal(session.project(), &cutoff).is_some());
    }
}
