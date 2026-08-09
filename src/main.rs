// Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
// Co-author: Cursor-AI.

//! # timesheet — Timesheet CLI
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
//! ## Configuration
//!
//! Optional settings live in `$HOME/.config/timesheet.yml` (see [`config_path`]). `rotate` says
//! when a new timesheet week begins — the boundary at which the log is automatically rotated
//! (default Sunday 00:00 local). The rest supply defaults for `pdf` and `email`, and may be
//! written at the top level or, under `prefixes:`, per job tag; see [`settings`] and the
//! CONFIGURATION section of the man page.
//!
//! ```yaml
//! rotate:
//!   day: monday
//!   time: "00:00"
//! name: "Jane Contractor"
//! prefixes:
//!   ST:
//!     template: "~/Documents/timesheet-fillable.pdf"
//!     to: "timesheets@employer.example"
//! ```
//!
//! ## Subcommands
//!
//! | Command    | Description |
//! |------------|-------------|
//! | `alias`    | Interactively replace activity text in this week's START entries (regex). |
//! | `autostart` | Register `timesheet start` on login and `timesheet stop` on logout/shutdown (macOS/Linux: LaunchAgents/systemd units plus an admin-installed logout hook; Windows: a Startup-folder shortcut plus the daemon's best-effort console control handler, no admin required or available). |
//! | `edit`     | Open the timesheet log in `$EDITOR` (then `$VISUAL`, else `vi`; the `.txt`-associated program on Windows). |
//! | `email`    | Fill the timesheet PDF as `pdf` does and mail it as an attachment. |
//! | `help`     | Show the man page in a pager (groff -man -Tascii \| less; plain text via `more` on Windows). |
//! | `install`  | Copy binary and icon to a directory on PATH (icon embedded on macOS). |
//! | `interval` | Set or show reminder daemon interval (e.g. 3, 3m, 100s, 1h30m). |
//! | `list`     | Report % per activity and hours per weekday; optional file/extension arg, date, or negative rotated-log index; `-p/--prefix` reports one job only. |
//! | `migrate`  | Convert all timesheet.* files in the log directory to strict ISO 8601 timestamps. |
//! | `pdf`      | Fill a form-fillable PDF template with one week of the timesheet; optional file/date/index arg selects the week. |
//! | `prefix`   | `timesheet prefix foo bar` is `timesheet alias bar foo:bar`: prepend `foo:` to this week's activities matching `bar`. |
//! | `sprint`   | Report % per activity and hours per weekday across the current log plus the most recently rotated log. |
//! | `tail`     | Last 10 log entries with timestamps in local time; optional file/extension arg. |
//! | `manpage`  | Output Unix manual page in groff format to stdout. |
//! | `rebuild`  | Build from local dir or clone; then install to current binary's directory. |
//! | `rename`   | Same as `alias`. |
//! | `restart`, `reminder` | Aliases for `interval`. |
//! | `rotate`   | Rename log to `timesheet.YYMMDD`; add STOP first if last entry is START; append if same-day exists. |
//! | `start`    | Record work start now; with no activity, shows reminder chooser to pick/enter (macOS via AppKit; Linux via PyQt single-click chooser, falling back to kdialog/zenity; Windows via PowerShell/WinForms); otherwise optional activity (default: misc/unspecified); adds a STOP first only when the open session is over one reminder interval old (otherwise the START closes it by itself); starts/restarts reminder daemon. |
//! | `started`  | Record a past start time; inserts at the correct chronological position without discarding entries. |
//! | `stop`     | Record work stop (optional time); amends previous STOP if work already stopped; always stops the reminder daemon and closes any prompt it has on screen, and shows the "stopped" dialog (skipped during logout/shutdown). |
//! | `timeoff`  | Show stop time for 8 h/day average; only requires a START entry (adds one if log empty or last is STOP). |
//! | `uninstall` | Stop daemon, remove autostart hooks, optionally remove log files, remove binary and icon. |

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime, SecondsFormat, Weekday};
#[cfg(target_os = "macos")]
use libc::getuid;
#[cfg(unix)]
use libc::{
    getpgid, getpgrp, kill, pthread_sigmask, setpgid, setsid, sigaddset, sigemptyset, signal,
    sigwait, SIGHUP, SIGKILL, SIGTERM, SIG_BLOCK, SIG_IGN, SIG_SETMASK,
};
use regex::Regex;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::thread;
use std::time::Duration;

mod mail;
mod pdf;
#[cfg(target_os = "macos")]
mod reminder_dialog_macos;
mod report;
mod settings;
#[cfg(target_os = "windows")]
mod win_ffi;
mod yaml;

use yaml::{strip_yaml_comment, unquote_yaml_scalar};

/// Default path segment under `$HOME` for the timesheet log file.
const DEFAULT_TIMESHEET: &str = "Documents/timesheet.log";

/// Canonical source repository for this project.
const CANONICAL_SOURCE_URL: &str = "https://github.com/pillarsdotnet/timesheet";

/// Icon for macOS reminder dock; embedded so "timesheet install" can write it without the repo.
#[cfg(target_os = "macos")]
const EMBEDDED_ICON_SVG: &[u8] = include_bytes!("../assets/icon.svg");

/// Weekday names for the list report (Sunday first).
const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Truncate hours to two decimal places (discard fractions beyond the second decimal).
fn trunc2(h: f64) -> f64 {
    (h * 100.0).trunc() / 100.0
}

/// Formats a log timestamp using the canonical on-disk representation.
fn format_log_timestamp(dt: DateTime<Local>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// Formats a START log line without the trailing newline.
fn format_start_log_entry(dt: DateTime<Local>, activity: &str) -> String {
    format!("{}|START|{}", format_log_timestamp(dt), activity)
}

/// Formats a STOP log line without the trailing newline.
fn format_stop_log_entry(dt: DateTime<Local>) -> String {
    format!("{}|STOP", format_log_timestamp(dt))
}

/// Caps automatic STOP timestamps so they do not land more than one reminder interval after the
/// latest log entry. The interval is how often you are prompted (default 5 minutes), so a session
/// you forgot to stop is recorded as ending at most one interval after your last logged activity.
fn clamp_auto_stop_time(timesheet: &Path, requested_dt: DateTime<Local>) -> DateTime<Local> {
    let Some(last_dt) = last_line_dt(timesheet) else {
        return requested_dt;
    };
    let cap = chrono::Duration::seconds(get_reminder_interval_secs() as i64);
    std::cmp::min(requested_dt, last_dt + cap)
}

fn append_log_entry(timesheet: &Path, entry: &str) -> Result<(), String> {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(timesheet)
        .map_err(|e| e.to_string())?;
    f.write_all(format!("{}\n", entry).as_bytes())
        .map_err(|e| e.to_string())
}

/// Returns the default timesheet path: `$HOME/Documents/timesheet.log`, falling back to the
/// platform home directory (e.g. `%USERPROFILE%` on Windows) when `HOME` is unset, or
/// `./Documents/timesheet.log` if neither is available.
fn timesheet_path() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_TIMESHEET)
}

/// Path for the reminder daemon PID file (under $HOME/.cache, $XDG_CACHE_HOME, or the
/// platform cache directory, e.g. `%LOCALAPPDATA%` on Windows).
fn reminder_pid_path() -> PathBuf {
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .or_else(dirs::cache_dir);
    cache
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ts-reminder.pid")
}

/// Atomically claim sole ownership of the reminder daemon by creating the PID file with O_EXCL.
/// Returns true if this process now owns the daemon role, false if a live daemon already owns it.
/// This makes the daemon self-deduplicating: if several are spawned in a race (interactive `timesheet start`,
/// `timesheet autostart`, and the systemd start unit can all fire near-simultaneously), only the first to
/// claim the file runs the loop; the rest see a live owner and exit.
fn claim_reminder_daemon_ownership(pid_path: &Path) -> bool {
    let my_pid = process::id();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(pid_path)
        {
            Ok(mut f) => {
                let _ = f.write_all(my_pid.to_string().as_bytes());
                return true;
            }
            Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // A pid file exists; keep it only if it names a live process other than us.
                // If the file vanished between the failed create and this read, just retry.
                if let Ok(data) = fs::read_to_string(pid_path) {
                    if let Ok(pid) = data.trim().parse::<u32>() {
                        if pid != my_pid && is_pid_running(pid) {
                            return false;
                        }
                    }
                    // Stale or unparsable owner: remove and retry the exclusive create.
                    let _ = fs::remove_file(pid_path);
                }
            }
            Err(_) => return false,
        }
    }
}

/// True if the reminder PID file still names this process (i.e. we are the current owner).
fn owns_reminder_daemon(pid_path: &Path) -> bool {
    fs::read_to_string(pid_path)
        .ok()
        .map(|d| d.trim() == process::id().to_string())
        .unwrap_or(false)
}

/// How often a daemon re-checks that it still owns the PID file, while sleeping between prompts
/// and while a prompt is on screen.
const REMINDER_OWNERSHIP_POLL: Duration = Duration::from_millis(500);

/// Set once this process has claimed the daemon role. Code shared with the foreground `timesheet start`
/// chooser uses it to tell the two apart: the foreground chooser never owns the PID file and must
/// not treat that as a reason to close itself.
static IS_REMINDER_DAEMON: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// True when this process is a reminder daemon that no longer owns the PID file — `timesheet stop` removed
/// it, or a newer daemon replaced it. Both mean this daemon and any prompt it has on screen should
/// go away.
fn reminder_daemon_disowned() -> bool {
    IS_REMINDER_DAEMON.load(std::sync::atomic::Ordering::Relaxed)
        && !owns_reminder_daemon(&reminder_pid_path())
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

/// Activities from the current timesheet plus the most recently rotated timesheet,
/// limited to START entries from the last 7 days and sorted most-recent first.
fn reminder_activities_most_recent_first(timesheet: &Path) -> Vec<String> {
    reminder_activities_most_recent_first_at(timesheet, Local::now())
}

fn reminder_activities_most_recent_first_at(timesheet: &Path, now: DateTime<Local>) -> Vec<String> {
    // Stored timestamps are floored to microsecond precision (see format_log_timestamp), so floor
    // the cutoff to microseconds too; otherwise an entry written exactly at the 7-day boundary is
    // dropped because its re-parsed value lands just below a nanosecond-precision cutoff.
    let cutoff = now - chrono::Duration::days(7);
    let cutoff =
        cutoff - chrono::Duration::nanoseconds((cutoff.timestamp_subsec_nanos() % 1000) as i64);
    let mut by_activity: std::collections::HashMap<String, DateTime<Local>> =
        std::collections::HashMap::new();

    let mut sources = Vec::with_capacity(2);
    if let Some(path) = latest_rotated_timesheet(timesheet) {
        sources.push(path);
    }
    sources.push(timesheet.to_path_buf());

    for path in sources {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            if let Some(LogLine::Start(dt, activity)) = migrate_parse_line(line) {
                if dt >= cutoff && dt <= now {
                    let replace = by_activity
                        .get(&activity)
                        .map(|existing| dt > *existing)
                        .unwrap_or(true);
                    if replace {
                        by_activity.insert(activity, dt);
                    }
                }
            }
        }
    }
    let mut order: Vec<(String, DateTime<Local>)> = by_activity.into_iter().collect();
    order.sort_by_key(|b| std::cmp::Reverse(b.1));
    order.into_iter().map(|(a, _)| a).collect()
}

/// Append a START log entry for the given activity (used by reminder daemon). Calls maybe_rotate first.
fn append_start_entry(timesheet: &Path, activity: &str) -> Result<(), String> {
    maybe_rotate_if_previous_week(timesheet)?;
    let now = Local::now();
    append_log_entry(timesheet, &format_start_log_entry(now, activity))
}

/// Append an automatic STOP log entry, capped to no more than one reminder interval after the latest log entry.
fn append_stop_entry(timesheet: &Path, dt: DateTime<Local>) -> Result<(), String> {
    let dt = clamp_auto_stop_time(timesheet, dt);
    maybe_rotate_if_previous_week(timesheet)?;
    append_log_entry(timesheet, &format_stop_log_entry(dt))
}

/// Append the STOP for a reminder prompt left unanswered for one reminder interval (see
/// [`get_reminder_interval_secs`]), timestamped at `dt` -- the moment the prompt appeared, which is
/// when you were last known to be working. Deliberately skips [`clamp_auto_stop_time`]: the prompt
/// appears one reminder interval after the previous entry, so `dt` already satisfies the "never
/// record work all night" guarantee without the cap pulling the STOP earlier than the prompt.
///
/// The prompt stays on screen after this, so picking an activity on your return opens a new session
/// at the return time and the interval away stays unbilled. Does nothing when no session is open, so
/// a prompt that is still unanswered on a later check does not add a second STOP.
fn append_reminder_timeout_stop(timesheet: &Path, dt: DateTime<Local>) -> Result<(), String> {
    maybe_rotate_if_previous_week(timesheet)?;
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    if !matches!(last_recorded_event(&content), Some(LogLine::Start(_, _))) {
        return Ok(());
    }
    append_log_entry(timesheet, &format_stop_log_entry(dt))
}

/// When a new timesheet week begins: the weekday and local time of day at which
/// [`maybe_rotate_if_previous_week`] rotates the log. Configurable in `timesheet.yml`
/// (see [`config_path`]); defaults to Sunday 00:00.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RotationBoundary {
    day: Weekday,
    time: NaiveTime,
}

impl Default for RotationBoundary {
    fn default() -> Self {
        RotationBoundary {
            day: Weekday::Sun,
            time: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        }
    }
}

/// Config file path: `$TS_CONFIG`, else `$XDG_CONFIG_HOME/timesheet.yml`, else
/// `$HOME/.config/timesheet.yml`, else the platform config directory (e.g. `%APPDATA%` on
/// Windows). A `timesheet.yaml` sibling is used when no `.yml` exists.
///
/// Under `cargo test` this resolves to a generated config instead, so the suite never depends on
/// the machine it runs on — see [`test_config_path`].
fn config_path() -> PathBuf {
    #[cfg(test)]
    {
        test_config_path()
    }
    #[cfg(not(test))]
    {
        if let Some(p) = env::var_os("TS_CONFIG") {
            return PathBuf::from(p);
        }
        let dir = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .or_else(dirs::config_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let yml = dir.join("timesheet.yml");
        if !yml.exists() {
            let yaml = dir.join("timesheet.yaml");
            if yaml.exists() {
                return yaml;
            }
        }
        yml
    }
}

/// The `timesheet.yml` the test suite runs against, generated once per test process under
/// `target/tmp/` and never read from the developer's home directory.
///
/// Several tests write a log entry minutes or hours in the past and then call an append path,
/// every one of which runs [`maybe_rotate_if_previous_week`]. Whether that backdated entry counts
/// as "previous week" depends entirely on the configured rotation boundary, so with no config of
/// its own the suite inherited whatever the machine had: green on a developer box set to Monday,
/// red on CI, which has no config at all and so fell back to the Sunday-00:00 default.
///
/// Any *fixed* boundary just moves the problem, because it leaves a window immediately after
/// itself in which a backdated entry lands in the previous week and the log is rotated out from
/// under the test. Pinning the boundary to *tomorrow's* weekday removes the window instead: the
/// most recent boundary is then always six to seven days behind, far enough that no backdated
/// entry the tests create can reach across it, whatever day and time the suite runs.
#[cfg(test)]
fn test_config_path() -> PathBuf {
    static CONFIG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            // Per-process so concurrent `cargo test` runs cannot write each other's config.
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("tmp")
                .join(format!("test-config-{}", std::process::id()));
            fs::create_dir_all(&dir).expect("create test config dir");
            let path = dir.join("timesheet.yml");
            let tomorrow = (Local::now() + chrono::Duration::days(1)).weekday();
            fs::write(&path, format!("rotate: {:?}\n", tomorrow))
                .expect("write test timesheet.yml");
            path
        })
        .clone()
}

/// Parses the supported YAML subset: `key: value` pairs, `#` comments, optional quotes, and one
/// level of nested mapping. Nested keys are returned dotted (`rotate: {day: monday}` becomes
/// `rotate.day` -> `monday`). Keys are lowercased; anything else in the file is ignored.
fn parse_simple_yaml(text: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut section: Option<String> = None;
    for raw in text.lines() {
        let line = strip_yaml_comment(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "---" {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        let value = unquote_yaml_scalar(value.trim()).trim().to_string();
        if indent == 0 {
            section = if value.is_empty() {
                Some(key.clone())
            } else {
                None
            };
            if !value.is_empty() {
                map.insert(key, value);
            }
        } else {
            match &section {
                Some(sec) => {
                    map.insert(format!("{}.{}", sec, key), value);
                }
                None => {
                    map.insert(key, value);
                }
            }
        }
    }
    map
}

/// Parses a weekday name (full or three-letter abbreviation, any case).
fn parse_weekday(s: &str) -> Option<Weekday> {
    s.trim().parse::<Weekday>().ok()
}

/// Strips `suffix` from the end of `s`, ignoring ASCII case. `suffix` must be ASCII.
fn strip_suffix_ignore_case<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = s.len().checked_sub(suffix.len())?;
    if s.is_char_boundary(cut) && s[cut..].eq_ignore_ascii_case(suffix) {
        Some(&s[..cut])
    } else {
        None
    }
}

/// Splits a trailing meridiem off a clock time, returning the remainder and whether it was PM.
/// Accepts `am`/`pm`, `a.m.`/`p.m.`, and a bare `a`/`p`, in any case.
fn split_meridiem(s: &str) -> (&str, Option<bool>) {
    for (suffix, is_pm) in [
        ("a.m.", false),
        ("p.m.", true),
        ("am", false),
        ("pm", true),
        ("a", false),
        ("p", true),
    ] {
        if let Some(rest) = strip_suffix_ignore_case(s, suffix) {
            let rest = rest.trim_end();
            // A meridiem needs digits in front of it; "am" alone is not a time.
            if !rest.is_empty() {
                return (rest, Some(is_pm));
            }
        }
    }
    (s, None)
}

/// Parses a time of day: `HH:MM`, `HH:MM:SS`, or a bare hour (`0`, `9`), optionally with a
/// meridiem (`7am`, `7 AM`, `7:30 pm`, `12:15:30a.m.`). Without a meridiem the hour is 24-hour.
fn parse_time_of_day(s: &str) -> Option<NaiveTime> {
    let (body, pm) = split_meridiem(s.trim());
    let mut fields = body.split(':');
    let mut next_field = |required: bool| -> Option<u32> {
        match fields.next() {
            Some(f) => {
                let f = f.trim();
                if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                    None
                } else {
                    f.parse().ok()
                }
            }
            None if required => None,
            None => Some(0),
        }
    };
    let hour = next_field(true)?;
    let minute = next_field(false)?;
    let second = next_field(false)?;
    if fields.next().is_some() {
        return None;
    }
    let hour = match pm {
        // A meridiem makes the hour 12-hour: 12am is midnight, 12pm is noon.
        Some(pm) => match (hour, pm) {
            (0, _) | (13.., _) => return None,
            (12, false) => 0,
            (12, true) => 12,
            (h, false) => h,
            (h, true) => h + 12,
        },
        None => hour,
    };
    NaiveTime::from_hms_opt(hour, minute, second)
}

/// Reads the rotation boundary out of a parsed config, collecting a message for each bad value.
/// Accepted forms: `rotate: monday`, `rotate: "monday 09:00"`, or a `rotate:` mapping with
/// `day:` and/or `time:` keys. Missing or invalid values fall back to the default (Sunday 00:00).
fn rotation_boundary_from_config(
    map: &std::collections::HashMap<String, String>,
) -> (RotationBoundary, Vec<String>) {
    let mut boundary = RotationBoundary::default();
    let mut warnings = Vec::new();

    // Scalar shorthand: `rotate: monday` or `rotate: monday 09:00`.
    if let Some(scalar) = map.get("rotate") {
        let mut fields = scalar.split_whitespace();
        match fields.next().and_then(parse_weekday) {
            Some(day) => boundary.day = day,
            None => warnings.push(format!("rotate: unrecognized weekday \"{}\"", scalar)),
        }
        if let Some(rest) = fields.next() {
            match parse_time_of_day(rest) {
                Some(time) => boundary.time = time,
                None => warnings.push(format!("rotate: unrecognized time \"{}\"", rest)),
            }
        }
    }
    if let Some(day) = map.get("rotate.day") {
        match parse_weekday(day) {
            Some(d) => boundary.day = d,
            None => warnings.push(format!("rotate.day: unrecognized weekday \"{}\"", day)),
        }
    }
    if let Some(time) = map.get("rotate.time") {
        match parse_time_of_day(time) {
            Some(t) => boundary.time = t,
            None => warnings.push(format!("rotate.time: unrecognized time \"{}\"", time)),
        }
    }
    (boundary, warnings)
}

/// Rotation boundary from `path`, or the default when the file is absent. Unreadable files and
/// invalid values warn on stderr and fall back to the default rather than failing the command.
fn load_rotation_boundary(path: &Path) -> RotationBoundary {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return RotationBoundary::default(),
        Err(e) => {
            eprintln!(
                "timesheet: {}: {}; using default rotation",
                path.display(),
                e
            );
            return RotationBoundary::default();
        }
    };
    let (boundary, warnings) = rotation_boundary_from_config(&parse_simple_yaml(&text));
    for w in warnings {
        eprintln!("timesheet: {}: {}", path.display(), w);
    }
    boundary
}

/// The configured rotation boundary (see [`config_path`]).
fn rotation_boundary() -> RotationBoundary {
    load_rotation_boundary(&config_path())
}

/// Resolves a local wall-clock date and time to an instant, tolerating daylight-saving
/// transitions: an ambiguous time uses the earlier instant, and a time that does not exist
/// (spring-forward gap) advances to the first instant that does.
fn local_datetime_at(date: NaiveDate, time: NaiveTime) -> DateTime<Local> {
    let naive = date.and_time(time);
    for extra_minutes in [0, 15, 30, 45, 60, 90, 120, 180] {
        let candidate = naive + chrono::Duration::minutes(extra_minutes);
        match candidate.and_local_timezone(Local) {
            chrono::LocalResult::Single(dt) => return dt,
            chrono::LocalResult::Ambiguous(earliest, _) => return earliest,
            chrono::LocalResult::None => continue,
        }
    }
    naive.and_utc().with_timezone(&Local)
}

/// DateTime of the start of the timesheet week containing `now`: the most recent occurrence of
/// the configured rotation boundary (default Sunday 00:00) at or before `now`, in local time.
fn week_start(now: DateTime<Local>) -> DateTime<Local> {
    week_start_with(now, rotation_boundary())
}

fn week_start_with(now: DateTime<Local>, boundary: RotationBoundary) -> DateTime<Local> {
    let days_back =
        (now.weekday().num_days_from_sunday() + 7 - boundary.day.num_days_from_sunday()) % 7;
    let date = now.date_naive();
    let date = date
        .checked_sub_days(chrono::Days::new(days_back as u64))
        .unwrap_or(date);
    let start = local_datetime_at(date, boundary.time);
    if start <= now {
        return start;
    }
    // The boundary has not been reached yet today; the week began a week earlier.
    let earlier = date.checked_sub_days(chrono::Days::new(7)).unwrap_or(date);
    local_datetime_at(earlier, boundary.time)
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

type ParsedLogLines = Vec<(usize, LogLine)>;
type CurrentTask = Option<(DateTime<Local>, String)>;

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

fn log_line_dt(line: &LogLine) -> DateTime<Local> {
    match line {
        LogLine::Start(dt, _) | LogLine::Stop(dt) => *dt,
    }
}

fn parse_log_lines(content: &str) -> ParsedLogLines {
    let mut lines = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if let Some(parsed) = parse_line(line) {
            lines.push((i + 1, parsed));
        }
    }
    lines
}

fn read_log_lines(path: &Path) -> Result<ParsedLogLines, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    Ok(parse_log_lines(&content))
}

fn last_recorded_event(content: &str) -> Option<LogLine> {
    content.lines().rev().find_map(parse_line)
}

/// If the last log entry is an open START, append a STOP (capped to no more than one reminder
/// interval after the latest entry) to close the session, and return `true`. No-op returning `false`
/// if work is already stopped or the log is empty/unreadable.
fn close_open_session(timesheet: &Path, now: DateTime<Local>) -> bool {
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    let open = last_recorded_event(&content)
        .map(|ll| matches!(ll, LogLine::Start(_, _)))
        .unwrap_or(false);
    if open {
        let _ = append_stop_entry(timesheet, now);
    }
    open
}

/// Close an open session ahead of a new START recorded at `now`, writing a STOP only when one is
/// actually needed. Start/stop pairs match in LIFO order, so a STOP at the same instant as the new
/// START is redundant -- the START closes the previous session by itself. A STOP is written only when
/// [`clamp_auto_stop_time`] places it before `now`, which is the case that matters: it leaves a
/// deliberate unbilled gap for time spent away from an open session. Returns whether a STOP was
/// written.
fn close_open_session_before_start(timesheet: &Path, now: DateTime<Local>) -> bool {
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    let open = last_recorded_event(&content)
        .map(|ll| matches!(ll, LogLine::Start(_, _)))
        .unwrap_or(false);
    if !open || clamp_auto_stop_time(timesheet, now) >= now {
        return false;
    }
    let _ = append_stop_entry(timesheet, now);
    true
}

/// Reconcile a session left open by a missed shutdown/logout STOP: if the last log entry is a START
/// older than five minutes, close it with a STOP capped to one reminder interval after that entry
/// (via clamp_auto_stop_time) so a shutdown without `timesheet stop` never records work all night. A recent
/// open session is left untouched so we never close one the user is actively working on. Returns
/// whether a STOP was written.
fn reconcile_stale_open_session(timesheet: &Path, now: DateTime<Local>) -> bool {
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    if let Some(LogLine::Start(dt, _)) = last_recorded_event(&content) {
        if now.signed_duration_since(dt) > chrono::Duration::minutes(5) {
            let stop_dt = clamp_auto_stop_time(timesheet, now);
            let _ = append_log_entry(timesheet, &format_stop_log_entry(stop_dt));
            return true;
        }
    }
    false
}

fn last_start_entry(lines: &[(usize, LogLine)]) -> CurrentTask {
    lines.iter().rev().find_map(|(_, line)| match line {
        LogLine::Start(dt, activity) => Some((*dt, activity.clone())),
        LogLine::Stop(_) => None,
    })
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

/// Minimum DateTime among all START/STOP lines in the log; `None` if no valid entries.
fn min_dt_in_log(path: &Path) -> Option<DateTime<Local>> {
    let content = fs::read_to_string(path).ok()?;
    let mut min: Option<DateTime<Local>> = None;
    for line in content.lines() {
        match parse_line(line) {
            Some(LogLine::Start(dt, _)) | Some(LogLine::Stop(dt)) if min.is_none_or(|m| dt < m) => {
                min = Some(dt);
            }
            _ => {}
        }
    }
    min
}

/// Date range (min, max) of all START/STOP entries in the log; `None` if no valid entries.
fn date_range_in_log(path: &Path) -> Option<(NaiveDate, NaiveDate)> {
    let content = fs::read_to_string(path).ok()?;
    let mut min_dt: Option<DateTime<Local>> = None;
    let mut max_dt: Option<DateTime<Local>> = None;
    for line in content.lines() {
        match parse_line(line) {
            Some(LogLine::Start(dt, _)) | Some(LogLine::Stop(dt)) => {
                if min_dt.is_none_or(|m| dt < m) {
                    min_dt = Some(dt);
                }
                if max_dt.is_none_or(|m| dt > m) {
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

/// Rotates the log: renames it to `timesheet.YYMMDD` using the earliest entry's date.
/// If that file already exists (same day), appends the current log to it and removes the source.
/// If the last entry is START (work in progress), appends a STOP no later than one reminder interval after that entry before rotating.
fn do_rotate(timesheet: &Path) -> Result<(), String> {
    if !timesheet.exists() {
        return Err("timesheet rotate: no timesheet data found.".to_string());
    }
    let content = fs::read_to_string(timesheet).map_err(|e| e.to_string())?;
    let last = content.lines().rev().find(|l| !l.trim().is_empty());
    if last
        .and_then(parse_line)
        .map(|ll| matches!(ll, LogLine::Start(..)))
        .unwrap_or(false)
    {
        let stop_dt = clamp_auto_stop_time(timesheet, Local::now());
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(timesheet)
            .map_err(|e| e.to_string())?;
        f.write_all(format!("{}\n", format_stop_log_entry(stop_dt)).as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let min_dt =
        min_dt_in_log(timesheet).ok_or("timesheet rotate: no valid entries in timesheet.")?;
    let stamp = min_dt.format("%y%m%d").to_string();
    let parent = timesheet
        .parent()
        .ok_or("timesheet rotate: no parent dir")?;
    let stem = timesheet
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("timesheet");
    let dest = parent.join(format!("{}.{}", stem, stamp));
    let content = fs::read_to_string(timesheet).map_err(|e| e.to_string())?;
    if dest.exists() {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&dest)
            .map_err(|e| e.to_string())?;
        f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
        fs::remove_file(timesheet).map_err(|e| e.to_string())?;
        println!("Appended to {}", dest.display());
    } else {
        fs::rename(timesheet, &dest).map_err(|e| e.to_string())?;
        println!("Rotated {} to {}", timesheet.display(), dest.display());
    }
    Ok(())
}

/// If the last log entry is from the previous week (before the most recent rotation boundary —
/// Sunday 00:00 unless `timesheet.yml` says otherwise), runs [`do_rotate`].
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
    let dir = timesheet
        .parent()
        .ok_or("timesheet migrate: no parent dir")?;
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
        let content = fs::read_to_string(path)
            .map_err(|e| format!("timesheet migrate: read {}: {}", path.display(), e))?;
        let mut out = String::new();
        for line in content.lines() {
            let new_line = match migrate_parse_line(line) {
                Some(LogLine::Start(dt, activity)) => {
                    format!("{}\n", format_start_log_entry(dt, &activity))
                }
                Some(LogLine::Stop(dt)) => format!("{}\n", format_stop_log_entry(dt)),
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
        fs::write(path, &out)
            .map_err(|e| format!("timesheet migrate: write {}: {}", path.display(), e))?;
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
/// - Negative integer (when enabled) → nth most recently rotated timesheet (`-1` is latest, `-2` is previous, etc.).
/// - Otherwise: match by extension in the timesheet directory (e.g. `260220`, `20260220`, `0220`, `2/20`).
///   Returns an error if zero or multiple files match.
fn resolve_list_input_impl(
    arg: Option<&str>,
    timesheet: &Path,
    allow_negative_rotated_index: bool,
) -> Result<PathBuf, String> {
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
    if allow_negative_rotated_index {
        if let Ok(index) = list_arg.parse::<i32>() {
            if index < 0 {
                let rotated_index = (-index - 1) as usize;
                if let Some(path) = nth_latest_rotated_timesheet(timesheet, rotated_index) {
                    return Ok(path);
                }
                return Err(format!(
                    "timesheet list: no timesheet matches \"{}\".",
                    list_arg
                ));
            }
        }
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
                if name.starts_with("timesheet.")
                    && name != "timesheet.log"
                    && name
                        .as_bytes()
                        .get(10)
                        .map(|&b| b.is_ascii_digit())
                        .unwrap_or(false)
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
            "timesheet list: multiple timesheets match \"{}\".",
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
        // use the most recent file whose extension date is on or before the requested date.
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
                        if ext_date <= want {
                            by_ext_date.push((path.clone(), ext_date));
                        }
                    }
                }
            }
        }
        if let Some((path, _)) = by_ext_date.into_iter().max_by_key(|(_, d)| *d) {
            return Ok(path);
        }
    }
    Err(format!(
        "timesheet list: no timesheet matches \"{}\".",
        list_arg
    ))
}

fn resolve_list_input(arg: Option<&str>, timesheet: &Path) -> Result<PathBuf, String> {
    resolve_list_input_impl(arg, timesheet, true)
}

fn resolve_tail_input(arg: Option<&str>, timesheet: &Path) -> Result<PathBuf, String> {
    resolve_list_input_impl(arg, timesheet, false)
}

fn rotated_timesheet_files(timesheet: &Path) -> Vec<PathBuf> {
    let Some(ts_dir) = timesheet.parent() else {
        return Vec::new();
    };
    let mut rotated = Vec::new();
    if let Ok(entries) = fs::read_dir(ts_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("timesheet.")
                    && name != "timesheet.log"
                    && name
                        .as_bytes()
                        .get(10)
                        .map(|&b| b.is_ascii_digit())
                        .unwrap_or(false)
                {
                    rotated.push(path);
                }
            }
        }
    }
    rotated
}

fn sorted_rotated_timesheet_files(timesheet: &Path) -> Vec<PathBuf> {
    let mut rotated = rotated_timesheet_files(timesheet);
    rotated.sort_by_key(|path| {
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_string()
    });
    rotated
}

fn nth_latest_rotated_timesheet(timesheet: &Path, index: usize) -> Option<PathBuf> {
    sorted_rotated_timesheet_files(timesheet)
        .into_iter()
        .rev()
        .nth(index)
}

fn latest_rotated_timesheet(timesheet: &Path) -> Option<PathBuf> {
    nth_latest_rotated_timesheet(timesheet, 0)
}

fn sprint_report_data(timesheet: &Path) -> Result<(ParsedLogLines, CurrentTask), String> {
    let latest_rotated = latest_rotated_timesheet(timesheet);
    let mut combined = Vec::new();
    let mut combined_index = 1usize;

    if let Some(path) = latest_rotated {
        for (_, line) in read_log_lines(&path)? {
            combined.push((combined_index, line));
            combined_index += 1;
        }
    }

    let current_task = if timesheet.exists() {
        let current_lines = read_log_lines(timesheet)?;
        let current_task = last_start_entry(&current_lines);
        for (_, line) in current_lines {
            combined.push((combined_index, line));
            combined_index += 1;
        }
        current_task
    } else {
        None
    };

    combined.sort_by_key(|(_, line)| log_line_dt(line));
    Ok((combined, current_task))
}

/// Records work start now; activity is optional. With no argument, shows the reminder chooser to pick/enter an activity (macOS via AppKit; Linux via PyQt single-click chooser, falling back to kdialog/zenity).
/// On other platforms or if the user declines, falls back to misc/unspecified.
/// Ensures the reminder daemon is running at entry (so it stays running even when timesheet start is run at system startup and
/// exits before the final start call), then restarts it after recording START to reset the timer.
fn cmd_start(args: &[String], timesheet: &Path) -> Result<(), String> {
    // Guard against shutdown/reload race: if auto-invoked (no args) and the last log
    // entry is a very recent STOP, skip — launchd is re-firing RunAtLoad during shutdown,
    // not a genuine login.
    if args.is_empty() {
        let startup_now = Local::now();
        let content = fs::read_to_string(timesheet).unwrap_or_default();
        if let Some(LogLine::Stop(dt)) = last_recorded_event(&content) {
            // Shutdown/reload guard: a very recent STOP means launchd/systemd is re-firing during
            // shutdown, not a genuine login -- skip.
            let age = startup_now.signed_duration_since(dt).num_seconds();
            if (0..60).contains(&age) {
                if env::var_os("TS_DEBUG").is_some() {
                    let _ = std::io::stderr().write_all(
                        b"timesheet: skipping start: last STOP was <60s ago (shutdown/reload guard)\n",
                    );
                }
                return Ok(());
            }
        }
        // Reconcile a missed shutdown STOP before any weekly rotation or recent-STOP guard could
        // hide the stale open session.
        reconcile_stale_open_session(timesheet, startup_now);
    }
    maybe_rotate_if_previous_week(timesheet)?;
    // Will we block on an interactive chooser below (no activity given and a GUI chooser is available)?
    #[cfg(not(test))]
    let will_prompt = args.is_empty() && start_chooser_available();
    #[cfg(test)]
    let will_prompt = false;
    if will_prompt {
        // The foreground chooser IS the reminder prompt. Stop any running daemon so it cannot pop a
        // SECOND prompt window after its interval elapses while the chooser is open. A fresh daemon
        // is (re)started after the chooser resolves and a START is recorded (below).
        kill_reminder_daemon_if_running();
    } else {
        // Start the daemon early so it is running even when timesheet start is invoked at login
        // (LaunchAgent / systemd) and exits quickly without prompting.
        start_reminder_daemon_if_needed(timesheet);
    }
    let activity = if args.is_empty() {
        #[cfg(not(test))]
        {
            match resolve_start_activity(timesheet) {
                Some(a) => a,
                None => {
                    // User chose "Stop Work" at the chooser: close the open session and stop reminders.
                    close_open_session(timesheet, Local::now());
                    kill_reminder_daemon_if_running();
                    return Ok(());
                }
            }
        }
        #[cfg(test)]
        "misc/unspecified".to_string()
    } else {
        args.join(" ")
    };
    let now = Local::now();
    // Close any open session before starting a new one, unless the START below already closes it.
    close_open_session_before_start(timesheet, now);
    append_log_entry(timesheet, &format_start_log_entry(now, &activity))?;
    println!(
        "Started: {} at {}",
        activity,
        Local::now().format("%a %b %d %H:%M:%S %Z %Y")
    );
    kill_reminder_daemon_if_running();
    thread::sleep(Duration::from_millis(100));
    start_reminder_daemon_if_needed(timesheet);
    Ok(())
}

/// Records work stop at the given time (or now if no time given). Same time formats as `timesheet started`
/// (see [`parse_start_time`]).
/// If the last entry is already STOP: no stop-time argument → no change; with stop-time → amend that entry.
fn cmd_stop(args: &[String], timesheet: &Path) -> Result<(), String> {
    maybe_rotate_if_previous_week(timesheet)?;
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    let last = content.lines().rev().find(|l| !l.trim().is_empty());
    if last
        .and_then(parse_line)
        .map(|ll| matches!(ll, LogLine::Stop(_)))
        .unwrap_or(false)
    {
        let Some(t) = args.first().map(String::as_str) else {
            // The log needs no change, but stopping is still stopping: silence the daemon anyway.
            // Otherwise a `timesheet stop` after an unanswered reminder (which records its own STOP)
            // would leave the daemon running to prompt again one interval later.
            let was_running = is_reminder_daemon_running();
            if was_running {
                show_reminders_stopped_notification();
            }
            kill_reminder_daemon_if_running();
            if was_running {
                println!("Work already stopped; reminders stopped.");
            }
            return Ok(());
        };
        let stop_dt = parse_start_time(t)
            .ok_or_else(|| format!("timesheet stop: could not parse stop time: {}", t))?;
        let lines: Vec<&str> = content.lines().collect();
        let without_last = if lines.is_empty() {
            String::new()
        } else {
            lines[..lines.len() - 1].join("\n") + "\n"
        };
        let new_content = format!("{}{}\n", without_last, format_stop_log_entry(stop_dt));
        fs::write(timesheet, &new_content).map_err(|e| e.to_string())?;
        if is_reminder_daemon_running() {
            show_reminders_stopped_notification();
        }
        kill_reminder_daemon_if_running();
        println!("Stopped at {}", stop_dt.format("%a %b %d %H:%M:%S %Z %Y"));
        return Ok(());
    }
    let stop_dt = match args.first().map(String::as_str) {
        Some(t) => parse_start_time(t)
            .ok_or_else(|| format!("timesheet stop: could not parse stop time: {}", t))?,
        None => Local::now(),
    };
    let line = format!("{}\n", format_stop_log_entry(stop_dt));
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(timesheet)
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    if is_reminder_daemon_running() {
        show_reminders_stopped_notification();
    }
    kill_reminder_daemon_if_running();
    println!("Stopped at {}", stop_dt.format("%a %b %d %H:%M:%S %Z %Y"));
    Ok(())
}

fn process_log_for_report(
    lines: &[(usize, LogLine)],
    virtual_stop: Option<DateTime<Local>>,
) -> (Vec<(String, f64, f64)>, Vec<f64>, bool) {
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

fn print_report(
    lines: &[(usize, LogLine)],
    virtual_stop: Option<DateTime<Local>>,
    current_task: CurrentTask,
    include_day_totals: bool,
) -> Result<(), String> {
    print!(
        "{}",
        render_report(lines, virtual_stop, current_task, include_day_totals)
    );
    Ok(())
}

fn render_report(
    lines: &[(usize, LogLine)],
    virtual_stop: Option<DateTime<Local>>,
    current_task: CurrentTask,
    include_day_totals: bool,
) -> String {
    let (by_act, dow_hr, work_in_progress) = process_log_for_report(lines, virtual_stop);
    if by_act.is_empty() {
        return "No work recorded.\n".to_string();
    }
    let mut out = String::new();
    for (act, pct, hr) in &by_act {
        let _ = writeln!(out, "{:.1}%  {:.2}h  {}", pct, hr, act);
    }
    if include_day_totals {
        for (i, name) in DAY_NAMES.iter().enumerate() {
            let _ = writeln!(
                out,
                "{}  {:.2}",
                name,
                dow_hr.get(i).copied().unwrap_or(0.0)
            );
        }
        let total_hr: f64 = dow_hr.iter().map(|&h| trunc2(h)).sum();
        let _ = writeln!(out, "Total  {:.2}", trunc2(total_hr));
    }
    if work_in_progress {
        if let Some((start_dt, activity)) = current_task {
            let now = Local::now();
            let dur_sec = (now - start_dt).num_seconds();
            let dur_min = dur_sec / 60;
            let dur_hr = dur_min / 60;
            let dur_rem = dur_min % 60;
            let duration_fmt = if dur_hr > 0 {
                format!("{}h {}m", dur_hr, dur_rem)
            } else {
                format!("{}m", dur_min)
            };
            let _ = writeln!(
                out,
                "\nCurrent Task: {}, started {}, worked {}",
                activity,
                start_dt.format("%a %b %d %H:%M:%S %Z %Y"),
                duration_fmt
            );
        }
    }
    out
}

/// Outputs the latest ten log entries with timestamps shown in local time. Optional arg selects file (same as list).
/// Consecutive START entries with the same activity are collapsed (first timestamp kept for aggregate duration); then the last 10 entries are shown.
fn cmd_tail(tail_arg: Option<&str>, timesheet: &Path) -> Result<(), String> {
    let path = resolve_tail_input(tail_arg, timesheet)?;
    if !path.exists() {
        println!("No timesheet data found.");
        return Ok(());
    }
    let content = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let entries: Vec<LogLine> = content.lines().filter_map(parse_line).collect();
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
        let end = last_ten
            .get(i + 1)
            .map(|n| match n {
                LogLine::Stop(e) => *e,
                LogLine::Start(e, _) => *e,
            })
            .unwrap_or(now);
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
                println!(
                    "START  {}  {:>width$}  {}",
                    dt.format("%Y-%m-%d %H:%M:%S"),
                    dur,
                    activity,
                    width = max_duration_width
                );
            }
            LogLine::Stop(dt) => {
                println!(
                    "STOP   {}  {:>width$}",
                    dt.format("%Y-%m-%d %H:%M:%S"),
                    dur,
                    width = max_duration_width
                );
            }
        }
    }
    Ok(())
}

/// Prints report: % per activity and hours per weekday; optional arg selects file (e.g. `log`, `0220`, `-1`, path).
/// Opens the timesheet log in the user's editor (`$EDITOR`, falling back to `$VISUAL`, then the
/// OS default: the program associated with `.txt` files on Windows, `vi` elsewhere).
fn cmd_edit(timesheet: &Path) -> Result<(), String> {
    if let Some(parent) = timesheet.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("timesheet edit: cannot create {}: {}", parent.display(), e))?;
    }
    let editor = env::var_os("EDITOR").or_else(|| env::var_os("VISUAL"));
    let (mut cmd, label) = match editor {
        Some(editor) => {
            let mut c = Command::new(&editor);
            c.arg(timesheet);
            (c, format!("{:?}", editor))
        }
        None if cfg!(windows) => {
            // No $EDITOR/$VISUAL: use whatever program Windows has associated with .txt files
            // (via `cmd /c start`) instead of hardcoding notepad, so this respects the user's
            // actual default text editor if they've changed it in Windows Settings > Default
            // Apps. The empty "" after /wait is the window-title argument `start` always expects
            // before the target path -- omitting it would make `start` treat the log path itself
            // as the title and fail to open it.
            let mut c = Command::new("cmd");
            c.args(["/c", "start", "/wait", ""]);
            c.arg(timesheet);
            // cmd.exe rejects a UNC current directory ("UNC paths are not supported") and warns
            // on stderr before falling back on its own -- which is exactly what timesheet.exe's
            // own cwd is whenever it's launched through WSL interop (e.g. from a WSL shell). The
            // log path argument above is already absolute, so this only silences a harmless but
            // alarming-looking warning; any real local directory works here.
            if let Some(parent) = timesheet.parent() {
                c.current_dir(parent);
            }
            (c, "the program associated with .txt files".to_string())
        }
        None => {
            let mut c = Command::new("vi");
            c.arg(timesheet);
            (c, "vi".to_string())
        }
    };
    let status = cmd
        .status()
        .map_err(|e| format!("timesheet edit: cannot run editor ({}): {}", label, e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "timesheet edit: editor ({}) exited with {}",
            label, status
        ))
    }
}

/// Parsed command line for `timesheet list`: at most one week selector plus the options.
struct ListArgs {
    input: Option<String>,
    /// `None` when no filtering was asked for; an empty `--prefix` reports every entry.
    prefix: Option<String>,
}

/// Parses `timesheet list`'s arguments. The selector may be a negative rotated-log index, so a
/// leading dash followed by digits is a positional rather than an option, and `--` forces
/// what follows to be positional.
fn parse_list_args(args: &[String]) -> Result<ListArgs, String> {
    let mut input: Option<String> = None;
    let mut prefix: Option<String> = None;
    let mut positional_only = false;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].clone();
        index += 1;
        let negative_index = arg.len() > 1 && arg[1..].chars().all(|c| c.is_ascii_digit());
        if positional_only || !arg.starts_with('-') || arg == "-" || negative_index {
            if input.is_some() {
                return Err(format!(
                    "timesheet list: unexpected extra argument \"{}\"; only one week may be selected",
                    arg
                ));
            }
            input = Some(arg);
            continue;
        }
        if arg == "--" {
            positional_only = true;
            continue;
        }

        let (name, inline) = match arg.strip_prefix("--").and_then(|r| r.split_once('=')) {
            Some((name, value)) => (format!("--{}", name), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        match name.as_str() {
            "-p" | "--prefix" => {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        let v = args
                            .get(index)
                            .cloned()
                            .ok_or_else(|| format!("timesheet list: {} needs a value", name))?;
                        index += 1;
                        v
                    }
                };
                prefix = Some(value);
            }
            other => return Err(format!("timesheet list: unknown option \"{}\"", other)),
        }
    }
    Ok(ListArgs { input, prefix })
}

/// Keeps only the sessions tagged with `prefix:`, stripping the tag from the description.
/// A START belonging to another job becomes a plain STOP, so it still closes the session
/// before it while contributing no hours of its own.
fn filter_lines_by_prefix(lines: &[(usize, LogLine)], prefix: &str) -> Vec<(usize, LogLine)> {
    lines
        .iter()
        .map(|(n, ll)| match ll {
            LogLine::Start(dt, activity) => match report::strip_prefix(activity, Some(prefix)) {
                Some(label) => (*n, LogLine::Start(*dt, label.to_string())),
                None => (*n, LogLine::Stop(*dt)),
            },
            LogLine::Stop(dt) => (*n, LogLine::Stop(*dt)),
        })
        .collect()
}

fn cmd_list(args: &[String], timesheet: &Path) -> Result<(), String> {
    if env::var_os("TS_DEBUG").is_some() {
        let _ = std::io::stderr().write_all(b"timesheet: cmd_list entered\n");
    }
    let parsed = parse_list_args(args)?;
    let list_arg = parsed.input.as_deref();
    let list_input = resolve_list_input(list_arg, timesheet)?;
    if !list_input.exists() {
        println!("No timesheet data found.");
        return Ok(());
    }
    let lines = read_log_lines(&list_input)?;
    // An empty `--prefix` asks for every entry, so it filters nothing.
    let lines = match parsed.prefix.as_deref() {
        Some(prefix) if !prefix.is_empty() => filter_lines_by_prefix(&lines, prefix),
        _ => lines,
    };
    let is_current = list_arg.is_none() || list_arg == Some("log");
    let current_task = if is_current {
        last_start_entry(&lines)
    } else {
        None
    };
    let virtual_stop = if is_current && current_task.is_some() {
        Some(Local::now())
    } else {
        None
    };
    print_report(&lines, virtual_stop, current_task, true)
}

fn cmd_sprint(timesheet: &Path) -> Result<(), String> {
    let latest_rotated = latest_rotated_timesheet(timesheet);
    if !timesheet.exists() && latest_rotated.is_none() {
        println!("No timesheet data found.");
        return Ok(());
    }
    let (lines, current_task) = sprint_report_data(timesheet)?;
    let virtual_stop = current_task.as_ref().map(|_| Local::now());
    print_report(&lines, virtual_stop, current_task, false)
}

/// Parses a date: `YYYY-MM-DD`, `MM/DD/YYYY`, or `MM/DD` (the year of `now`).
fn parse_date_part(s: &str, now: DateTime<Local>) -> Option<NaiveDate> {
    let s = s.trim();
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(s, "%m/%d/%Y"))
        .or_else(|_| NaiveDate::parse_from_str(&format!("{}/{}", s, now.year()), "%m/%d/%Y"))
        .ok()
}

/// Parses a start-time string into a DateTime<Local>. Tries strict ISO 8601 first, then an
/// optional leading date (`YYYY-MM-DD`, `MM/DD/YYYY`, `MM/DD`) followed by a clock time
/// ([`parse_time_of_day`]). A bare time means today; a bare date means midnight that day.
fn parse_start_time(s: &str) -> Option<DateTime<Local>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(dt) = parse_timestamp_field(s) {
        return Some(dt);
    }
    let now = Local::now();
    // "2025-02-16 9am" splits into date and time; "9:00 AM" has no date part and is today.
    let (date, time_str) = match s.split_once(char::is_whitespace) {
        Some((head, rest)) => match parse_date_part(head, now) {
            Some(d) => (d, rest.trim()),
            None => (now.date_naive(), s),
        },
        None => (now.date_naive(), s),
    };
    if let Some(t) = parse_time_of_day(time_str) {
        return Some(local_datetime_at(date, t));
    }
    // A date with no time at all: the start of that day.
    if let Some(d) = parse_date_part(s, now) {
        return Some(local_datetime_at(d, NaiveTime::MIN));
    }
    None
}

/// Records a past start time; inserts the new entry at the correct chronological position
/// without discarding any existing entries.
fn cmd_started(args: &[String], timesheet: &Path) -> Result<(), String> {
    let (start_time, activity) = match args.split_first() {
        Some((st, rest)) => (st.as_str(), rest.join(" ")),
        None => {
            eprintln!("Usage: timesheet started <start_time> [activity...]");
            eprintln!(
                "  start_time is required (e.g. \"2026-08-06 09:00\", 09:00, 9am, \"9:30 AM\")."
            );
            return Err("missing start_time".to_string());
        }
    };
    let activity = if activity.is_empty() {
        "misc/unspecified".to_string()
    } else {
        activity
    };
    let start_dt = parse_start_time(start_time).ok_or_else(|| {
        format!(
            "timesheet started: could not parse start time: {}",
            start_time
        )
    })?;
    maybe_rotate_if_previous_week(timesheet)?;
    let content = fs::read_to_string(timesheet).unwrap_or_default();
    let new_entry = format_start_log_entry(start_dt, &activity);

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
    println!(
        "Started: {} at {}",
        activity,
        start_dt.format("%a %b %d %H:%M:%S %Z %Y")
    );
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
        last.and_then(parse_line)
            .map(|ll| matches!(ll, LogLine::Stop(_)))
            .unwrap_or(true) // empty or last is STOP -> need START
    } else {
        true
    };
    if needs_start {
        if let Some(parent) = timesheet.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let now = Local::now();
        let line = format!("{}\n", format_start_log_entry(now, "misc/unspecified"));
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(timesheet)
            .map_err(|e| e.to_string())?;
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

/// Interactively replace activity text in this week's START entries.
/// Searches literally first; if nothing matches and the search text is a valid regex, falls back to regex replacement.
/// Prompts Replace (y/n/a) per match.
/// Used by both `alias` and `rename` subcommands.
struct WorkaliasMatch {
    line_num: usize,
    dt: DateTime<Local>,
    replacement: String,
}

fn should_replace_workalias_match(input: &str, replace_all: &mut bool) -> bool {
    if *replace_all {
        return true;
    }

    match input.trim().to_ascii_lowercase().as_str() {
        "y" => true,
        "a" => {
            *replace_all = true;
            true
        }
        _ => false,
    }
}

fn collect_workalias_matches_with<F>(
    content: &str,
    week_start_dt: DateTime<Local>,
    week_end: DateTime<Local>,
    replacer: F,
) -> Vec<WorkaliasMatch>
where
    F: Fn(&str) -> Option<String>,
{
    let mut matches_vec = Vec::new();

    for (i, line) in content.lines().enumerate() {
        if let Some(LogLine::Start(dt, activity)) = parse_line(line) {
            if dt >= week_start_dt && dt <= week_end {
                if let Some(updated_activity) = replacer(&activity) {
                    matches_vec.push(WorkaliasMatch {
                        line_num: i + 1,
                        dt,
                        replacement: updated_activity,
                    });
                }
            }
        }
    }

    matches_vec
}

fn collect_workalias_matches(
    content: &str,
    week_start_dt: DateTime<Local>,
    week_end: DateTime<Local>,
    search_text: &str,
    replacement: &str,
) -> Vec<WorkaliasMatch> {
    let literal_matches =
        collect_workalias_matches_with(content, week_start_dt, week_end, |activity| {
            activity
                .contains(search_text)
                .then(|| activity.replace(search_text, replacement))
        });
    if !literal_matches.is_empty() {
        return literal_matches;
    }

    let Ok(re) = Regex::new(search_text) else {
        return Vec::new();
    };

    collect_workalias_matches_with(content, week_start_dt, week_end, |activity| {
        re.is_match(activity)
            .then(|| re.replace_all(activity, replacement).into_owned())
    })
}

fn cmd_workalias(args: &[String], timesheet: &Path) -> Result<(), String> {
    let (search_text, replacement) = match args {
        [p, r, ..] => (p.as_str(), r.to_string()),
        _ => {
            eprintln!("Usage: timesheet alias <pattern> <replacement>");
            eprintln!("       timesheet rename <pattern> <replacement>");
            return Err("missing args".to_string());
        }
    };
    if !timesheet.exists() {
        return Err("timesheet alias: no timesheet data found.".to_string());
    }
    let now = Local::now();
    let week_start_dt = week_start(now);
    let week_end = week_start_dt + chrono::Duration::weeks(1) - chrono::Duration::seconds(1);
    let content = fs::read_to_string(timesheet).map_err(|e| e.to_string())?;
    let matches_vec =
        collect_workalias_matches(&content, week_start_dt, week_end, search_text, &replacement);
    if matches_vec.is_empty() {
        return Err(format!(
            "timesheet alias: no activities matching \"{}\" found for this week.",
            search_text
        ));
    }
    let lines_vec: Vec<&str> = content.lines().collect();
    let mut replace_lines: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut replace_all = false;
    for workalias_match in &matches_vec {
        let line_num = workalias_match.line_num;
        let dt = workalias_match.dt;
        let new_repl = &workalias_match.replacement;
        let orig_activity = lines_vec
            .get(line_num - 1)
            .and_then(|l| parse_line(l))
            .and_then(|ll| match ll {
                LogLine::Start(_, a) => Some(a),
                _ => None,
            })
            .unwrap_or_default();
        let end_dt = lines_vec
            .get(line_num)
            .and_then(|l| parse_line(l))
            .map(|ll| match ll {
                LogLine::Start(e, _) | LogLine::Stop(e) => e,
            })
            .unwrap_or(now);
        let secs = (end_dt - dt).num_seconds();
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
        if replace_all {
            replace_lines.insert(line_num, new_repl.clone());
            continue;
        }
        print!("Replace (y/n/a) ");
        stdout.flush().map_err(|e| e.to_string())?;
        let mut buf = String::new();
        if stdin.lock().read_line(&mut buf).is_ok()
            && should_replace_workalias_match(&buf, &mut replace_all)
        {
            replace_lines.insert(line_num, new_repl.clone());
        }
    }
    if replace_lines.is_empty() {
        return Ok(());
    }
    let mut out = String::new();
    for (i, line) in content.lines().enumerate() {
        let line_no = i + 1;
        if let Some(new_activity) = replace_lines.get(&line_no) {
            if let Some(LogLine::Start(dt, activity)) = parse_line(line) {
                if dt >= week_start_dt && dt <= week_end {
                    let _ = activity;
                    out.push_str(&format!("{}\n", format_start_log_entry(dt, new_activity)));
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

/// Prepends "<prefix>:" to activities matching a pattern in this week's START entries.
/// `timesheet prefix foo bar` is equivalent to `timesheet alias bar foo:bar`.
fn cmd_prefix(args: &[String], timesheet: &Path) -> Result<(), String> {
    let (prefix, pattern) = match args {
        [p, t, ..] => (p.as_str(), t.as_str()),
        _ => {
            eprintln!("Usage: timesheet prefix <prefix> <pattern>");
            return Err("missing args".to_string());
        }
    };
    cmd_workalias(
        &[pattern.to_string(), format!("{}:{}", prefix, pattern)],
        timesheet,
    )
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
        create_and_verify_writable(&PathBuf::from(d))?
    } else if cfg!(windows) {
        // Windows default install location. The reminder chooser is a real full-screen window now
        // (WinForms, like the AppKit/PyQt choosers on the other two platforms), not console-only
        // output, so it belongs in the per-user "Programs" location Windows documents for
        // no-admin-required app installs, rather than a PATH-searched bin-style directory.
        let local_app_data =
            env::var_os("LOCALAPPDATA").ok_or("timesheet install: %LOCALAPPDATA% is not set")?;
        create_and_verify_writable(
            &PathBuf::from(local_app_data)
                .join("Programs")
                .join("timesheet"),
        )?
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
        found.ok_or(
            "timesheet install: no writable directory on PATH. Specify an installation directory.",
        )?
    };
    // Look for a prebuilt binary under script_dir first (its filename must match this platform's
    // convention -- "timesheet.exe" on Windows, "timesheet" elsewhere -- checked via cfg!(windows)
    // rather than assumed, since a Linux/macOS build of the same repo can leave a same-named
    // extensionless binary sitting right next to the Windows one in a shared target dir, e.g. when
    // accessed through WSL interop), falling back to the currently running executable.
    let candidate = script_dir.join(if cfg!(windows) {
        "timesheet.exe"
    } else {
        "timesheet"
    });
    let src_to_use = if candidate.exists() { &candidate } else { &exe };
    if !src_to_use.exists() {
        return Err(format!(
            "timesheet install: missing {}",
            src_to_use.display()
        ));
    }
    // Installed as "timesheet", not "ts", on every platform: "ts" is a name several unrelated
    // tools already claim -- e.g. BusyBox's "ts" applet and moreutils' timestamp-prefixing "ts"
    // -- so a bare "ts" ahead of this binary on PATH (a Scoop shim, for instance) would silently
    // run the wrong program instead.
    let dest_file = dest.join(if cfg!(windows) {
        "timesheet.exe"
    } else {
        "timesheet"
    });
    if !paths_refer_to_same_file(src_to_use, &dest_file) {
        fs::copy(src_to_use, &dest_file)
            .map_err(|e| format!("timesheet install: copy failed: {}", e))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest_file)
            .map_err(|e| e.to_string())?
            .permissions();
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
    #[cfg(target_os = "windows")]
    {
        match install_windows_start_menu_shortcut(&dest_file) {
            Ok(path) => println!("Installed Start Menu shortcut {}", path.display()),
            Err(e) => eprintln!("timesheet install: warning: {}", e),
        }
    }
    println!("Installed {}", dest_file.display());
    println!("Done. timesheet is in {} and executable.", dest.display());
    Ok(())
}

/// Create (or overwrite) a Windows shortcut (.lnk) via PowerShell's WScript.Shell COM object, same
/// approach as the OS-tool shell-outs used for the macOS install steps above. Shared by the Start
/// Menu shortcut (`timesheet install`) and the Startup-folder autostart shortcut
/// (`timesheet autostart`).
#[cfg(target_os = "windows")]
fn create_windows_shortcut(
    link_path: &Path,
    target: &Path,
    args: &str,
    workdir: &Path,
    description: &str,
) -> Result<(), String> {
    if let Some(parent) = link_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
    }
    let ps_quote = |s: &str| s.replace('\'', "''");
    let link = ps_quote(&link_path.to_string_lossy());
    let target_s = ps_quote(&target.to_string_lossy());
    let args_s = ps_quote(args);
    let workdir_s = ps_quote(&workdir.to_string_lossy());
    let description_s = ps_quote(description);
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{link}'); \
         $s.TargetPath = '{target_s}'; \
         $s.Arguments = '{args_s}'; \
         $s.WorkingDirectory = '{workdir_s}'; \
         $s.IconLocation = '{target_s}'; \
         $s.Description = '{description_s}'; \
         $s.Save()"
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .map_err(|e| format!("failed to run powershell: {}", e))?;
    if !status.success() {
        return Err(format!(
            "powershell exited with {} while creating shortcut {}",
            status,
            link_path.display()
        ));
    }
    Ok(())
}

/// Create (or overwrite) a per-user Start Menu shortcut that runs `timesheet.exe start`, so
/// starting work is a point-and-click action.
#[cfg(target_os = "windows")]
fn install_windows_start_menu_shortcut(dest_file: &Path) -> Result<PathBuf, String> {
    let start_menu = dirs::config_dir()
        .ok_or("could not determine %APPDATA%")?
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs");
    let shortcut = start_menu.join("Start Timesheet.lnk");
    create_windows_shortcut(
        &shortcut,
        dest_file,
        "start",
        dest_file.parent().unwrap_or(Path::new(".")),
        "Record work start (same as running \"timesheet start\")",
    )?;
    Ok(shortcut)
}

/// Remove startup/shutdown/login/logout hooks that reference timesheet. No-op on unsupported platforms.
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

/// Stop reminder daemon, remove autostart hooks, optionally remove log files, then remove ts-icon.svg and the timesheet binary.
fn cmd_uninstall(args: &[String]) -> Result<(), String> {
    let _ = args;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let install_dir = exe
        .parent()
        .ok_or("timesheet uninstall: could not determine install directory")?;

    println!("Uninstalling timesheet from {} ...", install_dir.display());

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
                log_files
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
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
        fs::remove_file(&icon_path)
            .map_err(|e| format!("timesheet uninstall: could not remove icon: {}", e))?;
        println!("Removed {}", icon_path.display());
    }

    #[cfg(target_os = "windows")]
    if let Some(shortcut) = dirs::config_dir().map(|d| {
        d.join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Start Timesheet.lnk")
    }) {
        if shortcut.exists() {
            let _ = fs::remove_file(&shortcut);
            println!("Removed {}", shortcut.display());
        }
    }

    fs::remove_file(&exe)
        .map_err(|e| format!("timesheet uninstall: could not remove binary: {}", e))?;
    println!("Removed {}", exe.display());
    println!("Uninstall complete.");
    Ok(())
}

fn is_writable(p: &Path) -> bool {
    fs::metadata(p)
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false)
}

/// Create `p` if it doesn't exist yet, then verify it's a writable directory. Shared by an
/// explicit `timesheet install <dir>` argument and the Windows default install location.
fn create_and_verify_writable(p: &Path) -> Result<PathBuf, String> {
    if !p.exists() {
        fs::create_dir_all(p).map_err(|e| {
            format!(
                "timesheet install: cannot create directory {}: {}",
                p.display(),
                e
            )
        })?;
    }
    if !p.is_dir() || !is_writable(p) {
        return Err(format!(
            "timesheet install: directory is not writable: {}",
            p.display()
        ));
    }
    Ok(p.to_path_buf())
}

fn paths_refer_to_same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Rebuild from a local directory or clone: run `cargo build --release` then install to current binary's dir.
/// If arg is a directory with Cargo.toml, build there. If arg is missing and current dir has Cargo.toml, build there.
/// If arg is missing and current dir has no Cargo.toml, clone the timesheet repo and build from the clone.
fn cmd_rebuild(args: &[String]) -> Result<(), String> {
    let install_dir = env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("timesheet rebuild: could not determine install directory")?
        .to_path_buf();

    let build_dir_arg = args.first().map(String::as_str).unwrap_or(".");
    let build_dir = if build_dir_arg == "." {
        env::current_dir().map_err(|e| format!("timesheet rebuild: {}", e))?
    } else {
        let p = PathBuf::from(build_dir_arg);
        if !p.exists() {
            return Err(format!(
                "timesheet rebuild: no such directory: {}",
                p.display()
            ));
        }
        if !p.is_dir() {
            return Err(format!(
                "timesheet rebuild: not a directory: {}",
                p.display()
            ));
        }
        p.canonicalize()
            .map_err(|e| format!("timesheet rebuild: {}: {}", p.display(), e))?
    };

    let cargo_toml = build_dir.join("Cargo.toml");
    let build_dir = if cargo_toml.exists() {
        build_dir
    } else if args.is_empty() {
        // No arg and no Cargo.toml in current dir: clone repo
        let clone_parent = env::temp_dir().join(format!("timesheet-rebuild-{}", process::id()));
        if clone_parent.exists() {
            fs::remove_dir_all(&clone_parent).map_err(|e| e.to_string())?;
        }
        fs::create_dir_all(&clone_parent).map_err(|e| e.to_string())?;
        let status = Command::new("git")
            .args(["clone", "https://github.com/pillarsdotnet/timesheet"])
            .current_dir(&clone_parent)
            .status()
            .map_err(|e| format!("timesheet rebuild: git clone failed: {}", e))?;
        if !status.success() {
            return Err("timesheet rebuild: git clone failed.".to_string());
        }
        clone_parent.join("timesheet")
    } else {
        return Err(format!(
            "timesheet rebuild: no Cargo.toml in {}",
            build_dir.display()
        ));
    };

    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&build_dir)
        .status()
        .map_err(|e| format!("timesheet rebuild: cargo build failed: {}", e))?;
    if !status.success() {
        return Err("timesheet rebuild: cargo build failed.".to_string());
    }

    #[cfg(not(windows))]
    let exe = build_dir.join("target/release/timesheet");
    #[cfg(windows)]
    let exe = build_dir.join("target/release/timesheet.exe");
    if !exe.exists() {
        return Err(format!(
            "timesheet rebuild: binary not found after build: {}",
            exe.display()
        ));
    }

    let status = Command::new(&exe)
        .arg("install")
        .arg(&install_dir)
        .status()
        .map_err(|e| format!("timesheet rebuild: install failed: {}", e))?;
    if !status.success() {
        return Err("timesheet rebuild: install failed.".to_string());
    }

    println!("Rebuilt and installed to {}", install_dir.display());
    Ok(())
}

/// Groff man page source (shared by manpage and help).
fn manpage_content() -> &'static str {
    r#".TH TIMESHEET 1 "February 2025" "" "timesheet"
.SH NAME
timesheet \- track work time and report by activity and weekday (start, stop, list, ...)
.SH SYNOPSIS
.B timesheet
.I command
.RI [ args... ]
.PP
.B timesheet alias
.I pattern
.I replacement
.PP
.B timesheet autostart
.RI [ uninstall ]
.PP
.B timesheet email
.RI [ options "] [" week ]
.PP
.B timesheet help
.PP
.B timesheet install
.RI [ install_dir " [" repo_path ]]
.PP
.B timesheet uninstall
.PP
.B timesheet interval
.RI [ duration ]
.PP
.B timesheet list
.RI [ options "] [" file_or_extension ]
.PP
.B timesheet sprint
.PP
.B timesheet tail
.RI [ file_or_extension ]
.PP
.B timesheet manpage
.PP
.B timesheet pdf
.RI [ options "] [" week ]
.PP
.B timesheet prefix
.I prefix
.I pattern
.PP
.B timesheet rebuild
.RI [ directory ]
.PP
.B timesheet rename
.I pattern
.I replacement
.PP
.B timesheet reminder
.RI [ duration ]
.PP
.B timesheet restart
.RI [ duration ]
.PP
.B timesheet rotate
.PP
.B timesheet start
.RI [ activity ]
.PP
.B timesheet started
.I start_time
.RI [ activity... ]
.PP
.B timesheet stop
.RI [ stop_time ]
.PP
.B timesheet stopped
.RI [ stop_time ]
.PP
.B timesheet timeoff
.SH DESCRIPTION
.B timesheet
tracks work start/stop and reports time by activity and by day of week.
The log file is
.BR $HOME /Documents/timesheet.log
by default (compile-time constant
.BR DEFAULT_TIMESHEET
in source).
.SH CONFIGURATION
Optional settings are read from
.BR $HOME/.config/timesheet.yml
(see
.BR FILES ).
The file is missing by default and every setting except the
.B pdf
and
.B email
template has a default, so no configuration is required to track time.
Only a small YAML subset is understood:
.BR "key: value" " pairs, " #
comments, optional quotes, indented nesting, and sequences (either
.B "- item"
lines or
.BR "[a, b]" ).
Unknown keys are ignored; an unusable value prints a warning on stderr and the default is
used. Quote a value whose leading or trailing spaces matter, such as
.BR "separator: \(dq; \(dq" .
.PP
.B "rotate"
selects when a new timesheet week begins \(em the boundary at which
.B timesheet
automatically rotates the log (see
.BR "AUTOMATIC ROTATION" ).
It takes a mapping with
.B day
(weekday name or three-letter abbreviation, any case) and
.B time
(HH:MM, HH:MM:SS, a bare hour, or a 12-hour time with a meridiem such as 5pm), or a scalar shorthand
.RB ( "rotate: monday" ", " "rotate: \(dqfri 17:00\(dq" ).
Defaults:
.B day
Sunday,
.B time
00:00 (local time).
.PP
Rotating at midnight between Sunday night and Monday morning \(em i.e.\& a work week
that runs Monday through Sunday:
.PP
.RS
.nf
# ~/.config/timesheet.yml
rotate:
  day: monday
  time: "00:00"
.fi
.RE
.PP
The remaining settings supply defaults for
.B pdf
and
.BR email .
Each may be written at the top level or under
.BR "prefixes: " \(-> " " PREFIX ,
in which case it applies only when that prefix is in use; a per-prefix value wins over the
top-level one, and a command-line option wins over both. This is what lets a single log
serve several jobs.
.TP
.B name
Full name, as it should appear on the timesheet. Required by
.B pdf
and
.BR email .
.TP
.B prefix
Default for
.BR \-\-prefix .
When absent and exactly one prefix is listed under
.BR prefixes ,
that one is used.
.TP
.B template
Default for
.BR \-\-template :
the path to the form-fillable PDF. There is no built-in default.
.TP
.B output
Default for
.BR \-\-output .
When absent,
.B pdf
writes to standard output.
.TP
.BR activity ", " separator ", " zero
Defaults for
.BR \-\-activity ", " \-\-separator " and " \-\-zero .
.TP
.BR to ", " cc
Default recipients, each either one address or a sequence of them.
.TP
.BR from ", " reply
Default sender and Reply-To addresses.
.TP
.BR subject ", " body
Templates for the message, taking the same placeholders as
.BR \-\-output ,
plus
.BR {total_hours} .
.TP
.BR min_font_size ", " max_font_size
Shrink-to-fit range in points (default 5 and 10). Long descriptions step down from the
maximum toward the minimum.
.TP
.B fields
Maps each timesheet slot to a form-field name in the template. The slots are
.BR contractor_name ,
.BR week_start_month / day / year ,
.BR week_end_month / day / year ,
.IR weekday _hours
and
.IR weekday _activities
for each of the seven weekdays, and
.BR total_hours .
Values default to the field names of the stock form, and any listed here replace only the
slots they name. The field names of another form can be listed with
.BR "mutool show form.pdf form | grep Name:" .
.TP
.BR smtp_host ", " smtp_port ", " smtp_starttls
Relay to submit through (default
.BR localhost :25).
.B smtp_starttls
defaults to true on port 587 and false elsewhere.
.TP
.BR smtp_user ", " smtp_password_command
Credentials for a relay that requires them. Leave
.B smtp_user
unset for an unauthenticated relay; otherwise set
.B smtp_password_command
to a shell command that prints the password, so that no secret is stored in the config
file, e.g.\&
.BR "pass show smtp/me@example.com" .
A Gmail or Workspace account wants
.BR smtp.gmail.com :587
with STARTTLS, the full address as
.BR smtp_user ,
and an App Password \(em an account password is rejected.
.PP
A configuration for one job, tagged
.B ST
in the activity descriptions:
.PP
.RS
.nf
# ~/.config/timesheet.yml
name: "Jane Contractor"
from: "jane@example.com"
smtp_host: "smtp.gmail.com"
smtp_port: 587
smtp_user: "jane@example.com"
smtp_password_command: "pass show smtp/jane"
prefixes:
  ST:
    template: "~/Documents/timesheet-fillable.pdf"
    output: "timesheet_Jane_{week_start}-{week_end}.pdf"
    separator: "; "
    zero: ""
    reply: "jane@employer.example"
    to: "timesheets@employer.example"
.fi
.RE
.SH "AUTOMATIC ROTATION"
.B start,
.B stop,
.B started,
.B timeoff
and the reminder daemon first check whether the log's last entry predates the most recent
rotation boundary (by default Sunday 00:00; see
.BR CONFIGURATION ).
If so, they run
.B rotate
before recording anything, so each
.B timesheet.YYMMDD
file holds exactly one work week. The
.B rotate
command itself always rotates, regardless of the boundary. The boundary also defines
"this week" for
.BR alias .
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
.SH "TIME FORMATS"
The
.I start_time
of
.B timesheet started
and the
.I stop_time
of
.B timesheet stop
accept the same forms. A time with no date means today; a date with no time means midnight
that day. Quote any argument containing a space.
.TP
.B ISO 8601
2026\-08\-06T07:00:00\-04:00
.TP
.B "date and time"
\(dq2026\-08\-06 07:00\(dq, \(dq08/06/2026 7:00 PM\(dq, \(dq8/6 7am\(dq
.TP
.B "24-hour time"
07:00, 07:00:30, 7
.TP
.B "12-hour time"
7am, \(dq7 AM\(dq, 7pm, 7:30pm, \(dq12:15:30 p.m.\(dq
.TP
.B "date only"
2026\-08\-06, 08/06/2026, 8/6
.PP
A bare hour is 24-hour
.RB ( 19
is 7 pm), so a bare
.B 12
is noon while
.B 12am
is midnight. The meridiem is case-insensitive and may be written am/pm, a.m./p.m., or a/p.
MM/DD without a year means the current year.
.SH COMMANDS
.TP
.B alias
Interactively replace activity text in START entries from the current week.
.I pattern
is matched literally first;
.I pattern
is treated as a regex only if no literal matches are found and it compiles as a valid regex;
.I replacement
is the replacement string.
For each match, prompts
.B Replace\ (y/n/a);
.B y
or
.B Y
applies the replacement. Errors if no matches this week.
.B a
or
.B A
applies the current replacement and all remaining matches without prompting again.
.TP
.B autostart
[\fIinterval\fR]
Register
.B "timesheet start"
to run at login and
.B "timesheet stop"
to run at logout or system shutdown. Optional
.I interval
(e.g.\ \&5s, 3m) sets the reminder interval and starts the daemon in this session; if the daemon is already running, it is restarted so the new interval takes effect immediately.
On macOS installs two LaunchAgents and a logout hook:
.RS
.TP
\fBcom.ts.autostart.start\fR
Runs
.B "timesheet start"
at login (RunAtLoad, limited to Aqua sessions).
A shutdown guard skips the start if the last log entry is a STOP less than 60\ s old.
If the last recorded event is not STOP and is more than 5 minutes old, startup backfills a STOP one reminder interval after that event before recording the new START.
.TP
\fBcom.ts.autostart.session\fR
Runs
.B "timesheet \-\-session\-daemon"
as a persistent launchd job; on logout/shutdown launchd sends it SIGTERM and waits up to 30 s (ExitTimeOut) for it to write the STOP entry and exit.
.TP
\fBLogoutHook\fR
Runs as root before logout/shutdown and macOS blocks the shutdown sequence until it returns, providing a second guarantee that STOP is recorded. Uses
.B "launchctl asuser"
to invoke
.B "timesheet stop"
in the console user's launchd context. Requires sudo to register; if it cannot be set the command to run manually is printed.
.RE
On Linux uses systemd user services, plus a system-level logout hook
.RB ( ts-logout- uid .service)
whose ExecStop runs "timesheet stop" before shutdown.target as a second guarantee that STOP is recorded on a
full shutdown/reboot. Installing the system unit needs administrator access, so the
.B sudo
command is printed and offered to run; if declined, run it yourself. Once present, later runs skip it.
With
.I uninstall
removes the registration (the logout hook removal also needs
.BR sudo ).
Without
.I interval
: starts the daemon if not running and prints the current reminder interval.
On Windows, registers a per-user Startup-folder shortcut ("Timesheet Autostart") that runs
.B "timesheet start"
at login; no admin rights are needed for this, but unlike macOS/Linux there is no
no-admin-required second guarantee for logoff/shutdown available on Windows, so STOP there relies
solely on the reminder daemon's console control handler (best-effort: Windows does not guarantee
it waits for the handler to finish). With
.I uninstall
removes the Startup-folder shortcut.
.TP
.B help
Run the equivalent of
.B "timesheet manpage | groff \-man \-Tascii | less"
to show this manual page in the system pager. On Windows, where groff and less are not
available, renders this page as plain text and pages it with
.BR more .
.TP
.B install
Copy the binary (and on macOS the embedded icon as
.BR ts-icon.svg )
to a directory on
.BR PATH .
If
.I install_dir
is given, installs there (directory created if needed). Otherwise, on Windows,
installs to
.B "%LOCALAPPDATA%\\Programs\\timesheet"
(created if needed) \(em the per-user, no-admin-required location Windows documents for app
installs, matching the fact that the reminder chooser is a real window (WinForms) rather than
console-only output. On other platforms, if
.I install_dir
is omitted, uses the first writable directory on
.BR PATH .
Optional
.I repo_path
is the directory containing the binary (default: current executable's directory). On macOS the icon is embedded so
.B ts-icon.svg
is always written even without the source repository. The binary is installed as
.B timesheet
(
.B timesheet.exe
on Windows), not the shorter
.BR ts ,
since
.B ts
is a name several unrelated tools already claim (e.g. BusyBox's
.B ts
applet and moreutils'
.B ts
timestamp filter) \(em a shadowed
.B ts
ahead of this binary on
.B PATH
would silently run the wrong program. On Windows, also creates (or overwrites) a per-user Start
Menu shortcut, "Start Timesheet", that runs
.B "timesheet.exe start"
so starting work is a point-and-click action.
.TP
.B uninstall
Stop the reminder daemon, remove startup/shutdown/login/logout hooks (LaunchAgents and LogoutHook on macOS, systemd user units and the system-level logout hook on Linux), prompt to remove timesheet log files (y/N), then remove
.BR ts-icon.svg ,
the Start Menu shortcut (Windows), and the
.B timesheet
binary (
.B timesheet.exe
on Windows) from the directory containing the running executable.
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
Reminder daemon behavior: if a prompt goes unanswered for one reminder interval, records a STOP timestamped at the moment the prompt appeared, not when the interval expired. That timestamp is used exactly, without the one-interval cap, because the prompt appears one reminder interval after the previous entry and so already marks the last time you were known to be working. The prompt is then left on screen rather than dismissed (macOS also brings it back to the front of the window stack): choosing an activity when you return records a START at the return time, so the stretch away from the desk falls between the two entries and goes unbilled while your return is logged accurately. No second STOP is added while work is already stopped, so an unattended screen records one STOP rather than one per interval. The reminder window covers the full screen and stays on top on both macOS and Linux, so it cannot be hidden by accident by a mouse action in progress when it appears. Dismissed without choice (close, Escape) re-shows immediately. The "Enter new activity" dialog has no timeout; blank/cancelled re-shows the reminder. At logout/shutdown the open session is stopped: on macOS the daemon itself records STOP when launchd sends it SIGTERM (capped to one reminder interval after the latest entry); on Linux the systemd session unit's ExecStop runs "timesheet stop" instead, and the daemon stays silent on SIGTERM (systemd may signal it during ordinary teardown, so writing a STOP there would be spurious). Every other automatic STOP is capped to one reminder interval (default 5 minutes) after the latest entry, so forgetting to stop never records work all night.
.TP
.B list
Plaintext report: percentage of time per activity (high to low), and hours per day of week (Sun\-Sat).
If work is in progress (last entry is START), uses a virtual STOP at current time for the report
and shows current task, start time, and duration.
Optional
.I file_or_extension
selects an alternate log path or extension filter.
If it is a negative integer,
.B -1
selects the most recently rotated
.BR timesheet.YYMMDD ,
.B -2
the one before that, and so on.
Options:
.RS
.TP
.BR \-p ", " \-\-prefix " " \fIPREFIX\fR
Report only activities beginning
.IR PREFIX :
(the prefix followed by a colon), and strip that tag from the reported description, so an
entry logged as
.B ST:Setup Jira
is reported as
.BR "Setup Jira" ,
exactly as
.B pdf
reports it. Entries without the tag belong to another job and are excluded entirely, their
hours as well as their descriptions. Unlike
.BR pdf ", " list
consults no configuration for this: without the option every activity is reported, as is an
empty
.IR PREFIX .
.RE
.TP
.B edit
Open the timesheet log
.RB ( $HOME/Documents/timesheet.log )
in your editor, taken from
.B $EDITOR
(then
.BR $VISUAL ,
else
.B vi
on other platforms; on Windows, the program associated with
.B .txt
files, same as double-clicking the log in Explorer).
.TP
.BI "pdf " "[options] [week]"
Fill a form-fillable PDF template with one week of the timesheet and write it out.
.IP
The optional
.I week
argument selects which week to report, taking the same forms as
.BR list :
a log file path,
.B log
for the current log, a negative rotated-log index
.RB ( -1
is the most recently rotated), or a date
.RB ( YYYYMMDD ", " YYMMDD ", " M/D )
falling in the wanted week. With no argument, the week in progress is reported on its final
day and the most recently completed week on any other day \(em so a run late on the last day
of the week, or at any time in the days after it, both report the week just worked.
.IP
Hours are credited to the day each session started on, matching
.BR list ,
and the printed total is the sum of the day figures as rounded, so the column adds up. Every
log is read, so a week that straddles a rotation still reports in full. Options:
.RS
.TP
.BR \-p ", " \-\-prefix " " \fIPREFIX\fR
Report only activities beginning
.IR PREFIX :
(the prefix followed by a colon), and strip that tag from the description that reaches the
timesheet, so an entry logged as
.B ST:Setup Jira
is reported as
.BR "Setup Jira" .
Entries without the tag belong to another job and are excluded entirely, their hours as well
as their descriptions. An empty
.I PREFIX
reports every entry unchanged, while still reading the settings of the prefix the
configuration would otherwise have selected.
.TP
.BR \-o ", " \-\-output " " \fIFILE\fR
Write the PDF to
.I FILE
instead of standard output; an existing directory receives the default file name, and
.B \-
forces standard output.
.I FILE
may contain
.BR {date} ", " {week_start} ", " {week_end} ", " {name} " and " {prefix} ,
which are replaced before the file is opened.
.TP
.BR \-t ", " \-\-template " " \fIFILE\fR
The form-fillable PDF to fill.
.TP
.BR \-a ", " \-\-activity " " \fITEMPLATE\fR
Text for one reported activity, taking
.B {activity}
and
.B {hours}
(default
.BR {activity} ).
.TP
.BR \-s ", " \-\-separator " " \fISTRING\fR
Separator between adjacent activities within a day (default
.BR "\(dq; \(dq" ).
.TP
.BR \-z ", " \-\-zero " " \fISTRING\fR
Hours text for a day with no recorded work (default empty, which leaves the cell blank).
.RE
.IP
Text is shrunk to fit its cell and, in the activity columns, wrapped; a description that
cannot fit even at the minimum size warns on stderr and is clipped. Writing a PDF to a
terminal is refused.
.TP
.BI "email " "[options] [week]"
Fill the timesheet as
.B pdf
does and mail it as an attachment. Takes every
.B pdf
option except that
.B \-t
means
.B \-\-to
here, so the template is named with
.B \-\-template
or
.BR \-T .
Additional options:
.RS
.TP
.BR \-t ", " \-\-to " " \fIADDRESS\fR
Recipient. May be repeated, and may take a comma-separated list.
.TP
.BR \-c ", " \-\-cc " " \fIADDRESS\fR
Carbon-copy recipient, likewise repeatable.
.TP
.BR \-f ", " \-\-from " " \fIADDRESS\fR
Sender address.
.TP
.BR \-r ", " \-\-reply " " \fIADDRESS\fR
Reply-To address. Worth setting when the relay rewrites
.B From
\(em a Gmail account may only send as itself unless the address is a verified
"Send mail as" alias \(em so that replies still reach the address you read.
.RE
.IP
The relay, credentials, subject and body come from the configuration file (see
.BR CONFIGURATION ).
If the send fails the finished PDF is kept on disk rather than discarded, so the message can
be retried without rebuilding a week that may have moved on.
.TP
.B sprint
Plaintext report like
.BR list ,
but combines the current
.B timesheet.log
with the most recently rotated
.B timesheet.YYMMDD
file before calculating the activity and weekday totals.
If work is in progress in the current log, uses a virtual STOP at current time
and shows current task, start time, and duration.
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
selects an alternate log path, extension, or date match.
.TP
.B manpage
Write this manual page in groff format to stdout. Example:
.B "timesheet manpage | groff \-man \-Tascii | less"
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
.B "target/release/timesheet install"
.I install_dir
where
.I install_dir
is the directory of the running
.B timesheet
binary.
If
.I directory
is omitted and the current directory has no
.BR Cargo.toml ,
clones
.B https://github.com/pillarsdotnet/timesheet
and builds from the clone.
.TP
.B prefix
Prepend
.IB prefix :
to this week's activities matching
.IR pattern .
.B timesheet prefix foo bar
is equivalent to
.BR "timesheet alias bar foo:bar" ,
so matching and the
.B Replace\ (y/n/a)
prompt work exactly as for
.BR alias .
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
If the last entry is START (work in progress), appends a STOP no later than one reminder interval after that entry first.
Rename the timesheet log to
.B timesheet.YYMMDD
using the timestamp of the log's earliest entry (START or STOP).
Errors if the log is missing or has no valid entries.
Rotation also happens on its own at the start of each week; see
.BR "AUTOMATIC ROTATION" .
.TP
.B start
Record work start
.IR now .
With no
.IR activity ,
shows the reminder chooser to pick or enter an activity (macOS via AppKit; Linux via the PyQt
single-click chooser, falling back to kdialog/zenity; Windows via PowerShell/WinForms). A single
click acts immediately.
Otherwise optional
.I activity
(default: misc/unspecified). Appends a START line; does not modify existing entries.
If a session is already open, no STOP is added when the new START would close it anyway:
start/stop pairs match in LIFO order, so a STOP at the same instant as the START is redundant.
A STOP is added only when the open START is more than one reminder interval old, in which case it
is capped to one interval after that entry, leaving the time you were away unbilled.
Starts or restarts the reminder daemon (resets the timer).
.TP
.B started
Record a work start at a
.IR "past time" .
.I start_time
accepts the forms listed under
.BR "TIME FORMATS" ,
e.g.
.BR "\(dq2026\-08\-06 07:00\(dq" ,
.BR 07:00 ,
or
.B 7am
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
is given, the log is left unchanged. If
.I stop_time
is given, the last STOP entry is amended to that time.
If the last entry is START, appends the new STOP (normal pairing).
In every case the reminder daemon is stopped and any prompt it has on screen is closed, and a
dialog reports that reminders have been stopped (skipped when
.B TS_LOGOUT
is set, e.g.\ during logout/shutdown).
Stopping the daemon happens even when the log needs no new entry: an unanswered reminder records
its own STOP, so without this a following
.B timesheet stop
would write nothing and leave the daemon running to prompt again one interval later.
The daemon runs in its own process group and is signalled as a group, so the chooser it spawned
goes with it; a stray daemon that no longer owns the PID file notices within half a second and
exits instead of prompting again.
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
.BR "TS_DEBUG=1 timesheet restart" ).
.TP
.B TS_LOGOUT
If set (any value), suppresses the "reminders stopped" dialog when
.B timesheet\ stop
is invoked (used by autostart scripts during logout/shutdown).
.TP
.B TS_CONFIG
Path to the configuration file, overriding the default location (see
.BR FILES ).
.TP
.B XDG_CONFIG_HOME
Configuration directory searched for
.B timesheet.yml
when
.B TS_CONFIG
is unset; defaults to
.BR $HOME/.config .
.SH FILES
.B $HOME/Documents/timesheet.log
Default timesheet log (path is compile-time in
.BR DEFAULT_TIMESHEET ).
.TP
.B $XDG_CONFIG_HOME/timesheet.yml
or
.B $HOME/.config/timesheet.yml
Optional settings; see
.BR CONFIGURATION .
A
.B timesheet.yaml
sibling is used if no
.B timesheet.yml
exists. Overridden by
.BR $TS_CONFIG .
.TP
.B $XDG_CACHE_HOME/ts-reminder-interval
or
.B $HOME/.cache/ts-reminder-interval
Reminder interval in seconds (decimal). Used by the reminder daemon; set via
.BR "timesheet interval" .
.TP
.B "$HOME/Library/Application Support/ts/" (macOS)
Autostart scripts: session script (stop on TERM), logout hook script (stop on logout/shutdown). The logout hook is registered with
.BR "defaults write com.apple.loginwindow LogoutHook" ;
if
.B timesheet\ autostart
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

/// Show the man page in a pager using groff (timesheet manpage | groff -man -Tascii | less).
/// If groff is not available, pages the raw groff source with less.
fn help_prelude() -> String {
    format!("{}\n\n", CANONICAL_SOURCE_URL)
}

#[cfg(unix)]
fn cmd_help() -> Result<(), String> {
    let man = manpage_content();
    let prelude = help_prelude();

    let child = Command::new("sh")
        .args([
            "-c",
            "{ printf '%s' \"$1\"; groff -man -Tascii 2>/dev/null; } | less -R",
            "sh",
            &prelude,
        ])
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
                "no pager available (groff, less): {}. Try: timesheet manpage | groff -man -Tascii | less",
                e
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prelude.as_bytes())
            .map_err(|e| e.to_string())?;
        stdin.write_all(man.as_bytes()).map_err(|e| e.to_string())?;
    }
    let _ = child.wait();
    Ok(())
}

/// Windows has no groff/less; render the man page source as plain text and page it with `more`,
/// falling back to printing straight to stdout if even that is unavailable.
#[cfg(not(unix))]
fn cmd_help() -> Result<(), String> {
    let mut text = help_prelude();
    text.push_str(&render_groff_plain(manpage_content()));

    if let Ok(mut child) = Command::new("more").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
        return Ok(());
    }
    print!("{}", text);
    Ok(())
}

/// Minimal groff `-man` macro renderer used only where groff itself is unavailable (Windows).
/// Handles just the macros and escapes this project's man page source actually uses; not a
/// general-purpose groff interpreter.
#[cfg(not(unix))]
fn render_groff_plain(source: &str) -> String {
    fn unescape(s: &str) -> String {
        s.replace("\\(dq", "\"")
            .replace("\\(em", "--")
            .replace("\\(->", "->")
            .replace("\\fB", "")
            .replace("\\fI", "")
            .replace("\\fR", "")
            .replace("\\&", "")
            .replace("\\-", "-")
            .replace("\\ ", " ")
    }
    fn tokenize(s: &str) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        let mut tokens = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            if chars[i] == '"' {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
                tokens.push(chars[start..i].iter().collect());
                if i < chars.len() {
                    i += 1;
                }
            } else {
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                tokens.push(chars[start..i].iter().collect());
            }
        }
        tokens
    }

    let mut out = String::new();
    let mut indent = 0usize;
    for raw_line in source.lines() {
        let Some(rest) = raw_line.strip_prefix('.') else {
            out.push_str(&" ".repeat(indent));
            out.push_str(&unescape(raw_line));
            out.push('\n');
            continue;
        };
        let mut parts = rest.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("");
        let arg_str = parts.next().unwrap_or("").trim();
        match name {
            "SH" | "SS" => {
                let heading = unescape(&tokenize(arg_str).join(" "));
                out.push('\n');
                out.push_str(&heading);
                out.push('\n');
            }
            "PP" | "LP" | "TP" | "IP" => out.push('\n'),
            "RS" => indent += 2,
            "RE" => indent = indent.saturating_sub(2),
            "br" => out.push('\n'),
            "B" | "I" => {
                let toks: Vec<String> = tokenize(arg_str).iter().map(|t| unescape(t)).collect();
                if !toks.is_empty() {
                    out.push_str(&" ".repeat(indent));
                    out.push_str(&toks.join(" "));
                    out.push('\n');
                }
            }
            "BR" | "IR" | "RB" | "RI" | "BI" | "IB" => {
                let toks: Vec<String> = tokenize(arg_str).iter().map(|t| unescape(t)).collect();
                out.push_str(&" ".repeat(indent));
                out.push_str(&toks.concat());
                out.push('\n');
            }
            "TH" | "nf" | "fi" => {}
            _ => {
                if !arg_str.is_empty() {
                    out.push_str(&" ".repeat(indent));
                    out.push_str(&unescape(arg_str));
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Register "timesheet start" on login and "timesheet stop" on logout/shutdown (macOS: launchd; Linux: systemd user). Use "timesheet autostart uninstall" to remove.
/// Optional first argument: interval (e.g. 5s, 3m) to set reminder interval and start the daemon in this session so the reminder appears soon.
fn cmd_autostart(args: &[String]) -> Result<(), String> {
    let uninstall = args.first().map(String::as_str) == Some("uninstall");
    if !uninstall {
        // Like `timesheet start`, close a session left open by a previous day's missed shutdown STOP
        // (capped to one reminder interval) so autostart never leaves an all-night session dangling.
        reconcile_stale_open_session(&timesheet_path(), Local::now());
        let interval_set = if let Some(interval_arg) = args.first() {
            if let Ok(secs) = parse_interval_duration(interval_arg) {
                let path = reminder_interval_path();
                if let Err(e) = fs::write(&path, secs.to_string()) {
                    eprintln!("timesheet autostart: could not set interval: {}", e);
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
            if secs >= 3600 && secs.is_multiple_of(3600) {
                println!("Reminder interval: {}h", secs / 3600);
            } else if secs >= 60 && secs.is_multiple_of(60) {
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
    #[cfg(target_os = "windows")]
    {
        if uninstall {
            do_autostart_uninstall_windows()
        } else {
            do_autostart_install_windows()
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = uninstall;
        Err(
            "timesheet autostart: not supported on this platform (macOS, Linux, and Windows only)."
                .to_string(),
        )
    }
}

/// Per-user Startup-folder shortcut path used for Windows autostart (run at login, no admin
/// needed): the direct analog of a macOS LaunchAgent or Linux systemd user unit.
#[cfg(target_os = "windows")]
fn windows_startup_shortcut_path() -> Result<PathBuf, String> {
    Ok(dirs::config_dir()
        .ok_or("could not determine %APPDATA%")?
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("Timesheet Autostart.lnk"))
}

/// Register `timesheet.exe start` to run at login via a Startup-folder shortcut. STOP at
/// logoff/shutdown is handled by the reminder daemon's console control handler
/// (`windows_console_ctrl_handler`) rather than a separate hook: unlike macOS's LogoutHook or
/// Linux's system-level logout-hook unit, there is no per-user, no-admin-required Windows
/// mechanism to add a second guarantee, so this one path is best-effort (see the daemon's doc
/// comment for the same caveat other platforms already carry).
#[cfg(target_os = "windows")]
fn do_autostart_install_windows() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let shortcut = windows_startup_shortcut_path()?;
    create_windows_shortcut(
        &shortcut,
        &exe,
        "start",
        exe.parent().unwrap_or(Path::new(".")),
        "Record work start at login (same as running \"timesheet start\")",
    )?;
    println!("Autostart installed: {}", shortcut.display());
    println!(
        "\"timesheet start\" runs at login; STOP at logoff/shutdown is best-effort (no admin rights available for a second guarantee)."
    );
    println!("  To remove: timesheet autostart uninstall");
    Ok(())
}

#[cfg(target_os = "windows")]
fn do_autostart_uninstall_windows() -> Result<(), String> {
    let shortcut = windows_startup_shortcut_path()?;
    if shortcut.exists() {
        fs::remove_file(&shortcut).map_err(|e| {
            format!(
                "timesheet autostart: cannot remove {}: {}",
                shortcut.display(),
                e
            )
        })?;
        println!("Removed {}", shortcut.display());
    } else {
        println!("Autostart was not installed.");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn do_autostart_install_macos() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe_path = exe.to_string_lossy();
    let home = env::var_os("HOME").ok_or("timesheet autostart: HOME not set")?;
    let agents = PathBuf::from(&home).join("Library/LaunchAgents");
    let support = PathBuf::from(&home).join("Library/Application Support/ts");
    fs::create_dir_all(&support).map_err(|e| {
        format!(
            "timesheet autostart: cannot create {}: {}",
            support.display(),
            e
        )
    })?;

    // Remove old shell-script session wrapper if present (superseded by --session-daemon).
    let _ = fs::remove_file(support.join("autostart-session.sh"));

    // LogoutHook runs as root on logout/shutdown and macOS waits for it to complete before
    // proceeding, making it the most reliable mechanism for recording STOP. It uses
    // `launchctl asuser` to run timesheet stop in the console user's launchd context (faster than
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
    fs::write(&logout_hook_path, logout_script)
        .map_err(|e| format!("timesheet autostart: cannot write logout hook: {}", e))?;
    #[allow(clippy::disallowed_methods)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&logout_hook_path)
            .map_err(|e| e.to_string())?
            .permissions();
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
                    .args([
                        "defaults",
                        "write",
                        "com.apple.loginwindow",
                        "LogoutHook",
                        logout_hook_path.to_string_lossy().as_ref(),
                    ])
                    .status()
                    .map_err(|e| e.to_string())?
                    .success()
                {
                    return Err(
                        "timesheet autostart: logout hook command failed (sudo may have been cancelled)."
                            .to_string(),
                    );
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
        exe_path
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    );
    // The session plist runs `timesheet --session-daemon` directly (no shell-script wrapper).
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
        exe_path
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    );

    fs::create_dir_all(&agents).map_err(|e| {
        format!(
            "timesheet autostart: cannot create {}: {}",
            agents.display(),
            e
        )
    })?;
    let start_plist_path = agents.join("com.ts.autostart.start.plist");
    let session_plist_path = agents.join("com.ts.autostart.session.plist");
    fs::write(&start_plist_path, &start_plist)
        .map_err(|e| format!("timesheet autostart: cannot write plist: {}", e))?;
    fs::write(&session_plist_path, &session_plist)
        .map_err(|e| format!("timesheet autostart: cannot write plist: {}", e))?;

    let _ = Command::new("launchctl")
        .arg("unload")
        .arg(&start_plist_path)
        .output();
    let _ = Command::new("launchctl")
        .arg("unload")
        .arg(&session_plist_path)
        .output();
    if !Command::new("launchctl")
        .arg("load")
        .arg(&start_plist_path)
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("timesheet autostart: launchctl load start plist failed".to_string());
    }
    if !Command::new("launchctl")
        .arg("load")
        .arg(&session_plist_path)
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("timesheet autostart: launchctl load session plist failed".to_string());
    }
    println!(
        "Autostart installed: \"timesheet start\" runs at login, \"timesheet stop\" runs at logout/shutdown."
    );
    println!("  Start plist:   {}", start_plist_path.display());
    println!("  Session plist: {}", session_plist_path.display());
    println!("  Logout hook:   {}", logout_hook_path.display());
    println!("  To remove: timesheet autostart uninstall");
    Ok(())
}

#[cfg(target_os = "macos")]
fn do_autostart_uninstall_macos() -> Result<(), String> {
    let home = env::var_os("HOME").ok_or("timesheet autostart: HOME not set")?;
    let agents = PathBuf::from(&home).join("Library/LaunchAgents");
    let start_plist_path = agents.join("com.ts.autostart.start.plist");
    let session_plist_path = agents.join("com.ts.autostart.session.plist");
    let support = PathBuf::from(&home).join("Library/Application Support/ts");
    let logout_hook_path = support.join("logout-hook.sh");

    let _ = Command::new("launchctl")
        .arg("unload")
        .arg(&start_plist_path)
        .output();
    let _ = Command::new("launchctl")
        .arg("unload")
        .arg(&session_plist_path)
        .output();
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

/// Directory holding systemd user units ($XDG_CONFIG_HOME/systemd/user, defaulting to $HOME/.config/systemd/user).
#[cfg(target_os = "linux")]
fn linux_user_units_dir() -> Result<PathBuf, String> {
    let config = match env::var_os("XDG_CONFIG_HOME") {
        Some(c) => PathBuf::from(c),
        None => {
            let home = env::var_os("HOME").ok_or("timesheet autostart: HOME not set")?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(config.join("systemd/user"))
}

#[cfg(target_os = "linux")]
fn do_autostart_install_linux() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe_path = exe.to_string_lossy();
    let user_units = linux_user_units_dir()?;
    fs::create_dir_all(&user_units).map_err(|e| {
        format!(
            "timesheet autostart: cannot create {}: {}",
            user_units.display(),
            e
        )
    })?;

    // RemainAfterExit keeps the unit active after `timesheet start` exits so systemd does not tear down
    // its control group -- otherwise the reminder daemon `timesheet start` spawns (which lives in this
    // unit's cgroup; setsid only changes the process group, not the cgroup) would be killed the
    // moment `timesheet start` returns.
    let start_unit = format!(
        r#"[Unit]
Description=timesheet start on login
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart={} start
[Install]
WantedBy=default.target
"#,
        exe_path
    );
    let session_unit = format!(
        r#"[Unit]
Description=timesheet stop on logout
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
    fs::write(&start_path, &start_unit)
        .map_err(|e| format!("timesheet autostart: cannot write unit: {}", e))?;
    fs::write(&session_path, &session_unit)
        .map_err(|e| format!("timesheet autostart: cannot write unit: {}", e))?;

    if !Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("timesheet autostart: systemctl daemon-reload failed".to_string());
    }
    // Enable the start unit for next login but do NOT --now it: running `timesheet start` immediately would
    // close the caller's already-open session and pop another chooser. The reminder daemon for the
    // current session is started separately by `timesheet autostart` (start_reminder_daemon_if_needed).
    if !Command::new("systemctl")
        .args(["--user", "enable", "ts-autostart-start.service"])
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("timesheet autostart: systemctl enable start service failed".to_string());
    }
    if !Command::new("systemctl")
        .args(["--user", "enable", "--now", "ts-autostart-session.service"])
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("timesheet autostart: systemctl enable session service failed".to_string());
    }
    println!(
        "Autostart installed: \"timesheet start\" runs at login, \"timesheet stop\" runs at logout/shutdown."
    );
    println!(
        "  Units: {}  {}",
        start_path.display(),
        session_path.display()
    );
    // Also offer a system-level shutdown hook (like the macOS LogoutHook) as a second guarantee.
    install_linux_logout_hook(&exe_path)?;
    println!("  To remove: timesheet autostart uninstall");
    Ok(())
}

/// Name of the system-level logout-hook unit (keyed by uid so multiple users don't collide).
#[cfg(target_os = "linux")]
fn linux_logout_hook_unit_name() -> String {
    let uid = unsafe { libc::getuid() };
    format!("ts-logout-{}.service", uid)
}

/// Install a system-level systemd unit whose ExecStop records a STOP at shutdown/reboot, mirroring
/// the macOS LogoutHook. The systemd *user* session unit already records STOP at logout, but on a
/// full shutdown the user manager can be torn down before its ExecStop runs; a system unit ordered
/// `Before=shutdown.target` is the reliable, "system waits for it" guarantee. Installing into
/// /etc/systemd/system needs root, so (like macOS) we print the command and offer to run it via sudo.
#[cfg(target_os = "linux")]
fn install_linux_logout_hook(exe_path: &str) -> Result<(), String> {
    let unit_name = linux_logout_hook_unit_name();
    let dest = format!("/etc/systemd/system/{}", unit_name);
    if Path::new(&dest).exists() {
        // Already installed (readable without root).
        return Ok(());
    }
    let uid = unsafe { libc::getuid() };
    let unit = format!(
        r#"[Unit]
Description=Record timesheet STOP at shutdown for uid {uid}
DefaultDependencies=no
Before=shutdown.target reboot.target halt.target

[Service]
Type=oneshot
RemainAfterExit=yes
User={uid}
Environment=TS_LOGOUT=1
ExecStart=/bin/true
ExecStop={exe} stop

[Install]
WantedBy=multi-user.target
"#,
        uid = uid,
        exe = exe_path
    );

    // Stage the unit in a user-writable timesheet dir; the sudo command installs it system-wide.
    let staged = linux_user_units_dir()?
        .parent() // .../systemd
        .and_then(|p| p.parent()) // .../.config
        .map(|c| c.join("ts"))
        .ok_or("timesheet autostart: cannot resolve config dir")?;
    fs::create_dir_all(&staged).map_err(|e| {
        format!(
            "timesheet autostart: cannot create {}: {}",
            staged.display(),
            e
        )
    })?;
    let staged_unit = staged.join(&unit_name);
    fs::write(&staged_unit, &unit)
        .map_err(|e| format!("timesheet autostart: cannot write logout hook: {}", e))?;

    let inner = format!(
        "install -m644 '{src}' '{dest}' && systemctl daemon-reload && systemctl enable --now {name}",
        src = staged_unit.display(),
        dest = dest,
        name = unit_name
    );
    println!("  To also record STOP on a full shutdown/reboot (a second guarantee, like the macOS");
    println!("  logout hook), install a system service. This requires administrator access:");
    println!("  sudo sh -c \"{}\"", inner);
    print!("  Run this command now? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).is_ok() {
        let answer = line.trim().to_lowercase();
        if answer == "y" || answer == "yes" {
            match Command::new("sudo").args(["sh", "-c", &inner]).status() {
                Ok(s) if s.success() => println!("  Logout hook installed ({}).", dest),
                _ => println!(
                    "  timesheet autostart: logout hook not installed (sudo cancelled or failed); \
                     run the command above to enable it."
                ),
            }
        }
    }
    Ok(())
}

/// Remove the system-level logout-hook unit (best effort; prints the sudo command to run manually).
#[cfg(target_os = "linux")]
fn uninstall_linux_logout_hook() {
    let unit_name = linux_logout_hook_unit_name();
    let dest = format!("/etc/systemd/system/{}", unit_name);
    if !Path::new(&dest).exists() {
        return;
    }
    let inner = format!(
        "systemctl disable --now {name}; rm -f '{dest}'; systemctl daemon-reload",
        name = unit_name,
        dest = dest
    );
    println!("Removing the system-level logout hook requires administrator access:");
    println!("  sudo sh -c \"{}\"", inner);
    print!("  Run this command now? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).is_ok() {
        let answer = line.trim().to_lowercase();
        if answer == "y" || answer == "yes" {
            let _ = Command::new("sudo").args(["sh", "-c", &inner]).status();
        }
    }
}

#[cfg(target_os = "linux")]
fn do_autostart_uninstall_linux() -> Result<(), String> {
    let user_units = linux_user_units_dir()?;
    let start_path = user_units.join("ts-autostart-start.service");
    let session_path = user_units.join("ts-autostart-session.service");

    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", "ts-autostart-start.service"])
        .output();
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", "ts-autostart-session.service"])
        .output();
    let _ = fs::remove_file(&start_path);
    let _ = fs::remove_file(&session_path);
    uninstall_linux_logout_hook();
    println!("Autostart uninstalled.");
    Ok(())
}

const REMINDER_SLEEP_SECS: u64 = 300; // 5 minutes (default when no interval file)

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
        let _ = writeln!(io::stderr(), "timesheet: {}", msg);
    }
}

/// Returns true if a process with the given PID exists (Unix: kill(pid, 0)). Does not spawn any subprocess.
fn is_pid_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { kill(pid as i32, 0) == 0 }
    }
    #[cfg(target_os = "windows")]
    {
        win_ffi::is_pid_running(pid)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = pid;
        false
    }
}

/// Clear the inherited signal mask in a child process, between fork and exec.
///
/// The reminder daemon blocks SIGTERM so a dedicated `sigwait` thread can handle it, and a blocked
/// mask survives both fork and exec. Without this reset every dialog the daemon spawns inherits the
/// block and ignores SIGTERM for its whole life, so a chooser left on screen cannot be closed by an
/// ordinary `timesheet stop` — only SIGKILL reaches it. Apply this to anything the daemon runs.
#[cfg(unix)]
fn reset_child_signal_mask(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            let mut empty = std::mem::zeroed::<libc::sigset_t>();
            sigemptyset(&mut empty);
            pthread_sigmask(SIG_SETMASK, &empty, std::ptr::null_mut());
            Ok(())
        });
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

/// Signal the reminder daemon *and* any dialog it currently has on screen.
///
/// The daemon calls `setsid()` at spawn, so it leads its own session and process group, and the
/// chooser it runs (PyQt/kdialog/zenity) inherits that group. Signaling the group therefore closes
/// a prompt that is still up; signaling the bare PID would leave that window orphaned on screen.
/// Falls back to the bare PID when the daemon does not lead its own group, or when that group is
/// ours, so an unrelated group is never signaled.
/// Returns the daemon's own process group, or None when it does not lead one (or leads ours), in
/// which case only the bare PID is safe to signal. Read this *before* signaling: once the daemon
/// exits, `getpgid` fails and the dialogs it left behind can no longer be found this way.
#[cfg(unix)]
fn reminder_daemon_group(pid: u32) -> Option<i32> {
    let pgid = unsafe { getpgid(pid as i32) };
    (pgid == pid as i32 && pgid != unsafe { getpgrp() }).then_some(pgid)
}

#[cfg(unix)]
fn signal_reminder_daemon(group: Option<i32>, pid: u32, sig: i32) {
    match group {
        Some(pgid) => {
            let _ = unsafe { kill(-pgid, sig) };
        }
        None => signal_pid(pid, sig),
    }
}

/// Returns true if the reminder daemon is running (PID file exists, PID is alive, and not self).
/// False on platforms where `is_pid_running` is a no-op.
fn is_reminder_daemon_running() -> bool {
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
        let mut cmd = Command::new("notify-send");
        cmd.args([
            "--app-name=Timesheet",
            "Timesheet reminders have been stopped.",
        ]);
        // notify-send talks to the session bus, which may be missing when launched from systemd.
        linux_with_display(&mut cmd);
        let _ = cmd
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
/// intentional timesheet kill rather than a system shutdown, so it skips writing a STOP entry.
/// No-op on unsupported platforms. Never kills the current process.
fn kill_reminder_daemon_if_running() {
    #[cfg(target_os = "windows")]
    {
        ts_debug("kill_reminder: entry");
        let pid_path = reminder_pid_path();
        if let Ok(data) = fs::read_to_string(&pid_path) {
            if let Ok(pid) = data.trim().parse::<u32>() {
                if pid == process::id() {
                    ts_debug("kill_reminder: pid is self, removing file and skipping kill");
                    let _ = fs::remove_file(&pid_path);
                    return;
                }
                if is_pid_running(pid) {
                    // Remove PID file before terminating, same intentional-kill-vs-shutdown
                    // distinction the Unix path uses (the daemon's console ctrl handler checks it).
                    let _ = fs::remove_file(&pid_path);
                    // Terminate the daemon's job object first: this also closes any chooser dialog
                    // window it currently has open (a separate powershell.exe process, but a member
                    // of the same job), same purpose as signaling the whole process group on Unix.
                    ts_debug(&format!("kill_reminder: terminating job for {}", pid));
                    win_ffi::terminate_reminder_job(pid);
                    for _ in 0..20 {
                        if !is_pid_running(pid) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                    if is_pid_running(pid) {
                        // Job termination didn't take (e.g. the daemon never created one); fall
                        // back to terminating the bare PID.
                        ts_debug(&format!("kill_reminder: terminating pid {} directly", pid));
                        win_ffi::terminate_process(pid);
                    }
                    ts_debug("kill_reminder: done");
                    return;
                }
                ts_debug("kill_reminder: process not running");
            }
        } else {
            ts_debug("kill_reminder: no pid file or unreadable");
        }
        let _ = fs::remove_file(&pid_path);
        ts_debug("kill_reminder: done");
        return;
    }

    #[cfg(not(any(unix, target_os = "windows")))]
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
                    // Note the group before signaling; it is unreadable once the daemon exits.
                    let group = reminder_daemon_group(pid);
                    // Remove PID file before signaling: the daemon's SIGTERM handler checks for
                    // the PID file to distinguish intentional kills from system shutdown.
                    let _ = fs::remove_file(&pid_path);
                    ts_debug(&format!("kill_reminder: sending SIGTERM to {}", pid));
                    signal_reminder_daemon(group, pid, SIGTERM);
                    // Give the daemon a moment to leave on its own -- on macOS it records a STOP
                    // from its SIGTERM handler first, which a prompt SIGKILL would cut short.
                    for _ in 0..20 {
                        if !is_pid_running(pid) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                    if is_pid_running(pid) {
                        ts_debug(&format!("kill_reminder: sending SIGKILL to {}", pid));
                        signal_reminder_daemon(group, pid, SIGKILL);
                    } else if let Some(pgid) = group {
                        // The daemon is gone; SIGKILL whatever it left in its group. A dialog
                        // spawned by an older build inherited the daemon's blocked SIGTERM and
                        // survives everything short of SIGKILL, which would strand it on screen.
                        ts_debug(&format!(
                            "kill_reminder: SIGKILL leftovers in group {}",
                            pgid
                        ));
                        let _ = unsafe { kill(-pgid, SIGKILL) };
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
    #[cfg(target_os = "windows")]
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
            // Stop this process's own stdio handles from riding along into the daemon: Windows
            // handle inheritance is all-or-nothing, so without this a caller capturing this
            // process's output (a script, `$out = & timesheet.exe start`, ...) would block forever
            // waiting for EOF, since the long-lived daemon keeps the pipe's write end open. Only
            // safe to do here, not when TS_DEBUG wants the daemon to inherit these same handles.
            win_ffi::make_own_std_handles_noninheritable();
        }
        // CREATE_NO_WINDOW keeps a hidden (not absent) console so the daemon's console ctrl
        // handler can still receive logoff/shutdown events. Try to also break away from any
        // enclosing job (e.g. Windows Terminal's per-tab job, which may kill-on-close) so the
        // daemon survives after the spawning terminal closes -- the Windows analog of setsid()
        // detaching from the controlling terminal on Unix. CreateProcess fails outright if the
        // enclosing job forbids breakaway, so retry once without that flag on failure.
        let base_flags = win_ffi::CREATE_NO_WINDOW | win_ffi::CREATE_NEW_PROCESS_GROUP;
        let spawn = |flags: u32| {
            Command::new(&exe)
                .arg("--reminder-daemon")
                .stdin(Stdio::null())
                .stdout(if use_debug {
                    Stdio::inherit()
                } else {
                    Stdio::null()
                })
                .stderr(if use_debug {
                    Stdio::inherit()
                } else {
                    Stdio::null()
                })
                .creation_flags(flags)
                .spawn()
        };
        let result =
            spawn(base_flags | win_ffi::CREATE_BREAKAWAY_FROM_JOB).or_else(|_| spawn(base_flags));
        match result {
            Ok(child) => {
                ts_debug(&format!(
                    "start_reminder: spawned daemon pid {}",
                    child.id()
                ));
                drop(child);
            }
            Err(e) => {
                ts_debug(&format!("start_reminder: spawn failed: {}", e));
            }
        }
        ts_debug("start_reminder: done");
        return;
    }

    #[cfg(not(any(unix, target_os = "windows")))]
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
        // This places the reminder daemon in its own session before timesheet start exits,
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
                ts_debug(&format!(
                    "start_reminder: spawned daemon pid {}",
                    child.id()
                ));
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
    fs::write(&path, secs.to_string())
        .map_err(|e| format!("timesheet interval: cannot write config: {}", e))?;
    kill_reminder_daemon_if_running();
    thread::sleep(Duration::from_millis(100));
    start_reminder_daemon_if_needed(timesheet);
    if secs % 3600 == 0 && secs >= 3600 {
        println!(
            "Reminder interval set to {}h. Daemon restarted.",
            secs / 3600
        );
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
/// LaunchAgent by `timesheet autostart`. Because this is a launchd job, launchd delivers a
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
                if last
                    .and_then(parse_line)
                    .map(|ll| matches!(ll, LogLine::Start(_, _)))
                    .unwrap_or(false)
                {
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

/// Timesheet path stashed for `windows_console_ctrl_handler`, which -- being a raw function
/// pointer passed to `SetConsoleCtrlHandler` -- cannot capture it as a closure would.
#[cfg(target_os = "windows")]
static REMINDER_TIMESHEET_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Console control handler: the Windows analog of the Unix SIGTERM handler in this same function.
/// Unlike SIGTERM, `kill_reminder_daemon_if_running`'s intentional-kill path never reaches this
/// handler on Windows (it terminates the job/process directly, bypassing control handlers
/// entirely), so -- unlike the Unix version -- there is no need to distinguish an intentional kill
/// from a real logoff/shutdown here: every call is a genuine one. Records a STOP if a session is
/// open, then exits. Windows does not guarantee it waits for this to finish the way launchd/
/// systemd wait for their equivalents, so this is best-effort, same as the other platforms.
#[cfg(target_os = "windows")]
unsafe extern "system" fn windows_console_ctrl_handler(ctrl_type: u32) -> i32 {
    if ctrl_type == win_ffi::CTRL_LOGOFF_EVENT
        || ctrl_type == win_ffi::CTRL_SHUTDOWN_EVENT
        || ctrl_type == win_ffi::CTRL_CLOSE_EVENT
    {
        if let Some(timesheet) = REMINDER_TIMESHEET_PATH.get() {
            let content = fs::read_to_string(timesheet).unwrap_or_default();
            let last = content.lines().rev().find(|l| !l.trim().is_empty());
            if last
                .and_then(parse_line)
                .map(|ll| matches!(ll, LogLine::Start(_, _)))
                .unwrap_or(false)
            {
                let _ = append_stop_entry(timesheet, Local::now());
            }
        }
        process::exit(0);
    }
    0
}

fn run_reminder_daemon(timesheet: &Path) {
    #[cfg(target_os = "windows")]
    {
        // Join our own kill-on-close Job Object (the Windows analog of setsid() below): anything
        // this daemon later spawns, e.g. a chooser dialog, becomes a member too, so terminating
        // the job (kill_reminder_daemon_if_running) closes both in one call.
        let _ =
            win_ffi::create_and_join_kill_on_close_job(&win_ffi::reminder_job_name(process::id()));
        let _ = REMINDER_TIMESHEET_PATH.set(timesheet.to_path_buf());
        win_ffi::set_console_ctrl_handler(windows_console_ctrl_handler);
    }
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
            let _ = &timesheet_for_signal; // used only on non-Linux below
            let mut sig: libc::c_int = 0;
            if unsafe { sigwait(&set_for_sigwait, &mut sig) } == 0 && sig == SIGTERM {
                // On macOS the reminder daemon IS the session LaunchAgent: launchd SIGTERMs it at
                // logout/shutdown and it records the STOP here. kill_reminder_daemon_if_running()
                // removes the PID file before signaling, so an intentional `timesheet` kill (file gone or
                // no longer ours) is skipped.
                //
                // On Linux the logout STOP is recorded by the systemd session unit's ExecStop
                // (`timesheet stop`), and systemd may SIGTERM the daemon during ordinary unit/cgroup
                // teardown -- e.g. when the oneshot `timesheet start` that spawned it exits -- not only at
                // logout. Writing a STOP here would produce spurious entries, so the daemon stays
                // silent and just exits.
                #[cfg(not(target_os = "linux"))]
                {
                    let my_pid = process::id();
                    let is_shutdown = fs::read_to_string(reminder_pid_path())
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .map(|p| p == my_pid)
                        .unwrap_or(false);
                    if is_shutdown {
                        let _ = append_stop_entry(&timesheet_for_signal, Local::now());
                    }
                }
                process::exit(0);
            }
        });
    }
    let pid_path = reminder_pid_path();
    if let Some(parent) = pid_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Claim sole ownership; if another daemon is already running, exit instead of duplicating it.
    if !claim_reminder_daemon_ownership(&pid_path) {
        ts_debug("reminder daemon: another daemon already owns the pid file, exiting");
        return;
    }
    IS_REMINDER_DAEMON.store(true, std::sync::atomic::Ordering::Relaxed);
    let pid_path_guard = pid_path.clone();
    let _cleanup = defer(move || {
        // Only remove the pid file if we still own it, so we never delete a successor's file.
        if owns_reminder_daemon(&pid_path_guard) {
            let _ = fs::remove_file(&pid_path_guard);
        }
    });

    loop {
        // If ownership changed underneath us (e.g. another daemon took over), exit quietly.
        if !owns_reminder_daemon(&pid_path) {
            ts_debug("reminder daemon: lost pid ownership, exiting");
            return;
        }
        let interval_secs = get_reminder_interval_secs();
        ts_debug(&format!("reminder daemon: sleeping {}s", interval_secs));
        // Sleep in slices rather than one long nap, re-checking ownership as we go: `timesheet stop`
        // silences a daemon by removing the PID file, and a daemon it could not signal (a stray
        // that is not the file's owner) must notice within a moment instead of sleeping out the
        // interval and popping one more prompt.
        let deadline = std::time::Instant::now() + Duration::from_secs(interval_secs);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            if !owns_reminder_daemon(&pid_path) {
                ts_debug("reminder daemon: lost pid ownership while sleeping, exiting");
                return;
            }
            thread::sleep(remaining.min(REMINDER_OWNERSHIP_POLL));
        }
        // Re-check right before prompting: nothing should put a window on screen after a stop.
        if !owns_reminder_daemon(&pid_path) {
            ts_debug("reminder daemon: lost pid ownership before prompting, exiting");
            return;
        }
        ts_debug("reminder daemon: showing prompt");

        let activities = reminder_activities_most_recent_first(timesheet);
        match show_reminder_prompt(&activities, Some(timesheet)) {
            ReminderResult::DontBugMe => {
                // "Stop Work": close the open session (record a STOP) before stopping reminders.
                close_open_session(timesheet, Local::now());
                show_reminders_stopped_notification();
                break;
            }
            ReminderResult::Activity(activity) => {
                let _ = append_start_entry(timesheet, &activity);
            }
            ReminderResult::EnterNew => {
                unreachable!("show_reminder_prompt converts EnterNew to Activity")
            }
            ReminderResult::ShowAgainImmediate => {} // dismissed without choice; re-show immediately
            ReminderResult::TimeoutAddStop(dt) => {
                // Reached only when no chooser could be shown at all (no PyQt/kdialog/zenity) or the
                // dialog failed to run. A chooser that did appear keeps itself on screen past the
                // interval and records this STOP itself, so it returns Activity on your return
                // instead of coming through here.
                let _ = append_reminder_timeout_stop(timesheet, dt);
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

fn parse_native_reminder_dialog_output(output: &str) -> Option<ReminderResult> {
    let output = output.trim();
    if output.is_empty() {
        return None;
    }
    if output == "Stop Work" {
        return Some(ReminderResult::DontBugMe);
    }
    if output == "Enter new activity..." {
        return Some(ReminderResult::EnterNew);
    }
    Some(ReminderResult::Activity(output.to_string()))
}

/// Show "What are you working on?" prompt; returns user choice or timeout.
/// Platform-specific (macOS: AppKit/osascript; Linux: PyQt single-click chooser, else kdialog/zenity;
/// Windows: PowerShell/WinForms chooser).
/// timesheet: used when appending STOP on timeout (reminder daemon / timesheet start).
fn show_reminder_prompt(activities: &[String], timesheet: Option<&Path>) -> ReminderResult {
    #[cfg(target_os = "macos")]
    return show_reminder_prompt_macos(activities, timesheet);

    #[cfg(target_os = "linux")]
    return show_reminder_prompt_linux(activities, timesheet);

    #[cfg(target_os = "windows")]
    return show_reminder_prompt_windows(activities, timesheet);

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (activities, timesheet);
        ReminderResult::TimeoutAddStop(Local::now())
    }
}

/// Resolve the activity for `timesheet start` when none was given on the command line.
/// Returns `Some(activity)` to start, or `None` if the user chose "Stop Work" (caller should abort the start).
/// On platforms (or headless setups) without a GUI chooser, returns the default activity without prompting.
/// Whether `timesheet start` with no activity can show an interactive GUI chooser on this platform/setup
/// (macOS and Windows always; Linux when kdialog/zenity is installed). Used both to decide whether to
/// block on the chooser and to avoid starting the reminder daemon early when we will.
#[cfg(not(test))]
fn start_chooser_available() -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        detect_linux_dialog().is_some()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

#[cfg(not(test))]
fn resolve_start_activity(timesheet: &Path) -> Option<String> {
    if !start_chooser_available() {
        return Some("misc/unspecified".to_string());
    }

    let activities = reminder_activities_most_recent_first(timesheet);
    loop {
        match show_reminder_prompt(&activities, Some(timesheet)) {
            ReminderResult::Activity(a) => return Some(a),
            ReminderResult::DontBugMe => return None,
            ReminderResult::ShowAgainImmediate => {
                // Debounce on Linux: if the GUI helper exits instantly (e.g. the display is not
                // reachable yet at login) this avoids a tight CPU-spinning re-show loop.
                #[cfg(target_os = "linux")]
                thread::sleep(Duration::from_millis(500));
            }
            ReminderResult::TimeoutAddStop(dt) => {
                let _ = append_reminder_timeout_stop(timesheet, dt);
                // re-show immediately
            }
            ReminderResult::EnterNew => {
                unreachable!("show_reminder_prompt converts EnterNew to Activity")
            }
        }
    }
}

/// Build the `Stop Work` / activities / `Enter new activity...` choice list shown in the reminder dialog.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn reminder_choices(activities: &[String]) -> Vec<String> {
    let mut choices = vec!["Stop Work".to_string()];
    for a in activities.iter().rev() {
        if !a.is_empty() && !choices.contains(a) {
            choices.push(a.clone());
        }
    }
    choices.push("Enter new activity...".to_string());
    choices
}

/// Which GUI dialog helper to drive on Linux. kdialog is native to KDE/Plasma; zenity covers GNOME/other.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum LinuxDialog {
    KDialog,
    Zenity,
}

/// True if `name` is an executable found on `$PATH`.
#[cfg(target_os = "linux")]
fn command_on_path(name: &str) -> bool {
    let path = match env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file()
            && std::fs::metadata(&candidate)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o111 != 0
                })
                .unwrap_or(false)
    })
}

/// Pick a dialog backend. Prefer zenity only on explicitly GTK-based desktops; otherwise prefer
/// kdialog (KDE-first), then fall back to whichever is installed. Choosing kdialog when the desktop
/// is unknown keeps the dialog identical whether the daemon is launched from an interactive shell or
/// from a systemd user unit (where XDG_CURRENT_DESKTOP is typically unset), avoiding mixed window styles.
#[cfg(target_os = "linux")]
fn detect_linux_dialog() -> Option<LinuxDialog> {
    let has_kdialog = command_on_path("kdialog");
    let has_zenity = command_on_path("zenity");
    let desktop = env::var_os("XDG_CURRENT_DESKTOP")
        .map(|d| d.to_string_lossy().to_uppercase())
        .unwrap_or_default();
    let gtk_desktop = [
        "GNOME", "XFCE", "CINNAMON", "MATE", "UNITY", "LXDE", "PANTHEON",
    ]
    .iter()
    .any(|d| desktop.contains(d));
    if gtk_desktop && has_zenity {
        return Some(LinuxDialog::Zenity);
    }
    if has_kdialog {
        return Some(LinuxDialog::KDialog);
    }
    if has_zenity {
        return Some(LinuxDialog::Zenity);
    }
    None
}

/// Prepare the GUI session environment for a dialog command, preferring native Wayland (KDE Plasma)
/// with an X11/XWayland fallback.
///
/// Two problems are handled:
///   1. Launched from a systemd user unit, the graphical-session variables (XDG_RUNTIME_DIR,
///      WAYLAND_DISPLAY, DBUS_SESSION_BUS_ADDRESS) may be missing. We derive XDG_RUNTIME_DIR from
///      the uid and probe its directory for a `wayland-N` socket so the dialog still connects.
///   2. Even in an interactive Wayland session, Qt apps like kdialog default to the xcb (XWayland)
///      backend when DISPLAY is set and QT_QPA_PLATFORM is unset, so they render as X11 windows.
///      Setting QT_QPA_PLATFORM=wayland;xcb makes kdialog use Wayland natively when available and
///      fall back to X11 otherwise. (GTK apps like zenity already pick Wayland on their own.)
///
/// Existing values are never overridden, so an explicit user/session configuration wins.
#[cfg(target_os = "linux")]
fn linux_with_display(cmd: &mut Command) {
    // Every caller is a GUI helper the daemon may spawn, so make sure none of them inherits the
    // daemon's blocked SIGTERM and becomes an un-closable window.
    reset_child_signal_mask(cmd);

    // Where the Wayland and D-Bus sockets live.
    let runtime_dir = match env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d),
        None => {
            let uid = unsafe { libc::getuid() };
            let d = PathBuf::from(format!("/run/user/{}", uid));
            cmd.env("XDG_RUNTIME_DIR", &d);
            d
        }
    };

    // Determine the Wayland display: use the inherited one, else probe the runtime dir.
    let wayland_display = env::var_os("WAYLAND_DISPLAY").map(|s| s.to_string_lossy().into_owned());
    let wayland_display = wayland_display.or_else(|| {
        (0..4)
            .map(|n| format!("wayland-{}", n))
            .find(|name| runtime_dir.join(name).exists())
            .inspect(|name| {
                cmd.env("WAYLAND_DISPLAY", name);
            })
    });

    let have_x11 = env::var_os("DISPLAY").is_some();

    if wayland_display.is_some() {
        // Prefer native Wayland, fall back to X11 if the Qt Wayland plugin is unavailable.
        if env::var_os("QT_QPA_PLATFORM").is_none() {
            cmd.env("QT_QPA_PLATFORM", "wayland;xcb");
        }
    } else if !have_x11 {
        // No Wayland socket and no X11: last-resort default for Xorg/XWayland.
        cmd.env("DISPLAY", ":0");
    }

    // kdialog/zenity and notify-send need the user session bus.
    if env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        let bus = runtime_dir.join("bus");
        if bus.exists() {
            cmd.env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", bus.display()),
            );
        }
    }
}

/// Show a text-entry dialog for "Enter new activity..."; returns the typed activity or None.
#[cfg(target_os = "linux")]
fn prompt_enter_activity_linux(backend: LinuxDialog) -> Option<String> {
    let mut cmd = Command::new(match backend {
        LinuxDialog::KDialog => "kdialog",
        LinuxDialog::Zenity => "zenity",
    });
    match backend {
        LinuxDialog::KDialog => {
            cmd.args(["--title", "timesheet", "--inputbox", "Enter activity:"]);
        }
        LinuxDialog::Zenity => {
            cmd.args(["--entry", "--title=timesheet", "--text=Enter activity:"]);
        }
    }
    linux_with_display(&mut cmd);
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let out = wait_no_timeout(child)?;
    let activity = String::from_utf8_lossy(&out).trim().to_string();
    if activity.is_empty() {
        None
    } else {
        Some(activity)
    }
}

/// Single-click chooser implemented with PyQt (Qt, native to KDE). Each entry acts on a single
/// click with no OK/Cancel buttons: clicking "Stop Work" / an activity returns it, and clicking
/// "Enter new activity..." opens an input box in the same window (a non-empty entry returns it; a
/// blank entry returns to the list). The script writes the chosen string to stdout, or nothing if
/// the window is dismissed. It exits 3 when no Qt toolkit is available so the caller can fall back.
///
/// The window covers the whole screen and stays on top, matching the macOS chooser: a small window
/// is easy to dismiss by accident when it appears mid-click. The choices sit in a centered panel.
/// Qt enum access differs between PyQt5 (unscoped) and PyQt6 (scoped), hence the `WindowType`
/// getattr dance.
#[cfg(target_os = "linux")]
const REMINDER_CHOOSER_PY: &str = r#"
import sys, os
choices = sys.argv[1:]
def load_qt():
    for mod in ("PyQt6", "PyQt5"):
        try:
            w = __import__(mod + ".QtWidgets", fromlist=["x"])
            c = __import__(mod + ".QtCore", fromlist=["x"])
            return (w.QApplication, w.QWidget, w.QVBoxLayout, w.QHBoxLayout, w.QListWidget,
                    w.QLabel, w.QInputDialog, c.QTimer, c.Qt)
        except Exception:
            continue
    return None
bundle = load_qt()
if bundle is None:
    sys.exit(3)
QApplication, QWidget, QVBoxLayout, QHBoxLayout, QListWidget, QLabel, QInputDialog, QTimer, Qt = bundle
wintype = getattr(Qt, "WindowType", Qt)
align = getattr(Qt, "AlignmentFlag", Qt)
result = {"v": None}
app = QApplication([])
w = QWidget()
w.setWindowTitle("timesheet")
try:
    w.setWindowFlags(w.windowFlags() | wintype.WindowStaysOnTopHint)
except Exception:
    pass
prompt = QLabel("What are you working on?")
try:
    prompt.setAlignment(align.AlignCenter)
except Exception:
    pass
# Lay the choices out in columns instead of one tall scrolling list: with the
# window full-screen, a grid uses the available space instead of forcing a
# scrollbar. Column count is derived from screen height so the grid fits
# without scrolling for any realistic number of choices; filled column-major
# so the first choice ("Stop Work") lands top-of-first-column and the last
# ("Enter new activity...") lands bottom-of-last-column.
item_h = 28
screen = app.primaryScreen()
avail_h = (screen.availableGeometry().height() if screen else 800) - 220
max_rows = max(1, avail_h // item_h)
n = len(choices)
columns = max(1, -(-n // max_rows))  # ceil division
rows_per_col = -(-n // columns)  # ceil division
grid = QHBoxLayout()
lists = []
idx = 0
for col in range(columns):
    count = min(rows_per_col, n - idx)
    lst = QListWidget()
    lst.addItems(choices[idx:idx + count])
    lst.setFixedWidth(420)
    lst.setFixedHeight(count * item_h + 20)
    lists.append((lst, idx))
    grid.addWidget(lst)
    idx += count
# Centered panel: the window is full-screen, but the choices stay a comfortable size.
panel = QWidget()
panel_lay = QVBoxLayout(panel)
panel_lay.addWidget(prompt)
panel_lay.addLayout(grid)
row = QHBoxLayout()
row.addStretch(1)
row.addWidget(panel)
row.addStretch(1)
lay = QVBoxLayout(w)
lay.addStretch(1)
lay.addLayout(row)
lay.addStretch(1)
def finish(val):
    result["v"] = val
    app.quit()
def on_click(item):
    text = item.text()
    if text == "Enter new activity...":
        activity, ok = QInputDialog.getText(w, "timesheet", "Enter activity:")
        if ok and activity.strip():
            finish(activity.strip())
        else:
            item.listWidget().clearSelection()
        return
    finish(text)
for lst, _ in lists:
    lst.itemClicked.connect(on_click)
w.showFullScreen()
try:
    w.raise_()
    w.activateWindow()
except Exception:
    pass
autopick = os.environ.get("TS_CHOOSER_AUTOPICK")
if autopick is not None:
    ai = int(autopick)
    for lst, base in lists:
        if base <= ai < base + lst.count():
            QTimer.singleShot(200, lambda lst=lst, i=ai - base: on_click(lst.item(i)))
            break
run = getattr(app, "exec", None) or getattr(app, "exec_")
run()
if result["v"] is not None:
    sys.stdout.write(result["v"])
    sys.stdout.flush()
"#;

/// Try the PyQt single-click chooser. Returns `None` if Python/PyQt is unavailable (so the caller
/// falls back to the kdialog/zenity list dialog); otherwise returns the mapped ReminderResult.
#[cfg(target_os = "linux")]
fn show_reminder_prompt_pyqt(
    choices: &[String],
    reminder_appeared: DateTime<Local>,
    timesheet: Option<&Path>,
) -> Option<ReminderResult> {
    if !command_on_path("python3") {
        return None;
    }
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(REMINDER_CHOOSER_PY);
    for c in choices {
        cmd.arg(c);
    }
    linux_with_display(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Wait for an answer with no deadline, collecting both exit code and stdout. If one reminder
    // interval passes unanswered, record the STOP at `reminder_appeared` but leave the window up:
    // whenever you get back and pick an activity, that START lands at your return time and the
    // interval you were away is left unbilled.
    let interval = Duration::from_secs(get_reminder_interval_secs());
    let start = std::time::Instant::now();
    let mut appended_stop = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.code() == Some(3) {
                    // No Qt toolkit available: let the caller fall back to kdialog/zenity.
                    return None;
                }
                let mut out = Vec::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = io::copy(&mut s, &mut out);
                }
                let s = String::from_utf8_lossy(&out).trim().to_string();
                return Some(match parse_native_reminder_dialog_output(&s) {
                    // "Enter new activity..." is handled inside the script, so it never reaches here.
                    Some(result) => result,
                    None => ReminderResult::ShowAgainImmediate, // dismissed without a choice
                });
            }
            Ok(None) => {}
            Err(_) => return None,
        }
        // `timesheet stop` while this prompt is up: close the window and go. The daemon is a stray if it
        // reaches this (the one named in the PID file is signaled directly, and the signal takes
        // its chooser down with it), so there is nothing left for it to do but exit.
        if reminder_daemon_disowned() {
            ts_debug("reminder: disowned with a prompt on screen; closing it and exiting");
            let _ = child.kill();
            let _ = child.wait();
            process::exit(0);
        }
        if !appended_stop && start.elapsed() >= interval {
            if let Some(ts) = timesheet {
                let _ = append_reminder_timeout_stop(ts, reminder_appeared);
            }
            appended_stop = true;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Linux reminder prompt: present the activity chooser and map the choice to a ReminderResult.
/// Prefers the PyQt single-click chooser (no OK/Cancel); falls back to a kdialog/zenity list dialog.
#[cfg(target_os = "linux")]
fn show_reminder_prompt_linux(activities: &[String], timesheet: Option<&Path>) -> ReminderResult {
    let reminder_appeared = Local::now();
    let choices = reminder_choices(activities);

    if let Some(result) = show_reminder_prompt_pyqt(&choices, reminder_appeared, timesheet) {
        return result;
    }

    let backend = match detect_linux_dialog() {
        Some(b) => b,
        None => {
            // No GUI dialog helper installed: behave like a timed-out reminder (records STOP, re-shows later).
            ts_debug("reminder: no kdialog/zenity found; install one for interactive reminders");
            return ReminderResult::TimeoutAddStop(reminder_appeared);
        }
    };

    let mut cmd = Command::new(match backend {
        LinuxDialog::KDialog => "kdialog",
        LinuxDialog::Zenity => "zenity",
    });
    match backend {
        LinuxDialog::KDialog => {
            cmd.args(["--title", "timesheet", "--menu", "What are you working on?"]);
            // kdialog --menu takes (tag, label) pairs; selected tag is printed to stdout.
            for c in &choices {
                cmd.arg(c).arg(c);
            }
        }
        LinuxDialog::Zenity => {
            cmd.args([
                "--list",
                "--title=timesheet",
                "--text=What are you working on?",
                "--hide-header",
                "--column=Activity",
            ]);
            for c in &choices {
                cmd.arg(c);
            }
        }
    }
    linux_with_display(&mut cmd);

    let child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return ReminderResult::TimeoutAddStop(reminder_appeared),
    };

    // Wait indefinitely, recording the STOP once one reminder interval passes unanswered and
    // leaving the dialog up so a later choice starts a new session at the time you return.
    let interval = Duration::from_secs(get_reminder_interval_secs());
    let mut child = child;
    let mut appended_stop = false;
    let stdout = loop {
        match wait_with_timeout(child, interval, false) {
            WaitOutcome::Finished(out) => break out,
            // kill_on_timeout is false, so the dialog is handed back still running.
            WaitOutcome::TimedOut => break None,
            WaitOutcome::TimedOutWithChild(c) => {
                if !appended_stop {
                    if let Some(ts) = timesheet {
                        let _ = append_reminder_timeout_stop(ts, reminder_appeared);
                    }
                    appended_stop = true;
                }
                child = c;
            }
        }
    };
    match stdout {
        Some(stdout) => {
            let s = String::from_utf8_lossy(&stdout).trim().to_string();
            match parse_native_reminder_dialog_output(&s) {
                Some(ReminderResult::EnterNew) => {
                    if let Some(activity) = prompt_enter_activity_linux(backend) {
                        ReminderResult::Activity(activity)
                    } else {
                        ReminderResult::ShowAgainImmediate
                    }
                }
                Some(result) => result,
                // Cancelled or dismissed without a choice: re-show immediately.
                None => ReminderResult::ShowAgainImmediate,
            }
        }
        None => ReminderResult::TimeoutAddStop(reminder_appeared),
    }
}

/// PowerShell's `-EncodedCommand` expects the script as base64-encoded UTF-16LE; this sidesteps
/// every shell-quoting hazard a multi-line WinForms script would otherwise hit.
#[cfg(target_os = "windows")]
fn encode_powershell_command(script: &str) -> String {
    let mut utf16le = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        utf16le.push(unit as u8);
        utf16le.push((unit >> 8) as u8);
    }
    base64_encode(&utf16le)
}

#[cfg(target_os = "windows")]
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(ALPHABET[(n >> 18 & 0x3F) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Full-screen, topmost, single-click chooser implemented with PowerShell/WinForms (mirrors the
/// Linux PyQt chooser's feel more than kdialog/zenity's list+OK). Picking "Enter new activity..."
/// opens a small inline input dialog in the same script (like the PyQt chooser does), so the whole
/// interaction is one process and one round trip. Writes the chosen string to stdout, or nothing
/// if the window is dismissed without a choice.
/// `{CHOICES}` is replaced with a PowerShell array literal before encoding. Choices are embedded
/// directly in the script rather than passed as trailing argv, because -EncodedCommand does not
/// support trailing positional arguments the way -Command does (passing them anyway makes
/// powershell.exe silently fall back to printing its own usage text instead of running the
/// script).
#[cfg(target_os = "windows")]
const REMINDER_CHOOSER_PS1: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$choices = @({CHOICES})
$script:result = $null

function Prompt-NewActivity {
    $dlg = New-Object System.Windows.Forms.Form
    $dlg.Text = "timesheet"
    $dlg.StartPosition = "CenterScreen"
    $dlg.TopMost = $true
    $dlg.Width = 400
    $dlg.Height = 150
    $dlg.FormBorderStyle = "FixedDialog"
    $dlg.MinimizeBox = $false
    $dlg.MaximizeBox = $false
    $lbl = New-Object System.Windows.Forms.Label
    $lbl.Text = "Enter activity:"
    $lbl.Location = New-Object System.Drawing.Point(10, 10)
    $lbl.AutoSize = $true
    $txt = New-Object System.Windows.Forms.TextBox
    $txt.Location = New-Object System.Drawing.Point(10, 35)
    $txt.Width = 360
    $ok = New-Object System.Windows.Forms.Button
    $ok.Text = "OK"
    $ok.Location = New-Object System.Drawing.Point(210, 70)
    $ok.DialogResult = [System.Windows.Forms.DialogResult]::OK
    $cancel = New-Object System.Windows.Forms.Button
    $cancel.Text = "Cancel"
    $cancel.Location = New-Object System.Drawing.Point(295, 70)
    $cancel.DialogResult = [System.Windows.Forms.DialogResult]::Cancel
    $dlg.Controls.AddRange(@($lbl, $txt, $ok, $cancel))
    $dlg.AcceptButton = $ok
    $dlg.CancelButton = $cancel
    $r = $dlg.ShowDialog()
    if ($r -eq [System.Windows.Forms.DialogResult]::OK -and $txt.Text.Trim().Length -gt 0) {
        return $txt.Text.Trim()
    }
    return $null
}

$form = New-Object System.Windows.Forms.Form
$form.Text = "timesheet"
$form.FormBorderStyle = "None"
$form.WindowState = "Maximized"
$form.TopMost = $true
$form.BackColor = [System.Drawing.Color]::FromArgb(30, 30, 30)
$form.KeyPreview = $true
$form.Add_KeyDown({ if ($_.KeyCode -eq "Escape") { $form.Close() } })

$label = New-Object System.Windows.Forms.Label
$label.Text = "What are you working on?"
$label.ForeColor = [System.Drawing.Color]::White
$label.Font = New-Object System.Drawing.Font("Segoe UI", 16)
$label.AutoSize = $true
$label.Location = New-Object System.Drawing.Point(10, 10)

# Lay the choices out in columns instead of one tall scrolling list: with the
# window full-screen (borderless + maximized above), a grid uses the
# available space instead of forcing a scrollbar. Column count is derived
# from screen height so the grid fits without scrolling for any realistic
# number of choices; filled column-major so the first choice ("Stop Work")
# lands top-of-first-column and the last ("Enter new activity...") lands
# bottom-of-last-column.
$itemHeight = 28
$screenHeight = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea.Height
$maxRows = [Math]::Max(1, [Math]::Floor(($screenHeight - 220) / $itemHeight))
$n = $choices.Count
$columns = [Math]::Max(1, [Math]::Ceiling($n / $maxRows))
$rowsPerCol = [Math]::Ceiling($n / $columns)
$colWidth = 420
$colSpacing = 20

$listBoxes = @()
$idx = 0
for ($col = 0; $col -lt $columns; $col++) {
    $count = [Math]::Min($rowsPerCol, $n - $idx)
    $lb = New-Object System.Windows.Forms.ListBox
    $lb.Font = New-Object System.Drawing.Font("Segoe UI", 12)
    $lb.Width = $colWidth
    $lb.Height = ($count * $itemHeight) + 20
    $lb.Location = New-Object System.Drawing.Point((10 + $col * ($colWidth + $colSpacing)), ($label.Bottom + 10))
    for ($j = 0; $j -lt $count; $j++) { [void]$lb.Items.Add($choices[$idx + $j]) }
    $listBoxes += $lb
    $idx += $count
}
$maxListHeight = ($listBoxes | ForEach-Object { $_.Height } | Measure-Object -Maximum).Maximum

$panel = New-Object System.Windows.Forms.Panel
$panel.Width = 20 + ($columns * $colWidth) + (($columns - 1) * $colSpacing)
$panel.Height = $label.Bottom + 10 + $maxListHeight + 10
$panel.Controls.Add($label)
foreach ($lb in $listBoxes) { $panel.Controls.Add($lb) }
$form.Controls.Add($panel)
$form.Add_Shown({
    $panel.Left = [int](($form.ClientSize.Width - $panel.Width) / 2)
    $panel.Top = [int](($form.ClientSize.Height - $panel.Height) / 2)
    $form.Activate()
})

# $this is bound to whichever ListBox raised the event, so one handler
# shared across all columns is enough.
$onListClick = {
    if ($this.SelectedItem -eq $null) { return }
    $item = $this.SelectedItem.ToString()
    if ($item -eq "Enter new activity...") {
        $activity = Prompt-NewActivity
        if ($activity) {
            $script:result = $activity
            $form.Close()
        } else {
            $this.ClearSelected()
        }
        return
    }
    $script:result = $item
    $form.Close()
}
foreach ($lb in $listBoxes) { $lb.Add_Click($onListClick) }

[void]$form.ShowDialog()
if ($script:result) {
    Write-Output $script:result
}
"#;

#[cfg(target_os = "windows")]
fn show_reminder_prompt_windows(activities: &[String], timesheet: Option<&Path>) -> ReminderResult {
    let reminder_appeared = Local::now();
    let choices = reminder_choices(activities);
    let ps_quote = |s: &str| s.replace('\'', "''");
    let choices_literal = choices
        .iter()
        .map(|c| format!("'{}'", ps_quote(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let script = REMINDER_CHOOSER_PS1.replace("{CHOICES}", &choices_literal);
    let encoded = encode_powershell_command(&script);

    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Sta",
        "-WindowStyle",
        "Hidden",
        "-EncodedCommand",
        &encoded,
    ]);

    let child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return ReminderResult::TimeoutAddStop(reminder_appeared),
    };

    // Same leave-it-on-screen-and-record-STOP-once behavior as the Linux dialog: wait indefinitely,
    // recording the STOP once one reminder interval passes unanswered.
    let interval = Duration::from_secs(get_reminder_interval_secs());
    let mut child = child;
    let mut appended_stop = false;
    let stdout = loop {
        match wait_with_timeout(child, interval, false) {
            WaitOutcome::Finished(out) => break out,
            WaitOutcome::TimedOut => break None,
            WaitOutcome::TimedOutWithChild(c) => {
                if !appended_stop {
                    if let Some(ts) = timesheet {
                        let _ = append_reminder_timeout_stop(ts, reminder_appeared);
                    }
                    appended_stop = true;
                }
                child = c;
            }
        }
    };
    match stdout {
        Some(stdout) => {
            let s = String::from_utf8_lossy(&stdout).trim().to_string();
            match parse_native_reminder_dialog_output(&s) {
                Some(result) => result,
                // Cancelled or dismissed without a choice: re-show immediately.
                None => ReminderResult::ShowAgainImmediate,
            }
        }
        None => ReminderResult::TimeoutAddStop(reminder_appeared),
    }
}

/// Run osascript "Enter activity:" text dialog in user session; returns the entered string or None.
#[cfg(target_os = "macos")]
fn prompt_enter_activity_macos(ts_debug: bool) -> Option<String> {
    // Return only the text so stdout is just the activity (no parsing "button returned:OK, text returned:...").
    let prompt_script = "text returned of (display dialog \"Enter activity:\" with title \"timesheet\" default answer \"\")";
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
            .stderr(if ts_debug {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .ok()?;
        let out = wait_no_timeout(child)?;
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
    // Same reason as on Linux: a dialog that inherited the daemon's blocked SIGTERM could not be
    // closed by `timesheet stop`.
    reset_child_signal_mask(&mut c);
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

    // Native Rust/AppKit dialog (many buttons, one click). Spawn timesheet --reminder-dialog in user's GUI session.
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
            .stderr(if ts_debug {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return NativeOutcome::Unavailable,
        };
        // Bring the chooser forward as soon as it launches; otherwise `timesheet start`
        // can sit behind Terminal until the first reminder timeout elapses.
        macos_bring_reminder_window_to_front(child.id());
        let appeared = Local::now();
        let timeout = Duration::from_secs(get_reminder_interval_secs());
        let mut appended_stop_for_this_reminder = false;
        loop {
            match wait_with_timeout(child, timeout, false) {
                WaitOutcome::Finished(Some(out)) => {
                    let s = String::from_utf8_lossy(&out).trim().to_string();
                    if let Some(result) = parse_native_reminder_dialog_output(&s) {
                        return NativeOutcome::Result(result);
                    }
                    return NativeOutcome::Dismissed;
                }
                WaitOutcome::Finished(None) => return NativeOutcome::Unavailable,
                WaitOutcome::TimedOut => return NativeOutcome::Unavailable,
                WaitOutcome::TimedOutWithChild(c) => {
                    if !appended_stop_for_this_reminder {
                        if let Some(ts) = timesheet {
                            let _ = append_reminder_timeout_stop(ts, appeared);
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
            ReminderResult::ShowAgainImmediate
        } else {
            res
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
            "timesheet: native reminder dialog failed or timed out, using SystemUIServer fallback\n"
        ));
    }

    // SystemUIServer can show dialogs from background processes (daemon). Try it first (with list of activities).
    match show_reminder_prompt_macos_systemui(&choices, reminder_appeared) {
        ReminderResult::DontBugMe => return ReminderResult::DontBugMe,
        ReminderResult::Activity(ref a) if !a.is_empty() => {
            return ReminderResult::Activity(a.clone())
        }
        ReminderResult::TimeoutAddStop(epoch) => {
            if let Some(ts) = timesheet {
                let _ = append_reminder_timeout_stop(ts, epoch);
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
        "choose from list {{{}}} with title \"timesheet\" with prompt \"What are you working on?\" default items {{item 1 of {{{}}}}}",
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

    let timeout = Duration::from_secs(get_reminder_interval_secs());
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
fn show_reminder_prompt_macos_systemui(
    choices: &[String],
    reminder_appeared: DateTime<Local>,
) -> ReminderResult {
    let stderr_mode = if env::var_os("TS_DEBUG").is_some() {
        Stdio::inherit()
    } else {
        Stdio::null()
    };
    let timeout_dur = Duration::from_secs(get_reminder_interval_secs());

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
        "tell application \"SystemUIServer\" to display dialog \"What are you working on?\" with title \"timesheet\" buttons {{{}}} default button \"Stop Work\"",
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
            "tell application \"SystemUIServer\" to choose from list {{{}}} with title \"timesheet\" with prompt \"What are you working on?\" default items {{item 1 of {{{}}}}}",
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
    let script = "tell application \"SystemUIServer\" to display dialog \"What are you working on?\" default answer \"\" with title \"timesheet\" buttons {\"Stop Work\", \"OK\"} default button \"OK\"";
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
    if let WaitOutcome::Finished(Some(stdout)) = wait_with_timeout(child, timeout_dur, true) {
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
    ReminderResult::TimeoutAddStop(reminder_appeared)
}

#[cfg(target_os = "macos")]
fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Wait for process to finish, or until timeout. Returns stdout if process exited normally.
/// When kill_on_timeout is false and we time out, returns TimedOutWithChild so the caller can bring the window to front and wait again.
enum WaitOutcome {
    Finished(Option<Vec<u8>>),
    TimedOut,
    /// Child still running (not killed); caller can bring the window to front and call
    /// wait_with_timeout again. Used by the reminder prompts to record a STOP after one unanswered
    /// interval while leaving the dialog on screen (macOS also re-fronts the held child).
    TimedOutWithChild(process::Child),
}

/// Wait for process to finish, or until timeout. If kill_on_timeout is false, the child is left running (not dismissed).
fn wait_with_timeout(
    mut child: process::Child,
    timeout: Duration,
    kill_on_timeout: bool,
) -> WaitOutcome {
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
        let _ = std::io::stderr().write_all(b"timesheet: main entered\n");
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
        let _ =
            std::io::stderr().write_fmt(format_args!("timesheet: dispatching to {:?}\n", cmd_name));
    }

    let result = match cmd.as_deref() {
        None => cmd_help(),
        Some("start") => cmd_start(&rest, &timesheet),
        Some("stop") => cmd_stop(&rest, &timesheet),
        Some("stopped") => cmd_stop(&rest, &timesheet),
        Some("list") => cmd_list(&rest, &timesheet),
        Some("edit") => cmd_edit(&timesheet),
        Some("sprint") => cmd_sprint(&timesheet),
        Some("tail") => cmd_tail(rest.first().map(String::as_str), &timesheet),
        Some("started") => cmd_started(&rest, &timesheet),
        Some("timeoff") => cmd_timeoff(&timesheet),
        Some("alias") => cmd_workalias(&rest, &timesheet),
        Some("rename") => cmd_workalias(&rest, &timesheet),
        Some("prefix") => cmd_prefix(&rest, &timesheet),
        Some("install") => cmd_install(&rest),
        Some("uninstall") => cmd_uninstall(&rest),
        Some("rebuild") => cmd_rebuild(&rest),
        Some("rotate") => do_rotate(&timesheet),
        Some("migrate") => cmd_migrate(&timesheet),
        Some("pdf") => report::cmd_pdf(&rest, &timesheet),
        Some("email") => report::cmd_email(&rest, &timesheet),
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
        format_log_timestamp(Local.timestamp_opt(epoch, 0).single().unwrap())
    }

    #[test]
    fn test_paths_refer_to_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ts");
        let other = dir.path().join("other");
        fs::write(&path, "bin").unwrap();
        fs::write(&other, "other").unwrap();

        assert!(paths_refer_to_same_file(&path, &path));
        assert!(!paths_refer_to_same_file(&path, &other));
    }

    #[test]
    fn test_help_prelude_starts_with_canonical_source_url() {
        assert_eq!(help_prelude(), format!("{}\n\n", CANONICAL_SOURCE_URL));
    }

    #[test]
    fn test_parse_line_start() {
        let line = "2023-11-14T22:13:20-05:00|START|coding";
        let parsed = parse_line(line);
        if let Some(LogLine::Start(dt, a)) = parsed {
            assert_eq!(
                dt.naive_local(),
                chrono::NaiveDateTime::parse_from_str("2023-11-14T22:13:20", "%Y-%m-%dT%H:%M:%S")
                    .unwrap()
            );
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
            assert_eq!(
                dt.naive_local(),
                chrono::NaiveDateTime::parse_from_str("2023-11-14T22:13:20", "%Y-%m-%dT%H:%M:%S")
                    .unwrap()
            );
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
            assert_eq!(
                dt.naive_local(),
                chrono::NaiveDateTime::parse_from_str("2023-11-14T22:13:20", "%Y-%m-%dT%H:%M:%S")
                    .unwrap()
            );
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
            assert_eq!(
                dt.naive_local(),
                chrono::NaiveDateTime::parse_from_str("2023-11-14T23:13:20", "%Y-%m-%dT%H:%M:%S")
                    .unwrap()
            );
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
            assert_eq!(
                dt.naive_local(),
                chrono::NaiveDateTime::parse_from_str("2026-03-06T14:30:00", "%Y-%m-%dT%H:%M:%S")
                    .unwrap()
            );
        } else {
            panic!("expected Some(Start)");
        }
        let line_stop = "2026-03-06T18:45:00-08:00|STOP";
        if let Some(LogLine::Stop(dt)) = parse_line(line_stop) {
            assert_eq!(
                dt.naive_local(),
                chrono::NaiveDateTime::parse_from_str("2026-03-06T18:45:00", "%Y-%m-%dT%H:%M:%S")
                    .unwrap()
            );
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
            assert_eq!(
                dt.naive_local(),
                chrono::NaiveDateTime::parse_from_str("2023-11-14T22:13:20", "%Y-%m-%dT%H:%M:%S")
                    .unwrap()
            );
            assert_eq!(activity, "  x");
        } else {
            panic!("expected Some(Start)");
        }
    }

    #[test]
    fn test_week_start() {
        // Tuesday 2023-11-14 12:00 local -> week start is Sunday 2023-11-12 00:00 local.
        let tuesday = local_datetime_at(
            NaiveDate::from_ymd_opt(2023, 11, 14).unwrap(),
            NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        );
        let week_start_dt = week_start_with(tuesday, RotationBoundary::default());
        assert_eq!(week_start_dt.weekday(), chrono::Weekday::Sun);
        assert_eq!(week_start_dt.hour(), 0);
        assert_eq!(week_start_dt.minute(), 0);
        assert!(week_start_dt <= tuesday);
    }

    #[test]
    fn test_week_start_with_monday_boundary() {
        let boundary = RotationBoundary {
            day: Weekday::Mon,
            time: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        };
        // Tuesday 2023-11-14 12:00 local -> Monday 2023-11-13 00:00 local.
        let tuesday = local_datetime_at(
            NaiveDate::from_ymd_opt(2023, 11, 14).unwrap(),
            NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        );
        let start = week_start_with(tuesday, boundary);
        assert_eq!(start.weekday(), Weekday::Mon);
        assert_eq!((start.hour(), start.minute()), (0, 0));
        assert_eq!(tuesday.signed_duration_since(start).num_days(), 1);

        // Sunday belongs to the week that began the previous Monday, not the coming one.
        let sunday = tuesday - chrono::Duration::days(2);
        assert_eq!(sunday.weekday(), Weekday::Sun);
        let start = week_start_with(sunday, boundary);
        assert_eq!(start.weekday(), Weekday::Mon);
        assert!(start < sunday);
        assert_eq!(sunday.signed_duration_since(start).num_days(), 6);
    }

    #[test]
    fn test_week_start_before_boundary_time_uses_previous_week() {
        let boundary = RotationBoundary {
            day: Weekday::Mon,
            time: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        };
        // Monday 08:00 local is still the previous week when the boundary is Monday 09:00.
        let monday_8am = local_datetime_at(
            NaiveDate::from_ymd_opt(2023, 11, 13).unwrap(),
            NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
        );
        let start = week_start_with(monday_8am, boundary);
        assert_eq!(start.weekday(), Weekday::Mon);
        assert_eq!(start.hour(), 9);
        assert_eq!(monday_8am.signed_duration_since(start).num_days(), 6);

        // One hour later, the new week has begun.
        let monday_10am = monday_8am + chrono::Duration::hours(2);
        let start = week_start_with(monday_10am, boundary);
        assert_eq!(start.hour(), 9);
        assert_eq!(monday_10am.signed_duration_since(start).num_hours(), 1);
    }

    #[test]
    fn test_week_start_exactly_on_boundary() {
        let boundary = RotationBoundary::default();
        let sunday_midnight = local_datetime_at(
            NaiveDate::from_ymd_opt(2023, 11, 12).unwrap(),
            NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        );
        assert_eq!(week_start_with(sunday_midnight, boundary), sunday_midnight);
    }

    #[test]
    fn test_parse_simple_yaml_nested_and_comments() {
        let text = "\
# rotation settings
rotate:
  day: Monday   # start of the work week
  time: \"00:00\"
other: value
";
        let map = parse_simple_yaml(text);
        assert_eq!(map.get("rotate.day").map(String::as_str), Some("Monday"));
        assert_eq!(map.get("rotate.time").map(String::as_str), Some("00:00"));
        assert_eq!(map.get("other").map(String::as_str), Some("value"));
        assert!(!map.contains_key("rotate"));
    }

    #[test]
    fn test_parse_simple_yaml_ignores_hash_inside_quotes() {
        let map = parse_simple_yaml("note: \"a # b\"\n");
        assert_eq!(map.get("note").map(String::as_str), Some("a # b"));
    }

    #[test]
    fn test_parse_weekday_and_time_of_day() {
        assert_eq!(parse_weekday("monday"), Some(Weekday::Mon));
        assert_eq!(parse_weekday("MON"), Some(Weekday::Mon));
        assert_eq!(parse_weekday(" Sunday "), Some(Weekday::Sun));
        assert_eq!(parse_weekday("funday"), None);
        assert_eq!(parse_time_of_day("9"), NaiveTime::from_hms_opt(9, 0, 0));
        assert_eq!(
            parse_time_of_day("09:30"),
            NaiveTime::from_hms_opt(9, 30, 0)
        );
        assert_eq!(
            parse_time_of_day("23:59:59"),
            NaiveTime::from_hms_opt(23, 59, 59)
        );
        assert_eq!(parse_time_of_day("noon"), None);
        assert_eq!(parse_time_of_day("25:00"), None);
        assert_eq!(parse_time_of_day("7am"), NaiveTime::from_hms_opt(7, 0, 0));
        assert_eq!(
            parse_time_of_day("7:30 PM"),
            NaiveTime::from_hms_opt(19, 30, 0)
        );
    }

    #[test]
    fn test_rotation_boundary_from_config_forms() {
        let nested = parse_simple_yaml("rotate:\n  day: monday\n  time: 09:30\n");
        let (boundary, warnings) = rotation_boundary_from_config(&nested);
        assert!(warnings.is_empty());
        assert_eq!(boundary.day, Weekday::Mon);
        assert_eq!(boundary.time, NaiveTime::from_hms_opt(9, 30, 0).unwrap());

        let scalar = parse_simple_yaml("rotate: monday\n");
        let (boundary, warnings) = rotation_boundary_from_config(&scalar);
        assert!(warnings.is_empty());
        assert_eq!(boundary.day, Weekday::Mon);
        assert_eq!(boundary.time, NaiveTime::from_hms_opt(0, 0, 0).unwrap());

        let scalar_with_time = parse_simple_yaml("rotate: \"fri 17:00\"\n");
        let (boundary, warnings) = rotation_boundary_from_config(&scalar_with_time);
        assert!(warnings.is_empty());
        assert_eq!(boundary.day, Weekday::Fri);
        assert_eq!(boundary.time, NaiveTime::from_hms_opt(17, 0, 0).unwrap());

        // Only `day` given: time keeps the midnight default.
        let day_only = parse_simple_yaml("rotate:\n  day: wed\n");
        let (boundary, _) = rotation_boundary_from_config(&day_only);
        assert_eq!(boundary.day, Weekday::Wed);
        assert_eq!(boundary.time, NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    }

    #[test]
    fn test_rotation_boundary_from_config_invalid_values_warn_and_default() {
        let map = parse_simple_yaml("rotate:\n  day: funday\n  time: half past ten\n");
        let (boundary, warnings) = rotation_boundary_from_config(&map);
        assert_eq!(boundary, RotationBoundary::default());
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|w| w.contains("funday")));
    }

    #[test]
    fn test_load_rotation_boundary_missing_and_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timesheet.yml");
        assert_eq!(load_rotation_boundary(&path), RotationBoundary::default());

        fs::write(&path, "rotate:\n  day: monday\n").unwrap();
        assert_eq!(
            load_rotation_boundary(&path),
            RotationBoundary {
                day: Weekday::Mon,
                time: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            }
        );
    }

    #[test]
    fn test_maybe_rotate_respects_configured_boundary() {
        // Sunday-boundary weeks and Monday-boundary weeks disagree about Sunday: an entry made
        // on Sunday is "this week" under the default and "last week" once rotation moves to Monday.
        let sunday = local_datetime_at(
            NaiveDate::from_ymd_opt(2023, 11, 12).unwrap(),
            NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        );
        let monday = sunday + chrono::Duration::days(1);
        let monday_boundary = RotationBoundary {
            day: Weekday::Mon,
            time: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        };
        assert!(sunday >= week_start_with(monday, RotationBoundary::default()));
        assert!(sunday < week_start_with(monday, monday_boundary));
    }

    #[test]
    fn test_timesheet_path_uses_home() {
        let path = timesheet_path();
        assert!(
            path.ends_with("Documents/timesheet.log") || path.ends_with("Documents\\timesheet.log")
        );
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
    fn test_min_dt_in_log() {
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
        assert_eq!(min_dt_in_log(&path).map(|d| d.timestamp()), Some(100));
        fs::write(&path, "").unwrap();
        assert!(min_dt_in_log(&path).is_none());
        fs::write(&path, "comment\n").unwrap();
        assert!(min_dt_in_log(&path).is_none());
    }

    #[test]
    fn test_append_stop_entry_caps_to_reminder_interval_after_latest_log_entry() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let start_dt = Local::now() - chrono::Duration::hours(2);
        fs::write(
            &log_path,
            format!("{}\n", format_start_log_entry(start_dt, "coding")),
        )
        .unwrap();

        let result = append_stop_entry(&log_path, Local::now());

        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], format_start_log_entry(start_dt, "coding"));
        // The auto STOP is capped to one reminder interval after the open START (default 5 min).
        let cap = chrono::Duration::seconds(get_reminder_interval_secs() as i64);
        assert_eq!(lines[1], format_stop_log_entry(start_dt + cap));
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
                fmt_ts(1730086400)
            ),
        )
        .unwrap();
        let result = do_rotate(&log_path);
        assert!(result.is_ok());
        assert!(!log_path.exists());
        let stamp = chrono::Local
            .timestamp_opt(1730000000, 0)
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
    fn test_do_rotate_caps_auto_stop_to_reminder_interval_after_open_start() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let start_dt = Local::now() - chrono::Duration::hours(2);
        fs::write(
            &log_path,
            format!("{}\n", format_start_log_entry(start_dt, "coding")),
        )
        .unwrap();

        let result = do_rotate(&log_path);

        assert!(result.is_ok());
        let stop_dt = start_dt + chrono::Duration::seconds(get_reminder_interval_secs() as i64);
        let stamp = stop_dt.format("%y%m%d").to_string();
        let rotated = dir.path().join(format!("timesheet.{}", stamp));
        let content = fs::read_to_string(&rotated).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], format_start_log_entry(start_dt, "coding"));
        assert_eq!(lines[1], format_stop_log_entry(stop_dt));
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
    fn test_resolve_list_input_negative_one_returns_latest_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let older = dir.path().join("timesheet.260220");
        let newer = dir.path().join("timesheet.260227");
        fs::File::create(&older).unwrap();
        fs::File::create(&newer).unwrap();

        let out = resolve_list_input(Some("-1"), &log_path).unwrap();

        assert_eq!(out, newer);
    }

    #[test]
    fn test_resolve_list_input_negative_two_returns_previous_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let oldest = dir.path().join("timesheet.260213");
        let older = dir.path().join("timesheet.260220");
        let newer = dir.path().join("timesheet.260227");
        fs::File::create(&oldest).unwrap();
        fs::File::create(&older).unwrap();
        fs::File::create(&newer).unwrap();

        let out = resolve_list_input(Some("-2"), &log_path).unwrap();

        assert_eq!(out, older);
    }

    #[test]
    fn test_resolve_list_input_negative_out_of_range_errors() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let only = dir.path().join("timesheet.260227");
        fs::File::create(&only).unwrap();

        let result = resolve_list_input(Some("-2"), &log_path);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no timesheet matches"));
    }

    #[test]
    fn test_resolve_tail_input_negative_integer_still_errors() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let rotated = dir.path().join("timesheet.260227");
        fs::File::create(&rotated).unwrap();

        let result = resolve_tail_input(Some("-1"), &log_path);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no timesheet matches"));
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
        fs::write(
            &later,
            format!(
                "{}|START|a\n{}|STOP\n",
                format_log_timestamp(Local.with_ymd_and_hms(2025, 2, 19, 12, 0, 0).unwrap()),
                format_log_timestamp(Local.with_ymd_and_hms(2025, 3, 2, 0, 0, 0).unwrap()),
            ),
        )
        .unwrap();
        let out = resolve_list_input(Some("250219"), &log_path).unwrap();
        assert_eq!(
            out, later,
            "timesheet list 250219 should use log that contains that date"
        );
    }

    #[test]
    fn test_resolve_list_input_date_fallback_by_extension() {
        // Empty rotated files fall back to filename semantics: pick the latest file whose start date is on/before the requested date.
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let later = dir.path().join("timesheet.260220");
        fs::File::create(&later).unwrap(); // empty or no 2025-02-19 in content
        let out = resolve_list_input(Some("2/21"), &log_path).unwrap();
        assert_eq!(
            out, later,
            "timesheet list 2/21 should fall back to file with extension date on or before that day"
        );
    }

    #[test]
    fn test_latest_rotated_timesheet_returns_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        let older = dir.path().join("timesheet.260220");
        let newer = dir.path().join("timesheet.260227");
        fs::File::create(&older).unwrap();
        fs::File::create(&newer).unwrap();

        let out = latest_rotated_timesheet(&log_path).unwrap();

        assert_eq!(out, newer);
    }

    #[test]
    fn test_reminder_activities_use_current_and_latest_rotated_from_last_7_days() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let latest = dir.path().join("timesheet.260227");
        let older = dir.path().join("timesheet.260220");
        let now = Local::now();

        fs::write(
            &older,
            format!(
                "{}|START|ignored-older-file\n",
                format_log_timestamp(now - chrono::Duration::hours(1))
            ),
        )
        .unwrap();
        fs::write(
            &latest,
            format!(
                "{}|START|rotated\n{}|START|boundary\n{}|START|dup\n",
                format_log_timestamp(now - chrono::Duration::days(6)),
                format_log_timestamp(now - chrono::Duration::days(7)),
                format_log_timestamp(now - chrono::Duration::days(5)),
            ),
        )
        .unwrap();
        fs::write(
            &log_path,
            format!(
                "{}|START|current\n{}|START|dup\n{}|START|ignored-too-old\n",
                format_log_timestamp(now - chrono::Duration::hours(2)),
                format_log_timestamp(now - chrono::Duration::hours(1)),
                format_log_timestamp(now - chrono::Duration::days(8)),
            ),
        )
        .unwrap();

        let activities = reminder_activities_most_recent_first_at(&log_path, now);

        assert_eq!(activities, vec!["dup", "current", "rotated", "boundary"]);
    }

    #[test]
    fn test_reminder_activities_accept_legacy_current_and_rotated_logs() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let latest = dir.path().join("timesheet.260227");
        let now = Local::now();

        fs::write(
            &latest,
            format!(
                "START|{}|legacy-rotated\n",
                format_log_timestamp(now - chrono::Duration::days(2))
            ),
        )
        .unwrap();
        fs::write(
            &log_path,
            format!(
                "START|{}|legacy-current\nSTART|{}|legacy-dup\n",
                format_log_timestamp(now - chrono::Duration::hours(2)),
                format_log_timestamp(now - chrono::Duration::hours(1)),
            ),
        )
        .unwrap();

        let activities = reminder_activities_most_recent_first_at(&log_path, now);

        assert_eq!(
            activities,
            vec!["legacy-dup", "legacy-current", "legacy-rotated"]
        );
    }

    #[test]
    fn test_sprint_report_data_uses_current_and_latest_rotated_only() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let latest = dir.path().join("timesheet.260227");
        let older = dir.path().join("timesheet.260220");

        fs::write(
            &older,
            format!(
                "{}|START|older\n{}|STOP\n",
                fmt_ts(1730000000),
                fmt_ts(1730003600)
            ),
        )
        .unwrap();
        fs::write(
            &latest,
            format!(
                "{}|START|recent\n{}|STOP\n",
                fmt_ts(1730086400),
                fmt_ts(1730090000)
            ),
        )
        .unwrap();
        fs::write(
            &log_path,
            format!(
                "{}|START|current\n{}|STOP\n",
                fmt_ts(1730172800),
                fmt_ts(1730176400)
            ),
        )
        .unwrap();

        let (lines, current_task) = sprint_report_data(&log_path).unwrap();
        let (by_act, _, work_in_progress) = process_log_for_report(&lines, None);

        assert!(!work_in_progress);
        assert!(current_task.is_some());
        assert_eq!(by_act.len(), 2);
        assert!(by_act.iter().any(|(activity, _, _)| activity == "recent"));
        assert!(by_act.iter().any(|(activity, _, _)| activity == "current"));
        assert!(!by_act.iter().any(|(activity, _, _)| activity == "older"));
    }

    #[test]
    fn test_cmd_sprint_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_sprint(&log_path);
        assert!(result.is_ok());
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
    fn test_render_report_can_omit_day_totals() {
        let dt1 = Local.timestamp_opt(1000, 0).single().unwrap();
        let dt2 = Local.timestamp_opt(4600, 0).single().unwrap();
        let lines = vec![
            (1, LogLine::Start(dt1, "coding".to_string())),
            (2, LogLine::Stop(dt2)),
        ];

        let rendered = render_report(&lines, None, None, false);

        assert!(rendered.contains("100.0%  1.00h  coding"));
        assert!(!rendered.contains("Sunday"));
        assert!(!rendered.contains("Total  "));
    }

    #[test]
    fn list_options_and_week_selector_are_told_apart() {
        let parse = |args: &[&str]| {
            parse_list_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        let a = parse(&["-p", "ST"]).unwrap();
        assert_eq!(a.prefix.as_deref(), Some("ST"));
        assert_eq!(a.input, None);
        let a = parse(&["--prefix=ST", "-1"]).unwrap();
        assert_eq!(a.prefix.as_deref(), Some("ST"));
        assert_eq!(a.input.as_deref(), Some("-1"));
        // A rotated-log index stays a selector, and `--prefix ""` reports every entry.
        let a = parse(&["-2", "--prefix", ""]).unwrap();
        assert_eq!(a.prefix.as_deref(), Some(""));
        assert_eq!(a.input.as_deref(), Some("-2"));
        assert!(parse(&["-p"]).is_err());
        assert!(parse(&["-x"]).is_err());
        assert!(parse(&["log", "20250101"]).is_err());
        // `--` forces a positional, so a file really named `-p` can be selected.
        let a = parse(&["--", "-p"]).unwrap();
        assert_eq!(a.input.as_deref(), Some("-p"));
        assert_eq!(a.prefix, None);
    }

    #[test]
    fn the_list_prefix_excludes_other_jobs_hours_and_descriptions() {
        let at = |s: i64| Local.timestamp_opt(s, 0).single().unwrap();
        let lines = vec![
            (1, LogLine::Start(at(1000), "ST:Setup Jira".to_string())),
            // Another job's START closes the tagged session without counting itself...
            (2, LogLine::Start(at(4600), "OT:Other work".to_string())),
            (3, LogLine::Stop(at(8200))),
        ];
        let filtered = filter_lines_by_prefix(&lines, "ST");
        let rendered = render_report(&filtered, None, None, false);
        assert!(rendered.contains("100.0%  1.00h  Setup Jira"));
        assert!(!rendered.contains("Other work"));
        // ...and an untagged entry is excluded the same way.
        let untagged = vec![
            (1, LogLine::Start(at(1000), "misc".to_string())),
            (2, LogLine::Stop(at(4600))),
        ];
        let filtered = filter_lines_by_prefix(&untagged, "ST");
        assert_eq!(
            render_report(&filtered, None, None, false),
            "No work recorded.\n"
        );
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
    fn test_parse_start_time_short_and_meridiem_forms() {
        let today = Local::now().date_naive();
        let cases = [
            ("7am", 7, 0, 0),
            ("7AM", 7, 0, 0),
            ("7 am", 7, 0, 0),
            ("7 a.m.", 7, 0, 0),
            ("7pm", 19, 0, 0),
            ("7", 7, 0, 0),
            ("19", 19, 0, 0),
            ("7:30pm", 19, 30, 0),
            ("12am", 0, 0, 0),
            ("12:15 AM", 0, 15, 0),
            ("12pm", 12, 0, 0),
            ("12:30:45 pm", 12, 30, 45),
            ("11:59:59", 11, 59, 59),
        ];
        for (input, hour, minute, second) in cases {
            let dt = parse_start_time(input)
                .unwrap_or_else(|| panic!("failed to parse start time {:?}", input));
            assert_eq!(dt.date_naive(), today, "{:?} should be today", input);
            assert_eq!(
                (dt.hour(), dt.minute(), dt.second()),
                (hour, minute, second),
                "{:?}",
                input
            );
        }
    }

    #[test]
    fn test_parse_start_time_date_with_meridiem() {
        let dt = parse_start_time("2025-02-20 9am").unwrap();
        assert_eq!(
            dt.date_naive(),
            NaiveDate::from_ymd_opt(2025, 2, 20).unwrap()
        );
        assert_eq!((dt.hour(), dt.minute()), (9, 0));

        let dt = parse_start_time("02/20/2025 9:05 PM").unwrap();
        assert_eq!(
            dt.date_naive(),
            NaiveDate::from_ymd_opt(2025, 2, 20).unwrap()
        );
        assert_eq!((dt.hour(), dt.minute()), (21, 5));

        // A bare date is midnight that day; MM/DD assumes the current year.
        let dt = parse_start_time("2025-02-20").unwrap();
        assert_eq!(
            dt.date_naive(),
            NaiveDate::from_ymd_opt(2025, 2, 20).unwrap()
        );
        assert_eq!((dt.hour(), dt.minute()), (0, 0));
        let dt = parse_start_time("2/20 8am").unwrap();
        assert_eq!(dt.year(), Local::now().year());
        assert_eq!((dt.month(), dt.day(), dt.hour()), (2, 20, 8));
    }

    #[test]
    fn test_parse_start_time_invalid() {
        assert!(parse_start_time("").is_none());
        assert!(parse_start_time("not-a-date").is_none());
        assert!(parse_start_time("am").is_none());
        assert!(parse_start_time("0am").is_none());
        assert!(parse_start_time("13pm").is_none());
        assert!(parse_start_time("25:00").is_none());
        assert!(parse_start_time("7:60").is_none());
        assert!(parse_start_time("7:00:00:00").is_none());
        assert!(parse_start_time("7xm").is_none());
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
    fn test_cmd_start_backfills_stale_open_session_before_new_start() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let stale_start = Local::now() - chrono::Duration::hours(2);
        fs::write(
            &log_path,
            format!("{}\n", format_start_log_entry(stale_start, "coding")),
        )
        .unwrap();

        let result = cmd_start(&[], &log_path);

        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            3,
            "expected stale START, backfilled STOP, new START"
        );
        assert_eq!(lines[0], format_start_log_entry(stale_start, "coding"));
        // Backfilled STOP is capped to one reminder interval after the stale START (default 5 min).
        let cap = chrono::Duration::seconds(get_reminder_interval_secs() as i64);
        assert_eq!(lines[1], format_stop_log_entry(stale_start + cap));
        match parse_line(lines[2]) {
            Some(LogLine::Start(_, activity)) => assert_eq!(activity, "misc/unspecified"),
            other => panic!("expected new START entry, got {:?}", other),
        }
    }

    #[test]
    fn test_cmd_start_caps_auto_closed_session_before_new_start() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let stale_start = Local::now() - chrono::Duration::hours(2);
        fs::write(
            &log_path,
            format!("{}\n", format_start_log_entry(stale_start, "coding")),
        )
        .unwrap();

        let result = cmd_start(&["next task".to_string()], &log_path);

        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3, "expected START, auto STOP, new START");
        assert_eq!(lines[0], format_start_log_entry(stale_start, "coding"));
        // Auto STOP is capped to one reminder interval after the open START (default 5 min).
        let cap = chrono::Duration::seconds(get_reminder_interval_secs() as i64);
        assert_eq!(lines[1], format_stop_log_entry(stale_start + cap));
        match parse_line(lines[2]) {
            Some(LogLine::Start(_, activity)) => assert_eq!(activity, "next task"),
            other => panic!("expected new START entry, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_native_reminder_dialog_output_accepts_new_activity() {
        match parse_native_reminder_dialog_output("brand new task") {
            Some(ReminderResult::Activity(activity)) => assert_eq!(activity, "brand new task"),
            _ => panic!("expected activity result"),
        }
    }

    #[test]
    fn test_parse_native_reminder_dialog_output_handles_special_buttons() {
        assert!(matches!(
            parse_native_reminder_dialog_output("Stop Work"),
            Some(ReminderResult::DontBugMe)
        ));
        assert!(matches!(
            parse_native_reminder_dialog_output("Enter new activity..."),
            Some(ReminderResult::EnterNew)
        ));
        assert!(parse_native_reminder_dialog_output("   ").is_none());
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
        assert_eq!(
            lines.len(),
            2,
            "expected START and STOP lines, got: {:?}",
            lines
        );
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
        assert_eq!(
            before, after,
            "timesheet stop should not change file when last entry is STOP and no time given"
        );
    }

    /// The reminder daemon blocks SIGTERM for its sigwait thread. That mask is inherited across
    /// fork and exec, so without an explicit reset every dialog it spawns ignores SIGTERM for life
    /// and `timesheet stop` cannot close a prompt left on screen.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_reset_child_signal_mask_clears_the_inherited_block() {
        let blocked_mask = |cmd: &mut Command| -> String {
            let out = cmd.output().expect("read /proc/self/status");
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find(|l| l.starts_with("SigBlk:"))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
                .expect("SigBlk line")
        };
        let status_cmd = || {
            let mut c = Command::new("cat");
            c.arg("/proc/self/status");
            c
        };

        // Block SIGTERM on this thread, the way run_reminder_daemon does.
        let mut set = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        unsafe {
            sigemptyset(&mut set);
            sigaddset(&mut set, SIGTERM);
            pthread_sigmask(SIG_BLOCK, &set, std::ptr::null_mut());
        }
        let inherited = blocked_mask(&mut status_cmd());
        let mut cleared_cmd = status_cmd();
        reset_child_signal_mask(&mut cleared_cmd);
        let cleared = blocked_mask(&mut cleared_cmd);
        unsafe { pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) };

        // Bit 15 (0x4000) is SIGTERM: a plain spawn inherits the block, ours does not.
        assert_eq!(
            inherited, "0000000000004000",
            "a plain child should inherit the daemon's blocked SIGTERM"
        );
        assert_eq!(
            cleared, "0000000000000000",
            "reset_child_signal_mask should hand the child a clear mask"
        );
    }

    #[test]
    fn test_owns_reminder_daemon_tracks_the_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("ts-reminder.pid");
        // Missing file: nobody owns the daemon role.
        assert!(!owns_reminder_daemon(&pid_path));
        // Claiming it writes our own pid, which is what the daemon loop polls for.
        assert!(claim_reminder_daemon_ownership(&pid_path));
        assert!(owns_reminder_daemon(&pid_path));
        // `timesheet stop` silences a daemon by removing the file; the daemon must see that.
        fs::remove_file(&pid_path).unwrap();
        assert!(!owns_reminder_daemon(&pid_path));
        // A successor's pid in the file disowns us just the same.
        fs::write(&pid_path, format!("{}", process::id() + 1)).unwrap();
        assert!(!owns_reminder_daemon(&pid_path));
    }

    #[test]
    fn test_reminder_daemon_disowned_is_false_outside_the_daemon() {
        // The foreground `timesheet start` chooser shares the prompt code but never owns the PID file.
        // It must not read that as "a stop happened" and close itself.
        assert!(!IS_REMINDER_DAEMON.load(std::sync::atomic::Ordering::Relaxed));
        assert!(!reminder_daemon_disowned());
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
        let new_epoch = parse_line(lines[1])
            .and_then(|ll| match ll {
                LogLine::Stop(e) => Some(e),
                _ => None,
            })
            .unwrap();
        let expected = parse_start_time(new_time).unwrap();
        assert_eq!(
            new_epoch, expected,
            "last STOP should be amended to the given time"
        );
    }

    #[test]
    fn test_cmd_list_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_list(&[], &log_path);
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
        let result = cmd_list(&[], &log_path);
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
    fn test_cmd_started_uses_canonical_timestamp_format() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let input = "2025-02-20 10:00".to_string();

        let result = cmd_started(&[input.clone(), "manual".to_string()], &log_path);

        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        let expected_dt = parse_start_time(&input).unwrap();
        let expected_line = format!("{}\n", format_start_log_entry(expected_dt, "manual"));
        assert_eq!(content, expected_line);
    }

    #[test]
    fn test_cmd_migrate_normalizes_timestamp_precision() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::write(
            &log_path,
            "2026-03-30T14:30:00-04:00|START|manual\n2026-03-30T15:21:48.022092-04:00|STOP\n",
        )
        .unwrap();

        let result = cmd_migrate(&log_path);

        assert!(result.is_ok());
        let content = fs::read_to_string(&log_path).unwrap();
        // Compute the expected lines through the same parse+format path migrate uses, so the
        // assertion is independent of the machine's timezone (CI runs in UTC). The seconds-only
        // START must gain microsecond precision; the STOP keeps its existing micros.
        let start_dt = parse_timestamp_field("2026-03-30T14:30:00-04:00").unwrap();
        let stop_dt = parse_timestamp_field("2026-03-30T15:21:48.022092-04:00").unwrap();
        assert!(content.contains(&format!("{}\n", format_start_log_entry(start_dt, "manual"))));
        assert!(content.contains(&format!("{}\n", format_stop_log_entry(stop_dt))));
        // The START gained microsecond precision (it had none in the input).
        assert!(content.contains(".000000"));
    }

    #[test]
    fn test_close_open_session_records_stop_when_open() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::write(
            &log_path,
            format!(
                "{}|START|task\n",
                format_log_timestamp(Local::now() - chrono::Duration::minutes(1))
            ),
        )
        .unwrap();

        let wrote = close_open_session(&log_path, Local::now());

        assert!(wrote);
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(matches!(
            last_recorded_event(&content),
            Some(LogLine::Stop(_))
        ));
    }

    #[test]
    fn test_close_open_session_noop_when_already_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let original = format!(
            "{}|START|task\n{}|STOP\n",
            format_log_timestamp(Local::now() - chrono::Duration::minutes(2)),
            format_log_timestamp(Local::now() - chrono::Duration::minutes(1))
        );
        fs::write(&log_path, &original).unwrap();

        let wrote = close_open_session(&log_path, Local::now());

        assert!(!wrote);
        assert_eq!(fs::read_to_string(&log_path).unwrap(), original);
    }

    #[test]
    fn test_close_open_session_before_start_skips_redundant_stop() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        // Open session younger than one reminder interval: the STOP would land at `now`, exactly
        // where the new START goes, so LIFO pairing makes it redundant.
        let original = format!(
            "{}|START|task\n",
            format_log_timestamp(Local::now() - chrono::Duration::minutes(1))
        );
        fs::write(&log_path, &original).unwrap();

        let wrote = close_open_session_before_start(&log_path, Local::now());

        assert!(!wrote);
        assert_eq!(fs::read_to_string(&log_path).unwrap(), original);
    }

    #[test]
    fn test_close_open_session_before_start_writes_stop_when_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        // Open session older than one reminder interval: the STOP is clamped back before `now`,
        // leaving an unbilled gap, so it must be recorded.
        let start_dt = Local::now() - chrono::Duration::minutes(30);
        fs::write(
            &log_path,
            format!("{}|START|task\n", format_log_timestamp(start_dt)),
        )
        .unwrap();

        let now = Local::now();
        let wrote = close_open_session_before_start(&log_path, now);

        assert!(wrote);
        let content = fs::read_to_string(&log_path).unwrap();
        let Some(LogLine::Stop(stop_dt)) = last_recorded_event(&content) else {
            panic!("expected a STOP entry, got: {}", content);
        };
        assert!(stop_dt < now);
        // Compare through the log's own format: it truncates to microseconds, so the raw
        // `Local::now()` nanoseconds never survive the round trip.
        let expected = start_dt + chrono::Duration::seconds(get_reminder_interval_secs() as i64);
        assert_eq!(
            format_stop_log_entry(stop_dt),
            format_stop_log_entry(expected)
        );
    }

    #[test]
    fn test_close_open_session_before_start_noop_when_already_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let original = format!(
            "{}|START|task\n{}|STOP\n",
            format_log_timestamp(Local::now() - chrono::Duration::minutes(40)),
            format_log_timestamp(Local::now() - chrono::Duration::minutes(30))
        );
        fs::write(&log_path, &original).unwrap();

        let wrote = close_open_session_before_start(&log_path, Local::now());

        assert!(!wrote);
        assert_eq!(fs::read_to_string(&log_path).unwrap(), original);
    }

    #[test]
    fn test_reminder_timeout_stop_uses_prompt_time_unclamped() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let start_dt = Local::now() - chrono::Duration::minutes(30);
        fs::write(
            &log_path,
            format!("{}|START|task\n", format_log_timestamp(start_dt)),
        )
        .unwrap();

        // The prompt appeared 20 minutes ago -- well past the clamp cap, which would have pulled
        // the STOP back to start + one interval. The prompt time must survive intact.
        let appeared = Local::now() - chrono::Duration::minutes(20);
        append_reminder_timeout_stop(&log_path, appeared).unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        let Some(LogLine::Stop(stop_dt)) = last_recorded_event(&content) else {
            panic!("expected a STOP entry, got: {}", content);
        };
        assert_eq!(
            format_stop_log_entry(stop_dt),
            format_stop_log_entry(appeared)
        );
    }

    #[test]
    fn test_reminder_timeout_stop_then_return_leaves_away_time_unbilled() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let interval = chrono::Duration::seconds(get_reminder_interval_secs() as i64);
        // Worked for an hour, then the prompt went up one interval after the last entry and sat
        // unanswered while we were away.
        let start_dt = Local::now() - chrono::Duration::minutes(60);
        fs::write(
            &log_path,
            format!("{}|START|task\n", format_log_timestamp(start_dt)),
        )
        .unwrap();
        let appeared = start_dt + interval;

        append_reminder_timeout_stop(&log_path, appeared).unwrap();
        // Back at the desk 40 minutes later: picking an activity opens a fresh session now.
        let returned = Local::now() - chrono::Duration::minutes(10);
        append_log_entry(&log_path, &format_start_log_entry(returned, "task")).unwrap();

        let lines = read_log_lines(&log_path).unwrap();
        let (per_activity, _dow, _open) = process_log_for_report(&lines, Some(Local::now()));
        let (_label, _pct, hours) = per_activity
            .iter()
            .find(|(label, _, _)| label == "task")
            .expect("task should be reported");
        // Billed: the interval before the prompt plus the 10 minutes since returning. The stretch
        // between the STOP and the return is a gap and must not appear.
        let expected = (interval + chrono::Duration::minutes(10)).num_seconds() as f64 / 3600.0;
        assert!(
            (hours - expected).abs() < 0.01,
            "billed {} h, expected {} h",
            hours,
            expected
        );
    }

    #[test]
    fn test_reminder_timeout_stop_noop_when_no_session_open() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        // Work is already stopped: repeated unanswered prompts must not pile up STOP entries.
        let original = format!(
            "{}|START|task\n{}|STOP\n",
            format_log_timestamp(Local::now() - chrono::Duration::minutes(40)),
            format_log_timestamp(Local::now() - chrono::Duration::minutes(35))
        );
        fs::write(&log_path, &original).unwrap();

        append_reminder_timeout_stop(&log_path, Local::now()).unwrap();

        assert_eq!(fs::read_to_string(&log_path).unwrap(), original);
    }

    #[test]
    fn test_reconcile_stale_open_session_caps_old_open_start() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let start = Local::now() - chrono::Duration::hours(8);
        fs::write(
            &log_path,
            format!("{}|START|task\n", format_log_timestamp(start)),
        )
        .unwrap();

        let wrote = reconcile_stale_open_session(&log_path, Local::now());

        assert!(wrote);
        let content = fs::read_to_string(&log_path).unwrap();
        // STOP is capped to one reminder interval after the open START, not "now" (no all-nighter).
        let cap = chrono::Duration::seconds(get_reminder_interval_secs() as i64);
        let expected_stop = format_stop_log_entry(start + cap);
        assert!(content.contains(&format!("{}\n", expected_stop)));
    }

    #[test]
    fn test_reconcile_stale_open_session_leaves_recent_session() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        // Open START only 1 minute ago: the user is actively working; do not close it.
        let original = format!(
            "{}|START|task\n",
            format_log_timestamp(Local::now() - chrono::Duration::minutes(1))
        );
        fs::write(&log_path, &original).unwrap();

        let wrote = reconcile_stale_open_session(&log_path, Local::now());

        assert!(!wrote);
        assert_eq!(fs::read_to_string(&log_path).unwrap(), original);
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
    fn test_cmd_prefix_missing_args() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        fs::File::create(&log_path).unwrap();
        assert!(cmd_prefix(&[], &log_path).is_err());
        assert!(cmd_prefix(&["foo".to_string()], &log_path).is_err());
    }

    /// "timesheet prefix foo bar" searches for "bar" (not "foo:bar"), like "timesheet alias bar foo:bar".
    #[test]
    fn test_cmd_prefix_searches_for_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let week_start = week_start(Local::now());
        fs::write(
            &log_path,
            format!(
                "{}|START|other\n{}|STOP\n",
                fmt_ts(week_start.timestamp()),
                fmt_ts(week_start.timestamp() + 100)
            ),
        )
        .unwrap();
        let err = cmd_prefix(&["foo".to_string(), "bar".to_string()], &log_path).unwrap_err();
        assert!(err.contains("\"bar\""), "unexpected error: {}", err);
    }

    #[test]
    fn test_cmd_prefix_replacement_is_prefixed_pattern() {
        let week_start = week_start(Local::now());
        let week_end = week_start + chrono::Duration::weeks(1) - chrono::Duration::seconds(1);
        let content = format!("{}|START|bar\n", fmt_ts(week_start.timestamp()));
        let matches_vec =
            collect_workalias_matches(&content, week_start, week_end, "bar", "foo:bar");
        assert_eq!(matches_vec.len(), 1);
        assert_eq!(matches_vec[0].replacement, "foo:bar");
    }

    #[test]
    fn test_cmd_workalias_no_timesheet() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("timesheet.log");
        let result = cmd_workalias(&["coding".to_string(), "dev".to_string()], &log_path);
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
        let result = cmd_workalias(&["nonexistent".to_string(), "repl".to_string()], &log_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no activities matching"));
    }

    #[test]
    fn test_collect_workalias_matches_prefers_literal_replacement() {
        let now = chrono::Local::now().timestamp();
        let week_start = week_start(Local.timestamp_opt(now, 0).single().unwrap());
        let content = format!(
            "{}|START|api.v1\n{}|STOP\n{}|START|plain text\n{}|STOP\n",
            fmt_ts(week_start.timestamp()),
            fmt_ts(week_start.timestamp() + 60),
            fmt_ts(week_start.timestamp() + 120),
            fmt_ts(week_start.timestamp() + 180)
        );

        let matches_vec = collect_workalias_matches(
            &content,
            week_start,
            week_start + chrono::Duration::weeks(1) - chrono::Duration::seconds(1),
            ".",
            "-",
        );

        assert_eq!(matches_vec.len(), 1);
        assert_eq!(matches_vec[0].replacement, "api-v1");
    }

    #[test]
    fn test_collect_workalias_matches_falls_back_to_regex() {
        let now = chrono::Local::now().timestamp();
        let week_start = week_start(Local.timestamp_opt(now, 0).single().unwrap());
        let content = format!(
            "{}|START|feature 123\n{}|STOP\n",
            fmt_ts(week_start.timestamp()),
            fmt_ts(week_start.timestamp() + 60)
        );

        let matches_vec = collect_workalias_matches(
            &content,
            week_start,
            week_start + chrono::Duration::weeks(1) - chrono::Duration::seconds(1),
            "\\d+",
            "456",
        );

        assert_eq!(matches_vec.len(), 1);
        assert_eq!(matches_vec[0].replacement, "feature 456");
    }

    #[test]
    fn test_collect_workalias_matches_invalid_regex_without_literal_match() {
        let now = chrono::Local::now().timestamp();
        let week_start = week_start(Local.timestamp_opt(now, 0).single().unwrap());
        let content = format!(
            "{}|START|feature 123\n{}|STOP\n",
            fmt_ts(week_start.timestamp()),
            fmt_ts(week_start.timestamp() + 60)
        );

        let matches_vec = collect_workalias_matches(
            &content,
            week_start,
            week_start + chrono::Duration::weeks(1) - chrono::Duration::seconds(1),
            "[",
            "456",
        );

        assert!(matches_vec.is_empty());
    }

    #[test]
    fn test_should_replace_workalias_match_accepts_replace_all() {
        let mut replace_all = false;

        assert!(should_replace_workalias_match("a\n", &mut replace_all));
        assert!(replace_all);
    }

    #[test]
    fn test_should_replace_workalias_match_auto_replaces_after_replace_all() {
        let mut replace_all = true;

        assert!(should_replace_workalias_match("n\n", &mut replace_all));
        assert!(replace_all);
    }

    #[test]
    fn test_should_replace_workalias_match_skips_other_inputs() {
        let mut replace_all = false;

        assert!(!should_replace_workalias_match("n\n", &mut replace_all));
        assert!(!replace_all);
    }

    #[test]
    fn test_cmd_install_to_dir() {
        let dest_dir = tempfile::tempdir().unwrap();
        let dest_path = dest_dir.path().to_path_buf();
        let result = cmd_install(&[dest_path.to_string_lossy().to_string()]);
        assert!(result.is_ok());
        let exe_name = if cfg!(windows) {
            "timesheet.exe"
        } else {
            "timesheet"
        };
        let installed = dest_path.join(exe_name);
        assert!(installed.exists());
    }
}
