use std::{
    backtrace::Backtrace,
    fs, io,
    panic::{self, PanicHookInfo},
    path::{Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

const KEPT_REPORTS: usize = 5;
const REPORT_PREFIX: &str = "crash-";
const REPORT_SUFFIX: &str = ".log";

/// The hook must not lock application state: the panicking thread may hold it.
pub(crate) fn install(dir: PathBuf) {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let _ = write_report(&dir, unix_seconds, &report(info, unix_seconds));
        default(info);
    }));
}

fn report(info: &PanicHookInfo<'_>, unix_seconds: u64) -> String {
    let location = info.location().map_or_else(
        || "unknown".to_owned(),
        |location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        },
    );
    let (date, time) = utc(unix_seconds);
    format!(
        "taffymeters {version} panicked\n\
         time: {date} {time} UTC\n\
         thread: {thread}\n\
         location: {location}\n\
         message: {message}\n\n\
         backtrace:\n{backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        date = format_args!("{:04}-{:02}-{:02}", date[0], date[1], date[2]),
        time = format_args!("{:02}:{:02}:{:02}", time[0], time[1], time[2]),
        thread = thread::current().name().unwrap_or("unnamed"),
        message = info.payload_as_str().unwrap_or("(not a string)"),
        backtrace = Backtrace::force_capture(),
    )
}

/// The process ID separates panics within one second.
fn write_report(dir: &Path, unix_seconds: u64, report: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let ([year, month, day], [hour, minute, second]) = utc(unix_seconds);
    let path = dir.join(format!(
        "{REPORT_PREFIX}{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}-{}{REPORT_SUFFIX}",
        std::process::id()
    ));
    fs::write(&path, report)?;
    prune(dir)?;
    Ok(path)
}

fn prune(dir: &Path) -> io::Result<()> {
    let mut reports: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(REPORT_PREFIX) && name.ends_with(REPORT_SUFFIX)
                })
        })
        .collect();
    reports.sort();
    let excess = reports.len().saturating_sub(KEPT_REPORTS);
    for old in &reports[..excess] {
        fs::remove_file(old)?;
    }
    Ok(())
}

/// Howard Hinnant's `civil_from_days`, valid for any date after 1970.
fn utc(unix_seconds: u64) -> ([u64; 3], [u64; 3]) {
    let days = unix_seconds / 86_400;
    let second_of_day = unix_seconds % 86_400;
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (
        [year, month, day],
        [
            second_of_day / 3_600,
            second_of_day % 3_600 / 60,
            second_of_day % 60,
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates_cross_leap_days_and_century_ends() {
        assert_eq!(utc(0), ([1970, 1, 1], [0, 0, 0]));
        assert_eq!(utc(951_868_800), ([2000, 3, 1], [0, 0, 0]));
        assert_eq!(utc(1_709_210_096), ([2024, 2, 29], [12, 34, 56]));
        assert_eq!(utc(4_133_980_799), ([2100, 12, 31], [23, 59, 59]));
    }

    #[test]
    fn reports_keep_the_newest_five_and_leave_other_files() {
        let dir =
            std::env::temp_dir().join(format!("taffymeters-crash-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("session.json"), "{}").unwrap();
        let written: Vec<PathBuf> = (0..7)
            .map(|day| write_report(&dir, 1_709_210_096 + day * 86_400, "report").unwrap())
            .collect();
        let mut left: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        left.sort();
        let mut expected = written[2..].to_vec();
        expected.push(dir.join("session.json"));
        expected.sort();
        assert_eq!(left, expected);
        assert!(
            written[6]
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("crash-20240306-123456-")
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
