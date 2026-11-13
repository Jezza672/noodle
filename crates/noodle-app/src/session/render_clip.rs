//! Render to clip: a track or group, or a stretch of it, written to an audio
//! file in the project's renders folder and added as a new audio track. The
//! original is muted and kept.

use super::*;
use crate::timeline::add_track;
use noodle_core::group::{MUTE, PORT_NAME};
use noodle_core::{Clip, Value};
use noodle_engine::{Job, JobPanicked};
use noodle_nodes::{
    ClipReport, ClipRequest, ExportJobError, MAX_TAIL_SECONDS, RenderRequest, spawn_render_clip,
};

/// The renders folder's name for a project called `name`: `<name> renders`,
/// next to the project file.
pub fn renders_folder(name: &str) -> String {
    format!("{name} renders")
}

/// A render to clip under way.
pub(super) struct Run {
    job: Job<Result<ClipReport, ExportJobError>>,
    /// The group rendered to a new track, or `None` for a bounce of the mix
    /// that becomes a Sample node.
    group: Option<NodeId>,
    name: String,
    /// The file's path relative to the project file, as a clip names it.
    source: String,
    path: PathBuf,
    start: Tick,
}

impl Session {
    /// Starts rendering `group` (a track or bus) to a clip. With a `range`,
    /// in ticks, only that part; otherwise the whole timeline. The render
    /// runs on past the end until the group's effects fall silent, to at
    /// most ten seconds. When it is done the clip arrives on a new track,
    /// and the original is muted, as one undo step.
    pub fn render_to_clip(&mut self, group: NodeId, range: Option<(Tick, Tick)>) {
        self.start_render(Some(group), range);
    }

    /// Starts bouncing the mix over `range`, in ticks, to a file in the
    /// project's renders folder. When it is done a Sample node playing it
    /// arrives, as one undo step. The mix is what the Output nodes play, so
    /// it is the sound of every track.
    pub fn bounce_to_sample(&mut self, range: (Tick, Tick)) {
        self.start_render(None, Some(range));
    }

    fn start_render(&mut self, group: Option<NodeId>, range: Option<(Tick, Tick)>) {
        if self.rendering.is_some() {
            self.message = Some("A render is already running".into());
            return;
        }
        let Some(dir) = self.directory().map(Path::to_path_buf) else {
            self.message = Some("Save the project first: the render is written next to it".into());
            return;
        };
        let name = match group {
            Some(group) => {
                let Some((_, name)) = self
                    .exportable_groups()
                    .into_iter()
                    .find(|(id, _)| *id == group)
                else {
                    self.message =
                        Some("Only a track or group with an output can be rendered".into());
                    return;
                };
                name
            }
            None => "Bounce".to_owned(),
        };
        let rate = self.export_rate();
        let table = TempoTable::new(self.project.tempo_map(), rate);
        let (start, end) = match range {
            Some((from, to)) => (from.0.max(0), to.0),
            None => (0, i64::MAX),
        };
        let start_frame = table.sample_at_tick(Tick(start));
        let end_frame = if end == i64::MAX {
            self.timeline_end(rate)
        } else {
            table.sample_at_tick(Tick(end))
        };
        if end_frame <= start_frame {
            self.message = Some("There is nothing to render".into());
            return;
        }
        let folder = dir.join(renders_folder(&self.name()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.message = Some(format!("Couldn't make the renders folder: {e}"));
            return;
        }
        // A file name no earlier render took.
        let taken: Vec<String> = std::fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                Path::new(&e.file_name())
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .collect();
        let file = export::unique_file_name(&name, &taken);
        let path = folder.join(format!("{file}.wav"));
        let source = format!("{}/{file}.wav", renders_folder(&self.name()));
        let freezer = if Freezing::in_use(&self.project, &self.registry) {
            match self.freezing.open_freezer() {
                Ok(freezer) => Some(freezer),
                Err(why) => {
                    self.message = Some(format!("Couldn't render {name}: {why}"));
                    return;
                }
            }
        } else {
            None
        };
        let request = ClipRequest {
            render: RenderRequest {
                project: self.project.clone(),
                base: self.base().to_owned(),
                settings: Settings {
                    sample_rate: rate,
                    max_frames: FREEZE_SETTINGS.max_frames,
                    channels: FREEZE_SETTINGS.channels,
                },
                frames: self.freeze_frames(rate),
                extend_registry: self.extend_registry(),
            },
            group,
            name: name.clone(),
            start: start_frame,
            end: end_frame,
            max_tail: (MAX_TAIL_SECONDS * f64::from(rate)).round() as u64,
            path: path.clone(),
            freezer,
        };
        self.rendering = Some(Run {
            job: spawn_render_clip(request),
            group,
            name,
            source,
            path,
            start: Tick(start),
        });
    }

    /// How far the render under way has got, from 0 to 1, and what it is
    /// rendering.
    pub fn render_progress(&self) -> Option<(f32, &str)> {
        self.rendering
            .as_ref()
            .map(|run| (run.job.fraction(), run.name.as_str()))
    }

    /// Stops the render under way. Nothing is left of it.
    pub fn cancel_render(&mut self) {
        if let Some(run) = &self.rendering {
            run.job.cancel();
        }
    }

    /// Takes in a finished render: adds the track and clip, or says why not.
    pub(super) fn poll_render(&mut self) {
        let Some(run) = &mut self.rendering else {
            return;
        };
        let Some(done) = run.job.poll() else {
            return;
        };
        let run = self.rendering.take().expect("just polled");
        let name = run.name.clone();
        match done {
            Ok(Ok(report)) => match self.render_command(&run, report.frames) {
                Ok(command) => {
                    self.edit([Edit::Apply(command)]);
                    let mut text = match run.group {
                        Some(_) => format!("Rendered {name} to a clip"),
                        None => "Bounced the selection to a Sample node".to_owned(),
                    };
                    if report.capped {
                        text.push_str(&format!(
                            ", cut at {MAX_TAIL_SECONDS:.0} s of tail while still sounding"
                        ));
                    }
                    if !report.is_complete() {
                        text.push_str(", but some audio files couldn't be read");
                    }
                    self.message = Some(text);
                }
                Err(why) => {
                    let _ = std::fs::remove_file(&run.path);
                    self.message = Some(format!("Couldn't render {name}: {why}"));
                }
            },
            Ok(Err(ExportJobError::Cancelled)) => self.message = Some("Render cancelled".into()),
            Ok(Err(error)) => self.message = Some(format!("Couldn't render {name}: {error}")),
            Err(JobPanicked) => {
                self.message = Some(format!("Couldn't render {name}: {JobPanicked}"))
            }
        }
    }

    /// The command that adds a track holding the rendered clip just after the
    /// original, and mutes the original: one undo step.
    fn render_command(&self, run: &Run, frames: u64) -> Result<Command, String> {
        let Some(source_group) = run.group else {
            let hash = self
                .samples()
                .import(&run.path)
                .map_err(|e| format!("couldn't read the bounce: {e}"))?;
            return Ok(self.sample_node_command(&noodle_nodes::SampleRef::whole(hash)));
        };
        let project = &self.project;
        let graph = project.graph();
        let mute = graph
            .control_target(source_group, MUTE)
            .ok_or("the track was removed while it was being rendered")?;
        let mut scratch = project.clone();
        let (track, input) = add_track::with_input(scratch.graph(), || self.new_node_id());
        track
            .clone()
            .apply(&mut scratch)
            .map_err(|e| e.to_string())?;
        let group = scratch
            .graph()
            .node(input)
            .and_then(|n| n.parent)
            .ok_or("the new track has no group")?;
        let rename = Command::SetConfig {
            node: group,
            key: PORT_NAME.into(),
            value: Some(Value::Text(format!("{} (render)", run.name))),
        };
        rename
            .clone()
            .apply(&mut scratch)
            .map_err(|e| e.to_string())?;
        let clip = Command::AddClip {
            id: project.next_clip_id(),
            clip: Clip::audio(input, run.start, run.source.clone(), frames),
        };
        let mute = Command::SetParam {
            node: mute.0,
            key: mute.1,
            value: Some(1.0),
        };
        let order = Command::SetTrackOrder(stems::order_after(&scratch, source_group, &[group]));
        Ok(Command::Batch(vec![track, rename, clip, mute, order]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Command;

    const RATE: u32 = 44_100;

    /// A saved project with a one-second file on a track, from the start.
    fn rig(dir: &Path) -> (Session, NodeId) {
        let mut session = Session::new(crate::session::Nodes::all());
        let (track, input) =
            add_track::with_input(session.project().graph(), || session.new_node_id());
        session.edit([Edit::Apply(track)]);
        let samples: Vec<f32> = (0..RATE as usize)
            .flat_map(|i| {
                let x = 0.3 * (std::f32::consts::TAU * 440.0 * i as f32 / RATE as f32).sin();
                [x, x]
            })
            .collect();
        noodle_io::write_wav(&dir.join("song.wav"), &samples, 2, RATE).unwrap();
        let id = session.project().next_clip_id();
        session.edit([Edit::Apply(Command::AddClip {
            id,
            clip: Clip::audio(input, Tick(0), "song.wav", u64::from(RATE)),
        })]);
        assert!(session.save_as(&dir.join("project.ron")));
        let group = session
            .project()
            .graph()
            .node(input)
            .unwrap()
            .parent
            .unwrap();
        (session, group)
    }

    fn wait(session: &mut Session) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while session.render_progress().is_some() {
            assert!(Instant::now() < deadline, "the render never finished");
            std::thread::sleep(Duration::from_millis(5));
            session.maintain();
        }
    }

    #[test]
    fn rendering_a_track_adds_a_track_with_the_clip_and_mutes_the_original_in_one_undo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, group) = rig(dir.path());
        let before = session.project().clone();

        session.render_to_clip(group, None);
        assert!(session.render_progress().is_some());
        wait(&mut session);
        assert_eq!(session.message(), Some("Rendered Track 1 to a clip"));

        // A new track right under the original, holding the one clip.
        let project = session.project();
        let tracks = crate::timeline::tracks(project);
        assert_eq!(tracks.len(), 2);
        let (_, clip) = project
            .clips()
            .find(|(_, c)| c.node == tracks[1])
            .expect("a clip on the new track");
        let audio = clip.as_audio().unwrap();
        assert_eq!(audio.source, "project renders/Track 1.wav");
        assert_eq!(clip.start, Tick(0));
        assert!(audio.length >= u64::from(RATE) - 1);
        let file = dir.path().join("project renders/Track 1.wav");
        let written = noodle_io::read_wav(&file).unwrap();
        assert_eq!(written.samples.len() as u64, audio.length * 2);
        assert!(written.samples.iter().any(|s| s.abs() > 0.1));
        // The original is muted and kept.
        assert!(project.graph().group_controls(group).mute);
        assert_eq!(project.clips().count(), 2);
        assert!(
            session.clip_problems().is_empty(),
            "{:?}",
            session.clip_problems()
        );

        // One undo takes all of it back, and one redo restores it.
        session.undo();
        assert_eq!(*session.project(), before);
        session.redo();
        assert_eq!(crate::timeline::tracks(session.project()).len(), 2);
        assert!(session.project().graph().group_controls(group).mute);
    }

    #[test]
    fn bouncing_a_range_makes_a_sample_node_of_the_mix_in_one_undo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _) = rig(dir.path());
        let before = session.project().clone();
        // Half a second in, a quarter of a second long, at 120 bpm.
        session.bounce_to_sample((Tick(960), Tick(1_440)));
        wait(&mut session);
        assert_eq!(
            session.message(),
            Some("Bounced the selection to a Sample node")
        );
        let nodes: Vec<_> = session
            .project()
            .graph()
            .nodes()
            .filter(|(_, n)| n.type_id == noodle_nodes::SAMPLE_INPUT_ID)
            .collect();
        assert_eq!(nodes.len(), 1);
        let Some(Value::Text(text)) = nodes[0].1.config.get(noodle_nodes::SAMPLE_KEY) else {
            panic!("the node has no sample")
        };
        let sample = session
            .samples()
            .get(&noodle_nodes::SampleRef::parse(text).hash)
            .expect("the bounce is in the sample store");
        let rate = session.export_rate();
        assert!(sample.frames() as f64 >= f64::from(rate) / 4.0 - 1.0);
        assert!(sample.left.iter().any(|s| s.abs() > 0.05));
        // The mix is not muted or replaced: nothing else changed.
        assert_eq!(session.project().clips().count(), 1);
        session.undo();
        assert_eq!(*session.project(), before);
    }

    #[test]
    fn a_range_is_rendered_from_its_start_and_an_unsaved_project_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, group) = rig(dir.path());
        // Half a second in, a quarter of a second long, at 120 bpm.
        session.render_to_clip(group, Some((Tick(960), Tick(1_440))));
        wait(&mut session);
        let project = session.project();
        let tracks = crate::timeline::tracks(project);
        let (_, clip) = project.clips().find(|(_, c)| c.node == tracks[1]).unwrap();
        assert_eq!(clip.start, Tick(960));
        let rate = u64::from(session.export_rate().round() as u32);
        let audio = clip.as_audio().unwrap();
        // A quarter second, and the tail, which the source's end cuts short.
        assert!(audio.length >= rate / 4, "{}", audio.length);
        assert!(audio.length < rate, "{}", audio.length);

        let mut unsaved = Session::new(crate::session::Nodes::all());
        let (track, input) =
            add_track::with_input(unsaved.project().graph(), || unsaved.new_node_id());
        unsaved.edit([Edit::Apply(track)]);
        let group = unsaved
            .project()
            .graph()
            .node(input)
            .unwrap()
            .parent
            .unwrap();
        unsaved.render_to_clip(group, None);
        assert!(unsaved.render_progress().is_none());
        assert!(
            unsaved
                .message()
                .unwrap()
                .contains("Save the project first")
        );
    }
}
