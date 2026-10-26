//! Surround and multichannel nodes: Surround Pan places a source among the
//! speakers of a layout, Convert Layout changes between layouts, and Split
//! and Merge Channels take a multichannel wire apart and put one together.
//!
//! The channel order of every layout is the one WAV files use (see
//! [`ChannelLayout`]), so a 5.1 bus is L, R, C, LFE, Lb, Rb.

use noodle_engine::{
    ChannelLayout, Config, ConfigInfo, Context, Instance, Io, Layout, MAX_LAYOUT_CHANNELS, Node,
    NodeError, NodeInfo, NodeType, ParamInfo, Ports, Setup, Shape, SignalIn, Unit, Value,
};

/// The config key naming a layout (`"5.1"`, `"Stereo"`, ...).
pub const LAYOUT_KEY: &str = "layout";

const LAYOUT: ConfigInfo = ConfigInfo::text(LAYOUT_KEY, "Layout");

/// The layout a node's `layout` setting names; empty means `default`.
fn config_layout(config: &Config, default: ChannelLayout) -> Result<ChannelLayout, NodeError> {
    let name = LAYOUT.get_text(config);
    if name.trim().is_empty() {
        return Ok(default);
    }
    ChannelLayout::from_name(&name).ok_or_else(|| {
        NodeError::config(format!(
            "unknown channel layout \"{name}\" (use one of {})",
            ChannelLayout::ALL.map(|l| l.name()).join(", ")
        ))
    })
}

/// Whether every channel of `voice` in `input` is known to be silent.
fn voice_silent(input: &SignalIn<'_>, voice: usize) -> bool {
    (0..input.shape().channels).all(|c| input.is_silent(voice, c))
}

// ---------------------------------------------------------------------------
// Surround Pan

/// Places a signal among the speakers of a layout. The input's channels are
/// averaged to one source, which is panned by `azimuth` (degrees from straight
/// ahead, positive to the right, wrapping at ±180) with constant power between
/// the two speakers either side of it. `spread` blends toward every speaker
/// playing the source equally, and `lfe` sends some of it to the LFE channel
/// of a layout that has one (off at its minimum). A wire into `azimuth` moves
/// it along its range, so a full-scale LFO circles the listener. It works per
/// voice, and every parameter may be a polyphonic signal.
pub struct SurroundPan;

pub const SURROUND_PAN_ID: &str = "noodle.util.surround_pan";

#[derive(Ports)]
#[ports(memoryless)]
struct PanPorts {
    #[input("in", "In")]
    input: (),
    #[param("azimuth", "Azimuth", ParamInfo::new(-180.0, 180.0, 0.0).unit(Unit::Degrees).offset())]
    azimuth: (),
    #[param("spread", "Spread", ParamInfo::new(0.0, 1.0, 0.0))]
    spread: (),
    #[param("lfe", "LFE send", ParamInfo::new(-60.0, 12.0, -60.0).unit(Unit::Decibels))]
    lfe: (),
    #[output("out", "Out")]
    out: (),
}

static PAN_INFO: NodeInfo = NodeInfo {
    id: SURROUND_PAN_ID,
    version: 1,
    name: "Surround Pan",
    category: "Utilities",
};

static PAN_CONFIG: [ConfigInfo; 1] = [LAYOUT];

/// The layout Surround Pan uses when none is set.
const PAN_DEFAULT: ChannelLayout = ChannelLayout::Surround51;

/// The layout a node of type `type_id` has when its `layout` setting is
/// empty, if the type has such a setting.
pub fn default_layout(type_id: &str) -> Option<ChannelLayout> {
    match type_id {
        SURROUND_PAN_ID => Some(PAN_DEFAULT),
        CONVERT_LAYOUT_ID => Some(CONVERT_DEFAULT),
        _ => None,
    }
}

impl NodeType for SurroundPan {
    fn view(&self, _config: &Config) -> noodle_engine::NodeView {
        noodle_engine::NodeView::Panner
    }

    fn info(&self) -> &NodeInfo {
        &PAN_INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &PAN_CONFIG
    }

    fn initial_config(&self) -> Config {
        Config::new().with(LAYOUT_KEY, Value::Text(PAN_DEFAULT.name().into()))
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        config_layout(config, PAN_DEFAULT)?;
        Ok(PanPorts::layout())
    }

    fn output_shapes(
        &self,
        config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let layout = config_layout(config, PAN_DEFAULT)?;
        let shape = Shape::broadcast_all(inputs.iter().copied())?;
        Ok(vec![Shape::new(shape.voices, layout.channels())])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let layout = config_layout(setup.config, PAN_DEFAULT)?;
        Ok(Instance::realtime(SurroundPanNode {
            layout,
            mono: vec![0.0; setup.max_frames].into_boxed_slice(),
            table: vec![0.0; layout.channels() * setup.max_frames].into_boxed_slice(),
        }))
    }
}

struct SurroundPanNode {
    layout: ChannelLayout,
    /// The averaged source for the voice being done.
    mono: Box<[f32]>,
    /// Each channel's gain for each frame of the voice being done, when the
    /// pan moves: channel `c`, frame `f` at `c * frames + f`.
    table: Box<[f32]>,
}

/// The linear gain of an LFE send of `db`, 0 at the minimum.
fn lfe_gain(db: f32) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

impl Node for SurroundPanNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let frames = ctx.frames;
        let input = io.inputs[PanPorts::INPUT];
        let (azimuth, spread, lfe) = (
            io.inputs[PanPorts::AZIMUTH],
            io.inputs[PanPorts::SPREAD],
            io.inputs[PanPorts::LFE],
        );
        let out = &mut io.outputs[PanPorts::OUT];
        let layout = self.layout;
        let channels = layout.channels();
        let lfe_channel = layout.index_of(noodle_engine::Speaker::Lfe);
        let in_channels = input.shape().channels;
        let scale = 1.0 / in_channels.max(1) as f32;

        for voice in 0..out.shape().voices {
            if voice_silent(&input, voice) {
                for channel in 0..channels {
                    out.silence(voice, channel);
                }
                continue;
            }
            let mono = &mut self.mono[..frames];
            mono.fill(0.0);
            for channel in 0..in_channels {
                if input.is_silent(voice, channel) {
                    continue;
                }
                for (m, x) in mono.iter_mut().zip(input.lane(voice, channel)) {
                    *m += x;
                }
            }
            if in_channels > 1 {
                mono.iter_mut().for_each(|m| *m *= scale);
            }

            let (az, sp, lf) = (
                azimuth.lane(voice, 0),
                spread.lane(voice, 0),
                lfe.lane(voice, 0),
            );
            let fixed = azimuth.constant().zip(spread.constant());
            let mut gains = [0.0f32; MAX_LAYOUT_CHANNELS];
            if let Some((az, sp)) = fixed {
                layout.pan_gains(az, sp, &mut gains);
            } else {
                for f in 0..frames {
                    layout.pan_gains(az[f], sp[f], &mut gains);
                    for (c, g) in gains[..channels].iter().enumerate() {
                        self.table[c * frames + f] = *g;
                    }
                }
            }
            for (channel, &g) in gains[..channels].iter().enumerate() {
                let lane = out.lane_mut(voice, channel);
                if Some(channel) == lfe_channel {
                    if let Some(db) = lfe.constant() {
                        let k = lfe_gain(db);
                        for (o, x) in lane.iter_mut().zip(mono.iter()) {
                            *o = x * k;
                        }
                    } else {
                        for ((o, x), db) in lane.iter_mut().zip(mono.iter()).zip(lf) {
                            *o = x * lfe_gain(*db);
                        }
                    }
                } else if fixed.is_some() {
                    for (o, x) in lane.iter_mut().zip(mono.iter()) {
                        *o = x * g;
                    }
                } else {
                    let table = &self.table[channel * frames..(channel + 1) * frames];
                    for ((o, x), g) in lane.iter_mut().zip(mono.iter()).zip(table) {
                        *o = x * g;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Convert Layout

/// Converts a signal from the layout its channel count implies (1 mono, 2
/// stereo, 3 as 3.0, 4 quad, 6 as 5.1, 8 as 7.1) to the layout in its
/// `layout` setting, by the downmix and upmix rules of
/// [`ChannelLayout::convert_matrix`]: -3 dB folds, the LFE dropped where
/// there is no LFE, speakers the source lacks left silent.
pub struct ConvertLayout;

pub const CONVERT_LAYOUT_ID: &str = "noodle.util.convert_layout";

static CONVERT_INFO: NodeInfo = NodeInfo {
    id: CONVERT_LAYOUT_ID,
    version: 1,
    name: "Convert Layout",
    category: "Utilities",
};

static CONVERT_CONFIG: [ConfigInfo; 1] = [LAYOUT];

const CONVERT_DEFAULT: ChannelLayout = ChannelLayout::Stereo;

fn source_layout(shape: Shape) -> Result<ChannelLayout, NodeError> {
    ChannelLayout::from_channels(shape.channels).ok_or_else(|| {
        NodeError::config(format!(
            "Convert Layout takes 1, 2, 3, 4, 6 or 8 channels, not {}",
            shape.channels
        ))
    })
}

impl NodeType for ConvertLayout {
    fn info(&self) -> &NodeInfo {
        &CONVERT_INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONVERT_CONFIG
    }

    fn initial_config(&self) -> Config {
        Config::new().with(LAYOUT_KEY, Value::Text(CONVERT_DEFAULT.name().into()))
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        config_layout(config, CONVERT_DEFAULT)?;
        Ok(Layout::realtime()
            .input("in", "In")
            .output("out", "Out")
            .memoryless())
    }

    fn output_shapes(
        &self,
        config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        source_layout(inputs[0])?;
        let to = config_layout(config, CONVERT_DEFAULT)?;
        Ok(vec![Shape::new(inputs[0].voices, to.channels())])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let from = source_layout(setup.input_shapes[0])?;
        let to = config_layout(setup.config, CONVERT_DEFAULT)?;
        Ok(Instance::realtime(ConvertNode {
            matrix: ChannelLayout::convert_matrix(from, to).into_boxed_slice(),
            from_channels: from.channels(),
        }))
    }
}

struct ConvertNode {
    /// Row-major, one row per output channel.
    matrix: Box<[f32]>,
    from_channels: usize,
}

impl Node for ConvertNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let input = io.inputs[0];
        let out = &mut io.outputs[0];
        let cols = self.from_channels;
        for voice in 0..out.shape().voices {
            for (row, gains) in self.matrix.chunks(cols).enumerate() {
                let lane = out.lane_mut(voice, row);
                lane.fill(0.0);
                let mut any = false;
                for (channel, &gain) in gains.iter().enumerate() {
                    if gain == 0.0 || input.is_silent(voice, channel) {
                        continue;
                    }
                    any = true;
                    for (o, x) in lane.iter_mut().zip(input.lane(voice, channel)) {
                        *o += x * gain;
                    }
                }
                if !any {
                    out.set_silent(voice, row);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Split and Merge Channels

/// The most channels Split and Merge Channels will have.
const MAX_CHANNELS: i64 = 32;

const CHANNELS: ConfigInfo = ConfigInfo::int("channels", "Channels", 2);

static CHANNELS_CONFIG: [ConfigInfo; 1] = [CHANNELS];

fn channel_count(config: &Config, who: &str) -> Result<usize, NodeError> {
    let channels = CHANNELS.get_int(config);
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(NodeError::config(format!(
            "{who} needs between 1 and {MAX_CHANNELS} channels, not {channels}"
        )));
    }
    Ok(channels as usize)
}

/// Takes a multichannel wire apart: output `chN` is channel N of the input,
/// as a one-channel signal with the input's voices. The input must have
/// exactly as many channels as the `channels` setting, or one (which every
/// output repeats).
pub struct SplitChannels;

pub const SPLIT_CHANNELS_ID: &str = "noodle.util.split_channels";

static SPLIT_INFO: NodeInfo = NodeInfo {
    id: SPLIT_CHANNELS_ID,
    version: 1,
    name: "Split Channels",
    category: "Utilities",
};

impl NodeType for SplitChannels {
    fn info(&self) -> &NodeInfo {
        &SPLIT_INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CHANNELS_CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let channels = channel_count(config, "Split Channels")?;
        Ok((1..=channels)
            .fold(Layout::realtime().input("in", "In"), |layout, i| {
                layout.output(format!("ch{i}"), format!("Ch {i}"))
            })
            .memoryless())
    }

    fn output_shapes(
        &self,
        config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let channels = channel_count(config, "Split Channels")?;
        let input = inputs[0];
        if input.channels != channels && input.channels != 1 {
            return Err(NodeError::config(format!(
                "Split Channels is set to {channels} channels but its input has {}",
                input.channels
            )));
        }
        Ok(vec![Shape::new(input.voices, 1); channels])
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(SplitNode))
    }
}

struct SplitNode;

impl Node for SplitNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let input = io.inputs[0];
        for (channel, out) in io.outputs.iter_mut().enumerate() {
            for voice in 0..out.shape().voices {
                if input.is_silent(voice, channel) {
                    out.silence(voice, 0);
                } else {
                    out.lane_mut(voice, 0)
                        .copy_from_slice(input.lane(voice, channel));
                }
            }
        }
    }
}

/// Puts a multichannel wire together: input `inN` becomes channel N of the
/// output. Inputs may have different voices (they broadcast) but each must
/// be one channel, so a stereo signal is split first.
pub struct MergeChannels;

pub const MERGE_CHANNELS_ID: &str = "noodle.util.merge_channels";

static MERGE_INFO: NodeInfo = NodeInfo {
    id: MERGE_CHANNELS_ID,
    version: 1,
    name: "Merge Channels",
    category: "Utilities",
};

impl NodeType for MergeChannels {
    fn info(&self) -> &NodeInfo {
        &MERGE_INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CHANNELS_CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let channels = channel_count(config, "Merge Channels")?;
        Ok((1..=channels)
            .fold(Layout::realtime(), |layout, i| {
                layout.input(format!("in{i}"), format!("In {i}"))
            })
            .output("out", "Out")
            .memoryless())
    }

    fn output_shapes(
        &self,
        config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let channels = channel_count(config, "Merge Channels")?;
        if let Some(i) = inputs.iter().position(|s| s.channels != 1) {
            return Err(NodeError::config(format!(
                "Merge Channels input {} has {} channels; each input must be one channel",
                i + 1,
                inputs[i].channels
            )));
        }
        let shape = Shape::broadcast_all(inputs.iter().copied())?;
        Ok(vec![Shape::new(shape.voices, channels)])
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(MergeNode))
    }
}

struct MergeNode;

impl Node for MergeNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let out = &mut io.outputs[0];
        for (channel, input) in io.inputs.iter().enumerate() {
            for voice in 0..out.shape().voices {
                if input.is_silent(voice, 0) {
                    out.silence(voice, channel);
                } else {
                    out.lane_mut(voice, channel)
                        .copy_from_slice(input.lane(voice, 0));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    const FRAMES: usize = 8;

    fn with_layout(name: &str) -> Config {
        Config::new().with(LAYOUT_KEY, Value::Text(name.into()))
    }

    fn pan(config: &Config, azimuth: f32, spread: f32, lfe: f32) -> Vec<Vec<f32>> {
        let mut h = Harness::new(
            &SurroundPan,
            config,
            &[(PanPorts::INPUT, Shape::MONO)],
            48_000.0,
            FRAMES,
        )
        .unwrap();
        h.input(PanPorts::INPUT, FRAMES).fill(1.0);
        h.set(PanPorts::AZIMUTH, azimuth);
        h.set(PanPorts::SPREAD, spread);
        h.set(PanPorts::LFE, lfe);
        h.run(FRAMES).unwrap();
        let out = h.output(PanPorts::OUT);
        (0..out.shape().channels)
            .map(|c| out.lane(0, c).to_vec())
            .collect()
    }

    #[test]
    fn a_pan_to_a_speaker_plays_only_that_speaker() {
        let out = pan(&with_layout("5.1"), 0.0, 0.0, -60.0);
        assert_eq!(out.len(), 6);
        for (c, lane) in out.iter().enumerate() {
            let want = if c == 2 { 1.0 } else { 0.0 };
            assert!(lane.iter().all(|x| (x - want).abs() < 1e-6), "channel {c}");
        }
    }

    #[test]
    fn the_default_layout_is_5_1_and_a_bad_name_is_an_error() {
        assert_eq!(pan(&Config::new(), 0.0, 0.0, -60.0).len(), 6);
        assert!(
            Harness::new(
                &SurroundPan,
                &with_layout("9.4"),
                &[(PanPorts::INPUT, Shape::MONO)],
                48_000.0,
                FRAMES
            )
            .is_err()
        );
    }

    #[test]
    fn the_lfe_gets_its_send_and_only_that() {
        let out = pan(&with_layout("7.1"), 0.0, 0.0, 0.0);
        assert_eq!(out.len(), 8);
        assert!(out[3].iter().all(|x| (x - 1.0).abs() < 1e-6));
        let out = pan(&with_layout("7.1"), 0.0, 0.0, -60.0);
        assert!(out[3].iter().all(|x| *x == 0.0));
    }

    #[test]
    fn a_wired_azimuth_moves_the_source_sample_by_sample() {
        let config = with_layout("Quad");
        let mut h = Harness::new(
            &SurroundPan,
            &config,
            &[
                (PanPorts::INPUT, Shape::MONO),
                (PanPorts::AZIMUTH, Shape::MONO),
            ],
            48_000.0,
            FRAMES,
        )
        .unwrap();
        h.input(PanPorts::INPUT, FRAMES).fill(1.0);
        {
            let mut az = h.input(PanPorts::AZIMUTH, FRAMES);
            let lane = az.lane_mut(0, 0);
            for (f, x) in lane.iter_mut().enumerate() {
                *x = if f < FRAMES / 2 { -30.0 } else { 30.0 };
            }
        }
        h.run(FRAMES).unwrap();
        let out = h.output(PanPorts::OUT);
        assert!((out.lane(0, 0)[0] - 1.0).abs() < 1e-5 && out.lane(0, 1)[0].abs() < 1e-5);
        assert!(
            out.lane(0, 0)[FRAMES - 1].abs() < 1e-5
                && (out.lane(0, 1)[FRAMES - 1] - 1.0).abs() < 1e-5
        );
    }

    #[test]
    fn each_voice_keeps_its_own_lanes_and_silence_is_flagged() {
        let mut h = Harness::new(
            &SurroundPan,
            &with_layout("Stereo"),
            &[(PanPorts::INPUT, Shape::new(2, 1))],
            48_000.0,
            FRAMES,
        )
        .unwrap();
        {
            let mut input = h.input(PanPorts::INPUT, FRAMES);
            input.lane_mut(0, 0).fill(1.0);
            input.silence(1, 0);
        }
        h.set(PanPorts::AZIMUTH, -90.0);
        h.run(FRAMES).unwrap();
        let out = h.output(PanPorts::OUT);
        assert_eq!(out.shape(), Shape::new(2, 2));
        assert_eq!(out.lane(0, 0), &[1.0; FRAMES]);
        assert!(out.is_silent(1, 0) && out.is_silent(1, 1));
    }

    #[test]
    fn convert_downmixes_5_1_to_stereo() {
        let mut h = Harness::new(
            &ConvertLayout,
            &Config::new(),
            &[(0, Shape::new(1, 6))],
            48_000.0,
            FRAMES,
        )
        .unwrap();
        {
            let mut input = h.input(0, FRAMES);
            input.lane_mut(0, 2).fill(1.0); // centre
            input.lane_mut(0, 3).fill(1.0); // LFE, dropped
            input.lane_mut(0, 0).fill(0.5); // left
        }
        h.run(FRAMES).unwrap();
        let out = h.output(0);
        assert_eq!(out.shape(), Shape::STEREO);
        let k = std::f32::consts::FRAC_1_SQRT_2;
        assert!((out.lane(0, 0)[0] - (0.5 + k)).abs() < 1e-6);
        assert!((out.lane(0, 1)[0] - k).abs() < 1e-6);
    }

    #[test]
    fn convert_refuses_a_channel_count_with_no_layout() {
        assert!(
            Harness::new(
                &ConvertLayout,
                &Config::new(),
                &[(0, Shape::new(1, 5))],
                48_000.0,
                FRAMES
            )
            .is_err()
        );
    }

    #[test]
    fn split_and_merge_round_trip_channels() {
        let config = Config::new().with("channels", Value::Int(3));
        let mut split = Harness::new(
            &SplitChannels,
            &config,
            &[(0, Shape::new(1, 3))],
            48_000.0,
            FRAMES,
        )
        .unwrap();
        {
            let mut input = split.input(0, FRAMES);
            for c in 0..3 {
                input.lane_mut(0, c).fill(c as f32 + 1.0);
            }
        }
        split.run(FRAMES).unwrap();
        for c in 0..3 {
            assert_eq!(split.output(c).lane(0, 0), &[c as f32 + 1.0; FRAMES]);
        }

        let mut merge = Harness::new(
            &MergeChannels,
            &config,
            &[(0, Shape::MONO), (1, Shape::MONO), (2, Shape::MONO)],
            48_000.0,
            FRAMES,
        )
        .unwrap();
        for c in 0..3 {
            merge.input(c, FRAMES).fill(10.0 + c as f32);
        }
        merge.run(FRAMES).unwrap();
        let out = merge.output(0);
        assert_eq!(out.shape(), Shape::new(1, 3));
        for c in 0..3 {
            assert_eq!(out.lane(0, c), &[10.0 + c as f32; FRAMES]);
        }
    }

    #[test]
    fn split_and_merge_check_their_channels() {
        let three = Config::new().with("channels", Value::Int(3));
        assert!(
            Harness::new(
                &SplitChannels,
                &three,
                &[(0, Shape::new(1, 2))],
                48_000.0,
                FRAMES
            )
            .is_err()
        );
        assert!(
            Harness::new(
                &MergeChannels,
                &three,
                &[(0, Shape::STEREO), (1, Shape::MONO), (2, Shape::MONO)],
                48_000.0,
                FRAMES
            )
            .is_err()
        );
        let zero = Config::new().with("channels", Value::Int(0));
        assert!(Harness::new(&MergeChannels, &zero, &[], 48_000.0, FRAMES).is_err());
    }
}
