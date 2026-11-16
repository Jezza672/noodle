//! Splits a song with the app's own drum separation (HT-Demucs) and keeps only the drums.
//!
//!     drum-real-split SONG OUT.wav [--model htdemucs.onnx] [--runtime libonnxruntime.dylib]
//!
//! The model and runtime default to the app's data folder on macOS
//! (`~/Library/Application Support/Noodle/models/`), where the Models tab puts them.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use noodle_io::decode_file;
use noodle_stems::{ModelId, OnnxSeparator, Quality, SplitRequest, split};

fn run(args: &[String]) -> Result<(), String> {
    let mut pos = Vec::new();
    let (mut model, mut runtime) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--model" => model = it.next().map(PathBuf::from),
            "--runtime" => runtime = it.next().map(PathBuf::from),
            _ => pos.push(a.clone()),
        }
    }
    let [song, out] = pos.as_slice() else {
        return Err("usage: drum-real-split SONG OUT.wav [--model F] [--runtime F]".into());
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let models = PathBuf::from(home).join("Library/Application Support/Noodle/models");
    let model = model.unwrap_or_else(|| models.join("htdemucs.onnx"));
    let runtime = runtime.unwrap_or_else(|| models.join("onnxruntime-1.22.0/libonnxruntime.dylib"));
    let info = decode_file(Path::new(song)).map_err(|e| format!("{song}: {e}"))?;
    let frames = (info.samples.len() / info.channels.max(1)) as u64;
    drop(info);
    OnnxSeparator::load_runtime(Some(&runtime)).map_err(|e| e.to_string())?;
    let id = ModelId {
        id: "htdemucs".into(),
        version: "local".into(),
    };
    let mut sep = OnnxSeparator::open(&model, Some(&runtime), id).map_err(|e| e.to_string())?;
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let req = SplitRequest {
        source: PathBuf::from(song),
        frames,
        out_dir: dir.path().to_path_buf(),
        name: "s".into(),
        quality: Quality::Better,
    };
    let mut last = 0;
    let made = split(&req, &mut sep, &mut |p| {
        let pct = (p * 100.0) as u32;
        if pct >= last + 10 {
            last = pct;
            eprint!("{pct}% ");
        }
        true
    })
    .map_err(|e| e.to_string())?;
    let drums = made
        .files
        .iter()
        .find(|f| f.file.contains("drums"))
        .ok_or("no drums file")?;
    std::fs::copy(&drums.path, out).map_err(|e| e.to_string())?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("drum-real-split: {e}");
            ExitCode::FAILURE
        }
    }
}
