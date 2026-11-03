//! The thirty stories and how far Noodle gets with each.
//!
//! The titles are checked against `docs/usability/stories.md`, which owns the
//! IDs. A story's status moves up as its routes land.

/// How far a story's best route gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// A route reaches every part of the goal.
    Full,
    /// A route reaches part of it; the rest is listed in `missing`.
    Partial,
    /// Possible only with an external tool or a hack.
    Workaround,
    /// Not possible yet; `missing` names the missing piece.
    Blocked,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Full => "full",
            Status::Partial => "partial",
            Status::Workaround => "workaround",
            Status::Blocked => "blocked",
        }
    }
}

pub struct Story {
    pub id: &'static str,
    pub title: &'static str,
    pub status: Status,
    /// For anything short of `Full`: what is missing, in one line.
    pub missing: &'static str,
}

const fn blocked(id: &'static str, title: &'static str) -> Story {
    Story {
        id,
        title,
        status: Status::Blocked,
        missing: "No route written yet.",
    }
}

pub const STORIES: [Story; 30] = [
    Story {
        id: "S01",
        title: "Comp a vocal from multiple takes",
        status: Status::Partial,
        missing: "Recording the takes (the route starts from a take folder), a ninth take and sliding the comp: #334.",
    },
    blocked("S02", "Punch in on a live overdub"),
    blocked("S03", "Drive external hardware like an internal instrument"),
    blocked("S04", "Tighten a multitrack drum recording"),
    blocked("S05", "Conform audio to a new tempo and key"),
    blocked("S06", "Separate a song into stems and remix it"),
    Story {
        id: "S07",
        title: "Restructure an arrangement by section",
        status: Status::Blocked,
        missing: "No named arrangement sections: #329.",
    },
    blocked("S08", "Edit MIDI performance detail"),
    Story {
        id: "S09",
        title: "Program a drum pattern with swing and choke",
        status: Status::Partial,
        missing: "No swing or hat choke, and loading the samples and swapping the snare are not routed: #330.",
    },
    blocked("S10", "Program multi-articulation orchestral parts"),
    blocked("S11", "Turn an audio phrase into MIDI"),
    Story {
        id: "S12",
        title: "Build a multisampled instrument from a folder",
        status: Status::Blocked,
        missing: "Nothing maps a folder of note-named samples to an instrument: #331.",
    },
    blocked("S13", "Design a modulated synth patch"),
    blocked("S14", "Share modulation sources across many parameters"),
    blocked("S15", "Make a tempo-synced rhythmic gate"),
    blocked("S16", "Make a vocoder or spectral effect"),
    blocked("S17", "Resample and mangle audio"),
    blocked("S18", "Use third-party plugins"),
    Story {
        id: "S19",
        title: "Build a parallel processing mix bus",
        status: Status::Partial,
        missing: "No pre-fader sends, so no parallel bus and no send switch: #332.",
    },
    Story {
        id: "S20",
        title: "Automate a mix in real time",
        status: Status::Partial,
        missing: "Control surface cannot be driven from a test.",
    },
    blocked("S21", "Recall alternate mix versions"),
    blocked("S22", "Produce a surround or immersive mix"),
    blocked("S23", "Master to loudness targets"),
    blocked("S24", "Deliver stems to a collaborator"),
    blocked("S25", "Exchange a session via interchange formats"),
    blocked("S26", "Score and mix to picture"),
    blocked("S27", "Perform a set live with clips and scenes"),
    blocked("S28", "Run backing tracks reliably on stage"),
    blocked("S29", "Organise a large session with templates"),
    blocked("S30", "Keep a large project responsive"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_matches_the_stories_doc() {
        let doc = include_str!("../../../../docs/usability/stories.md");
        for story in &STORIES {
            let line = format!("- **{}** **{}.**", story.id, story.title);
            assert!(doc.contains(&line), "{} is not in stories.md", story.id);
        }
        let in_doc = doc.matches("\n- **S").count();
        assert_eq!(
            in_doc,
            STORIES.len(),
            "a story is missing from the registry"
        );
    }

    #[test]
    fn every_story_has_a_status_and_a_missing_piece() {
        for (i, story) in STORIES.iter().enumerate() {
            assert_eq!(story.id, format!("S{:02}", i + 1));

            if story.status != Status::Full {
                assert!(!story.missing.is_empty(), "{}", story.id);
            }
        }
    }
}
