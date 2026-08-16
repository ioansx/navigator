use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    str::FromStr,
    sync::{
        OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use log::{Level, Log, Metadata, Record};

use crate::error::{Errx, Resultx};

const UNLOCK_MSG: &str = "LOG_STORE lock should not be poisoned";

pub static LOG_STORE: OnceLock<LogStore> = OnceLock::new();

/// Routes the `log` crate into [`LOG_STORE`], filtered by `rust_log` (default `info`).
///
/// # Errors
/// Fails if a logger was already installed.
pub fn init_logger(rust_log: Option<String>) -> Resultx<()> {
    let level_filter = rust_log
        .and_then(|x| log::LevelFilter::from_str(&x).ok())
        .unwrap_or(log::LevelFilter::Info);

    let store = LOG_STORE.get_or_init(|| LogStore::new(default_dump_path()));
    log::set_logger(store).map_err(|e| Errx::e_any(e, "failed to initialize logging"))?;
    log::set_max_level(level_filter);

    Ok(())
}

/// One file per running `nav`, so two of them side by side do not overwrite each
/// other's dump.
fn default_dump_path() -> PathBuf {
    std::env::temp_dir().join(format!("nav-{}.log", std::process::id()))
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: Level,
    pub message: String,
    pub timestamp: Instant,
}

pub struct LogStore {
    /// The whole session. Nothing is dropped: `nav` is launched per-invocation and
    /// exits when a file is opened, so a session's log stays small — and a dump
    /// missing its beginning is the half you usually need.
    entries: RwLock<VecDeque<LogEntry>>,
    start_time: Instant,
    dump_path: PathBuf,
    /// Whether the log has been told where the dump goes.
    announced: AtomicBool,
}

impl LogStore {
    fn new(dump_path: PathBuf) -> Self {
        Self {
            entries: RwLock::new(VecDeque::new()),
            start_time: Instant::now(),
            dump_path,
            announced: AtomicBool::new(false),
        }
    }

    /// Not part of the public API: only the log panel needs this, to clamp its scroll.
    pub(crate) fn len(&self) -> usize {
        self.entries.read().expect(UNLOCK_MSG).len()
    }

    /// # Panics
    /// Only if a thread panicked while holding the lock, which nothing here does.
    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries
            .read()
            .expect(UNLOCK_MSG)
            .iter()
            .cloned()
            .collect()
    }

    /// # Panics
    /// Only if a thread panicked while holding the lock, which nothing here does.
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
        self.entries.write().expect(UNLOCK_MSG).push_back(entry);
    }

    /// Names the dump file once, and just before the error that calls for it, so
    /// the error itself is still the last line the status line shows.
    fn announce_dump(&self) {
        if self.announced.swap(true, Ordering::Relaxed) {
            return;
        }
        self.push(entry(
            Level::Warn,
            format!("full log: {}", self.dump_path.display()),
        ));
    }

    /// Writes the session out, so the log outlives the screen it was drawn on.
    ///
    /// Rewritten whole on each error rather than appended to: whichever error
    /// wrote it last, the file is the complete session up to that point.
    fn dump(&self) {
        if let Err(e) = fs::write(&self.dump_path, render(&self.entries(), self.start_time)) {
            // Pushed straight in: reporting this through the log macros would come
            // back here and try to dump all over again.
            self.push(entry(
                Level::Warn,
                format!("could not write {}: {e}", self.dump_path.display()),
            ));
        }
    }
}

fn entry(level: Level, message: String) -> LogEntry {
    LogEntry {
        level,
        message,
        timestamp: Instant::now(),
    }
}

/// One line per entry: how far into the session it was logged, then its level and
/// message.
fn render(entries: &[LogEntry], start: Instant) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for entry in entries {
        // Writing into a `String` cannot fail, so there is no error to handle.
        let _ = writeln!(
            out,
            "{:>8.3}s {:<5} {}",
            entry.timestamp.duration_since(start).as_secs_f64(),
            entry.level,
            entry.message
        );
    }
    out
}

impl Log for LogStore {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let failed = record.level() == Level::Error;
        if failed {
            self.announce_dump();
        }

        self.push(entry(record.level(), record.args().to_string()));

        if failed {
            self.dump();
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testdir::TempDir;

    fn store(tmp: &TempDir) -> LogStore {
        LogStore::new(tmp.path().join("nav.log"))
    }

    fn send(store: &LogStore, level: Level, message: &str) {
        store.log(
            &Record::builder()
                .level(level)
                .args(format_args!("{message}"))
                .build(),
        );
    }

    fn dumped(store: &LogStore) -> String {
        fs::read_to_string(&store.dump_path).expect("the dump should have been written")
    }

    fn messages(store: &LogStore) -> Vec<String> {
        store
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect()
    }

    #[test]
    fn a_session_that_goes_well_leaves_no_file_behind() {
        let tmp = TempDir::new();
        let store = store(&tmp);

        send(&store, Level::Info, "started");
        send(&store, Level::Warn, "nothing serious");

        assert!(!store.dump_path.exists());
    }

    #[test]
    fn an_error_dumps_the_whole_session_not_just_the_error() {
        let tmp = TempDir::new();
        let store = store(&tmp);

        send(&store, Level::Info, "started");
        send(&store, Level::Debug, "read a directory");
        send(&store, Level::Error, "boom");

        let dump = dumped(&store);

        assert!(dump.contains("started"), "{dump}");
        assert!(dump.contains("read a directory"), "{dump}");
        assert!(dump.contains("boom"), "{dump}");
    }

    #[test]
    fn the_log_says_where_the_dump_went() {
        let tmp = TempDir::new();
        let store = store(&tmp);

        send(&store, Level::Error, "boom");

        let path = store.dump_path.display().to_string();
        assert!(
            messages(&store)
                .iter()
                .any(|message| message.contains(&path)),
            "the dump should be named in the log"
        );
    }

    #[test]
    fn the_error_is_still_the_latest_thing_that_happened() {
        // The status line shows the latest entry, and that has to be the error
        // rather than the housekeeping note about where it was written.
        let tmp = TempDir::new();
        let store = store(&tmp);

        send(&store, Level::Error, "boom");

        assert_eq!(store.latest().unwrap().message, "boom");
    }

    #[test]
    fn the_dump_is_named_once_however_many_errors_follow() {
        let tmp = TempDir::new();
        let store = store(&tmp);

        send(&store, Level::Error, "first");
        send(&store, Level::Error, "second");
        send(&store, Level::Error, "third");

        let path = store.dump_path.display().to_string();
        let mentions = messages(&store)
            .iter()
            .filter(|message| message.contains(&path))
            .count();

        assert_eq!(mentions, 1);
    }

    #[test]
    fn later_errors_keep_the_dump_up_to_date() {
        let tmp = TempDir::new();
        let store = store(&tmp);

        send(&store, Level::Error, "first");
        send(&store, Level::Error, "second");

        let dump = dumped(&store);

        assert!(dump.contains("first"), "{dump}");
        assert!(dump.contains("second"), "{dump}");
    }

    #[test]
    fn nothing_is_ever_dropped() {
        let tmp = TempDir::new();
        let store = store(&tmp);

        for i in 0..5_000 {
            send(&store, Level::Info, &format!("entry {i}"));
        }

        assert_eq!(store.len(), 5_000);
        assert_eq!(store.entries().first().unwrap().message, "entry 0");
    }

    #[test]
    fn a_dumped_line_reads_as_time_level_message() {
        let start = Instant::now();
        let entries = [
            LogEntry {
                level: Level::Info,
                message: "started".into(),
                timestamp: start,
            },
            LogEntry {
                level: Level::Error,
                message: "boom".into(),
                timestamp: start + Duration::from_millis(1_500),
            },
        ];

        assert_eq!(
            render(&entries, start),
            "   0.000s INFO  started\n   1.500s ERROR boom\n"
        );
    }

    #[test]
    fn an_entry_logged_before_the_store_started_does_not_panic() {
        let start = Instant::now();
        let entries = [LogEntry {
            level: Level::Info,
            message: "early".into(),
            timestamp: start.checked_sub(Duration::from_secs(1)).unwrap(),
        }];

        assert_eq!(render(&entries, start), "   0.000s INFO  early\n");
    }
}
