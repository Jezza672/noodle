//! The system clipboard, for what Noodle copies: clips and nodes go on it
//! under their own MIME types (`application/x.noodle.clip` and
//! `application/x.noodle.node`, RON text inside) beside a plain-text summary,
//! so they paste into another window or another run of the app, and pasting
//! reads the type it wants and ignores anything else on the clipboard.
//!
//! Where the system clipboard can't be opened (a session with no display), or
//! in tests, what is copied is kept in memory instead and pastes just the same.

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Clips copied from the timeline.
pub const CLIP_MIME: &str = "application/x.noodle.clip";
/// Nodes, their wires and frames, copied from the node editor.
pub const NODE_MIME: &str = "application/x.noodle.node";

/// Where copied things are put and found.
trait Backend {
    /// Replaces the clipboard's contents with these (MIME type, bytes) pairs.
    fn set(&mut self, items: Vec<(String, Vec<u8>)>) -> bool;
    fn get(&mut self, mime: &str) -> Option<Vec<u8>>;
    /// Whether `mime` is there, without fetching it (menus ask every frame).
    fn has(&mut self, mime: &str) -> bool {
        self.get(mime).is_some()
    }
}

/// The clipboard held in memory.
#[derive(Default)]
struct Memory(BTreeMap<String, Vec<u8>>);

impl Backend for Memory {
    fn set(&mut self, items: Vec<(String, Vec<u8>)>) -> bool {
        self.0 = items.into_iter().collect();
        true
    }

    fn get(&mut self, mime: &str) -> Option<Vec<u8>> {
        self.0.get(mime).cloned()
    }
}

/// The operating system's clipboard.
#[cfg(not(test))]
struct System(clipboard_rs::ClipboardContext);

#[cfg(not(test))]
impl Backend for System {
    fn set(&mut self, items: Vec<(String, Vec<u8>)>) -> bool {
        use clipboard_rs::{Clipboard, ClipboardContent};
        let contents = items
            .into_iter()
            .map(|(mime, data)| match mime.as_str() {
                TEXT => ClipboardContent::Text(String::from_utf8_lossy(&data).into_owned()),
                _ => ClipboardContent::Other(mime, data),
            })
            .collect();
        self.0.set(contents).is_ok()
    }

    fn get(&mut self, mime: &str) -> Option<Vec<u8>> {
        use clipboard_rs::Clipboard;
        self.0.get_buffer(mime).ok().filter(|data| !data.is_empty())
    }

    fn has(&mut self, mime: &str) -> bool {
        use clipboard_rs::{Clipboard, ContentFormat};
        self.0.has(ContentFormat::Other(mime.to_owned()))
    }
}

const TEXT: &str = "text/plain";

thread_local! {
    // The UI thread's only: the platform clipboards aren't `Send`.
    static BACKEND: RefCell<Option<Box<dyn Backend>>> = const { RefCell::new(None) };
    // What a failed system clipboard falls back to.
    static FALLBACK: RefCell<Memory> = RefCell::new(Memory::default());
}

fn with_backend<T>(f: impl FnOnce(&mut dyn Backend) -> T) -> T {
    BACKEND.with(|slot| {
        let mut slot = slot.borrow_mut();
        let backend = slot.get_or_insert_with(open);
        f(backend.as_mut())
    })
}

#[cfg(not(test))]
fn open() -> Box<dyn Backend> {
    match clipboard_rs::ClipboardContext::new() {
        Ok(context) => Box::new(System(context)),
        Err(_) => Box::new(Memory::default()),
    }
}

/// Tests never touch the real clipboard.
#[cfg(test)]
fn open() -> Box<dyn Backend> {
    Box::new(Memory::default())
}

/// Puts `value` on the clipboard under `mime` (as RON), with `summary` as its
/// plain text, so a text field or another app pastes something readable and
/// the window sees a paste. Returns whether the clipboard took it; if not, it
/// is kept in memory.
pub fn copy<T: Serialize>(mime: &str, value: &T, summary: &str) {
    let Ok(text) = ron::ser::to_string(value) else {
        return;
    };
    let items = vec![
        (TEXT.to_owned(), summary.as_bytes().to_vec()),
        (mime.to_owned(), text.into_bytes()),
    ];
    if !with_backend(|b| b.set(items.clone())) {
        FALLBACK.with(|f| f.borrow_mut().set(items));
    }
}

/// What the clipboard holds under `mime`, if it holds something of that type
/// that reads as a `T`.
pub fn paste<T: DeserializeOwned>(mime: &str) -> Option<T> {
    let bytes =
        with_backend(|b| b.get(mime)).or_else(|| FALLBACK.with(|f| f.borrow_mut().get(mime)))?;
    ron::de::from_bytes(&bytes).ok()
}

/// Whether the clipboard holds something to paste under `mime`.
pub fn holds(mime: &str) -> bool {
    with_backend(|b| b.has(mime)) || FALLBACK.with(|f| f.borrow_mut().get(mime)).is_some()
}

/// Empties the clipboard, for tests.
#[cfg(test)]
pub fn clear() {
    BACKEND.with(|slot| *slot.borrow_mut() = None);
    FALLBACK.with(|f| *f.borrow_mut() = Memory::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_copied_comes_back_under_its_own_type_only() {
        clear();
        copy(CLIP_MIME, &vec![1u32, 2, 3], "3 clips");
        assert_eq!(paste::<Vec<u32>>(CLIP_MIME), Some(vec![1, 2, 3]));
        assert!(holds(CLIP_MIME));
        assert!(!holds(NODE_MIME));
        assert_eq!(paste::<Vec<u32>>(NODE_MIME), None);
        // Something else on the clipboard, or the wrong shape, doesn't paste.
        copy(NODE_MIME, &"nodes", "1 node");
        assert!(!holds(CLIP_MIME));
        assert_eq!(paste::<Vec<u32>>(NODE_MIME), None);
    }

    #[test]
    fn the_plain_text_summary_goes_with_it() {
        clear();
        copy(CLIP_MIME, &1u32, "1 clip");
        let text = with_backend(|b| b.get(TEXT)).unwrap();
        assert_eq!(text, b"1 clip");
    }
}
