//! `noodle-eval compare REF TEST [SPEED]` prints every metric as JSON;
//! `noodle-eval validate MANIFEST` correlates them with listener scores.

use noodle_eval::{Clip, TimeMap, evaluate, validate};
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("noodle-eval: {e}");
            eprintln!("usage: noodle-eval compare REF TEST [SPEED] | validate MANIFEST.csv");
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
        _ => Err("bad arguments".into()),
    }
}
