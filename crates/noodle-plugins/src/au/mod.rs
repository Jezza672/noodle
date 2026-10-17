//! Hosting Audio Units (macOS), through AudioToolbox's component API.
//!
//! An AU isn't a file the host loads: the system keeps a registry of
//! components, found by a type, subtype and manufacturer code. So an AU is
//! listed as a *virtual file* whose path is `audiounit:<id>` (see
//! [`component_path`]), the scan child instantiates it to describe it, and a
//! node's plugin ID is the same `<id>`: 24 hex digits, the three codes as
//! 32-bit numbers. Everything after that is the same as for CLAP and VST3:
//! the scan cache, the `Catalog`, `PluginNodeType`, the sandbox host.
//!
//! One AU instance is made, set up and torn down on the shared plugin thread
//! (`thread.rs`), and rendered on the audio thread by pulling its output
//! bus (`AudioUnitRender`); its input buses pull from the node's input
//! buffers through a render callback. Notes go in as MIDI events
//! (`MusicDeviceMIDIEvent`), parameters through `AudioUnitSetParameter` with
//! a sample offset, and state is the unit's class-info property list.
//!
//! Not supported yet: plugin editors (parameters have ports, and the state
//! saved in a project is whatever the node already holds), AUv3 extensions
//! that need asynchronous instantiation, MIDI output, and sidechain buses
//! beyond what the unit's own input buses give.

// Most of the helpers are for the macOS code; the rest of the crate and the
// tests still use the pure parts everywhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};

use noodle_engine::EventKind;

#[cfg(not(target_os = "macos"))]
use crate::host::HostError;

#[cfg(target_os = "macos")]
mod node;
#[cfg(target_os = "macos")]
mod plugin;

#[cfg(target_os = "macos")]
pub(crate) use node::{AuNode, load};
#[cfg(target_os = "macos")]
pub use plugin::describe_file;
#[cfg(target_os = "macos")]
pub(crate) use plugin::{capture_state, component_files, stamp};

/// What starts the path of the virtual file an AU is listed as.
pub(crate) const PATH_PREFIX: &str = "audiounit:";

/// An audio component's identity: the three codes AudioToolbox finds one by,
/// each a four-character code as a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ComponentId {
    pub kind: u32,
    pub subtype: u32,
    pub manufacturer: u32,
}

/// The unit types a node can host, as four-character codes.
pub(crate) const EFFECT: u32 = u32::from_be_bytes(*b"aufx");
pub(crate) const MUSIC_DEVICE: u32 = u32::from_be_bytes(*b"aumu");
pub(crate) const MUSIC_EFFECT: u32 = u32::from_be_bytes(*b"aumf");
pub(crate) const GENERATOR: u32 = u32::from_be_bytes(*b"augn");

/// The types listed: effects, instruments, effects that take notes, and
/// generators. (Mixers, panners, converters and outputs are for hosts that
/// build their own signal chains.)
pub(crate) const HOSTED_TYPES: [u32; 4] = [EFFECT, MUSIC_DEVICE, MUSIC_EFFECT, GENERATOR];

impl ComponentId {
    /// The plugin ID text: three groups of eight hex digits.
    pub fn to_text(self) -> String {
        format!(
            "{:08x}-{:08x}-{:08x}",
            self.kind, self.subtype, self.manufacturer
        )
    }

    pub fn from_text(text: &str) -> Option<Self> {
        let mut parts = text.split('-');
        let mut next = || {
            let part = parts.next()?;
            (part.len() == 8 && part.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| u32::from_str_radix(part, 16).ok())?
        };
        let id = Self {
            kind: next()?,
            subtype: next()?,
            manufacturer: next()?,
        };
        parts.next().is_none().then_some(id)
    }

    /// Whether the unit takes notes (an instrument, or an effect that does).
    pub fn takes_notes(self) -> bool {
        matches!(self.kind, MUSIC_DEVICE | MUSIC_EFFECT)
    }

    /// Whether the unit has audio inputs by nature: a generator and an
    /// instrument make sound from nothing.
    pub fn is_instrument(self) -> bool {
        self.kind == MUSIC_DEVICE
    }
}

/// The virtual file an AU is listed as.
pub(crate) fn component_path(id: ComponentId) -> PathBuf {
    PathBuf::from(format!("{PATH_PREFIX}{}", id.to_text()))
}

/// The component a virtual file is for, if it is one.
pub(crate) fn component_of_path(path: &Path) -> Option<ComponentId> {
    let text = path.to_str()?.strip_prefix(PATH_PREFIX)?;
    ComponentId::from_text(text)
}

/// Whether `path` is an AU's virtual file.
pub(crate) fn is_component_path(path: &Path) -> bool {
    path.to_str().is_some_and(|p| p.starts_with(PATH_PREFIX))
}

/// A four-character code as text (`aufx`), or hex if it isn't printable.
pub(crate) fn code_text(code: u32) -> String {
    let bytes = code.to_be_bytes();
    if bytes.iter().all(|b| (0x20..0x7f).contains(b)) {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("{code:08x}")
    }
}

/// Splits the name AudioToolbox gives a component, `Manufacturer: Name`,
/// into the vendor and the name. A name without the colon has no vendor.
pub(crate) fn split_name(full: &str) -> (String, String) {
    match full.split_once(':') {
        Some((vendor, name)) => (vendor.trim().to_owned(), name.trim().to_owned()),
        None => (String::new(), full.trim().to_owned()),
    }
}

/// The MIDI message for a note event, as status and two data bytes, or
/// `None` for an event a unit has no MIDI form for (note expressions).
/// Velocities run 0 to 1 in Noodle and 1 to 127 in MIDI (a note-on of
/// velocity 0 is a note-off in MIDI, so it never goes below 1).
pub(crate) fn midi_message(kind: &EventKind) -> Option<[u8; 3]> {
    let velocity = |v: f32| (v.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8;
    match *kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity: v,
            ..
        } => Some([0x90 | (channel & 0x0f), key & 0x7f, velocity(v)]),
        EventKind::NoteOff { .. } => None,
        EventKind::Midi(_) | EventKind::Channel { .. } => kind.midi1(),
        EventKind::Expression { .. } => None,
    }
}

/// The error for a request this system can't serve.
#[cfg(not(target_os = "macos"))]
pub(crate) fn unsupported() -> HostError {
    HostError("Audio Units are only available on macOS".into())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn describe_file(_path: &Path) -> Result<Vec<crate::Descriptor>, HostError> {
    Err(unsupported())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn capture_state(
    _path: &Path,
    _class_id: &str,
    _state: Option<&[u8]>,
) -> Result<Vec<u8>, HostError> {
    Err(unsupported())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn stamp(_path: &Path) -> Option<(u64, u64)> {
    None
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn component_files() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::NoteId;

    fn id(kind: &[u8; 4], sub: &[u8; 4], man: &[u8; 4]) -> ComponentId {
        ComponentId {
            kind: u32::from_be_bytes(*kind),
            subtype: u32::from_be_bytes(*sub),
            manufacturer: u32::from_be_bytes(*man),
        }
    }

    #[test]
    fn component_ids_round_trip_through_text_and_path() {
        let delay = id(b"aufx", b"dely", b"appl");
        assert_eq!(delay.to_text(), "61756678-64656c79-6170706c");
        assert_eq!(ComponentId::from_text(&delay.to_text()), Some(delay));
        let path = component_path(delay);
        assert!(is_component_path(&path));
        assert_eq!(component_of_path(&path), Some(delay));
    }

    #[test]
    fn bad_ids_and_paths_are_refused() {
        for text in [
            "",
            "aufx",
            "61756678-64656c79",
            "61756678-64656c79-6170706c-00000000",
            "61756678-64656c79-6170706",
            "61756678-64656c79-6170706z",
            "61756678-64656c79-+170706c",
        ] {
            assert_eq!(ComponentId::from_text(text), None, "{text:?}");
        }
        assert_eq!(component_of_path(Path::new("/x/y.clap")), None);
        assert!(!is_component_path(Path::new("/x/y.component")));
    }

    #[test]
    fn instruments_and_music_effects_take_notes() {
        assert!(id(b"aumu", b"dls ", b"appl").takes_notes());
        assert!(id(b"aumf", b"abcd", b"abcd").takes_notes());
        assert!(!id(b"aufx", b"dely", b"appl").takes_notes());
        assert!(id(b"aumu", b"dls ", b"appl").is_instrument());
        assert!(!id(b"aumf", b"abcd", b"abcd").is_instrument());
    }

    #[test]
    fn codes_print_as_text_when_they_can() {
        assert_eq!(code_text(u32::from_be_bytes(*b"dls ")), "dls ");
        assert_eq!(code_text(0x0000_0001), "00000001");
    }

    #[test]
    fn component_names_split_into_vendor_and_name() {
        assert_eq!(
            split_name("Apple: AUDelay"),
            ("Apple".to_owned(), "AUDelay".to_owned())
        );
        assert_eq!(
            split_name("Vendor: Name: Part 2"),
            ("Vendor".to_owned(), "Name: Part 2".to_owned())
        );
        assert_eq!(split_name("Plain"), (String::new(), "Plain".to_owned()));
    }

    #[test]
    fn notes_become_midi_messages() {
        let on = EventKind::NoteOn {
            note: NoteId(1),
            channel: 3,
            key: 60,
            velocity: 1.0,
        };
        assert_eq!(midi_message(&on), Some([0x93, 60, 127]));
        let quiet = EventKind::NoteOn {
            note: NoteId(1),
            channel: 0,
            key: 200,
            velocity: 0.0,
        };
        // Never velocity 0 (that would be a note-off), and keys stay 7-bit.
        assert_eq!(midi_message(&quiet), Some([0x90, 200 & 0x7f, 1]));
        assert_eq!(
            midi_message(&EventKind::Midi([0xb0, 1, 64])),
            Some([0xb0, 1, 64])
        );
        assert_eq!(
            midi_message(&EventKind::NoteOff {
                note: NoteId(1),
                velocity: 0.0
            }),
            None
        );
    }
}
