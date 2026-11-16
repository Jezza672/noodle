//! Share groups and presets as a pack.
//!
//! The user's own library entries (their saved groups and presets, not the
//! ones Noodle ships) are bundled into a pack the same way a folder of sounds
//! is: the same definition, the same validation CI runs, the same static site.
//! The samples the entries use go in the pack too, so whoever installs an entry
//! gets the sounds it needs from the same source.
//!
//! [`Draft`] is the pack being made, with no UI in it, so a test can drive it.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use egui::{Context, RichText, Ui};
use noodle_core::library::Item;
use noodle_engine::Job;
use noodle_nodes::SampleStore;
use noodle_packs::{FORMAT_VERSION, ItemDef, PackDef, SPDX_IDS, SampleDef, validate_dir};
use noodle_publish::{FoundPack, item_file, item_samples, kind_of, sha256_hex};

use super::Sounds;
use super::wizard::{Choice, Exported, Format, slug, tags_of_text};
use crate::session::Session;

/// A library entry that can be offered.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// The library's ID for it.
    pub id: String,
    pub item: Item,
    /// Why it can't be shared, if it can't.
    pub blocked: Option<String>,
}

/// The user's entries, which are the ones a pack can hold. An entry that
/// came from another pack under a licence that forbids passing it on is
/// listed but can't be picked.
pub fn candidates(session: &Session) -> Vec<Candidate> {
    session
        .library()
        .listed()
        .filter(|e| !e.is_system())
        .map(|e| {
            let blocked = e.item.origin().and_then(|o| {
                (!SPDX_IDS.contains(&o.licence.as_str())).then(|| {
                    format!(
                        "It came from {} under the {} licence, which doesn't let you pass it on",
                        o.pack, o.licence
                    )
                })
            });
            Candidate {
                id: e.id.clone(),
                item: e.item.clone(),
                blocked,
            }
        })
        .collect()
}

/// The pack being made.
#[derive(Clone)]
pub struct Draft {
    pub name: String,
    pub id: String,
    pub id_typed: bool,
    pub description: String,
    pub author: String,
    pub licence: Choice,
    pub tags: String,
    /// The IDs of the entries chosen.
    pub picked: HashSet<String>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            name: String::new(),
            id: String::new(),
            id_typed: false,
            description: String::new(),
            author: String::new(),
            licence: Choice::default(),
            tags: String::new(),
            picked: HashSet::new(),
        }
    }
}

impl Draft {
    pub fn set_name(&mut self, name: String) {
        self.name = name;
        if !self.id_typed {
            self.id = slug(&self.name);
        }
    }

    /// What stops it being exported, in words, before any files are made.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push("Give the pack a name");
        }
        if !noodle_packs::is_pack_id(&self.id) {
            out.push("The pack's id may hold only lowercase letters, digits and hyphens");
        }
        if self.author.trim().is_empty() {
            out.push("Say who the author is");
        }
        if self.picked.is_empty() {
            out.push("Pick at least one group or preset");
        }
        out
    }
}

/// The pack for `draft`, with its files written under `scratch`: each entry as
/// its own RON, each sound the entries use as a WAV. The entries are the
/// `picked` ones of `from`, in the order given.
pub fn prepare(
    draft: &Draft,
    from: &[Candidate],
    samples: &SampleStore,
    scratch: &Path,
) -> Result<FoundPack, String> {
    let write = |path: &Path, bytes: &[u8]| -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    };
    let author = draft.author.trim().to_owned();
    let licence = draft.licence.licence();
    let credit = |name: &str| {
        licence
            .requires_credit()
            .then(|| format!("{name} by {author} ({})", licence.label()))
    };
    let tags = tags_of_text(&draft.tags);

    let mut items = Vec::new();
    // The sounds the entries use, each once, with what uses it.
    let mut needed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in from.iter().filter(|c| draft.picked.contains(&c.id)) {
        if let Some(why) = &c.blocked {
            return Err(format!("{}: {why}", c.item.name()));
        }
        let text = item_file(&c.item);
        let hash = sha256_hex(text.as_bytes());
        let file = format!("items/{hash}.ron");
        write(&scratch.join(&file), text.as_bytes())?;
        let needs = item_samples(&c.item);
        for h in &needs {
            needed
                .entry(h.clone())
                .or_default()
                .push(c.item.name().to_owned());
        }
        items.push(ItemDef {
            hash,
            name: c.item.name().to_owned(),
            kind: kind_of(&c.item),
            file: Some(file),
            author: None,
            licence: None,
            tags: tags.clone(),
            credit: credit(c.item.name()),
            needs,
        });
    }

    let mut sounds = Vec::new();
    for (hash, users) in &needed {
        let sample = samples.get(hash).ok_or_else(|| {
            format!(
                "{} uses a sound that isn't on this computer ({})",
                users.join(", "),
                &hash[..8]
            )
        })?;
        let audio = sample.to_audio();
        let file = format!("samples/{hash}.wav");
        let path = scratch.join(&file);
        std::fs::create_dir_all(path.parent().expect("has a parent")).map_err(|e| e.to_string())?;
        noodle_io::write_wav(&path, &audio.samples, audio.channels, audio.sample_rate)
            .map_err(|e| e.to_string())?;
        let name = format!("Sound {}", &hash[..8]);
        sounds.push(SampleDef {
            hash: hash.clone(),
            file: Some(file),
            tags: tags.clone(),
            credit: credit(&name),
            name,
            author: None,
            licence: None,
        });
    }

    let def = PackDef {
        format: FORMAT_VERSION,
        id: draft.id.clone(),
        name: draft.name.trim().to_owned(),
        version: 1,
        description: draft.description.trim().to_owned(),
        author,
        licence,
        samples: sounds,
        items,
    };
    let problems = validate_dir(&def, scratch);
    if !problems.is_empty() {
        return Err(problems
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join("; "));
    }
    Ok(FoundPack {
        def,
        dir: scratch.to_owned(),
    })
}

/// Builds the site for `draft` at `out`: into the folder `out`, or as
/// `<out>/<id>.zip`.
pub fn export(
    draft: &Draft,
    from: &[Candidate],
    samples: &SampleStore,
    format: Format,
    out: &Path,
) -> Result<Exported, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let scratch = std::env::temp_dir().join(format!(
        "noodle-share-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let result = prepare(draft, from, samples, &scratch).and_then(|pack| {
        let packs = [pack];
        let fail = |e: noodle_publish::PublishError| e.to_string();
        let (path, report) = match format {
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
            samples: report.samples + report.items,
            written: report.written,
        })
    });
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// The window.
pub struct Share {
    draft: Draft,
    format: Format,
    exporting: Option<Job<Result<Exported, String>>>,
    exported: Option<Exported>,
    error: Option<String>,
}

impl Default for Share {
    fn default() -> Self {
        Self {
            draft: Draft::default(),
            format: Format::Folder,
            exporting: None,
            exported: None,
            error: None,
        }
    }
}

impl Share {
    pub fn busy(&self) -> bool {
        self.exporting.is_some()
    }

    fn poll(&mut self) {
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

    /// Draws the contents. Returns a folder to add as a source, when asked.
    fn contents(&mut self, ui: &mut Ui, session: &Session) -> Option<PathBuf> {
        ui.weak(
            "Makes a folder of files you can host anywhere, for others to add as a source. \
             The sounds your groups and presets use go in the pack too, so share only sounds \
             you have the right to. Nothing is uploaded.",
        );
        let from = candidates(session);
        if from.is_empty() {
            ui.label(
                "Your library has no groups or presets yet. Save one from a group's \
                 right-click menu, or from the Presets section.",
            );
            return None;
        }
        ui.label(RichText::new("The pack").strong());
        let d = &mut self.draft;
        egui::Grid::new("share details")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Pack name");
                let mut name = d.name.clone();
                if ui.text_edit_singleline(&mut name).changed() {
                    d.set_name(name);
                }
                ui.end_row();
                ui.label("Pack id");
                if ui.text_edit_singleline(&mut d.id).changed() {
                    d.id_typed = true;
                }
                ui.end_row();
                ui.label("Author");
                ui.text_edit_singleline(&mut d.author);
                ui.end_row();
                ui.label("Licence");
                ui.vertical(|ui| Choice::pick(ui, "share licence", &mut d.licence));
                ui.end_row();
                ui.label("Tags");
                ui.add(
                    egui::TextEdit::singleline(&mut d.tags).hint_text("comma, separated, words"),
                );
                ui.end_row();
                ui.label("Description");
                ui.text_edit_singleline(&mut d.description);
                ui.end_row();
            });
        ui.separator();
        ui.label(RichText::new("What to share").strong());
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for c in &from {
                    let mut on = d.picked.contains(&c.id);
                    let label = format!("{} · {}", c.item.name(), kind_label(&c.item));
                    let row =
                        ui.add_enabled(c.blocked.is_none(), egui::Checkbox::new(&mut on, label));
                    if let Some(why) = &c.blocked {
                        row.on_disabled_hover_text(why);
                    }
                    if on {
                        d.picked.insert(c.id.clone());
                    } else {
                        d.picked.remove(&c.id);
                    }
                }
            });
        ui.separator();
        let problems = self.draft.problems();
        for p in &problems {
            ui.colored_label(ui.visuals().error_fg_color, format!("• {p}"));
        }
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
                && let Some(out) = crate::picker::pick_folder_in("to put the pack in", None)
            {
                let (draft, format, samples) =
                    (self.draft.clone(), self.format, session.samples().clone());
                self.error = None;
                self.exported = None;
                self.exporting = Some(Job::spawn(move |_| {
                    export(&draft, &from, &samples, format, &out)
                }));
            }
            if self.exporting.is_some() {
                ui.spinner();
                ui.label("Making the pack…");
            }
        });
        if let Some(why) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, why);
        }
        if let Some(done) = &self.exported {
            ui.label(format!("Exported to {}.", done.path.display()));
            if done.format == Format::Folder {
                ui.weak(
                    "Put this folder on any web host that serves static files, then add its \
                     address as a source. To try it first, add it from here.",
                );
                if ui.button("Add it as a source").clicked() {
                    add = Some(done.path.clone());
                }
            } else {
                ui.weak("Unzip it on the host so index.json is at the address you share.");
            }
        }
        add
    }
}

fn kind_label(item: &Item) -> &'static str {
    match item {
        Item::Template(_) => "Group",
        Item::NodePreset(_) => "Node preset",
        Item::GroupPreset(_) => "Group preset",
    }
}

/// Draws the window, if it is open.
pub(super) fn show(sounds: &mut Sounds, ctx: &Context, session: &Session) {
    let Some(mut share) = sounds.share.take() else {
        return;
    };
    share.poll();
    if share.busy() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    let mut open = true;
    let mut add = None;
    egui::Window::new("Share groups and presets")
        .open(&mut open)
        .default_width(520.0)
        .default_pos([160.0, 80.0])
        .show(ctx, |ui| add = share.contents(ui, session));
    if let Some(path) = add {
        let name = share.draft.name.clone();
        if let Err(why) = sounds.add_site(&path, &name) {
            share.error = Some(why);
        }
    }
    if open {
        sounds.share = Some(share);
    }
}
