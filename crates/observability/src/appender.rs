// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! File appender with size-based rotation — the Rust port of pyfly's
//! `pyfly.logging.handlers` (`parse_size`, `build_file_handler` /
//! `RotatingFileHandler`).
//!
//! [`RollingFileWriter`] implements [`std::io::Write`] and rotates the
//! target file when the next write would exceed `max_size`, keeping at
//! most `max_history` numbered backups (`app.log.1` is the newest, like
//! Python's `RotatingFileHandler`). [`TeeWriter`] duplicates the stream so
//! console and file both receive every record — pyfly keeps the console
//! handler on when a file appender is configured.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

/// File-appender settings — pyfly's `FileProperties` + `RollingProperties`
/// (`pyfly.logging.file.*` / `pyfly.logging.rolling.*`). Set
/// [`name`](FileConfig::name) to enable file output (console stays on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileConfig {
    /// Log file name (e.g. `app.log`); empty disables the file appender.
    pub name: String,
    /// Directory for the log file; empty means the working directory.
    /// Created (with parents) when missing.
    pub path: String,
    /// Rotation threshold as a human size (`10MB`, `512KB`, `4096`); an
    /// empty/invalid value (parsed size 0) disables rotation.
    pub max_size: String,
    /// Number of rotated backups to keep; 0 disables rotation.
    pub max_history: u32,
}

impl Default for FileConfig {
    /// pyfly defaults: no file name, working directory, `10MB`, 7 backups.
    fn default() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            max_size: "10MB".to_string(),
            max_history: 7,
        }
    }
}

impl FileConfig {
    /// A config for the given file name with pyfly's rotation defaults.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Sets the directory (builder-style).
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Sets the rotation threshold (builder-style).
    #[must_use]
    pub fn with_max_size(mut self, max_size: impl Into<String>) -> Self {
        self.max_size = max_size.into();
        self
    }

    /// Sets the backup count (builder-style).
    #[must_use]
    pub fn with_max_history(mut self, max_history: u32) -> Self {
        self.max_history = max_history;
        self
    }
}

/// Parses a human size like `10MB` / `512KB` / `4096` to bytes, returning
/// 0 when empty or invalid — byte-for-byte pyfly's
/// `pyfly.logging.handlers.parse_size`.
pub fn parse_size(value: &str) -> u64 {
    static SIZE_RE: OnceLock<Regex> = OnceLock::new();
    let re = SIZE_RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(\d+(?:\.\d+)?)\s*([KMGT]?B?)\s*$").expect("size regex")
    });
    let Some(caps) = re.captures(value) else {
        return 0;
    };
    let number: f64 = caps[1].parse().unwrap_or(0.0);
    let mut unit = caps[2].to_uppercase();
    if !unit.is_empty() && !unit.ends_with('B') {
        unit.push('B');
    }
    let factor: u64 = match unit.as_str() {
        "" | "B" => 1,
        "KB" => 1024,
        "MB" => 1024 * 1024,
        "GB" => 1024 * 1024 * 1024,
        "TB" => 1024_u64.pow(4),
        _ => 1,
    };
    (number * factor as f64) as u64
}

/// A size-rotating file writer — the Rust analog of Python's
/// `RotatingFileHandler` as built by pyfly's `build_file_handler`.
///
/// Backups are numbered `target.1` (newest) … `target.N` (oldest); the
/// rotation shifts every backup up by one and prunes anything beyond
/// `max_history`. Rotation never occurs when `max_size` parses to 0 or
/// `max_history` is 0 (Python's "rollover never occurs" rule).
#[derive(Debug)]
pub struct RollingFileWriter {
    target: PathBuf,
    max_bytes: u64,
    max_history: u32,
    file: File,
    written: u64,
}

impl RollingFileWriter {
    /// Opens (appending) the configured file, creating the directory with
    /// parents when missing — pyfly's `build_file_handler` behaviour.
    /// Fails with `InvalidInput` when `config.name` is empty.
    pub fn new(config: &FileConfig) -> io::Result<Self> {
        if config.name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FileConfig.name must not be empty",
            ));
        }
        let dir = if config.path.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(&config.path)
        };
        std::fs::create_dir_all(&dir)?;
        let target = dir.join(&config.name);
        let file = OpenOptions::new().create(true).append(true).open(&target)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            target,
            max_bytes: parse_size(&config.max_size),
            max_history: config.max_history,
            file,
            written,
        })
    }

    /// The full path of the active log file.
    pub fn path(&self) -> &Path {
        &self.target
    }

    /// The rotation threshold in bytes (0 = rotation disabled).
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// The configured backup count.
    pub fn max_history(&self) -> u32 {
        self.max_history
    }

    fn backup_path(&self, index: u32) -> PathBuf {
        let mut os = self.target.clone().into_os_string();
        os.push(format!(".{index}"));
        PathBuf::from(os)
    }

    fn should_rotate(&self, incoming: usize) -> bool {
        self.max_bytes > 0
            && self.max_history > 0
            && self.written > 0
            && self.written + incoming as u64 > self.max_bytes
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        // Shift backups up: .N-1 -> .N, …, .1 -> .2 — pruning happens
        // implicitly because .N is overwritten and nothing beyond N is
        // ever created.
        let prune = self.backup_path(self.max_history);
        if prune.exists() {
            std::fs::remove_file(&prune)?;
        }
        for i in (1..self.max_history).rev() {
            let from = self.backup_path(i);
            if from.exists() {
                std::fs::rename(&from, self.backup_path(i + 1))?;
            }
        }
        std::fs::rename(&self.target, self.backup_path(1))?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.target)?;
        self.written = 0;
        Ok(())
    }
}

impl Write for RollingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.should_rotate(buf.len()) {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Duplicates every write to two sinks — pyfly keeps the console handler
/// attached alongside the file appender; `TeeWriter` is the Rust analog
/// for a single-writer log layer.
///
/// ```
/// use std::io::Write;
/// use firefly_observability::{BufferWriter, TeeWriter};
///
/// let (a, b) = (BufferWriter::new(), BufferWriter::new());
/// let mut tee = TeeWriter::new(a.clone(), b.clone());
/// tee.write_all(b"hello").unwrap();
/// assert_eq!(a.as_string(), "hello");
/// assert_eq!(b.as_string(), "hello");
/// ```
#[derive(Debug)]
pub struct TeeWriter<A: Write, B: Write> {
    a: A,
    b: B,
}

impl<A: Write, B: Write> TeeWriter<A, B> {
    /// Combines two sinks into one.
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A: Write, B: Write> Write for TeeWriter<A, B> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.a.write_all(buf)?;
        self.b.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.a.flush()?;
        self.b.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pyfly `test_parse_size`.
    #[test]
    fn parse_size_matches_pyfly() {
        assert_eq!(parse_size("10MB"), 10 * 1024 * 1024);
        assert_eq!(parse_size("512KB"), 512 * 1024);
        assert_eq!(parse_size("2GB"), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("4096"), 4096);
        assert_eq!(parse_size(""), 0);
        assert_eq!(parse_size("garbage"), 0);
        assert_eq!(parse_size(" 1.5 kb "), 1536);
        assert_eq!(parse_size("100B"), 100);
        assert_eq!(parse_size("1TB"), 1024_u64.pow(4));
    }

    /// pyfly `test_build_file_handler`.
    #[test]
    fn builds_writer_with_configured_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FileConfig::new("app.log")
            .with_path(dir.path().to_string_lossy())
            .with_max_size("1MB")
            .with_max_history(3);
        let w = RollingFileWriter::new(&cfg).unwrap();
        assert_eq!(w.max_bytes(), 1024 * 1024);
        assert_eq!(w.max_history(), 3);
        assert_eq!(w.path(), dir.path().join("app.log"));
        assert!(w.path().exists());
    }

    /// pyfly `test_build_file_handler_none_without_name`.
    #[test]
    fn empty_name_is_an_error() {
        assert!(RollingFileWriter::new(&FileConfig::default()).is_err());
    }

    #[test]
    fn rotates_and_prunes_backups() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FileConfig::new("app.log")
            .with_path(dir.path().to_string_lossy())
            .with_max_size("32B")
            .with_max_history(2);
        let mut w = RollingFileWriter::new(&cfg).unwrap();
        for i in 0..5 {
            // One write per record (the log layer writes whole lines).
            w.write_all(format!("record-{i}-aaaaaaaaaaaaaaaaaaaaaaaa\n").as_bytes())
                .unwrap();
        }
        w.flush().unwrap();
        let base = dir.path().join("app.log");
        assert!(base.exists());
        assert!(dir.path().join("app.log.1").exists());
        assert!(dir.path().join("app.log.2").exists());
        // Pruned: nothing beyond max_history.
        assert!(!dir.path().join("app.log.3").exists());
        // The newest backup holds the record written just before the
        // last rotation.
        let backup1 = std::fs::read_to_string(dir.path().join("app.log.1")).unwrap();
        assert!(backup1.contains("record-3"), "{backup1}");
    }

    #[test]
    fn no_rotation_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FileConfig::new("flat.log")
            .with_path(dir.path().to_string_lossy())
            .with_max_size("") // parses to 0 -> never rotate
            .with_max_history(3);
        let mut w = RollingFileWriter::new(&cfg).unwrap();
        for _ in 0..10 {
            writeln!(w, "0123456789012345678901234567890123456789").unwrap();
        }
        w.flush().unwrap();
        assert!(!dir.path().join("flat.log.1").exists());
        let content = std::fs::read_to_string(dir.path().join("flat.log")).unwrap();
        assert_eq!(content.lines().count(), 10);
    }

    #[test]
    fn appends_to_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FileConfig::new("keep.log").with_path(dir.path().to_string_lossy());
        {
            let mut w = RollingFileWriter::new(&cfg).unwrap();
            writeln!(w, "first").unwrap();
        }
        {
            let mut w = RollingFileWriter::new(&cfg).unwrap();
            writeln!(w, "second").unwrap();
        }
        let content = std::fs::read_to_string(dir.path().join("keep.log")).unwrap();
        assert_eq!(content, "first\nsecond\n");
    }
}
