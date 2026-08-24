use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use log::{Level, LevelFilter, Metadata, Record};

/// Truncation, not rotation. An unbounded log on a mailbox that reconnects in
/// a loop is a real disk problem.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// IMAP servers can return error payloads megabytes long.
const MAX_LINE_CHARS: usize = 4000;

struct FileLogger {
    file: Mutex<Option<File>>,
    /// Must track `set_max_level`. Hardcoding it once silently discarded every
    /// `log::debug!` in the workspace; `enabled_respects_the_configured_level`
    /// is the guard against that returning.
    level: Level,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = truncate(&record.args().to_string(), MAX_LINE_CHARS);
        let line = format!(
            "{} {:5} {} {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            record.level(),
            record.target(),
            message,
        );
        eprintln!("{line}");
        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

pub fn init(data_dir: &Path) {
    let path = data_dir.join("birdman.log");
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        let _ = std::fs::remove_file(&path);
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();
    let level = level_from_env();
    let logger = Box::leak(Box::new(FileLogger {
        file: Mutex::new(file),
        level: level.to_level().unwrap_or(Level::Info),
    }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}

/// A scope timer that reports itself when it runs long. Silent unless logging
/// is at `debug`; over-budget scopes report at `warn` so they stand out.
///
/// ```ignore
/// let _timed = Timed::new("query messages", Timed::ROUND_TRIP);
/// ```
pub struct Timed {
    label: std::borrow::Cow<'static, str>,
    started: std::time::Instant,
    budget: std::time::Duration,
}

impl Timed {
    pub const FRAME: std::time::Duration = std::time::Duration::from_millis(16);
    pub const ROUND_TRIP: std::time::Duration = std::time::Duration::from_millis(250);
    pub const NETWORK: std::time::Duration = std::time::Duration::from_secs(2);

    pub fn new(
        label: impl Into<std::borrow::Cow<'static, str>>,
        budget: std::time::Duration,
    ) -> Self {
        Self {
            label: label.into(),
            started: std::time::Instant::now(),
            budget,
        }
    }

    pub fn elapsed_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }
}

pub fn instrumented() -> bool {
    log::log_enabled!(log::Level::Debug)
}

impl Drop for Timed {
    fn drop(&mut self) {
        if !instrumented() {
            return;
        }
        let took = self.started.elapsed();
        if took >= self.budget {
            log::warn!(
                "{} took {}ms (budget {}ms)",
                self.label,
                took.as_millis(),
                self.budget.as_millis()
            );
        } else {
            // `trace`, not `debug`: there is a guard on every frame, so at
            // 60fps this alone would fill the size cap in minutes.
            log::trace!("{} took {}ms", self.label, took.as_millis());
        }
    }
}

/// A debug build logs at `debug`, a release build at `info`. `BIRDMAN_LOG`
/// overrides either, and is read from the environment rather than config --
/// the moment you want it is usually the moment config is too broken to read.
fn level_from_env() -> LevelFilter {
    let default = if cfg!(debug_assertions) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    match std::env::var("BIRDMAN_LOG")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        "info" => LevelFilter::Info,
        _ => default,
    }
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept}… ({} more characters)", total - max_chars)
}

#[cfg(test)]
mod tests {
    use log::Log as _;
    #[test]
    fn a_debug_build_logs_debug_without_being_asked() {
        let expected = if cfg!(debug_assertions) {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        };
        assert_eq!(level_from_env(), expected);
    }

    #[test]
    fn the_file_logger_honours_the_level_it_was_built_for() {
        let logger = FileLogger {
            file: Mutex::new(None),
            level: Level::Debug,
        };
        assert!(logger.enabled(&Metadata::builder().level(Level::Debug).build()));
        assert!(!logger.enabled(&Metadata::builder().level(Level::Trace).build()));

        let quiet = FileLogger {
            file: Mutex::new(None),
            level: Level::Info,
        };
        assert!(!quiet.enabled(&Metadata::builder().level(Level::Debug).build()));
        assert!(quiet.enabled(&Metadata::builder().level(Level::Warn).build()));
    }

    use super::*;

    #[test]
    fn leaves_short_text_alone() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn reports_how_much_it_dropped() {
        assert_eq!(truncate("abcdefgh", 3), "abc… (5 more characters)");
    }

    #[test]
    fn counts_characters_not_bytes() {
        assert_eq!(truncate("ünïcödé", 3), "ünï… (4 more characters)");
    }
}
