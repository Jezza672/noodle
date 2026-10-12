//! The validation tool, end to end on files whose "listener scores" are made
//! up to fall as the damage grows.

use noodle_eval::validate;
use noodle_io::write_wav;

const RATE: u32 = 22_050;

#[test]
fn damage_that_listeners_would_hear_correlates_with_the_spectral_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let mut seed = 5u32;
    let mut noise = |n: usize| -> Vec<f32> {
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    };
    let mut manifest = String::from("reference,test,method,speed,mos\n");
    for (i, speed) in [1.0f64, 1.0, 1.0, 1.0, 1.0, 1.0].iter().enumerate() {
        let reference: Vec<f32> = (0..RATE as usize * 3)
            .map(|n| (n as f32 * 0.07 * (1.0 + i as f32 * 0.1)).sin() * 0.4)
            .collect();
        // Damage grows with the file's number: more and more noise.
        let amount = i as f32 * 0.05;
        let hiss = noise(reference.len());
        let test: Vec<f32> = reference
            .iter()
            .zip(&hiss)
            .map(|(s, h)| s + h * amount)
            .collect();
        write_wav(&dir.path().join(format!("r{i}.wav")), &reference, 1, RATE).unwrap();
        write_wav(&dir.path().join(format!("t{i}.wav")), &test, 1, RATE).unwrap();
        manifest += &format!("r{i}.wav,t{i}.wav,noisy,{speed},{}\n", 5.0 - i as f64 * 0.8);
    }
    let path = dir.path().join("manifest.csv");
    std::fs::write(&path, manifest).unwrap();
    let rows = validate::parse_manifest(&path).unwrap();
    assert_eq!(rows.len(), 6);
    let result = validate::run(&rows).unwrap();
    let c = result
        .iter()
        .find(|c| c.metric == "log_distance_db")
        .unwrap();
    assert!(
        c.pooled_spearman < -0.9,
        "worse sound, higher distance: {c:?}"
    );
    assert!(c.usable() || c.within_speed_spearman.is_nan() || c.within_speed_spearman.abs() >= 0.5);
}

#[test]
fn a_bad_manifest_header_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.csv");
    std::fs::write(&path, "a,b,c\n1,2,3\n").unwrap();
    assert!(validate::parse_manifest(&path).is_err());
}
