//! Журнал в файл с разбивкой по дням (находка Н-8, шаг 2.6 плана).
//!
//! Под службой у exch нет окна: без файла после аварии нечего разбирать и нечего прислать.
//! Файлы — `<папка проекта>/logs/exch.ГГГГ-ММ-ДД.log`, день — по UTC, как время в строках.
//! Каждая строка журнала пишется в файл сразу, одним вызовом: падение процесса не теряет того,
//! что уже записано. Новый день — новый файл, и тогда же удаляются файлы старше `keep` дней.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};

/// Имя файла: `exch.2026-11-13.log`.
const PREFIX: &str = "exch.";
const SUFFIX: &str = ".log";

type Clock = Box<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Писатель журнала по дням. Его отдают `tracing_subscriber` как `MakeWriter`.
pub struct DailyFile {
    dir: PathBuf,
    keep: usize,
    clock: Clock,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    day: Option<NaiveDate>,
    file: Option<File>,
}

impl DailyFile {
    /// Папка создаётся, если её нет. `keep` — сколько дней хранить (не меньше 1).
    pub fn new(dir: impl Into<PathBuf>, keep: usize) -> io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            keep: keep.max(1),
            clock: Box::new(Utc::now),
            state: Mutex::new(State::default()),
        })
    }

    /// Свои часы (для тестов).
    pub fn with_clock(mut self, clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Записать одну строку (событие журнала целиком).
    pub fn write_line(&self, line: &[u8]) -> io::Result<()> {
        let today = (self.clock)().date_naive();
        let mut st = self
            .state
            .lock()
            .map_err(|_| io::Error::other("log file lock poisoned"))?;
        if st.day != Some(today) || st.file.is_none() {
            let path = self.dir.join(file_name(today));
            st.file = Some(OpenOptions::new().create(true).append(true).open(path)?);
            st.day = Some(today);
            self.prune(today);
        }
        match st.file.as_mut() {
            Some(f) => f.write_all(line),
            None => Ok(()),
        }
    }

    /// Удалить файлы журнала, которые старше `keep` дней от `today`. Чужие файлы не трогает.
    fn prune(&self, today: NaiveDate) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let oldest_kept = today - chrono::Days::new(self.keep as u64 - 1);
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(day) = name.to_str().and_then(day_of) else {
                continue;
            };
            if day < oldest_kept {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn file_name(day: NaiveDate) -> String {
    format!("{PREFIX}{}{SUFFIX}", day.format("%Y-%m-%d"))
}

/// День из имени `exch.ГГГГ-ММ-ДД.log`; другое имя — `None`.
fn day_of(name: &str) -> Option<NaiveDate> {
    let date = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

/// Писатель одного события: копит строку и отдаёт её файлу целиком, когда событие записано.
pub struct EventWriter<'a> {
    file: &'a DailyFile,
    buf: Vec<u8>,
}

impl Write for EventWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for EventWriter<'_> {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            // Журнал не должен ронять программу: не записалось — сказать в окно и работать дальше.
            if let Err(e) = self.file.write_line(&self.buf) {
                eprintln!(
                    "cannot write the log file in {}: {e}",
                    self.file.dir.display()
                );
            }
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for DailyFile {
    type Writer = EventWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        EventWriter {
            file: self,
            buf: Vec::with_capacity(256),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exch-logs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_day_of_work_leaves_files_by_day_and_old_ones_are_deleted() {
        let dir = temp_dir("days");
        // Часы, которые двигает тест: старт 10 ноября 2026, 23:59:58 UTC.
        let now = Arc::new(AtomicI64::new(
            DateTime::parse_from_rfc3339("2026-11-10T23:59:58Z")
                .unwrap()
                .timestamp(),
        ));
        let clock = now.clone();
        let log = DailyFile::new(&dir, 3)
            .unwrap()
            .with_clock(move || DateTime::from_timestamp(clock.load(Ordering::SeqCst), 0).unwrap());
        // Чужой файл в папке журналов не трогаем.
        std::fs::write(dir.join("notes.txt"), "keep me").unwrap();
        log.write_line(b"before midnight\n").unwrap();
        for _ in 0..4 {
            now.fetch_add(86_400, Ordering::SeqCst);
            log.write_line(b"next day\n").unwrap();
        }
        // Пять дней работы, хранится три последних.
        assert_eq!(
            files(&dir),
            [
                "exch.2026-11-12.log",
                "exch.2026-11-13.log",
                "exch.2026-11-14.log",
                "notes.txt"
            ]
        );
        let last = std::fs::read_to_string(dir.join("exch.2026-11-14.log")).unwrap();
        assert_eq!(last, "next day\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_restart_appends_to_the_file_of_the_day() {
        let dir = temp_dir("append");
        let fixed = || {
            DateTime::parse_from_rfc3339("2026-11-13T10:00:00Z")
                .unwrap()
                .to_utc()
        };
        DailyFile::new(&dir, 14)
            .unwrap()
            .with_clock(fixed)
            .write_line(b"first run\n")
            .unwrap();
        DailyFile::new(&dir, 14)
            .unwrap()
            .with_clock(fixed)
            .write_line(b"second run\n")
            .unwrap();
        let text = std::fs::read_to_string(dir.join("exch.2026-11-13.log")).unwrap();
        assert_eq!(text, "first run\nsecond run\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_event_is_written_as_one_line_through_tracing() {
        use tracing_subscriber::fmt::MakeWriter as _;
        let dir = temp_dir("event");
        let log = DailyFile::new(&dir, 14).unwrap();
        {
            let mut w = log.make_writer();
            w.write_all(b"part one, ").unwrap();
            w.write_all(b"part two\n").unwrap();
        }
        let name = file_name(Utc::now().date_naive());
        let text = std::fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(text, "part one, part two\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
