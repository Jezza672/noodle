use std::path::Path;
use std::process::Command;

fn noodle(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_noodle"))
        .args(args)
        .output()
        .unwrap()
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
    assert!(result.stderr.is_empty(), "{result:?}");

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
