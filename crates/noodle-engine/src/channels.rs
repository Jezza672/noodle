//! Channel layouts: which speaker each channel of a multichannel signal feeds,
//! how to pan a source across them and how to convert between layouts.
//!
//! The engine itself treats channels as numbered lanes. A layout is only a
//! naming convention the surround nodes, the exporter and the outputs share,
//! with the channels in the order WAV files use.

use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_2};

/// One speaker position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    FrontLeft,
    FrontRight,
    Center,
    /// The low-frequency effects channel. It has no direction.
    Lfe,
    BackLeft,
    BackRight,
    SideLeft,
    SideRight,
}

impl Speaker {
    /// The short name drawn on a channel.
    pub fn name(self) -> &'static str {
        match self {
            Self::FrontLeft => "L",
            Self::FrontRight => "R",
            Self::Center => "C",
            Self::Lfe => "LFE",
            Self::BackLeft => "Lb",
            Self::BackRight => "Rb",
            Self::SideLeft => "Ls",
            Self::SideRight => "Rs",
        }
    }

    /// The bit this speaker has in a WAV file's channel mask. Channels in a
    /// file go in the order of these bits, which is the order of a layout.
    pub fn wav_bit(self) -> u32 {
        match self {
            Self::FrontLeft => 0x1,
            Self::FrontRight => 0x2,
            Self::Center => 0x4,
            Self::Lfe => 0x8,
            Self::BackLeft => 0x10,
            Self::BackRight => 0x20,
            Self::SideLeft => 0x200,
            Self::SideRight => 0x400,
        }
    }

    fn is_back(self) -> bool {
        matches!(self, Self::BackLeft | Self::BackRight)
    }

    fn is_side(self) -> bool {
        matches!(self, Self::SideLeft | Self::SideRight)
    }

    fn is_left(self) -> bool {
        matches!(self, Self::FrontLeft | Self::BackLeft | Self::SideLeft)
    }

    /// The same-side speaker of the other rear pair (back or side).
    fn other_rear(self) -> Self {
        match self {
            Self::BackLeft => Self::SideLeft,
            Self::BackRight => Self::SideRight,
            Self::SideLeft => Self::BackLeft,
            Self::SideRight => Self::BackRight,
            other => other,
        }
    }

    /// The same-side front speaker.
    fn front(self) -> Self {
        if self.is_left() {
            Self::FrontLeft
        } else {
            Self::FrontRight
        }
    }
}

use Speaker::{BackLeft, BackRight, Center, FrontLeft, FrontRight, Lfe, SideLeft, SideRight};

/// A named arrangement of speakers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    Mono,
    Stereo,
    /// Left, right and centre (3.0).
    Lcr,
    Quad,
    /// 5.1: left, right, centre, LFE, back left and right.
    Surround51,
    /// 7.1: 5.1 with a side pair added to the backs.
    Surround71,
}

/// The most channels any layout has.
pub const MAX_LAYOUT_CHANNELS: usize = 8;

impl ChannelLayout {
    pub const ALL: [ChannelLayout; 6] = [
        Self::Mono,
        Self::Stereo,
        Self::Lcr,
        Self::Quad,
        Self::Surround51,
        Self::Surround71,
    ];

    /// The name shown in the app and stored in a node's `layout` setting.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mono => "Mono",
            Self::Stereo => "Stereo",
            Self::Lcr => "3.0",
            Self::Quad => "Quad",
            Self::Surround51 => "5.1",
            Self::Surround71 => "7.1",
        }
    }

    /// The layout `name` names, ignoring case.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL
            .into_iter()
            .find(|layout| layout.name().eq_ignore_ascii_case(name))
    }

    /// The layout a signal with `channels` channels has, if any. Each channel
    /// count has at most one.
    pub fn from_channels(channels: usize) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|layout| layout.channels() == channels)
    }

    /// The speakers, one per channel, in WAV order.
    pub fn speakers(self) -> &'static [Speaker] {
        match self {
            Self::Mono => &[Center],
            Self::Stereo => &[FrontLeft, FrontRight],
            Self::Lcr => &[FrontLeft, FrontRight, Center],
            Self::Quad => &[FrontLeft, FrontRight, BackLeft, BackRight],
            Self::Surround51 => &[FrontLeft, FrontRight, Center, Lfe, BackLeft, BackRight],
            Self::Surround71 => &[
                FrontLeft, FrontRight, Center, Lfe, BackLeft, BackRight, SideLeft, SideRight,
            ],
        }
    }

    pub fn channels(self) -> usize {
        self.speakers().len()
    }

    /// The WAV file channel mask for this layout.
    pub fn wav_mask(self) -> u32 {
        self.speakers().iter().fold(0, |mask, s| mask | s.wav_bit())
    }

    /// The channel `speaker` plays on, if this layout has it.
    pub fn index_of(self, speaker: Speaker) -> Option<usize> {
        self.speakers().iter().position(|&s| s == speaker)
    }

    /// Where `speaker` stands in this layout, in degrees from straight ahead,
    /// positive to the right. `None` for the LFE, which has no direction.
    pub fn azimuth(self, speaker: Speaker) -> Option<f32> {
        let side = if speaker.is_left() { -1.0 } else { 1.0 };
        Some(match speaker {
            Lfe => return None,
            Center => 0.0,
            FrontLeft | FrontRight => side * 30.0,
            SideLeft | SideRight => side * 90.0,
            BackLeft | BackRight => match self {
                Self::Quad => side * 135.0,
                Self::Surround71 => side * 150.0,
                _ => side * 110.0,
            },
        })
    }

    /// Pans a source to `azimuth` degrees (0 straight ahead, positive to the
    /// right, wrapping at 180) with constant power: the gains of the two
    /// speakers either side of it, or of the nearest one if the layout has no
    /// speaker behind it. `spread` from 0 to 1 blends toward every speaker
    /// playing it equally. The LFE is not panned to. Writes one gain per
    /// channel into `gains`, which must be at least `self.channels()` long.
    pub fn pan_gains(self, azimuth: f32, spread: f32, gains: &mut [f32]) {
        let speakers = self.speakers();
        let gains = &mut gains[..speakers.len()];
        gains.fill(0.0);
        // The panned speakers by azimuth, ascending.
        let mut order = [(0.0f32, 0usize); MAX_LAYOUT_CHANNELS];
        let mut count = 0;
        for (i, &speaker) in speakers.iter().enumerate() {
            if let Some(az) = self.azimuth(speaker) {
                order[count] = (az, i);
                count += 1;
            }
        }
        let order = &mut order[..count];
        order.sort_by(|a, b| a.0.total_cmp(&b.0));

        let az = wrap_degrees(azimuth);
        let behind = order.iter().any(|(az, _)| az.abs() > 90.0);
        let (first, last) = (order[0], order[count - 1]);
        if count == 1 {
            gains[first.1] = 1.0;
        } else if !behind && (az <= first.0 || az >= last.0) {
            // A layout with nothing behind has a hard pan at its edges.
            gains[if az <= first.0 { first.1 } else { last.1 }] = 1.0;
        } else {
            // The pair of neighbours that az falls between, the last and the
            // first wrapping round the back.
            let upper = order.iter().position(|(a, _)| *a > az);
            let (lo, hi, lo_az, hi_az) = match upper {
                Some(0) | None => {
                    let hi_az = if upper.is_none() {
                        first.0 + 360.0
                    } else {
                        first.0
                    };
                    let lo_az = if upper.is_none() {
                        last.0
                    } else {
                        last.0 - 360.0
                    };
                    (last.1, first.1, lo_az, hi_az)
                }
                Some(i) => (order[i - 1].1, order[i].1, order[i - 1].0, order[i].0),
            };
            let t = ((az - lo_az) / (hi_az - lo_az)).clamp(0.0, 1.0);
            gains[lo] = (t * FRAC_PI_2).cos();
            gains[hi] = (t * FRAC_PI_2).sin();
        }
        let spread = spread.clamp(0.0, 1.0);
        if spread > 0.0 {
            let even = spread / count as f32;
            for &(_, i) in order.iter() {
                gains[i] = ((1.0 - spread) * gains[i] * gains[i] + even).sqrt();
            }
        }
    }

    /// The gains that turn a signal in layout `from` into one in layout `to`,
    /// row-major: output channel `o` is the sum over input channels `i` of
    /// `matrix[o * from.channels() + i] * input[i]`.
    ///
    /// A speaker both layouts have passes straight through. The centre of a
    /// layout without one is shared between left and right at -3 dB; a
    /// missing rear pair is folded onto the other rear pair or, if there is
    /// none, onto the front on the same side at -3 dB; stereo into mono sums
    /// at -3 dB each. The LFE is dropped where there is no LFE. Where the
    /// target has speakers the source lacks they stay silent. There is no
    /// normalising: summing several channels can pass full scale, as in the
    /// usual downmix recipes.
    pub fn convert_matrix(from: Self, to: Self) -> Vec<f32> {
        let (rows, cols) = (to.channels(), from.channels());
        let mut matrix = vec![0.0f32; rows * cols];
        let mut add = |source: usize, speaker: Speaker, gain: f32| {
            if let Some(row) = to.index_of(speaker) {
                matrix[row * cols + source] += gain;
            }
        };
        for (source, &speaker) in from.speakers().iter().enumerate() {
            if to.index_of(speaker).is_some() {
                add(source, speaker, 1.0);
                continue;
            }
            match speaker {
                Lfe => {}
                Center => {
                    add(source, FrontLeft, FRAC_1_SQRT_2);
                    add(source, FrontRight, FRAC_1_SQRT_2);
                }
                FrontLeft | FrontRight => add(source, Center, FRAC_1_SQRT_2),
                rear => {
                    debug_assert!(rear.is_back() || rear.is_side());
                    let other = rear.other_rear();
                    if to.index_of(other).is_some() {
                        // At full level if nothing else of the source plays
                        // there, else -3 dB under it.
                        let shared = from.index_of(other).is_some();
                        add(source, other, if shared { FRAC_1_SQRT_2 } else { 1.0 });
                    } else if to.index_of(rear.front()).is_some() {
                        add(source, rear.front(), FRAC_1_SQRT_2);
                    } else {
                        add(source, Center, 0.5);
                    }
                }
            }
        }
        matrix
    }
}

/// `degrees` wrapped into -180..180.
fn wrap_degrees(degrees: f32) -> f32 {
    if !degrees.is_finite() {
        return 0.0;
    }
    // Exactly +180 comes out as -180; both mean straight behind.
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gains(layout: ChannelLayout, azimuth: f32, spread: f32) -> Vec<f32> {
        let mut gains = vec![0.0; layout.channels()];
        layout.pan_gains(azimuth, spread, &mut gains);
        gains
    }

    #[test]
    fn channel_counts_and_masks_follow_wav() {
        assert_eq!(ChannelLayout::ALL.map(|l| l.channels()), [1, 2, 3, 4, 6, 8]);
        assert_eq!(ChannelLayout::Stereo.wav_mask(), 0x3);
        assert_eq!(ChannelLayout::Surround51.wav_mask(), 0x3F);
        assert_eq!(ChannelLayout::Surround71.wav_mask(), 0x63F);
        for layout in ChannelLayout::ALL {
            assert_eq!(
                ChannelLayout::from_channels(layout.channels()),
                Some(layout)
            );
            assert_eq!(ChannelLayout::from_name(layout.name()), Some(layout));
        }
        assert_eq!(ChannelLayout::from_channels(5), None);
        assert_eq!(
            ChannelLayout::from_name(" 5.1 "),
            Some(ChannelLayout::Surround51)
        );
    }

    #[test]
    fn panning_keeps_the_power_everywhere() {
        for layout in ChannelLayout::ALL {
            for spread in [0.0, 0.4, 1.0] {
                for step in -36..=36 {
                    let g = gains(layout, step as f32 * 5.0, spread);
                    let power: f32 = g.iter().map(|x| x * x).sum();
                    assert!(
                        (power - 1.0).abs() < 1e-5,
                        "{layout:?} az {} spread {spread}: {power}",
                        step * 5
                    );
                }
            }
        }
    }

    #[test]
    fn a_source_on_a_speaker_plays_only_there() {
        let g = gains(ChannelLayout::Surround51, 0.0, 0.0);
        assert_eq!(g, [0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
        let g = gains(ChannelLayout::Surround51, 110.0, 0.0);
        assert!((g[5] - 1.0).abs() < 1e-6 && g[..5].iter().all(|x| x.abs() < 1e-6));
        let g = gains(ChannelLayout::Surround71, -90.0, 0.0);
        assert!((g[6] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_source_between_two_speakers_shares_them_and_the_lfe_stays_silent() {
        // Halfway between L (-30) and C (0): equal gains.
        let g = gains(ChannelLayout::Surround51, -15.0, 0.0);
        assert!((g[0] - g[2]).abs() < 1e-6 && (g[0] - FRAC_1_SQRT_2).abs() < 1e-6);
        // Straight behind: the two backs.
        let g = gains(ChannelLayout::Surround51, 180.0, 0.0);
        assert!((g[4] - g[5]).abs() < 1e-5 && (g[4] - FRAC_1_SQRT_2).abs() < 1e-5);
        for step in -36..=36 {
            assert_eq!(
                gains(ChannelLayout::Surround51, step as f32 * 5.0, 0.5)[3],
                0.0
            );
        }
    }

    #[test]
    fn a_front_only_layout_hard_pans_at_its_edges() {
        let g = gains(ChannelLayout::Stereo, 90.0, 0.0);
        assert_eq!(g, [0.0, 1.0]);
        let g = gains(ChannelLayout::Stereo, -170.0, 0.0);
        assert_eq!(g, [1.0, 0.0]);
        let g = gains(ChannelLayout::Stereo, 0.0, 0.0);
        assert!((g[0] - FRAC_1_SQRT_2).abs() < 1e-6 && (g[1] - FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(gains(ChannelLayout::Mono, 77.0, 0.0), [1.0]);
    }

    #[test]
    fn full_spread_plays_every_speaker_equally_except_the_lfe() {
        let g = gains(ChannelLayout::Surround51, 40.0, 1.0);
        let even = (1.0f32 / 5.0).sqrt();
        for (i, x) in g.iter().enumerate() {
            let want = if i == 3 { 0.0 } else { even };
            assert!((x - want).abs() < 1e-6, "{i}: {x}");
        }
    }

    #[test]
    fn a_non_finite_azimuth_is_straight_ahead() {
        assert_eq!(
            gains(ChannelLayout::Surround51, f32::NAN, 0.0),
            gains(ChannelLayout::Surround51, 0.0, 0.0)
        );
    }

    fn apply(from: ChannelLayout, to: ChannelLayout, input: &[f32]) -> Vec<f32> {
        let m = ChannelLayout::convert_matrix(from, to);
        let cols = from.channels();
        (0..to.channels())
            .map(|o| (0..cols).map(|i| m[o * cols + i] * input[i]).sum())
            .collect()
    }

    #[test]
    fn converting_to_the_same_layout_is_the_identity() {
        for layout in ChannelLayout::ALL {
            let n = layout.channels();
            let m = ChannelLayout::convert_matrix(layout, layout);
            for o in 0..n {
                for i in 0..n {
                    assert_eq!(m[o * n + i], if o == i { 1.0 } else { 0.0 });
                }
            }
        }
    }

    #[test]
    fn downmixing_5_1_to_stereo_follows_the_usual_recipe() {
        // L R C LFE Lb Rb
        let out = apply(
            ChannelLayout::Surround51,
            ChannelLayout::Stereo,
            &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0],
        );
        let k = FRAC_1_SQRT_2;
        assert!((out[0] - (1.0 + 4.0 * k + 16.0 * k)).abs() < 1e-5);
        assert!((out[1] - (2.0 + 4.0 * k + 32.0 * k)).abs() < 1e-5);
    }

    #[test]
    fn upmixing_stereo_leaves_the_rest_silent_and_mono_goes_to_the_centre() {
        let out = apply(
            ChannelLayout::Stereo,
            ChannelLayout::Surround51,
            &[1.0, 2.0],
        );
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0, 0.0, 0.0]);
        let out = apply(ChannelLayout::Mono, ChannelLayout::Surround51, &[3.0]);
        assert_eq!(out, [0.0, 0.0, 3.0, 0.0, 0.0, 0.0]);
        let out = apply(ChannelLayout::Mono, ChannelLayout::Stereo, &[1.0]);
        assert!((out[0] - FRAC_1_SQRT_2).abs() < 1e-6 && out[0] == out[1]);
    }

    #[test]
    fn rear_pairs_fold_onto_each_other() {
        // 7.1 into 5.1: the back stays and the side joins it at -3 dB.
        let out = apply(
            ChannelLayout::Surround71,
            ChannelLayout::Surround51,
            &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
        );
        assert!((out[4] - (1.0 + FRAC_1_SQRT_2)).abs() < 1e-6);
        // 5.1 into 7.1: the backs stay the backs.
        let out = apply(
            ChannelLayout::Surround51,
            ChannelLayout::Surround71,
            &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        );
        assert_eq!(out[4], 1.0);
        assert_eq!(out[6], 0.0);
        // Quad into 3.0: the backs fold to the front on their side.
        let out = apply(
            ChannelLayout::Quad,
            ChannelLayout::Lcr,
            &[0.0, 0.0, 1.0, 0.0],
        );
        assert!((out[0] - FRAC_1_SQRT_2).abs() < 1e-6 && out[1] == 0.0);
    }
}
