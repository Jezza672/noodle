//! Drum separation models in the app: which are enabled, downloading and
//! removing their weights, the Models tab of Settings and the first-use popup.
//!
//! What a model is lives in `noodle_stems::drum` (manifests, and the store that
//! keeps the weights). This is the user's side: *enabled* is a setting kept in
//! the preferences (a model not in them has not been asked about yet), and a
//! model is *ready* when it is enabled and its weights are on disk. The drum
//! rack dialog offers only the ready ones.
//!
//! A model whose manifest has no address or checksum yet cannot be downloaded
//! (`StoreError::NotPublished`). The UI says "not available yet" for it, and
//! the first-use popup leaves it unasked until it can be downloaded.

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{RichText, Ui};
use noodle_engine::Job;
use noodle_stems::drum::{ALL_MODELS, DrumModelManifest, DrumModelStore, StoreError};

/// Fetches a model's weights into a store, reporting progress (0 to 1) and
/// answering whether to go on. Tests swap in one that needs no network.
type Fetch = Arc<
    dyn Fn(
            &DrumModelStore,
            &DrumModelManifest,
            &mut dyn FnMut(f32) -> bool,
        ) -> Result<(), StoreError>
        + Send
        + Sync,
>;

type Download = Job<Result<(), StoreError>>;

pub struct Models {
    manifests: Vec<&'static DrumModelManifest>,
    store: Option<DrumModelStore>,
    fetch: Fetch,
    running: BTreeMap<&'static str, Download>,
    /// Why the last download or removal of a model failed.
    problems: BTreeMap<&'static str, String>,
    /// The first-use popup's toggles, while it is open.
    popup: Option<BTreeMap<&'static str, bool>>,
}

impl Default for Models {
    fn default() -> Self {
        let store = directories::ProjectDirs::from("", "", "Noodle")
            .map(|dirs| DrumModelStore::new(dirs.data_dir().join("models").join("drums")));
        Self::with(
            ALL_MODELS.to_vec(),
            store,
            Arc::new(|store, model, progress| store.fetch(model, progress)),
        )
    }
}

impl Models {
    fn with(
        manifests: Vec<&'static DrumModelManifest>,
        store: Option<DrumModelStore>,
        fetch: Fetch,
    ) -> Self {
        Self {
            manifests,
            store,
            fetch,
            running: BTreeMap::new(),
            problems: BTreeMap::new(),
            popup: None,
        }
    }

    /// Where the weights are kept.
    pub fn folder(&self) -> Option<&std::path::Path> {
        self.store.as_ref().map(|s| s.dir())
    }

    /// Whether the user has switched `model` on. Not asked about yet is off.
    pub fn is_enabled(&self, model: &DrumModelManifest) -> bool {
        crate::prefs::drum_models()
            .get(model.id)
            .copied()
            .unwrap_or(false)
    }

    pub fn set_enabled(&self, model: &DrumModelManifest, on: bool) {
        crate::prefs::remember_drum_model(model.id, on);
    }

    pub fn is_downloaded(&self, model: &DrumModelManifest) -> bool {
        self.store.as_ref().is_some_and(|s| s.is_downloaded(model))
    }

    pub fn is_downloading(&self, model: &DrumModelManifest) -> bool {
        self.running.contains_key(model.id)
    }

    /// The models the drum rack dialog offers: enabled, and downloaded, in the
    /// order the app lists them.
    pub fn ready(&self) -> Vec<&'static DrumModelManifest> {
        self.manifests
            .iter()
            .copied()
            .filter(|m| self.is_enabled(m) && self.is_downloaded(m))
            .collect()
    }

    /// Starts downloading `model`, unless it is already under way or here.
    pub fn download(&mut self, model: &'static DrumModelManifest) {
        let Some(store) = self.store.clone() else {
            self.problems.insert(
                model.id,
                "There is nowhere to keep the model on this machine".into(),
            );
            return;
        };
        if self.running.contains_key(model.id) || store.is_downloaded(model) {
            return;
        }
        self.problems.remove(model.id);
        if !model.is_published() {
            self.problems.insert(model.id, NOT_PUBLISHED.into());
            return;
        }
        let fetch = Arc::clone(&self.fetch);
        self.running.insert(
            model.id,
            Job::spawn(move |progress| fetch(&store, model, &mut |f| progress.report(f).is_ok())),
        );
    }

    pub fn cancel(&self, model: &DrumModelManifest) {
        if let Some(job) = self.running.get(model.id) {
            job.cancel();
        }
    }

    /// Deletes the weights of `model`. It stays enabled, but is not offered
    /// until it is downloaded again.
    pub fn remove(&mut self, model: &DrumModelManifest) {
        let Some(store) = &self.store else { return };
        match store.remove(model) {
            Ok(()) => {
                self.problems.remove(model.id);
            }
            Err(e) => {
                self.problems.insert(model.id, e.to_string());
            }
        }
    }

    /// Collects finished downloads. Called every frame; keeps the frames
    /// coming while one is under way, for its progress bar.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut done = Vec::new();
        for (id, job) in &mut self.running {
            if let Some(result) = job.poll() {
                done.push((*id, result));
            }
        }
        for (id, result) in done {
            self.running.remove(id);
            match result {
                Ok(Ok(())) => {
                    self.problems.remove(id);
                }
                Ok(Err(e)) if e.is_cancelled() => {
                    self.problems.remove(id);
                }
                Ok(Err(e)) => {
                    self.problems.insert(id, e.to_string());
                }
                Err(e) => {
                    self.problems.insert(id, e.to_string());
                }
            }
        }
        if !self.running.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    // ---- first-use popup ----

    /// The models the popup would ask about: not answered yet, and
    /// downloadable.
    fn unasked(&self) -> Vec<&'static DrumModelManifest> {
        let answered = crate::prefs::drum_models();
        self.manifests
            .iter()
            .copied()
            .filter(|m| m.is_published() && !answered.contains_key(m.id))
            .collect()
    }

    /// Whether the first-use popup is due: some model is downloadable and has
    /// not been asked about.
    pub fn first_use_due(&self) -> bool {
        self.popup.is_some() || !self.unasked().is_empty()
    }

    /// Draws the first-use popup, which offers each model not answered for yet
    /// with a toggle. A model under a non-commercial licence starts off, so a
    /// download of it is a choice. Download selected enables and fetches the
    /// ticked ones; Not now leaves every one off. Either way the models are
    /// answered for, and the Models tab changes them later.
    pub fn show_first_use(&mut self, ctx: &egui::Context) {
        let asking = self.unasked();
        if asking.is_empty() {
            self.popup = None;
            return;
        }
        let mut toggles = self.popup.take().unwrap_or_default();
        for m in &asking {
            toggles.entry(m.id).or_insert(starts_on(m));
        }
        let mut answer = None;
        let modal = crate::dialog::modal(ctx, "models first use", |ui| {
            ui.set_max_width(480.0);
            ui.heading("Separation models");
            ui.add_space(6.0);
            ui.label(
                "Create drum rack can pull apart drums that sound together using an AI \
                 model. The models are not included with Noodle: each is downloaded once \
                 from its author's page and kept on this computer. Your audio is never \
                 uploaded. Pick the ones to download now.",
            );
            ui.add_space(6.0);
            for m in &asking {
                let on = toggles.entry(m.id).or_default();
                ui.checkbox(on, RichText::new(m.name).strong());
                ui.indent(m.id, |ui| facts(ui, m));
                ui.add_space(4.0);
            }
            if let Some(folder) = self.folder() {
                ui.weak(format!("Kept in {}", folder.display()));
            }
            ui.weak("You can change this later in Settings, on the Models tab.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let any = toggles.values().any(|on| *on);
                if ui
                    .add_enabled(any, egui::Button::new("Download selected"))
                    .clicked()
                {
                    answer = Some(true);
                }
                if ui.button("Not now").clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() {
            answer.get_or_insert(false);
        }
        match answer {
            Some(download) => self.answer(&asking, &toggles, download),
            None => self.popup = Some(toggles),
        }
    }

    /// Records the popup's answer for `asking`: with `download`, the ticked
    /// models are enabled and fetched and the rest are off; without, all are off.
    fn answer(
        &mut self,
        asking: &[&'static DrumModelManifest],
        toggles: &BTreeMap<&'static str, bool>,
        download: bool,
    ) {
        for &m in asking {
            let on = download && toggles.get(m.id).copied().unwrap_or(false);
            self.set_enabled(m, on);
            if on {
                self.download(m);
            }
        }
    }

    // ---- the Models tab ----

    /// The body of the Models tab: one block per model.
    pub fn body(&mut self, ui: &mut Ui) {
        ui.weak(
            "The AI models Create drum rack can separate drums with. A model is offered \
             there when it is enabled and downloaded. They are not included with Noodle.",
        );
        ui.add_space(4.0);
        match self.folder() {
            Some(folder) => {
                ui.horizontal(|ui| {
                    ui.label("Stored in");
                    ui.label(
                        RichText::new(folder.display().to_string())
                            .monospace()
                            .small(),
                    );
                });
            }
            None => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "There is nowhere to keep models on this machine.",
                );
            }
        }
        for model in self.manifests.clone() {
            ui.add_space(6.0);
            ui.separator();
            self.block(ui, model);
        }
    }

    fn block(&mut self, ui: &mut Ui, model: &'static DrumModelManifest) {
        let published = model.is_published();
        let downloaded = self.is_downloaded(model);
        let downloading = self.is_downloading(model);
        let mut enabled = self.is_enabled(model);
        if ui
            .checkbox(&mut enabled, RichText::new(model.name).strong())
            .on_hover_text("Offered in Create drum rack when on and downloaded")
            .changed()
        {
            self.set_enabled(model, enabled);
        }
        ui.indent(model.id, |ui| {
            facts(ui, model);
            let size = |bytes: u64| human_size(bytes);
            if downloading {
                let fraction = self.running.get(model.id).map_or(0.0, |j| j.fraction());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(200.0)
                            .show_percentage(),
                    );
                    if ui.button("Cancel").clicked() {
                        self.cancel(model);
                    }
                });
            } else if downloaded {
                let on_disk = self.store.as_ref().map_or(0, |s| s.size_on_disk(model));
                ui.horizontal(|ui| {
                    ui.label(format!("Downloaded, {} on disk", size(on_disk)));
                    if ui.button("Remove").clicked() {
                        self.remove(model);
                    }
                });
            } else if published {
                ui.horizontal(|ui| {
                    ui.label("Not downloaded");
                    if ui.button("Download").clicked() {
                        self.download(model);
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    ui.weak(NOT_PUBLISHED);
                    ui.add_enabled(false, egui::Button::new("Download"));
                });
            }
            if let Some(problem) = self.problems.get(model.id) {
                ui.colored_label(ui.visuals().error_fg_color, problem);
            }
        });
    }
}

/// A model under a non-commercial licence starts off in the popup, so
/// downloading it is a choice.
fn starts_on(m: &DrumModelManifest) -> bool {
    !m.non_commercial
}

const NOT_PUBLISHED: &str = "Not available yet: its download address is not published";

/// What the user should know before downloading: what it gives, the licence
/// (with the non-commercial warning), who made it, where it comes from and how
/// big it is.
fn facts(ui: &mut Ui, m: &DrumModelManifest) {
    let parts: Vec<_> = m.parts.iter().map(|p| p.label()).collect();
    ui.label(format!("Separates: {}", parts.join(", ")));
    if !m.lacks.is_empty() {
        let lacks: Vec<_> = m.lacks.iter().map(|p| p.label()).collect();
        ui.weak(format!("No separate {}", lacks.join(" or ")));
    }
    ui.label(format!(
        "Size: {}",
        m.size().map_or("not known yet".to_owned(), human_size)
    ));
    ui.label(format!("Licence: {}", m.licence));
    if m.non_commercial {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Non-commercial use only: not for music you sell.",
        );
    }
    ui.weak(format!("By {}", m.credit));
    ui.weak(m.provenance);
    ui.hyperlink_to("Source", m.source_url);
}

fn human_size(bytes: u64) -> String {
    const MB: f64 = 1_000_000.0;
    let b = bytes as f64;
    if b >= 1000.0 * MB {
        format!("{:.1} GB", b / (1000.0 * MB))
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else {
        format!("{:.0} kB", b / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use noodle_stems::drum::{Chunking, DrumPart, Layout, MaskOutput, ModelFile};

    use super::*;

    fn leak_model(id: &'static str, published: bool, nc: bool) -> &'static DrumModelManifest {
        let file = ModelFile {
            part: Some(DrumPart::Kick),
            name: "kick.onnx",
            url: published.then_some("http://example/kick.onnx"),
            sha256: published.then_some("ab"),
            size: Some(1_000_000),
        };
        Box::leak(Box::new(DrumModelManifest {
            id,
            name: id,
            version: "1",
            parts: &[DrumPart::Kick],
            lacks: &[],
            licence: if nc { "CC BY-NC 4.0" } else { "MIT" },
            non_commercial: nc,
            credit: "x",
            source_url: "https://example.com",
            sample_rate: 44_100,
            chunking: Chunking {
                segment_samples: 1024,
                stft_size: 64,
                hop: 16,
            },
            layout: Layout::PerPartSpectrogram(MaskOutput::Mask),
            files: Box::leak(Box::new([file])),
            provenance: "",
        }))
    }

    fn models(list: Vec<&'static DrumModelManifest>, dir: &std::path::Path) -> Models {
        // A fetch that "downloads" by writing the file the store expects.
        Models::with(
            list,
            Some(DrumModelStore::new(dir)),
            Arc::new(|store, model, progress| {
                for file in model.files {
                    let path = store.path(file).unwrap();
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, b"weights").unwrap();
                }
                progress(1.0);
                Ok(())
            }),
        )
    }

    fn prefs_in(dir: &std::path::Path) {
        crate::prefs::init(dir.join("prefs.ron"));
    }

    fn finish(models: &mut Models, ctx: &egui::Context) {
        for _ in 0..200 {
            models.poll(ctx);
            if models.running.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the download did not finish");
    }

    #[test]
    fn a_model_is_ready_when_enabled_and_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        prefs_in(dir.path());
        let a = leak_model("a", true, false);
        let mut m = models(vec![a], &dir.path().join("w"));
        let ctx = egui::Context::default();
        assert!(m.ready().is_empty());
        m.download(a);
        finish(&mut m, &ctx);
        assert!(m.is_downloaded(a));
        // Downloaded alone is not enough.
        assert!(m.ready().is_empty());
        m.set_enabled(a, true);
        assert_eq!(m.ready(), vec![a]);
        m.set_enabled(a, false);
        assert!(m.ready().is_empty());
        m.set_enabled(a, true);
        m.remove(a);
        assert!(m.ready().is_empty());
    }

    #[test]
    fn enabling_is_remembered_in_the_preferences() {
        let dir = tempfile::tempdir().unwrap();
        prefs_in(dir.path());
        let a = leak_model("a", true, false);
        let m = models(vec![a], &dir.path().join("w"));
        assert!(!m.is_enabled(a));
        m.set_enabled(a, true);
        let saved = crate::prefs::load(&dir.path().join("prefs.ron")).unwrap();
        assert_eq!(saved.drum_models.get("a"), Some(&true));
        // Portable settings leave it out: it is about this machine's files.
        assert!(saved.portable().drum_models.is_empty());
    }

    #[test]
    fn an_unpublished_model_cannot_be_downloaded_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        prefs_in(dir.path());
        let a = leak_model("a", false, false);
        let mut m = models(vec![a], &dir.path().join("w"));
        m.download(a);
        assert!(!m.is_downloading(a));
        assert_eq!(m.problems.get("a").map(String::as_str), Some(NOT_PUBLISHED));
        // And the popup has nothing to offer for it.
        assert!(!m.first_use_due());
    }

    #[test]
    fn the_popup_asks_once_per_model_and_non_commercial_starts_off() {
        let dir = tempfile::tempdir().unwrap();
        prefs_in(dir.path());
        let free = leak_model("free", true, false);
        let nc = leak_model("nc", true, true);
        let mut m = models(vec![free, nc], &dir.path().join("w"));
        let ctx = egui::Context::default();
        assert!(m.first_use_due());
        // Download selected with the popup's own defaults: the free one only.
        let asking = m.unasked();
        let toggles: BTreeMap<_, _> = asking.iter().map(|x| (x.id, starts_on(x))).collect();
        assert_eq!(toggles.get("free"), Some(&true));
        assert_eq!(toggles.get("nc"), Some(&false));
        m.answer(&asking, &toggles, true);
        finish(&mut m, &ctx);
        assert!(!m.first_use_due());
        assert!(m.is_enabled(free) && !m.is_enabled(nc));
        assert_eq!(m.ready(), vec![free]);
    }

    #[test]
    fn not_now_answers_every_model_off() {
        let dir = tempfile::tempdir().unwrap();
        prefs_in(dir.path());
        let free = leak_model("free", true, false);
        let mut m = models(vec![free], &dir.path().join("w"));
        let asking = m.unasked();
        let toggles = asking.iter().map(|x| (x.id, true)).collect();
        m.answer(&asking, &toggles, false);
        assert!(!m.first_use_due());
        assert!(!m.is_enabled(free));
        assert!(!m.is_downloading(free));
    }

    #[test]
    fn a_cancelled_download_is_not_a_problem_but_a_failure_is() {
        let dir = tempfile::tempdir().unwrap();
        prefs_in(dir.path());
        let a = leak_model("a", true, false);
        let ctx = egui::Context::default();
        for (error, shown) in [
            (
                StoreError::Download(noodle_network::Error::Cancelled),
                false,
            ),
            (
                StoreError::Download(noodle_network::Error::WrongChecksum),
                true,
            ),
        ] {
            let error = std::sync::Mutex::new(Some(error));
            let mut m = Models::with(
                vec![a],
                Some(DrumModelStore::new(dir.path().join("w"))),
                Arc::new(move |_, _, _| Err(error.lock().unwrap().take().unwrap())),
            );
            m.download(a);
            finish(&mut m, &ctx);
            assert_eq!(m.problems.contains_key("a"), shown);
        }
    }

    #[test]
    fn the_models_are_listed_in_the_agreed_order() {
        let ids: Vec<_> = Models::default().manifests.iter().map(|m| m.id).collect();
        assert_eq!(ids, ["larsnet", "timbrica", "drumsep"]);
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(human_size(500), "0 kB");
        assert_eq!(human_size(12_000_000), "12 MB");
        assert_eq!(human_size(1_500_000_000), "1.5 GB");
    }
}
