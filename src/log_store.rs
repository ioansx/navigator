use std::{
    collections::VecDeque,
    str::FromStr,
    sync::{OnceLock, RwLock},
    time::{Duration, Instant},
};

use log::{Level, Log, Metadata, Record};

use crate::error::{Errx, Resultx};

const MAX_LOG_ENTRIES: usize = 1000;
const UNLOCK_MSG: &str = "LOG_STORE lock should not be poisoned";

pub static LOG_STORE: OnceLock<LogStore> = OnceLock::new();

pub fn init_logger(rust_log: Option<String>) -> Resultx<()> {
    let level_filter = rust_log
        .and_then(|x| log::LevelFilter::from_str(&x).ok())
        .unwrap_or(log::LevelFilter::Info);

    let store = LOG_STORE.get_or_init(LogStore::new);
    log::set_logger(store).map_err(|e| Errx::e_any(e, "failed to initialize logging"))?;
    log::set_max_level(level_filter);

    Ok(())
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: Level,
    pub message: String,
    pub timestamp: Instant,
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

    pub fn len(&self) -> usize {
        self.entries.read().expect(UNLOCK_MSG).len()
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries
            .read()
            .expect(UNLOCK_MSG)
            .iter()
            .cloned()
            .collect()
    }

    pub fn latest(&self) -> Option<LogEntry> {
        self.entries.read().expect(UNLOCK_MSG).back().cloned()
    }

    pub fn elapsed_since(&self, entry: &LogEntry) -> Duration {
        entry.timestamp.duration_since(self.start_time)
    }

    pub fn time_since_start(&self) -> Duration {
        self.start_time.elapsed()
    }

    fn push(&self, entry: LogEntry) {
        let mut entries = self.entries.write().expect(UNLOCK_MSG);
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
            };
            self.push(entry);
        }
    }

    fn flush(&self) {}
}
