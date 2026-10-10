//! `#[derive(Ports)]` builds the same layout as the builder, and its
//! constants index the ports it declares.

use noodle_engine::{Layout, ParamInfo, Ports, Unit};

#[derive(Ports)]
struct Demo {
    #[input("in", "In")]
    input: (),
    #[param(
        "cutoff",
        "Cutoff",
        ParamInfo::new(20.0, 20_000.0, 1_000.0)
            .log()
            .unit(Unit::Hertz)
    )]
    cutoff: (),
    #[output("low", "Low")]
    low: (),
    #[output("high", "High")]
    high: (),
    #[event_input("notes", "Notes")]
    notes: (),
    #[event_output("out", "Events")]
    events: (),
}

#[derive(Ports)]
#[ports(offline, nondeterministic)]
struct Odd {
    #[input("in", "In")]
    input: (),
}

#[test]
fn layout_matches_the_builder() {
    let by_hand = Layout::realtime()
        .input("in", "In")
        .param(
            "cutoff",
            "Cutoff",
            ParamInfo::new(20.0, 20_000.0, 1_000.0)
                .log()
                .unit(Unit::Hertz),
        )
        .output("low", "Low")
        .output("high", "High")
        .event_input("notes", "Notes")
        .event_output("out", "Events");
    assert_eq!(Demo::layout(), by_hand);
}

#[test]
fn constants_index_each_kind_of_port_separately() {
    // Audio and parameter inputs share an index space; outputs and event
    // ports each have their own.
    assert_eq!((Demo::INPUT, Demo::CUTOFF), (0, 1));
    assert_eq!((Demo::LOW, Demo::HIGH), (0, 1));
    assert_eq!((Demo::NOTES, Demo::EVENTS), (0, 0));
    assert_eq!(
        (
            Demo::INPUTS,
            Demo::OUTPUTS,
            Demo::EVENT_INPUTS,
            Demo::EVENT_OUTPUTS
        ),
        (2, 2, 1, 1)
    );
    let layout = Demo::layout();
    assert_eq!(layout.inputs[Demo::CUTOFF].key, "cutoff");
    assert_eq!(layout.outputs[Demo::HIGH].key, "high");
}

#[test]
fn mode_options() {
    let layout = Odd::layout();
    assert_eq!(layout.mode, noodle_engine::Mode::Offline);
    assert!(!layout.deterministic);
}
