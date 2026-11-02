//! The scorecard: a JSON file per route and a Markdown table across stories
//! (the layout in `docs/architecture/usability.md`, section 4).
//!
//! Columns for metric groups that are not built yet show a dash.

use std::fmt::Write;
use std::path::Path;

use super::effort::Effort;
use super::registry::STORIES;

/// One route's result.
pub struct RouteScore {
    pub story: String,
    pub tag: String,
    pub name: String,
    pub size: Option<usize>,
    pub passed: bool,
    pub effort: Effort,
    /// Late changes run from this route's end state.
    pub late: Vec<LateScore>,
}

pub struct LateScore {
    pub name: String,
    pub passed: bool,
    pub effort: Effort,
    /// The late change's actions over the route's, or `None` for a route
    /// with no actions.
    pub cost: Option<f64>,
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
         \"wheels\":{},\"drops\":{},\"texts\":{},\"steps\":{},\"view_switches\":{},\
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

/// The JSON for one route.
pub fn route_json(r: &RouteScore) -> String {
    let size = r.size.map_or("null".to_string(), |s| s.to_string());
    let late: Vec<String> = r
        .late
        .iter()
        .map(|l| {
            format!(
                "{{\"name\":\"{}\",\"passed\":{},\"cost\":{},\"effort\":{}}}",
                esc(&l.name),
                l.passed,
                l.cost.map_or("null".to_string(), |c| format!("{c:.3}")),
                effort_json(&l.effort)
            )
        })
        .collect();
    format!(
        "{{\"story\":\"{}\",\"tag\":\"{}\",\"name\":\"{}\",\"size\":{},\"passed\":{},\
         \"effort\":{},\"late_changes\":[{}]}}",
        esc(&r.story),
        esc(&r.tag),
        esc(&r.name),
        size,
        r.passed,
        effort_json(&r.effort),
        late.join(",")
    )
}

/// One row per story, from the registry, then any routes that belong to no
/// story (the framework's own check).
pub fn markdown(routes: &[RouteScore]) -> String {
    let mut out = String::new();
    out.push_str(
        "| Story | Status | Route | Actions | KLM s | Per extra item | Repetition | Rework \
         | Worst change cost | Loss | Hidden entry points | Naive success |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|\n");
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
            "| {id} {title} | {status} | {route} | {actions} | {klm} | - | - | - | {worst} | - | - | - |"
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

/// Writes `scorecard.md` and `routes/<story>-<tag>-<name>.json` under `dir`.
pub fn write(dir: &Path, routes: &[RouteScore]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("routes"))?;
    std::fs::write(dir.join("scorecard.md"), markdown(routes))?;
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
            late: vec![LateScore {
                name: "late".into(),
                passed: true,
                effort: Effort::default(),
                cost: Some(0.25),
            }],
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
        let s01 = lines.iter().find(|l| l.contains("| S01 ")).unwrap();
        assert!(s01.contains("| blocked | - | - | - |"), "{s01}");
        assert!(lines.last().unwrap().contains("S00 (framework check)"));
    }
}
