//! Group E of the scorecard, part two: does searching for a feature by the
//! name other DAWs give it find it?
//!
//! Each node term is typed into the add-node search (`editor::search::entries`,
//! the same function the search box calls) and the top three results are
//! compared with what the term should find. Features that are not nodes
//! (bounce, freeze, comp, punch, markers...) are typed into the command
//! palette's search (`app::commands::search`) and must find the palette
//! command that runs the feature or says where it lives. A feature with
//! neither is reported as not searchable.
//!
//! The terms are the ones other DAWs use for what Noodle does; add a term
//! whenever a user story or a review turns up a new name.

use noodle_core::library::Library;

use crate::editor::search_entries as entries;

/// Where the feature a term names lives in Noodle.
pub enum Home {
    /// A node. These names, any of them, are the right result.
    Node(&'static [&'static str]),
    /// A command of the command palette, by id: any of them in the top three
    /// is the right result. The palette has no selection to act on, so for a
    /// feature that works on a clip or a track the entry says where it lives.
    Command(&'static [&'static str]),
    /// Somewhere that is neither a node nor a palette command, and so cannot
    /// be found by search.
    Elsewhere(&'static str),
}

pub struct Term {
    /// What another DAW calls it.
    pub term: &'static str,
    pub home: Home,
}

const fn node(term: &'static str, names: &'static [&'static str]) -> Term {
    Term {
        term,
        home: Home::Node(names),
    }
}

const fn command(term: &'static str, ids: &'static [&'static str]) -> Term {
    Term {
        term,
        home: Home::Command(ids),
    }
}

const fn elsewhere(term: &'static str, place: &'static str) -> Term {
    Term {
        term,
        home: Home::Elsewhere(place),
    }
}

pub const TERMS: &[Term] = &[
    // Dynamics.
    node("sidechain", &["Dynamics"]),
    node("duck", &["Dynamics"]),
    node("compressor", &["Dynamics"]),
    node("limiter", &["Dynamics"]),
    node("gate", &["Dynamics", "Spectral Gate"]),
    // Sampling.
    node("multisample", &["Sampler"]),
    node("sampler", &["Sampler"]),
    node("stretch", &["Time Stretch"]),
    node("time stretch", &["Time Stretch"]),
    // Filters and effects.
    node("eq", &["Parametric EQ"]),
    node("equalizer", &["Parametric EQ"]),
    node("lowpass", &["State Variable Filter", "Ladder Filter"]),
    node("filter", &["State Variable Filter", "Ladder Filter"]),
    node("echo", &["Delay", "Tempo Echo"]),
    node("saturation", &["Distortion"]),
    node("bitcrush", &["Bitcrusher"]),
    node("arpeggio", &["Arpeggiator"]),
    node("click track", &["Metronome"]),
    node("oscillator", &["Sine", "Saw", "Square", "Triangle"]),
    // Not nodes: commands of the palette (Cmd+K), several of which say where
    // the feature lives.
    command("bounce", &["guide.render", "file.export"]),
    command("freeze", &["guide.freeze"]),
    command("comp", &["guide.takes"]),
    command("consolidate", &["guide.render", "guide.takes"]),
    command("flatten", &["guide.takes"]),
    command("quantize", &["guide.quantise"]),
    command("warp", &["guide.warp"]),
    command("punch", &["punch.toggle", "punch.in", "punch.out"]),
    command("marker", &["markers.add"]),
    command("render", &["guide.render", "file.export"]),
    command("stem", &["guide.stems", "file.export"]),
    command("transient", &["guide.transients"]),
    command("version", &["file.export"]),
    command("lufs", &["file.export"]),
    command("lane", &["guide.lanes"]),
    node("keyswitch", &["Articulation Map"]),
];

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// One of the right nodes is the nth result (1 to 3).
    Found(usize),
    /// A node feature that the top three do not include.
    Missed,
    /// Not a node, and there is no command search.
    NotSearchable(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub term: String,
    pub verdict: Verdict,
    /// The top three results.
    pub top: Vec<String>,
}

impl Row {
    pub fn ok(&self) -> bool {
        matches!(self.verdict, Verdict::Found(_))
    }
}

/// The top three results for `term`, by name.
pub fn top_three(registry: &noodle_engine::Registry, library: &Library, term: &str) -> Vec<String> {
    entries(registry, library, term, false)
        .into_iter()
        .take(3)
        .map(|e| e.name.to_string())
        .collect()
}

pub fn check(registry: &noodle_engine::Registry, library: &Library, terms: &[Term]) -> Vec<Row> {
    terms
        .iter()
        .map(|t| {
            let top = top_three(registry, library, t.term);
            let verdict = match &t.home {
                Home::Node(names) => top
                    .iter()
                    .position(|n| names.iter().any(|w| n.eq_ignore_ascii_case(w)))
                    .map_or(Verdict::Missed, |i| Verdict::Found(i + 1)),
                Home::Command(ids) => {
                    let commands = crate::app::commands::search(t.term);
                    let top_commands: Vec<String> = commands
                        .iter()
                        .take(3)
                        .map(|e| e.name.to_string())
                        .collect();
                    let position = commands
                        .iter()
                        .take(3)
                        .position(|e| ids.contains(&e.id))
                        .map_or(Verdict::Missed, |i| Verdict::Found(i + 1));
                    return Row {
                        term: t.term.into(),
                        verdict: position,
                        top: top_commands,
                    };
                }
                Home::Elsewhere(place) => Verdict::NotSearchable((*place).into()),
            };
            Row {
                term: t.term.into(),
                verdict,
                top,
            }
        })
        .collect()
}

/// The report: a table of every term, then the misses.
pub fn markdown(rows: &[Row]) -> String {
    let mut out = String::from("| Term | Result | Top three |\n|---|---|---|\n");
    for r in rows {
        let result = match &r.verdict {
            Verdict::Found(n) => format!("found, #{n}"),
            Verdict::Missed => "MISSED".to_string(),
            Verdict::NotSearchable(place) => format!("not searchable: {place}"),
        };
        let top = if r.top.is_empty() {
            "(nothing)".to_string()
        } else {
            r.top.join(", ")
        };
        out.push_str(&format!("| {} | {result} | {top} |\n", r.term));
    }
    let found = rows.iter().filter(|r| r.ok()).count();
    let nodes = rows
        .iter()
        .filter(|r| !matches!(r.verdict, Verdict::NotSearchable(_)))
        .count();
    out.push_str(&format!(
        "\n{found} of {nodes} node features are in the top three; {} features are not nodes and \
         cannot be found by search.\n",
        rows.len() - nodes
    ));
    out
}

/// The same, as JSON.
pub fn json(rows: &[Row]) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let items: Vec<String> = rows
        .iter()
        .map(|r| {
            let (verdict, detail) = match &r.verdict {
                Verdict::Found(n) => ("found", n.to_string()),
                Verdict::Missed => ("missed", "null".into()),
                Verdict::NotSearchable(p) => ("not_searchable", format!("\"{}\"", esc(p))),
            };
            let top: Vec<String> = r.top.iter().map(|n| format!("\"{}\"", esc(n))).collect();
            format!(
                "{{\"term\":\"{}\",\"verdict\":\"{verdict}\",\"detail\":{detail},\"top\":[{}]}}",
                esc(&r.term),
                top.join(",")
            )
        })
        .collect();
    format!("[{}]\n", items.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(terms: &[Term]) -> Vec<Row> {
        check(
            &crate::session::Nodes::all().registry,
            &Library::in_memory(),
            terms,
        )
    }

    #[test]
    fn a_name_that_finds_the_node_is_found_and_one_that_does_not_is_missed() {
        let r = rows(&[
            node("sampler", &["Sampler"]),
            node("duck", &["Sampler"]),
            node("zzzz", &["Sampler"]),
        ]);
        assert_eq!(r[0].verdict, Verdict::Found(1));
        // Right words, wrong node: the search finds nothing called "duck".
        assert_eq!(r[1].verdict, Verdict::Missed);
        assert_eq!(r[2].verdict, Verdict::Missed);
        assert!(r[2].top.is_empty());
    }

    #[test]
    fn a_feature_that_is_not_a_node_is_not_searchable_whatever_the_search_returns() {
        // "freeze" does find Spectral Freeze, which is not track freeze.
        let r = rows(&[elsewhere("freeze", "the track menu")]);
        assert!(matches!(r[0].verdict, Verdict::NotSearchable(_)));
        assert!(
            r[0].top.iter().any(|n| n == "Spectral Freeze"),
            "{:?}",
            r[0].top
        );
        assert!(!r[0].ok());
    }

    #[test]
    fn every_term_that_is_a_command_is_found_by_the_palette_in_the_top_three() {
        let commands: Vec<Term> = TERMS
            .iter()
            .filter(|t| matches!(t.home, Home::Command(_)))
            .map(|t| Term {
                term: t.term,
                home: match &t.home {
                    Home::Command(ids) => Home::Command(ids),
                    _ => unreachable!(),
                },
            })
            .collect();
        assert!(commands.len() >= 10);
        for row in rows(&commands) {
            assert!(row.ok(), "{}: {:?}", row.term, row.top);
        }
    }

    #[test]
    fn the_report_lists_every_term_and_counts_the_misses() {
        let r = rows(&[
            node("sampler", &["Sampler"]),
            node("zzzz", &["Sampler"]),
            elsewhere("bounce", "the clip menu"),
        ]);
        let md = markdown(&r);
        assert!(md.contains("| sampler | found, #1 |"), "{md}");
        assert!(md.contains("| zzzz | MISSED | (nothing) |"), "{md}");
        assert!(md.contains("not searchable: the clip menu"), "{md}");
        assert!(md.contains("1 of 2 node features"), "{md}");
        let js = json(&r);
        assert!(js.contains("\"verdict\":\"missed\""), "{js}");
    }

    /// The terms that are in the list are not misspelt: each node name is a
    /// node the search can offer.
    #[test]
    fn every_expected_node_exists() {
        let all: Vec<String> = entries(
            &crate::session::Nodes::all().registry,
            &Library::in_memory(),
            "",
            false,
        )
        .into_iter()
        .map(|e| e.name.to_string())
        .collect();
        for t in TERMS {
            if let Home::Node(names) = &t.home {
                for n in *names {
                    assert!(all.iter().any(|a| a == n), "no node is called {n:?}");
                }
            }
        }
    }
}
