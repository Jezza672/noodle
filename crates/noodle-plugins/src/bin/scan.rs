//! Scans one CLAP plugin file and prints what it offers. Run by the host in
//! a child process, so a plugin that crashes or hangs doesn't take the host
//! with it. Programs that can't ship this binary use
//! `noodle_plugins::run_scan_child_if_requested` instead.
//!
//! Started with `--noodle-plugin-host`, it is a plugin host instead (see
//! `noodle_plugins::set_sandbox`), so tests and tools can use it as the
//! sandbox program.

fn main() {
    noodle_plugins::run_scan_child_if_requested();
    noodle_plugins::scan_child_main(std::env::args_os().nth(1));
}
