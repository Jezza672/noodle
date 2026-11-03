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

pub const STORIES: [Story; 30] = [
    Story {
        id: "S01",
        title: "Comp a vocal from multiple takes",
        status: Status::Partial,
        missing: "A new recording lands as its own clip instead of joining the take folder it was recorded over, and sliding the comp. The takes are recorded for real through the virtual input (#334).",
    },
    Story {
        id: "S02",
        title: "Punch in on a live overdub",
        status: Status::Partial,
        missing: "A headphone cue mix, which waits on sends in the Mix node (#332, #345); recording is not routed because a route has no live input (#334).",
    },
    Story {
        id: "S03",
        title: "Drive external hardware like an internal instrument",
        status: Status::Partial,
        missing: "The MIDI Out port is typed text (#346); recording the synth's audio back is not routed (#334).",
    },
    Story {
        id: "S04",
        title: "Tighten a multitrack drum recording",
        status: Status::Blocked,
        missing: "No group transient quantize with a strength and crossfades: #347.",
    },
    Story {
        id: "S05",
        title: "Conform audio to a new tempo and key",
        status: Status::Partial,
        missing: "No clip pitch shift, project key or formant preservation: #348.",
    },
    Story {
        id: "S06",
        title: "Separate a song into stems and remix it",
        status: Status::Partial,
        missing: "The route stops at the model download question, so separation and the remix are not routed; no quality setting: #349.",
    },
    Story {
        id: "S07",
        title: "Restructure an arrangement by section",
        status: Status::Blocked,
        missing: "No named arrangement sections: #329.",
    },
    Story {
        id: "S08",
        title: "Edit MIDI performance detail",
        status: Status::Partial,
        missing: "No humanise: #350; recording the part and redrawing the sustain lane are not routed.",
    },
    Story {
        id: "S09",
        title: "Program a drum pattern with swing and choke",
        status: Status::Partial,
        missing: "No swing; the hat choke needs a hand-written Script node; loading the samples and swapping the snare are not routed: #330.",
    },
    Story {
        id: "S10",
        title: "Program multi-articulation orchestral parts",
        status: Status::Workaround,
        missing: "Keyswitch notes are placed by hand; no articulation map, chasing or rename: #351.",
    },
    Story {
        id: "S11",
        title: "Turn an audio phrase into MIDI",
        status: Status::Blocked,
        missing: "No audio-to-MIDI conversion: #352.",
    },
    Story {
        id: "S12",
        title: "Build a multisampled instrument from a folder",
        status: Status::Full,
        missing: "",
    },
    Story {
        id: "S13",
        title: "Design a modulated synth patch",
        status: Status::Partial,
        missing: "The macro is built by hand from Remap nodes; no macro control with ranges and curves: #353. Saving and updating the preset are not routed.",
    },
    Story {
        id: "S14",
        title: "Share modulation sources across many parameters",
        status: Status::Partial,
        missing: "No step sequencer to replace the LFO with, keeping its targets: #354.",
    },
    Story {
        id: "S15",
        title: "Make a tempo-synced rhythmic gate",
        status: Status::Partial,
        missing: "No step-editable gate pattern, so no triplet change by pattern: #354.",
    },
    Story {
        id: "S16",
        title: "Make a vocoder or spectral effect",
        status: Status::Partial,
        missing: "The spectral frame size is fixed (a roadmap item); the band count is Cross's `bands` parameter (#355).",
    },
    Story {
        id: "S17",
        title: "Resample and mangle audio",
        status: Status::Partial,
        missing: "No slicing of a clip by transient onto a sampler: #356; rendering to a clip is not routed.",
    },
    Story {
        id: "S18",
        title: "Use third-party plugins",
        status: Status::Partial,
        missing: "Only CLAP is hosted, and the test machine has no plugin to add, automate or save; a swap keeping automation is routed with descriptors only (S18 replace route); opening a project without a plugin is a session test.",
    },
    Story {
        id: "S19",
        title: "Build a parallel processing mix bus",
        status: Status::Partial,
        missing: "Sends are extra wires and pre/post is which wire; delay compensation and adding a drum track are not routed: #332.",
    },
    Story {
        id: "S20",
        title: "Automate a mix in real time",
        status: Status::Partial,
        missing: "Trimming a range by a dB amount is not routed: an Automation lane trims in its 0 to 1 value (#333 built the range trim); changing parameter or surface is not routed.",
    },
    Story {
        id: "S21",
        title: "Recall alternate mix versions",
        status: Status::Blocked,
        missing: "No mix snapshots, versions or batch export of versions: #358.",
    },
    Story {
        id: "S22",
        title: "Produce a surround or immersive mix",
        status: Status::Partial,
        missing: "No 3D object panning, 7.1.4 layout or ADM export: #359.",
    },
    Story {
        id: "S23",
        title: "Master to loudness targets",
        status: Status::Partial,
        missing: "No album sequence, true-peak limiter after normalising, DDP or ISRC: #360; the native file dialog stops the export.",
    },
    Story {
        id: "S24",
        title: "Deliver stems to a collaborator",
        status: Status::Partial,
        missing: "No tail length setting: #361; the native folder dialog stops the export.",
    },
    Story {
        id: "S25",
        title: "Exchange a session via interchange formats",
        status: Status::Blocked,
        missing: "No AAF or OMF import or export: #362.",
    },
    Story {
        id: "S26",
        title: "Score and mix to picture",
        status: Status::Blocked,
        missing: "No video, timecode or hit points: #363.",
    },
    Story {
        id: "S27",
        title: "Perform a set live with clips and scenes",
        status: Status::Blocked,
        missing: "No clip launching or scenes (#364) and no Ableton Link (#365; tap tempo and MIDI clock in are built, but a route cannot see the session-only tempo source).",
    },
    Story {
        id: "S28",
        title: "Run backing tracks reliably on stage",
        status: Status::Blocked,
        missing: "No setlist: #366.",
    },
    Story {
        id: "S29",
        title: "Organise a large session with templates",
        status: Status::Partial,
        missing: "No track colours, folders, hiding or macros: #367; saving a template is not routed.",
    },
    Story {
        id: "S30",
        title: "Keep a large project responsive",
        status: Status::Partial,
        missing: "The CPU view and the 60-track size are not routed: #369.",
    },
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
