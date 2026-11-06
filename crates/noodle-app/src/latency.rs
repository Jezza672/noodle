//! Where latency comes from and where it was made up (delay compensation),
//! read from the engine's plan and put into words for the editor.
//!
//! A node reports how late its own output sounds (a plugin's latency, a
//! lookahead, a Synthesize's frame). Where paths of different lateness meet,
//! the engine holds the early ones back. Both are shown: a badge on the node
//! and a panel in its properties.

use noodle_core::NodeId;

use crate::session::Session;

/// What one node adds, and what was held back at its inputs.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Reading {
    /// The latency the node reports, or the longest of the nodes inside a
    /// group, in samples.
    pub own: u32,
    /// How late the node's output sounds against the timeline, in samples.
    pub arrival: u32,
    /// The inputs delayed to line up, by name, with the samples held.
    pub held: Vec<(String, u32)>,
}

impl Reading {
    fn is_quiet(&self) -> bool {
        self.own == 0 && self.held.is_empty()
    }
}

/// Samples as milliseconds: "43 ms", "1.5 ms", or "0.3 ms" for a short one.
pub fn ms(samples: u32, rate: f32) -> String {
    let ms = samples as f32 * 1000.0 / rate.max(1.0);
    if ms >= 10.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.1} ms")
    }
}

/// What delay compensation did at `node`, or nothing if no latency reaches
/// it. A group has the longest latency of what is inside it (paths inside
/// run in parallel, so a sum would overstate it) and the latest arrival.
pub fn reading(session: &Session, node: NodeId) -> Option<Reading> {
    let report = session.plan_latency();
    if report.nodes.is_empty() {
        return None;
    }
    let graph = session.project().graph();
    let mut total = Reading::default();
    for (&id, latency) in &report.nodes {
        let inside = id == node || is_inside(session, id, node);
        if !inside {
            continue;
        }
        total.own = total.own.max(latency.own);
        total.arrival = total.arrival.max(latency.arrival);
        let Some(placed) = graph.node(id) else {
            continue;
        };
        let layout = session
            .registry()
            .get(&placed.type_id)
            .and_then(|t| t.layout(&placed.config).ok());
        for held in &latency.held {
            let name = layout
                .as_ref()
                .and_then(|l| {
                    if held.spectral {
                        l.spectral_inputs
                            .get(held.input)
                            .map(|p| p.name.to_string())
                    } else {
                        l.inputs.get(held.input).map(|p| p.name.to_string())
                    }
                })
                .unwrap_or_else(|| format!("input {}", held.input + 1));
            let name = if id == node {
                name
            } else {
                format!(
                    "{} {name}",
                    crate::editor::friendly_type_name(&placed.type_id)
                )
            };
            total.held.push((name, held.samples));
        }
    }
    total.held.sort();
    (!total.is_quiet()).then_some(total)
}

fn is_inside(session: &Session, id: NodeId, group: NodeId) -> bool {
    let graph = session.project().graph();
    let mut parent = graph.node(id).and_then(|n| n.parent);
    while let Some(p) = parent {
        if p == group {
            return true;
        }
        parent = graph.node(p).and_then(|n| n.parent);
    }
    false
}

/// The short text on a node's header: what it adds, or what it holds back.
pub fn badge(reading: &Reading, rate: f32) -> String {
    if reading.own > 0 {
        format!("+{}", ms(reading.own, rate))
    } else {
        let held = reading.held.iter().map(|h| h.1).max().unwrap_or(0);
        format!("holds {}", ms(held, rate))
    }
}

/// The header badge of every node that adds or holds back latency, with the
/// tooltip that explains it.
pub fn badges(session: &Session) -> std::collections::HashMap<NodeId, String> {
    let mut out = std::collections::HashMap::new();
    if session.plan_latency().nodes.is_empty() {
        return out;
    }
    let rate = session.engine_sample_rate().unwrap_or(48_000.0);
    for (id, _) in session.project().graph().nodes() {
        if let Some(r) = reading(session, id) {
            out.insert(id, badge(&r, rate));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milliseconds_keep_a_decimal_below_ten() {
        assert_eq!(ms(480, 48_000.0), "10 ms");
        assert_eq!(ms(72, 48_000.0), "1.5 ms");
        assert_eq!(ms(2_048, 48_000.0), "43 ms");
    }

    #[test]
    fn a_latent_node_shows_what_it_adds_and_a_held_one_what_it_holds() {
        let latent = Reading {
            own: 2_048,
            arrival: 2_048,
            held: vec![],
        };
        assert_eq!(badge(&latent, 48_000.0), "+43 ms");
        let held = Reading {
            own: 0,
            arrival: 2_048,
            held: vec![("In 2".into(), 2_048)],
        };
        assert_eq!(badge(&held, 48_000.0), "holds 43 ms");
    }
}
