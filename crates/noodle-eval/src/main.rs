//! `noodle-eval compare REF TEST [SPEED]` prints every metric as JSON;
//! `noodle-eval validate MANIFEST` correlates them with listener scores;
//! `noodle-eval probes` warps the probe signals and scores the result;
//! `noodle-eval songs` does the same for the generated songs;
//! `noodle-eval borrowed` scores them against reference stretchers on audio
//! we cannot generate.

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
                "usage: noodle-eval compare REF TEST [SPEED] | validate MANIFEST.csv | probes [OPTIONS] | songs [OPTIONS] | borrowed [OPTIONS]"
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
        ["songs", rest @ ..] => songs(rest),
        ["borrowed", rest @ ..] => borrowed(rest),
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
    report(&rows, &baseline, update)
}

const SONGS_HELP: &str =
    "songs [--song NAME] [--tempo NAME] [--method NAME] [--full] [--baseline FILE] [--update]
       [--export DIR]
  Warps the generated songs (crates/noodle-eval/songs) to each target tempo with
  each method and scores them. By default the 15 to 20 s excerpt that joins the
  CI gate; --full takes the whole song (rows are named NAME-full). Other options
  as for probes (--export writes <song>_source.wav, <song>_<tempo>_native.wav and so on);
  the default baseline is crates/noodle-eval/baseline/songs.json.";

fn songs(args: &[&str]) -> Result<(), String> {
    let (mut song, mut tempo, mut method, mut full) = (None, None, None, false);
    let mut export = None;
    let mut baseline = Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline/songs.json");
    let mut update = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().copied().ok_or(format!("{arg} needs a value"));
        match *arg {
            "--song" => song = Some(value()?.to_string()),
            "--tempo" => {
                let v = value()?;
                tempo = Some(TempoCase::parse(v).ok_or(format!("no tempo case {v}"))?);
            }
            "--method" => {
                let v = value()?;
                method = Some(Method::parse(v).ok_or(format!("no method {v}"))?);
            }
            "--full" => full = true,
            "--export" => export = Some(PathBuf::from(value()?)),
            "--baseline" => baseline = value()?.into(),
            "--update" => update = true,
            "--help" | "-h" => {
                println!("{SONGS_HELP}");
                return Ok(());
            }
            other => return Err(format!("unknown option {other}\n{SONGS_HELP}")),
        }
    }
    let cases = tempo.map_or(TempoCase::all(), |c| vec![c]);
    let methods = method.map_or(Method::ALL.to_vec(), |m| vec![m]);
    let mut rows = Vec::new();
    let mut found = false;
    for s in noodle_eval::song::all() {
        if song.as_deref().is_some_and(|n| n != s.name) {
            continue;
        }
        found = true;
        let (range, name) = if full {
            (s.whole(), format!("{}-full", s.name))
        } else {
            (s.excerpt_range(), s.name.to_string())
        };
        let mut harness = Harness::for_song(&s, range, 48_000).map_err(|e| e.to_string())?;
        harness.export.clone_from(&export);
        for &c in &cases {
            eprintln!("{name} at {}", c.name());
            rows.extend(harness.run_song(&name, &s, range, c, &methods)?);
        }
    }
    if !found {
        return Err("no such song".into());
    }
    report(&rows, &baseline, update)
}

const BORROWED_HELP: &str =
    "borrowed [--clip NAME] [--probes] [--probe NAME] [--tempo NAME] [--method NAME]
         [--seconds N] [--dir DIR] [--cache DIR] [--tool NAME] [--tables] [--export DIR]
         [--baseline FILE] [--update] [--no-latency-compensation]
  Scores Noodle's warps of audio we cannot generate against what reference
  stretchers (Rubber Band R3, SoundTouch, Signalsmith; whichever are installed)
  make of it. Clips come from eval/local (any file you drop in) and
  eval/cache/clips (eval/fetch.sh); --clip picks one by file name, --dir adds a
  folder, --seconds cuts each to its first N seconds (default 30). --probes
  adds the probe signals, which also have a native render to score against.
  --tool NAME (repeatable) limits the references; --tables prints every case
  instead of the summary; --export writes each contender's audio. The stretches
  are cached in --cache (default eval/cache/refs), outside git. The baseline
  (default crates/noodle-eval/baseline/borrowed.json) is a before and after,
  never a threshold.";

fn borrowed(args: &[&str]) -> Result<(), String> {
    use noodle_eval::borrowed::{self as b, Options};
    use noodle_eval::refs::{RefCache, Stretcher, Tool};
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval");
    let mut dirs = vec![repo.join("local"), repo.join("cache/clips")];
    let mut cache = repo.join("cache/refs");
    let mut baseline = Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline/borrowed.json");
    let (mut clip, mut probes, mut probe, mut tempo, mut method) = (None, false, None, None, None);
    let (mut seconds, mut tables, mut update, mut compensate) =
        (b::DEFAULT_SECONDS, false, false, true);
    let (mut export, mut only_tools) = (None, Vec::new());
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().copied().ok_or(format!("{arg} needs a value"));
        match *arg {
            "--clip" => clip = Some(value()?.to_string()),
            "--probes" => probes = true,
            "--probe" => {
                let v = value()?;
                probe = Some(Probe::parse(v).ok_or(format!("no probe {v}"))?);
                probes = true;
            }
            "--tempo" => {
                let v = value()?;
                tempo = Some(TempoCase::parse(v).ok_or(format!("no tempo case {v}"))?);
            }
            "--method" => {
                let v = value()?;
                method = Some(Method::parse(v).ok_or(format!("no method {v}"))?);
            }
            "--seconds" => seconds = value()?.parse().map_err(|e| format!("--seconds: {e}"))?,
            "--dir" => dirs.push(value()?.into()),
            "--cache" => cache = value()?.into(),
            "--tool" => {
                let v = value()?;
                only_tools.push(
                    Tool::ALL
                        .into_iter()
                        .find(|t| t.name() == v)
                        .ok_or(format!("no tool {v}"))?,
                );
            }
            "--tables" => tables = true,
            "--export" => export = Some(PathBuf::from(value()?)),
            "--baseline" => baseline = value()?.into(),
            "--update" => update = true,
            "--no-latency-compensation" => compensate = false,
            "--help" | "-h" => {
                println!("{BORROWED_HELP}");
                return Ok(());
            }
            other => return Err(format!("unknown option {other}\n{BORROWED_HELP}")),
        }
    }
    let bin = repo.join("cache/bin");
    let mut references: Vec<Box<dyn Stretcher>> = Vec::new();
    for tool in Tool::ALL {
        if !only_tools.is_empty() && !only_tools.contains(&tool) {
            continue;
        }
        match tool.find(std::slice::from_ref(&bin)) {
            Some(found) => {
                eprintln!("reference {}: {}", tool.name(), found.binary.display());
                references.push(Box::new(found) as Box<dyn Stretcher>);
            }
            None => eprintln!(
                "reference {} not found ({})",
                tool.name(),
                tool.install_hint()
            ),
        }
    }
    let mut pieces = Vec::new();
    for path in b::find_clips(&dirs) {
        if clip
            .as_deref()
            .is_some_and(|c| path.file_stem().and_then(|s| s.to_str()) != Some(c))
        {
            continue;
        }
        pieces.push(b::load_clip(&path, seconds)?);
    }
    if probes {
        for p in probe.map_or(Probe::ALL.to_vec(), |p| vec![p]) {
            pieces.push(b::Piece::probe(p));
        }
    }
    if pieces.is_empty() {
        return Err(
            "no clips: drop audio files into eval/local, run eval/fetch.sh, or pass --probes"
                .into(),
        );
    }
    let options = Options {
        cases: tempo.map_or(TempoCase::all(), |c| vec![c]),
        methods: method.map_or(Method::ALL.to_vec(), |m| vec![m]),
        references,
        cache: RefCache::new(cache),
        compensate_latency: compensate,
        export,
    };
    let (mut rows, mut notes) = (Vec::new(), Vec::new());
    for piece in &pieces {
        eprintln!("{} ({:.1} s)", piece.name, piece.seconds());
        let (r, n) = b::run(piece, &options)?;
        rows.extend(r);
        notes.extend(n);
    }
    if tables {
        print!("{}", b::format_tables(&rows));
        println!();
    }
    print!("{}", b::format_standings(&rows));
    if tables {
        print!("{}", b::format_delays(&notes));
    }
    print!("{}", b::format_notes(&notes));
    let fresh = b::baseline(&rows);
    let old = match std::fs::read_to_string(&baseline) {
        Ok(text) => {
            Baseline::from_json(&text).map_err(|e| format!("{}: {e}", baseline.display()))?
        }
        Err(_) => Baseline::default(),
    };
    println!();
    print!("{}", table::format_changes(&table::diff(&old, &fresh)));
    if update {
        std::fs::write(&baseline, old.merged(fresh).to_json())
            .map_err(|e| format!("{}: {e}", baseline.display()))?;
        println!("wrote {}", baseline.display());
    }
    Ok(())
}

/// Prints the rows, their ranks and the diff against `baseline`, and writes
/// them into it when `update`.
fn report(rows: &[noodle_eval::harness::Row], baseline: &Path, update: bool) -> Result<(), String> {
    print!("{}", table::format_rows(rows));
    println!();
    print!("{}", table::format_ranks(&table::mean_ranks(rows)));
    let fresh = Baseline::of(rows);
    let old = match std::fs::read_to_string(baseline) {
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
        std::fs::write(baseline, merged.to_json())
            .map_err(|e| format!("{}: {e}", baseline.display()))?;
        println!("wrote {}", baseline.display());
    }
    Ok(())
}
