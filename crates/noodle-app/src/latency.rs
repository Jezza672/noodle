//! Where latency comes from and where it was made up (delay compensation),
//! read from the engine's plan and put into words for the editor.
//!
//! A node reports how late its own output sounds (a plugin's latency, a
//! lookahead, a Synthesize's frame). Where paths of different lateness meet,
//! the engine holds the early ones back. Both are shown: a badge on the node
//! and a panel in its properties.

use std::time::Duration;

use noodle_core::NodeId;
use serde::{Deserialize, Serialize};

use crate::session::Session;

/// What the device buffer does while tracks are armed or monitored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitoringLatency {
    /// The smallest buffer while a track is armed or monitored, the usual one
    /// otherwise, and a large one after a while with nothing to do.
    #[default]
    Auto,
    /// The smallest buffer all the time.
    AlwaysLow,
    /// Leave the buffer alone (the size chosen in the audio settings, or the
    /// usual one).
    Off,
}

impl MonitoringLatency {
    pub const ALL: [Self; 3] = [Self::Auto, Self::AlwaysLow, Self::Off];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::AlwaysLow => "Always low",
            Self::Off => "Off",
        }
    }
}

/// How big the device buffer is asked to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferTier {
    /// Armed or monitored: as little delay as the device allows.
    Low,
    /// The usual size, which is what the stream opens with.
    Normal,
    /// Nothing is playing or armed: fewer, larger callbacks, which cost less
    /// power.
    Relaxed,
}

impl BufferTier {
    /// The size asked for, in frames. `Normal` leaves the choice to the
    /// device's usual live size.
    pub fn request(self) -> Option<u32> {
        match self {
            Self::Low => Some(64),
            Self::Normal => None,
            Self::Relaxed => Some(1024),
        }
    }
}

/// The Monitoring latency row of the Audio settings, and what it is doing now.
/// A change takes effect at once (the buffer follows when the transport is
/// stopped) and is saved.
pub fn monitoring_row(ui: &mut egui::Ui, session: &mut Session) {
    let mut setting = session.monitoring_latency();
    ui.horizontal(|ui| {
        ui.label("Monitoring latency");
        egui::ComboBox::from_id_salt("monitoring latency")
            .selected_text(setting.label())
            .show_ui(ui, |ui| {
                for choice in MonitoringLatency::ALL {
                    ui.selectable_value(&mut setting, choice, choice.label());
                }
            })
            .response
            .on_hover_text(
                "Auto: the smallest device buffer while a track is armed or monitored, \
                 a large one when nothing has been going on for a while. Always low: the \
                 smallest all the time. Off: leave the buffer as it is. A buffer size \
                 chosen below is never changed.",
            );
    });
    if setting != session.monitoring_latency() {
        session.set_monitoring_latency(setting);
        crate::prefs::remember_monitoring_latency(setting);
    }
    if let Some(line) = monitoring_readout(session) {
        ui.weak(line);
    }
}

/// What monitoring is doing: the buffer it asked for and what the monitored
/// path adds on top.
pub fn monitoring_readout(session: &Session) -> Option<String> {
    let rate = session.latency_report()?.sample_rate;
    let mut parts = Vec::new();
    if let Some(tier) = session.buffer_tier() {
        parts.push(match tier {
            BufferTier::Low => "Buffer: low while armed or monitored".to_owned(),
            BufferTier::Normal => "Buffer: usual size".to_owned(),
            BufferTier::Relaxed => "Buffer: large while idle".to_owned(),
        });
    }
    if let Some(samples) = session.plan_latency().monitored {
        parts.push(if samples == 0 {
            "Monitored path: no plugin latency".to_owned()
        } else {
            format!(
                "Monitored path: {} of plugin latency, not held back by other tracks",
                ms(samples, rate)
            )
        });
    }
    (!parts.is_empty()).then(|| parts.join(". "))
}

/// How long nothing has to be playing or armed before the buffer relaxes.
pub const RELAX_AFTER: Duration = Duration::from_secs(10);

/// The tier the device buffer should be at, `None` for no say in it.
/// `current` is the tier it is at. The buffer follows arming one way only:
/// once low it stays low until it has been idle long enough to relax, and a
/// relaxed one goes back to low and never to normal, so one session takes
/// few reopenings.
pub fn wanted_tier(
    setting: MonitoringLatency,
    live: bool,
    idle_long: bool,
    current: Option<BufferTier>,
) -> Option<BufferTier> {
    match setting {
        MonitoringLatency::Off => None,
        MonitoringLatency::AlwaysLow => Some(BufferTier::Low),
        MonitoringLatency::Auto if live => Some(BufferTier::Low),
        MonitoringLatency::Auto if idle_long => Some(BufferTier::Relaxed),
        MonitoringLatency::Auto => Some(current.unwrap_or(BufferTier::Normal)),
    }
}

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

#[cfg(test)]
mod tier_tests {
    use super::BufferTier::*;
    use super::MonitoringLatency::*;
    use super::*;

    #[test]
    fn auto_is_low_while_live_and_relaxes_after_idling() {
        assert_eq!(wanted_tier(Auto, true, false, None), Some(Low));
        assert_eq!(wanted_tier(Auto, false, false, None), Some(Normal));
        assert_eq!(wanted_tier(Auto, false, true, Some(Normal)), Some(Relaxed));
        // Not live any more but not idle long: stays low rather than bounce.
        assert_eq!(wanted_tier(Auto, false, false, Some(Low)), Some(Low));
        assert_eq!(
            wanted_tier(Auto, false, false, Some(Relaxed)),
            Some(Relaxed)
        );
        // Live again wakes a relaxed buffer.
        assert_eq!(wanted_tier(Auto, true, false, Some(Relaxed)), Some(Low));
    }

    #[test]
    fn always_low_and_off() {
        assert_eq!(wanted_tier(AlwaysLow, false, true, None), Some(Low));
        assert_eq!(wanted_tier(Off, true, false, Some(Low)), None);
    }

    #[test]
    fn the_sizes_asked_for() {
        assert_eq!(Low.request(), Some(64));
        assert_eq!(Normal.request(), None);
        assert_eq!(Relaxed.request(), Some(1024));
    }
}
