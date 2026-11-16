//! Finds the drum hits in one audio file with the app's own detector and
//! writes them out for `tools/drum-real/label.py`.
//!
//!     drum-real-detect AUDIO OUT_DIR
//!
//! `OUT_DIR/hits.json` lists every detection (onset, cut, flags, the 36-value
//! feature vector the classifier takes, and the rule labeller's guess);
//! `OUT_DIR/NNNN.wav` is each cut as the app would make it (mono, 16-bit).
//! Hits quieter than the app's cut-off are dropped, as the app drops them.

use std::path::Path;
use std::process::ExitCode;

use noodle_io::{analyse_hits, decode_file, feature_vector, guess, write_wav};
use serde_json::json;

fn run(audio: &Path, out: &Path) -> Result<usize, String> {
    let a = decode_file(audio).map_err(|e| format!("{}: {e}", audio.display()))?;
    let ch = a.channels.max(1);
    let mono: Vec<f32> = a
        .samples
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    let rate = a.sample_rate;
    let hits = analyse_hits(&mono, rate, &mut |_| true).map_err(|_| "cancelled".to_string())?;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let mut list = Vec::new();
    for (i, h) in hits.iter().enumerate() {
        if h.level_db < -45.0 {
            continue;
        }
        let (start, end) = (h.start as usize, (h.end as usize).min(mono.len()));
        if start >= end {
            continue;
        }
        let name = format!("{i:04}.wav");
        write_wav(&out.join(&name), &mono[start..end], 1, rate).map_err(|e| e.to_string())?;
        let (kind, conf) = guess(&h.features);
        let r = f64::from(rate);
        list.push(json!({
            "id": i,
            "wav": name,
            "onset_s": h.onset as f64 / r,
            "start_s": h.start as f64 / r,
            "end_s": end as f64 / r,
            "truncated": h.truncated,
            "mixed": h.mixed,
            "level_db": h.level_db,
            "guess": kind.name(),
            "guess_confidence": conf,
            "features": feature_vector(h).to_vec(),
            "feat": {
                "bands": h.features.bands.to_vec(),
                "centroid": h.features.centroid,
                "flatness": h.features.flatness,
                "low": h.features.split[0],
                "mid": h.features.split[1],
                "high": h.features.split[2],
                "decay": h.features.decay,
                "pitched": h.features.pitched,
                "pitch_key": h.features.pitch_key,
                "attack_peaks": h.features.attack_peaks,
            },
            "pitch_key": h.features.pitch_key,
        }));
    }
    let doc = json!({
        "audio": audio.display().to_string(),
        "sample_rate": rate,
        "duration_s": mono.len() as f64 / f64::from(rate),
        "hits": list,
    });
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(out.join("hits.json"), text).map_err(|e| e.to_string())?;
    Ok(list.len())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [audio, out] = args.as_slice() else {
        eprintln!("usage: drum-real-detect AUDIO OUT_DIR");
        return ExitCode::from(2);
    };
    match run(Path::new(audio), Path::new(out)) {
        Ok(n) => {
            println!("{n} hits");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("drum-real-detect: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_io::{Hit, HitFeatures};

    /// `label.py` reads the named `feat` object and checks it against `features`
    /// (what the classifier takes) in this order; if `feature_vector` changes, this fails.
    #[test]
    fn feature_vector_order_is_what_label_py_expects() {
        let mut bands = [0.0; 24];
        for (i, b) in bands.iter_mut().enumerate() {
            *b = -(i as f32);
        }
        let hit = Hit {
            onset: 0,
            start: 0,
            end: 1,
            truncated: true,
            mixed: true,
            level_db: -3.0,
            features: HitFeatures {
                bands,
                centroid: 1000.0,
                flatness: 0.25,
                split: [0.5, 0.3, 0.2],
                decay: 0.125,
                pitched: 0.75,
                pitch_key: Some(36.5),
                attack_peaks: 3,
            },
        };
        let v = feature_vector(&hit);
        assert_eq!(&v[..24], &bands[..]);
        assert_eq!(
            &v[24..],
            &[
                1000.0, 0.25, 0.5, 0.3, 0.2, 0.125, 0.75, 36.5, 1.0, 3.0, 1.0, 1.0
            ]
        );
    }
}
