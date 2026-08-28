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
    /// What everything outside the workspace is held to. html5ever logs every
    /// token of every sanitized body -- ~10k lines for one email -- which at
    /// `debug` flushes the whole log through `MAX_LOG_BYTES` in minutes and
    /// costs real time on the render path, since each line is a synchronous
    /// write.
    dependency_level: Level,
}

/// Workspace crates all share this prefix, so a target that starts with it is
/// ours and anything else came from a dependency.
const WORKSPACE_TARGET_PREFIX: &str = "birdman";

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let ceiling = if metadata.target().starts_with(WORKSPACE_TARGET_PREFIX) {
            self.level
        } else {
            self.dependency_level
        };
        metadata.level() <= ceiling
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
    let dependency_level = dependency_level_from_env(level);
    let logger = Box::leak(Box::new(FileLogger {
        file: Mutex::new(file),
        level: level.to_level().unwrap_or(Level::Info),
        dependency_level: dependency_level.to_level().unwrap_or(Level::Info),
    }));
    if log::set_logger(logger).is_ok() {
        // The looser of the two, or the `log` macros would drop records before
        // `enabled` ever saw them.
        log::set_max_level(level.max(dependency_level));
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
    parse_level(&std::env::var("BIRDMAN_LOG").unwrap_or_default()).unwrap_or(default)
}

/// Dependencies are capped at `info` however loud the workspace is asked to be.
/// `BIRDMAN_LOG_DEPS` lifts the cap for when the thing being debugged is inside
/// one of them -- the TLS handshake and the OAuth2 refresh are only visible
/// through `rustls` and `ureq`.
fn dependency_level_from_env(workspace: LevelFilter) -> LevelFilter {
    parse_level(&std::env::var("BIRDMAN_LOG_DEPS").unwrap_or_default())
        .unwrap_or_else(|| workspace.min(LevelFilter::Info))
}

fn parse_level(value: &str) -> Option<LevelFilter> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
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
            dependency_level: Level::Info,
        };
        // Targeted: an untargeted record is a dependency's as far as `enabled`
        // is concerned, and would be judged against the cap instead.
        let ours = |level| {
            logger.enabled(
                &Metadata::builder()
                    .target("birdman_config::logging")
                    .level(level)
                    .build(),
            )
        };
        assert!(ours(Level::Debug));
        assert!(!ours(Level::Trace));

        let quiet = FileLogger {
            file: Mutex::new(None),
            level: Level::Info,
            dependency_level: Level::Info,
        };
        let theirs = |level| {
            quiet.enabled(
                &Metadata::builder()
                    .target("birdman_config::logging")
                    .level(level)
                    .build(),
            )
        };
        assert!(!theirs(Level::Debug));
        assert!(theirs(Level::Warn));
    }

    #[test]
    fn a_dependency_is_held_to_info_while_the_workspace_debugs() {
        let logger = FileLogger {
            file: Mutex::new(None),
            level: Level::Debug,
            dependency_level: Level::Info,
        };
        let at = |target: &'static str, level| {
            logger.enabled(&Metadata::builder().target(target).level(level).build())
        };

        assert!(at("birdman_imap::sync", Level::Debug), "ours at debug");
        assert!(
            !at("html5ever::tree_builder", Level::Debug),
            "a dependency's debug is what floods the log"
        );
        assert!(
            at("html5ever::tree_builder", Level::Warn),
            "a dependency still gets to report a problem"
        );
    }

    #[test]
    fn the_dependency_cap_never_exceeds_the_workspace_level() {
        assert_eq!(
            dependency_level_from_env(LevelFilter::Warn),
            LevelFilter::Warn,
            "quieter than info means quieter for everything"
        );
        assert_eq!(
            dependency_level_from_env(LevelFilter::Trace),
            LevelFilter::Info
        );
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
