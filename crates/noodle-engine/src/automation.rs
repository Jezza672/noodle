//! Automation lanes in the engine: each lane becomes a hidden node wired into
//! the parameter it drives, so it modulates what a wire would and the target
//! needs no support for it. See "Automation" in docs/ARCHITECTURE.md.

use std::borrow::Cow;
use std::fmt::Write as _;

use noodle_core::{
    AutomationLane, AutomationPoint, Config, Connection, Curve, Endpoint, Graph, LaneId, Node,
    NodeId, Tick, Value,
};

use crate::{
    Context, Diagnostic, InputKind, Instance, Io, Layout, Location, NodeError, NodeInfo, NodeType,
    ParamKind, Problem, Registry, Setup,
};

pub const AUTOMATION_ID: &str = "noodle.internal.automation";

/// The category of node types the user never adds by hand.
pub const INTERNAL_CATEGORY: &str = "Internal";

const OUT: usize = 0;
const POINTS: &str = "points";
const SMOOTHING_MS: &str = "smoothing_ms";

/// Lanes to compile, as the project lists them.
pub type Lanes<'a> = [(LaneId, &'a AutomationLane)];

/// The node ID a lane's source gets in the compiled graph. Counted down from
/// the top, so it can't clash with a project node, and it's the same every
/// compile, so the node's state carries over.
fn source_id(lane: LaneId) -> NodeId {
    NodeId(u64::MAX - lane.0)
}

/// Returns `graph` with each lane's source node added and wired into its
/// target. A lane that can't be applied is left out, with a diagnostic where
/// the user can do something about it.
pub(crate) fn add_lanes<'a>(
    graph: &'a Graph,
    lanes: &Lanes<'_>,
    registry: &Registry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Cow<'a, Graph> {
    let mut nodes = Vec::new();
    let mut connections = Vec::new();
    for &(id, lane) in lanes {
        let target = &lane.target;
        if lane.points.is_empty() {
            continue;
        }
        // A node that isn't here (removed, or a group's boundary, which
        // flattening drops) has nothing to drive.
        let Some(smoothing) = graph.node(target.node).and_then(|node| {
            let layout = registry.get(&node.type_id)?.layout(&node.config).ok()?;
            Some(match layout.inputs.iter().find(|i| i.key == target.port) {
                None => Err(Problem::UnknownPort(target.clone())),
                Some(input) => match &input.kind {
                    InputKind::Param(info) => Ok(match info.kind {
                        ParamKind::Continuous { smoothing_ms } => smoothing_ms,
                        ParamKind::Stepped { .. } => 0.0,
                    }),
                    InputKind::Audio => Err(Problem::NotAParam(target.port.clone())),
                },
            })
        }) else {
            continue;
        };
        let smoothing = match smoothing {
            Ok(smoothing) => smoothing,
            Err(problem) => {
                diagnostics.push(Diagnostic::node(target.node, problem));
                continue;
            }
        };
        if graph.source(target).is_some() {
            diagnostics.push(Diagnostic {
                location: Location::Wire(target.clone()),
                problem: Problem::LaneOverridden,
            });
            continue;
        }
        let source = source_id(id);
        nodes.push((
            source,
            Node {
                type_id: AUTOMATION_ID.into(),
                params: Default::default(),
                config: Config::new()
                    .with(POINTS, Value::Text(encode(&lane.points)))
                    .with(SMOOTHING_MS, Value::Float(f64::from(smoothing))),
                position: Default::default(),
                parent: None,
            },
        ));
        connections.push(Connection {
            from: Endpoint::new(source, "out"),
            to: target.clone(),
        });
    }
    if nodes.is_empty() {
        return Cow::Borrowed(graph);
    }
    let all_nodes = graph
        .nodes()
        .map(|(id, node)| (id, node.clone()))
        .chain(nodes);
    let all_connections = graph.connections().chain(connections);
    Cow::Owned(
        Graph::from_parts(all_nodes, all_connections)
            .expect("lane sources have fresh IDs and wire into existing nodes"),
    )
}

/// A lane's points as text, since a node's config can't hold a list. Floats
/// print in the shortest form that reads back exactly.
fn encode(points: &[AutomationPoint]) -> String {
    let mut text = String::new();
    for point in points {
        let curve = match point.curve {
            Curve::Linear => 'l',
            Curve::Hold => 'h',
        };
        let _ = write!(text, "{} {} {curve};", point.tick.0, point.value);
    }
    text
}

fn decode(text: &str) -> Option<Vec<AutomationPoint>> {
    text.split_terminator(';')
        .map(|item| {
            let mut fields = item.split(' ');
            let tick = Tick(fields.next()?.parse().ok()?);
            let value = fields.next()?.parse().ok()?;
            let curve = match fields.next()? {
                "l" => Curve::Linear,
                "h" => Curve::Hold,
                _ => return None,
            };
            Some(AutomationPoint { tick, value, curve })
        })
        .collect()
}

/// The hidden source node: writes a lane's value at the transport's position.
pub struct Automation;

static INFO: NodeInfo = NodeInfo {
    id: AUTOMATION_ID,
    version: 1,
    name: "Automation",
    category: INTERNAL_CATEGORY,
};

impl NodeType for Automation {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        points(config)?;
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let smoothing_ms = match setup.config.get(SMOOTHING_MS) {
            Some(&Value::Float(ms)) => ms.max(0.0),
            _ => 0.0,
        };
        Ok(Instance::realtime(LaneSource {
            points: points(setup.config)?,
            smoothing_ms,
        }))
    }
}

fn points(config: &Config) -> Result<Vec<AutomationPoint>, NodeError> {
    match config.get(POINTS) {
        Some(Value::Text(text)) => decode(text),
        _ => None,
    }
    .ok_or_else(|| NodeError::config("an automation lane's points are unreadable"))
}

struct LaneSource {
    points: Vec<AutomationPoint>,
    smoothing_ms: f64,
}

impl LaneSource {
    /// The value at `tick`, given `ramp` ticks to move across a hold step.
    ///
    /// A linear segment is continuous, so it needs nothing. A hold segment
    /// that follows a hold segment starts with a jump, which is spread over
    /// `ramp` ticks (no more than the segment) so it doesn't click. An
    /// unconnected parameter smooths its own changes, but a wired one takes
    /// the signal as it comes, hence this. Being a function of the tick
    /// alone, it gives the same samples however the blocks fall.
    fn value_at(&self, tick: f64, ramp: f64) -> f32 {
        let points = &self.points;
        let after = points.partition_point(|p| p.tick.0 as f64 <= tick);
        let Some(i) = after.checked_sub(1) else {
            return points[0].value;
        };
        let (before, next) = (&points[i], points.get(after));
        let Some(next) = next else {
            return before.value;
        };
        let along = tick - before.tick.0 as f64;
        match before.curve {
            Curve::Linear => {
                let span = (next.tick.0 - before.tick.0) as f64;
                before.value + (next.value - before.value) * (along / span) as f32
            }
            Curve::Hold => {
                let from = i.checked_sub(1).map(|j| &points[j]);
                match from {
                    Some(prev) if prev.curve == Curve::Hold => {
                        let ramp = ramp.min((next.tick.0 - before.tick.0) as f64);
                        if along < ramp {
                            let t = (along / ramp) as f32;
                            prev.value + (before.value - prev.value) * t
                        } else {
                            before.value
                        }
                    }
                    _ => before.value,
                }
            }
        }
    }
}

impl crate::Node for LaneSource {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let transport = &ctx.transport;
        let ticks_per_second = transport.bpm / 60.0 * noodle_core::TICKS_PER_QUARTER as f64;
        let ticks_per_sample = if transport.playing {
            ticks_per_second / f64::from(ctx.sample_rate)
        } else {
            0.0
        };
        let ramp = self.smoothing_ms / 1000.0 * ticks_per_second;
        let out = io.outputs[OUT].lane_mut(0, 0);
        for (i, sample) in out.iter_mut().enumerate() {
            *sample = self.value_at(transport.tick + i as f64 * ticks_per_sample, ramp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_survive_the_trip_through_config_text() {
        let points: Vec<_> = [0.1f32, -0.0, 1e-30, 123456.79, f32::MIN_POSITIVE]
            .iter()
            .enumerate()
            .map(|(i, &value)| AutomationPoint {
                tick: Tick(i as i64 * 7919),
                value,
                curve: if i % 2 == 0 {
                    Curve::Linear
                } else {
                    Curve::Hold
                },
            })
            .collect();
        let back = decode(&encode(&points)).unwrap();
        assert_eq!(back.len(), points.len());
        for (a, b) in points.iter().zip(&back) {
            assert_eq!((a.tick, a.curve), (b.tick, b.curve));
            assert_eq!(a.value.to_bits(), b.value.to_bits());
        }
        assert_eq!(decode("1 2 x;"), None);
        assert_eq!(decode("1 2;"), None);
    }
}
