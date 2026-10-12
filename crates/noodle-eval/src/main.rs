//! `noodle-eval compare REF TEST [SPEED]` prints every metric as JSON;
//! `noodle-eval validate MANIFEST` correlates them with listener scores;
//! `noodle-eval probes` warps the probe signals and scores the result.

use noodle_eval::harness::{Harness, Method};
use noodle_eval::probe::Probe;
use noodle_eval::table::{self, Baseline};
use noodle_eval::tempo::TempoCase;
use noodle_eval::{Clip, TimeMap, evaluate, validate};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("noodle-eval: {e}");
            eprintln!(
                "usage: noodle-eval compare REF TEST [SPEED] | validate MANIFEST.csv | probes [OPTIONS]"
            );
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["compare", reference, test, rest @ ..] => {
            let load =
                |p: &str| noodle_io::decode_file(Path::new(p)).map_err(|e| format!("{p}: {e}"));
            let (r, t) = (load(reference)?, load(test)?);
            if r.sample_rate != t.sample_rate {
                return Err("the files have different sample rates".into());
            }
            let map = match rest {
                [] => TimeMap::Identity,
                [s] => TimeMap::Speed(s.parse().map_err(|e| format!("speed: {e}"))?),
                _ => return Err("too many arguments".into()),
            };
            let report = evaluate(Clip::from_audio(&r), Clip::from_audio(&t), &map);
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        ["validate", manifest] => {
            let rows = validate::parse_manifest(Path::new(manifest))?;
            println!("{} pairs", rows.len());
            println!(
                "{:<32} {:>8} {:>8} {:>6}  usable",
                "metric", "pooled", "in-speed", "pairs"
            );
            for c in validate::run(&rows)? {
                println!(
                    "{:<32} {:>8.3} {:>8.3} {:>6}  {}",
                    c.metric,
                    c.pooled_spearman,
                    c.within_speed_spearman,
                    c.pairs,
                    if c.usable() { "yes" } else { "no" }
                );
            }
            Ok(())
        }
        ["probes", rest @ ..] => probes(rest),
        _ => Err("bad arguments".into()),
    }
}

const PROBES_HELP: &str =
    "probes [--probe NAME] [--tempo NAME] [--method NAME] [--baseline FILE] [--update]
       [--export DIR]
  Warps each probe signal to each target tempo with each method and scores it.
  --probe, --tempo and --method each pick one (default: all; see the names in
  the table). --baseline FILE diffs against that file (default: the committed
  crates/noodle-eval/baseline/probes.json); --update writes the rows that ran
  into it. --export DIR also writes every render as a WAV to listen to
  (source, native, and per method the warped and round-trip audio). A diff is
  a before and after, never a pass or fail.";

fn probes(args: &[&str]) -> Result<(), String> {
    let (mut probe, mut tempo, mut method) = (None, None, None);
    let mut baseline = Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline/probes.json");
    let mut update = false;
    let mut export = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().copied().ok_or(format!("{arg} needs a value"));
        match *arg {
            "--probe" => {
                let v = value()?;
                probe = Some(Probe::parse(v).ok_or(format!("no probe {v}"))?);
            }
            "--tempo" => {
                let v = value()?;
                tempo = Some(TempoCase::parse(v).ok_or(format!("no tempo case {v}"))?);
            }
            "--method" => {
                let v = value()?;
                method = Some(Method::parse(v).ok_or(format!("no method {v}"))?);
            }
            "--baseline" => baseline = value()?.into(),
            "--update" => update = true,
            "--export" => export = Some(PathBuf::from(value()?)),
            "--help" | "-h" => {
                println!("{PROBES_HELP}");
                return Ok(());
            }
            other => return Err(format!("unknown option {other}\n{PROBES_HELP}")),
        }
    }
    let probes = probe.map_or(Probe::ALL.to_vec(), |p| vec![p]);
    let cases = tempo.map_or(TempoCase::all(), |c| vec![c]);
    let methods = method.map_or(Method::ALL.to_vec(), |m| vec![m]);
    let mut harness = Harness::new(48_000, 110.0).map_err(|e| e.to_string())?;
    harness.export = export;
    let mut rows = Vec::new();
    for &p in &probes {
        for &c in &cases {
            eprintln!("{} at {}", p.name(), c.name());
            rows.extend(harness.run(p, c, &methods)?);
        }
    }
    print!("{}", table::format_rows(&rows));
    println!();
    print!("{}", table::format_ranks(&table::mean_ranks(&rows)));
    let fresh = Baseline::of(&rows);
    let old = match std::fs::read_to_string(&baseline) {
        Ok(text) => {
            Baseline::from_json(&text).map_err(|e| format!("{}: {e}", baseline.display()))?
        }
        Err(_) => Baseline::default(),
    };
    println!();
    print!("{}", table::format_changes(&table::diff(&old, &fresh)));
    if update {
        let merged = old.merged(fresh);
        if let Some(dir) = baseline.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&baseline, merged.to_json())
            .map_err(|e| format!("{}: {e}", baseline.display()))?;
        println!("wrote {}", baseline.display());
    }
    Ok(())
}
