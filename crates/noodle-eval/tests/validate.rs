//! The validation tool, end to end on files whose "listener scores" are made
//! up to fall as the damage grows.

use noodle_eval::validate;
use noodle_io::write_wav;
use std::path::Path;

const RATE: u32 = 22_050;

fn noise(n: usize, seed: &mut u32) -> Vec<f32> {
    (0..n)
        .map(|_| {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (*seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        })
        .collect()
}

/// Two speeds, three noise levels each. Listeners score by noise within a
/// speed, but score everything at the fast speed lower, so the pooled
/// correlation is weaker than the within-speed one.
fn write_corpus(dir: &Path, mos_of: impl Fn(usize, usize) -> f64) -> std::path::PathBuf {
    let mut seed = 5u32;
    let mut manifest = String::from("reference,test,method,speed,mos\n");
    for (si, speed) in [1.0f64, 2.0].iter().enumerate() {
        for ni in 0..3 {
            let reference: Vec<f32> = (0..RATE as usize * 4)
                .map(|n| (n as f32 * 0.07 * (1.0 + ni as f32 * 0.05)).sin() * 0.4)
                .collect();
            // A steady tone stretched is the same tone for a different time.
            let len = (reference.len() as f64 / speed) as usize;
            let hiss = noise(len, &mut seed);
            let test: Vec<f32> = reference[..len]
                .iter()
                .zip(&hiss)
                .map(|(s, h)| s + h * ni as f32 * 0.08)
                .collect();
            let name = format!("{si}{ni}");
            write_wav(&dir.join(format!("r{name}.wav")), &reference, 1, RATE).unwrap();
            write_wav(&dir.join(format!("t{name}.wav")), &test, 1, RATE).unwrap();
            manifest += &format!("r{name}.wav,t{name}.wav,noisy,{speed},{}\n", mos_of(si, ni));
        }
    }
    let path = dir.join("manifest.csv");
    std::fs::write(&path, manifest).unwrap();
    path
}

fn find<'a>(c: &'a [validate::Correlation], name: &str) -> &'a validate::Correlation {
    c.iter().find(|c| c.metric == name).unwrap()
}

#[test]
fn metrics_that_track_the_listeners_are_usable() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_corpus(dir.path(), |s, n| 5.0 - n as f64 - 1.5 * s as f64);
    let rows = validate::parse_manifest(&path).unwrap();
    assert_eq!(rows.len(), 6);
    let result = validate::run(&rows).unwrap();
    let c = find(&result, "log_distance_db");
    assert!((c.within_speed_spearman + 1.0).abs() < 1e-9, "{c:?}");
    assert!(
        c.pooled_spearman < -0.5 && c.pooled_spearman > c.within_speed_spearman,
        "{c:?}"
    );
    assert!(c.usable(), "{c:?}");
}

#[test]
fn a_metric_that_tracks_in_the_wrong_direction_shows_the_sign() {
    let dir = tempfile::tempdir().unwrap();
    // Listeners score the noisiest best.
    let path = write_corpus(dir.path(), |s, n| 1.0 + n as f64 - 1.5 * s as f64);
    let result = validate::run(&validate::parse_manifest(&path).unwrap()).unwrap();
    let c = find(&result, "log_distance_db");
    assert!(c.within_speed_spearman > 0.9, "{c:?}");
    // Tracking in either direction counts as usable; the sign says which.
    assert!(c.usable(), "{c:?}");
}

#[test]
fn unrelated_scores_leave_a_metric_unusable() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_corpus(dir.path(), |s, n| [3.0, 1.0, 2.0, 1.0, 3.0, 2.0][s * 3 + n]);
    let result = validate::run(&validate::parse_manifest(&path).unwrap()).unwrap();
    assert!(!find(&result, "log_distance_db").usable());
}

#[test]
fn a_bad_manifest_header_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.csv");
    std::fs::write(&path, "a,b,c\n1,2,3\n").unwrap();
    assert!(validate::parse_manifest(&path).is_err());
}
