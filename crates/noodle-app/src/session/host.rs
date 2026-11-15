//! A session whose engine a host calls: the plugin's side of the app. The
//! shell (`noodle-plugin`) gives the audio callback the [`HostBackend`] that
//! [`Session::start_host_engine`] makes, and everything else (edits, undo,
//! the graph's compile) goes on as in the app.

use std::sync::Arc;

use noodle_core::{Command, Config, Endpoint, Node, NodeId, Position, Value};
use noodle_engine::{HOST_DEVICE, INPUT_DEVICE_KEY, INPUT_ID, OUTPUT_DEVICE_KEY, OUTPUT_ID};
use noodle_io::{HOST_PARAMS, HostBackend, HostConfig, HostLink, Playback};
use noodle_nodes::{
    HOST_PARAM_ID, HOST_PARAM_NAME, HOST_PARAM_NAME_KEY, HOST_PARAM_SLOT, HOST_PARAM_SLOT_KEY,
    MIDI_IN_ID,
};

use super::{Audio, Edit, MAX_FRAMES, Session};
use crate::tempo_source::TempoSource;

/// The name of the library template the default instrument plays.
const LEAD_SYNTH: &str = "Lead Synth";

/// What the host told the plugin at activation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostSettings {
    pub sample_rate: f32,
    /// Channels the host takes (2 for stereo).
    pub out_channels: usize,
    /// Channels the host gives; 0 for an instrument with no audio input.
    pub in_channels: usize,
}

/// What a plugin instance starts as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// Host In to Host Out.
    Effect,
    /// Host MIDI In, a Lead Synth, Host Out.
    Instrument,
}

fn text(key: &str, value: &str) -> Config {
    let mut config = Config::new();
    config.set(key, Value::Text(value.to_owned()));
    config
}

impl Session {
    /// Makes the engine a host calls, and returns the backend for the host's
    /// audio thread. The session then compiles into it like any engine. The
    /// tempo source becomes the host's.
    pub fn start_host_engine(&mut self, host: HostSettings) -> Result<HostBackend, String> {
        self.close_stream();
        let (mut controller, backend) = noodle_io::host_engine(HostConfig {
            sample_rate: host.sample_rate,
            out_channels: host.out_channels,
            in_channels: host.in_channels,
            block: MAX_FRAMES,
            midi: self.midi.clone(),
            midi_out: self.midi_out.clone(),
            link: Arc::clone(&self.host_link),
            workers: 0,
        })
        .map_err(|error| error.to_string())?;
        controller.set_telemetry(&self.telemetry);
        self.clips
            .set_input_tap(Some(controller.transport().input_tap()));
        self.host_driven = true;
        self.audio = Some(Audio {
            playback: Playback::host(backend.settings(), "Host"),
            controller,
            devices: Vec::new(),
            monitor: Default::default(),
            fallback: None,
            underruns: 0,
        });
        self.set_tempo_source(TempoSource::Host);
        self.recompile();
        Ok(backend)
    }

    /// Ends the host engine (the plugin is deactivated). The project stays.
    pub fn stop_host_engine(&mut self) {
        self.close_stream();
    }

    /// Whether a host drives the engine.
    pub fn is_host_driven(&self) -> bool {
        self.host_driven
    }

    /// The host's parameter bank, tempo and play state.
    pub fn host_link(&self) -> &Arc<HostLink> {
        &self.host_link
    }

    /// The latency the engine adds, in frames, for the host to be told.
    pub fn host_latency(&self) -> u32 {
        self.audio
            .as_ref()
            .map_or(0, |audio| audio.controller.output_latency() as u32)
    }

    /// Replaces the project with a fresh one for `flavour`: the effect passes
    /// the host's audio through, the instrument plays a Lead Synth from the
    /// host's notes. It is one project like any other, so everything about
    /// it can be edited.
    pub fn new_host_project(&mut self, flavour: Flavour) {
        self.new_project();
        let input = self.new_node_id();
        let output = self.new_node_id();
        let mut commands = Vec::new();
        match flavour {
            Flavour::Effect => {
                commands.push(Command::AddNode {
                    id: input,
                    node: Node::new(INPUT_ID)
                        .with_config(text(INPUT_DEVICE_KEY, HOST_DEVICE))
                        .at(0.0, 0.0),
                });
                commands.push(Command::AddNode {
                    id: output,
                    node: Node::new(OUTPUT_ID)
                        .with_config(text(OUTPUT_DEVICE_KEY, HOST_DEVICE))
                        .at(400.0, 0.0),
                });
                commands.push(noodle_core::spare::wire(
                    Endpoint::new(input, "out"),
                    Endpoint::new(output, "in"),
                ));
            }
            Flavour::Instrument => {
                commands.push(Command::AddNode {
                    id: input,
                    node: Node::new(MIDI_IN_ID)
                        .with_config(text("port", HOST_DEVICE))
                        .at(0.0, 0.0),
                });
                commands.push(Command::AddNode {
                    id: output,
                    node: Node::new(OUTPUT_ID)
                        .with_config(text(OUTPUT_DEVICE_KEY, HOST_DEVICE))
                        .at(900.0, 0.0),
                });
                let found = self
                    .library()
                    .templates()
                    .find(|(_, t)| t.name == LEAD_SYNTH)
                    .map(|(id, t)| (id.to_owned(), t.clone()));
                match found {
                    Some((entry, template)) => {
                        let (synth, add) = template.instantiate(
                            &entry,
                            None,
                            Position { x: 300.0, y: 0.0 },
                            || self.new_node_id(),
                        );
                        commands.push(add);
                        commands.push(noodle_core::spare::wire(
                            Endpoint::new(input, "out"),
                            Endpoint::new(synth, "notes"),
                        ));
                        commands.push(noodle_core::spare::wire(
                            Endpoint::new(synth, "out"),
                            Endpoint::new(output, "in"),
                        ));
                    }
                    None => {
                        self.message = Some("The Lead Synth is missing from the library".into());
                    }
                }
            }
        }
        self.edit([Edit::Apply(Command::Batch(commands))]);
        // The starting point is not an edit to undo.
        self.saved_position = self.history.position();
    }

    /// The project, as the host's state chunk holds it.
    pub fn host_state(&self) -> String {
        self.project.to_ron()
    }

    /// Replaces the project with the host's saved one. Returns the reason if
    /// it can't be read, and keeps what it had.
    pub fn load_host_state(&mut self, text: &str) -> Result<(), String> {
        let project = noodle_core::Project::from_ron(text).map_err(|error| error.to_string())?;
        self.replace(project, None);
        Ok(())
    }

    /// What the host shows each of its 64 parameters as: the name of the Host
    /// Parameter node on that slot (the lowest node ID if several share it),
    /// `None` for a slot nothing in the project reads.
    pub fn host_param_names(&self) -> [Option<String>; HOST_PARAMS] {
        let mut names: [Option<(NodeId, String)>; HOST_PARAMS] = std::array::from_fn(|_| None);
        for (id, node) in self.project.graph().nodes() {
            if node.type_id != HOST_PARAM_ID {
                continue;
            }
            let Ok(slot) = usize::try_from(HOST_PARAM_SLOT.get_int(&node.config)) else {
                continue;
            };
            let Some(entry) = slot.checked_sub(1).and_then(|i| names.get_mut(i)) else {
                continue;
            };
            let name = HOST_PARAM_NAME.get_text(&node.config);
            let name = if name.is_empty() {
                format!("Param {slot}")
            } else {
                name
            };
            if entry.as_ref().is_none_or(|(owner, _)| id < *owner) {
                *entry = Some((id, name));
            }
        }
        names.map(|entry| entry.map(|(_, name)| name))
    }

    /// "Expose to host": adds a Host Parameter node on the first free slot,
    /// named `name`, and wires it into `node`'s parameter `port`, as one undo
    /// step. A parameter that already has a wire is left alone. Returns the
    /// slot (from 1) or why it couldn't.
    pub fn expose_to_host(
        &mut self,
        node: NodeId,
        port: &str,
        name: &str,
    ) -> Result<usize, String> {
        let graph = self.project.graph();
        if graph.node(node).is_none() {
            return Err("That node is gone".into());
        }
        let to = Endpoint::new(node, port);
        if graph.connections().any(|c| c.to == to) {
            return Err("That parameter already has a wire".into());
        }
        let taken: Vec<usize> = graph
            .nodes()
            .filter(|(_, n)| n.type_id == HOST_PARAM_ID)
            .filter_map(|(_, n)| usize::try_from(HOST_PARAM_SLOT.get_int(&n.config)).ok())
            .collect();
        let Some(slot) = (1..=HOST_PARAMS).find(|slot| !taken.contains(slot)) else {
            return Err(format!("All {HOST_PARAMS} host parameters are in use"));
        };
        let position = graph.node(node).map(|n| n.position).unwrap_or_default();
        let id = self.new_node_id();
        let mut config = Config::new();
        config.set(HOST_PARAM_SLOT_KEY, Value::Int(slot as i64));
        config.set(HOST_PARAM_NAME_KEY, Value::Text(name.to_owned()));
        let mut host_param = Node::new(HOST_PARAM_ID).with_config(config);
        host_param.parent = graph.node(node).and_then(|n| n.parent);
        host_param.position = Position {
            x: position.x - 260.0,
            y: position.y,
        };
        self.edit([Edit::Apply(Command::Batch(vec![
            Command::AddNode {
                id,
                node: host_param,
            },
            noodle_core::spare::wire(Endpoint::new(id, "value"), to),
        ]))]);
        Ok(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Nodes;
    use noodle_io::{HostEvents, HostMidi, HostTransport};

    fn run(backend: &mut HostBackend, frames: usize, midi: &[HostMidi], out: &mut [Vec<f32>; 2]) {
        let [l, r] = out;
        backend.process(
            frames,
            &[],
            &mut [l.as_mut_slice(), r.as_mut_slice()],
            HostEvents {
                midi,
                params: &[],
                transport: HostTransport {
                    playing: true,
                    tempo: Some(120.0),
                    beats: None,
                },
            },
            &mut |_| {},
        );
    }

    fn host() -> HostSettings {
        HostSettings {
            sample_rate: 48_000.0,
            out_channels: 2,
            in_channels: 0,
        }
    }

    #[test]
    fn the_default_instrument_plays_a_lead_synth_from_host_notes() {
        let mut session = Session::new(Nodes::all());
        session.new_host_project(Flavour::Instrument);
        assert!(
            session
                .project()
                .graph()
                .nodes()
                .any(|(_, n)| n.template.is_some()),
            "{:?}",
            session.message()
        );
        let mut backend = session.start_host_engine(host()).unwrap();
        let mut out = [vec![0.0; 256], vec![0.0; 256]];
        let note = HostMidi {
            offset: 0,
            bytes: [0x90, 69, 100],
        };
        run(&mut backend, 256, &[note], &mut out);
        let mut loudest = 0.0f32;
        for _ in 0..64 {
            run(&mut backend, 256, &[], &mut out);
            loudest = out[0].iter().fold(loudest, |m, s| m.max(s.abs()));
        }
        assert!(
            loudest > 0.01,
            "silent: {loudest}; {:?}",
            session.diagnostics()
        );
    }

    #[test]
    fn the_default_effect_is_host_in_to_host_out() {
        let mut session = Session::new(Nodes::all());
        session.new_host_project(Flavour::Effect);
        assert!(
            session.diagnostics().is_empty(),
            "{:?}",
            session.diagnostics()
        );
        assert!(!session.is_dirty(), "the starting point is not an edit");
    }

    #[test]
    fn expose_to_host_adds_the_node_and_the_wire_as_one_undo_step() {
        let mut session = Session::new(Nodes::all());
        session.new_host_project(Flavour::Effect);
        let gain = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id: gain,
            node: Node::new("noodle.util.gain"),
        })]);
        let before = session.project().graph().nodes().count();
        assert_eq!(session.expose_to_host(gain, "gain", "Level"), Ok(1));
        assert_eq!(session.project().graph().nodes().count(), before + 1);
        assert_eq!(session.host_param_names()[0].as_deref(), Some("Level"));
        // A wired parameter is left alone, and the next slot is the next one.
        assert!(session.expose_to_host(gain, "gain", "Again").is_err());
        session.undo();
        assert_eq!(session.project().graph().nodes().count(), before);
        assert_eq!(session.host_param_names()[0], None);
    }
}
