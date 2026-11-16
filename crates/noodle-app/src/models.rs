//! Drum separation models in the app: downloading and removing their weights,
//! and the Models tab of Settings.
//!
//! What a model is lives in `noodle_stems::drum` (manifests, and the store that
//! keeps the weights). A model is *ready* when its weights are on disk: there is
//! no separate on/off switch, a downloaded model is offered and Remove turns it
//! off. Downloading starts where the model is needed (the Create drum rack
//! dialog, the Split to stems dialog), which shows the licence and size first;
//! the Models tab is where they are managed afterwards.
//!
//! A model whose manifest has no address or checksum yet cannot be downloaded
//! (`StoreError::NotPublished`). The UI says "not available yet" for it.

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{RichText, Ui};
use noodle_engine::Job;
use noodle_stems::drum::{ALL_MODELS, DrumModelManifest, DrumModelStore, StoreError};
use noodle_stems::{DownloadError, ModelSpec, ModelStore, RuntimeSpec};

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

/// Fetches a Split to stems model: ONNX Runtime first when the machine has
/// none, then the weights. Tests swap in one that needs no network.
type StemFetch = Arc<
    dyn Fn(&ModelStore, &ModelSpec, &mut dyn FnMut(f32) -> bool) -> Result<(), DownloadError>
        + Send
        + Sync,
>;

fn fetch_stem_model(
    store: &ModelStore,
    spec: &ModelSpec,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<(), DownloadError> {
    // The runtime is a few to 70 MB; the weights are over 300.
    let share = 0.05;
    let mut base = 0.0;
    if let Some(runtime) = RuntimeSpec::current()
        && !store.has_runtime(&runtime)
        && std::env::var("ORT_DYLIB_PATH").map_or(true, |p| p.is_empty())
    {
        store.download_runtime(&runtime, &mut |f| progress(f * share))?;
        base = share;
    }
    store
        .download(spec, &mut |f| progress(base + f * (1.0 - base)))
        .map(|_| ())
}

pub struct Models {
    manifests: Vec<&'static DrumModelManifest>,
    store: Option<DrumModelStore>,
    fetch: Fetch,
    running: BTreeMap<&'static str, Download>,
    /// Why the last download or removal of a model failed.
    problems: BTreeMap<&'static str, String>,
    /// Split to stems: its models, where their weights are kept, and the
    /// downloads under way and what went wrong with them.
    stem_specs: Vec<ModelSpec>,
    stem_store: Option<ModelStore>,
    stem_fetch: StemFetch,
    stem_running: BTreeMap<&'static str, Job<Result<(), DownloadError>>>,
    stem_problems: BTreeMap<&'static str, String>,
}

/// Where the drum separation weights are kept, for the Models tab and for the
/// analyser that opens them.
pub fn default_drum_store() -> Option<DrumModelStore> {
    directories::ProjectDirs::from("", "", "Noodle")
        .map(|dirs| DrumModelStore::new(dirs.data_dir().join("models").join("drums")))
}

impl Default for Models {
    fn default() -> Self {
        let store = default_drum_store();
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
            stem_specs: ModelSpec::all(),
            stem_store: directories::ProjectDirs::from("", "", "Noodle")
                .map(|dirs| ModelStore::new(dirs.data_dir().join("models"))),
            stem_fetch: Arc::new(fetch_stem_model),
            stem_running: BTreeMap::new(),
            stem_problems: BTreeMap::new(),
        }
    }

    pub fn stem_downloaded(&self, model: &ModelSpec) -> bool {
        self.stem_store
            .as_ref()
            .is_some_and(|s| s.is_present(model))
    }

    /// Starts downloading a Split to stems model, unless it is here or under way.
    pub fn download_stem(&mut self, model: &ModelSpec) {
        let Some(store) = self.stem_store.clone() else {
            self.stem_problems.insert(
                model.id,
                "There is nowhere to keep the model on this machine".into(),
            );
            return;
        };
        if self.stem_running.contains_key(model.id) || store.is_present(model) {
            return;
        }
        self.stem_problems.remove(model.id);
        let (fetch, spec) = (Arc::clone(&self.stem_fetch), model.clone());
        self.stem_running.insert(
            model.id,
            Job::spawn(move |progress| fetch(&store, &spec, &mut |f| progress.report(f).is_ok())),
        );
    }

    pub fn remove_stem(&mut self, model: &ModelSpec) {
        let Some(store) = &self.stem_store else {
            return;
        };
        match store.remove(model) {
            Ok(()) => {
                self.stem_problems.remove(model.id);
            }
            Err(e) => {
                self.stem_problems.insert(model.id, e.to_string());
            }
        }
    }

    /// Where the weights are kept.
    pub fn folder(&self) -> Option<&std::path::Path> {
        self.store.as_ref().map(|s| s.dir())
    }

    pub fn is_downloaded(&self, model: &DrumModelManifest) -> bool {
        self.store.as_ref().is_some_and(|s| s.is_downloaded(model))
    }

    pub fn is_downloading(&self, model: &DrumModelManifest) -> bool {
        self.running.contains_key(model.id)
    }

    /// The models the drum rack dialog offers: the downloaded ones, in the
    /// order the app lists them.
    pub fn ready(&self) -> Vec<&'static DrumModelManifest> {
        self.manifests
            .iter()
            .copied()
            .filter(|m| self.is_downloaded(m))
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

    /// Deletes the weights of `model`. It is not offered until it is
    /// downloaded again.
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
        let mut stem_done = Vec::new();
        for (id, job) in &mut self.stem_running {
            if let Some(result) = job.poll() {
                stem_done.push((*id, result));
            }
        }
        for (id, result) in stem_done {
            self.stem_running.remove(id);
            match result {
                Ok(Ok(())) | Ok(Err(DownloadError::Cancelled)) => {
                    self.stem_problems.remove(id);
                }
                Ok(Err(e)) => {
                    self.stem_problems.insert(id, e.to_string());
                }
                Err(e) => {
                    self.stem_problems.insert(id, e.to_string());
                }
            }
        }
        if !self.running.is_empty() || !self.stem_running.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    // ---- the Models tab ----

    /// Whether any model is downloading (for the tab's status dot).
    pub fn any_downloading(&self) -> bool {
        !self.running.is_empty() || !self.stem_running.is_empty()
    }

    /// The body of the Models tab: one compact list, drum models then the
    /// Split to stems model. Licence, author and source sit behind a row's
    /// disclosure; the non-commercial chip stays on the row.
    pub fn body(&mut self, ui: &mut Ui) {
        for model in self.manifests.clone() {
            self.drum_row(ui, model);
        }
        for model in self.stem_specs.clone() {
            self.stem_row(ui, &model);
        }
        ui.add_space(8.0);
        ui.separator();
        let mut total = 0;
        for m in &self.manifests {
            total += self.store.as_ref().map_or(0, |s| s.size_on_disk(m));
        }
        for m in &self.stem_specs {
            total += self.stem_store.as_ref().map_or(0, |s| s.size_on_disk(m));
        }
        match self.folder() {
            Some(folder) => {
                crate::settings::show_file_button(ui, "Show file", folder);
                ui.horizontal_wrapped(|ui| {
                    ui.weak(format!("{} on this computer, in", human_size(total)));
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
    }

    /// One row: name, what it is used by, size, and one action.
    fn row_header(
        ui: &mut Ui,
        name: &str,
        used_by: &str,
        size: String,
        non_commercial: bool,
        status: impl FnOnce(&mut Ui),
    ) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).strong());
            ui.weak(used_by);
            if non_commercial {
                ui.colored_label(ui.visuals().warn_fg_color, "Non-commercial");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                status(ui);
                ui.weak(size);
            });
        });
    }

    fn drum_row(&mut self, ui: &mut Ui, model: &'static DrumModelManifest) {
        let downloaded = self.is_downloaded(model);
        let size = model.size().map_or("size unknown".to_owned(), human_size);
        let mut act = None;
        ui.separator();
        Self::row_header(
            ui,
            model.name,
            "Create drum rack",
            size,
            model.non_commercial,
            |ui| {
                if self.is_downloading(model) {
                    if ui.button("Cancel").clicked() {
                        act = Some(false);
                    }
                    let f = self.running.get(model.id).map_or(0.0, |j| j.fraction());
                    ui.add(
                        egui::ProgressBar::new(f)
                            .desired_width(90.0)
                            .show_percentage(),
                    );
                } else if downloaded {
                    if ui.button("Remove").clicked() {
                        act = Some(false);
                    }
                    ui.weak("Downloaded");
                } else if model.is_published() {
                    if ui.button("Download").clicked() {
                        act = Some(true);
                    }
                } else {
                    ui.weak("Not available yet");
                }
            },
        );
        match act {
            Some(true) => self.download(model),
            Some(false) if self.running.contains_key(model.id) => self.cancel(model),
            Some(false) => self.remove(model),
            None => {}
        }
        ui.indent(model.id, |ui| {
            egui::CollapsingHeader::new("Details")
                .id_salt(("drum details", model.id))
                .show(ui, |ui| facts(ui, model));
            if let Some(problem) = self.problems.get(model.id) {
                ui.colored_label(ui.visuals().error_fg_color, problem);
            }
        });
    }

    fn stem_row(&mut self, ui: &mut Ui, model: &ModelSpec) {
        let info = model.info();
        let downloaded = self.stem_downloaded(model);
        let mut act = None;
        ui.separator();
        Self::row_header(
            ui,
            info.name,
            "Split to stems",
            human_size(info.bytes),
            false,
            |ui| {
                if let Some(job) = self.stem_running.get(model.id) {
                    if ui.button("Cancel").clicked() {
                        job.cancel();
                    }
                    ui.add(
                        egui::ProgressBar::new(job.fraction())
                            .desired_width(90.0)
                            .show_percentage(),
                    );
                } else if downloaded {
                    if ui.button("Remove").clicked() {
                        act = Some(false);
                    }
                    ui.weak("Downloaded");
                } else if ui.button("Download").clicked() {
                    act = Some(true);
                }
            },
        );
        match act {
            Some(true) => self.download_stem(model),
            Some(false) => self.remove_stem(model),
            None => {}
        }
        ui.indent(model.id, |ui| {
            egui::CollapsingHeader::new("Details")
                .id_salt(("stem details", model.id))
                .show(ui, |ui| {
                    ui.label(format!("Separates: {}", info.separates));
                    ui.label(format!("Licence: {}", info.licence));
                    ui.weak(format!("By {}", info.credit));
                    ui.weak("Runs on ONNX Runtime, which is downloaded with it.");
                    if !info.source_url.is_empty() {
                        ui.hyperlink_to("Source", info.source_url);
                    }
                });
            if let Some(problem) = self.stem_problems.get(model.id) {
                ui.colored_label(ui.visuals().error_fg_color, problem);
            }
        });
    }

    /// Inline download offer for Create drum rack: each model not on this
    /// computer, with what it separates, its licence and size first, then a
    /// Download button. Says what is under way.
    pub fn offer(&mut self, ui: &mut Ui) {
        let mut start = None;
        for &m in &self.manifests {
            if self.is_downloaded(m) {
                continue;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(m.name).strong());
                let size = m.size().map_or("size unknown".to_owned(), human_size);
                ui.weak(format!("{size} · {}", m.licence));
                if m.non_commercial {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Non-commercial use only: not for music you sell.",
                    );
                }
                if let Some(job) = self.running.get(m.id) {
                    ui.add(
                        egui::ProgressBar::new(job.fraction())
                            .desired_width(110.0)
                            .show_percentage(),
                    );
                    if ui.small_button("Cancel").clicked() {
                        job.cancel();
                    }
                } else if m.is_published() {
                    if ui
                        .button("Download")
                        .on_hover_text(format!(
                            "Separates: {}. By {}. Downloaded once from its author's page and \
                             kept on this computer; your audio is never uploaded.",
                            m.parts
                                .iter()
                                .map(|p| p.label())
                                .collect::<Vec<_>>()
                                .join(", "),
                            m.credit
                        ))
                        .clicked()
                    {
                        start = Some(m);
                    }
                } else {
                    ui.weak("Not available yet");
                }
            });
            if let Some(problem) = self.problems.get(m.id) {
                ui.colored_label(ui.visuals().error_fg_color, problem);
            }
        }
        if let Some(m) = start {
            self.download(m);
        }
    }

    /// Whether some drum model could still be downloaded here.
    pub fn has_offer(&self) -> bool {
        self.manifests.iter().any(|m| !self.is_downloaded(m))
    }
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
    fn a_model_is_ready_once_downloaded_and_not_after_removal() {
        let dir = tempfile::tempdir().unwrap();
        prefs_in(dir.path());
        let a = leak_model("a", true, false);
        let mut m = models(vec![a], &dir.path().join("w"));
        let ctx = egui::Context::default();
        assert!(m.ready().is_empty() && m.has_offer());
        m.download(a);
        finish(&mut m, &ctx);
        assert!(m.is_downloaded(a));
        assert_eq!(m.ready(), vec![a]);
        assert!(!m.has_offer());
        m.remove(a);
        assert!(m.ready().is_empty() && m.has_offer());
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
    fn a_stem_model_can_be_downloaded_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        prefs_in(dir.path());
        let mut m = models(vec![], &dir.path().join("w"));
        m.stem_store = Some(ModelStore::new(dir.path().join("s")));
        m.stem_fetch = Arc::new(|store, spec, progress| {
            std::fs::create_dir_all(store.dir()).unwrap();
            std::fs::write(store.path(spec), b"weights").unwrap();
            progress(1.0);
            Ok(())
        });
        let model = m.stem_specs[0].clone();
        let ctx = egui::Context::default();
        assert!(!m.stem_downloaded(&model));
        m.download_stem(&model);
        for _ in 0..200 {
            m.poll(&ctx);
            if m.stem_running.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(m.stem_downloaded(&model));
        m.remove_stem(&model);
        assert!(!m.stem_downloaded(&model));
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(human_size(500), "0 kB");
        assert_eq!(human_size(12_000_000), "12 MB");
        assert_eq!(human_size(1_500_000_000), "1.5 GB");
    }
}
