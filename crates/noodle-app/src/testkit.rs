//! The user actions every UI test performs, in one place: pointer drags and
//! clicks, double clicks, key presses, the wheel and dropped files.
//!
//! They send the events a real pointer and keyboard would, a frame at a
//! time, so a test exercises the same code paths a person does. Each is
//! generic over the harness's state, so any view's rig can use them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2};
use egui_kittest::Harness;
use noodle_io::{AudioConfig, VIRTUAL_OUTPUT};

/// Presses `button` at the first point, moves through the rest in small
/// steps, and releases at the last, holding `modifiers` throughout. It stops
/// after the release frame: callers settle the UI themselves.
fn gesture<S>(h: &mut Harness<'_, S>, button: PointerButton, modifiers: Modifiers, path: &[Pos2]) {
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerMoved(path[0]));
    h.step();
    h.event(Event::PointerButton {
        pos: path[0],
        button,
        pressed: true,
        modifiers,
    });
    h.step();
    for &p in &path[1..] {
        h.event(Event::PointerMoved(p));
        h.step();
    }
    h.event(Event::PointerButton {
        pos: *path.last().unwrap(),
        button,
        pressed: false,
        modifiers,
    });
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
}

/// [`gesture`], then runs until the UI settles.
pub fn drag_with<S>(
    h: &mut Harness<'_, S>,
    button: PointerButton,
    modifiers: Modifiers,
    path: &[Pos2],
) {
    gesture(h, button, modifiers, path);
    h.run();
}

/// A primary-button drag that settles in a fixed three frames, for apps that
/// keep repainting while audio plays, where `run` would never finish.
pub fn drag_playing<S>(h: &mut Harness<'_, S>, path: &[Pos2]) {
    gesture(h, PointerButton::Primary, Modifiers::NONE, path);
    h.run_steps(3);
}

/// A primary-button drag along `path` with `modifiers` held.
pub fn drag<S>(h: &mut Harness<'_, S>, modifiers: Modifiers, path: &[Pos2]) {
    drag_with(h, PointerButton::Primary, modifiers, path);
}

/// A drag from `from` to `to` in six equal steps.
pub fn drag_between<S>(h: &mut Harness<'_, S>, modifiers: Modifiers, from: Pos2, to: Pos2) {
    let path: Vec<Pos2> = (0..=6)
        .map(|i| from + (to - from) * (i as f32 / 6.0))
        .collect();
    drag(h, modifiers, &path);
}

/// Presses and releases `button` at `pos`, holding `modifiers`.
pub fn click_with<S>(
    h: &mut Harness<'_, S>,
    button: PointerButton,
    modifiers: Modifiers,
    pos: Pos2,
) {
    drag_with(h, button, modifiers, &[pos]);
}

/// A primary-button click at `pos`.
pub fn click<S>(h: &mut Harness<'_, S>, pos: Pos2, modifiers: Modifiers) {
    click_with(h, PointerButton::Primary, modifiers, pos);
}

/// Two primary clicks at `at`, each in its own frame, as a person's are.
pub fn double_click<S>(h: &mut Harness<'_, S>, at: Pos2) {
    h.event(Event::PointerMoved(at));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        h.step();
    }
    h.run();
}

/// Presses `key` with the pointer over `at`, holding `modifiers`.
pub fn press<S>(h: &mut Harness<'_, S>, at: Pos2, modifiers: Modifiers, key: Key) {
    h.hover_at(at);
    h.step();
    h.key_press_modifiers(modifiers, key);
    h.run();
}

/// One wheel or trackpad event over `at`. It steps once rather than running,
/// because egui keeps repainting while it smooths a scroll.
pub fn wheel<S>(h: &mut Harness<'_, S>, at: Pos2, unit: MouseWheelUnit, delta: Vec2) {
    wheel_with(h, at, unit, delta, Modifiers::NONE);
}

/// [`wheel`] holding `modifiers`.
pub fn wheel_with<S>(
    h: &mut Harness<'_, S>,
    at: Pos2,
    unit: MouseWheelUnit,
    delta: Vec2,
    modifiers: Modifiers,
) {
    h.hover_at(at);
    h.event(Event::MouseWheel {
        unit,
        delta,
        phase: TouchPhase::Move,
        modifiers,
    });
    h.step();
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not needed".into())
    }
}

/// Drops `files` from the desktop with the pointer at `at`.
pub fn drop_files<S>(h: &mut Harness<'_, S>, at: Pos2, files: &[PathBuf]) {
    h.event(Event::PointerMoved(at));
    h.step();
    for file in files {
        h.input_mut()
            .dropped_files
            .push(Arc::new(Dropped(file.clone())));
    }
    h.step();
    h.run();
}

/// Audio settings that play on the virtual output: the engine runs in real
/// time on every platform and nothing is heard.
pub fn virtual_audio() -> AudioConfig {
    AudioConfig {
        output: Some(VIRTUAL_OUTPUT.into()),
        ..AudioConfig::default()
    }
}

/// A harness that draws one view with `ui` over `state`, at `size`, and has
/// settled its first frames.
pub fn harness<S>(
    state: S,
    size: Vec2,
    ui: impl FnMut(&mut egui::Ui, &mut S) + 'static,
) -> Harness<'static, S> {
    let mut h = Harness::builder().with_size(size).build_ui_state(ui, state);
    h.run();
    h
}
