//! Audio Unit editors (macOS), embedded in a fake window.

#[cfg(target_os = "macos")]
#[path = "../common/mod.rs"]
mod common;
#[cfg(target_os = "macos")]
mod macos;

fn main() {
    #[cfg(target_os = "macos")]
    macos::run();
}
