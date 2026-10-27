//! The playback cache's panel: the switch, how full the cache is, how much it
//! has dropped to stay under its limit, and how often the output ran dry.
//!
//! Reading the cache folder takes a directory scan, so the panel does it when
//! it opens, on Refresh and every couple of seconds while open, never every
//! frame.

use std::time::{Duration, Instant};

use egui::Ui;

use crate::freezing::CacheReadout;
use crate::session::{CacheStatus, Session};
use crate::theme;

/// How often the open panel looks at the folder again.
const REFRESH: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct CachePanel {
    open: bool,
    readout: Option<Result<CacheReadout, String>>,
    read_at: Option<Instant>,
}

impl CachePanel {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.read_at = None;
    }

    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        if self.read_at.is_none_or(|at| at.elapsed() >= REFRESH) {
            self.readout = Some(session.cache_readout());
            self.read_at = Some(Instant::now());
        }
        ctx.request_repaint_after(REFRESH);
        let mut open = true;
        egui::Window::new("Playback cache")
            .open(&mut open)
            .default_width(340.0)
            .resizable(false)
            .show(ctx, |ui| self.contents(ui, session));
        self.open = open;
    }

    fn contents(&mut self, ui: &mut Ui, session: &mut Session) {
        let mut on = session.auto_cache();
        if ui
            .checkbox(&mut on, "Cache playback automatically")
            .on_hover_text(
                "Groups and tracks are rendered in the background, and replaying a \
                 pass then plays the audio instead of computing it. Turning it off \
                 plays everything live. Frozen nodes are not affected.",
            )
            .changed()
        {
            session.set_auto_cache(on);
            crate::prefs::remember_auto_cache(on);
        }
        ui.add_space(4.0);
        let (text, colour) = status_line(session.cache_status());
        ui.colored_label(colour, text);
        ui.add_space(4.0);
        match &self.readout {
            Some(Ok(readout)) => {
                ui.label(usage_line(readout));
                if readout.evicted > 0 {
                    ui.label(format!(
                        "{} dropped, least recently used first",
                        bytes_label(readout.evicted)
                    ));
                }
            }
            Some(Err(why)) => {
                ui.colored_label(theme::WARNING, why);
            }
            None => {}
        }
        match session.underruns() {
            Some(0) => {
                ui.label("No underruns since playback started");
            }
            Some(n) => {
                ui.colored_label(
                    theme::WARNING,
                    format!(
                        "{n} {} since playback started",
                        if n == 1 { "underrun" } else { "underruns" }
                    ),
                );
            }
            None => {
                ui.weak("The audio engine is off");
            }
        }
        ui.add_space(4.0);
        if ui.button("Refresh").clicked() {
            self.read_at = None;
        }
    }
}

/// What the indicator and the panel say about where the sound comes from.
pub fn status_line(status: CacheStatus) -> (String, egui::Color32) {
    match status {
        CacheStatus::Off => (
            "Off: everything plays live".into(),
            theme::timeline::TEXT_WEAK,
        ),
        CacheStatus::Idle => (
            "Nothing to cache: needs the engine on and a group or track".into(),
            theme::timeline::TEXT_WEAK,
        ),
        CacheStatus::Live => ("Playing live".into(), theme::timeline::TEXT),
        CacheStatus::Rendering(fraction) => (
            format!("Playing live, caching ({:.0}%)", fraction * 100.0),
            theme::timeline::TEXT,
        ),
        CacheStatus::Cached => ("Playing from the cache".into(), theme::ACCENT),
    }
}

/// "12 entries, 1.2 GB of 4.0 GB".
pub fn usage_line(readout: &CacheReadout) -> String {
    format!(
        "{} {}, {} of {}",
        readout.entries,
        if readout.entries == 1 {
            "entry"
        } else {
            "entries"
        },
        bytes_label(readout.bytes),
        bytes_label(readout.limit),
    )
}

/// A size in bytes, to one decimal place in the largest unit that fits.
pub fn bytes_label(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_given_in_the_largest_unit_that_fits() {
        assert_eq!(bytes_label(0), "0 B");
        assert_eq!(bytes_label(1023), "1023 B");
        assert_eq!(bytes_label(1536), "1.5 KB");
        assert_eq!(bytes_label(5 << 20), "5.0 MB");
        assert_eq!(bytes_label(4 << 30), "4.0 GB");
    }

    #[test]
    fn the_usage_line_says_how_full_the_cache_is() {
        let readout = CacheReadout {
            entries: 1,
            bytes: 3 << 20,
            limit: 4 << 30,
            evicted: 0,
        };
        assert_eq!(usage_line(&readout), "1 entry, 3.0 MB of 4.0 GB");
    }
}
