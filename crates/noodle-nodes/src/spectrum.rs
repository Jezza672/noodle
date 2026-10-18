//! The Spectrum node: shows its input's frequency content in the UI.
//!
//! The audio thread only sends samples, as the Scope does (the same
//! telemetry channel); the analysis happens on the UI thread, in
//! [`SpectrumAnalyser`], so it costs the graph nothing.

use std::f32::consts::TAU;
use std::sync::Arc;

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ScopeWriter, Setup,
    Telemetry,
};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::meter::voice_sum;

/// Sends its input's samples, voices summed, to the UI, which draws their
/// spectrum (a curve, and a spectrogram in the properties panel).
pub struct Spectrum {
    telemetry: Telemetry,
}

impl Spectrum {
    pub fn new(telemetry: &Telemetry) -> Self {
        Self {
            telemetry: telemetry.clone(),
        }
    }
}

/// The Spectrum's type ID.
pub const SPECTRUM_ID: &str = "noodle.view.spectrum";

const IN: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: SPECTRUM_ID,
    version: 1,
    name: "Spectrum",
    category: "Views",
};

impl NodeType for Spectrum {
    fn always_runs(&self) -> bool {
        true
    }

    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        // Room for a window and the blocks that arrive between two UI frames.
        let capacity = (WINDOW * 2).max(setup.max_frames);
        let channels = setup.input_shapes[IN].channels;
        Ok(Instance::realtime(SpectrumNode {
            writer: self.telemetry.open_scope(setup.node, channels, capacity),
        }))
    }
}

struct SpectrumNode {
    writer: ScopeWriter,
}

impl Node for SpectrumNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let input = &io.inputs[IN];
        self.writer.write(ctx.frames, |frame, channel| {
            voice_sum(input, channel, frame)
        });
    }
}

/// How many samples one analysis looks at: about 85 ms at 48 kHz, with bins
/// 11.7 Hz apart.
pub const WINDOW: usize = 4096;
/// The lowest and highest frequencies shown, in Hz. The top is cut at the
/// Nyquist frequency when the sample rate is low.
pub const LOW_HZ: f32 = 20.0;
pub const HIGH_HZ: f32 = 20_000.0;
/// What silence reads as, in dB; also the bottom of the display.
pub const FLOOR_DB: f32 = -90.0;

/// Turns the latest [`WINDOW`] samples into decibels per frequency bin and
/// per log-spaced display column.
pub struct SpectrumAnalyser {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// Scales a bin's magnitude so that a full-scale sine reads 0 dB.
    scale: f32,
    time: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// The latest result: dB per bin, `WINDOW / 2 + 1` of them.
    bins: Vec<f32>,
}

impl Default for SpectrumAnalyser {
    fn default() -> Self {
        Self::new()
    }
}

impl SpectrumAnalyser {
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW);
        let window: Vec<f32> = (0..WINDOW)
            .map(|n| 0.5 - 0.5 * (TAU * n as f32 / WINDOW as f32).cos())
            .collect();
        // A sine of amplitude A peaks at A * sum(window) / 2.
        let scale = 2.0 / window.iter().sum::<f32>();
        Self {
            time: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            bins: vec![FLOOR_DB; WINDOW / 2 + 1],
            fft,
            window,
            scale,
        }
    }

    /// Analyses the end of `samples` (mono, oldest first). Fewer than
    /// [`WINDOW`] samples are placed at the end of the window, ahead of
    /// silence. Returns the dB of each bin, bin `k` being at
    /// `k * sample_rate / WINDOW` Hz.
    pub fn analyse(&mut self, samples: &[f32]) -> &[f32] {
        let used = samples.len().min(WINDOW);
        let latest = &samples[samples.len() - used..];
        self.time.fill(0.0);
        let start = WINDOW - used;
        for (k, (out, &x)) in self.time[start..].iter_mut().zip(latest).enumerate() {
            *out = if x.is_finite() { x } else { 0.0 } * self.window[start + k];
        }
        let ok = self
            .fft
            .process_with_scratch(&mut self.time, &mut self.spectrum, &mut self.scratch)
            .is_ok();
        for (db, bin) in self.bins.iter_mut().zip(&self.spectrum) {
            *db = if ok {
                (20.0 * (bin.norm() * self.scale).max(1e-9).log10()).max(FLOOR_DB)
            } else {
                FLOOR_DB
            };
        }
        &self.bins
    }
}

/// The frequency of display column `column` of `columns`, log-spaced from
/// [`LOW_HZ`] to the top shown at `sample_rate`.
pub fn column_frequency(column: usize, columns: usize, sample_rate: f32) -> f32 {
    let high = HIGH_HZ.min(sample_rate * 0.5);
    let t = column as f32 / columns.saturating_sub(1).max(1) as f32;
    LOW_HZ * (high / LOW_HZ).powf(t)
}

/// Fills `columns` with the spectrum on a log frequency axis: where a column
/// spans several bins it takes the loudest, and where it falls between two
/// it interpolates, so low notes aren't drawn as steps.
pub fn columns_from_bins(bins: &[f32], sample_rate: f32, columns: &mut [f32]) {
    let n = columns.len();
    let bin_hz = sample_rate / WINDOW as f32;
    // How far a column's neighbours are, as a frequency ratio.
    let step = (HIGH_HZ.min(sample_rate * 0.5) / LOW_HZ).powf(1.0 / n.saturating_sub(1).max(1) as f32);
    let half = step.sqrt();
    let last = bins.len() - 1;
    for (k, out) in columns.iter_mut().enumerate() {
        let f = column_frequency(k, n, sample_rate);
        let (lo, hi) = (f / half / bin_hz, f * half / bin_hz);
        *out = if hi - lo < 1.0 {
            let at = (f / bin_hz).min(last as f32);
            let (i, frac) = (at.floor() as usize, at.fract());
            let next = (i + 1).min(last);
            bins[i] + (bins[next] - bins[i]) * frac
        } else {
            let (a, b) = (lo.ceil() as usize, (hi.floor() as usize).min(last));
            bins[a.min(last)..=b.max(a.min(last))]
                .iter()
                .copied()
                .fold(FLOOR_DB, f32::max)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{NodeId, ScopeView, Shape};

    const RATE: f32 = 48_000.0;

    fn sine(hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|n| amplitude * (TAU * hz * n as f32 / RATE).sin())
            .collect()
    }

    fn loudest(columns: &[f32]) -> usize {
        let mut best = 0;
        for (k, &db) in columns.iter().enumerate() {
            if db > columns[best] {
                best = k;
            }
        }
        best
    }

    #[test]
    fn a_full_scale_sine_peaks_near_zero_db_at_its_frequency() {
        let mut analyser = SpectrumAnalyser::new();
        let bins = analyser.analyse(&sine(1000.0, 1.0, WINDOW)).to_vec();
        let peak = (0..bins.len())
            .max_by(|&a, &b| bins[a].total_cmp(&bins[b]))
            .unwrap();
        assert!(((peak as f32 * RATE / WINDOW as f32) - 1000.0).abs() < 12.0);
        assert!(bins[peak] > -1.6 && bins[peak] <= 0.1, "{}", bins[peak]);
        // A sine half as loud is 6 dB lower.
        let half = analyser.analyse(&sine(1000.0, 0.5, WINDOW))[peak];
        assert!((bins[peak] - half - 6.02).abs() < 0.1, "{half}");
    }

    #[test]
    fn silence_reads_as_the_floor_and_bad_samples_are_ignored() {
        let mut analyser = SpectrumAnalyser::new();
        assert!(
            analyser
                .analyse(&[0.0; 100])
                .iter()
                .all(|&db| db == FLOOR_DB)
        );
        assert!(analyser.analyse(&[]).iter().all(|&db| db == FLOOR_DB));
        let mut noisy = sine(440.0, 1.0, WINDOW);
        noisy[10] = f32::NAN;
        noisy[20] = f32::INFINITY;
        assert!(analyser.analyse(&noisy).iter().all(|db| db.is_finite()));
    }

    #[test]
    fn the_columns_put_a_tone_where_its_frequency_is_on_a_log_axis() {
        let mut analyser = SpectrumAnalyser::new();
        let mut columns = vec![0.0; 128];
        for hz in [60.0, 440.0, 3000.0, 12_000.0] {
            let bins = analyser.analyse(&sine(hz, 1.0, WINDOW)).to_vec();
            columns_from_bins(&bins, RATE, &mut columns);
            let at = column_frequency(loudest(&columns), columns.len(), RATE);
            let semitones = 12.0 * (at / hz).log2();
            assert!(semitones.abs() < 1.0, "{hz} Hz drawn at {at} Hz");
            assert!(columns.iter().copied().fold(f32::MIN, f32::max) > -3.0);
        }
        assert_eq!(column_frequency(0, 128, RATE), LOW_HZ);
        assert!((column_frequency(127, 128, RATE) - HIGH_HZ).abs() < 1.0);
        // The top follows the Nyquist frequency at a low sample rate.
        assert!((column_frequency(127, 128, 22_050.0) - 11_025.0).abs() < 1.0);
    }

    #[test]
    fn the_node_sends_its_samples_with_voices_summed() {
        let telemetry = Telemetry::new();
        let mut h = Harness::new(
            &Spectrum::new(&telemetry),
            &Config::new(),
            &[(IN, Shape::new(2, 1))],
            RATE,
            3,
        )
        .unwrap();
        let mut input = h.input(IN, 3);
        input.lane_mut(0, 0).copy_from_slice(&[1.0, 2.0, 3.0]);
        input.lane_mut(1, 0).copy_from_slice(&[10.0, 20.0, 30.0]);
        h.run(3).unwrap();
        let mut view = ScopeView::default();
        assert!(telemetry.read_scope(NodeId(0), &mut view));
        assert_eq!(view.samples(), [11.0, 22.0, 33.0]);
    }
}
