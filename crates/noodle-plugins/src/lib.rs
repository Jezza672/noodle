//! Hosting CLAP and VST3 plugins as graph nodes.
//!
//! - [`scan`](ScanCache) finds `.clap` and `.vst3` files and scans each in a child
//!   process; results are cached.
//! - A [`Catalog`] turns scan results into node types, one per plugin
//!   (`clap.<plugin id>` or `vst3.<class id>`), whose ports are the plugin's audio ports, note
//!   ports and parameters.
//! - [`set_sandbox`] runs plugins in separate host processes, so a crashing plugin
//!   can't take the session with it.
//! - [`Catalog::extender`] is for `RenderRequest::extend_registry`, so
//!   freezes, exports and background renders see the same plugins as live
//!   playback.

mod catalog;
mod descriptor;
mod editor;
mod host;
mod node;
mod sandbox;
mod scan;
mod thread;
mod vst3;
mod window;

pub use catalog::{
    Catalog, Discovery, ExtendRegistry, discover, discover_in, discover_in_with, discover_with,
};
pub use descriptor::*;
pub use editor::{
    Captured, EditorKey, capture, capture_key, close_editor, editor_instance_running,
    editor_is_open, editor_key_is_open, is_running, open_editor,
};
pub use host::{HostError, MAX_PARAMS, capture_state, describe_file};
pub use node::{PARAM_STEP, PluginNodeType};
pub use sandbox::{
    HOST_ARG, SandboxCommand, SandboxStatus, host_main, sandbox_enabled, sandbox_status,
    set_sandbox,
};
pub use scan::{
    SCAN_ARG, SCAN_TIMEOUT, ScanCache, ScanCommand, find_plugin_files, run_scan_child_if_requested,
    scan_child_main, scan_file, standard_dirs,
};
pub use thread::{adopt_current_thread, pump};
pub use window::{EditorWindow, ParentWindow, set_window_factory};

/// The node setting that holds a plugin's saved state, as base64 text.
pub const STATE_KEY: &str = "state";

/// The node setting that lists which parameters get ports (see
/// [`exposed_params`]).
pub const PARAMS_KEY: &str = "params";
