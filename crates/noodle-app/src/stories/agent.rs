//! The "new user" driver: lets an agent that knows nothing about Noodle use
//! the app, through the same pointer and keyboard events a person's hands
//! produce, and logs everything it does.
//!
//! This is the harness behind the naive-agent runs (U13, findability). It is
//! an ignored test, run by `scripts/new_user/start.sh` and never in CI:
//!
//! ```sh
//! NOODLE_AGENT_STORY=S12 NOODLE_AGENT_DIR=/some/run \
//!   cargo test -p noodle-app stories::agent::drive -- --ignored --nocapture
//! ```
//!
//! It starts the story's route fixture, then serves commands from files, so
//! any agent with a shell (and, if it can look, image reading) can drive it,
//! whatever model it is. `scripts/new_user/ctl.sh` is the client. See
//! `docs/usability/new-user-runs.md` for the protocol and the commands.
//!
//! Files in `NOODLE_AGENT_DIR`, which is all the agent can see:
//! - `cmd.N` (written by the client) and `resp.N` (written here): one command
//!   and its answer, numbered from 1.
//! - `ready`: written when the app is up.
//! - `world/`: the fixture's files, where the user's samples and so on are.
//! - `screens/NNN.png`: the screen after each action.
//! - `over`: written when the run ends.
//!
//! Files in `NOODLE_AGENT_PRIVATE`, which the agent is never pointed at:
//! - `log.jsonl`: every command with its result, the widget and view it hit,
//!   and the seconds since the start.
//! - `result.json`: written when the run ends: whether the story's done check
//!   passed, and the action count.
//! - `prefs.ron`: the app's own settings.
//!
//! The agent is never told whether the done check passed.

use std::cell::Cell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers, MouseWheelUnit, Pos2, Vec2};
use egui_kittest::kittest::NodeT;

use super::handle::{AppHarness, Probe};
use super::{Route, routes, window_with};
use crate::testkit;

/// How many actions an agent gets before the run ends, unless told otherwise.
const DEFAULT_BUDGET: usize = 80;

/// The agent's one command, parsed.
#[derive(Clone, Debug, PartialEq)]
enum Command {
    Screenshot,
    /// The accessibility tree, only the lines holding this text if given.
    Tree(Option<String>),
    /// What is in the task's folder, as a file manager would show it.
    Files,
    Click {
        at: Pos2,
        modifiers: Modifiers,
    },
    RightClick(Pos2),
    DoubleClick(Pos2),
    Drag {
        path: Vec<Pos2>,
        modifiers: Modifiers,
    },
    Hover(Pos2),
    Key {
        key: Key,
        modifiers: Modifiers,
    },
    Type(String),
    Scroll {
        at: Pos2,
        delta: Vec2,
    },
    /// The agent thinking aloud. Free, and only logged.
    Note(String),
    /// Answers the file picker that opened: the paths, or none to cancel.
    Choose(Vec<PathBuf>),
    /// Listens to what is playing for this long, and describes it in words.
    /// Free: an agent cannot hear, and a person would listen as often as they
    /// liked.
    Listen(Duration),
    Finish,
    GiveUp,
    /// Sent by the launcher when the agent's session ended without `finish`
    /// or `giveup`, so the run still ends and is checked. Not in the prompt.
    Abandon,
}

impl Command {
    /// Whether the command spends the action budget.
    fn is_action(&self) -> bool {
        !matches!(
            self,
            Command::Screenshot
                | Command::Tree(_)
                | Command::Files
                | Command::Note(_)
                | Command::Choose(_)
                | Command::Listen(_)
                | Command::Finish
                | Command::GiveUp
                | Command::Abandon
        )
    }
}

fn number(word: Option<&&str>, what: &str) -> Result<f32, String> {
    word.ok_or_else(|| format!("missing {what}"))?
        .parse::<f32>()
        .ok()
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("{what} must be a number"))
}

fn point(words: &[&str], i: usize) -> Result<Pos2, String> {
    Ok(Pos2::new(
        number(words.get(i), "x")?,
        number(words.get(i + 1), "y")?,
    ))
}

/// `ctrl`, `shift`, `alt`, `cmd`, joined by `+`.
fn modifiers_of(word: &str) -> Result<Modifiers, String> {
    let mut m = Modifiers::NONE;
    for part in word.split('+').filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmd" | "command" => m |= Modifiers::COMMAND,
            "shift" => m |= Modifiers::SHIFT,
            "alt" | "option" => m |= Modifiers::ALT,
            other => return Err(format!("unknown modifier {other}")),
        }
    }
    Ok(m)
}

fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    let words: Vec<&str> = rest.split_whitespace().collect();
    Ok(match verb {
        "screenshot" | "look" => Command::Screenshot,
        "tree" => Command::Tree((!rest.is_empty()).then(|| rest.to_lowercase())),
        "files" => Command::Files,
        "click" => Command::Click {
            at: point(&words, 0)?,
            modifiers: words
                .get(2)
                .map_or(Ok(Modifiers::NONE), |w| modifiers_of(w))?,
        },
        "rclick" => Command::RightClick(point(&words, 0)?),
        "dclick" => Command::DoubleClick(point(&words, 0)?),
        "hover" => Command::Hover(point(&words, 0)?),
        "drag" => {
            // `drag x1 y1 x2 y2 [x3 y3 ...] [modifiers]`
            let (coords, modifiers) = match words.last() {
                Some(last) if last.parse::<f32>().is_err() => {
                    (&words[..words.len() - 1], modifiers_of(last)?)
                }
                _ => (&words[..], Modifiers::NONE),
            };
            if coords.len() < 4 || coords.len() % 2 != 0 {
                return Err("drag needs x1 y1 x2 y2, and more points if you like".into());
            }
            let mut path: Vec<Pos2> = (0..coords.len() / 2)
                .map(|i| point(coords, i * 2))
                .collect::<Result<_, _>>()?;
            if path.len() == 2 {
                // A straight drag moves in steps, as a hand does.
                let (a, b) = (path[0], path[1]);
                path = (0..=6).map(|i| a + (b - a) * (i as f32 / 6.0)).collect();
            }
            Command::Drag { path, modifiers }
        }
        "key" => {
            let (mods, name) = rest.rsplit_once('+').unwrap_or(("", rest));
            // A trailing "+" is the plus key: `+` or `ctrl++`.
            let (mods, name) = if name.is_empty() {
                (mods.strip_suffix('+').unwrap_or(mods), "+")
            } else {
                (mods, name)
            };
            let key = Key::from_name(name)
                .or_else(|| {
                    let mut c = name.chars();
                    let first = c.next()?.to_ascii_uppercase();
                    Key::from_name(&format!("{first}{}", c.as_str().to_ascii_lowercase()))
                })
                .ok_or_else(|| format!("no key called {name}"))?;
            Command::Key {
                key,
                modifiers: modifiers_of(mods)?,
            }
        }
        "type" => {
            if rest.is_empty() {
                return Err("type needs some text".into());
            }
            Command::Type(rest.to_string())
        }
        "scroll" => Command::Scroll {
            at: point(&words, 0)?,
            delta: Vec2::new(number(words.get(2), "dx")?, number(words.get(3), "dy")?),
        },
        "choose" => {
            if rest.is_empty() {
                return Err("choose needs a path (separate several with ;)".into());
            }
            Command::Choose(rest.split(';').map(|p| PathBuf::from(p.trim())).collect())
        }
        "cancel" => Command::Choose(Vec::new()),
        "listen" => {
            let secs = match words.first() {
                None => 2.0,
                Some(w) => w
                    .parse::<f32>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| format!("listen takes seconds, not {w}"))?,
            };
            Command::Listen(Duration::from_secs_f32(secs.clamp(0.5, 5.0)))
        }
        "note" => Command::Note(rest.to_string()),
        "finish" | "done" => Command::Finish,
        "giveup" => Command::GiveUp,
        "abandon" => Command::Abandon,
        "" => return Err("empty command".into()),
        other => return Err(format!("unknown command {other}")),
    })
}

/// The visible parts of the accessibility tree: what a screen reader would
/// say about the window, with each widget's rectangle.
fn tree(h: &AppHarness) -> String {
    let ppp = h.ctx.pixels_per_point();
    let mut seen = std::collections::BTreeSet::new();
    let mut out = String::new();
    for node in h.root().children_recursive() {
        let ak = node.accesskit_node();
        let label = ak.label().unwrap_or_default();
        let value = ak.value().unwrap_or_default();
        if label.is_empty() && value.is_empty() {
            continue;
        }
        let Some(b) = ak.bounding_box() else { continue };
        let line = format!(
            "{:?} \"{}\"{} [{:.0},{:.0} to {:.0},{:.0}]",
            ak.role(),
            label,
            if value.is_empty() {
                String::new()
            } else {
                format!(" = \"{value}\"")
            },
            b.x0 as f32 / ppp,
            b.y0 as f32 / ppp,
            b.x1 as f32 / ppp,
            b.y1 as f32 / ppp,
        );
        if seen.insert(line.clone()) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn screenshot(h: &mut AppHarness, path: &Path) -> Result<(), String> {
    h.render()?
        .save(path)
        .map_err(|e| format!("could not save the screenshot: {e}"))
}

fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

struct Run {
    /// What the agent can see: the screens, its files, the command files.
    dir: PathBuf,
    /// What it cannot: the log, the result, the app's own files.
    private: PathBuf,
    start: Instant,
    actions: usize,
    budget: usize,
    /// Where the pointer last was, for `type` and the log.
    pointer: Pos2,
    shots: usize,
    /// The number of the next command to read. Shared with the file picker's
    /// handler, which reads the commands while a picker is open.
    next: Rc<Cell<usize>>,
    /// The action count, for the picker's handler to log under.
    at_action: Rc<Cell<usize>>,
}

fn write_response(dir: &Path, n: usize, text: &str) {
    let tmp = dir.join(format!("resp.{n}.tmp"));
    std::fs::write(&tmp, format!("{text}\n")).expect("the response is written");
    std::fs::rename(&tmp, dir.join(format!("resp.{n}"))).expect("the response is shown");
}

fn append_log(private: &Path, start: Instant, action: usize, line: &str, result: &str, hit: &str) {
    let entry = format!(
        "{{\"t\":{:.1},\"action\":{},\"command\":\"{}\",\"result\":\"{}\"{}}}\n",
        start.elapsed().as_secs_f64(),
        action,
        escape(line),
        escape(result),
        hit
    );
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(private.join("log.jsonl"))
        .expect("the log opens")
        .write_all(entry.as_bytes())
        .expect("the log is written");
}

impl Run {
    fn write_response(&self, n: usize, text: &str) {
        write_response(&self.dir, n, text);
    }

    fn log(&self, line: &str, result: &str, rec: &testkit::Recording) {
        let hit = rec
            .log()
            .last()
            .map(|a| {
                format!(
                    ",\"widget\":\"{}\",\"role\":\"{}\",\"view\":\"{}\"",
                    escape(
                        &a.start_hit
                            .as_ref()
                            .map(|h| h.label.clone())
                            .unwrap_or_default()
                    ),
                    escape(
                        &a.start_hit
                            .as_ref()
                            .map(|h| h.role.clone())
                            .unwrap_or_default()
                    ),
                    escape(&a.start_view),
                )
            })
            .unwrap_or_default();
        append_log(&self.private, self.start, self.actions, line, result, &hit);
    }

    fn shot(&mut self, h: &mut AppHarness) -> String {
        self.shots += 1;
        let path = self
            .dir
            .join("screens")
            .join(format!("{:03}.png", self.shots));
        match screenshot(h, &path) {
            Ok(()) => format!("screenshot: {}", path.display()),
            Err(why) => format!("screenshot failed: {why}"),
        }
    }
}

/// The app only advances when it is stepped. Without this, work on another
/// thread (an export, a render) never reaches the screen while the agent
/// merely watches, and a finished export looks stuck at 0%.
fn watch(h: &mut AppHarness) {
    h.run_steps(2);
}

/// Carries out one command and says what came of it.
fn execute(h: &mut AppHarness, run: &mut Run, command: Command) -> String {
    if matches!(command, Command::Screenshot | Command::Tree(_)) {
        watch(h);
    }
    match command {
        Command::Screenshot => run.shot(h),
        Command::Tree(filter) => {
            let all = tree(h);
            match filter {
                None => all,
                Some(text) => {
                    let kept: Vec<&str> = all
                        .lines()
                        .filter(|l| l.to_lowercase().contains(&text))
                        .collect();
                    if kept.is_empty() {
                        format!("no line of the tree holds \"{text}\"")
                    } else {
                        kept.join("\n")
                    }
                }
            }
        }
        Command::Files => {
            let world = run.dir.join("world");
            format!("{}: {}", world.display(), listing(&world))
        }
        Command::Note(_) => "noted".into(),
        Command::Listen(time) => listen(h, time),
        Command::Finish | Command::GiveUp | Command::Abandon => "ok".into(),
        // A choose or cancel is read by the picker's handler while a picker is
        // open. Arriving here, none is.
        Command::Choose(_) => "no file picker is open".into(),
        action => {
            run.actions += 1;
            run.at_action.set(run.actions);
            match action {
                Command::Click { at, modifiers } => {
                    run.pointer = at;
                    testkit::click(h, at, modifiers);
                }
                Command::RightClick(at) => {
                    run.pointer = at;
                    testkit::click_with(h, egui::PointerButton::Secondary, Modifiers::NONE, at);
                }
                Command::DoubleClick(at) => {
                    run.pointer = at;
                    testkit::double_click(h, at);
                }
                Command::Drag { path, modifiers } => {
                    run.pointer = *path.last().expect("a path has points");
                    testkit::drag(h, modifiers, &path);
                }
                Command::Hover(at) => {
                    run.pointer = at;
                    h.hover_at(at);
                    // A tooltip shows 0.5 s after the pointer stops; a pass is
                    // 0.05 s of frame time, so step well past that.
                    h.run_steps(16);
                }
                Command::Key { key, modifiers } => testkit::press(h, run.pointer, modifiers, key),
                Command::Type(text) => testkit::type_text(h, run.pointer, &text),
                Command::Scroll { at, delta } => {
                    run.pointer = at;
                    testkit::wheel(h, at, MouseWheelUnit::Point, delta);
                    h.run_steps(2);
                }
                _ => unreachable!("handled above"),
            }
            let shot = run.shot(h);
            format!("action {} of {} done. {shot}", run.actions, run.budget)
        }
    }
}

/// Listens to the output for `time` and says what it heard. Only what is
/// playing can be heard, so a stopped transport says so.
fn listen(h: &mut AppHarness, time: Duration) -> String {
    let (rate, channels) = {
        let config = h.state().session().audio_config();
        (
            config.sample_rate.unwrap_or(48_000),
            usize::from(config.channels.unwrap_or(2)),
        )
    };
    let mut probe = Probe::new(h);
    if !probe.transport_running() {
        return "Nothing is playing: the transport is stopped. Start it (Space, or the play \
                button), then listen again."
            .into();
    }
    let heard = probe.hear(time);
    describe(&heard, channels, rate)
}

/// Describes interleaved samples in words, the way someone listening might:
/// how loud, whether it is steady, how bright, and the note if there is one.
/// A stand-in for hearing, not hearing itself.
fn describe(samples: &[f32], channels: usize, rate: u32) -> String {
    let channels = channels.max(1);
    let mono: Vec<f32> = samples
        .chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect();
    if mono.is_empty() {
        return "Heard nothing: the audio engine is not running.".into();
    }
    let db = |x: f32| {
        if x <= 1e-6 {
            "silent".to_string()
        } else {
            format!("{:.0} dB", 20.0 * x.log10())
        }
    };
    let rms = |part: &[f32]| (part.iter().map(|x| x * x).sum::<f32>() / part.len() as f32).sqrt();
    let peak = samples.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
    if peak < 1e-4 {
        return format!(
            "Heard {:.1} s of silence. Nothing is reaching the output: check that something \
             makes sound, is wired to an Output, and is not muted.",
            mono.len() as f32 / rate as f32
        );
    }
    let quarter = (mono.len() / 4).max(1);
    let levels: Vec<String> = mono.chunks(quarter).take(4).map(|c| db(rms(c))).collect();
    let bright: Vec<String> = mono
        .chunks(quarter)
        .take(4)
        .map(|c| {
            let crossings = c
                .windows(2)
                .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
                .count();
            let hz = crossings as f32 * rate as f32 / (2.0 * c.len() as f32);
            format!("{hz:.0} Hz")
        })
        .collect();
    let clipping = if peak > 0.999 {
        " It reaches full scale, so it may be clipping."
    } else {
        ""
    };
    let pitch = match pitch_of(&mono, rate) {
        Some(hz) => format!(" A steady pitch of about {hz:.0} Hz ({}).", note_name(hz)),
        None => " No single steady pitch.".into(),
    };
    format!(
        "Heard {:.1} s. Loudness (RMS) in four equal parts: {}. Peak {}.{clipping} \
         Brightness (dominant frequency by zero-crossings) in the same four parts: {}.{pitch} \
         This is a measurement, not hearing: it cannot tell you whether it sounds good.",
        mono.len() as f32 / rate as f32,
        levels.join(", "),
        db(peak),
        bright.join(", "),
    )
}

/// The fundamental of a steady tone between 50 Hz and 1 kHz, by
/// autocorrelation over a loud stretch, or `None` if it is not clearly
/// periodic.
fn pitch_of(mono: &[f32], rate: u32) -> Option<f32> {
    let window = 4096.min(mono.len());
    // Lags below a period of 1 kHz would also fit a tone an octave up.
    let lags = (rate as usize / 1000)..(rate as usize / 50).min(window / 2);
    if lags.is_empty() {
        return None;
    }
    // The loudest window, so a tail of silence does not hide a note.
    let start = (0..=mono.len() - window)
        .step_by((window / 2).max(1))
        .max_by(|&a, &b| {
            let e = |i: usize| mono[i..i + window].iter().map(|x| x * x).sum::<f32>();
            e(a).total_cmp(&e(b))
        })?;
    let x = &mono[start..start + window];
    let energy: f32 = x.iter().map(|v| v * v).sum();
    if energy < 1e-6 {
        return None;
    }
    let corr = |lag: usize| {
        let n = x.len() - lag;
        x[..n]
            .iter()
            .zip(&x[lag..])
            .map(|(a, b)| a * b)
            .sum::<f32>()
            / energy
    };
    let (lag, best) = lags
        .map(|l| (l, corr(l)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    (best > 0.8).then(|| rate as f32 / lag as f32)
}

/// The nearest note, such as "A4 +3 cents".
fn note_name(hz: f32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    let near = midi.round();
    let cents = ((midi - near) * 100.0).round();
    let n = near as i32;
    format!(
        "{}{} {:+} cents",
        NAMES[n.rem_euclid(12) as usize],
        n.div_euclid(12) - 1,
        cents
    )
}

/// Makes every file picker the app opens wait for the agent. The action that
/// opened it is answered at once with what the picker is (the screen cannot
/// show a native dialog), then the next commands are read here: `choose` or
/// `cancel` answers it, and the action carries on with the answer, as it does
/// when a person picks in a dialog. Anything else is told to answer first.
fn install_picker(run: &Run) {
    let (dir, private, start) = (run.dir.clone(), run.private.clone(), run.start);
    let (next, at_action, budget) = (run.next.clone(), run.at_action.clone(), run.budget);
    crate::picker::set_handler(Box::new(move |what: &str| {
        let asked = next.get() - 1;
        let world = dir.join("world");
        let prompt = format!(
            "action {} of {budget} done, and a file picker opened ({what}). It is a native \
             dialog that screenshots do not show, and it starts in {}, which holds: {}. \
             Answer it by typing the full path yourself: `choose /full/path` (several files: \
             separate with ;), or `cancel` to close it.",
            at_action.get(),
            world.display(),
            listing(&world)
        );
        write_response(&dir, asked, &prompt);
        loop {
            let n = next.get();
            next.set(n + 1);
            let line = wait_for(&dir.join(format!("cmd.{n}")));
            let line = line.trim().to_string();
            match parse(&line) {
                Ok(Command::Choose(paths)) => {
                    append_log(
                        &private,
                        start,
                        at_action.get(),
                        &line,
                        "answered the picker",
                        "",
                    );
                    return paths;
                }
                other => {
                    let text = match other {
                        Ok(Command::Note(_)) => "noted".to_string(),
                        _ => "A file picker is open and nothing else works until it is \
                              answered: `choose /full/path` or `cancel`."
                            .to_string(),
                    };
                    append_log(&private, start, at_action.get(), &line, &text, "");
                    write_response(&dir, n, &text);
                }
            }
        }
    }));
}

/// The names in a folder, as a file dialog would list them, with what is in
/// each folder inside it (one level), as a person browsing would see.
fn listing(dir: &Path) -> String {
    fn names(dir: &Path) -> Vec<(String, bool)> {
        let mut names: Vec<(String, bool)> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().into_owned(),
                            e.path().is_dir(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
    names(dir)
        .into_iter()
        .map(|(name, is_dir)| {
            if is_dir {
                let inside: Vec<String> = names(&dir.join(&name))
                    .into_iter()
                    .map(|(n, d)| if d { format!("{n}/") } else { n })
                    .collect();
                format!("{name}/ (holds: {})", inside.join(", "))
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn wait_for(path: &Path) -> String {
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn route_for(story: &str) -> Route {
    routes::all()
        .into_iter()
        .filter(|r| r.story == story)
        .min_by_key(|r| u8::from(r.tag != super::Tag::Default))
        .unwrap_or_else(|| panic!("no route for story {story}"))
}

/// The size the done check is asked about: `NOODLE_AGENT_SIZE`, else the
/// route's own. A route that scales sets some of its fixture aside to make
/// the size, which an agent's run does not, so its size is what the fixture
/// holds.
fn size_of(route: &Route) -> usize {
    std::env::var("NOODLE_AGENT_SIZE")
        .ok()
        .and_then(|n| n.parse().ok())
        .or(route.size)
        .unwrap_or(1)
}

fn end(run: &Run, route: &Route, h: &mut AppHarness, how: &str) {
    let n = size_of(route);
    // Stop the transport first: the done check looks at the result.
    h.state_mut().session_mut().set_transport_running(false);
    h.run_steps(3);
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (route.done)(&mut Probe::new(h), n)
    }));
    let (passed, why) = match checked {
        Ok(Ok(())) => (true, String::new()),
        Ok(Err(why)) => (false, why),
        Err(payload) => (false, super::panic_message(payload)),
    };
    let result = format!(
        "{{\"story\":\"{}\",\"ended\":\"{}\",\"passed\":{},\"why_not\":\"{}\",\"actions\":{},\"budget\":{},\"seconds\":{:.1}}}\n",
        route.story,
        how,
        passed,
        escape(&why),
        run.actions,
        run.budget,
        run.start.elapsed().as_secs_f64()
    );
    std::fs::write(run.private.join("result.json"), result).expect("the result is written");
    // The client stops sending when this appears. It says nothing of the result.
    std::fs::write(run.dir.join("over"), "").expect("the end is marked");
}

/// Serves one run. Ignored: it needs a person or an agent to drive it.
#[test]
#[ignore = "serves an agent session; run by scripts/new_user/start.sh"]
fn drive() {
    let story = std::env::var("NOODLE_AGENT_STORY").expect("NOODLE_AGENT_STORY, such as S12");
    let dir = PathBuf::from(std::env::var("NOODLE_AGENT_DIR").expect("NOODLE_AGENT_DIR"));
    let budget = std::env::var("NOODLE_AGENT_BUDGET")
        .ok()
        .and_then(|b| b.parse().ok())
        .unwrap_or(DEFAULT_BUDGET);
    let private = PathBuf::from(
        std::env::var("NOODLE_AGENT_PRIVATE")
            .expect("NOODLE_AGENT_PRIVATE, outside NOODLE_AGENT_DIR"),
    );
    let world = dir.join("world");
    std::fs::create_dir_all(dir.join("screens")).expect("a place for screenshots");
    std::fs::create_dir_all(&world).expect("a place for the user's files");
    std::fs::create_dir_all(&private).expect("a place for the app's own files");
    crate::prefs::init(private.join("prefs.ron"));

    let route = route_for(&story);
    let mut h = window_with((route.fixture)(&world), true);
    let rec = testkit::record();
    let mut run = Run {
        dir: dir.clone(),
        private,
        start: Instant::now(),
        actions: 0,
        budget,
        pointer: Pos2::new(640.0, 400.0),
        shots: 0,
        next: Rc::new(Cell::new(1)),
        at_action: Rc::new(Cell::new(0)),
    };
    install_picker(&run);
    let first = run.shot(&mut h);
    std::fs::write(dir.join("ready"), first).expect("ready is written");

    let how = loop {
        let n = run.next.get();
        run.next.set(n + 1);
        let line = wait_for(&dir.join(format!("cmd.{n}")));
        let line = line.trim().to_string();
        let command = match parse(&line) {
            Ok(c) => c,
            Err(why) => {
                let text = format!("could not understand that: {why}");
                run.log(&line, &text, &rec);
                run.write_response(n, &text);
                continue;
            }
        };
        if command.is_action() && run.actions >= run.budget {
            let text = "out of actions: the session is over".to_string();
            run.log(&line, &text, &rec);
            run.write_response(n, &text);
            break "budget";
        }
        let finishing = match command {
            Command::Finish => Some("finished"),
            Command::GiveUp => Some("gave up"),
            Command::Abandon => Some("walked away"),
            _ => None,
        };
        let result = execute(&mut h, &mut run, command);
        run.log(&line, &result, &rec);
        // A picker opened by the action read the commands that answered it, so
        // the result answers the last of them.
        run.write_response(run.next.get() - 1, &result);
        if let Some(how) = finishing {
            break how;
        }
    };
    end(&run, &route, &mut h, how);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(48_000.0 * secs) as usize)
            .flat_map(|i| {
                let v = amp * (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn listening_finds_the_note_and_says_when_it_is_silent() {
        let text = describe(&sine(440.0, 1.0, 0.5), 2, 48_000);
        assert!(text.contains("A4"), "{text}");
        assert!(text.contains("about 440 Hz"), "{text}");
        let text = describe(&vec![0.0; 96_000], 2, 48_000);
        assert!(text.contains("silence"), "{text}");
        let mut x = 0x1234_5678u32;
        let noise: Vec<f32> = (0..96_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x % 2000) as f32 / 1000.0 - 1.0
            })
            .collect();
        assert!(describe(&noise, 2, 48_000).contains("No single steady pitch"));
    }

    #[test]
    fn watching_lets_a_background_export_finish() {
        let dir = tempfile::tempdir().unwrap();
        crate::prefs::init(dir.path().join("prefs.ron"));
        let route = route_for("S04");
        let mut h = window_with((route.fixture)(dir.path()), true);
        let out = dir.path().join("out.wav");
        h.state_mut()
            .session_mut()
            .start_export(
                out,
                crate::session::ExportChoice::mixdown(
                    noodle_io::ExportFormat::WavFloat,
                    noodle_engine::ChannelLayout::Stereo,
                    None,
                ),
            )
            .unwrap();
        let until = Instant::now() + std::time::Duration::from_secs(60);
        while h.state().session().export_progress().is_some() {
            assert!(Instant::now() < until, "the export never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
            watch(&mut h);
        }
    }

    #[test]
    fn numbers_must_be_finite_and_ctrl_plus_keeps_its_modifier() {
        assert!(parse("listen nan").is_err());
        assert!(parse("click nan 3").is_err());
        assert!(parse("scroll 1 2 inf 0").is_err());
        assert_eq!(
            parse("key ctrl++").unwrap(),
            Command::Key {
                key: Key::Plus,
                modifiers: Modifiers::COMMAND
            }
        );
    }

    #[test]
    fn listen_is_free_and_clamped() {
        assert!(!parse("listen").unwrap().is_action());
        assert_eq!(
            parse("listen 99").unwrap(),
            Command::Listen(Duration::from_secs(5))
        );
        assert!(parse("listen soon").is_err());
    }

    #[test]
    fn the_listing_shows_what_is_inside_folders() {
        let dir = std::env::temp_dir().join(format!("noodle-listing-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("recordings")).unwrap();
        std::fs::write(dir.join("recordings/a.wav"), "").unwrap();
        std::fs::write(dir.join("project.ron"), "").unwrap();
        let text = listing(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(text, "project.ron, recordings/ (holds: a.wav)");
        assert_eq!(parse("abandon").unwrap(), Command::Abandon);
        assert_eq!(parse("tree").unwrap(), Command::Tree(None));
        assert_eq!(
            parse("tree Formant").unwrap(),
            Command::Tree(Some("formant".into()))
        );
        assert_eq!(parse("files").unwrap(), Command::Files);
        assert!(!Command::Files.is_action());
        assert!(!Command::Abandon.is_action());
    }

    #[test]
    fn commands_parse() {
        assert_eq!(
            parse("click 10 20 shift").unwrap(),
            Command::Click {
                at: Pos2::new(10.0, 20.0),
                modifiers: Modifiers::SHIFT
            }
        );
        assert_eq!(
            parse("key ctrl+shift+a").unwrap(),
            Command::Key {
                key: Key::A,
                modifiers: Modifiers::COMMAND | Modifiers::SHIFT
            }
        );
        assert_eq!(
            parse("key Enter").unwrap(),
            Command::Key {
                key: Key::Enter,
                modifiers: Modifiers::NONE
            }
        );
        assert_eq!(
            parse("type hello world").unwrap(),
            Command::Type("hello world".into())
        );
        assert!(
            matches!(parse("drag 0 0 60 0").unwrap(), Command::Drag { path, .. } if path.len() == 7)
        );
        assert!(
            matches!(parse("drag 0 0 5 5 9 9 alt").unwrap(), Command::Drag { path, modifiers } if path.len() == 3 && modifiers.alt)
        );
        assert_eq!(
            parse("choose /a/b; /c/d.wav").unwrap(),
            Command::Choose(vec!["/a/b".into(), "/c/d.wav".into()])
        );
        assert_eq!(parse("cancel").unwrap(), Command::Choose(Vec::new()));
        assert!(parse("click 10").is_err());
        assert!(parse("fly").is_err());
    }

    #[test]
    fn only_acting_spends_the_budget() {
        assert!(!parse("screenshot").unwrap().is_action());
        assert!(!parse("note stuck").unwrap().is_action());
        assert!(parse("click 1 2").unwrap().is_action());
        assert!(parse("key a").unwrap().is_action());
    }
}
