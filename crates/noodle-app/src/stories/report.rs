//! The scorecard: a JSON file per route and a Markdown table across stories
//! (the layout in `docs/architecture/usability.md`, section 4).
//!
//! Columns for metric groups that are not built yet show a dash. The repetition
//! column shows the per-item cost only (the repetition ratio comes later).
//! The vocabulary report (group E) is a table of its own, written beside the
//! scorecard.

use std::fmt::Write;
use std::path::Path;

use super::effort::Effort;
use super::feedback::{Feedback, Hearing};
use super::registry::STORIES;
use super::repetition::PerItem;
use super::rework::{Rework, RoundTrip};
use super::robustness::Robustness;
use super::surface::{Surface, Surfaces};
use super::vocabulary::{self, Row};

/// One route's result.
pub struct RouteScore {
    pub story: String,
    pub tag: String,
    pub name: String,
    pub size: Option<usize>,
    pub passed: bool,
    pub effort: Effort,
    /// What each extra item costs, for a route that scales.
    pub per_item: Option<PerItem>,
    pub rework: Rework,
    pub round_trip: RoundTrip,
    /// Late changes run from this route's end state.
    pub late: Vec<LateScore>,
    /// How each action's entry point shows itself (group E).
    pub surfaces: Surfaces,
    /// Time to hear, live editing and state visibility (group F).
    pub feedback: Feedback,
    /// Dropouts, and save and reopen (group G).
    pub robustness: Robustness,
}

pub struct LateScore {
    pub name: String,
    pub passed: bool,
    pub effort: Effort,
    /// The late change's actions over the route's, or `None` for a route
    /// with no actions.
    pub cost: Option<f64>,
    /// The late change's KLM seconds over the route's.
    pub klm_ratio: Option<f64>,
    /// The parts of the state it had to leave alone and did not.
    pub lost: Vec<String>,
}

fn esc(s: &str) -> String {
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

fn effort_json(e: &Effort) -> String {
    let c = &e.counts;
    let o = &e.operators;
    format!(
        "{{\"actions\":{},\"clicks\":{},\"double_clicks\":{},\"drags\":{},\"keys\":{},\
         \"wheels\":{},\"drops\":{},\"texts\":{},\"controls\":{},\"code\":{},\"code_statements\":{},\"steps\":{},\"view_switches\":{},\
         \"dialogs_opened\":{},\"klm\":{{\"k\":{},\"p\":{},\"b\":{},\"h\":{},\"m\":{},\
         \"seconds\":{:.2}}},\"wait_seconds\":{:.2}}}",
        e.actions,
        c.clicks,
        c.double_clicks,
        c.drags,
        c.keys,
        c.wheels,
        c.drops,
        c.texts,
        c.controls,
        c.code,
        e.code_statements,
        e.steps,
        e.view_switches,
        e.dialogs_opened,
        o.k,
        o.p,
        o.b,
        o.h,
        o.m,
        e.klm_seconds,
        e.wait_seconds,
    )
}

fn opt(v: Option<f64>) -> String {
    v.map_or("null".to_string(), |c| format!("{c:.3}"))
}

fn per_item_json(p: &Option<PerItem>) -> String {
    match p {
        Some(p) => format!(
            "{{\"small\":{},\"large\":{},\"actions\":{:.3},\"klm_seconds\":{:.3}}}",
            p.small, p.large, p.actions, p.klm_seconds
        ),
        None => "null".into(),
    }
}

fn rework_json(r: &Rework, t: &RoundTrip) -> String {
    let detected: Vec<String> = r
        .detected
        .iter()
        .map(|k| format!("\"{}\"", esc(k)))
        .collect();
    format!(
        "{{\"marked\":{},\"marked_actions\":{},\"detected\":[{}],\"undos\":{},\
         \"round_trip\":{{\"undo_steps\":{},\"redo_steps\":{},\"restores_start\":{},\
         \"restores_end\":{}}}}}",
        r.marked,
        r.marked_actions,
        detected.join(","),
        r.undos,
        t.undo_steps,
        t.redo_steps,
        t.restores_start,
        t.restores_end
    )
}

fn strings(items: &[String]) -> String {
    items
        .iter()
        .map(|k| format!("\"{}\"", esc(k)))
        .collect::<Vec<_>>()
        .join(",")
}

fn surfaces_json(s: &Surfaces) -> String {
    let counts: Vec<String> = Surface::ALL
        .iter()
        .map(|&x| format!("\"{}\":{}", x.name(), s.count(x)))
        .collect();
    format!(
        "{{\"total\":{},\"hidden\":{},\"hidden_share\":{:.3},\"counts\":{{{}}},\
         \"hidden_entry_points\":[{}],\"untagged\":[{}]}}",
        s.total,
        s.hidden_count(),
        s.hidden_share(),
        counts.join(","),
        strings(&s.hidden),
        strings(&s.untagged)
    )
}

fn hearing_json(h: &Hearing) -> String {
    match h {
        Hearing::NotListened => "{\"listened\":false}".into(),
        Hearing::Silent => "{\"listened\":true,\"heard\":false}".into(),
        Hearing::At {
            actions,
            klm_seconds,
            peak,
        } => format!(
            "{{\"listened\":true,\"heard\":true,\"actions\":{actions},\
             \"klm_seconds\":{klm_seconds:.2},\"peak\":{peak:.3}}}"
        ),
    }
}

fn feedback_json(f: &Feedback) -> String {
    format!(
        "{{\"time_to_hear\":{},\"live\":{{\"works\":{},\"stops\":[{}],\"failed\":{}}},\
         \"unseen\":[{}],\"steps\":{}}}",
        hearing_json(&f.hearing),
        f.live.ok(),
        strings(&f.live.stops),
        f.live
            .failed
            .as_ref()
            .map_or("null".to_string(), |w| format!("\"{}\"", esc(w))),
        strings(&f.unseen),
        f.steps
    )
}

fn robustness_json(r: &Robustness) -> String {
    format!(
        "{{\"underruns\":{},\"reopen\":{{\"ok\":{},\"detail\":\"{}\"}}}}",
        r.underruns.map_or("null".to_string(), |n| n.to_string()),
        r.reopen.ok(),
        esc(&r.reopen.summary())
    )
}

/// The JSON for one route.
pub fn route_json(r: &RouteScore) -> String {
    let size = r.size.map_or("null".to_string(), |s| s.to_string());
    let late: Vec<String> = r
        .late
        .iter()
        .map(|l| {
            format!(
                "{{\"name\":\"{}\",\"passed\":{},\"cost\":{},\"klm_ratio\":{},\
                 \"lost\":[{}],\"effort\":{}}}",
                esc(&l.name),
                l.passed,
                opt(l.cost),
                opt(l.klm_ratio),
                l.lost
                    .iter()
                    .map(|k| format!("\"{}\"", esc(k)))
                    .collect::<Vec<_>>()
                    .join(","),
                effort_json(&l.effort)
            )
        })
        .collect();
    format!(
        "{{\"story\":\"{}\",\"tag\":\"{}\",\"name\":\"{}\",\"size\":{},\"passed\":{},\
         \"effort\":{},\"per_item\":{},\"rework\":{},\"late_changes\":[{}],\"surfaces\":{},\
         \"feedback\":{},\"robustness\":{}}}",
        esc(&r.story),
        esc(&r.tag),
        esc(&r.name),
        size,
        r.passed,
        effort_json(&r.effort),
        per_item_json(&r.per_item),
        rework_json(&r.rework, &r.round_trip),
        late.join(","),
        surfaces_json(&r.surfaces),
        feedback_json(&r.feedback),
        robustness_json(&r.robustness)
    )
}

/// One row per story, from the registry, then any routes that belong to no
/// story (the framework's own check).
pub fn markdown(routes: &[RouteScore]) -> String {
    let mut out = String::new();
    out.push_str(
        "| Story | Status | Route | Actions | KLM s | Per extra item | Repetition | Rework \
         | Code | Worst change cost | Loss | Hidden entry points | Time to hear | Live editing \
         | Unseen | Dropouts | Save and reopen | Naive success |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    let mut row = |id: &str, title: &str, status: &str, mine: Vec<&RouteScore>| {
        let tags: Vec<&str> = mine.iter().map(|r| r.tag.as_str()).collect();
        let pick = mine.iter().find(|r| r.tag == "default").or(mine.first());
        let (actions, klm) = match pick {
            Some(r) => (
                r.effort.actions.to_string(),
                format!("{:.1}", r.effort.klm_seconds),
            ),
            None => ("-".into(), "-".into()),
        };
        let worst = mine
            .iter()
            .flat_map(|r| r.late.iter().filter_map(|l| l.cost))
            .fold(None, |a: Option<f64>, c| Some(a.map_or(c, |a| a.max(c))))
            .map_or("-".to_string(), |c| format!("{c:.2}"));
        let per_item = pick
            .and_then(|r| r.per_item.as_ref())
            .map_or("-".to_string(), |p| {
                format!("{:.1} act / {:.1} s", p.actions, p.klm_seconds)
            });
        let rework = pick.map_or("-".to_string(), |r| {
            let w = &r.rework;
            let mut text = format!(
                "{} recreate ({} act), {} undo",
                w.marked + w.detected.len(),
                w.marked_actions,
                w.undos
            );
            if !r.round_trip.ok() {
                text.push_str(", UNDO ROUND TRIP FAILS");
            }
            text
        });
        // Code written anywhere in the story's routes or late changes.
        let statements: usize = mine
            .iter()
            .map(|r| {
                r.effort.code_statements
                    + r.late
                        .iter()
                        .map(|l| l.effort.code_statements)
                        .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
        let code = if statements == 0 {
            "-".to_string()
        } else {
            format!("{statements} statements")
        };
        let lost: Vec<&str> = mine
            .iter()
            .flat_map(|r| {
                r.late
                    .iter()
                    .flat_map(|l| l.lost.iter().map(String::as_str))
            })
            .collect();
        let loss = if mine.iter().all(|r| r.late.is_empty()) {
            "-".to_string()
        } else if lost.is_empty() {
            "none".to_string()
        } else {
            format!("LOST {}", lost.join(", "))
        };
        let hidden = pick.map_or("-".to_string(), |r| {
            let s = &r.surfaces;
            format!(
                "{} of {} ({:.0}%), {} untagged",
                s.hidden_count(),
                s.total,
                s.hidden_share() * 100.0,
                s.count(Surface::Untagged)
            )
        });
        let hear = pick.map_or("-".to_string(), |r| match &r.feedback.hearing {
            Hearing::NotListened => "never listens".to_string(),
            Hearing::Silent => "SILENT".to_string(),
            Hearing::At {
                actions,
                klm_seconds,
                ..
            } => format!("{actions} act / {klm_seconds:.1} s"),
        });
        let live = pick.map_or("-".to_string(), |r| r.feedback.live.summary());
        let unseen = pick.map_or("-".to_string(), |r| {
            if r.feedback.unseen.is_empty() {
                "none".to_string()
            } else {
                format!("{} of {} steps", r.feedback.unseen.len(), r.feedback.steps)
            }
        });
        let dropouts = pick
            .and_then(|r| r.robustness.underruns)
            .map_or("-".to_string(), |n| n.to_string());
        let reopen = pick.map_or("-".to_string(), |r| r.robustness.reopen.summary());
        let route = if tags.is_empty() {
            "-".to_string()
        } else {
            tags.join(" / ")
        };
        let failed = mine.iter().any(|r| !r.passed);
        let status = if failed {
            format!("{status} (route FAILS)")
        } else {
            status.to_string()
        };
        let _ = writeln!(
            out,
            "| {id} {title} | {status} | {route} | {actions} | {klm} | {per_item} | - | {rework} | {code} \
             | {worst} | {loss} \
             | {hidden} | {hear} | {live} | {unseen} | {dropouts} | {reopen} | - |"
        );
    };
    for story in &STORIES {
        let mine: Vec<&RouteScore> = routes.iter().filter(|r| r.story == story.id).collect();
        row(story.id, story.title, story.status.name(), mine);
    }
    let known = |id: &str| STORIES.iter().any(|s| s.id == id);
    let mut others: Vec<&str> = routes
        .iter()
        .map(|r| r.story.as_str())
        .filter(|id| !known(id))
        .collect();
    others.sort_unstable();
    others.dedup();
    for id in others {
        let mine: Vec<&RouteScore> = routes.iter().filter(|r| r.story == id).collect();
        row(id, "(framework check)", "-", mine);
    }
    out
}

/// Writes `scorecard.md` (the table, then the vocabulary report),
/// `vocabulary.json` and `routes/<story>-<tag>-<name>.json` under `dir`.
pub fn write(dir: &Path, routes: &[RouteScore], words: &[Row]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("routes"))?;
    let card = format!(
        "{}\n## Vocabulary\n\nThe names other DAWs give a feature, typed into the add-node \
         search.\n\n{}",
        markdown(routes),
        vocabulary::markdown(words)
    );
    std::fs::write(dir.join("scorecard.md"), card)?;
    std::fs::write(dir.join("vocabulary.json"), vocabulary::json(words))?;
    let all: Vec<String> = routes.iter().map(route_json).collect();
    std::fs::write(
        dir.join("scorecard.json"),
        format!("[{}]\n", all.join(",\n")),
    )?;
    for r in routes {
        let file = format!("{}-{}-{}.json", r.story, r.tag, r.name.replace(' ', "_"));
        std::fs::write(dir.join("routes").join(file), route_json(r) + "\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::feedback::LiveEdit;
    use super::super::robustness::Reopen;
    use super::*;

    fn score(story: &str, tag: &str, actions: usize) -> RouteScore {
        RouteScore {
            story: story.into(),
            tag: tag.into(),
            name: "a \"quoted\" name".into(),
            size: Some(4),
            passed: true,
            effort: Effort {
                actions,
                klm_seconds: 12.34,
                ..Effort::default()
            },
            per_item: Some(PerItem {
                small: 4,
                large: 8,
                actions: 2.5,
                klm_seconds: 3.0,
            }),
            rework: Rework {
                marked: 1,
                marked_actions: 4,
                detected: vec!["sine".into()],
                undos: 2,
            },
            round_trip: RoundTrip {
                undo_steps: 3,
                redo_steps: 3,
                restores_start: true,
                restores_end: true,
            },
            late: vec![LateScore {
                name: "late".into(),
                passed: true,
                effort: Effort::default(),
                cost: Some(0.25),
                klm_ratio: Some(0.5),
                lost: vec!["automation".into()],
            }],
            surfaces: Surfaces {
                counts: [4, 1, 0, 1, 1, 0, 1],
                total: 8,
                untagged: vec!["click on an unlabelled target in the node editor".into()],
                hidden: vec!["Shift+A in the node editor".into()],
            },
            feedback: Feedback {
                hearing: Hearing::At {
                    actions: 5,
                    klm_seconds: 9.5,
                    peak: 0.5,
                },
                unseen: vec!["the lane".into()],
                steps: 4,
                live: LiveEdit {
                    stops: vec!["wire".into()],
                    failed: None,
                },
            },
            robustness: Robustness {
                underruns: Some(2),
                reopen: Reopen::Same,
            },
        }
    }

    #[test]
    fn json_escapes_and_nests() {
        let json = route_json(&score("S12", "default", 7));
        assert!(json.starts_with("{\"story\":\"S12\",\"tag\":\"default\""));
        assert!(json.contains("a \\\"quoted\\\" name"));
        assert!(json.contains("\"size\":4"));
        assert!(json.contains("\"actions\":7"));
        assert!(json.contains("\"seconds\":12.34"));
        assert!(json.contains("\"cost\":0.250"));
        assert!(json.contains("\"klm_ratio\":0.500"));
        assert!(json.contains("\"lost\":[\"automation\"]"));
        assert!(json.contains("\"per_item\":{\"small\":4,\"large\":8,\"actions\":2.500"));
        assert!(json.contains("\"detected\":[\"sine\"]"));
        assert!(json.contains("\"restores_start\":true"));
        assert!(json.contains("\"hidden_share\":0.125"), "{json}");
        assert!(json.contains("\"shortcut only\":1"), "{json}");
        assert!(
            json.contains("\"untagged\":[\"click on an unlabelled"),
            "{json}"
        );
        assert!(json.contains("\"time_to_hear\":{\"listened\":true,\"heard\":true,\"actions\":5"));
        assert!(json.contains("\"live\":{\"works\":false,\"stops\":[\"wire\"],\"failed\":null}"));
        assert!(json.contains("\"unseen\":[\"the lane\"]"));
        assert!(json.contains("\"robustness\":{\"underruns\":2,\"reopen\":{\"ok\":true"));
    }

    #[test]
    fn the_table_has_a_row_per_story_and_fills_in_routed_ones() {
        let md = markdown(&[
            score("S12", "default", 7),
            score("S12", "expert", 3),
            score("S00", "default", 5),
        ]);
        let lines: Vec<&str> = md.lines().collect();
        // Header, rule, 30 stories, the framework row.
        assert_eq!(lines.len(), 2 + 30 + 1);
        let s12 = lines.iter().find(|l| l.contains("| S12 ")).unwrap();
        assert!(s12.contains("default / expert"), "{s12}");
        assert!(s12.contains("| 7 | 12.3 |"), "{s12}");
        assert!(s12.contains("| 0.25 |"), "{s12}");
        assert!(s12.contains("2.5 act / 3.0 s"), "{s12}");
        assert!(s12.contains("2 recreate (4 act), 2 undo"), "{s12}");
        assert!(s12.contains("LOST automation"), "{s12}");
        assert!(s12.contains("| - | 0.25 |"), "no code: {s12}");
        // Group E, F and G.
        assert!(s12.contains("| 1 of 8 (12%), 1 untagged |"), "{s12}");
        assert!(s12.contains("| 5 act / 9.5 s |"), "{s12}");
        assert!(s12.contains("| stops in wire |"), "{s12}");
        assert!(s12.contains("| 1 of 4 steps | 2 | identical |"), "{s12}");
        let s02 = lines.iter().find(|l| l.contains("| S02 ")).unwrap();
        assert!(s02.contains("| blocked | - | - | - |"), "{s02}");
        assert!(lines.last().unwrap().contains("S00 (framework check)"));
    }

    #[test]
    fn code_written_in_a_late_change_shows_in_the_table() {
        let mut route = score("S09", "default", 7);
        route.late[0].effort.code_statements = 6;
        let md = markdown(&[route]);
        let s09 = md.lines().find(|l| l.contains("| S09 ")).unwrap();
        assert!(s09.contains("| 6 statements |"), "{s09}");
    }
}
