//! The harness on probes: the time maps line a native render up with the
//! source, the engine's warp comes out the length it should, and the metrics
//! tell the floor (Repitch) from a pitch-keeping mode. None of it gates on how
//! good Grain or Beats sound; ADR 0003 holds that back until the metrics are
//! checked against listeners.

use noodle_eval::harness::{Harness, Method, Row, View};
use noodle_eval::probe::Probe;
use noodle_eval::tempo::TempoCase;
use noodle_eval::{Clip, evaluate};

const RATE: u32 = 48_000;

fn harness() -> Harness {
    Harness::new(RATE, 120.0).unwrap()
}

fn find(rows: &[Row], method: Method, view: View) -> &Row {
    rows.iter()
        .find(|r| r.method == method && r.view == view)
        .unwrap()
}

#[test]
fn the_source_map_puts_the_native_render_on_the_source_for_every_tempo_case() {
    // The native render at the target tempo is the source, retimed exactly.
    // Scoring the two through the map built from the tempo maps must find
    // every click where the map says, or the scores of every warp are off.
    let h = harness();
    for case in TempoCase::all() {
        let target = h.target(case);
        let native = Probe::ClickTrain.render(&target, RATE);
        let source = h.source(Probe::ClickTrain);
        let report = evaluate(
            Clip::new(&source, 2, RATE),
            Clip::new(&native, 2, RATE),
            &h.source_map(case, &target),
        );
        let t = report.timing;
        assert!(
            t.expected >= 20 && t.matched + 1 >= t.expected,
            "{}: {t:?}",
            case.name()
        );
        assert!(
            t.max_abs_offset_ms < 12.0 && t.drift_ms < 6.0,
            "{}: {t:?}",
            case.name()
        );
    }
}

#[test]
fn a_wrong_map_is_caught() {
    // The same pair under the wrong ratio must not look aligned.
    let h = harness();
    let case = TempoCase::Ratio(1.5);
    let target = h.target(case);
    let native = Probe::ClickTrain.render(&target, RATE);
    let source = h.source(Probe::ClickTrain);
    let report = evaluate(
        Clip::new(&source, 2, RATE),
        Clip::new(&native, 2, RATE),
        &noodle_eval::TimeMap::Speed(1.4),
    );
    assert!(
        report.timing.max_abs_offset_ms > 30.0,
        "{:?}",
        report.timing
    );
}

#[test]
fn a_probe_scores_perfectly_against_its_own_native_render() {
    let h = harness();
    let target = h.target(TempoCase::Ramp);
    let native = Probe::HeldChord.render(&target, RATE);
    let r = evaluate(
        Clip::new(&native, 2, RATE),
        Clip::new(&native, 2, RATE),
        &noodle_eval::TimeMap::Identity,
    );
    assert!(r.spectral.log_distance_db < 1e-6);
    assert!(r.paper.mph_mw < 1e-9);
}

#[test]
fn every_method_renders_every_view_and_the_floor_cannot_keep_pitch() {
    let h = harness();
    let rows = h
        .run(Probe::Sweep, TempoCase::Ratio(1.5), &Method::ALL)
        .unwrap();
    assert_eq!(rows.len(), Method::ALL.len() * View::ALL.len());
    assert!(rows.iter().all(|r| r.metrics.len() >= 23));
    // Only the headline method carries the detected tempo.
    assert!(
        rows.iter()
            .all(|r| { (r.method == Method::Auto) == r.metric("detected_bpm").is_some() })
    );
    // A fifth up: Repitch moves the sweep (one voice, so pitch is measurable),
    // Grain leaves it.
    for view in [View::Native, View::Source] {
        let floor = find(&rows, Method::Repitch, view)
            .metric("pitch_median_abs_cents")
            .unwrap();
        let grain = find(&rows, Method::Grain, view)
            .metric("pitch_median_abs_cents")
            .unwrap();
        assert!(floor > 300.0, "{view:?}: repitch {floor}");
        assert!(grain < 100.0, "{view:?}: grain {grain}");
    }
    // Repitch there and back is the identity, which is why the round trip
    // alone would hide the floor.
    let trip = find(&rows, Method::Repitch, View::RoundTrip);
    assert!(trip.metric("pitch_median_abs_cents").unwrap() < 50.0);
}

#[test]
fn a_changing_tempo_warps_to_the_native_length_and_keeps_the_clicks_on_the_map() {
    let h = harness();
    let rows = h
        .run(Probe::ClickTrain, TempoCase::Step, &[Method::Beats])
        .unwrap();
    let row = find(&rows, Method::Beats, View::Native);
    // The warp renders as many frames as the native render has, and its clicks
    // land where the native ones do, give or take the stretcher's latency.
    assert!(
        row.metric("missed_onsets").unwrap() < 0.3,
        "{:?}",
        row.metrics
    );
    assert!(row.metric("median_abs_offset_ms").unwrap() < 30.0);
}
