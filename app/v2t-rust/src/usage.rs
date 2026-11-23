use crate::config::atomic_write;
use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use time::{
    Date, Duration, OffsetDateTime,
    format_description::{BorrowedFormatItem, well_known::Rfc3339},
    macros::format_description,
};

const DATE: &[BorrowedFormatItem<'static>] = format_description!("[year]-[month]-[day]");
const TIMESTAMP: &[BorrowedFormatItem<'static>] = format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3][offset_hour sign:mandatory]:[offset_minute]"
);

pub fn local_now() -> OffsetDateTime {
    OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

const HEADER: [&str; 10] = [
    "started_at",
    "stage",
    "provider",
    "model",
    "outcome",
    "audio_seconds",
    "elapsed_seconds",
    "billed_seconds",
    "cost_usd",
    "cost_kind",
];

pub struct Entry<'a> {
    pub started_at: OffsetDateTime,
    pub stage: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub outcome: &'a str,
    pub audio_seconds: Option<f64>,
    pub elapsed: f64,
    pub billed_seconds: Option<f64>,
    pub cost: Option<f64>,
    pub cost_kind: &'a str,
}

pub struct Log {
    dir: PathBuf,
    cleaned_on: Option<Date>,
}

impl Log {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cleaned_on: None,
        }
    }

    pub fn append(&self, row: Entry<'_>) -> io::Result<()> {
        let date = row.started_at.date();
        if date < cutoff(local_now().date()) {
            return Ok(());
        }
        let daily = self.dir.join("usage");
        fs::create_dir_all(&daily)?;
        let path = daily.join(format!("{date}.csv"));
        let mut file = fs::File::options()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        let size = file.metadata()?.len();
        let mut preamble = Vec::new();
        if size != 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0u8];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                preamble.extend_from_slice(b"\r\n");
            }
        }
        let mut writer = csv::WriterBuilder::new()
            .terminator(csv::Terminator::CRLF)
            .from_writer(preamble);
        if size == 0 {
            writer.write_record(HEADER)?;
        }
        let finite = |n: Option<f64>| {
            n.filter(|value| value.is_finite() && *value >= 0.0)
                .map_or_else(String::new, |value| value.to_string())
        };
        writer.write_record([
            row.started_at.format(TIMESTAMP).map_err(io::Error::other)?,
            row.stage.into(),
            row.provider.into(),
            row.model.into(),
            row.outcome.into(),
            finite(row.audio_seconds),
            finite(Some(row.elapsed)),
            finite(row.billed_seconds),
            finite(row.cost),
            row.cost_kind.into(),
        ])?;
        let bytes = writer.into_inner().map_err(|error| error.into_error())?;
        file.seek(SeekFrom::End(0))?;
        let written = file.write_all(&bytes).and_then(|()| file.flush());
        if written.is_err() {
            let _ = file.set_len(size);
        }
        written
    }

    pub fn cleanup(&mut self) -> io::Result<()> {
        let today = local_now().date();
        if self.cleaned_on == Some(today) {
            return Ok(());
        }
        let oldest = cutoff(today);
        self.cleanup_daily(oldest)?;
        cleanup_legacy(&self.dir.join("usage.csv"), oldest)?;
        self.cleaned_on = Some(today);
        Ok(())
    }

    fn cleanup_daily(&self, cutoff: Date) -> io::Result<()> {
        let files = match fs::read_dir(self.dir.join("usage")) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in files {
            let entry = entry?;
            let name = entry.file_name();
            let date = name
                .to_str()
                .and_then(|label| label.strip_suffix(".csv"))
                .and_then(exact_date);
            if date.is_some_and(|date| date < cutoff) && entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }
}

fn exact_date(value: &str) -> Option<Date> {
    let date = Date::parse(value, DATE).ok()?;
    (date.to_string() == value).then_some(date)
}

fn cutoff(today: Date) -> Date {
    today.saturating_sub(Duration::days(6))
}

fn cleanup_legacy(path: &Path, oldest: Date) -> io::Result<()> {
    let Some(data) = crate::config::read_bounded(path, 64 * 1024 * 1024)? else {
        return Ok(());
    };
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(data.as_slice());
    let mut row = csv::ByteRecord::new();
    let mut retained = Vec::new();
    let mut unchanged = 0;
    while reader.read_byte_record(&mut row)? {
        let date = row
            .get(0)
            .and_then(|bytes| str::from_utf8(bytes).ok())
            .and_then(|value| {
                OffsetDateTime::parse(value, &Rfc3339)
                    .ok()
                    .map(|time| time.date())
            });
        if date.is_some_and(|date| date < oldest)
            && let Some(start) = row
                .position()
                .and_then(|pos| usize::try_from(pos.byte()).ok())
        {
            retained.extend_from_slice(&data[unchanged..start]);
            unchanged = reader.position().byte() as usize;
        }
    }
    if unchanged != 0 {
        retained.extend_from_slice(&data[unchanged..]);
        atomic_write(path, &retained)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cutoff_keeps_today_and_six_days_including_month_boundary() {
        assert_eq!(
            cutoff(exact_date("2026-12-03").unwrap()).to_string(),
            "2026-11-27"
        );
        assert!(exact_date("2026-12-3").is_none());
    }
}
