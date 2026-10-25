//! The control channel between a node and its host process: one JSON object
//! per line over a loopback socket. Audio never goes through it (see
//! `shm.rs`); this carries only the start-up handshake and the rare
//! main-thread requests (editor, state capture), each answered by one line.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use noodle_engine::Config;
use serde::{Deserialize, Serialize};

use super::shm::Spec;
use crate::descriptor::Descriptor;

/// Everything the host needs to build the same node the app would have.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Hello {
    pub shm: PathBuf,
    pub spec: Spec,
    pub plugin_path: PathBuf,
    pub descriptor: Descriptor,
    pub config: Config,
    pub node: u64,
    pub sample_rate: f32,
    pub seed: u64,
    pub position: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum ToHost {
    Hello(Box<Hello>),
    OpenEditor,
    CloseEditor,
    Capture,
    /// Leave, now.
    Quit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum FromHost {
    Ready {
        latency: u32,
    },
    Done,
    Failed {
        error: String,
    },
    Captured {
        /// The plugin's state, base64.
        state: String,
        params: Vec<(u32, f64)>,
    },
}

/// One end of the channel.
pub(crate) struct Channel {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Channel {
    pub fn new(stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nodelay(true)?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        })
    }

    pub fn set_timeout(&self, timeout: Option<Duration>) {
        let _ = self.writer.set_read_timeout(timeout);
        let _ = self.writer.set_write_timeout(timeout);
    }

    pub fn send<T: Serialize>(&mut self, message: &T) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.writer.write_all(&line)
    }

    /// The next message; `Ok(None)` when the other end has closed. A read
    /// that times out is an error of kind `WouldBlock` or `TimedOut`.
    pub fn receive<T: for<'de> Deserialize<'de>>(&mut self) -> std::io::Result<Option<T>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        serde_json::from_str(&line)
            .map(Some)
            .map_err(std::io::Error::other)
    }

    pub fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")
    }

    pub fn receive_line(&mut self) -> std::io::Result<Option<String>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end().to_owned()))
    }
}

pub(crate) fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}
