// Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
// Co-author: Cursor-AI.

//! # ts — Timesheet CLI
//!
//! Tracks work start/stop and reports time by activity and by day of week.
//! The log file lives at `$HOME/Documents/timesheet.log` by default.
//!
//! ## Log format
//!
//! One entry per line:
//!
//! - `ISO8601_timestamp|START|activity`
//! - `ISO8601_timestamp|STOP`
//!
//! The timestamp is the first field (strict ISO 8601, e.g. `2026-03-06T14:30:00-08:00`).
//!
//! Start/stop pairs are matched in LIFO order (each STOP pairs with the most recent START).
//!
//! ## Subcommands
//!
//! | Command    | Description |
//! |------------|-------------|
//! | `alias`    | Interactively replace activity text in this week's START entries (regex). |
//! | `autostart` | Register `ts start` on login and `ts stop` on logout/shutdown (macOS/Linux). |
//! | `help`     | Show the man page in a pager (groff -man -Tascii \| less). |
//! | `install`  | Copy binary and icon to a directory on PATH (icon embedded on macOS). |
//! | `interval` | Set or show reminder daemon interval (e.g. 3, 3m, 100s, 1h30m). |
//! | `list`     | Report % per activity and hours per weekday; optional file/extension arg. |
//! | `migrate`  | Convert all timesheet.* files in the log directory to strict ISO 8601 timestamps. |
//! | `tail`     | Last 10 log entries with timestamps in local time; optional file/extension arg. |
//! | `manpage`  | Output Unix manual page in groff format to stdout. |
//! | `rebuild`  | Build from local dir or clone; then install to current binary's directory. |
//! | `rename`   | Same as `alias`. |
//! | `restart`, `reminder` | Aliases for `interval`. |
//! | `rotate`   | Rename log to `timesheet.YYMMDD`; add STOP first if last entry is START; append if same-day exists. |
//! | `start`    | Record work start now; on macOS with no activity, shows reminder dialog to pick/enter; otherwise optional activity (default: misc/unspecified); starts/restarts reminder daemon. |
//! | `started`  | Record a past start time; inserts at the correct chronological position without discarding entries. |
//! | `stop`     | Record work stop (optional time); amends previous STOP if work already stopped; stops reminder daemon and shows "stopped" dialog when a stop is recorded (skipped during logout/shutdown). |
//! | `timeoff`  | Show stop time for 8 h/day average; only requires a START entry (adds one if log empty or last is STOP). |
//! | `uninstall` | Stop daemon, remove autostart hooks, optionally remove log files, remove binary and icon. |

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime};
use regex::Regex;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::thread;
use std::time::Duration;
#[cfg(unix)]
use libc::{kill, pthread_sigmask, setpgid, setsid, sigaddset, sigemptyset, signal, sigwait, SIG_BLOCK, SIG_IGN, SIGHUP, SIGKILL, SIGTERM};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "macos")]
use libc::getuid;

#[cfg(target_os = "macos")]
mod reminder_dialog_macos;

/// Default path segment under `$HOME` for the timesheet log file.
const DEFAULT_TIMESHEET: &str = "Documents/timesheet.log";

/// Icon for macOS reminder dock; embedded so "ts install" can write it without the repo.
#[cfg(target_os = "macos")]
const EMBEDDED_ICON_SVG: &[u8] = include_bytes!("../assets/icon.svg");

/// Weekday names for the list report (Sunday first).
const DAY_NAMES: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

/// Truncate hours to two decimal places (discard fractions beyond the second decimal).
fn trunc2(h: f64) -> f64 {
    (h * 100.0).trunc() / 100.0
}

/// Returns the default timesheet path: `$HOME/Documents/timesheet.log`, or `./Documents/timesheet.log` if `HOME` is unset.
fn timesheet_path() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_TIMESHEET)
}

/// Path for the reminder daemon PID file (under $HOME/.cache or $XDG_CACHE_HOME).
fn reminder_pid_path() -> PathBuf {
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
    cache
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ts-reminder.pid")
}

/// Path for the reminder interval config file (seconds as decimal string; same dir as PID file).
fn reminder_interval_path() -> PathBuf {
    reminder_pid_path()
        .parent()
        .unwrap_or(Path::new("."))
        .join("ts-reminder-interval")
}

/// Parse a duration string into seconds. E.g. "3", "3m" -> 180; "100s" -> 100; "1h30m" -> 5400.
/// Bare number is treated as minutes. Units: h, m, s (case-insensitive).
fn parse_interval_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("interval cannot be empty".to_string());
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut total_secs: u64 = 0;
    while i < bytes.len() {
        while i < bytes.len() && !bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let num: u64 = s[start..i]
            .parse()
            .map_err(|_| format!("invalid number in interval: {}", s))?;
        let unit = if i < bytes.len() {
            let u = bytes[i];
            if u == b'h' || u == b'H' || u == b'm' || u == b'M' || u == b's' || u == b'S' {
                i += 1;
                u
            } else {
                b'm'
            }
        } else {
            b'm'
        };
        match unit {
            b'h' | b'H' => total_secs += num * 3600,
            b'm' | b'M' => total_secs += num * 60,
            b's' | b'S' => total_secs += num,
            _ => total_secs += num * 60,
        }
    }
    if total_secs == 0 {
        return Err("interval must be positive".to_string());
    }
    Ok(total_secs)
}

/// Activities from the current week's START entries, most-recently logged first (by last occurrence).
fn activities_this_week_most_recent_first(timesheet: &Path) -> Vec<String> {
    let now = Local::now();
    let week_start_dt = week_start(now);
    let week_end = week_start_dt + chrono::Duration::weeks(1) - chrono::Duration::seconds(1);
    let content = match fs::read_to_string(timesheet) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut by_activity: std::collections::HashMap<String, DateTime<Local>> = std::collections::HashMap::new();
    for line in content.lines() {
        if let Some(LogLine::Start(dt, activity)) = parse_line(line) {
            if dt >= week_start_dt && dt <= week_end {
                by_activity.insert(activity.clone(), dt);
            }
        }
    }
    let mut order: Vec<(String, DateTime<Local>)> = by_activity.into_iter().collect();
    order.sort_by(|a, b| b.1.cmp(&a.1));
    order.into_iter().map(|(a, _)| a).collect()
}

/// Append a START log entry for the given activity (used by reminder daemon). Calls maybe_rotate first.
fn append_start_entry(timesheet: &Path, activity: &str) -> Result<(), String> {
    maybe_rotate_if_previous_week(timesheet)?;
    let now = Local::now();
    let line = format!("{}|START|{}\n", now.to_rfc3339(), activity);
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(timesheet)
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

/// Append a STOP log entry at the given datetime (used by reminder daemon on timeout/shutdown). Calls maybe_rotate first.
fn append_stop_entry(timesheet: &Path, dt: DateTime<Local>) -> Result<(), String> {
    maybe_rotate_if_previous_week(timesheet)?;
    let line = format!("{}|STOP\n", dt.to_rfc3339());
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(timesheet)
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

/// DateTime of Sunday 00:00:00 for the week containing `now` (local time).
fn week_start(now: DateTime<Local>) -> DateTime<Local> {
    let today = now.date_naive();
    let dow = today.weekday().num_days_from_sunday() as u64;
    today
        .checked_sub_days(chrono::Days::new(dow))
        .unwrap_or(today)
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
}

/// Parses a timestamp field: strict ISO 8601 (RFC 3339) only.
/// The wall-clock time in the stored offset is treated as local time without
/// any conversion through UTC.
fn parse_timestamp_field(s: &str) -> Option<DateTime<Local>> {
    let s = s.trim();
    DateTime::parse_from_rfc3339(s)
        .ok()
        .and_then(|dt| dt.naive_local().and_local_timezone(Local).single())
}

/// A single parsed line from the timesheet log.
#[derive(Clone, Debug)]
enum LogLine {
    /// `timestamp|START|activity`
    Start(DateTime<Local>, String),
    /// `timestamp|STOP`
    Stop(DateTime<Local>),
}

/// Parses a log line into `LogLine::Start(dt, activity)` or `LogLine::Stop(dt)`; returns `None` if not a valid START/STOP line.
/// Format: timestamp (ISO 8601) is the first field, then START|activity or STOP.
fn parse_line(s: &str) -> Option<LogLine> {
    let s = s.trim();
    let mut parts = s.splitn(3, '|');
    let ts = parts.next()?;
    let dt = parse_timestamp_field(ts)?;
    match parts.next()? {
        "START" => Some(LogLine::Start(dt, parts.next().unwrap_or("").to_string())),
        "STOP" => Some(LogLine::Stop(dt)),
        _ => None,
    }
}

/// DateTime from the last START or STOP line in the file, or `None` if empty/unreadable.
fn last_line_dt(path: &Path) -> Option<DateTime<Local>> {
    let content = fs::read_to_string(path).ok()?;
    let line = content.lines().rev().find(|l| !l.trim().is_empty())?;
    match parse_line(line) {
        Some(LogLine::Start(dt, _)) | Some(LogLine::Stop(dt)) => Some(dt),
        None => None,
    }
}

/// Maximum DateTime among all START/STOP lines in the log; `None` if no valid entries.
fn max_dt_in_log(path: &Path) -> Option<DateTime<Local>> {
    let content = fs::read_to_string(path).ok()?;
    let mut max: Option<DateTime<Local>> = None;
    for line in content.lines() {
        match parse_line(line) {
            Some(LogLine::Start(dt, _)) | Some(LogLine::Stop(dt)) => {
                if max.map_or(true, |m| dt > m) {
                    max = Some(dt);
                }
            }
            None => {}
        }
    }
    max
}

/// Date range (min, max) of all START/STOP entries in the log; `None` if no valid entries.
fn date_range_in_log(path: &Path) -> Option<(NaiveDate, NaiveDate)> {
    let content = fs::read_to_string(path).ok()?;
    let mut min_dt: Option<DateTime<Local>> = None;
    let mut max_dt: Option<DateTime<Local>> = None;
    for line in content.lines() {
        match parse_line(line) {
            Some(LogLine::Start(dt, _)) | Some(LogLine::Stop(dt)) => {
                if min_dt.map_or(true, |m| dt < m) {
                    min_dt = Some(dt);
                }
                if max_dt.map_or(true, |m| dt > m) {
                    max_dt = Some(dt);
                }
            }
            None => {}
        }
    }
    match (min_dt, max_dt) {
        (Some(mn), Some(mx)) => Some((mn.date_naive(), mx.date_naive())),
        _ => None,
    }
}

/// Rotates the log: renames it to `timesheet.YYMMDD` using the most recent entry's date.
/// If that file already exists (same day), appends the current log to it and removes the source.
/// If the last entry is START (work in progress), appends a STOP at current time before rotating.
fn do_rotate(timesheet: &Path) -> Result<(), String> {
    if !timesheet.exists() {
        return Err("ts rotate: no timesheet data found.".to_string());
    }
    let content = fs::read_to_string(timesheet).map_err(|e| e.to_string())?;
    let last = content.lines().rev().find(|l| !l.trim().is_empty());
    if last.and_then(parse_line).map(|ll| matches!(ll, LogLine::Start(..))).unwrap_or(false) {
        let now = Local::now();
        let mut f = fs::OpenOptions::new().append(true).open(timesheet).map_err(|e| e.to_string())?;
        f.write_all(format!("{}|STOP\n", now.to_rfc3339()).as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let max_dt = max_dt_in_log(timesheet).ok_or("ts rotate: no valid entries in timesheet.")?;
    let stamp = max_dt.format("%y%m%d").to_string();
    let parent = timesheet.parent().ok_or("ts rotate: no parent dir")?;
    let stem = timesheet.file_stem().and_then(|s| s.to_str()).unwrap_or("timesheet");
    let dest = parent.join(format!("{}.{}", stem, stamp));
    let content = fs::read_to_string(timesheet).map_err(|e| e.to_string())?;
    if dest.exists() {
        let mut f = fs::OpenOptions::new().append(true).open(&dest).map_err(|e| e.to_string())?;
        f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
        fs::remove_file(timesheet).map_err(|e| e.to_string())?;
        println!("Appended to {}", dest.display());
    } else {
        fs::rename(timesheet, &dest).map_err(|e| e.to_string())?;
        println!("Rotated {} to {}", timesheet.display(), dest.display());
    }
    Ok(())
}

/// If the last log entry is from the previous week (before this week's Sunday 00:00), runs [`do_rotate`].
fn maybe_rotate_if_previous_week(timesheet: &Path) -> Result<(), String> {
    if !timesheet.exists() {
        return Ok(());
    }
    let last_dt = match last_line_dt(timesheet) {
        Some(d) => d,
        None => return Ok(()),
    };
    let now = Local::now();
    let week_start = week_start(now);
    if last_dt < week_start {
        do_rotate(timesheet)?;
    }
    Ok(())
}

/// Parses a line in either current format (timestamp first) or old format (START|ts|..., STOP|ts) for migration only.
fn migrate_parse_line(line: &str) -> Option<LogLine> {
    if let Some(ll) = parse_line(line) {
        return Some(ll);
    }
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("START|") {
        let mut parts = rest.splitn(2, '|');
        let dt = parse_timestamp_field(parts.next()?)?;
        let activity = parts.next().unwrap_or("").to_string();
        return Some(LogLine::Start(dt, activity));
    }
    if let Some(rest) = line.strip_prefix("STOP|") {
        let dt = parse_timestamp_field(rest.trim())?;
        return Some(LogLine::Stop(dt));
    }
    None
}

/// Converts all timesheet.* files in the timesheet directory to current format (timestamp first, ISO 8601).
fn cmd_migrate(timesheet: &Path) -> Result<(), String> {
    let dir = timesheet.parent().ok_or("ts migrate: no parent dir")?;
    if !dir.exists() {
        return Ok(());
    }
    let mut files: Vec<PathBuf> = Vec::new();
    if timesheet.exists() {
        files.push(timesheet.to_path_buf());
    }
    for e in fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let p = e.path();
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("timesheet.") && name != "timesheet.log" && p != timesheet {
                files.push(p);
            }
        }
    }
    for path in &files {
        let content = fs::read_to_string(path).map_err(|e| format!("ts migrate: read {}: {}", path.display(), e))?;
        let mut out = String::new();
        for line in content.lines() {
            let new_line = match migrate_parse_line(line) {
                Some(LogLine::Start(dt, activity)) => format!("{}|START|{}\n", dt.to_rfc3339(), activity),
                Some(LogLine::Stop(dt)) => format!("{}|STOP\n", dt.to_rfc3339()),
                None => {
                    if line.is_empty() {
                        String::new()
                    } else {
                        format!("{}\n", line)
                    }
                }
            };
            out.push_str(&new_line);
        }
        fs::write(path, &out).map_err(|e| format!("ts migrate: write {}: {}", path.display(), e))?;
        println!("Migrated {}", path.display());
    }
    if files.is_empty() {
        println!("No timesheet files to migrate.");
    }
    Ok(())
}

/// Resolves the optional list argument to a single timesheet file path.
///
/// - Empty / `None` → current timesheet.
/// - `"log"` → current timesheet.
/// - Existing path → that path.
/// - Otherwise: match by extension in the timesheet directory (e.g. `260220`, `20260220`, `0220`, `2/20`).
///   Returns an error if zero or multiple files match.
fn resolve_list_input(arg: Option<&str>, timesheet: &Path) -> Result<PathBuf, String> {
    let list_arg = match arg {
        Some(a) => a,
        None => {
            return Ok(timesheet.to_path_buf());
        }
    };
    if list_arg.is_empty() {
        return Ok(timesheet.to_path_buf());
    }
    if Path::new(list_arg).exists() {
        return Ok(PathBuf::from(list_arg));
    }
    if list_arg == "log" {
        return Ok(timesheet.to_path_buf());
    }
    let ts_dir = timesheet.parent().ok_or("no parent")?;
    let base = ts_dir.join("timesheet");
    let mut candidates: Vec<PathBuf> = Vec::new();
    if base.with_extension("log").exists() {
        candidates.push(base.with_extension("log"));
    }
    if let Ok(entries) = fs::read_dir(ts_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("timesheet.") && name != "timesheet.log"
                    && name.as_bytes().get(10).map(|&b| b.is_ascii_digit()).unwrap_or(false)
                {
                    candidates.push(p);
                }
            }
        }
    }
    let norm = if list_arg.len() == 8 && list_arg.chars().all(|c| c.is_ascii_digit()) {
        Some(list_arg[2..].to_string())
    } else if list_arg.len() == 6 && list_arg.chars().all(|c| c.is_ascii_digit()) {
        Some(list_arg.to_string())
    } else if list_arg.contains('/') {
        let parts: Vec<&str> = list_arg.splitn(2, '/').collect();
        if parts.len() == 2 {
            if let (Ok(m), Ok(d)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                let y = Local::now().format("%y").to_string();
                Some(format!("{}{:02}{:02}", y, m, d))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    let mut matches = Vec::new();
    for f in &candidates {
        let suffix = f
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("log")
            .to_string();
        if list_arg == suffix
            || suffix.contains(list_arg)
            || list_arg.contains(&suffix)
            || norm.as_ref().map(|n| n == &suffix).unwrap_or(false)
        {
            matches.push(f.clone());
        }
    }
    if matches.len() == 1 {
        return Ok(matches.into_iter().next().unwrap());
    }
    if matches.len() > 1 {
        return Err(format!(
            "ts list: multiple timesheets match \"{}\".",
            list_arg
        ));
    }
    // No file matched by name/extension. If the arg looks like a date (e.g. 2/19 or YYMMDD),
    // find a timesheet whose entry date range includes that date (e.g. a later log that still has 2/19).
    let requested_date = norm.as_ref().and_then(|n| {
        if n.len() == 6 && n.chars().all(|c| c.is_ascii_digit()) {
            let yy: i32 = n[0..2].parse().ok()?;
            let mm: u32 = n[2..4].parse().ok()?;
            let dd: u32 = n[4..6].parse().ok()?;
            let year = 2000 + yy; // 00..99 -> 2000..2099
            NaiveDate::from_ymd_opt(year, mm, dd)
        } else {
            None
        }
    });
    if let Some(want) = requested_date {
        // Try requested date and same month/day in adjacent years (e.g. 2/19 in current year and ±1).
        let (mm, dd) = (want.month(), want.day());
        let want_prev = NaiveDate::from_ymd_opt(want.year() - 1, mm, dd);
        let want_next = NaiveDate::from_ymd_opt(want.year() + 1, mm, dd);
        let dates_to_try: Vec<NaiveDate> = [Some(want), want_prev, want_next]
            .into_iter()
            .flatten()
            .collect();
        let mut containing: Vec<(PathBuf, NaiveDate, u8)> = Vec::new(); // (path, max_d, priority: 0=want, 1=next, 2=prev)
        for path in &candidates {
            if let Some((min_d, max_d)) = date_range_in_log(path) {
                for (priority, &d) in dates_to_try.iter().enumerate() {
                    if d >= min_d && d <= max_d {
                        containing.push((path.clone(), max_d, priority as u8));
                        break;
                    }
                }
            }
        }
        // Prefer match for requested year, then smallest max_date (the "current" log as of that day).
        if let Some((path, _, _)) = containing
            .into_iter()
            .min_by_key(|(_, max_d, priority)| (*priority, *max_d))
        {
            return Ok(path);
        }
        // Content-based found nothing (e.g. empty rotated file). Fall back to extension-as-date:
        // use a file whose extension is YYMMDD on or after the requested date (the log that was current then).
        let mut by_ext_date: Vec<(PathBuf, NaiveDate)> = Vec::new();
        for path in &candidates {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.len() == 6 && ext.chars().all(|c| c.is_ascii_digit()) {
                if let (Ok(yy), Ok(mm), Ok(dd)) = (
                    ext[0..2].parse::<i32>(),
                    ext[2..4].parse::<u32>(),
                    ext[4..6].parse::<u32>(),
                ) {
                    if let Some(ext_date) = NaiveDate::from_ymd_opt(2000 + yy, mm, dd) {
                        if ext_date >= want {
                            by_ext_date.push((path.clone(), ext_date));
                        }
                    }
                }
            }
        }
        if let Some((path, _)) = by_ext_date.into_iter().min_by_key(|(_, d)| *d) {
            return Ok(path);
        }
    }
    Err(format!("ts list: no timesheet matches \"{}\".", list_arg))
}

/// Records work start now; activity is optional. With no argument on macOS, shows the reminder dialog to pick/enter an activity.
/// On other platforms or if the user declines, falls back to misc/unspecified.
/// Ensures the reminder daemon is running at entry (so it stays running even when ts start is run at system startup and
/// exits before the final start call), then restarts it after recording START to reset the timer.
fn cmd_start(args: &[String], timesheet: &Path) -> Result<(), String> {
    maybe_rotate_if_previous_week(timesheet)?;
    // Guard against shutdown/reload race: if auto-invoked (no args) and the last log
    // entry is a very recent STOP, skip — launchd is re-firing RunAtLoad during shutdown,
    // not a genuine login.
    if args.is_empty() {
        let content = fs::read_to_string(timesheet).unwrap_or_default();
        let last = content.lines().rev().find(|l| !l.trim().is_empty());
        if let Some(LogLine::Stop(dt)) = last.and_then(parse_line) {
            let age = Local::now().signed_duration_since(dt).num_seconds();
            if age >= 0 && age < 60 {
                if env::var_os("TS_DEBUG").is_some() {
                    let _ = std::io::stderr().write_all(
                        b"ts: skipping start: last STOP was <60s ago (shutdown/reload guard)\n",
                    );
                }
                return Ok(());
            }
        }
    }
    // Start daemon early so it is running even when ts start is invoked at login (LaunchAgent) and exits quickly.
    start_reminder_daemon_if_needed(timesheet);
    let activity = if args.is_empty() {
        #[cfg(all(target_os = "macos", not(test)))]
        {
            let activities = activities_this_week_most_recent_first(timesheet);
            loop {
                match show_reminder_prompt(&activities, Some(timesheet)) {
                    ReminderResult::Activity(a) => break a,
                    ReminderResult::DontBugMe => {
                        kill_reminder_daemon_if_running();
                        return Ok(());
                    }
                    ReminderResult::ShowAgainImmediate => {} // re-show immediately
                    ReminderResult::TimeoutAddStop(dt) => {
                        let _ = append_stop_entry(timesheet, dt);
                        // re-show immediately
                    }
                    ReminderResult::EnterNew => unreachable!("show_reminder_prompt converts EnterNew to Activity"),
                }
            }
        }
        #[cfg(any(not(target_os = "macos"), test))]
        "misc/unspecified".to_string()
    } else {
        args.join(" ")
    };
    let now = Local::now();
    // Close any open session before starting a new one.
    {
        let content = fs::read_to_string(timesheet).unwrap_or_default();
        let last = content.lines().rev().find(|l| !l.trim().is_empty());
        if last.and_then(parse_line).map(|ll| matches!(ll, LogLine::Start(_, _))).unwrap_or(false) {
            let stop_line = format!("{}|STOP\n", now.to_rfc3339());
            if let Ok(mut sf) = fs::OpenOptions::new().append(true).open(timesheet) {
                let _ = sf.write_all(stop_line.as_bytes());
            }
        }
    }
    let line = format!("{}|START|{}\n", now.to_rfc3339(), activity);
    let mut f = fs::OpenOptions::new().create(true).append(true).open(timesheet).map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    println!("Started: {} at {}", activity, Local::now().format("%a %b %d %H:%M:%S %Z %Y"));
    kill_reminder_daemon_if_running();
    thread::sleep(Duration::from_millis(100));
    start_reminder_daemon_if_needed(timesheet);
    Ok(())
}

/// Records work stop at the given time (or now if no time given). Same time formats as `ts started`.
/// If the last entry is already STOP: no stop-time argument → no change; with stop-time → amend that entry.
fn cmd_stop(args: &[String], timesheet: &Path) -> Result<(), String> {
    maybe_rotate_if_previous_week(timesheet)?;
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    let last = content.lines().rev().find(|l| !l.trim().is_empty());
    if last.and_then(parse_line).map(|ll| matches!(ll, LogLine::Stop(_))).unwrap_or(false) {
        let Some(t) = args.first().map(String::as_str) else {
            return Ok(());
        };
        let stop_dt = parse_start_time(t).ok_or_else(|| format!("ts stop: could not parse stop time: {}", t))?;
        let lines: Vec<&str> = content.lines().collect();
        let without_last = if lines.is_empty() {
            String::new()
        } else {
            lines[..lines.len() - 1].join("\n") + "\n"
        };
        let new_content = format!("{}{}|STOP\n", without_last, stop_dt.to_rfc3339());
        fs::write(timesheet, &new_content).map_err(|e| e.to_string())?;
        if is_reminder_daemon_running() {
            show_reminders_stopped_notification();
        }
        kill_reminder_daemon_if_running();
        println!("Stopped at {}", stop_dt.format("%a %b %d %H:%M:%S %Z %Y"));
        return Ok(());
    }
    let stop_dt = match args.first().map(String::as_str) {
        Some(t) => parse_start_time(t).ok_or_else(|| format!("ts stop: could not parse stop time: {}", t))?,
        None => Local::now(),
    };
    let line = format!("{}|STOP\n", stop_dt.to_rfc3339());
    let mut f = fs::OpenOptions::new().create(true).append(true).open(timesheet).map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    if is_reminder_daemon_running() {
        show_reminders_stopped_notification();
    }
    kill_reminder_daemon_if_running();
    println!("Stopped at {}", stop_dt.format("%a %b %d %H:%M:%S %Z %Y"));
    Ok(())
}

fn process_log_for_report(lines: &[(usize, LogLine)], virtual_stop: Option<DateTime<Local>>) -> (Vec<(String, f64, f64)>, Vec<f64>, bool) {
    let mut stack: Vec<(DateTime<Local>, String)> = Vec::new();
    let mut act_sec: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut dow_sec: [f64; 7] = [0.0; 7];
    for x in lines.iter() {
        let (ll, _line) = (x.1.clone(), x);
        match &ll {
            LogLine::Start(dt, a) => {
                if let Some((start_dt, start_act)) = stack.pop() {
                    let dur = (*dt - start_dt).num_seconds();
                    if dur > 0 {
                        *act_sec.entry(start_act).or_insert(0) += dur;
                        let dow = start_dt.weekday().num_days_from_sunday() as usize;
                        dow_sec[dow] += dur as f64;
                    }
                }
                stack.push((*dt, a.clone()));
            }
            LogLine::Stop(dt) => {
                if let Some((start_dt, start_act)) = stack.pop() {
                    let dur = (*dt - start_dt).num_seconds();
                    if dur > 0 {
                        *act_sec.entry(start_act).or_insert(0) += dur;
                        let dow = start_dt.weekday().num_days_from_sunday() as usize;
                        dow_sec[dow] += dur as f64;
                    }
                }
            }
        }
    }
    if let Some(vstop) = virtual_stop {
        if let Some((start_dt, start_act)) = stack.pop() {
            let dur = (vstop - start_dt).num_seconds();
            if dur > 0 {
                *act_sec.entry(start_act).or_insert(0) += dur;
                let dow = start_dt.weekday().num_days_from_sunday() as usize;
                dow_sec[dow] += dur as f64;
            }
        }
    }
    let total: i64 = act_sec.values().sum();
    let work_in_progress = !stack.is_empty();
    let mut by_act: Vec<(String, f64, f64)> = act_sec
        .into_iter()
        .map(|(a, s)| {
            let sec = s as f64;
            let pct = 100.0 * sec / total as f64;
            let hr = sec / 3600.0;
            (a, pct, hr)
        })
        .collect();
    by_act.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let dow_hr: Vec<f64> = dow_sec.iter().map(|s| s / 3600.0).collect();
    (by_act, dow_hr, work_in_progress)
}

/// Outputs the latest ten log entries with timestamps shown in local time. Optional arg selects file (same as list).
/// Consecutive START entries with the same activity are collapsed (first timestamp kept for aggregate duration); then the last 10 entries are shown.
fn cmd_tail(tail_arg: Option<&str>, timesheet: &Path) -> Result<(), String> {
    let path = resolve_list_input(tail_arg, timesheet)?;
    if !path.exists() {
        println!("No timesheet data found.");
        return Ok(());
    }
    let content = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let entries: Vec<LogLine> = content
        .lines()
        .filter_map(|line| parse_line(line))
        .collect();
    let mut dedup: Vec<LogLine> = Vec::new();
    for ll in &entries {
        match ll {
            LogLine::Start(_epoch, activity) => {
                if let Some(LogLine::Start(_, prev_act)) = dedup.last() {
                    if prev_act == activity {
                        continue; // keep the first timestamp of the consecutive run
                    }
                }
                dedup.push(ll.clone());
            }
            LogLine::Stop(epoch) => {
                dedup.push(LogLine::Stop(*epoch));
            }
        }
    }
    let last_ten: Vec<&LogLine> = dedup.iter().rev().take(10).rev().collect();
    let now = Local::now();
    let fmt_duration = |secs: i64| -> String {
        if secs >= 3600 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else if secs >= 60 {
            format!("{}m", secs / 60)
        } else {
            format!("{}s", secs)
        }
    };
    let duration_for = |i: usize, ll: &&LogLine| -> String {
        let dt = match ll {
            LogLine::Start(dt, _) => dt,
            LogLine::Stop(dt) => dt,
        };
        let end = last_ten.get(i + 1).and_then(|n| match n {
            LogLine::Stop(e) => Some(*e),
            LogLine::Start(e, _) => Some(*e),
        }).unwrap_or(now);
        fmt_duration((end - *dt).num_seconds())
    };
    let mut max_duration_width = 0usize;
    for (i, ll) in last_ten.iter().enumerate() {
        max_duration_width = max_duration_width.max(duration_for(i, ll).len());
    }
    for (i, ll) in last_ten.iter().enumerate() {
        let dur = duration_for(i, ll);
        match ll {
            LogLine::Start(dt, activity) => {
                println!("START  {}  {:>width$}  {}", dt.format("%Y-%m-%d %H:%M:%S"), dur, activity, width = max_duration_width);
            }
            LogLine::Stop(dt) => {
                println!("STOP   {}  {:>width$}", dt.format("%Y-%m-%d %H:%M:%S"), dur, width = max_duration_width);
            }
        }
    }
    Ok(())
}

/// Prints report: % per activity and hours per weekday; optional arg selects file (e.g. `log`, `0220`, path).
fn cmd_list(list_arg: Option<&str>, timesheet: &Path) -> Result<(), String> {
    if env::var_os("TS_DEBUG").is_some() {
        let _ = std::io::stderr().write_all(b"ts: cmd_list entered\n");
    }
    let list_input = resolve_list_input(list_arg, timesheet)?;
    if !list_input.exists() {
        println!("No timesheet data found.");
        return Ok(());
    }
    let content = fs::read_to_string(&list_input).map_err(|e| e.to_string())?;
    let mut lines: Vec<(usize, LogLine)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if let Some(ll) = parse_line(line) {
            lines.push((i + 1, ll));
        }
    }
    let is_current = list_arg.is_none() || list_arg == Some("log");
    let last_start = lines.iter().rev().find(|(_, l)| matches!(l, LogLine::Start(_, _)));
    let virtual_stop = if is_current && last_start.is_some() {
        Some(Local::now())
    } else {
        None
    };
    let (by_act, dow_hr, work_in_progress) = process_log_for_report(&lines, virtual_stop);
    if by_act.is_empty() {
        println!("No work recorded.");
        return Ok(());
    }
    for (act, pct, hr) in &by_act {
        println!("{:.1}%  {:.2}h  {}", pct, hr, act);
    }
    for (i, name) in DAY_NAMES.iter().enumerate() {
        println!("{}  {:.2}", name, dow_hr.get(i).copied().unwrap_or(0.0));
    }
    let total_hr: f64 = dow_hr.iter().map(|&h| trunc2(h)).sum();
    println!("Total  {:.2}", trunc2(total_hr));
    if is_current && work_in_progress {
        if let Some((_, LogLine::Start(start_dt, activity))) = last_start {
            let now = Local::now();
            let dur_sec = (now - *start_dt).num_seconds();
            let dur_min = dur_sec / 60;
            let dur_hr = dur_min / 60;
            let dur_rem = dur_min % 60;
            let duration_fmt = if dur_hr > 0 {
                format!("{}h {}m", dur_hr, dur_rem)
            } else {
                format!("{}m", dur_min)
            };
            println!(
                "\nCurrent Task: {}, started {}, worked {}",
                activity,
                start_dt.format("%a %b %d %H:%M:%S %Z %Y"),
                duration_fmt
            );
        }
    }
    Ok(())
}

/// Parses a start-time string into a DateTime<Local>; tries strict ISO 8601 first, then several other formats (e.g. `%Y-%m-%d %H:%M`, `%H:%M`, `%I:%M %p`).
fn parse_start_time(s: &str) -> Option<DateTime<Local>> {
    let s = s.trim();
    if let Some(dt) = parse_timestamp_field(s) {
        return Some(dt);
    }
    let now = Local::now();
    let today = now.date_naive();
    let formats = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%H:%M",
        "%H:%M:%S",
        "%m/%d/%Y %H:%M",
        "%m/%d %H:%M",
        "%I:%M %p",
        "%I:%M%p",
    ];
    for fmt in formats {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            return ndt.and_local_timezone(Local).single();
        }
    }
    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M") {
        return today.and_time(t).and_local_timezone(Local).single();
    }
    if let Ok(t) = NaiveTime::parse_from_str(s, "%I:%M %p") {
        return today.and_time(t).and_local_timezone(Local).single();
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).unwrap().and_local_timezone(Local).single();
    }
    None
}

/// Records a past start time; inserts the new entry at the correct chronological position
/// without discarding any existing entries.
fn cmd_started(args: &[String], timesheet: &Path) -> Result<(), String> {
    let (start_time, activity) = match args.split_first() {
        Some((st, rest)) => (st.as_str(), rest.join(" ")),
        None => {
            eprintln!("Usage: ts started <start_time> [activity...]");
            eprintln!("  start_time is required (e.g. \"2025-02-16 09:00\" or \"9:00 AM\").");
            return Err("missing start_time".to_string());
        }
    };
    let activity = if activity.is_empty() {
        "misc/unspecified".to_string()
    } else {
        activity
    };
    let start_dt = parse_start_time(start_time).ok_or_else(|| format!("ts started: could not parse start time: {}", start_time))?;
    maybe_rotate_if_previous_week(timesheet)?;
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    let new_entry = format!("{}|START|{}", start_dt.to_rfc3339(), activity);

    let mut result: Vec<&str> = Vec::new();
    let mut inserted = false;
    for line in content.lines() {
        if !inserted {
            if let Some(ll) = parse_line(line) {
                let line_dt = match &ll {
                    LogLine::Start(dt, _) => *dt,
                    LogLine::Stop(dt) => *dt,
                };
                if line_dt > start_dt {
                    result.push(&new_entry);
                    inserted = true;
                }
            }
        }
        result.push(line);
    }
    if !inserted {
        result.push(&new_entry);
    }
    let new_content = result.join("\n") + "\n";
    fs::write(timesheet, new_content).map_err(|e| e.to_string())?;
    println!("Started: {} at {}", activity, start_dt.format("%a %b %d %H:%M:%S %Z %Y"));
    start_reminder_daemon_if_needed(timesheet);
    Ok(())
}

/// Shows stop time for 8 h/day average. Requires only a START entry (work in progress); no completed
/// session on the current day is required. If the log is empty or the last entry is STOP, appends a START first.
fn cmd_timeoff(timesheet: &Path) -> Result<(), String> {
    maybe_rotate_if_previous_week(timesheet)?;
    let needs_start = if timesheet.exists() {
        let content = fs::read_to_string(timesheet).unwrap_or_default();
        let last = content.lines().rev().find(|l| !l.trim().is_empty());
        last.and_then(parse_line).map(|ll| matches!(ll, LogLine::Stop(_))).unwrap_or(true) // empty or last is STOP -> need START
    } else {
        true
    };
    if needs_start {
        if let Some(parent) = timesheet.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let now = Local::now();
        let line = format!("{}|START|misc/unspecified\n", now.to_rfc3339());
        let mut f = fs::OpenOptions::new().create(true).append(true).open(timesheet).map_err(|e| e.to_string())?;
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    }
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    let mut stack: Vec<(DateTime<Local>, String)> = Vec::new();
    let mut total_sec: i64 = 0;
    let mut day_seen: std::collections::HashSet<NaiveDate> = std::collections::HashSet::new();
    let mut lines: Vec<LogLine> = Vec::new();
    for line in content.lines() {
        if let Some(ll) = parse_line(line) {
            lines.push(ll);
        }
    }
    let now = Local::now();
    let mut effective = lines.clone();
    if let Some(LogLine::Start(_, _)) = lines.last() {
        effective.push(LogLine::Stop(now));
    }
    for line in &effective {
        match line {
            LogLine::Start(e, a) => {
                if let Some((start_dt, _)) = stack.pop() {
                    let dur = (*e - start_dt).num_seconds();
                    if dur > 0 {
                        total_sec += dur;
                    }
                    day_seen.insert(start_dt.date_naive()); // count day even if dur == 0 (e.g. just started)
                }
                stack.push((*e, a.clone()));
            }
            LogLine::Stop(e) => {
                if let Some((start_dt, _)) = stack.pop() {
                    let dur = (*e - start_dt).num_seconds();
                    if dur > 0 {
                        total_sec += dur;
                    }
                    day_seen.insert(start_dt.date_naive());
                }
            }
        }
    }
    let num_days = day_seen.len() as f64;
    if num_days == 0.0 {
        println!("No work recorded.");
        return Ok(());
    }
    let total_hr_worked = trunc2(total_sec as f64 / 3600.0);
    let target_hr = trunc2(8.0 * num_days);
    let need_hr = trunc2(target_hr - total_hr_worked);
    if need_hr <= 0.0 {
        println!("Average already at least 8 hours per day worked. You may stop now.");
        println!("{}", Local::now().format("%a %b %d %H:%M:%S %Z %Y"));
        return Ok(());
    }
    let stop_dt = now + chrono::Duration::seconds((need_hr * 3600.0) as i64);
    println!("Stop at: {}", stop_dt.format("%a %b %d %H:%M:%S %Z %Y"));
    println!(
        "({:.2} hours remaining for 8h/day average over {} day(s))",
        need_hr, num_days
    );
    Ok(())
}

/// Interactively replace activity text in this week's START entries; pattern is a regex, prompt Replace (y/n) per match.
/// Used by both `alias` and `rename` subcommands.
fn cmd_workalias(args: &[String], timesheet: &Path) -> Result<(), String> {
    let (pattern, replacement) = match args {
        [p, r, ..] => (p.as_str(), r.to_string()),
        _ => {
            eprintln!("Usage: ts alias <pattern> <replacement>");
            eprintln!("       ts rename <pattern> <replacement>");
            return Err("missing args".to_string());
        }
    };
    if !timesheet.exists() {
        return Err("ts alias: no timesheet data found.".to_string());
    }
    let now = Local::now();
    let week_start_dt = week_start(now);
    let week_end = week_start_dt + chrono::Duration::weeks(1) - chrono::Duration::seconds(1);
    let re = Regex::new(pattern).map_err(|e| format!("invalid pattern: {}", e))?;
    let content = fs::read_to_string(timesheet).map_err(|e| e.to_string())?;
    let mut matches_vec: Vec<(usize, DateTime<Local>, String)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if let Some(LogLine::Start(dt, activity)) = parse_line(line) {
            if dt >= week_start_dt && dt <= week_end && re.is_match(&activity) {
                matches_vec.push((i + 1, dt, replacement.clone()));
            }
        }
    }
    if matches_vec.is_empty() {
        return Err(format!(
            "ts alias: no activities matching \"{}\" found for this week.",
            pattern
        ));
    }
    let lines_vec: Vec<&str> = content.lines().collect();
    let mut replace_lines: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for (line_num, dt, new_repl) in &matches_vec {
        let orig_activity = lines_vec
            .get(*line_num - 1)
            .and_then(|l| parse_line(l))
            .and_then(|ll| match ll {
                LogLine::Start(_, a) => Some(a),
                _ => None,
            })
            .unwrap_or_default();
        let end_dt = lines_vec
            .get(*line_num)
            .and_then(|l| parse_line(l))
            .map(|ll| match ll {
                LogLine::Start(e, _) | LogLine::Stop(e) => e,
            })
            .unwrap_or(now);
        let secs = (end_dt - *dt).num_seconds();
        let duration_fmt = if secs >= 3600 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else if secs >= 60 {
            format!("{}m", secs / 60)
        } else {
            format!("{}s", secs)
        };
        println!(
            "Original:  {}  {:>8}  {}",
            dt.format("%Y-%m-%d %H:%M:%S"),
            duration_fmt,
            orig_activity
        );
        println!(
            "Replaced:  {}  {:>8}  {}",
            dt.format("%Y-%m-%d %H:%M:%S"),
            duration_fmt,
            new_repl
        );
        print!("Replace (y/n) ");
        stdout.flush().map_err(|e| e.to_string())?;
        let mut buf = String::new();
        if stdin.lock().read_line(&mut buf).is_ok()
            && buf.trim().eq_ignore_ascii_case("y")
        {
            replace_lines.insert(*line_num);
        }
    }
    if replace_lines.is_empty() {
        return Ok(());
    }
    let mut out = String::new();
    for (i, line) in content.lines().enumerate() {
        let line_no = i + 1;
        let should_replace = replace_lines.contains(&line_no);
        if should_replace {
            if let Some(LogLine::Start(dt, activity)) = parse_line(line) {
                if dt >= week_start_dt && dt <= week_end && re.is_match(&activity) {
                    out.push_str(&format!("{}|START|{}\n", dt.to_rfc3339(), replacement));
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    fs::write(timesheet, out).map_err(|e| e.to_string())?;
    Ok(())
}

/// Copies the binary to a directory on PATH (first writable) or the given directory.
fn cmd_install(args: &[String]) -> Result<(), String> {
    let dest_dir = args.first().map(String::as_str);
    let repo_path = args.get(1).map(String::as_str);
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let script_dir = repo_path
        .map(PathBuf::from)
        .unwrap_or_else(|| exe.parent().unwrap_or(Path::new(".")).to_path_buf());
    let dest = if let Some(d) = dest_dir {
        let p = PathBuf::from(d);
        if !p.exists() {
            fs::create_dir_all(&p).map_err(|e| format!("ts install: cannot create directory {}: {}", d, e))?;
        }
        if !p.is_dir() || !is_writable(&p) {
            return Err(format!("ts install: directory is not writable: {}", d));
        }
        p
    } else {
        let path_env = env::var_os("PATH").unwrap_or_default();
        let mut found = None;
        for dir in env::split_paths(&path_env) {
            let d = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                &dir
            };
            if d.is_dir() && is_writable(d) {
                found = Some(d.to_path_buf());
                break;
            }
        }
        found.ok_or("ts install: no writable directory on PATH. Specify an installation directory.")?
    };
    let src = script_dir.join("ts");
    let src_exe = if script_dir == exe.parent().unwrap_or(Path::new(".")) {
        exe.clone()
    } else {
        script_dir.join(if cfg!(windows) { "ts.exe" } else { "ts" })
    };
    let src_to_use = if src.exists() {
        &src
    } else if src_exe.exists() {
        &src_exe
    } else {
        &exe
    };
    if !src_to_use.exists() {
        return Err(format!("ts install: missing {}", src_to_use.display()));
    }
    let dest_file = dest.join(if cfg!(windows) { "ts.exe" } else { "ts" });
    fs::copy(src_to_use, &dest_file).map_err(|e| format!("ts install: copy failed: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest_file).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest_file, perms).map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("xattr")
            .arg("-d")
            .arg("com.apple.quarantine")
            .arg(&dest_file)
            .output();
        let _ = Command::new("codesign")
            .arg("-s")
            .arg("-")
            .arg(&dest_file)
            .output();
        // Write embedded icon so reminder dialog shows timesheet icon in dock (works without repo).
        let dest_icon = dest.join("ts-icon.svg");
        if fs::write(&dest_icon, EMBEDDED_ICON_SVG).is_ok() {
            println!("Installed icon {}", dest_icon.display());
        }
    }
    println!("Installed {}", dest_file.display());
    println!("Done. ts is in {} and executable.", dest.display());
    Ok(())
}

/// Remove startup/shutdown/login/logout hooks that reference ts. No-op on unsupported platforms.
fn uninstall_autostart_hooks() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return do_autostart_uninstall_macos();
    #[cfg(target_os = "linux")]
    return do_autostart_uninstall_linux();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = ();
        Ok(())
    }
}

/// Stop reminder daemon, remove autostart hooks, optionally remove log files, then remove ts-icon.svg and the ts binary.
fn cmd_uninstall(args: &[String]) -> Result<(), String> {
    let _ = args;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let install_dir = exe.parent().ok_or("ts uninstall: could not determine install directory")?;

    println!("Uninstalling ts from {} ...", install_dir.display());

    if is_reminder_daemon_running() {
        show_reminders_stopped_notification();
    }
    kill_reminder_daemon_if_running();

    uninstall_autostart_hooks()?;

    let default_log = timesheet_path();
    if let Some(log_dir) = default_log.parent() {
        let mut log_files: Vec<PathBuf> = Vec::new();
        if default_log.exists() {
            log_files.push(default_log.clone());
        }
        if log_dir.exists() {
            if let Ok(entries) = fs::read_dir(log_dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with("timesheet.") && name != "timesheet.log" {
                            log_files.push(p);
                        }
                    }
                }
            }
        }
        if !log_files.is_empty() {
            println!(
                "Timesheet log files: {}",
                log_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            );
            print!("Remove timesheet log files? [y/N] ");
            let _ = io::stdout().flush();
            let mut line = String::new();
            if io::stdin().lock().read_line(&mut line).is_ok() {
                let answer = line.trim().to_lowercase();
                if answer == "y" || answer == "yes" {
                    for f in &log_files {
                        let _ = fs::remove_file(f);
                        println!("Removed {}", f.display());
                    }
                }
            }
        }
    }

    let icon_path = install_dir.join("ts-icon.svg");
    if icon_path.exists() {
        fs::remove_file(&icon_path).map_err(|e| format!("ts uninstall: could not remove icon: {}", e))?;
        println!("Removed {}", icon_path.display());
    }

    fs::remove_file(&exe).map_err(|e| format!("ts uninstall: could not remove binary: {}", e))?;
    println!("Removed {}", exe.display());
    println!("Uninstall complete.");
    Ok(())
}

fn is_writable(p: &Path) -> bool {
    fs::metadata(p).map(|m| !m.permissions().readonly()).unwrap_or(false)
}

/// Rebuild from a local directory or clone: run `cargo build --release` then install to current binary's dir.
/// If arg is a directory with Cargo.toml, build there. If arg is missing and current dir has Cargo.toml, build there.
/// If arg is missing and current dir has no Cargo.toml, clone the timesheet repo and build from the clone.
fn cmd_rebuild(args: &[String]) -> Result<(), String> {
    let install_dir = env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("ts rebuild: could not determine install directory")?
        .to_path_buf();

    let build_dir_arg = args.first().map(String::as_str).unwrap_or(".");
    let build_dir = if build_dir_arg == "." {
        env::current_dir().map_err(|e| format!("ts rebuild: {}", e))?
    } else {
        let p = PathBuf::from(build_dir_arg);
        if !p.exists() {
            return Err(format!("ts rebuild: no such directory: {}", p.display()));
        }
        if !p.is_dir() {
            return Err(format!("ts rebuild: not a directory: {}", p.display()));
        }
        p.canonicalize().map_err(|e| format!("ts rebuild: {}: {}", p.display(), e))?
    };

    let cargo_toml = build_dir.join("Cargo.toml");
    let build_dir = if cargo_toml.exists() {
        build_dir
    } else if args.is_empty() {
        // No arg and no Cargo.toml in current dir: clone repo
        let clone_parent = env::temp_dir().join(format!("ts-rebuild-{}", process::id()));
        if clone_parent.exists() {
            fs::remove_dir_all(&clone_parent).map_err(|e| e.to_string())?;
        }
        fs::create_dir_all(&clone_parent).map_err(|e| e.to_string())?;
        let status = Command::new("git")
            .args(["clone", "https://github.com/pillarsdotnet/timesheet"])
            .current_dir(&clone_parent)
            .status()
            .map_err(|e| format!("ts rebuild: git clone failed: {}", e))?;
        if !status.success() {
            return Err("ts rebuild: git clone failed.".to_string());
        }
        clone_parent.join("timesheet")
    } else {
        return Err(format!(
            "ts rebuild: no Cargo.toml in {}",
            build_dir.display()
        ));
    };

    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&build_dir)
        .status()
        .map_err(|e| format!("ts rebuild: cargo build failed: {}", e))?;
    if !status.success() {
        return Err("ts rebuild: cargo build failed.".to_string());
    }

    let exe = build_dir.join("target/release/ts");
    #[cfg(windows)]
    let exe = build_dir.join("target/release/ts.exe");
    if !exe.exists() {
        return Err(format!("ts rebuild: binary not found after build: {}", exe.display()));
    }

    let status = Command::new(&exe)
        .arg("install")
        .arg(&install_dir)
        .status()
        .map_err(|e| format!("ts rebuild: install failed: {}", e))?;
    if !status.success() {
        return Err("ts rebuild: install failed.".to_string());
    }

    println!("Rebuilt and installed to {}", install_dir.display());
    Ok(())
}

/// Groff man page source (shared by manpage and help).
fn manpage_content() -> &'static str {
    r#".TH TS 1 "February 2025" "" "ts"
.SH NAME
ts \- timesheet CLI (start, stop, list, report by activity and weekday)
.SH SYNOPSIS
.B ts
.I command
.RI [ args... ]
.PP
.B ts alias
.I pattern
.I replacement
.PP
.B ts autostart
.RI [ uninstall ]
.PP
.B ts help
.PP
.B ts install
.RI [ install_dir " [" repo_path ]]
.PP
.B ts uninstall
.PP
.B ts interval
.RI [ duration ]
.PP
.B ts list
.RI [ file_or_extension ]
.PP
.B ts tail
.RI [ file_or_extension ]
.PP
.B ts manpage
.PP
.B ts rebuild
.RI [ directory ]
.PP
.B ts rename
.I pattern
.I replacement
.PP
.B ts reminder
.RI [ duration ]
.PP
.B ts restart
.RI [ duration ]
.PP
.B ts rotate
.PP
.B ts start
.RI [ activity ]
.PP
.B ts started
.I start_time
.RI [ activity... ]
.PP
.B ts stop
.RI [ stop_time ]
.PP
.B ts stopped
.RI [ stop_time ]
.PP
.B ts timeoff
.SH DESCRIPTION
.B ts
tracks work start/stop and reports time by activity and by day of week.
The log file is
.BR $HOME /Documents/timesheet.log
by default (compile-time constant
.BR DEFAULT_TIMESHEET
in source).
.SH "LOG FORMAT"
One entry per line. The timestamp is the first field, strict ISO 8601 (e.g. 2026-03-06T14:30:00-08:00).
.TP
.B ISO8601_timestamp|START|activity
Record the start of a work session at the given time with the given activity name.
.TP
.B ISO8601_timestamp|STOP
Record the end of a work session at the given time.
.PP
Start/stop pairs are matched in LIFO order (each STOP pairs with the most recent START).
The report uses these pairs to compute duration and attribute time to activity and weekday.
.SH COMMANDS
.TP
.B alias
Interactively replace activity text in START entries from the current week.
.I pattern
is a regex;
.I replacement
is the replacement string.
For each match, prompts
.B Replace\ (y/n);
.B y
or
.B Y
applies the replacement. Errors if no matches this week.
.TP
.B autostart
[\fIinterval\fR]
Register
.B "ts start"
to run at login and
.B "ts stop"
to run at logout or system shutdown. Optional
.I interval
(e.g.\ \&5s, 3m) sets the reminder interval and starts the daemon in this session; if the daemon is already running, it is restarted so the new interval takes effect immediately.
On macOS installs two LaunchAgents and a logout hook:
.RS
.TP
\fBcom.ts.autostart.start\fR
Runs
.B "ts start"
at login (RunAtLoad, limited to Aqua sessions).
A shutdown guard skips the start if the last log entry is a STOP less than 60\ s old.
.TP
\fBcom.ts.autostart.session\fR
Runs
.B "ts \-\-session\-daemon"
as a persistent launchd job; on logout/shutdown launchd sends it SIGTERM and waits up to 30 s (ExitTimeOut) for it to write the STOP entry and exit.
.TP
\fBLogoutHook\fR
Runs as root before logout/shutdown and macOS blocks the shutdown sequence until it returns, providing a second guarantee that STOP is recorded. Uses
.B "launchctl asuser"
to invoke
.B "ts stop"
in the console user's launchd context. Requires sudo to register; if it cannot be set the command to run manually is printed.
.RE
On Linux uses systemd user services. With
.I uninstall
removes the registration.
Without
.I interval
: starts the daemon if not running and prints the current reminder interval.
.TP
.B help
Run the equivalent of
.B "ts manpage | groff \-man \-Tascii | less"
to show this manual page in the system pager.
.TP
.B install
Copy the binary (and on macOS the embedded icon as
.BR ts-icon.svg )
to a directory on
.BR PATH .
If
.I install_dir
is omitted, uses the first writable directory on
.BR PATH .
If
.I install_dir
is given, installs there (directory created if needed).
Optional
.I repo_path
is the directory containing the binary (default: current executable's directory). On macOS the icon is embedded so
.B ts-icon.svg
is always written even without the source repository.
.TP
.B uninstall
Stop the reminder daemon, remove startup/shutdown/login/logout hooks (LaunchAgents and LogoutHook on macOS, systemd user units on Linux), prompt to remove timesheet log files (y/N), then remove
.B ts-icon.svg
and the
.B ts
binary from the directory containing the running executable.
.TP
.B interval
Set or show the time between reminder daemon prompts. With no argument, print the current interval. With one argument, set the interval and restart the daemon.
.I duration
accepts: a bare number (treated as minutes, e.g.
.BR 3 " or " 3m ),
seconds (e.g.
.BR 100s ),
or combined (e.g.
.BR 1h30m ).
.B restart
and
.B reminder
are aliases for
.BR interval .
Reminder daemon behavior: on timeout (no click), records STOP at reminder-appeared time and brings the existing reminder window to the front of the window stack (does not launch a new prompt). Dismissed without choice (close, Escape) re-shows immediately. The "Enter new activity" dialog has no timeout; blank/cancelled re-shows the reminder. On system shutdown (SIGTERM), records STOP and exits.
.TP
.B list
Plaintext report: percentage of time per activity (high to low), and hours per day of week (Sun\-Sat).
If work is in progress (last entry is START), uses a virtual STOP at current time for the report
and shows current task, start time, and duration.
Optional
.I file_or_extension
selects an alternate log path or extension filter.
.TP
.B migrate
Convert all
.B timesheet.*
files in the timesheet log directory to current format (timestamp first, ISO 8601).
.TP
.B tail
Output the latest ten log entries; timestamps are shown in local time.
Each entry includes a duration: for START, time until the next different event or current time;
for STOP, time until the next START or current time.
Consecutive START entries with the same activity are collapsed (last timestamp kept), then the last 10 entries are shown.
Optional
.I file_or_extension
selects an alternate log path or extension (same as
.BR list ).
.TP
.B manpage
Write this manual page in groff format to stdout. Example:
.B "ts manpage | groff \-man \-Tascii | less"
.TP
.B rebuild
Build from source and install into the directory of the currently running binary.
Optional
.I directory
(default: current directory): path to a directory containing
.BR Cargo.toml .
Runs
.B "cargo build \-\-release"
there, then
.B "target/release/ts install"
.I install_dir
where
.I install_dir
is the directory of the running
.B ts
binary.
If
.I directory
is omitted and the current directory has no
.BR Cargo.toml ,
clones
.B https://github.com/pillarsdotnet/timesheet
and builds from the clone.
.TP
.B rename
Same as
.BR alias .
.TP
.B reminder
Alias for
.BR interval .
.TP
.B restart
Alias for
.BR interval .
.TP
.B rotate
If the last entry is START (work in progress), appends a STOP at current time first.
Rename the timesheet log to
.B timesheet.YYMMDD
using the timestamp of the log's most recent entry (START or STOP).
Errors if the log is missing or has no valid entries.
.TP
.B start
Record work start
.IR now .
On macOS with no
.IR activity ,
shows the reminder dialog to pick or enter an activity.
Otherwise optional
.I activity
(default: misc/unspecified). Appends a START line; does not modify existing entries.
Starts or restarts the reminder daemon (resets the timer).
.TP
.B started
Record a work start at a
.IR "past time" .
.I start_time
accepts GNU
.B date \-d
style, or
.B YYYY\-MM\-DD\ HH:MM[:SS],
or
.B HH:MM
(today).
Inserts the new START entry at the correct chronological position.
No existing entries are discarded.
.TP
.B stop
Record work stop at
.IR now
or at optional
.I stop_time
(same formats as
.BR started ).
If the last entry is already STOP and no
.I stop_time
is given, nothing happens. If
.I stop_time
is given, the last STOP entry is amended to that time.
If the last entry is START, appends the new STOP (normal pairing).
When a stop is recorded (append or amend), stops the reminder daemon and shows a dialog that reminders have been stopped (skipped when
.B TS_LOGOUT
is set, e.g.\ during logout/shutdown).
.TP
.B stopped
Alias for
.BR stop .
.TP
.B timeoff
Show the stop-work time that would give an average of 8 hours per day worked.
Requires only a START entry (work in progress); no completed session on the current day is required.
If the log is empty or the last entry is STOP, appends a START first so the calculation can run.
.SH ENVIRONMENT
.TP
.B TS_DEBUG
If set (any value), log debug messages to stderr for
.B restart
and reminder daemon start/kill (e.g.
.BR "TS_DEBUG=1 ts restart" ).
.TP
.B TS_LOGOUT
If set (any value), suppresses the "reminders stopped" dialog when
.B ts\ stop
is invoked (used by autostart scripts during logout/shutdown).
.SH FILES
.B $HOME/Documents/timesheet.log
Default timesheet log (path is compile-time in
.BR DEFAULT_TIMESHEET ).
.TP
.B $XDG_CACHE_HOME/ts-reminder-interval
or
.B $HOME/.cache/ts-reminder-interval
Reminder interval in seconds (decimal). Used by the reminder daemon; set via
.BR "ts interval" .
.TP
.B "$HOME/Library/Application Support/ts/" (macOS)
Autostart scripts: session script (stop on TERM), logout hook script (stop on logout/shutdown). The logout hook is registered with
.BR "defaults write com.apple.loginwindow LogoutHook" ;
if
.B ts\ autostart
cannot set it, run the printed
.B sudo
command once.
.SH "SEE ALSO"
Full documentation and install instructions: see
.BR INSTALL.md
and
.BR README.md
in the source repository.
.SH AUTHORS
Robert August Vincent II <pillarsdotnet@gmail.com>
Co-author: Cursor-AI.
"#
}

/// Output a Unix manual page in groff format to stdout.
fn cmd_manpage() -> Result<(), String> {
    let man = manpage_content();
    let mut out = io::stdout();
    if let Err(e) = out.write_all(man.as_bytes()) {
        if e.kind() != io::ErrorKind::BrokenPipe {
            return Err(e.to_string());
        }
    }
    let _ = out.flush();
    Ok(())
}

/// Show the man page in a pager using groff (ts manpage | groff -man -Tascii | less).
/// If groff is not available, pages the raw groff source with less.
fn cmd_help() -> Result<(), String> {
    let man = manpage_content();

    let child = Command::new("sh")
        .args(["-c", "groff -man -Tascii 2>/dev/null | less -R"])
        .stdin(Stdio::piped())
        .spawn();

    if let Ok(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(man.as_bytes());
        }
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }
    }

    // Fallback: page the raw groff source with less
    let mut child = Command::new("less")
        .arg("-R")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "no pager available (groff, less): {}. Try: ts manpage | groff -man -Tascii | less",
                e
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(man.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let _ = child.wait();
    Ok(())
}

/// Register "ts start" on login and "ts stop" on logout/shutdown (macOS: launchd; Linux: systemd user). Use "ts autostart uninstall" to remove.
/// Optional first argument: interval (e.g. 5s, 3m) to set reminder interval and start the daemon in this session so the reminder appears soon.
fn cmd_autostart(args: &[String]) -> Result<(), String> {
    let uninstall = args.first().map(String::as_str) == Some("uninstall");
    if !uninstall {
        let interval_set = if let Some(interval_arg) = args.first() {
            if let Ok(secs) = parse_interval_duration(interval_arg) {
                let path = reminder_interval_path();
                if let Err(e) = fs::write(&path, secs.to_string()) {
                    eprintln!("ts autostart: could not set interval: {}", e);
                    false
                } else {
                    kill_reminder_daemon_if_running();
                    thread::sleep(Duration::from_millis(100));
                    start_reminder_daemon_if_needed(&timesheet_path());
                    true
                }
            } else {
                false
            }
        } else {
            false
        };
        if !interval_set {
            start_reminder_daemon_if_needed(&timesheet_path());
            let secs = get_reminder_interval_secs();
            if secs >= 3600 && secs % 3600 == 0 {
                println!("Reminder interval: {}h", secs / 3600);
            } else if secs >= 60 && secs % 60 == 0 {
                println!("Reminder interval: {}m", secs / 60);
            } else {
                println!("Reminder interval: {}s", secs);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if uninstall {
            do_autostart_uninstall_macos()
        } else {
            do_autostart_install_macos()
        }
    }
    #[cfg(target_os = "linux")]
    {
        if uninstall {
            do_autostart_uninstall_linux()
        } else {
            do_autostart_install_linux()
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = uninstall;
        Err("ts autostart: not supported on this platform (macOS and Linux only).".to_string())
    }
}

#[cfg(target_os = "macos")]
fn do_autostart_install_macos() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe_path = exe.to_string_lossy();
    let home = env::var_os("HOME").ok_or("ts autostart: HOME not set")?;
    let agents = PathBuf::from(&home).join("Library/LaunchAgents");
    let support = PathBuf::from(&home).join("Library/Application Support/ts");
    fs::create_dir_all(&support).map_err(|e| format!("ts autostart: cannot create {}: {}", support.display(), e))?;

    // Remove old shell-script session wrapper if present (superseded by --session-daemon).
    let _ = fs::remove_file(support.join("autostart-session.sh"));

    // LogoutHook runs as root on logout/shutdown and macOS waits for it to complete before
    // proceeding, making it the most reliable mechanism for recording STOP. It uses
    // `launchctl asuser` to run ts stop in the console user's launchd context (faster than
    // `su -` because it does not spawn a full login shell).
    let logout_hook_path = support.join("logout-hook.sh");
    let exe_escaped = exe_path.replace('\\', "\\\\").replace('"', "\\\"");
    let logout_script = format!(
        r#"#!/bin/sh
uid=$(stat -f '%u' /dev/console 2>/dev/null)
[ -z "$uid" ] && exit 0
export TS_LOGOUT=1
exec launchctl asuser "$uid" "{}" stop
"#,
        exe_escaped
    );
    fs::write(&logout_hook_path, logout_script).map_err(|e| format!("ts autostart: cannot write logout hook: {}", e))?;
    #[allow(clippy::disallowed_methods)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&logout_hook_path).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&logout_hook_path, perms).map_err(|e| e.to_string())?;
    }
    // Skip sudo prompt if we already registered (marker file), or if defaults read shows our path.
    // Reading com.apple.loginwindow often requires root; try without sudo first, then with sudo when marker is missing.
    let marker_path = support.join("logout-hook-registered");
    let ours = logout_hook_path.to_string_lossy().trim().to_string();
    let canonical_ours = fs::canonicalize(&logout_hook_path)
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_default();
    let path_matches = |current: &str| {
        current == ours.as_str() || (!canonical_ours.is_empty() && current == canonical_ours)
    };
    let mut hook_already_set = marker_path.exists();
    if !hook_already_set {
        let read_out = Command::new("defaults")
            .args(["read", "com.apple.loginwindow", "LogoutHook"])
            .output()
            .ok();
        if let Some(o) = read_out {
            if o.status.success() {
                let current = String::from_utf8_lossy(&o.stdout).trim().to_string();
                hook_already_set = path_matches(&current);
            }
        }
        if !hook_already_set {
            let sudo_out = Command::new("sudo")
                .args(["defaults", "read", "com.apple.loginwindow", "LogoutHook"])
                .output()
                .ok();
            if let Some(o) = sudo_out {
                if o.status.success() {
                    let current = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    hook_already_set = path_matches(&current);
                }
            }
        }
    }
    if hook_already_set && !marker_path.exists() {
        let _ = fs::write(&marker_path, "");
    }

    if !hook_already_set {
        let logout_cmd = format!(
            "sudo defaults write com.apple.loginwindow LogoutHook \"{}\"",
            logout_hook_path.display()
        );
        println!("  To record STOP on logout/shutdown, register the logout hook.");
        println!("  This command requires local administrator access (you may be prompted for your password):");
        println!("  {}", logout_cmd);
        print!("  Run this command now? [y/N] ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).is_ok() {
            let answer = line.trim().to_lowercase();
            if answer == "y" || answer == "yes" {
                if !Command::new("sudo")
                    .args(["defaults", "write", "com.apple.loginwindow", "LogoutHook", logout_hook_path.to_string_lossy().as_ref()])
                    .status()
                    .map_err(|e| e.to_string())?
                    .success()
                {
                    return Err("ts autostart: logout hook command failed (sudo may have been cancelled).".to_string());
                }
                if fs::write(&marker_path, "").is_err() {
                    eprintln!("  Warning: could not save registration state; you may be prompted again next time.");
                }
                println!("  Logout hook registered.");
            }
        }
    }

    let start_plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.ts.autostart.start</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>AbandonProcessGroup</key>
    <true/>
</dict>
</plist>
"#,
        exe_path.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    );
    // The session plist runs `ts --session-daemon` directly (no shell-script wrapper).
    // ExitTimeOut tells launchd to wait up to 30 s after SIGTERM before sending SIGKILL,
    // giving the daemon time to write the STOP entry and exit cleanly.
    let session_plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.ts.autostart.session</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>--session-daemon</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>ExitTimeOut</key>
    <integer>30</integer>
</dict>
</plist>
"#,
        exe_path.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    );

    fs::create_dir_all(&agents).map_err(|e| format!("ts autostart: cannot create {}: {}", agents.display(), e))?;
    let start_plist_path = agents.join("com.ts.autostart.start.plist");
    let session_plist_path = agents.join("com.ts.autostart.session.plist");
    fs::write(&start_plist_path, &start_plist).map_err(|e| format!("ts autostart: cannot write plist: {}", e))?;
    fs::write(&session_plist_path, &session_plist).map_err(|e| format!("ts autostart: cannot write plist: {}", e))?;

    let _ = Command::new("launchctl").arg("unload").arg(&start_plist_path).output();
    let _ = Command::new("launchctl").arg("unload").arg(&session_plist_path).output();
    if !Command::new("launchctl").arg("load").arg(&start_plist_path).status().map_err(|e| e.to_string())?.success() {
        return Err("ts autostart: launchctl load start plist failed".to_string());
    }
    if !Command::new("launchctl").arg("load").arg(&session_plist_path).status().map_err(|e| e.to_string())?.success() {
        return Err("ts autostart: launchctl load session plist failed".to_string());
    }
    println!("Autostart installed: \"ts start\" runs at login, \"ts stop\" runs at logout/shutdown.");
    println!("  Start plist:   {}", start_plist_path.display());
    println!("  Session plist: {}", session_plist_path.display());
    println!("  Logout hook:   {}", logout_hook_path.display());
    println!("  To remove: ts autostart uninstall");
    Ok(())
}

#[cfg(target_os = "macos")]
fn do_autostart_uninstall_macos() -> Result<(), String> {
    let home = env::var_os("HOME").ok_or("ts autostart: HOME not set")?;
    let agents = PathBuf::from(&home).join("Library/LaunchAgents");
    let start_plist_path = agents.join("com.ts.autostart.start.plist");
    let session_plist_path = agents.join("com.ts.autostart.session.plist");
    let support = PathBuf::from(&home).join("Library/Application Support/ts");
    let logout_hook_path = support.join("logout-hook.sh");

    let _ = Command::new("launchctl").arg("unload").arg(&start_plist_path).output();
    let _ = Command::new("launchctl").arg("unload").arg(&session_plist_path).output();
    let _ = Command::new("sudo")
        .args(["defaults", "delete", "com.apple.loginwindow", "LogoutHook"])
        .output();
    let _ = fs::remove_file(&start_plist_path);
    let _ = fs::remove_file(&session_plist_path);
    let _ = fs::remove_file(support.join("autostart-session.sh")); // legacy shell-script wrapper
    let _ = fs::remove_file(&logout_hook_path);
    let _ = fs::remove_file(support.join("logout-hook-registered"));
    println!("Autostart uninstalled.");
    Ok(())
}

#[cfg(target_os = "linux")]
fn do_autostart_install_linux() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe_path = exe.to_string_lossy();
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").ok_or("ts autostart: HOME not set")?).join(".config"));
    let user_units = config.join("systemd/user");
    fs::create_dir_all(&user_units).map_err(|e| format!("ts autostart: cannot create {}: {}", user_units.display(), e))?;

    let start_unit = format!(
        r#"[Unit]
Description=ts start on login
[Service]
Type=oneshot
ExecStart={} start
[Install]
WantedBy=default.target
"#,
        exe_path
    );
    let session_unit = format!(
        r#"[Unit]
Description=ts stop on logout
[Service]
Type=simple
Environment=TS_LOGOUT=1
ExecStart=/bin/sleep infinity
ExecStop={} stop
[Install]
WantedBy=default.target
"#,
        exe_path
    );

    let start_path = user_units.join("ts-autostart-start.service");
    let session_path = user_units.join("ts-autostart-session.service");
    fs::write(&start_path, &start_unit).map_err(|e| format!("ts autostart: cannot write unit: {}", e))?;
    fs::write(&session_path, &session_unit).map_err(|e| format!("ts autostart: cannot write unit: {}", e))?;

    if !Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("ts autostart: systemctl daemon-reload failed".to_string());
    }
    if !Command::new("systemctl")
        .args(["--user", "enable", "--now", "ts-autostart-start.service"])
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("ts autostart: systemctl enable start service failed".to_string());
    }
    if !Command::new("systemctl")
        .args(["--user", "enable", "--now", "ts-autostart-session.service"])
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("ts autostart: systemctl enable session service failed".to_string());
    }
    println!("Autostart installed: \"ts start\" runs at login, \"ts stop\" runs at logout/shutdown.");
    println!("  Units: {}  {}", start_path.display(), session_path.display());
    println!("  To remove: ts autostart uninstall");
    Ok(())
}

#[cfg(target_os = "linux")]
fn do_autostart_uninstall_linux() -> Result<(), String> {
    let home = env::var_os("HOME").ok_or("ts autostart: HOME not set")?;
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&home));
    let user_units = config.join("systemd/user");
    let start_path = user_units.join("ts-autostart-start.service");
    let session_path = user_units.join("ts-autostart-session.service");

    let _ = Command::new("systemctl").args(["--user", "disable", "--now", "ts-autostart-start.service"]).output();
    let _ = Command::new("systemctl").args(["--user", "disable", "--now", "ts-autostart-session.service"]).output();
    let _ = fs::remove_file(&start_path);
    let _ = fs::remove_file(&session_path);
    println!("Autostart uninstalled.");
    Ok(())
}

const REMINDER_SLEEP_SECS: u64 = 300; // 5 minutes (default when no interval file)
const REMINDER_PROMPT_TIMEOUT_SECS: u64 = 300; // 5 minutes

/// Reminder interval in seconds: from config file if present and valid, else default.
fn get_reminder_interval_secs() -> u64 {
    let path = reminder_interval_path();
    match fs::read_to_string(&path) {
        Ok(s) => s.trim().parse::<u64>().unwrap_or(REMINDER_SLEEP_SECS),
        Err(_) => REMINDER_SLEEP_SECS,
    }
}

/// Returns true if a process with the given PID is running (Unix: kill -0).
fn ts_debug(msg: &str) {
    if env::var_os("TS_DEBUG").is_some() {
        let _ = writeln!(io::stderr(), "ts: {}", msg);
    }
}

/// Returns true if a process with the given PID exists (Unix: kill(pid, 0)). Does not spawn any subprocess.
fn is_pid_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Send a signal to a process by PID. Does not spawn the kill binary. No-op on non-Unix.
fn signal_pid(pid: u32, sig: i32) {
    #[cfg(unix)]
    {
        let _ = unsafe { kill(pid as i32, sig) };
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, sig);
    }
}

/// Returns true if the reminder daemon is running (PID file exists, PID is alive, and not self). No-op on non-Unix.
fn is_reminder_daemon_running() -> bool {
    #[cfg(not(unix))]
    return false;

    #[cfg(unix)]
    {
        let pid_path = reminder_pid_path();
        if let Ok(data) = fs::read_to_string(&pid_path) {
            if let Ok(pid) = data.trim().parse::<u32>() {
                if pid != process::id() && is_pid_running(pid) {
                    return true;
                }
            }
        }
        false
    }
}

/// Show a dialog/notification that timesheet reminders have been stopped. Spawns and does not block.
/// No-op if TS_LOGOUT is set (logout/shutdown); skips on non-macOS/Linux.
fn show_reminders_stopped_notification() {
    if env::var_os("TS_LOGOUT").is_some() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let script = "display dialog \"Timesheet reminders have been stopped.\" with title \"Timesheet\" buttons {\"OK\"} default button 1";
        let _ = macos_run_in_user_session("/usr/bin/osascript", &["-e", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("notify-send")
            .args(["--app-name=Timesheet", "Timesheet reminders have been stopped."])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = ();
    }
}

/// Kill the reminder daemon if running (read PID from file, remove PID file, then send SIGTERM).
/// Removing the PID file *before* signaling tells the daemon's SIGTERM handler that this is an
/// intentional ts kill rather than a system shutdown, so it skips writing a STOP entry.
/// No-op on non-Unix. Never kills the current process.
fn kill_reminder_daemon_if_running() {
    #[cfg(not(unix))]
    return;

    #[cfg(unix)]
    {
        ts_debug("kill_reminder: entry");
        let pid_path = reminder_pid_path();
        if let Ok(data) = fs::read_to_string(&pid_path) {
            ts_debug(&format!("kill_reminder: read pid file {:?}", data.trim()));
            if let Ok(pid) = data.trim().parse::<u32>() {
                if pid == process::id() {
                    ts_debug("kill_reminder: pid is self, removing file and skipping kill");
                    let _ = fs::remove_file(&pid_path);
                    return;
                }
                if is_pid_running(pid) {
                    // Remove PID file before signaling: the daemon's SIGTERM handler checks for
                    // the PID file to distinguish intentional kills from system shutdown.
                    let _ = fs::remove_file(&pid_path);
                    ts_debug(&format!("kill_reminder: sending SIGTERM to {}", pid));
                    signal_pid(pid, SIGTERM);
                    thread::sleep(Duration::from_millis(150));
                    if is_pid_running(pid) {
                        ts_debug(&format!("kill_reminder: sending SIGKILL to {}", pid));
                        signal_pid(pid, SIGKILL);
                    }
                    ts_debug("kill_reminder: done");
                    return;
                } else {
                    ts_debug("kill_reminder: process not running");
                }
            }
        } else {
            ts_debug("kill_reminder: no pid file or unreadable");
        }
        let _ = fs::remove_file(&pid_path);
        ts_debug("kill_reminder: done");
    }
}

/// Start the reminder daemon in the background if not already running. No-op on non-Unix or if daemon already running.
fn start_reminder_daemon_if_needed(_timesheet: &Path) {
    #[cfg(not(unix))]
    return;

    #[cfg(unix)]
    {
        ts_debug("start_reminder: entry");
        let pid_path = reminder_pid_path();
        if let Ok(data) = fs::read_to_string(&pid_path) {
            if let Ok(pid) = data.trim().parse::<u32>() {
                if is_pid_running(pid) {
                    ts_debug("start_reminder: daemon already running, skipping spawn");
                    return;
                }
            }
        }
        let exe = match env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                ts_debug(&format!("start_reminder: current_exe failed: {}", e));
                return;
            }
        };
        let use_debug = env::var_os("TS_DEBUG").is_some();
        if use_debug {
            ts_debug("start_reminder: TS_DEBUG set, spawning daemon with inherited stdio");
        } else {
            ts_debug(&format!("start_reminder: spawning {}", exe.display()));
        }
        let (stdout, stderr) = if use_debug {
            (Stdio::inherit(), Stdio::inherit())
        } else {
            (Stdio::null(), Stdio::null())
        };
        // Use pre_exec to call setsid() in the child after fork but before exec.
        // This places the reminder daemon in its own session before ts start exits,
        // preventing launchd from killing it when the LaunchAgent's process group is cleaned up.
        let result = unsafe {
            Command::new(&exe)
                .arg("--reminder-daemon")
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(stderr)
                .pre_exec(|| {
                    setsid();
                    Ok(())
                })
                .spawn()
        };
        match result {
            Ok(child) => {
                ts_debug(&format!("start_reminder: spawned daemon pid {}", child.id()));
                drop(child);
            }
            Err(e) => {
                ts_debug(&format!("start_reminder: spawn failed: {}", e));
            }
        }
        ts_debug("start_reminder: done");
    }
}

/// Set or show the reminder interval. With no arg: print current interval. With one arg: parse duration, save, restart daemon.
/// Duration examples: 3, 3m (minutes), 100s (seconds), 1h30m.
fn cmd_interval(args: &[String], timesheet: &Path) -> Result<(), String> {
    if args.is_empty() {
        let secs = get_reminder_interval_secs();
        if secs >= 3600 && secs.is_multiple_of(3600) {
            println!("{}h", secs / 3600);
        } else if secs >= 60 && secs.is_multiple_of(60) {
            println!("{}m", secs / 60);
        } else {
            println!("{}s", secs);
        }
        kill_reminder_daemon_if_running();
        thread::sleep(Duration::from_millis(100));
        start_reminder_daemon_if_needed(timesheet);
        return Ok(());
    }
    let duration_str = args[0].as_str();
    let secs = parse_interval_duration(duration_str)?;
    let path = reminder_interval_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, secs.to_string()).map_err(|e| format!("ts interval: cannot write config: {}", e))?;
    kill_reminder_daemon_if_running();
    thread::sleep(Duration::from_millis(100));
    start_reminder_daemon_if_needed(timesheet);
    if secs % 3600 == 0 && secs >= 3600 {
        println!("Reminder interval set to {}h. Daemon restarted.", secs / 3600);
    } else if secs % 60 == 0 && secs >= 60 {
        println!("Reminder interval set to {}m. Daemon restarted.", secs / 60);
    } else {
        println!("Reminder interval set to {}s. Daemon restarted.", secs);
    }
    Ok(())
}

/// Run the reminder daemon loop: sleep for configured interval, show "What are you working on?" prompt, handle response or timeout.
/// Long-running session daemon that records a STOP entry when launchd sends SIGTERM
/// (i.e. at logout or system shutdown). Installed as the `com.ts.autostart.session`
/// LaunchAgent by `ts autostart`. Because this is a launchd job, launchd delivers a
/// clean SIGTERM and waits for the process to exit (see ExitTimeOut in the plist) before
/// proceeding with the shutdown sequence, making STOP recording reliable.
fn run_session_daemon(timesheet: &Path) {
    #[cfg(unix)]
    {
        // Block SIGTERM in the main thread; a dedicated sigwait thread handles it
        // synchronously, so the STOP entry is written before process::exit is called.
        let mut set = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        unsafe {
            sigemptyset(&mut set);
            sigaddset(&mut set, SIGTERM);
            pthread_sigmask(SIG_BLOCK, &set, std::ptr::null_mut());
        }
        let set_for_sigwait = set;
        let ts_path = timesheet.to_path_buf();
        thread::spawn(move || {
            let mut sig: libc::c_int = 0;
            if unsafe { sigwait(&set_for_sigwait, &mut sig) } == 0 && sig == SIGTERM {
                // Write STOP only if a session is currently open (last line is START).
                let content = fs::read_to_string(&ts_path).unwrap_or_default();
                let last = content.lines().rev().find(|l| !l.trim().is_empty());
                if last.and_then(parse_line).map(|ll| matches!(ll, LogLine::Start(_, _))).unwrap_or(false) {
                    let _ = append_stop_entry(&ts_path, Local::now());
                }
                process::exit(0);
            }
        });
    }
    // Main thread: sleep indefinitely. The sigwait thread calls process::exit on SIGTERM.
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn run_reminder_daemon(timesheet: &Path) {
    #[cfg(unix)]
    {
        let _ = unsafe { signal(SIGHUP, SIG_IGN) };
        // Detach from any process group inherited from parent (belt-and-suspenders; setsid() in
        // start_reminder_daemon_if_needed's pre_exec is the primary guard against launchd cleanup).
        let _ = unsafe { setpgid(0, 0) };
        // Block SIGTERM in main thread and spawn a handler that appends STOP on shutdown.
        let mut set = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        unsafe {
            sigemptyset(&mut set);
            sigaddset(&mut set, SIGTERM);
            pthread_sigmask(SIG_BLOCK, &set, std::ptr::null_mut());
        }
        let set_for_sigwait = set;
        let timesheet_for_signal = timesheet.to_path_buf();
        thread::spawn(move || {
            let mut sig: libc::c_int = 0;
            if unsafe { sigwait(&set_for_sigwait, &mut sig) } == 0 && sig == SIGTERM {
                // Only write STOP on real system shutdown. kill_reminder_daemon_if_running()
                // removes the PID file before sending SIGTERM, so if the file is gone (or no
                // longer points to us) this is an intentional ts kill and we skip the STOP.
                let my_pid = process::id();
                let is_shutdown = fs::read_to_string(reminder_pid_path())
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .map(|p| p == my_pid)
                    .unwrap_or(false);
                if is_shutdown {
                    let _ = append_stop_entry(&timesheet_for_signal, Local::now());
                }
                process::exit(0);
            }
        });
    }
    let pid_path = reminder_pid_path();
    if let Some(parent) = pid_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(&pid_path, process::id().to_string()).is_err() {
        return;
    }
    let pid_path_guard = pid_path.clone();
    let _cleanup = defer(move || {
        let _ = fs::remove_file(&pid_path_guard);
    });

    loop {
        let interval_secs = get_reminder_interval_secs();
        ts_debug(&format!("reminder daemon: sleeping {}s", interval_secs));
        thread::sleep(Duration::from_secs(interval_secs));
        ts_debug("reminder daemon: showing prompt");

        let activities = activities_this_week_most_recent_first(timesheet);
        match show_reminder_prompt(&activities, Some(timesheet)) {
            ReminderResult::DontBugMe => {
                show_reminders_stopped_notification();
                break;
            }
            ReminderResult::Activity(activity) => {
                let _ = append_start_entry(timesheet, &activity);
            }
            ReminderResult::EnterNew => unreachable!("show_reminder_prompt converts EnterNew to Activity"),
            ReminderResult::ShowAgainImmediate => {} // dismissed without choice; re-show immediately
                    ReminderResult::TimeoutAddStop(dt) => {
                        let _ = append_stop_entry(timesheet, dt);
                // Do not dismiss reminder window; continue loop to re-show
            }
        }
    }
}

/// Defer a closure to run when the guard is dropped (e.g. for PID file cleanup).
struct Defer<F: FnOnce()>(Option<F>);
fn defer<F: FnOnce()>(f: F) -> Defer<F> {
    Defer(Some(f))
}
impl<F: FnOnce()> Drop for Defer<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

#[derive(Debug)]
enum ReminderResult {
    DontBugMe,
    Activity(String),
    /// User chose "Enter new activity..."; caller should show text dialog.
    EnterNew,
    /// Dialog dismissed without choice (e.g. process killed, cancelled, blank); re-show immediately.
    ShowAgainImmediate,
    /// Reminder timed out without click; add STOP at given datetime and re-show immediately.
    TimeoutAddStop(DateTime<Local>),
}

/// Show "What are you working on?" prompt; returns user choice or timeout. Platform-specific (macOS: osascript).
/// timesheet: used when appending STOP on timeout (reminder daemon / ts start).
fn show_reminder_prompt(activities: &[String], timesheet: Option<&Path>) -> ReminderResult {
    #[cfg(target_os = "macos")]
    return show_reminder_prompt_macos(activities, timesheet);

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (activities, timesheet);
        ReminderResult::TimeoutAddStop(Local::now())
    }
}

/// Run osascript "Enter activity:" text dialog in user session; returns the entered string or None.
#[cfg(target_os = "macos")]
fn prompt_enter_activity_macos(ts_debug: bool) -> Option<String> {
    // Return only the text so stdout is just the activity (no parsing "button returned:OK, text returned:...").
    let prompt_script = "text returned of (display dialog \"Enter activity:\" with title \"ts\" default answer \"\")";
    let run = |use_launchctl: bool| -> Option<String> {
        let mut cmd: Command = if use_launchctl {
            macos_run_in_user_session("/usr/bin/osascript", &["-e", prompt_script])
        } else {
            let mut c = Command::new("/usr/bin/osascript");
            c.args(["-e", prompt_script]);
            c
        };
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(if ts_debug { Stdio::inherit() } else { Stdio::null() })
            .spawn()
            .ok()?;
        let out = match wait_no_timeout(child) {
            Some(o) => o,
            None => return None,
        };
        let activity = String::from_utf8_lossy(&out).trim().to_string();
        if activity.is_empty() {
            None
        } else {
            Some(activity)
        }
    };
    run(true).or_else(|| run(false))
}

/// On macOS, run a command in the user's GUI session so dialogs can appear (avoids "no user interaction allowed" from nohup daemon).
#[cfg(target_os = "macos")]
fn macos_run_in_user_session(exe: &str, exe_args: &[&str]) -> Command {
    let uid = unsafe { getuid() }.to_string();
    let mut args = vec!["asuser", &uid, exe];
    let mut all = std::vec::Vec::from(exe_args);
    args.append(&mut all);
    let mut c = Command::new("/usr/bin/launchctl");
    c.args(args);
    c
}

/// On macOS, bring the reminder window (process with given PID) to the front of the window stack. Runs in user's GUI session.
#[cfg(target_os = "macos")]
fn macos_bring_reminder_window_to_front(pid: u32) {
    let script = format!(
        "tell application \"System Events\" to set frontmost of (first process whose unix id is {}) to true",
        pid
    );
    let _ = macos_run_in_user_session("/usr/bin/osascript", &["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(target_os = "macos")]
fn show_reminder_prompt_macos(activities: &[String], timesheet: Option<&Path>) -> ReminderResult {
    let reminder_appeared = Local::now();
    let mut choices = vec!["Stop Work".to_string()];
    for a in activities.iter().rev() {
        if !a.is_empty() && !choices.contains(a) {
            choices.push(a.clone());
        }
    }
    choices.push("Enter new activity...".to_string());

    // Native Rust/AppKit dialog (many buttons, one click). Spawn ts --reminder-dialog in user's GUI session.
    let ts_debug = env::var_os("TS_DEBUG").is_some();
    enum NativeOutcome {
        Result(ReminderResult),
        Dismissed,   // Child ran but returned empty; re-show immediately
        Unavailable, // Spawn failed; fall through to SystemUIServer
    }
    let try_native = |use_launchctl: bool| -> NativeOutcome {
        let exe = match env::current_exe().ok() {
            Some(e) => e,
            None => return NativeOutcome::Unavailable,
        };
        let exe_str = exe.to_string_lossy();
        let mut args = vec!["--reminder-dialog".to_string()];
        args.extend(choices.iter().cloned());
        let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut cmd = if use_launchctl {
            macos_run_in_user_session(&exe_str, &args_ref)
        } else {
            let mut c = Command::new(&exe);
            c.args(&args_ref);
            c
        };
        let mut child = match cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(if ts_debug { Stdio::inherit() } else { Stdio::null() })
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return NativeOutcome::Unavailable,
        };
        let appeared = Local::now();
        let timeout = Duration::from_secs(REMINDER_PROMPT_TIMEOUT_SECS);
        let mut appended_stop_for_this_reminder = false;
        loop {
            match wait_with_timeout(child, timeout, false) {
                WaitOutcome::Finished(Some(out)) => {
                    let s = String::from_utf8_lossy(&out).trim().to_string();
                    if s == "Stop Work" {
                        return NativeOutcome::Result(ReminderResult::DontBugMe);
                    }
                    if s == "Enter new activity..." {
                        return NativeOutcome::Result(ReminderResult::EnterNew);
                    }
                    if !s.is_empty() && choices.contains(&s) {
                        return NativeOutcome::Result(ReminderResult::Activity(s));
                    }
                    return NativeOutcome::Dismissed;
                }
                WaitOutcome::Finished(None) => return NativeOutcome::Unavailable,
                WaitOutcome::TimedOut => return NativeOutcome::Unavailable,
                WaitOutcome::TimedOutWithChild(c) => {
                    if !appended_stop_for_this_reminder {
                        if let Some(ts) = timesheet {
                            let _ = append_stop_entry(ts, appeared);
                        }
                        appended_stop_for_this_reminder = true;
                    }
                    macos_bring_reminder_window_to_front(c.id());
                    child = c;
                }
            }
        }
    };

    let handle_native = |res: ReminderResult| {
        if let ReminderResult::EnterNew = res {
            if let Some(activity) = prompt_enter_activity_macos(ts_debug) {
                return ReminderResult::Activity(activity);
            }
            return ReminderResult::ShowAgainImmediate;
        } else {
            return res;
        }
    };
    match try_native(true) {
        NativeOutcome::Result(res) => return handle_native(res),
        NativeOutcome::Dismissed => return ReminderResult::ShowAgainImmediate,
        NativeOutcome::Unavailable => {}
    }
    match try_native(false) {
        NativeOutcome::Result(res) => return handle_native(res),
        NativeOutcome::Dismissed => return ReminderResult::ShowAgainImmediate,
        NativeOutcome::Unavailable => {}
    }
    if ts_debug {
        let _ = std::io::stderr().write_fmt(format_args!(
            "ts: native reminder dialog failed or timed out, using SystemUIServer fallback\n"
        ));
    }

    // SystemUIServer can show dialogs from background processes (daemon). Try it first (with list of activities).
    match show_reminder_prompt_macos_systemui(&choices, reminder_appeared) {
        ReminderResult::DontBugMe => return ReminderResult::DontBugMe,
        ReminderResult::Activity(ref a) if !a.is_empty() => return ReminderResult::Activity(a.clone()),
        ReminderResult::TimeoutAddStop(epoch) => {
            if let Some(ts) = timesheet {
                let _ = append_stop_entry(ts, epoch);
            }
            return ReminderResult::ShowAgainImmediate;
        }
        _ => {}
    }
    // Fall through: SystemUIServer dialog failed or timed out, try osascript

    let ts_debug_stderr = env::var_os("TS_DEBUG").is_some();
    let stderr_mode = if ts_debug_stderr {
        Stdio::inherit()
    } else {
        Stdio::null()
    };

    // Fallback: osascript choose from list (requires click then OK), run in user session so dialog appears
    let list_script = choices
        .iter()
        .map(|s| escape_applescript_string(s))
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        "choose from list {{{}}} with title \"ts\" with prompt \"What are you working on?\" default items {{item 1 of {{{}}}}}",
        list_script,
        list_script
    );
    let child = match macos_run_in_user_session("/usr/bin/osascript", &["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr_mode)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return ReminderResult::TimeoutAddStop(reminder_appeared),
    };

    let timeout = Duration::from_secs(REMINDER_PROMPT_TIMEOUT_SECS);
    let result = match wait_with_timeout(child, timeout, true) {
        WaitOutcome::Finished(Some(stdout)) => {
            let s = String::from_utf8_lossy(&stdout).trim().to_string();
            if s == "false" {
                return ReminderResult::TimeoutAddStop(reminder_appeared);
            }
            if s == *"Stop Work" {
                return ReminderResult::DontBugMe;
            }
            if s == "Enter new activity..." {
                if let Some(activity) = prompt_enter_activity_macos(ts_debug_stderr) {
                    return ReminderResult::Activity(activity);
                }
                return ReminderResult::ShowAgainImmediate;
            }
            ReminderResult::Activity(s)
        }
        WaitOutcome::Finished(None) => ReminderResult::TimeoutAddStop(reminder_appeared),
        WaitOutcome::TimedOut => ReminderResult::TimeoutAddStop(reminder_appeared),
        WaitOutcome::TimedOutWithChild(_) => ReminderResult::TimeoutAddStop(reminder_appeared), // kill_on_timeout=true here
    };
    result
}

/// Buttons-only dialog via SystemUIServer (one click = done; works from daemon).
/// AppleScript display dialog allows at most 3 buttons, so we show: Stop Work | first activity (least-recent) | Enter new activity...
#[cfg(target_os = "macos")]
fn show_reminder_prompt_macos_systemui(choices: &[String], reminder_appeared: DateTime<Local>) -> ReminderResult {
    let stderr_mode = if env::var_os("TS_DEBUG").is_some() {
        Stdio::inherit()
    } else {
        Stdio::null()
    };
    let timeout_dur = Duration::from_secs(REMINDER_PROMPT_TIMEOUT_SECS);

    // AppleScript display dialog allows max 3 buttons. Build exactly 3: Stop Work, (optional) first activity, Enter new activity...
    let three_buttons: Vec<&str> = {
        let mut b = Vec::with_capacity(3);
        b.push("Stop Work");
        if choices.len() > 2 {
            b.push(choices[1].as_str());
        }
        b.push("Enter new activity...");
        b
    };
    let buttons_script = three_buttons
        .iter()
        .map(|s| format!("\"{}\"", escape_applescript_string(s)))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        "tell application \"SystemUIServer\" to display dialog \"What are you working on?\" with title \"ts\" buttons {{{}}} default button \"Stop Work\"",
        buttons_script
    );
    if let Ok(child) = macos_run_in_user_session("/usr/bin/osascript", &["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr_mode)
        .spawn()
    {
        match wait_with_timeout(child, timeout_dur, true) {
            WaitOutcome::Finished(Some(stdout)) => {
                let s = String::from_utf8_lossy(&stdout).trim().to_string();
                for part in s.split(',') {
                    let part = part.trim();
                    if let Some(rest) = part.strip_prefix("button returned:") {
                        let btn = rest.trim().trim_matches('"');
                        if btn == "Stop Work" {
                            return ReminderResult::DontBugMe;
                        }
                        if btn == "Enter new activity..." {
                            break;
                        }
                        return ReminderResult::Activity(btn.to_string());
                    }
                }
            }
            _ => return ReminderResult::TimeoutAddStop(reminder_appeared),
        }
    }

    // When user chose "Enter new activity...": try choose from list (all activities) for one more click, then text dialog.
    let stderr2 = if env::var_os("TS_DEBUG").is_some() {
        Stdio::inherit()
    } else {
        Stdio::null()
    };
    if choices.len() > 2 {
        let list_script = choices
            .iter()
            .map(|s| format!("\"{}\"", escape_applescript_string(s)))
            .collect::<Vec<_>>()
            .join(", ");
        let list_cmd = format!(
            "tell application \"SystemUIServer\" to choose from list {{{}}} with title \"ts\" with prompt \"What are you working on?\" default items {{item 1 of {{{}}}}}",
            list_script,
            list_script
        );
        if let Ok(child) = macos_run_in_user_session("/usr/bin/osascript", &["-e", &list_cmd])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(stderr2)
            .spawn()
        {
            match wait_with_timeout(child, timeout_dur, true) {
                WaitOutcome::Finished(Some(stdout)) => {
                    let s = String::from_utf8_lossy(&stdout).trim().to_string();
                    if s == "false" {
                        return ReminderResult::TimeoutAddStop(reminder_appeared);
                    }
                    if s == "Stop Work" {
                        return ReminderResult::DontBugMe;
                    }
                    if s != "Enter new activity..." {
                        return ReminderResult::Activity(s);
                    }
                }
                _ => return ReminderResult::TimeoutAddStop(reminder_appeared),
            }
        }
    }
    // Text dialog for new activity or when list was cancelled.
    let script = "tell application \"SystemUIServer\" to display dialog \"What are you working on?\" default answer \"\" with title \"ts\" buttons {\"Stop Work\", \"OK\"} default button \"OK\"";
    let stderr2 = if env::var_os("TS_DEBUG").is_some() {
        Stdio::inherit()
    } else {
        Stdio::null()
    };
    let child = match macos_run_in_user_session("/usr/bin/osascript", &["-e", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr2)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return ReminderResult::TimeoutAddStop(reminder_appeared),
    };
    match wait_with_timeout(child, timeout_dur, true) {
        WaitOutcome::Finished(Some(stdout)) => {
            let s = String::from_utf8_lossy(&stdout).trim().to_string();
            let mut activity_from_text: Option<String> = None;
            for part in s.split(',') {
                let part = part.trim();
                if let Some(rest) = part.strip_prefix("button returned:") {
                    let btn = rest.trim().trim_matches('"');
                    if btn == "Stop Work" {
                        return ReminderResult::DontBugMe;
                    }
                }
                if let Some(rest) = part.strip_prefix("text returned:") {
                    let activity = rest.trim().trim_matches('"').trim();
                    if !activity.is_empty() {
                        activity_from_text = Some(activity.to_string());
                    }
                }
            }
            if let Some(activity) = activity_from_text {
                return ReminderResult::Activity(activity);
            }
        }
        _ => {}
    }
    ReminderResult::TimeoutAddStop(reminder_appeared)
}

fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Wait for process to finish, or until timeout. Returns stdout if process exited normally.
/// When kill_on_timeout is false and we time out, returns TimedOutWithChild so the caller can bring the window to front and wait again.
enum WaitOutcome {
    Finished(Option<Vec<u8>>),
    TimedOut,
    /// Child still running (not killed); caller can bring window to front and call wait_with_timeout again.
    TimedOutWithChild(process::Child),
}

/// Wait for process to finish, or until timeout. If kill_on_timeout is false, the child is left running (not dismissed).
fn wait_with_timeout(mut child: process::Child, timeout: Duration, kill_on_timeout: bool) -> WaitOutcome {
    let start = std::time::Instant::now();
    let check_interval = Duration::from_millis(100);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let stdout = child.stdout.take().map(|mut s| {
                    let mut v = Vec::new();
                    let _ = io::copy(&mut s, &mut v);
                    v
                });
                return WaitOutcome::Finished(stdout);
            }
            Ok(None) => {}
            Err(_) => return WaitOutcome::Finished(None),
        }
        if start.elapsed() >= timeout {
            if kill_on_timeout {
                let _ = child.kill();
                return WaitOutcome::TimedOut;
            }
            return WaitOutcome::TimedOutWithChild(child);
        }
        thread::sleep(check_interval);
    }
}

/// Wait for process to finish indefinitely (no timeout). Used for "Enter new activity" dialog.
fn wait_no_timeout(mut child: process::Child) -> Option<Vec<u8>> {
    match child.wait() {
        Ok(_) => child.stdout.take().map(|mut s| {
            let mut v = Vec::new();
            let _ = io::copy(&mut s, &mut v);
            v
        }),
        Err(_) => None,
    }
}

fn main() {
    if env::var_os("TS_DEBUG").is_some() {
        let _ = std::io::stderr().write_all(b"ts: main entered\n");
    }
    #[cfg(unix)]
    unsafe {
        signal(libc::SIGPIPE, SIG_IGN);
    }
    let mut args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().cloned();
    let rest: Vec<String> = if args.len() > 1 {
        args.drain(1..).collect()
    } else {
        Vec::new()
    };
    let timesheet = timesheet_path();

    if cmd.as_deref() == Some("--reminder-daemon") {
        run_reminder_daemon(&timesheet);
        process::exit(0);
    }

    if cmd.as_deref() == Some("--session-daemon") {
        run_session_daemon(&timesheet);
        process::exit(0);
    }

    #[cfg(target_os = "macos")]
    if cmd.as_deref() == Some("--reminder-dialog") {
        let choices: Vec<String> = rest.clone();
        if let Some(selected) = reminder_dialog_macos::run_native_reminder_dialog(choices) {
            println!("{}", selected);
        }
        process::exit(0);
    }

    if env::var_os("TS_DEBUG").is_some() {
        let cmd_name = cmd.as_deref().unwrap_or("(none)");
        let _ = std::io::stderr().write_fmt(format_args!("ts: dispatching to {:?}\n", cmd_name));
    }

    let result = match cmd.as_deref() {
        None => cmd_help(),
        Some("start") => cmd_start(&rest, &timesheet),
        Some("stop") => cmd_stop(&rest, &timesheet),
        Some("stopped") => cmd_stop(&rest, &timesheet),
        Some("list") => cmd_list(rest.first().map(String::as_str), &timesheet),
        Some("tail") => cmd_tail(rest.first().map(String::as_str), &timesheet),
        Some("started") => cmd_started(&rest, &timesheet),
        Some("timeoff") => cmd_timeoff(&timesheet),
        Some("alias") => cmd_workalias(&rest, &timesheet),
        Some("rename") => cmd_workalias(&rest, &timesheet),
        Some("install") => cmd_install(&rest),
        Some("uninstall") => cmd_uninstall(&rest),
        Some("rebuild") => cmd_rebuild(&rest),
        Some("rotate") => do_rotate(&timesheet),
        Some("migrate") => cmd_migrate(&timesheet),
        Some("interval") => cmd_interval(&rest, &timesheet),
        Some("restart") | Some("reminder") => cmd_interval(&rest, &timesheet),
        Some("autostart") => cmd_autostart(&rest),
        Some("manpage") => cmd_manpage(),
        Some("help") => cmd_help(),
        Some(_) => cmd_help(),
    };
    if let Err(e) = result {
        eprintln!("{}", e);
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    /// Helper: format epoch as RFC3339 for log file content (replaces format_epoch_iso8601 in tests).
    fn fmt_ts(epoch: i64) -> String {
        Local.timestamp_opt(epoch, 0).single().unwrap().to_rfc3339()
    }

    #[test]
    fn test_parse_line_start() {
        let line = "2023-11-14T22:13:20-05:00|START|coding";
        let parsed = parse_line(line);
        if let Some(LogLine::Start(dt, a)) = parsed {
            assert_eq!(dt.naive_local(), chrono::NaiveDateTime::parse_from_str("2023-11-14T22:13:20", "%Y-%m-%dT%H:%M:%S").unwrap());
            assert_eq!(a, "coding");
        } else {
            panic!("expected Some(Start)");
        }
    }

    #[test]
    fn test_parse_line_start_empty_activity() {
        let line = "2023-11-14T22:13:20-05:00|START|";
        let parsed = parse_line(line);
        if let Some(LogLine::Start(dt, a)) = parsed {
            assert_eq!(dt.naive_local(), chrono::NaiveDateTime::parse_from_str("2023-11-14T22:13:20", "%Y-%m-%dT%H:%M:%S").unwrap());
            assert!(a.is_empty());
        } else {
            panic!("expected Some(Start)");
        }
    }

    #[test]
    fn test_parse_line_start_activity_with_pipe() {
        let line = "2023-11-14T22:13:20-05:00|START|misc|unspecified";
        let parsed = parse_line(line);
        if let Some(LogLine::Start(dt, a)) = parsed {
            assert_eq!(dt.naive_local(), chrono::NaiveDateTime::parse_from_str("2023-11-14T22:13:20", "%Y-%m-%dT%H:%M:%S").unwrap());
            assert_eq!(a, "misc|unspecified");
        } else {
            panic!("expected Some(Start)");
        }
    }

    #[test]
    fn test_parse_line_stop() {
        let line = "2023-11-14T23:13:20-05:00|STOP";
        let parsed = parse_line(line);
        if let Some(LogLine::Stop(dt)) = parsed {
            assert_eq!(dt.naive_local(), chrono::NaiveDateTime::parse_from_str("2023-11-14T23:13:20", "%Y-%m-%dT%H:%M:%S").unwrap());
        } else {
            panic!("expected Some(Stop)");
        }
    }

    #[test]
    fn test_parse_line_iso8601() {
        let line_start = "2026-03-06T14:30:00-08:00|START|coding";
        if let Some(LogLine::Start(dt, a)) = parse_line(line_start) {
            assert_eq!(a, "coding");
            // Wall-clock time is preserved from the stored offset without UTC conversion.
            assert_eq!(dt.naive_local(), chrono::NaiveDateTime::parse_from_str("2026-03-06T14:30:00", "%Y-%m-%dT%H:%M:%S").unwrap());
        } else {
            panic!("expected Some(Start)");
        }
        let line_stop = "2026-03-06T18:45:00-08:00|STOP";
        if let Some(LogLine::Stop(dt)) = parse_line(line_stop) {
            assert_eq!(dt.naive_local(), chrono::NaiveDateTime::parse_from_str("2026-03-06T18:45:00", "%Y-%m-%dT%H:%M:%S").unwrap());
        } else {
            panic!("expected Some(Stop)");
        }
    }

    #[test]
    fn test_parse_line_invalid() {
        assert!(parse_line("").is_none());
        assert!(parse_line("  \n  ").is_none());
        assert!(parse_line("START").is_none());
        assert!(parse_line("STOP").is_none());
        assert!(parse_line("not-iso8601|START|act").is_none());
        assert!(parse_line("not-iso8601|STOP").is_none());
        assert!(parse_line("2026-03-06T12:00:00Z|OTHER|x").is_none());
    }

    #[test]
    fn test_parse_line_whitespace_trimmed() {
        let line = "  2023-11-14T22:13:20-05:00|START|  x  ";
        let parsed = parse_line(line);
        if let Some(LogLine::Start(dt, activity)) = parsed {
            assert_eq!(dt.naive_local(), chrono::NaiveDateTime::parse_from_str("2023-11-14T22:13:20", "%Y-%m-%dT%H:%M:%S").unwrap());
            assert_eq!(activity, "  x");
        } else {
            panic!("expected Some(Start)");
        }
    }

    #[test]
    fn test_week_start() {
        // 2023-11-14 12:00:00 UTC-ish Tuesday -> week start is Sunday 2023-11-12 00:00:00 local
        let tuesday = Local.timestamp_opt(1700000000, 0).single().unwrap();
        let week_start_dt = week_start(tuesday);
        assert_eq!(week_start_dt.weekday(), chrono::Weekday::Sun);
        assert_eq!(week_start_dt.hour(), 0);
        assert_eq!(week_start_dt.minute(), 0);
    }

    #[test]
    fn test_timesheet_path_uses_home() {
        let path = timesheet_path();
        assert!(path.ends_with("Documents/timesheet.log") || path.ends_with("Documents\\timesheet.log"));
    }

    #[test]
    fn test_last_line_dt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        fs::write(
            &path,
            format!("{}|START|a\n{}|STOP\n", fmt_ts(100), fmt_ts(200)),
        )
        .unwrap();
        assert_eq!(last_line_dt(&path).map(|d| d.timestamp()), Some(200));
        fs::write(&path, format!("{}|START|a\n", fmt_ts(100))).unwrap();
        assert_eq!(last_line_dt(&path).map(|d| d.timestamp()), Some(100));
        fs::write(&path, "").unwrap();
        assert!(last_line_dt(&path).is_none());
    }

    #[test]
    fn test_max_dt_in_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        fs::write(
            &path,
            format!(
                "{}|START|a\n{}|STOP\n{}|START|b\n",
                fmt_ts(100),
                fmt_ts(200),
                fmt_ts(150)
            ),
        )
        .unwrap();
        assert_eq!(max_dt_in_log(&path).map(|d| d.timestamp()), Some(200));
        fs::write(&path, "").unwrap();
        assert!(max_dt_in_log(&path).is_none());
        fs::write(&path, "comment\n").unwrap();
        assert!(max_dt_in_log(&path).is_none());
    }

    #[test]
    fn test_do_rotate_renames_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::write(
            &log_path,
            format!(
                "{}|START|coding\n{}|STOP\n",
                fmt_ts(1730000000),
                fmt_ts(1730003600)
            ),
        )
        .unwrap();
        let result = do_rotate(&log_path);
        assert!(result.is_ok());
        assert!(!log_path.exists());
        let stamp = chrono::Local
            .timestamp_opt(1730003600, 0)
            .single()
            .unwrap()
            .format("%y%m%d")
            .to_string();
        let rotated = dir.path().join(format!("timesheet.{}", stamp));
        assert!(rotated.exists(), "expected timesheet.{} to exist", stamp);
        let content = fs::read_to_string(&rotated).unwrap();
        assert!(content.contains("|START|coding"));
    }

    #[test]
    fn test_do_rotate_appends_when_same_day_exists() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::write(
            &log_path,
            format!(
                "{}|START|first\n{}|STOP\n",
                fmt_ts(1730000000),
                fmt_ts(1730001000)
            ),
        )
        .unwrap();
        let stamp = chrono::Local
            .timestamp_opt(1730000000, 0)
            .single()
            .unwrap()
            .format("%y%m%d")
            .to_string();
        let dest = dir.path().join(format!("timesheet.{}", stamp));
        fs::write(
            &dest,
            format!(
                "{}|START|old\n{}|STOP\n",
                fmt_ts(1729900000),
                fmt_ts(1729901000)
            ),
        )
        .unwrap();
        let result = do_rotate(&log_path);
        assert!(result.is_ok());
        assert!(!log_path.exists());
        let content = fs::read_to_string(&dest).unwrap();
        assert!(content.contains("old"));
        assert!(content.contains("first"));
    }

    #[test]
    fn test_do_rotate_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = do_rotate(&log_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no timesheet data"));
    }

    #[test]
    fn test_do_rotate_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::write(&log_path, "").unwrap();
        let result = do_rotate(&log_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no valid entries"));
    }

    #[test]
    fn test_maybe_rotate_does_nothing_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = maybe_rotate_if_previous_week(&log_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_list_input_none_returns_timesheet() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let out = resolve_list_input(None, &log_path).unwrap();
        assert_eq!(out, log_path);
    }

    #[test]
    fn test_resolve_list_input_log_returns_timesheet() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let out = resolve_list_input(Some("log"), &log_path).unwrap();
        assert_eq!(out, log_path);
    }

    #[test]
    fn test_resolve_list_input_exact_extension() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let rotated = dir.path().join("timesheet.260220");
        fs::File::create(&rotated).unwrap();
        let out = resolve_list_input(Some("260220"), &log_path).unwrap();
        assert_eq!(out, rotated);
    }

    #[test]
    fn test_resolve_list_input_substring_extension() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let rotated = dir.path().join("timesheet.260220");
        fs::File::create(&rotated).unwrap();
        let out = resolve_list_input(Some("0220"), &log_path).unwrap();
        assert_eq!(out, rotated);
    }

    #[test]
    fn test_resolve_list_input_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let result = resolve_list_input(Some("999999"), &log_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no timesheet matches"));
    }

    #[test]
    fn test_resolve_list_input_date_in_range_fallback() {
        // No timesheet.250219 exists, but timesheet.250301 has entries spanning 2025-02-19 -> use it for 250219
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let later = dir.path().join("timesheet.250301");
        // Epochs on 2025-02-19 and 2025-03-02 so the file's date range includes 2025-02-19
        fs::write(
            &later,
            "START|1739984400|a\nSTOP|1740891600\n",
        )
            .unwrap(); // 2025-02-19 12:00 UTC, 2025-03-02 00:00 UTC
        let out = resolve_list_input(Some("250219"), &log_path).unwrap();
        assert_eq!(out, later, "ts list 250219 should use log that contains that date");
    }

    #[test]
    fn test_resolve_list_input_date_fallback_by_extension() {
        // No timesheet.250219; timesheet.260220 exists (extension 2026-02-20 >= 2025-02-19). Use it for 2/19.
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let later = dir.path().join("timesheet.260220");
        fs::File::create(&later).unwrap(); // empty or no 2025-02-19 in content
        let out = resolve_list_input(Some("2/19"), &log_path).unwrap();
        assert_eq!(out, later, "ts list 2/19 should fall back to file with extension date on or after that day");
    }

    #[test]
    fn test_process_log_for_report_one_pair() {
        let dt1 = Local.timestamp_opt(1000, 0).single().unwrap();
        let dt2 = Local.timestamp_opt(4600, 0).single().unwrap();
        let lines = vec![
            (1, LogLine::Start(dt1, "coding".to_string())),
            (2, LogLine::Stop(dt2)),
        ];
        let (by_act, dow_hr, wip) = process_log_for_report(&lines, None);
        assert!(!wip);
        assert_eq!(by_act.len(), 1);
        assert_eq!(by_act[0].0, "coding");
        assert!((by_act[0].1 - 100.0).abs() < 0.01);
        assert!((dow_hr.iter().sum::<f64>() - 3600.0 / 3600.0).abs() < 0.01);
    }

    #[test]
    fn test_process_log_for_report_virtual_stop() {
        let dt1 = Local.timestamp_opt(1000, 0).single().unwrap();
        let vstop = Local.timestamp_opt(2000, 0).single().unwrap();
        let lines = vec![(1, LogLine::Start(dt1, "x".to_string()))];
        let (by_act, _, wip) = process_log_for_report(&lines, Some(vstop));
        assert!(!wip);
        assert_eq!(by_act.len(), 1);
        assert_eq!(by_act[0].0, "x");
        assert!((by_act[0].1 - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_start_time_ymd_hm() {
        let dt = parse_start_time("2025-02-20 09:00");
        assert!(dt.is_some());
        let dt = dt.unwrap();
        assert_eq!(dt.year(), 2025);
        assert_eq!(dt.month(), 2);
        assert_eq!(dt.day(), 20);
        assert_eq!(dt.hour(), 9);
        assert_eq!(dt.minute(), 0);
    }

    #[test]
    fn test_parse_start_time_hm() {
        let dt = parse_start_time("14:30");
        assert!(dt.is_some());
        let dt = dt.unwrap();
        assert_eq!(dt.hour(), 14);
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn test_parse_start_time_invalid() {
        assert!(parse_start_time("").is_none());
        assert!(parse_start_time("not-a-date").is_none());
    }

    #[test]
    fn test_cmd_start_appends_line() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_start(&["my-activity".to_string()], &log_path);
        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("|START|"));
        assert!(content.contains("my-activity"));
    }

    #[test]
    fn test_cmd_start_default_activity() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_start(&[], &log_path);
        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("misc/unspecified"));
    }

    #[test]
    fn test_cmd_stop_appends_when_last_is_start() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let now = Local::now();
        let week_start_dt = week_start(now);
        let start_epoch = week_start_dt.timestamp() + 3600;
        fs::write(&log_path, format!("{}|START|coding\n", fmt_ts(start_epoch))).unwrap();
        let result = cmd_stop(&[], &log_path);
        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "expected START and STOP lines, got: {:?}", lines);
        assert!(lines[0].contains("|START|"));
        assert!(lines[1].contains("|STOP"));
    }

    #[test]
    fn test_cmd_stop_no_op_when_last_is_stop() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let now = Local::now();
        let week_start_dt = week_start(now);
        fs::write(
            &log_path,
            format!(
                "{}|START|coding\n{}|STOP\n",
                fmt_ts(week_start_dt.timestamp() + 3600),
                fmt_ts(week_start_dt.timestamp() + 7200)
            ),
        )
        .unwrap();
        let before = fs::read_to_string(&log_path).unwrap();
        let result = cmd_stop(&[], &log_path);
        assert!(result.is_ok());
        let after = fs::read_to_string(&log_path).unwrap();
        assert_eq!(before, after, "ts stop should not change file when last entry is STOP and no time given");
    }

    #[test]
    fn test_cmd_stop_amends_last_stop_when_time_given() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let now = Local::now();
        let week_start_dt = week_start(now);
        let old_stop = week_start_dt.timestamp() + 7200;
        fs::write(
            &log_path,
            format!(
                "{}|START|coding\n{}|STOP\n",
                fmt_ts(week_start_dt.timestamp() + 3600),
                fmt_ts(old_stop)
            ),
        )
        .unwrap();
        let new_time = "2026-02-20 15:00";
        let result = cmd_stop(&[new_time.to_string()], &log_path);
        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("|STOP"));
        let new_epoch = parse_line(lines[1]).and_then(|ll| match ll {
            LogLine::Stop(e) => Some(e),
            _ => None,
        }).unwrap();
        let expected = parse_start_time(new_time).unwrap();
        assert_eq!(new_epoch, expected, "last STOP should be amended to the given time");
    }

    #[test]
    fn test_cmd_list_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_list(None, &log_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cmd_list_with_data() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::write(
            &log_path,
            format!(
                "{}|START|coding\n{}|STOP\n",
                fmt_ts(1730000000),
                fmt_ts(1730003600)
            ),
        )
        .unwrap();
        let result = cmd_list(None, &log_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cmd_started_missing_args() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_started(&[], &log_path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("missing start_time") || err.contains("parse"));
    }

    #[test]
    fn test_cmd_started_appends() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_started(
            &["2025-02-20 10:00".to_string(), "manual".to_string()],
            &log_path,
        );
        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("|START|"));
        assert!(content.contains("manual"));
    }

    #[test]
    fn test_cmd_started_inserts_chronologically() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let now = chrono::Local::now().timestamp();
        let week_start = week_start(Local.timestamp_opt(now, 0).single().unwrap());
        let e_early = week_start.timestamp() + 6 * 3600;
        let e_stop = week_start.timestamp() + 7 * 3600;
        let e_late = week_start.timestamp() + 10 * 3600;
        fs::write(
            &log_path,
            format!(
                "{}|START|early\n{}|STOP\n{}|START|late\n",
                fmt_ts(e_early),
                fmt_ts(e_stop),
                fmt_ts(e_late)
            ),
        )
        .unwrap();
        let new_epoch = week_start.timestamp() + 8 * 3600;
        let new_time = chrono::Local
            .timestamp_opt(new_epoch, 0)
            .single()
            .unwrap()
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let result = cmd_started(&[new_time, "mid".to_string()], &log_path);
        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("early"));
        assert!(content.contains("mid"));
        assert!(content.contains("late"));
        let early_pos = content.find("early").unwrap();
        let mid_pos = content.find("mid").unwrap();
        let late_pos = content.find("late").unwrap();
        assert!(early_pos < mid_pos, "early should come before mid");
        assert!(mid_pos < late_pos, "mid should come before late");
    }

    #[test]
    fn test_cmd_timeoff_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_timeoff(&log_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cmd_workalias_missing_args() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let result = cmd_workalias(&[], &log_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_cmd_workalias_one_arg() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let result = cmd_workalias(&["pattern".to_string()], &log_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_cmd_workalias_no_timesheet() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_workalias(
            &["coding".to_string(), "dev".to_string()],
            &log_path,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no timesheet data"));
    }

    #[test]
    fn test_cmd_workalias_no_match_this_week() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        // Entry from this week (use current week_start..week_end)
        let now = chrono::Local::now().timestamp();
        let week_start = week_start(Local.timestamp_opt(now, 0).single().unwrap());
        fs::write(
            &log_path,
            format!(
                "{}|START|other\n{}|STOP\n",
                fmt_ts(week_start.timestamp()),
                fmt_ts(week_start.timestamp() + 100)
            ),
        )
        .unwrap();
        let result = cmd_workalias(
            &["nonexistent".to_string(), "repl".to_string()],
            &log_path,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no activities matching"));
    }

    #[test]
    fn test_cmd_install_to_dir() {
        let dest_dir = tempfile::tempdir().unwrap();
        let dest_path = dest_dir.path().to_path_buf();
        let result = cmd_install(&[dest_path.to_string_lossy().to_string()]);
        assert!(result.is_ok());
        let exe_name = if cfg!(windows) { "ts.exe" } else { "ts" };
        let installed = dest_path.join(exe_name);
        assert!(installed.exists());
    }
}
