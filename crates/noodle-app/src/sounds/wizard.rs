//! The Make a shareable pack wizard (ADR 0012).
//!
//! Choose a folder of audio, name the pack, set an author, a licence and tags
//! for the pack or for single sounds, see the result of the same validation
//! CI runs, and export a static site (or one zip of it) for the user to host.
//! It never uploads anything: publishing to the built-in source is the CLI's
//! `noodle packs upload` and a pull request.
//!
//! [`Draft`] is the pack being made, with no UI in it, so a test can drive it
//! and check what the wizard would write. The window is at the bottom.

use std::path::{Path, PathBuf};

use egui::{Context, RichText, Ui};
use noodle_engine::Job;
use noodle_packs::{FORMAT_VERSION, Licence, PackDef, Problem, SPDX_IDS, SampleDef, validate_dir};
use noodle_publish::{FoundPack, PublishError, Report};

use super::Sounds;
use super::folder::{files_under, tags_of};
use super::source::audio_hash;

/// What the default licence says, in the words the wizard shows.
pub const DEFAULT_LICENCE_TEXT: &str = "Everyone with access may use the samples in their own \
     projects, commercial or not, but may not redistribute them.";

/// How a licence is picked: the default, an SPDX id or the author's own text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub kind: Kind,
    pub spdx: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Default,
    Spdx,
    Custom,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            kind: Kind::Default,
            spdx: SPDX_IDS[0].into(),
            text: String::new(),
        }
    }
}

impl Choice {
    pub fn licence(&self) -> Licence {
        match self.kind {
            Kind::Default => Licence::Default,
            Kind::Spdx => Licence::Spdx(self.spdx.clone()),
            Kind::Custom => Licence::Custom(self.text.clone()),
        }
    }

    fn label(&self) -> String {
        match self.kind {
            Kind::Default => "Default".into(),
            Kind::Spdx => self.spdx.clone(),
            Kind::Custom => "Custom text".into(),
        }
    }

    /// The combo box: Default, each SPDX id, Custom text.
    fn pick(ui: &mut Ui, salt: impl std::hash::Hash + std::fmt::Debug, choice: &mut Choice) {
        egui::ComboBox::from_id_salt(salt)
            .selected_text(choice.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut choice.kind, Kind::Default, "Default");
                for id in SPDX_IDS {
                    if ui
                        .selectable_label(choice.kind == Kind::Spdx && choice.spdx == *id, *id)
                        .clicked()
                    {
                        choice.kind = Kind::Spdx;
                        choice.spdx = (*id).into();
                    }
                }
                ui.selectable_value(&mut choice.kind, Kind::Custom, "Custom text");
            });
        if choice.kind == Kind::Custom {
            ui.add(
                egui::TextEdit::multiline(&mut choice.text)
                    .hint_text("Write the terms in your own words")
                    .desired_rows(3)
                    .desired_width(320.0),
            );
        }
    }
}

/// An audio file found in the folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Relative to the folder, `/`-separated: what the pack names as `file`.
    pub rel: String,
    pub name: String,
    pub hash: String,
    /// The folders it sits in.
    pub tags: Vec<String>,
}

/// Reads and hashes the audio files under `dir`. A file that doesn't decode is
/// left out and counted; the same audio twice is one sound.
pub fn scan(dir: &Path) -> Result<(Vec<Found>, usize), String> {
    if !dir.is_dir() {
        return Err("That isn't a folder".into());
    }
    let mut seen = std::collections::HashSet::new();
    let (mut found, mut skipped) = (Vec::new(), 0);
    for path in files_under(dir) {
        let Ok(hash) = audio_hash(&path) else {
            skipped += 1;
            continue;
        };
        if !seen.insert(hash.clone()) {
            continue;
        }
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        found.push(Found {
            rel,
            name: path
                .file_stem()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            hash,
            tags: tags_of(dir, &path),
        });
    }
    Ok((found, skipped))
}

/// One sound in the draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub rel: String,
    pub hash: String,
    pub include: bool,
    pub name: String,
    /// Comma-separated, as typed.
    pub tags: String,
    /// Empty means the pack's.
    pub author: String,
    /// `None` means the pack's.
    pub licence: Option<Choice>,
    pub credit: String,
}

/// A pack being made from a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub dir: PathBuf,
    pub name: String,
    /// Follows the name until it is typed over.
    pub id: String,
    pub id_typed: bool,
    pub description: String,
    pub author: String,
    pub licence: Choice,
    pub rows: Vec<Row>,
}

/// `Some Pack!` as `some-pack`: what a pack id can be.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

fn tags_of_text(text: &str) -> Vec<String> {
    text.split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

impl Draft {
    pub fn new(dir: PathBuf, found: Vec<Found>) -> Self {
        let name = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Self {
            id: slug(&name),
            id_typed: false,
            name,
            dir,
            description: String::new(),
            author: String::new(),
            licence: Choice::default(),
            rows: found
                .into_iter()
                .map(|f| Row {
                    rel: f.rel,
                    hash: f.hash,
                    include: true,
                    name: f.name,
                    tags: f.tags.join(", "),
                    author: String::new(),
                    licence: None,
                    credit: String::new(),
                })
                .collect(),
        }
    }

    pub fn set_name(&mut self, name: String) {
        self.name = name;
        if !self.id_typed {
            self.id = slug(&self.name);
        }
    }

    /// The pack definition this draft makes: only the included sounds.
    pub fn def(&self) -> PackDef {
        PackDef {
            format: FORMAT_VERSION,
            id: self.id.clone(),
            name: self.name.trim().to_owned(),
            version: 1,
            description: self.description.trim().to_owned(),
            author: self.author.trim().to_owned(),
            licence: self.licence.licence(),
            samples: self
                .rows
                .iter()
                .filter(|r| r.include)
                .map(|r| SampleDef {
                    hash: r.hash.clone(),
                    name: r.name.trim().to_owned(),
                    file: Some(r.rel.clone()),
                    author: Some(r.author.trim().to_owned()).filter(|a| !a.is_empty()),
                    licence: r.licence.as_ref().map(Choice::licence),
                    tags: tags_of_text(&r.tags),
                    credit: Some(r.credit.trim().to_owned()).filter(|c| !c.is_empty()),
                })
                .collect(),
        }
    }

    /// What is wrong with it: the very check CI and the CLI make. A problem's
    /// sample index is the position among the included sounds, so each comes
    /// back with the name of the sound it is about.
    pub fn problems(&self) -> Vec<(Option<String>, Problem)> {
        let included: Vec<&Row> = self.rows.iter().filter(|r| r.include).collect();
        validate_dir(&self.def(), &self.dir)
            .into_iter()
            .map(|p| {
                let name = p
                    .sample
                    .and_then(|i| included.get(i))
                    .map(|r| r.name.clone());
                (name, p)
            })
            .collect()
    }

    pub fn included(&self) -> usize {
        self.rows.iter().filter(|r| r.include).count()
    }

    /// Sets the author of every included sound.
    pub fn set_author_all(&mut self, author: &str) {
        for r in self.rows.iter_mut().filter(|r| r.include) {
            r.author = author.trim().to_owned();
        }
    }

    /// Sets the licence of every included sound.
    pub fn set_licence_all(&mut self, licence: Option<Choice>) {
        for r in self.rows.iter_mut().filter(|r| r.include) {
            r.licence = licence.clone();
        }
    }

    /// Adds a tag to every included sound that lacks it.
    pub fn add_tag_all(&mut self, tag: &str) {
        let tag = tag.trim().to_lowercase();
        if tag.is_empty() {
            return;
        }
        for r in self.rows.iter_mut().filter(|r| r.include) {
            let mut tags = tags_of_text(&r.tags);
            if !tags.contains(&tag) {
                tags.push(tag.clone());
                r.tags = tags.join(", ");
            }
        }
    }
}

/// How the result is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// A folder to serve as it is.
    Folder,
    /// One zip of the same files.
    Zip,
}

/// What an export wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub format: Format,
    /// The folder, or the zip file.
    pub path: PathBuf,
    pub samples: usize,
    pub written: usize,
}

/// Builds the site for `draft` at `out`: into the folder `out`, or as
/// `<out>/<id>.zip`. The folder being packed can't be where it goes, because
/// the site would then be read back as sounds.
pub fn export(draft: &Draft, format: Format, out: &Path) -> Result<Exported, String> {
    let problems = draft.problems();
    if !problems.is_empty() {
        return Err("The pack has problems to fix first".into());
    }
    let (dir, target) = (
        std::fs::canonicalize(&draft.dir).unwrap_or_else(|_| draft.dir.clone()),
        std::fs::canonicalize(out).unwrap_or_else(|_| out.to_owned()),
    );
    if target.starts_with(&dir) {
        return Err("Choose a folder outside the one you are packing".into());
    }
    let pack = FoundPack {
        def: draft.def(),
        dir: draft.dir.clone(),
    };
    let packs = [pack];
    let fail = |e: PublishError| e.to_string();
    let (path, report): (PathBuf, Report) = match format {
        Format::Folder => (
            out.to_owned(),
            noodle_publish::build_packs(&packs, out).map_err(fail)?,
        ),
        Format::Zip => {
            std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
            let zip = out.join(format!("{}.zip", draft.id));
            let report = noodle_publish::build_zip(&packs, &zip).map_err(fail)?;
            (zip, report)
        }
    };
    Ok(Exported {
        format,
        path,
        samples: report.samples,
        written: report.written,
    })
}

enum Stage {
    Reading(Job<Result<(Vec<Found>, usize), String>>),
    Editing,
}

/// The wizard's window.
pub struct Wizard {
    stage: Stage,
    dir: PathBuf,
    draft: Draft,
    skipped: usize,
    format: Format,
    exporting: Option<Job<Result<Exported, String>>>,
    exported: Option<Exported>,
    error: Option<String>,
    bulk_author: String,
    bulk_tag: String,
    bulk_licence: Choice,
}

impl Wizard {
    pub fn start(dir: PathBuf) -> Self {
        let read = dir.clone();
        Self {
            stage: Stage::Reading(Job::spawn(move |_| scan(&read))),
            draft: Draft::new(dir.clone(), Vec::new()),
            dir,
            skipped: 0,
            format: Format::Folder,
            exporting: None,
            exported: None,
            error: None,
            bulk_author: String::new(),
            bulk_tag: String::new(),
            bulk_licence: Choice::default(),
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Reading(_)) || self.exporting.is_some()
    }

    fn poll(&mut self) {
        if let Stage::Reading(job) = &mut self.stage
            && let Some(done) = job.poll()
        {
            match done.map_err(|p| p.to_string()).and_then(|r| r) {
                Ok((found, skipped)) => {
                    self.draft = Draft::new(self.dir.clone(), found);
                    self.skipped = skipped;
                }
                Err(why) => self.error = Some(why),
            }
            self.stage = Stage::Editing;
        }
        if let Some(job) = &mut self.exporting
            && let Some(done) = job.poll()
        {
            self.exporting = None;
            match done.map_err(|p| p.to_string()).and_then(|r| r) {
                Ok(exported) => self.exported = Some(exported),
                Err(why) => self.error = Some(why),
            }
        }
    }

    fn export(&mut self, out: PathBuf) {
        let (draft, format) = (self.draft.clone(), self.format);
        self.error = None;
        self.exported = None;
        self.exporting = Some(Job::spawn(move |_| export(&draft, format, &out)));
    }

    /// Draws the contents. Returns a folder to add as a source, when asked.
    fn contents(&mut self, ui: &mut Ui) -> Option<PathBuf> {
        if matches!(self.stage, Stage::Reading(_)) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Reading the sounds in {}…", self.dir.display()));
            });
            return None;
        }
        ui.weak(
            "Makes a folder of files you can host anywhere, for others to add as a source. \
             Nothing is uploaded, and your sounds stay where they are.",
        );
        if self.skipped > 0 {
            ui.weak(format!(
                "{} file{} couldn't be read as audio and {} left out.",
                self.skipped,
                if self.skipped == 1 { "" } else { "s" },
                if self.skipped == 1 { "was" } else { "were" }
            ));
        }
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                self.pack_section(ui);
                ui.separator();
                self.sounds_section(ui);
            });
        // The check and the export stay in view however long the list is.
        ui.separator();
        self.check_and_export(ui)
    }

    fn pack_section(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("The pack").strong());
        let d = &mut self.draft;
        egui::Grid::new("pack details")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Pack name");
                let mut name = d.name.clone();
                if ui
                    .add(egui::TextEdit::singleline(&mut name).desired_width(280.0))
                    .changed()
                {
                    d.set_name(name);
                }
                ui.end_row();
                ui.label("Pack id");
                if ui
                    .add(egui::TextEdit::singleline(&mut d.id).desired_width(280.0))
                    .on_hover_text(
                        "Lowercase letters, digits and hyphens. Names the pack in a source",
                    )
                    .changed()
                {
                    d.id_typed = true;
                }
                ui.end_row();
                ui.label("Description");
                ui.add(
                    egui::TextEdit::singleline(&mut d.description)
                        .hint_text("Optional")
                        .desired_width(280.0),
                );
                ui.end_row();
                ui.label("Author");
                ui.add(
                    egui::TextEdit::singleline(&mut d.author)
                        .hint_text("Who made these sounds")
                        .desired_width(280.0),
                );
                ui.end_row();
                ui.label("Licence");
                ui.vertical(|ui| Choice::pick(ui, "pack licence", &mut d.licence));
                ui.end_row();
            });
        match d.licence.kind {
            Kind::Default => {
                ui.weak(DEFAULT_LICENCE_TEXT);
            }
            Kind::Spdx => {
                ui.weak(
                    "A standard licence. A sound under a CC-BY or MIT licence needs a credit \
                     line, which travels with it onto the timeline.",
                );
            }
            Kind::Custom => {
                ui.weak(
                    "Your own terms are shown as written. They are taken to forbid redistribution.",
                );
            }
        }
    }

    fn sounds_section(&mut self, ui: &mut Ui) {
        let total = self.draft.rows.len();
        ui.label(
            RichText::new(format!(
                "Sounds ({} of {total} included)",
                self.draft.included()
            ))
            .strong(),
        );
        if total == 0 {
            ui.label("There is no audio in that folder.");
            return;
        }
        ui.weak("Set these for every included sound at once, or edit any one below.");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.bulk_author)
                    .hint_text("Author")
                    .desired_width(140.0),
            );
            if ui.button("Set author on all").clicked() {
                self.draft.set_author_all(&self.bulk_author.clone());
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.bulk_tag)
                    .hint_text("Tag")
                    .desired_width(100.0),
            );
            if ui.button("Add tag to all").clicked() {
                self.draft.add_tag_all(&self.bulk_tag.clone());
            }
        });
        ui.horizontal(|ui| {
            Choice::pick(ui, "bulk licence", &mut self.bulk_licence);
            if ui.button("Set licence on all").clicked() {
                self.draft.set_licence_all(Some(self.bulk_licence.clone()));
            }
            if ui.button("Use the pack's licence").clicked() {
                self.draft.set_licence_all(None);
            }
        });
        let pack_credit = self.draft.licence.licence().requires_credit();
        for (i, row) in self.draft.rows.iter_mut().enumerate() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut row.include, "")
                    .on_hover_text("Include this sound in the pack");
                ui.add(egui::TextEdit::singleline(&mut row.name).desired_width(150.0))
                    .on_hover_text(row.rel.clone());
                ui.add(
                    egui::TextEdit::singleline(&mut row.tags)
                        .hint_text("tags, separated by commas")
                        .desired_width(150.0),
                );
            });
            if !row.include {
                continue;
            }
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.add(
                    egui::TextEdit::singleline(&mut row.author)
                        .hint_text("Author (the pack's)")
                        .desired_width(110.0),
                );
                let mut own = row.licence.is_some();
                if ui.checkbox(&mut own, "Own licence").changed() {
                    row.licence = own.then(Choice::default);
                }
                if let Some(choice) = &mut row.licence {
                    Choice::pick(ui, ("sample licence", i), choice);
                }
            });
            let needs_credit = match &row.licence {
                Some(c) => c.licence().requires_credit(),
                None => pack_credit,
            };
            if needs_credit || !row.credit.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut row.credit)
                            .hint_text("Credit line, shown wherever the sound is used")
                            .desired_width(300.0),
                    );
                });
            }
        }
    }

    fn check_and_export(&mut self, ui: &mut Ui) -> Option<PathBuf> {
        ui.label(RichText::new("Check").strong());
        let problems = self.draft.problems();
        if problems.is_empty() {
            ui.label("✓ The pack passes the checks CI and the command line make.");
        } else {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!(
                    "{} thing{} to fix before it can be exported:",
                    problems.len(),
                    if problems.len() == 1 { "" } else { "s" }
                ),
            );
            for (name, problem) in &problems {
                // The problem's own text names a sample by position, which
                // means nothing here; say which sound it is.
                let text = problem.to_string();
                let text = text
                    .split_once(": ")
                    .filter(|_| problem.sample.is_some())
                    .map_or(text.as_str(), |(_, rest)| rest);
                match name {
                    Some(name) => ui.label(format!("• {name}: {text}")),
                    None => ui.label(format!("• {text}")),
                };
            }
        }

        ui.add_space(6.0);
        ui.label(RichText::new("Export").strong());
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.format, Format::Folder, "Folder to host");
            ui.radio_value(&mut self.format, Format::Zip, "One zip file");
        });
        let ready = problems.is_empty() && self.exporting.is_none();
        let mut add = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new("Export…"))
                .on_hover_text("Choose where to put it")
                .clicked()
                && let Some(out) =
                    crate::picker::pick_folder_in("to put the pack in", self.dir.parent())
            {
                self.export(out);
            }
            if self.exporting.is_some() {
                ui.spinner();
                ui.label("Encoding the sounds…");
            }
        });
        if let Some(why) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, why);
        }
        if let Some(done) = &self.exported {
            ui.label(format!(
                "Exported {} sound{} to {}.",
                done.samples,
                if done.samples == 1 { "" } else { "s" },
                done.path.display()
            ));
            match done.format {
                Format::Folder => {
                    ui.weak(
                        "Put this folder on any web host that serves static files, then add \
                         its address as a source. To try it first, add it from here.",
                    );
                    if ui.button("Add it as a source").clicked() {
                        add = Some(done.path.clone());
                    }
                }
                Format::Zip => {
                    ui.weak("Unzip it on the host so index.json is at the address you share.");
                }
            }
        }
        add
    }
}

/// Draws the wizard, if one is open.
pub(super) fn show(sounds: &mut Sounds, ctx: &Context) {
    let Some(mut wizard) = sounds.wizard.take() else {
        return;
    };
    wizard.poll();
    if wizard.busy() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    let mut open = true;
    let mut add = None;
    egui::Window::new("Make a shareable pack")
        .open(&mut open)
        .default_width(520.0)
        // Below the menu bar, which a window at the corner would cover.
        .default_pos([160.0, 80.0])
        .show(ctx, |ui| add = wizard.contents(ui));
    if let Some(path) = add {
        let name = wizard.draft.name.clone();
        if let Err(why) = sounds.add_site(&path, &name) {
            wizard.error = Some(why);
        }
    }
    if open {
        sounds.wizard = Some(wizard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sounds::source::fake;

    fn folder() -> (tempfile::TempDir, Draft) {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let kicks = dir.path().join("Kit").join("Kicks");
        std::fs::create_dir_all(&kicks).unwrap();
        for (name, hz) in [("Deep", 110.0), ("High", 330.0)] {
            let (bytes, _) = fake::tone(scratch.path(), hz);
            std::fs::write(kicks.join(format!("{name}.flac")), bytes).unwrap();
        }
        std::fs::write(kicks.join("broken.wav"), "not audio").unwrap();
        let root = dir.path().join("Kit");
        let (found, skipped) = scan(&root).unwrap();
        assert_eq!((found.len(), skipped), (2, 1));
        (dir, Draft::new(root, found))
    }

    #[test]
    fn a_draft_starts_from_the_folder_and_asks_for_an_author() {
        let (_dir, mut draft) = folder();
        assert_eq!((draft.name.as_str(), draft.id.as_str()), ("Kit", "kit"));
        assert_eq!(draft.rows[0].tags, "kicks");
        let text: Vec<String> = draft
            .problems()
            .iter()
            .map(|(_, p)| p.to_string())
            .collect();
        assert!(text.iter().any(|t| t.contains("no author")), "{text:?}");
        // The problem is about a sound the user can see, by name.
        assert!(
            draft
                .problems()
                .iter()
                .any(|(n, _)| n.as_deref() == Some("Deep"))
        );
        draft.author = "Jo".into();
        assert!(draft.problems().is_empty());
        draft.set_name("My Kit!".into());
        assert_eq!(draft.id, "my-kit");
    }

    #[test]
    fn bulk_apply_reaches_the_included_sounds_and_the_default_licence_is_the_default() {
        let (_dir, mut draft) = folder();
        draft.author = "Jo".into();
        assert_eq!(draft.def().licence, Licence::Default);
        draft.rows[1].include = false;
        draft.add_tag_all("Punchy");
        draft.set_author_all("Al");
        assert_eq!(draft.rows[0].tags, "kicks, punchy");
        assert_eq!(draft.rows[1].tags, "kicks");
        let def = draft.def();
        assert_eq!(def.samples.len(), 1);
        assert_eq!(def.samples[0].author.as_deref(), Some("Al"));
        // A licence that needs credit asks for it, as CI would.
        draft.set_licence_all(Some(Choice {
            kind: Kind::Spdx,
            spdx: "CC-BY-4.0".into(),
            text: String::new(),
        }));
        assert!(!draft.problems().is_empty());
        draft.rows[0].credit = "Deep by Al".into();
        assert!(draft.problems().is_empty());
        // Custom text is the author's own.
        draft.licence = Choice {
            kind: Kind::Custom,
            spdx: String::new(),
            text: "Only for Al's friends".into(),
        };
        assert_eq!(
            draft.def().licence,
            Licence::Custom("Only for Al's friends".into())
        );
        draft.licence.text.clear();
        assert!(!draft.problems().is_empty());
    }

    #[test]
    fn an_exported_site_verifies_and_is_the_same_check_the_cli_makes() {
        let (dir, mut draft) = folder();
        draft.author = "Jo".into();
        let out = dir.path().join("site");
        let done = export(&draft, Format::Folder, &out).unwrap();
        assert_eq!((done.samples, done.written), (2, 2));
        assert_eq!(noodle_publish::verify(&out).unwrap(), 2);
        // The definition the wizard writes passes the CI-side checks too.
        let ron = draft.def().to_ron().unwrap();
        assert!(noodle_packs::PackDef::from_ron(&ron).is_ok());

        let zipped = export(&draft, Format::Zip, &dir.path().join("zips")).unwrap();
        assert!(zipped.path.ends_with("kit.zip") && zipped.path.is_file());
    }

    #[test]
    fn a_pack_with_problems_or_exported_into_its_own_folder_is_refused() {
        let (dir, mut draft) = folder();
        let out = dir.path().join("site");
        assert!(export(&draft, Format::Folder, &out).is_err());
        assert!(!out.exists(), "nothing is written for a pack that fails");
        draft.author = "Jo".into();
        let inside = draft.dir.join("site");
        assert!(export(&draft, Format::Folder, &inside).is_err());
        assert!(!inside.exists());
    }
}
