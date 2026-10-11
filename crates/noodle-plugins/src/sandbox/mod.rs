//! Running plugins in separate processes.
//!
//! A plugin is somebody else's code, and the process it runs in goes where it
//! goes: a crash takes the whole session, a hang stalls the shared plugin
//! thread. With sandboxing on, each plugin node instead talks to a *plugin
//! host*, a child process that loads the plugin and runs it block by block on
//! audio exchanged through shared memory:
//!
//! - `proxy.rs`: the app's side, a [`Node`](noodle_engine::Node) that posts a
//!   block and waits for the answer, and a monitor thread that replaces a host
//!   that died or hung (the node plays silence meanwhile);
//! - `host.rs`: the child, which builds the very node the app would have
//!   built in-process and runs it;
//! - `shm.rs`: the shared file's layout;
//! - `proto.rs`: the control socket (start-up, editor, state capture).
//!
//! It is off until [`set_sandbox`] gives it the program to start.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};

use noodle_core::NodeId;

pub(crate) mod host;
pub(crate) mod proto;
pub(crate) mod proxy;
pub(crate) mod shm;

pub use host::{HOST_ARG, host_main, run_host_if_requested};

use proxy::{Remote, Shared};

/// How to start a plugin host: a program and the arguments before the port and
/// token it is given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl SandboxCommand {
    /// This program, started with [`HOST_ARG`]. Needs
    /// [`run_host_if_requested`] (called by `run_scan_child_if_requested`) in
    /// its `main`.
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            args: vec![HOST_ARG.into()],
        })
    }

    /// The `noodle-plugin-scan` program at `program`, which is also a host.
    pub fn program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: vec![HOST_ARG.into()],
        }
    }
}

static COMMAND: RwLock<Option<Arc<SandboxCommand>>> = RwLock::new(None);

/// Turns sandboxing on (with the program to start as the host) or off.
/// It applies to plugin nodes built afterwards; ones already running stay as
/// they are until the graph compiles them again.
pub fn set_sandbox(command: Option<SandboxCommand>) {
    *COMMAND.write().unwrap_or_else(PoisonError::into_inner) = command.map(Arc::new);
}

/// Whether new plugin nodes run in a host process.
pub fn sandbox_enabled() -> bool {
    current_command().is_some()
}

pub(crate) fn current_command() -> Option<Arc<SandboxCommand>> {
    COMMAND
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// How a sandboxed plugin node is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxStatus {
    /// The host is up and the node is using it.
    pub running: bool,
    /// The host kept dying and the node has given up: it plays silence until
    /// the graph compiles it again.
    pub failed: bool,
    /// How many times the host died or hung and was replaced.
    pub restarts: u32,
    /// The host process's ID, for tools and tests.
    pub pid: Option<u32>,
    /// Blocks played as silence because the host was late or down.
    pub dropped_blocks: u64,
    pub last_error: Option<String>,
}

struct Entry {
    node: NodeId,
    key: u64,
    shared: Weak<Shared>,
}

static INSTANCES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Editor keys for sandboxed instances have this bit set (VST3 keys use bit
/// 63), so they never meet
/// an in-process instance's key.
pub(crate) const REMOTE_KEY_BIT: u64 = 1 << 62;

pub(crate) fn is_remote_key(key: u64) -> bool {
    key & REMOTE_KEY_BIT != 0
}

pub(crate) fn register(node: NodeId, shared: &Arc<Shared>) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let key = REMOTE_KEY_BIT | NEXT.fetch_add(1, Ordering::Relaxed);
    let mut entries = INSTANCES.lock().unwrap_or_else(PoisonError::into_inner);
    entries.retain(|e| e.shared.strong_count() > 0);
    entries.push(Entry {
        node,
        key,
        shared: Arc::downgrade(shared),
    });
    key
}

pub(crate) fn unregister(key: u64) {
    INSTANCES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|e| e.key != key);
}

/// The sandboxed instance for an editor key.
pub(crate) fn remote(key: u64) -> Option<Remote> {
    INSTANCES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|e| e.key == key)
        .and_then(|e| e.shared.upgrade())
        .map(Remote)
}

/// The newest sandboxed instance of `node`, as (editor key, handle).
pub(crate) fn remote_for(node: NodeId) -> Option<(u64, Remote)> {
    INSTANCES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .rev()
        .filter(|e| e.node == node)
        .find_map(|e| Some((e.key, Remote(e.shared.upgrade()?))))
}

/// How the newest sandboxed instance of `node` is doing, or `None` if the
/// node isn't running in a host process.
pub fn sandbox_status(node: NodeId) -> Option<SandboxStatus> {
    remote_for(node).map(|(_, remote)| remote.0.status())
}
