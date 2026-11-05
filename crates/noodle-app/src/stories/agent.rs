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
//! Files in `NOODLE_AGENT_DIR`:
//! - `cmd.N` (written by the client) and `resp.N` (written here): one command
//!   and its answer, numbered from 1.
//! - `ready`: written when the app is up.
//! - `world/`: the fixture's files, where the user's samples and so on are.
//! - `screens/NNN.png`: the screen after each action.
//! - `log.jsonl`: every command with its result, the widget and view it hit,
//!   and the seconds since the start.
//! - `result.json`: written when the run ends: whether the story's done check
//!   passed, and the action count.
//!
//! The agent is never told whether the done check passed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Key, Modifiers, MouseWheelUnit, Pos2, Vec2};
use egui_kittest::kittest::NodeT;

use super::handle::{AppHarness, Probe};
use super::{Route, routes, window_with};
use crate::testkit;

/// How many actions an agent gets before the run ends, unless told otherwise.
const DEFAULT_BUDGET: usize = 80;

/// The agent's one command, parsed.
#[derive(Debug, PartialEq)]
enum Command {
    Screenshot,
    Tree,
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
    Finish,
    GiveUp,
}

impl Command {
    /// Whether the command spends the action budget.
    fn is_action(&self) -> bool {
        !matches!(
            self,
            Command::Screenshot
                | Command::Tree
                | Command::Note(_)
                | Command::Finish
                | Command::GiveUp
        )
    }
}

fn number(word: Option<&&str>, what: &str) -> Result<f32, String> {
    word.ok_or_else(|| format!("missing {what}"))?
        .parse::<f32>()
        .map_err(|_| format!("{what} must be a number"))
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
        "tree" => Command::Tree,
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
            // A bare "+" is the plus key.
            let (mods, name) = if name.is_empty() {
                ("", "+")
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
        "note" => Command::Note(rest.to_string()),
        "finish" | "done" => Command::Finish,
        "giveup" => Command::GiveUp,
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
    dir: PathBuf,
    start: Instant,
    actions: usize,
    budget: usize,
    /// Where the pointer last was, for `type` and the log.
    pointer: Pos2,
    shots: usize,
    next: usize,
}

impl Run {
    fn write_response(&self, n: usize, text: &str) {
        let tmp = self.dir.join(format!("resp.{n}.tmp"));
        std::fs::write(&tmp, format!("{text}\n")).expect("the response is written");
        std::fs::rename(&tmp, self.dir.join(format!("resp.{n}"))).expect("the response is shown");
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
        let entry = format!(
            "{{\"t\":{:.1},\"action\":{},\"command\":\"{}\",\"result\":\"{}\"{}}}\n",
            self.start.elapsed().as_secs_f64(),
            self.actions,
            escape(line),
            escape(result),
            hit
        );
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("log.jsonl"))
            .expect("the log opens")
            .write_all(entry.as_bytes())
            .expect("the log is written");
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

/// Carries out one command and says what came of it.
fn execute(h: &mut AppHarness, run: &mut Run, command: Command) -> String {
    match command {
        Command::Screenshot => run.shot(h),
        Command::Tree => tree(h),
        Command::Note(_) => "noted".into(),
        Command::Finish | Command::GiveUp => "ok".into(),
        action => {
            run.actions += 1;
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
                    h.run_steps(3);
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

fn end(run: &Run, route: &Route, h: &mut AppHarness, how: &str) {
    let n = route.size.unwrap_or(1);
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
    std::fs::write(run.dir.join("result.json"), result).expect("the result is written");
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
    let world = dir.join("world");
    std::fs::create_dir_all(dir.join("screens")).expect("a place for screenshots");
    std::fs::create_dir_all(&world).expect("a place for the user's files");
    std::fs::create_dir_all(dir.join("internal")).expect("a place for the app's own files");
    crate::prefs::init(dir.join("internal").join("prefs.ron"));

    let route = route_for(&story);
    let mut h = window_with((route.fixture)(&world), true);
    let rec = testkit::record();
    let mut run = Run {
        dir: dir.clone(),
        start: Instant::now(),
        actions: 0,
        budget,
        pointer: Pos2::new(640.0, 400.0),
        shots: 0,
        next: 1,
    };
    let first = run.shot(&mut h);
    std::fs::write(dir.join("ready"), first).expect("ready is written");

    let how = loop {
        let n = run.next;
        run.next += 1;
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
            _ => None,
        };
        let result = execute(&mut h, &mut run, command);
        run.log(&line, &result, &rec);
        run.write_response(n, &result);
        if let Some(how) = finishing {
            break how;
        }
    };
    end(&run, &route, &mut h, how);
}

#[cfg(test)]
mod tests {
    use super::*;

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
