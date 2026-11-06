//! S16: impose the spectral envelope of one sound on another.
//!
//! Partial. The spectral nodes work in one fixed frame size, but Cross has a
//! `bands` parameter that smooths the spectra into that many log-spaced
//! bands (#355), which is the story's band count. A saw chord is the carrier and a noise burst
//! stands in for the vocal modulator: both go through Analyze, then Cross
//! and Synthesize. Swapping the carrier or the modulator is one wire.

use std::path::Path;

use super::super::handle::Probe;
use super::super::{LateChange, Route, Tag};
use super::patch::{Op, Ref, audible, check, kind, output, reaches, run, seeded, upstream};
use crate::app::App;

const CARRIER: Ref = Ref("osc.saw", 0);
const CARRIER2: Ref = Ref("osc.square", 0);
const MODULATOR: Ref = Ref("noise.white", 0);
const MODULATOR2: Ref = Ref("osc.triangle", 0);
const A1: Ref = Ref("spectral.analyze", 0);
const A2: Ref = Ref("spectral.analyze", 1);
const CROSS: Ref = Ref("spectral.cross", 0);
const SYNTH: Ref = Ref("spectral.synthesize", 0);
const OUT: Ref = Ref("io.output", 0);

fn fixture(dir: &Path) -> App {
    seeded(
        dir,
        &[
            ("osc.saw", 60.0, 120.0, &[("frequency", 110.0)]),
            ("noise.white", 60.0, 420.0, &[]),
            ("io.output", 1150.0, 300.0, &[]),
            ("osc.square", 60.0, 600.0, &[("frequency", 165.0)]),
            ("osc.triangle", 300.0, 600.0, &[("frequency", 440.0)]),
        ],
        &[],
    )
}

const BUILD: &[Op] = &[
    Op::Step("analyse both sounds"),
    Op::Add(A1, 300.0, 316.0),
    Op::Add(A2, 300.0, 556.0),
    Op::Wire(CARRIER, "out", A1, "in"),
    Op::Wire(MODULATOR, "out", A2, "in"),
    Op::Step("cross them and resynthesise"),
    Op::Add(CROSS, 580.0, 436.0),
    Op::Add(SYNTH, 850.0, 436.0),
    Op::Wire(A1, "spectrum", CROSS, "carrier"),
    Op::Wire(A2, "spectrum", CROSS, "modulator"),
    Op::Wire(CROSS, "out", SYNTH, "spectrum"),
    Op::Wire(SYNTH, "out", OUT, "in"),
    Op::Step("listen"),
    Op::Play,
];

const SWAP_CARRIER: &[Op] = &[
    Op::Step("analyse a square chord as the carrier instead"),
    Op::Wire(CARRIER2, "out", A1, "in"),
];

const SWAP_MODULATOR: &[Op] = &[
    Op::Step("use another source as the modulator"),
    Op::Wire(MODULATOR2, "out", A2, "in"),
];

const MORE_BANDS: &[Op] = &[
    Op::Step("change the number of bands"),
    Op::Set(CROSS, "bands", 16.0),
];

/// The goal: two different sounds meet in a spectral Cross, and the result
/// reaches the output, whether or not it goes straight there from Synthesize.
fn crossed(p: &mut Probe) -> Result<(), String> {
    let project = p.project();
    let out = output(project)?;
    let cross = project
        .graph()
        .nodes()
        .map(|(id, _)| id)
        .find(|&n| kind(project, n) == "spectral.cross" && reaches(project, n, out))
        .ok_or("no spectral Cross reaches the output")?;
    let sounds = |port: &str| {
        project
            .graph()
            .source(&noodle_core::Endpoint::new(cross, port))
            .map(|s| {
                let mut all = upstream(project, s.node);
                all.insert(s.node);
                all
            })
            .unwrap_or_default()
    };
    let (carrier, modulator) = (sounds("carrier"), sounds("modulator"));
    if carrier.is_empty() || modulator.is_empty() {
        return Err("the Cross needs a sound on both its carrier and its modulator".into());
    }
    if carrier == modulator {
        return Err("the carrier and the modulator are the same sound".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S16",
        tag: Tag::Default,
        name: "cross a saw carrier with a noise modulator",
        fixture,
        size: None,
        steps: |h, _| run(h, BUILD),
        done: |p, _| {
            crossed(p)?;
            audible(p)
        },
        late: &[
            LateChange {
                name: "swap the carrier",
                steps: |h, _| run(h, SWAP_CARRIER),
                done: |p, _| check(p, SWAP_CARRIER),
                keep: &[],
            },
            LateChange {
                name: "change the number of bands",
                steps: |h, _| run(h, MORE_BANDS),
                done: |p, _| check(p, MORE_BANDS),
                keep: &[],
            },
            LateChange {
                name: "use a different modulator",
                steps: |h, _| run(h, SWAP_MODULATOR),
                done: |p, _| check(p, SWAP_MODULATOR),
                keep: &[],
            },
        ],
    }
}
