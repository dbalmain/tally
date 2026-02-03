use log::{LevelFilter, Log, Metadata, Record};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{env, io};
use tracing_appender::rolling::{RollingFileAppender, Rotation};

use crate::Result;

struct TallyLogger {
    file_writer: Arc<Mutex<RollingFileAppender>>,
    level: LevelFilter,
}

impl Log for TallyLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let message = format!("{}", record.args());
        let level = record.level();
        let timestamp = chrono::Local::now();

        if let Ok(mut writer) = self.file_writer.lock() {
            let _ = writeln!(
                writer,
                "{} [{}] {}",
                timestamp.format("%Y-%m-%d %H:%M:%S"),
                level,
                message
            );
        }
    }

    fn flush(&self) {}
}

fn parse_level(level_str: &str) -> LevelFilter {
    match level_str.to_lowercase().as_str() {
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}

/// Get data directory following XDG Base Directory Specification.
/// Returns $XDG_DATA_HOME/tally or ~/.local/share/tally
fn get_data_dir() -> Result<PathBuf> {
    let data_dir = if let Ok(xdg_data) = env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg_data).join("tally")
    } else {
        let home = env::var("HOME")
            .map_err(|_| crate::Error::Io(io::Error::other("HOME environment variable not set")))?;
        PathBuf::from(home).join(".local/share/tally")
    };
    Ok(data_dir)
}

/// Initialize file-based logging with daily rotation.
/// Log level controlled by TALLY_LOG env var (default: info).
/// Logs to ~/.local/share/tally/tally.<date>.log
pub fn init() -> Result<PathBuf> {
    let data_dir = get_data_dir()?;
    fs::create_dir_all(&data_dir)?;

    let level_str = std::env::var("TALLY_LOG").unwrap_or_else(|_| "info".to_string());
    let level = parse_level(&level_str);

    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .max_log_files(3)
        .filename_prefix("tally")
        .filename_suffix("log")
        .build(&data_dir)
        .map_err(|e| crate::Error::Io(io::Error::other(e.to_string())))?;

    let logger = TallyLogger {
        file_writer: Arc::new(Mutex::new(file_appender)),
        level,
    };

    log::set_boxed_logger(Box::new(logger))
        .map_err(|e| crate::Error::Io(io::Error::other(e.to_string())))?;
    log::set_max_level(level);

    Ok(data_dir)
}
