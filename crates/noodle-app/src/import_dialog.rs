//! The Import dialog: the host-neutral way to bring files into a plugin's
//! editor, where files dragged from the host are not supported.
//!
//! A file picker, and a field to paste a path into for hosts whose native
//! dialog will not open from an embedded window. Audio becomes a Sample Set
//! on the canvas (what dropping it there does); MIDI files become clips on
//! tracks the host's transport plays.

use std::path::{Path, PathBuf};

/// The extensions the picker offers: audio, then MIDI.
const AUDIO: &[&str] = &["wav", "flac", "mp3", "ogg", "aif", "aiff", "m4a", "aac"];
const MIDI: &[&str] = &["mid", "midi"];

/// What a path is, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Audio,
    Midi,
    /// A folder (of samples) or anything else.
    Other,
}

pub fn kind(path: &Path) -> Kind {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some(e) if MIDI.contains(&e) => Kind::Midi,
        Some(e) if AUDIO.contains(&e) => Kind::Audio,
        _ => Kind::Other,
    }
}

/// The dialog's state.
#[derive(Default)]
pub struct ImportDialog {
    open: bool,
    text: String,
    error: Option<String>,
}

impl ImportDialog {
    pub fn open(&mut self) {
        self.open = true;
        self.error = None;
    }

    /// Sets the pasted path, for tests.
    #[cfg(test)]
    pub fn text_for_test(&mut self, text: &str) {
        self.text = text.to_owned();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Shows the dialog, and returns the files to import once the user has
    /// chosen some.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Vec<PathBuf>> {
        if !self.open {
            return None;
        }
        let mut chosen = None;
        let mut close = false;
        let modal = crate::dialog::modal(ctx, "import files", |ui| {
            ui.set_max_width(420.0);
            ui.heading("Import");
            ui.add_space(6.0);
            ui.label(
                "Audio files become a Sample Set in the graph. MIDI files become \
                 clips that play with the host's transport.",
            );
            ui.add_space(6.0);
            if ui.button("Choose files…").clicked() {
                let mut all = AUDIO.to_vec();
                all.extend_from_slice(MIDI);
                if let Some(files) = crate::picker::pick_files(("Audio and MIDI", &all)) {
                    chosen = Some(files);
                }
            }
            ui.add_space(6.0);
            ui.label("Or paste a path:");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.text)
                    .hint_text("/path/to/file.wav")
                    .desired_width(f32::INFINITY),
            );
            let go = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if let Some(error) = &self.error {
                ui.colored_label(crate::theme::MUTE, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let typed = !self.text.trim().is_empty();
                if (ui.add_enabled(typed, egui::Button::new("Import")).clicked() || go) && typed {
                    let path = PathBuf::from(clean(&self.text));
                    if path.exists() {
                        chosen = Some(vec![path]);
                    } else {
                        self.error = Some(format!("There is nothing at {}", path.display()));
                    }
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if chosen.is_some() {
            self.text.clear();
            self.open = false;
        } else if close || modal.should_close() {
            self.open = false;
        }
        chosen
    }
}

/// A pasted path as a path: no surrounding space or quotes (a file manager's
/// "copy as path" adds them).
fn clean(text: &str) -> &str {
    text.trim().trim_matches(|c| c == '"' || c == '\'')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_told_apart_by_extension() {
        assert_eq!(kind(Path::new("a/Kick.WAV")), Kind::Audio);
        assert_eq!(kind(Path::new("song.mid")), Kind::Midi);
        assert_eq!(kind(Path::new("song.MIDI")), Kind::Midi);
        assert_eq!(kind(Path::new("samples")), Kind::Other);
    }

    #[test]
    fn a_pasted_path_loses_its_quotes_and_space() {
        assert_eq!(clean("  \"/a b/c.wav\" "), "/a b/c.wav");
        assert_eq!(clean("'/x.mid'"), "/x.mid");
    }
}
