//! The block of shared memory a node and its plugin host process exchange
//! audio and events through.
//!
//! Both sides compute the same [`Layout`] from a [`Spec`], so offsets never
//! travel; the file is the same program's, so the structs don't need a
//! version. Who may touch what, and when, is the handshake in
//! `proxy.rs`/`host.rs`:
//!
//! - the node writes a block's inputs, events and [`BlockParams`], then
//!   bumps `req_seq`; it never writes them again until `done_seq` catches up;
//! - the host reads them, writes the outputs, then sets `done_seq`;
//! - the node reads the outputs after it sees `done_seq`.
//!
//! Lanes are packed by the block's frame count, not by `max_frames`, so the
//! host can hand them to nodes as contiguous signals.

// The accessors hand out `&mut` slices of shared memory from `&self`: that is
// the point of the type, with the handshake as the exclusion.
#![allow(clippy::mut_from_ref)]

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64};

use memmap2::MmapMut;
use noodle_engine::{Event, EventKind, Expression, NoteId, Shape, Transport};
use serde::{Deserialize, Serialize};

pub(crate) const MAGIC: u32 = 0x4e4f_4f44;

/// The most events per note input port in one block. More are dropped.
pub(crate) const MAX_EVENTS_IN: usize = 4096;
/// The most events per note output port in one block.
pub(crate) const MAX_EVENTS_OUT: usize = 1024;

/// What fixes the layout: everything the node and the host both know when the
/// host starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Spec {
    pub max_frames: usize,
    /// (voices, channels) of each input, as wired.
    pub inputs: Vec<(usize, usize)>,
    pub outputs: Vec<(usize, usize)>,
    pub event_inputs: usize,
    pub event_outputs: usize,
}

impl Spec {
    pub fn input_shape(&self, index: usize) -> Shape {
        let (voices, channels) = self.inputs[index];
        Shape::new(voices, channels)
    }

    pub fn output_shape(&self, index: usize) -> Shape {
        let (voices, channels) = self.outputs[index];
        Shape::new(voices, channels)
    }
}

/// Shared flags and counters at the start of the file.
#[repr(C)]
pub(crate) struct Header {
    pub magic: AtomicU32,
    /// The node is going away: the host should exit.
    pub shutdown: AtomicU32,
    /// The host is blocked (or about to block) waiting for a wake-up byte, so
    /// the node must send one after it posts a block.
    pub sleeping: AtomicU32,
    /// The plugin's editor window is up (kept by the host, read by the node).
    pub editor_open: AtomicU32,
    /// Bumped by the node to post a block.
    pub req_seq: AtomicU64,
    /// Set by the host to the `req_seq` it has finished.
    pub done_seq: AtomicU64,
    /// Bumped by the node when the plugin should `reset`.
    pub reset_seq: AtomicU64,
}

/// What the host needs of the engine's `Context`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct BlockParams {
    pub frames: u32,
    pub playing: u32,
    pub has_loop: u32,
    pub numerator: u32,
    pub denominator: u32,
    pub _pad: u32,
    pub position: u64,
    pub loop_start: u64,
    pub loop_end: u64,
    pub tick: f64,
    pub bpm: f64,
}

impl BlockParams {
    pub fn new(frames: usize, transport: &Transport) -> Self {
        let (loop_start, loop_end) = transport.loop_range.unwrap_or((0, 0));
        Self {
            frames: frames as u32,
            playing: u32::from(transport.playing),
            has_loop: u32::from(transport.loop_range.is_some()),
            numerator: u32::from(transport.signature.numerator),
            denominator: u32::from(transport.signature.denominator),
            _pad: 0,
            position: transport.position,
            loop_start,
            loop_end,
            tick: transport.tick,
            bpm: transport.bpm,
        }
    }

    pub fn transport(&self) -> Transport {
        Transport {
            playing: self.playing != 0,
            position: self.position,
            loop_range: (self.has_loop != 0).then_some((self.loop_start, self.loop_end)),
            tick: self.tick,
            bpm: self.bpm,
            signature: noodle_core::TimeSignature {
                numerator: self.numerator as u8,
                denominator: self.denominator as u8,
            },
        }
    }
}

/// How an input arrives: its samples are in the file, or it is one value for
/// the whole block (the host fills its own buffer).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct InputMeta {
    pub constant: u32,
    pub value: f32,
}

/// An [`Event`] in a fixed-size, plain form.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct WireEvent {
    pub time: u32,
    /// 0 note on, 1 note off, 2 expression, 3 midi.
    pub kind: u8,
    pub channel: u8,
    pub key: u8,
    /// An [`Expression`]'s index.
    pub expression: u8,
    pub note: u32,
    /// Velocity or an expression's value.
    pub value: f32,
    pub midi: [u8; 3],
    pub _pad: u8,
}

const EXPRESSIONS: [Expression; 5] = [
    Expression::Pitch,
    Expression::Pressure,
    Expression::Timbre,
    Expression::Volume,
    Expression::Pan,
];

impl WireEvent {
    pub const ZERO: WireEvent = WireEvent {
        time: 0,
        kind: 0,
        channel: 0,
        key: 0,
        expression: 0,
        note: 0,
        value: 0.0,
        midi: [0; 3],
        _pad: 0,
    };

    pub fn new(event: &Event) -> Self {
        let mut wire = Self {
            time: event.time,
            ..Self::ZERO
        };
        match event.kind {
            EventKind::NoteOn {
                note,
                channel,
                key,
                velocity,
            } => {
                wire.kind = 0;
                wire.note = note.0;
                wire.channel = channel;
                wire.key = key;
                wire.value = velocity;
            }
            EventKind::NoteOff { note, velocity } => {
                wire.kind = 1;
                wire.note = note.0;
                wire.value = velocity;
            }
            EventKind::Expression {
                note,
                expression,
                value,
            } => {
                wire.kind = 2;
                wire.note = note.0;
                wire.value = value;
                wire.expression = EXPRESSIONS
                    .iter()
                    .position(|e| *e == expression)
                    .unwrap_or_default() as u8;
            }
            EventKind::Midi(bytes) => {
                wire.kind = 3;
                wire.midi = bytes;
            }
        }
        wire
    }

    pub fn event(&self) -> Event {
        let note = NoteId(self.note);
        let kind = match self.kind {
            0 => EventKind::NoteOn {
                note,
                channel: self.channel,
                key: self.key,
                velocity: self.value,
            },
            1 => EventKind::NoteOff {
                note,
                velocity: self.value,
            },
            2 => EventKind::Expression {
                note,
                expression: EXPRESSIONS
                    .get(usize::from(self.expression))
                    .copied()
                    .unwrap_or(Expression::Pitch),
                value: self.value,
            },
            _ => EventKind::Midi(self.midi),
        };
        Event {
            time: self.time,
            kind,
        }
    }
}

/// Where everything is in the file.
#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub spec: Spec,
    pub block: usize,
    pub input_meta: usize,
    pub event_in_counts: usize,
    pub event_out_counts: usize,
    pub silent_in: usize,
    pub silent_out: usize,
    /// Offset of each input's samples, and how many floats it holds at most.
    pub inputs: Vec<(usize, usize)>,
    pub outputs: Vec<(usize, usize)>,
    pub events_in: usize,
    pub events_out: usize,
    pub total: usize,
    /// First lane index of each input / output in the silence flags.
    pub in_lane_base: Vec<usize>,
    pub out_lane_base: Vec<usize>,
}

fn align(offset: usize) -> usize {
    offset.next_multiple_of(64)
}

impl Layout {
    pub fn new(spec: &Spec) -> Self {
        let mut at = align(size_of::<Header>());
        let mut take = |bytes: usize| {
            let start = at;
            at = align(at + bytes);
            start
        };
        let block = take(size_of::<BlockParams>());
        let input_meta = take(size_of::<InputMeta>() * spec.inputs.len());
        let event_in_counts = take(4 * spec.event_inputs);
        let event_out_counts = take(4 * spec.event_outputs);
        let lanes = |shapes: &[(usize, usize)]| -> Vec<usize> {
            let mut base = Vec::new();
            let mut sum = 0;
            for (voices, channels) in shapes {
                base.push(sum);
                sum += voices * channels;
            }
            base.push(sum);
            base
        };
        let in_lane_base = lanes(&spec.inputs);
        let out_lane_base = lanes(&spec.outputs);
        let silent_in = take(*in_lane_base.last().unwrap_or(&0));
        let silent_out = take(*out_lane_base.last().unwrap_or(&0));
        let inputs = spec
            .inputs
            .iter()
            .map(|(v, c)| {
                let floats = v * c * spec.max_frames;
                (take(4 * floats), floats)
            })
            .collect();
        let outputs = spec
            .outputs
            .iter()
            .map(|(v, c)| {
                let floats = v * c * spec.max_frames;
                (take(4 * floats), floats)
            })
            .collect();
        let events_in = take(size_of::<WireEvent>() * MAX_EVENTS_IN * spec.event_inputs);
        let events_out = take(size_of::<WireEvent>() * MAX_EVENTS_OUT * spec.event_outputs);
        Self {
            spec: spec.clone(),
            block,
            input_meta,
            event_in_counts,
            event_out_counts,
            silent_in,
            silent_out,
            inputs,
            outputs,
            events_in,
            events_out,
            total: at,
            in_lane_base,
            out_lane_base,
        }
    }
}

/// A mapped file and its layout. All access goes through raw pointers, since
/// the other process writes the same bytes; the handshake in the header says
/// when each side may.
pub(crate) struct Mapping {
    map: MmapMut,
    // Keeps the descriptor open for as long as the mapping lives.
    _file: File,
    pub layout: Layout,
    path: PathBuf,
    /// Delete the file when this is dropped (the side that made it).
    owner: bool,
}

// SAFETY: the mapping is shared memory that is only touched through raw
// pointers and atomics, under the handshake described in the module docs.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Where shared files go: memory-backed where the system has such a
    /// place.
    fn dir() -> PathBuf {
        let shm = Path::new("/dev/shm");
        if cfg!(target_os = "linux") && shm.is_dir() {
            shm.to_owned()
        } else {
            std::env::temp_dir()
        }
    }

    /// Makes a new, zeroed file for `spec`.
    pub fn create(spec: &Spec) -> std::io::Result<Self> {
        use std::sync::atomic::Ordering;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let layout = Layout::new(spec);
        let path = Self::dir().join(format!(
            "noodle-plugin-{}-{}.shm",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // The file can't be unlinked while it is mapped there: have the
            // system delete it when the last handle goes (ours and the
            // host's share delete access, as std opens files by default).
            const GENERIC_READ_WRITE_DELETE: u32 = 0x8000_0000 | 0x4000_0000 | 0x0001_0000;
            const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
            options
                .access_mode(GENERIC_READ_WRITE_DELETE)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
        }
        let file = options.open(&path)?;
        file.set_len(layout.total as u64)?;
        // SAFETY: the file is ours alone until the host opens it.
        let map = unsafe { MmapMut::map_mut(&file)? };
        let mapping = Self {
            map,
            _file: file,
            layout,
            path,
            owner: true,
        };
        mapping.header().magic.store(MAGIC, Ordering::Release);
        Ok(mapping)
    }

    /// Opens the file the node made.
    pub fn open(path: &Path, spec: &Spec) -> std::io::Result<Self> {
        let layout = Layout::new(spec);
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        if file.metadata()?.len() < layout.total as u64 {
            return Err(std::io::Error::other("the shared file is too small"));
        }
        // SAFETY: shared with the node by design; see the module docs.
        let map = unsafe { MmapMut::map_mut(&file)? };
        let mapping = Self {
            map,
            _file: file,
            layout,
            path: path.to_owned(),
            owner: false,
        };
        if mapping
            .header()
            .magic
            .load(std::sync::atomic::Ordering::Acquire)
            != MAGIC
        {
            return Err(std::io::Error::other("not a plugin host file"));
        }
        Ok(mapping)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Deletes the file's name (the mapping stays valid). Unix lets us do it
    /// while it is mapped; Windows doesn't, and the drop does it instead.
    pub fn unlink(&self) {
        if cfg!(unix) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn base(&self) -> *mut u8 {
        self.map.as_ptr() as *mut u8
    }

    pub fn header(&self) -> &Header {
        // SAFETY: the file starts with a header, 64-byte aligned, and it is
        // all atomics.
        unsafe { &*(self.base() as *const Header) }
    }

    /// SAFETY (all of these): only while the handshake gives this side the
    /// region, and `T` is plain data.
    pub unsafe fn write_block(&self, params: BlockParams) {
        unsafe { (self.base().add(self.layout.block) as *mut BlockParams).write(params) }
    }

    pub unsafe fn read_block(&self) -> BlockParams {
        unsafe { (self.base().add(self.layout.block) as *const BlockParams).read() }
    }

    pub unsafe fn write_input_meta(&self, index: usize, meta: InputMeta) {
        unsafe {
            (self.base().add(self.layout.input_meta) as *mut InputMeta)
                .add(index)
                .write(meta)
        }
    }

    pub unsafe fn read_input_meta(&self, index: usize) -> InputMeta {
        unsafe {
            (self.base().add(self.layout.input_meta) as *const InputMeta)
                .add(index)
                .read()
        }
    }

    /// The input's float region (`lanes * max_frames` long).
    pub unsafe fn input_floats(&self, index: usize) -> &mut [f32] {
        let (offset, floats) = self.layout.inputs[index];
        unsafe { std::slice::from_raw_parts_mut(self.base().add(offset) as *mut f32, floats) }
    }

    pub unsafe fn output_floats(&self, index: usize) -> &mut [f32] {
        let (offset, floats) = self.layout.outputs[index];
        unsafe { std::slice::from_raw_parts_mut(self.base().add(offset) as *mut f32, floats) }
    }

    /// One byte per lane of the input: 1 if it is known silent.
    pub unsafe fn input_silent(&self, index: usize) -> &mut [u8] {
        let base = &self.layout.in_lane_base;
        unsafe {
            std::slice::from_raw_parts_mut(
                self.base().add(self.layout.silent_in + base[index]),
                base[index + 1] - base[index],
            )
        }
    }

    pub unsafe fn output_silent(&self, index: usize) -> &mut [u8] {
        let base = &self.layout.out_lane_base;
        unsafe {
            std::slice::from_raw_parts_mut(
                self.base().add(self.layout.silent_out + base[index]),
                base[index + 1] - base[index],
            )
        }
    }

    pub unsafe fn event_in_count(&self, port: usize) -> &mut u32 {
        unsafe { &mut *(self.base().add(self.layout.event_in_counts) as *mut u32).add(port) }
    }

    pub unsafe fn event_out_count(&self, port: usize) -> &mut u32 {
        unsafe { &mut *(self.base().add(self.layout.event_out_counts) as *mut u32).add(port) }
    }

    pub unsafe fn events_in(&self, port: usize) -> &mut [WireEvent] {
        unsafe {
            std::slice::from_raw_parts_mut(
                (self.base().add(self.layout.events_in) as *mut WireEvent)
                    .add(port * MAX_EVENTS_IN),
                MAX_EVENTS_IN,
            )
        }
    }

    pub unsafe fn events_out(&self, port: usize) -> &mut [WireEvent] {
        unsafe {
            std::slice::from_raw_parts_mut(
                (self.base().add(self.layout.events_out) as *mut WireEvent)
                    .add(port * MAX_EVENTS_OUT),
                MAX_EVENTS_OUT,
            )
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if self.owner && cfg!(unix) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            max_frames: 64,
            inputs: vec![(1, 2), (1, 1)],
            outputs: vec![(1, 2)],
            event_inputs: 1,
            event_outputs: 1,
        }
    }

    #[test]
    fn regions_do_not_overlap_and_are_aligned() {
        let layout = Layout::new(&spec());
        let mut spans = vec![
            (layout.block, size_of::<BlockParams>()),
            (layout.input_meta, 2 * size_of::<InputMeta>()),
            (layout.event_in_counts, 4),
            (layout.event_out_counts, 4),
            (layout.silent_in, 3),
            (layout.silent_out, 2),
            (layout.events_in, size_of::<WireEvent>() * MAX_EVENTS_IN),
            (layout.events_out, size_of::<WireEvent>() * MAX_EVENTS_OUT),
        ];
        for (offset, floats) in layout.inputs.iter().chain(&layout.outputs) {
            spans.push((*offset, 4 * floats));
        }
        spans.sort();
        for pair in spans.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0, "{pair:?}");
        }
        for (offset, _) in &spans {
            assert_eq!(offset % 64, 0);
        }
        assert!(spans.last().map(|(o, l)| o + l).unwrap() <= layout.total);
    }

    #[test]
    fn events_survive_the_wire() {
        for kind in [
            EventKind::NoteOn {
                note: NoteId(7),
                channel: 3,
                key: 60,
                velocity: 0.5,
            },
            EventKind::NoteOff {
                note: NoteId(7),
                velocity: 0.25,
            },
            EventKind::Expression {
                note: NoteId(9),
                expression: Expression::Pan,
                value: -0.5,
            },
            EventKind::Midi([0xb0, 1, 2]),
        ] {
            let event = Event { time: 17, kind };
            assert_eq!(WireEvent::new(&event).event(), event);
        }
    }

    #[test]
    fn a_second_mapping_sees_the_first_ones_writes() {
        let spec = spec();
        let a = Mapping::create(&spec).unwrap();
        let b = Mapping::open(a.path(), &spec).unwrap();
        unsafe {
            a.input_floats(0)[5] = 1.5;
            *a.event_in_count(0) = 2;
        }
        unsafe {
            assert_eq!(b.input_floats(0)[5], 1.5);
            assert_eq!(*b.event_in_count(0), 2);
        }
        a.header()
            .req_seq
            .store(3, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            b.header().req_seq.load(std::sync::atomic::Ordering::SeqCst),
            3
        );
    }
}
