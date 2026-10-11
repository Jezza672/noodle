//! Finding plugin files and scanning them in a child process.
//!
//! Loading a plugin runs its code, and plugins crash and hang. So the scan
//! happens in a child process that prints what it found: a crash or timeout
//! costs one plugin, not the app. Results are cached by file size and
//! modification time, so a plugin is only scanned again when it changes.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::descriptor::{Descriptor, PluginFile, file_stamp};
use crate::host::describe_file;

/// The argument that makes a host program act as the scanner.
pub const SCAN_ARG: &str = "--noodle-scan-plugin";

/// What the scanner prints before its JSON.
const MARKER: &str = "NOODLE-SCAN ";

/// How long one file may take to scan.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(30);

/// If this program was started as the scanner (`<exe> --noodle-scan-plugin
/// <file>`), scans the file, prints the result and exits; otherwise returns.
/// `noodle` and the app call this first thing in `main`.
pub fn run_scan_child_if_requested() {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|arg| arg == SCAN_ARG) {
        scan_child_main(args.next());
    }
}

/// The scanner's whole job; never returns.
pub fn scan_child_main(path: Option<OsString>) -> ! {
    let Some(path) = path else {
        eprintln!("usage: <scanner> <plugin file>");
        std::process::exit(2);
    };
    match describe_file(Path::new(&path)) {
        Ok(plugins) => {
            let json = serde_json::to_string(&plugins).expect("descriptors serialise");
            println!("{MARKER}{json}");
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

/// How to start the scanner.
#[derive(Clone, Debug)]
pub struct ScanCommand {
    pub program: PathBuf,
    /// Arguments before the plugin file's path.
    pub args: Vec<OsString>,
    pub timeout: Duration,
}

impl ScanCommand {
    /// This program, started with [`SCAN_ARG`]. Needs
    /// [`run_scan_child_if_requested`] in its `main`.
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            args: vec![SCAN_ARG.into()],
            timeout: SCAN_TIMEOUT,
        })
    }

    /// The `noodle-plugin-scan` program at `program`.
    pub fn program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            timeout: SCAN_TIMEOUT,
        }
    }
}

/// Scans one file in a child process. Never fails: a file that can't be
/// scanned comes back with `error` set and no plugins.
pub fn scan_file(command: &ScanCommand, path: &Path) -> PluginFile {
    let (len, modified) = file_stamp(path).unwrap_or((0, 0));
    let (plugins, error) = match run_child(command, path) {
        Ok(plugins) => (plugins, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    PluginFile {
        path: path.to_owned(),
        len,
        modified,
        plugins,
        error,
    }
}

fn run_child(command: &ScanCommand, path: &Path) -> Result<Vec<Descriptor>, String> {
    let mut child = Command::new(&command.program)
        .args(&command.args)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't start the scanner: {error}"))?;
    // Read both pipes on their own threads, so a chatty plugin can't fill one
    // and block the child.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                text = String::from_utf8_lossy(&bytes).into_owned();
            }
            text
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > command.timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {:?}", command.timeout));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(format!("can't wait for the scanner: {error}")),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        let reason = stderr.lines().last().unwrap_or("").trim();
        return Err(if reason.is_empty() {
            format!("the scanner failed ({status})")
        } else {
            format!("the scanner failed ({status}): {reason}")
        });
    }
    let json = stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(MARKER))
        .ok_or("the scanner printed no result")?;
    serde_json::from_str(json).map_err(|error| format!("unreadable scan result: {error}"))
}

/// Where CLAP and VST3 plugins are installed by convention, plus `CLAP_PATH`
/// and `VST3_PATH`.
pub fn standard_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["CLAP_PATH", "VST3_PATH"] {
        if let Some(paths) = std::env::var_os(var) {
            dirs.extend(std::env::split_paths(&paths));
        }
    }
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_owned());
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = &home {
            dirs.push(home.join(".clap"));
        }
        dirs.push("/usr/lib/clap".into());
        dirs.push("/usr/local/lib/clap".into());
        if let Some(home) = &home {
            dirs.push(home.join(".vst3"));
        }
        dirs.push("/usr/lib/vst3".into());
        dirs.push("/usr/local/lib/vst3".into());
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = &home {
            dirs.push(home.join("Library/Audio/Plug-Ins/CLAP"));
        }
        dirs.push("/Library/Audio/Plug-Ins/CLAP".into());
        if let Some(home) = &home {
            dirs.push(home.join("Library/Audio/Plug-Ins/VST3"));
        }
        dirs.push("/Library/Audio/Plug-Ins/VST3".into());
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            dirs.push(PathBuf::from(common).join("CLAP"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Programs/Common/CLAP"));
        }
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            dirs.push(PathBuf::from(common).join("VST3"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Programs/Common/VST3"));
        }
        let _ = &home;
    }
    dirs
}

/// The `.clap` and `.vst3` files under `dirs`. On macOS a `.clap` is a folder
/// (a bundle) and is returned as one file, not searched; elsewhere a folder
/// with that name is searched like any other. A `.vst3` is a bundle on every
/// system (a folder holding the binary), so it is always returned as one.
pub fn find_plugin_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // A `.clap` folder is a bundle on macOS; elsewhere it is just a
            // folder (some packages put the file inside one of that name).
            let ext = path.extension();
            let named = ext.is_some_and(|ext| ext == "clap");
            if ext.is_some_and(|ext| ext == "vst3")
                || (named && (cfg!(target_os = "macos") || !path.is_dir()))
            {
                found.push(path);
            } else if depth < 3 && path.is_dir() {
                walk(&path, depth + 1, found);
            }
        }
    }
    let mut found = Vec::new();
    for dir in dirs {
        walk(dir, 0, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

/// The saved scan results.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanCache {
    pub files: Vec<PluginFile>,
}

impl ScanCache {
    /// Where the app keeps it.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Noodle")
            .map(|dirs| dirs.cache_dir().join("plugins.json"))
    }

    /// The cache at `path`; empty if it's missing or unreadable.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(partial, path)
    }

    /// Scans `files`, reusing the cached result for any file that hasn't
    /// changed. Files that are gone drop out. Returns how many were scanned.
    pub fn refresh(&mut self, command: &ScanCommand, files: &[PathBuf]) -> usize {
        let mut scanned = 0;
        let mut fresh = Vec::with_capacity(files.len());
        for path in files {
            match self
                .files
                .iter()
                .position(|f| &f.path == path && f.is_current())
            {
                Some(index) => fresh.push(self.files.swap_remove(index)),
                None => {
                    scanned += 1;
                    fresh.push(scan_file(command, path));
                }
            }
        }
        self.files = fresh;
        scanned
    }
}
