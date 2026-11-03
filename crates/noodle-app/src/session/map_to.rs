//! The Map to gesture: a wire dropped on a parameter with Alt held goes
//! through a new Remap, whose output range is the parameter's own range and
//! whose curve is linear, as one undo step. One macro knob fanned to many
//! parameters is then one gesture per target, and each target's range and
//! curve are edited on its own Remap, where they can be seen.

use noodle_core::{Command, Endpoint, Node, NodeId, Project, spare};
use noodle_engine::{InputKind, Registry};
use noodle_nodes::REMAP_ID;

/// How far left of its target the Remap sits.
const OFFSET_X: f32 = 220.0;

/// The command that wires `from` to `target` through a new Remap with ID
/// `remap`.
pub fn command(
    project: &Project,
    registry: &Registry,
    from: &Endpoint,
    target: &Endpoint,
    remap: NodeId,
) -> Result<Command, String> {
    let graph = project.graph();
    if graph.source(target).is_some() {
        return Err("this parameter already has a wire".to_owned());
    }
    if project.lane_for(target).is_some() {
        return Err("this parameter already has an automation lane".to_owned());
    }
    let node = graph.node(target.node).ok_or("the node is gone")?;
    let layout = registry
        .get(&node.type_id)
        .ok_or("the node's type is unknown")?
        .layout(&node.config)
        .map_err(|e| e.to_string())?;
    let input = layout
        .inputs
        .iter()
        .find(|i| i.key == target.port)
        .ok_or("the parameter is gone")?;
    let InputKind::Param(info) = &input.kind else {
        return Err("only a parameter can be mapped to".to_owned());
    };
    // A parameter a wire offsets takes the signal as a move along its
    // travel, so the Remap spans -1 to 1; one a wire replaces gets its range.
    let (low, high) = if info.is_offset() {
        (-1.0, 1.0)
    } else {
        (info.min, info.max)
    };
    let mut mapper = Node::new(REMAP_ID).at(node.position.x - OFFSET_X, node.position.y);
    mapper.parent = node.parent;
    if low != 0.0 {
        mapper = mapper.with_param("out_min", low);
    }
    if high != 1.0 {
        mapper = mapper.with_param("out_max", high);
    }
    Ok(Command::Batch(vec![
        Command::AddNode {
            id: remap,
            node: mapper,
        },
        spare::wire(from.clone(), Endpoint::new(remap, "in")),
        spare::wire(Endpoint::new(remap, "out"), target.clone()),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_nodes::SVF_ID;

    fn session_with_filter() -> (Session, NodeId, NodeId) {
        let mut session = Session::new(Nodes::all());
        let knob = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id: knob,
            node: Node::new(noodle_nodes::AUTOMATION_NODE_ID),
        })]);
        let filter = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id: filter,
            node: Node::new(SVF_ID),
        })]);
        (session, knob, filter)
    }

    #[test]
    fn mapping_inserts_a_remap_with_the_targets_range_in_one_undo_step() {
        let (mut session, knob, filter) = session_with_filter();
        let before = session.project().graph().nodes().count();
        session.edit([Edit::MapTo {
            from: Endpoint::new(knob, "out"),
            to: Endpoint::new(filter, "cutoff"),
            prepare: Vec::new(),
        }]);
        let graph = session.project().graph();
        assert_eq!(graph.nodes().count(), before + 1);
        let (id, remap) = graph.nodes().find(|(_, n)| n.type_id == REMAP_ID).unwrap();
        // The cutoff is offset by a wire, so the Remap spans -1 to 1.
        assert_eq!(remap.params.get("out_min"), Some(&-1.0));
        assert_eq!(
            graph.source(&Endpoint::new(filter, "cutoff")),
            Some(&Endpoint::new(id, "out"))
        );
        session.undo();
        assert_eq!(session.project().graph().nodes().count(), before);
    }

    #[test]
    fn a_wired_parameter_refuses() {
        let (mut session, knob, filter) = session_with_filter();
        let to = Endpoint::new(filter, "cutoff");
        session.edit([Edit::Apply(spare::wire(
            Endpoint::new(knob, "out"),
            to.clone(),
        ))]);
        session.edit([Edit::MapTo {
            from: Endpoint::new(knob, "out"),
            to,
            prepare: Vec::new(),
        }]);
        assert!(session.message().is_some_and(|m| m.contains("already")));
    }
}
