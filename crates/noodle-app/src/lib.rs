//! The Noodle application, as a library: everything that takes an
//! `egui::Context`. Two shells put it in a window: the desktop shell
//! (`main.rs`, on `eframe`) and the plugin shell (`noodle-plugin`).
//!
//! The shells own the window, the process-wide wiring (the repaint waker,
//! preferences, autosave) and the engine's start; the app proper holds the
//! session and draws it.

#[cfg(test)]
mod acceptance;
pub mod app;
pub mod autosave;
mod browser;
mod cache_panel;
mod carry;
mod clip_editor;
mod clip_panel;
mod clipboard;
mod devices;
mod dialog;
mod display;
mod drum_rack_dialog;
mod editor;
mod export_dialog;
mod freezing;
mod inline_result;
mod keyboard;
mod latency;
mod looper_panel;
mod metronome;
mod midi_ports;
mod midi_take;
mod mixer;
mod models;
mod morph_panel;
mod outputs;
mod pattern_panel;
mod piano_roll;
mod picker;
mod pill_buttons;
mod plugin_browser;
pub mod prefs;
mod properties;
mod sample_drop;
mod sample_nodes;
mod sample_set_dialog;
mod sample_view;
pub mod session;
mod settings;
mod snap;
mod sounds;
mod stems_dialog;
#[cfg(test)]
mod stories;
mod tempo;
mod tempo_source;
#[cfg(test)]
mod testkit;
pub mod theme;
mod time_axis;
mod timeline;
mod toolbar;
mod transport_widgets;
mod widgets;
