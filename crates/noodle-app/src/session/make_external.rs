//! Make external: right-click a parameter's port and a macro appears for it,
//! as one undo step. The macro is a *variable* (an Automation node with no
//! lane, a plain 0 to 1 knob) wired through a new Remap that matches the
//! parameter's range into the parameter. The variable is set so the
//! parameter's value is what it was, so the gesture changes nothing audible.
//! Further parameters can then be wired from the same variable, each through
//! its own Remap (the Map to gesture), which is also how one source drives
//! several targets at different depths.

use noodle_core::{Command, Config, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{InputKind, Registry};
use noodle_nodes::{AUTOMATION_LABEL, AUTOMATION_NODE_ID, AUTOMATION_VALUE};

use super::map_to;

/// How far left of the Remap the variable sits.
const OFFSET_X: f32 = 220.0;

/// The command that makes `target` external, with `variable` and `remap` as
/// the new nodes' IDs.
pub fn command(
    project: &Project,
    registry: &Registry,
    target: &Endpoint,
    variable: NodeId,
    remap: NodeId,
) -> Result<Command, String> {
    let graph = project.graph();
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
        return Err("only a parameter can be made external".to_owned());
    };
    let current = node
        .params
        .get(&target.port)
        .copied()
        .unwrap_or(info.default);
    // Where on the Remap's 0 to 1 input the parameter's current value sits.
    // A parameter a wire offsets takes the signal as a move along its travel,
    // so the middle means no move and the base value stays as it is.
    let start = if info.is_offset() {
        0.5
    } else {
        let span = info.max - info.min;
        if span == 0.0 {
            0.0
        } else {
            ((current - info.min) / span).clamp(0.0, 1.0)
        }
    };
    let mut knob = Node::new(AUTOMATION_NODE_ID)
        .at(node.position.x - 2.0 * OFFSET_X - 20.0, node.position.y)
        .with_config(Config::new().with(
            AUTOMATION_LABEL,
            Value::Text(format!("{} (macro)", input.name)),
        ));
    knob.parent = node.parent;
    if start != 0.0 {
        knob = knob.with_param(AUTOMATION_VALUE, start);
    }
    let wire = map_to::command(
        project,
        registry,
        &Endpoint::new(variable, "out"),
        target,
        remap,
    )?;
    Ok(Command::Batch(vec![
        Command::AddNode {
            id: variable,
            node: knob,
        },
        wire,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_nodes::{REMAP_ID, SVF_ID};

    fn session_with(node: Node) -> (Session, NodeId) {
        let mut session = Session::new(Nodes::all());
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        (session, id)
    }

    #[test]
    fn adds_a_variable_and_a_remap_in_one_undo_step() {
        let (mut session, filter) = session_with(Node::new(SVF_ID).with_param("resonance", 0.5));
        let before = session.project().graph().nodes().count();
        let target = Endpoint::new(filter, "resonance");
        session.edit([Edit::MakeExternal {
            target: target.clone(),
            prepare: Vec::new(),
        }]);
        let graph = session.project().graph();
        assert_eq!(graph.nodes().count(), before + 2);
        let (remap, _) = graph.nodes().find(|(_, n)| n.type_id == REMAP_ID).unwrap();
        let (knob, node) = graph
            .nodes()
            .find(|(_, n)| n.type_id == AUTOMATION_NODE_ID)
            .unwrap();
        assert_eq!(graph.source(&target), Some(&Endpoint::new(remap, "out")));
        assert_eq!(
            graph.source(&Endpoint::new(remap, "in")),
            Some(&Endpoint::new(knob, "out"))
        );
        assert!(node.params.get(AUTOMATION_VALUE).is_some_and(|v| *v > 0.0));
        session.undo();
        assert_eq!(session.project().graph().nodes().count(), before);
    }

    /// The Remap's output for the variable's value, as the parameter hears it.
    fn heard(session: &Session) -> f32 {
        let graph = session.project().graph();
        let (_, knob) = graph
            .nodes()
            .find(|(_, n)| n.type_id == AUTOMATION_NODE_ID)
            .unwrap();
        let (_, remap) = graph.nodes().find(|(_, n)| n.type_id == REMAP_ID).unwrap();
        let value = knob.params.get(AUTOMATION_VALUE).copied().unwrap_or(0.0);
        let low = remap.params.get("out_min").copied().unwrap_or(0.0);
        let high = remap.params.get("out_max").copied().unwrap_or(1.0);
        low + value * (high - low)
    }

    #[test]
    fn the_variable_is_set_so_a_replaced_value_stays_put() {
        let (mut session, pan) =
            session_with(Node::new(noodle_nodes::PAN_ID).with_param("pan", 0.5));
        session.edit([Edit::MakeExternal {
            target: Endpoint::new(pan, "pan"),
            prepare: Vec::new(),
        }]);
        assert!((heard(&session) - 0.5).abs() < 1e-5, "{}", heard(&session));
    }

    #[test]
    fn an_offset_parameter_starts_with_no_move() {
        let (mut session, filter) = session_with(Node::new(SVF_ID).with_param("resonance", 0.5));
        session.edit([Edit::MakeExternal {
            target: Endpoint::new(filter, "resonance"),
            prepare: Vec::new(),
        }]);
        // Halfway along -1 to 1 is an offset of 0: the base value stays.
        assert!(heard(&session).abs() < 1e-5, "{}", heard(&session));
    }

    #[test]
    fn a_wired_parameter_refuses_and_leaves_the_graph_alone() {
        let (mut session, filter) = session_with(Node::new(SVF_ID));
        let target = Endpoint::new(filter, "resonance");
        session.edit([Edit::MakeExternal {
            target: target.clone(),
            prepare: Vec::new(),
        }]);
        let count = session.project().graph().nodes().count();
        session.edit([Edit::MakeExternal {
            target,
            prepare: Vec::new(),
        }]);
        assert_eq!(session.project().graph().nodes().count(), count);
        assert!(session.message().is_some_and(|m| m.contains("already")));
    }
}
