//! Plugin editors and capturing a live plugin's state.
//!
//! Plugin nodes register while they run, by node ID. An editor is a
//! floating window the plugin draws itself; changes made in it go straight to
//! the running plugin, so they are heard at once. To keep them in the
//! project, [`capture`] reads the plugin's saved state and every parameter's
//! value, and [`Captured::commands`] turns that into the edit that stores
//! them: the state in the node's `state` setting and the values in its
//! parameter ports (so they show in the properties panel, and a plugin
//! restored without its state still sounds the same).
//!
//! Storing the state is a config change, so the graph compiles again and the
//! node's plugin is loaded afresh, which closes its editor. Capture when the
//! editor is closed (see [`editor_is_open`]).

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use noodle_core::{Command, NodeId, Value};
use noodle_engine::Config;

use crate::STATE_KEY;
use crate::descriptor::{Descriptor, exposed_params, param_key};
use crate::host::{Flags, HostError};
use crate::{sandbox, thread, vst3};

struct Running {
    node: NodeId,
    key: u64,
    flags: Arc<Flags>,
}

/// Plugin nodes that are running, oldest first. A node can have more than one
/// (a freeze renders it again while the live one plays); the newest is the
/// one the editor opens on.
static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());

pub(crate) fn register(node: NodeId, key: u64, flags: Arc<Flags>) {
    RUNNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Running { node, key, flags });
}

pub(crate) fn unregister(key: u64) {
    RUNNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|r| r.key != key);
}

/// Which kind of instance a node has running.
enum Found {
    /// In this process, with the flags the plugin thread keeps for it.
    Local(u64, Arc<Flags>),
    /// In a plugin host process (see `sandbox`).
    Remote(u64),
}

impl Found {
    fn key(&self) -> u64 {
        match self {
            Self::Local(key, _) | Self::Remote(key) => *key,
        }
    }
}

fn find(node: NodeId) -> Result<Found, HostError> {
    if let Some((key, _)) = sandbox::remote_for(node) {
        return Ok(Found::Remote(key));
    }
    RUNNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .rev()
        .find(|r| r.node == node)
        .map(|r| Found::Local(r.key, Arc::clone(&r.flags)))
        .ok_or_else(|| HostError("that plugin isn't running (is the graph playing?)".into()))
}

/// Whether the node has a running plugin that can be opened or captured.
pub fn is_running(node: NodeId) -> bool {
    find(node).is_ok()
}

/// Which running instance an editor belongs to. A node can be built again
/// (a recompile, a freeze render), so a caller that keeps an editor open
/// holds this, not the node ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorKey(u64);

/// Opens the node's plugin editor (or brings it back if it was hidden) on its
/// newest running instance.
pub fn open_editor(node: NodeId) -> Result<EditorKey, HostError> {
    let key = find(node)?.key();
    if sandbox::is_remote_key(key) {
        sandbox::remote(key)
            .ok_or_else(|| HostError("that plugin isn't running".into()))?
            .open_editor()?;
    } else if vst3::gui::is_key(key) {
        vst3::gui::open(key)?;
    } else {
        thread::open_editor(key)?;
    }
    Ok(EditorKey(key))
}

fn flags_of(key: EditorKey) -> Option<Arc<Flags>> {
    RUNNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|r| r.key == key.0)
        .map(|r| Arc::clone(&r.flags))
}

/// Whether the instance is still running (it is gone once the node is built
/// again).
pub fn editor_instance_running(key: EditorKey) -> bool {
    if sandbox::is_remote_key(key.0) {
        return sandbox::remote(key.0).is_some();
    }
    flags_of(key).is_some()
}

/// Whether that instance's editor window is up.
pub fn editor_key_is_open(key: EditorKey) -> bool {
    if sandbox::is_remote_key(key.0) {
        return sandbox::remote(key.0).is_some_and(|r| r.is_open());
    }
    flags_of(key).is_some_and(|f| f.editor_open.load(std::sync::atomic::Ordering::Acquire))
}

/// Reads that instance.
pub fn capture_key(key: EditorKey) -> Result<Captured, HostError> {
    if sandbox::is_remote_key(key.0) {
        sandbox::remote(key.0)
            .ok_or_else(|| HostError("that plugin isn't running".into()))?
            .capture()
    } else if vst3::gui::is_key(key.0) {
        vst3::gui::capture(key.0)
    } else {
        thread::capture(key.0)
    }
}

/// Closes the editor, if it is open.
pub fn close_editor(node: NodeId) {
    if let Ok(found) = find(node) {
        let key = found.key();
        if sandbox::is_remote_key(key) {
            if let Some(remote) = sandbox::remote(key) {
                remote.close_editor();
            }
        } else if vst3::gui::is_key(key) {
            vst3::gui::close(key);
        } else {
            thread::close_editor(key);
        }
    }
}

/// Whether the node's editor window is up. Cheap enough to ask every frame.
pub fn editor_is_open(node: NodeId) -> bool {
    match find(node) {
        Ok(Found::Local(_, flags)) => flags.editor_open.load(std::sync::atomic::Ordering::Acquire),
        Ok(Found::Remote(key)) => sandbox::remote(key).is_some_and(|r| r.is_open()),
        Err(_) => false,
    }
}

/// A running plugin's state and parameter values at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Captured {
    pub state: Vec<u8>,
    /// Each parameter's plugin ID and value.
    pub params: Vec<(u32, f64)>,
}

/// Reads the node's running plugin.
pub fn capture(node: NodeId) -> Result<Captured, HostError> {
    capture_key(EditorKey(find(node)?.key()))
}

impl Captured {
    /// The state as the text the node stores.
    pub fn state_text(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.state)
    }

    /// The edit that stores this in the node: its `state` setting, and a
    /// value for each parameter the node exposes (clamped into its range).
    pub fn commands(&self, node: NodeId, descriptor: &Descriptor, config: &Config) -> Command {
        let exposed = exposed_params(descriptor, config);
        let mut commands = vec![Command::SetConfig {
            node,
            key: STATE_KEY.into(),
            value: Some(Value::Text(self.state_text())),
        }];
        for (id, value) in &self.params {
            let Some(param) = exposed.iter().find(|p| p.id == *id) else {
                continue;
            };
            let (min, max) =
                if param.min.is_finite() && param.max.is_finite() && param.max > param.min {
                    (param.min, param.max)
                } else {
                    (0.0, 1.0)
                };
            if !value.is_finite() {
                continue;
            }
            commands.push(Command::SetParam {
                node,
                key: param_key(*id),
                value: Some(value.clamp(min, max) as f32),
            });
        }
        Command::Batch(commands)
    }
}
