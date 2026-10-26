use std::path::Path;
use std::process::Command;

fn noodle(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_noodle"))
        .args(args)
        .output()
        .unwrap()
}

/// `noodle` says how far a render has got on stderr whenever it takes longer
/// than one of its 20 ms polls, which a short render on a busy machine can.
/// That line is not a complaint.
fn without_progress(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !line.starts_with("rendering: "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn renders_a_project_to_wav() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sine.ron");
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sine.wav");
    let result = noodle(&[
        "render",
        project.to_str().unwrap(),
        output.to_str().unwrap(),
        "--seconds",
        "0.1",
    ]);
    assert!(result.status.success(), "{result:?}");
    let stderr = without_progress(&result.stderr);
    assert!(stderr.is_empty(), "{result:?}");

    let audio = noodle_io::read_wav(&output).unwrap();
    assert_eq!((audio.channels, audio.sample_rate), (2, 48_000));
    assert_eq!(audio.samples.len(), 4_800 * 2);
    assert!(audio.samples.iter().any(|&x| x != 0.0));
}

#[test]
fn reports_a_missing_project() {
    let result = noodle(&["render", "no-such-project.ron", "out.wav"]);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("can't read no-such-project.ron"),
        "{stderr}"
    );
}

#[test]
fn rejects_a_render_too_long_to_count() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sine.ron");
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("never.wav");
    let result = noodle(&[
        "render",
        project.to_str().unwrap(),
        output.to_str().unwrap(),
        "--seconds",
        "1e300",
    ]);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("seconds: too long"), "{stderr}");
    assert!(!output.exists());
}

#[test]
fn play_reports_a_missing_project() {
    let result = noodle(&["play", "no-such-project.ron"]);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("can't read no-such-project.ron"),
        "{stderr}"
    );
}
