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

/// A story assessed from the docs and the code, with no route driving it yet.
const fn assessed(
    id: &'static str,
    title: &'static str,
    status: Status,
    missing: &'static str,
) -> Story {
    Story {
        id,
        title,
        status,
        missing,
    }
}

pub const STORIES: [Story; 30] = [
    Story {
        id: "S01",
        title: "Comp a vocal from multiple takes",
        status: Status::Partial,
        missing: "Recording the takes (the route starts from a take folder), a ninth take and sliding the comp: #334.",
    },
    assessed(
        "S02",
        "Punch in on a live overdub",
        Status::Partial,
        "Input monitoring with a headphone cue mix. Punch, count-in and pre-roll exist. Not routed.",
    ),
    assessed(
        "S03",
        "Drive external hardware like an internal instrument",
        Status::Partial,
        "The MIDI Out port is typed text, with no picker; aligning the recorded audio to the round-trip latency is not routed.",
    ),
    assessed(
        "S04",
        "Tighten a multitrack drum recording",
        Status::Blocked,
        "Quantising detected audio transients across linked clips at a strength.",
    ),
    assessed(
        "S05",
        "Conform audio to a new tempo and key",
        Status::Partial,
        "Tempo conform works; no pitch shift to a project key and no formant preservation.",
    ),
    assessed(
        "S06",
        "Separate a song into stems and remix it",
        Status::Partial,
        "Stems split and linked; no quality setting to rerun at. Not routed.",
    ),
    Story {
        id: "S07",
        title: "Restructure an arrangement by section",
        status: Status::Blocked,
        missing: "No named arrangement sections: #329.",
    },
    assessed(
        "S08",
        "Edit MIDI performance detail",
        Status::Partial,
        "No humanise. CC lanes and quantise exist. Not routed.",
    ),
    Story {
        id: "S09",
        title: "Program a drum pattern with swing and choke",
        status: Status::Partial,
        missing: "No swing; the hat choke needs a hand-written Script node; loading the samples and swapping the snare are not routed: #330.",
    },
    assessed(
        "S10",
        "Program multi-articulation orchestral parts",
        Status::Blocked,
        "No articulations or keyswitch maps, and no chase of the switch when starting mid-cue.",
    ),
    assessed(
        "S11",
        "Turn an audio phrase into MIDI",
        Status::Blocked,
        "No audio-to-MIDI conversion.",
    ),
    Story {
        id: "S12",
        title: "Build a multisampled instrument from a folder",
        status: Status::Full,
        missing: "",
    },
    assessed(
        "S13",
        "Design a modulated synth patch",
        Status::Partial,
        "No macro knob mapping one control to many targets with range and curve each; it can be built by hand from group inputs and Remap. Not routed.",
    ),
    assessed(
        "S14",
        "Share modulation sources across many parameters",
        Status::Partial,
        "Fan-out and shared sources work; there is no step sequencer node to swap in. Not routed.",
    ),
    assessed(
        "S15",
        "Make a tempo-synced rhythmic gate",
        Status::Partial,
        "No step-editable gate node; it can be built from Clock, Comparator and Math. Not routed.",
    ),
    assessed(
        "S16",
        "Make a vocoder or spectral effect",
        Status::Partial,
        "Spectral Cross, Freeze and Blur exist; the frame size is fixed, so there is no band count. Not routed.",
    ),
    assessed(
        "S17",
        "Resample and mangle audio",
        Status::Partial,
        "Render to clip and Reverse exist; no slicing a clip to a sampler by transient. Not routed.",
    ),
    assessed(
        "S18",
        "Use third-party plugins",
        Status::Partial,
        "Plugins load and save state; replacing one keeping automation, and opening with one missing, are untested. Not routed.",
    ),
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
        missing: "Trimming a range needs a second gain stage and lane; changing parameter or surface is not routed: #333.",
    },
    assessed(
        "S21",
        "Recall alternate mix versions",
        Status::Blocked,
        "No mix snapshots or versions, A/B, or batch export of versions.",
    ),
    assessed(
        "S22",
        "Produce a surround or immersive mix",
        Status::Partial,
        "Surround beds and fold-down exist; no 3D object panning, no 7.1.4 and no ADM export.",
    ),
    assessed(
        "S23",
        "Master to loudness targets",
        Status::Partial,
        "Loudness, targets and dither exist; no album sequence, true-peak limiter, DDP or ISRC.",
    ),
    assessed(
        "S24",
        "Deliver stems to a collaborator",
        Status::Partial,
        "Track export aligns every stem; no tail length setting. Not routed.",
    ),
    assessed(
        "S25",
        "Exchange a session via interchange formats",
        Status::Blocked,
        "No AAF or OMF import or export.",
    ),
    assessed(
        "S26",
        "Score and mix to picture",
        Status::Blocked,
        "No video, timecode offset or SMPTE hit points.",
    ),
    assessed(
        "S27",
        "Perform a set live with clips and scenes",
        Status::Blocked,
        "No clip launching or scenes, and no transport tap tempo, MIDI clock in or Link.",
    ),
    assessed(
        "S28",
        "Run backing tracks reliably on stage",
        Status::Blocked,
        "No setlist, per-song cues or footswitch advance.",
    ),
    assessed(
        "S29",
        "Organise a large session with templates",
        Status::Partial,
        "Groups and templates exist; no track colours, folders, hiding or key-command macros. Not routed.",
    ),
    assessed(
        "S30",
        "Keep a large project responsive",
        Status::Partial,
        "Freeze, render to clip and `noodle profile` exist. Not routed.",
    ),
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
