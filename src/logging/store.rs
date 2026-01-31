use std::{
    collections::VecDeque,
    sync::{OnceLock, RwLock},
    time::Instant,
};

use log::{Level, Log, Metadata, Record, SetLoggerError};

const MAX_LOG_ENTRIES: usize = 1000;

pub static LOG_STORE: OnceLock<LogStore> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<Level> for LogLevel {
    fn from(level: Level) -> Self {
        match level {
            Level::Error => LogLevel::Error,
            Level::Warn => LogLevel::Warn,
            Level::Info => LogLevel::Info,
            Level::Debug => LogLevel::Debug,
            Level::Trace => LogLevel::Trace,
        }
    }
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
            LogLevel::Trace => "TRACE",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    pub timestamp: Instant,
    pub target: String,
}

pub struct LogStore {
    entries: RwLock<VecDeque<LogEntry>>,
    start_time: Instant,
}

impl LogStore {
    fn new() -> Self {
        Self {
            entries: RwLock::new(VecDeque::with_capacity(MAX_LOG_ENTRIES)),
            start_time: Instant::now(),
        }
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.read().unwrap().iter().cloned().collect()
    }

    pub fn latest(&self) -> Option<LogEntry> {
        self.entries.read().unwrap().back().cloned()
    }

    pub fn elapsed_since(&self, entry: &LogEntry) -> std::time::Duration {
        entry.timestamp.duration_since(self.start_time)
    }

    pub fn time_since_start(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    fn push(&self, entry: LogEntry) {
        let mut entries = self.entries.write().unwrap();
        if entries.len() >= MAX_LOG_ENTRIES {
            entries.pop_front();
        }
        entries.push_back(entry);
    }
}

impl Log for LogStore {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let entry = LogEntry {
                level: record.level().into(),
                message: record.args().to_string(),
                timestamp: Instant::now(),
                target: record.target().to_string(),
            };
            self.push(entry);
        }
    }

    fn flush(&self) {}
}

pub fn init_logger() -> Result<(), SetLoggerError> {
    let store = LOG_STORE.get_or_init(LogStore::new);
    log::set_logger(store)?;
    log::set_max_level(log::LevelFilter::Debug);
    Ok(())
}
