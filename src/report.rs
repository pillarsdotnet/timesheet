// Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
// Co-author: Claude-AI.

//! `ts pdf` and `ts email`: aggregate one timesheet week and fill the weekly contractor form.
//!
//! The week runs from one rotation boundary to the next, so the report follows whatever
//! `rotate:` says rather than assuming a particular first day. Hours are credited to the day
//! a session started on, matching `ts list`.

use crate::settings::{expand_placeholders, Overrides, Settings, DEFAULT_OUTPUT};
use crate::{
    config_path, log_line_dt, mail, parse_line, pdf, rotated_timesheet_files, settings,
    week_start_with, yaml, LogLine, RotationBoundary,
};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate};
use std::collections::HashMap;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

/// One worked interval: when it started, how long it ran, and what it was.
struct Session {
    start: DateTime<Local>,
    seconds: i64,
    activity: String,
}

/// Converts a time-ordered event list into sessions.
///
/// A START runs until the next event of any kind — the accounting `ts list` uses, so
/// consecutive reminder STARTs each contribute their own interval — and a trailing
/// unterminated START runs until `now`.
fn sessions(events: &[LogLine], now: DateTime<Local>) -> Vec<Session> {
    let mut out = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let LogLine::Start(start, activity) = event else {
            continue;
        };
        let end = events.get(index + 1).map(log_line_dt).unwrap_or(now);
        let seconds = (end - *start).num_seconds();
        if seconds > 0 {
            out.push(Session {
                start: *start,
                seconds,
                activity: activity.clone(),
            });
        }
    }
    out
}

/// Every timesheet log: the current one plus its rotated siblings. A week that straddles a
/// rotation still reports in full because all of them are read.
fn all_log_files(timesheet: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if timesheet.exists() {
        files.push(timesheet.to_path_buf());
    }
    files.extend(rotated_timesheet_files(timesheet));
    files
}

/// Reads and time-sorts every START/STOP entry in `paths`. An unreadable file warns rather
/// than aborting, since the remaining logs may still hold the week being reported.
fn read_events(paths: &[PathBuf], warnings: &mut Vec<String>) -> Vec<LogLine> {
    let mut events = Vec::new();
    for path in paths {
        match fs::read_to_string(path) {
            Ok(text) => events.extend(text.lines().filter_map(parse_line)),
            Err(e) => warnings.push(format!("cannot read {}: {}", path.display(), e)),
        }
    }
    events.sort_by_key(log_line_dt);
    events
}

/// The start of the week to report on when no argument is given.
///
/// On the last day of the week in progress that week ends today, so it is the one to submit.
/// On any other day the most recently completed week is used, which is what an early run on
/// the first day of a new week wants.
fn default_week(now: DateTime<Local>, boundary: RotationBoundary) -> DateTime<Local> {
    let current = week_start_with(now, boundary);
    if now >= current + Duration::days(6) {
        current
    } else {
        week_start_with(current - Duration::seconds(1), boundary)
    }
}

/// The start of whichever week holds the most recorded time in `work`.
///
/// Used when an explicit log file is named: a rotated file need not line up with the
/// reporting week, so picking the heavier week avoids reporting one that holds only a stray
/// trailing day.
fn busiest_week(work: &[Session], boundary: RotationBoundary) -> Option<DateTime<Local>> {
    let mut tally: HashMap<DateTime<Local>, i64> = HashMap::new();
    for session in work {
        *tally
            .entry(week_start_with(session.start, boundary))
            .or_insert(0) += session.seconds;
    }
    tally
        .into_iter()
        .max_by_key(|(start, seconds)| (*seconds, *start))
        .map(|(start, _)| start)
}

/// Strips the job prefix from an activity, or returns `None` when the entry belongs to
/// another job and should be left out of this timesheet entirely — its hours as well as its
/// description. With no prefix configured, every entry counts and none is rewritten.
pub fn strip_prefix<'a>(activity: &'a str, prefix: Option<&str>) -> Option<&'a str> {
    match prefix {
        None => Some(activity.trim()),
        Some(prefix) => activity
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::trim),
    }
}

/// The hours and activity text for each of the week's seven days, plus the total.
struct WeekReport {
    /// Indexed from the week's first day, not from any fixed weekday.
    hours: Vec<String>,
    activities: Vec<String>,
    total: String,
}

/// Builds the Hours and Key Activities column text.
///
/// Each session's whole duration is credited to the day it started on, matching `ts list`.
/// The total is the sum of the *displayed* day figures, so the column always adds up on
/// paper.
fn week_report(work: &[Session], week_start: DateTime<Local>, settings: &Settings) -> WeekReport {
    let mut day_seconds = [0i64; 7];
    let mut day_activities: Vec<HashMap<String, i64>> = vec![HashMap::new(); 7];
    for session in work {
        let index = (session.start.date_naive() - week_start.date_naive()).num_days();
        if !(0..7).contains(&index) {
            continue;
        }
        let Some(label) = strip_prefix(&session.activity, settings.prefix.as_deref()) else {
            continue;
        };
        let index = index as usize;
        day_seconds[index] += session.seconds;
        if !label.is_empty() {
            *day_activities[index].entry(label.to_string()).or_insert(0) += session.seconds;
        }
    }

    let mut hours = Vec::with_capacity(7);
    let mut activities = Vec::with_capacity(7);
    let mut total = 0.0;
    for index in 0..7 {
        // Round once, here, and total the rounded figures: the printed column must add up.
        let day_hours = (day_seconds[index] as f64 / 3600.0 * 100.0).round() / 100.0;
        total += day_hours;
        hours.push(if day_hours > 0.0 {
            format!("{:.2}", day_hours)
        } else {
            settings.zero.clone()
        });

        let mut ranked: Vec<(&String, &i64)> = day_activities[index].iter().collect();
        // Longest first, then alphabetically so that equal times report in a stable order.
        ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        activities.push(
            ranked
                .iter()
                .map(|(name, seconds)| {
                    expand_placeholders(
                        &settings.activity,
                        &[
                            ("activity", name.as_str()),
                            ("hours", &format!("{:.2}", **seconds as f64 / 3600.0)),
                        ],
                    )
                })
                .collect::<Vec<String>>()
                .join(&settings.separator),
        );
    }
    WeekReport {
        hours,
        activities,
        total: format!("{:.2}", (total * 100.0).round() / 100.0),
    }
}

/// The slot values to write into the form, keyed by slot name.
fn field_values(
    report: &WeekReport,
    week_start: NaiveDate,
    week_end: NaiveDate,
    settings: &Settings,
) -> Vec<(String, String)> {
    let mut values = vec![
        ("contractor_name".to_string(), settings.name.clone()),
        (
            "week_start_month".to_string(),
            format!("{:02}", week_start.month()),
        ),
        (
            "week_start_day".to_string(),
            format!("{:02}", week_start.day()),
        ),
        ("week_start_year".to_string(), week_start.year().to_string()),
        (
            "week_end_month".to_string(),
            format!("{:02}", week_end.month()),
        ),
        ("week_end_day".to_string(), format!("{:02}", week_end.day())),
        ("week_end_year".to_string(), week_end.year().to_string()),
        ("total_hours".to_string(), report.total.clone()),
    ];
    // Slots are named for the weekday, so the rows land correctly whatever day the
    // configured rotation boundary makes the first of the week.
    for offset in 0..7 {
        let date = week_start + Duration::days(offset as i64);
        let day = settings::DAYS[date.weekday().num_days_from_monday() as usize];
        values.push((format!("{}_hours", day), report.hours[offset].clone()));
        values.push((
            format!("{}_activities", day),
            report.activities[offset].clone(),
        ));
    }
    values
}

/// Parsed command line for `ts pdf` / `ts email`.
struct Invocation {
    input: Option<String>,
    overrides: Overrides,
}

/// Splits `--name=value` into its parts.
fn split_inline(arg: &str) -> Option<(&str, &str)> {
    arg.strip_prefix("--").and_then(|rest| rest.split_once('='))
}

/// Splits a comma-separated address list, so `--to a@x,b@y` works as well as repeating the
/// option.
fn split_addresses(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parses the options shared by both subcommands plus, when `email`, the address options.
///
/// `-t` means `--template` for `ts pdf` and `--to` for `ts email`; `ts email` spells the
/// template option `--template` or `-T`.
fn parse_args(args: &[String], email: bool) -> Result<Invocation, String> {
    let command = if email { "ts email" } else { "ts pdf" };
    let mut input: Option<String> = None;
    let mut over = Overrides::default();
    let mut positional_only = false;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].clone();
        index += 1;
        // `-1` and friends are rotated-log indices, the same selector `ts list` takes, so a
        // leading dash followed by digits is a positional rather than an option.
        let negative_index = arg.len() > 1 && arg[1..].chars().all(|c| c.is_ascii_digit());
        if positional_only || !arg.starts_with('-') || arg == "-" || negative_index {
            if input.is_some() {
                return Err(format!(
                    "{}: unexpected extra argument \"{}\"; only one week may be selected",
                    command, arg
                ));
            }
            input = Some(arg);
            continue;
        }
        if arg == "--" {
            positional_only = true;
            continue;
        }

        let (name, inline) = match split_inline(&arg) {
            Some((name, value)) => (format!("--{}", name), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            let v = args
                .get(index)
                .cloned()
                .ok_or_else(|| format!("{}: {} needs a value", command, name))?;
            index += 1;
            Ok(v)
        };

        match name.as_str() {
            "-p" | "--prefix" => over.prefix = Some(value()?),
            "-o" | "--output" => over.output = Some(value()?),
            "-a" | "--activity" => over.activity = Some(value()?),
            "-s" | "--separator" => over.separator = Some(value()?),
            "-z" | "--zero" => over.zero = Some(value()?),
            "--template" => over.template = Some(value()?),
            "-T" if email => over.template = Some(value()?),
            "-t" if !email => over.template = Some(value()?),
            "-t" | "--to" if email => {
                over.to
                    .get_or_insert_with(Vec::new)
                    .extend(split_addresses(&value()?));
            }
            "-c" | "--cc" if email => {
                over.cc
                    .get_or_insert_with(Vec::new)
                    .extend(split_addresses(&value()?));
            }
            "-f" | "--from" if email => over.from = Some(value()?),
            "-r" | "--reply" | "--reply-to" if email => over.reply = Some(value()?),
            other => return Err(format!("{}: unknown option \"{}\"", command, other)),
        }
    }
    Ok(Invocation {
        input,
        overrides: over,
    })
}

/// Everything one run produces, before it is written or mailed.
struct Filled {
    data: Vec<u8>,
    /// Where `ts pdf` writes, with placeholders expanded; `None` means stdout.
    output: Option<PathBuf>,
    /// The filename alone: what the attachment is called, and what lands in a directory
    /// named by `--output`.
    basename: String,
    subject: String,
    body: String,
}

/// Loads the config, selects the week, aggregates it, and fills the template.
fn build(
    args: &[String],
    timesheet: &Path,
    email: bool,
    warnings: &mut Vec<String>,
) -> Result<(Settings, Filled), String> {
    let invocation = parse_args(args, email)?;
    let config = config_path();
    let text = match fs::read_to_string(&config) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("ts: cannot read {}: {}", config.display(), e)),
    };
    let doc = yaml::parse(&text);
    // Point at the file the settings would have come from; with no config at all the first
    // run is otherwise told to edit a file it cannot find.
    let settings = Settings::resolve(&doc, &invocation.overrides)
        .map_err(|e| format!("{} ({})", e, config.display()))?;
    if !settings.template.is_file() {
        return Err(format!(
            "ts: template not found: {} (set \"template:\" in {})",
            settings.template.display(),
            config.display()
        ));
    }

    let boundary = crate::rotation_boundary();
    let now = Local::now();
    let paths = all_log_files(timesheet);
    if paths.is_empty() {
        return Err(format!(
            "ts: no timesheet log found at {}",
            timesheet.display()
        ));
    }

    // A named week is located inside the file the user asked for, but the report itself is
    // built from every log, so a week spanning a rotation still reports in full.
    let week_start = match invocation.input.as_deref() {
        Some(arg) => {
            // The selector is `ts list`'s, but the complaint should name the command the
            // user actually ran.
            let selected = crate::resolve_list_input(Some(arg), timesheet)
                .map_err(|e| e.replace("ts list:", if email { "ts email:" } else { "ts pdf:" }))?;
            let mut ignored = Vec::new();
            let events = read_events(std::slice::from_ref(&selected), &mut ignored);
            busiest_week(&sessions(&events, now), boundary)
                .ok_or_else(|| format!("ts: no usable log entries in {}", selected.display()))?
        }
        None => default_week(now, boundary),
    };
    let week_end = week_start + Duration::days(6);

    let events = read_events(&paths, warnings);
    let work = sessions(&events, now);

    // Warn only about an open session this timesheet actually counts; a task belonging to
    // another job is none of this report's business.
    if let Some(LogLine::Start(start, activity)) = events.last() {
        if *start >= week_start && *start < week_start + Duration::days(7) {
            if let Some(label) = strip_prefix(activity, settings.prefix.as_deref()) {
                warnings.push(format!(
                    "\"{}\" is still in progress since {}; counted up to now",
                    label,
                    start.format("%Y-%m-%d %H:%M")
                ));
            }
        }
    }

    let report = week_report(&work, week_start, &settings);
    if report.total == "0.00" {
        warnings.push(format!(
            "no work recorded for {} .. {}",
            week_start.format("%Y-%m-%d"),
            week_end.format("%Y-%m-%d")
        ));
    }

    let values = field_values(
        &report,
        week_start.date_naive(),
        week_end.date_naive(),
        &settings,
    );
    let placeholders: Vec<(&str, String)> = vec![
        ("date", now.format("%Y-%m-%d").to_string()),
        ("week_start", week_start.format("%Y-%m-%d").to_string()),
        ("week_end", week_end.format("%Y-%m-%d").to_string()),
        ("total_hours", report.total.clone()),
        ("contractor_name", settings.name.clone()),
        ("name", settings.name.clone()),
        ("prefix", settings.prefix.clone().unwrap_or_default()),
    ];
    let placeholders: Vec<(&str, &str)> =
        placeholders.iter().map(|(k, v)| (*k, v.as_str())).collect();

    // Translate slot names to the template's field names, dropping any slot the config maps
    // to an empty name so a form without that cell can still be filled.
    let mut pdf_values: Vec<(String, String)> = Vec::new();
    for (slot, text) in &values {
        if let Some(field) = settings.fields.get(slot).filter(|f| !f.is_empty()) {
            pdf_values.push((field.clone(), text.clone()));
        }
    }
    let multiline: Vec<String> = settings::DAYS
        .iter()
        .filter_map(|day| settings.fields.get(&format!("{}_activities", day)).cloned())
        .collect();
    let left_aligned: Vec<String> = settings
        .fields
        .get("contractor_name")
        .cloned()
        .into_iter()
        .collect();

    let mut fill_warnings = Vec::new();
    let data = pdf::fill(
        &settings.template,
        &pdf_values,
        &multiline,
        &left_aligned,
        settings.min_font_size,
        settings.max_font_size,
        &mut fill_warnings,
    )
    .map_err(|e| format!("ts: {}", e))?;
    warnings.extend(fill_warnings);

    let output = settings
        .output
        .as_deref()
        .map(|template| settings::expand_tilde(&expand_placeholders(template, &placeholders)));
    // A directory named by `--output` receives the default filename, and the attachment is
    // named the same way, so an emailed timesheet is never called after a directory.
    let basename = match output.as_deref().filter(|p| !p.is_dir()) {
        Some(path) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| expand_placeholders(DEFAULT_OUTPUT, &placeholders)),
        None => expand_placeholders(DEFAULT_OUTPUT, &placeholders),
    };
    let subject = expand_placeholders(&settings.subject, &placeholders);
    let body = expand_placeholders(&settings.body, &placeholders);
    Ok((
        settings,
        Filled {
            data,
            output,
            basename,
            subject,
            body,
        },
    ))
}

/// Writes `data` to `destination`, treating an existing directory as the place to put
/// `basename`.
fn write_output(destination: &Path, basename: &str, data: &[u8]) -> Result<PathBuf, String> {
    let path = if destination.is_dir() {
        destination.join(basename)
    } else {
        destination.to_path_buf()
    };
    fs::write(&path, data).map_err(|e| format!("ts: cannot write {}: {}", path.display(), e))?;
    Ok(path)
}

fn report_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("ts: {}", warning);
    }
}

/// `ts pdf`: fill the timesheet and write it to a file or to stdout.
pub fn cmd_pdf(args: &[String], timesheet: &Path) -> Result<(), String> {
    let mut warnings = Vec::new();
    let result = build(args, timesheet, false, &mut warnings);
    report_warnings(&warnings);
    let (_settings, filled) = result?;

    match &filled.output {
        Some(destination) => {
            let path = write_output(destination, &filled.basename, &filled.data)?;
            eprintln!("ts: wrote {}", path.display());
        }
        None => {
            if std::io::stdout().is_terminal() {
                return Err(
                    "ts: refusing to write a PDF to the terminal; redirect it or use --output"
                        .to_string(),
                );
            }
            let mut out = std::io::stdout().lock();
            out.write_all(&filled.data)
                .and_then(|_| out.flush())
                .map_err(|e| format!("ts: cannot write to stdout: {}", e))?;
        }
    }
    Ok(())
}

/// `ts email`: fill the timesheet and mail it.
pub fn cmd_email(args: &[String], timesheet: &Path) -> Result<(), String> {
    let mut warnings = Vec::new();
    let result = build(args, timesheet, true, &mut warnings);
    let (settings, filled) = match result {
        Ok(pair) => pair,
        Err(e) => {
            report_warnings(&warnings);
            return Err(e);
        }
    };

    let mut send_warnings = Vec::new();
    let sent = mail::send(
        &settings,
        filled.data.clone(),
        &filled.basename,
        &filled.subject,
        &filled.body,
        &mut send_warnings,
    );
    warnings.extend(send_warnings);
    report_warnings(&warnings);

    match sent {
        Ok(recipients) => {
            eprintln!("ts: sent {} to {}", filled.basename, recipients.join(", "));
            Ok(())
        }
        Err(error) => {
            // The PDF is already built; dropping it would mean rebuilding a week that may
            // have moved on. Keep it so the send can be retried by hand.
            let rescue = filled
                .output
                .clone()
                .unwrap_or_else(|| PathBuf::from(&filled.basename));
            match write_output(&rescue, &filled.basename, &filled.data) {
                Ok(path) => Err(format!(
                    "{}\nts: kept the unsent timesheet at {}",
                    error,
                    path.display()
                )),
                Err(write_error) => Err(format!("{}\n{}", error, write_error)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml;
    use chrono::{NaiveTime, TimeZone, Weekday};

    fn at(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, hh, mm, 0).unwrap()
    }

    fn monday_boundary() -> RotationBoundary {
        RotationBoundary {
            day: Weekday::Mon,
            time: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        }
    }

    fn settings_for(config: &str) -> Settings {
        Settings::resolve(&yaml::parse(config), &Overrides::default()).unwrap()
    }

    const BASE: &str = "name: Test User\ntemplate: /t.pdf\n";

    #[test]
    fn a_start_runs_until_the_next_event_of_any_kind() {
        let events = vec![
            LogLine::Start(at(2026, 7, 27, 9, 0), "ST:one".into()),
            // A second START without a STOP closes the first, as `ts list` accounts for it.
            LogLine::Start(at(2026, 7, 27, 10, 0), "ST:two".into()),
            LogLine::Stop(at(2026, 7, 27, 10, 30)),
        ];
        let work = sessions(&events, at(2026, 7, 27, 23, 0));
        assert_eq!(work.len(), 2);
        assert_eq!(work[0].seconds, 3600);
        assert_eq!(work[0].activity, "ST:one");
        assert_eq!(work[1].seconds, 1800);
    }

    #[test]
    fn a_trailing_start_runs_until_now() {
        let events = vec![LogLine::Start(at(2026, 7, 27, 9, 0), "ST:open".into())];
        let work = sessions(&events, at(2026, 7, 27, 11, 30));
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].seconds, 9000);
    }

    #[test]
    fn zero_length_sessions_are_dropped() {
        let events = vec![
            LogLine::Start(at(2026, 7, 27, 9, 0), "ST:a".into()),
            LogLine::Start(at(2026, 7, 27, 9, 0), "ST:b".into()),
            LogLine::Stop(at(2026, 7, 27, 10, 0)),
        ];
        assert_eq!(sessions(&events, at(2026, 7, 27, 12, 0)).len(), 1);
    }

    #[test]
    fn the_prefix_must_be_followed_by_a_colon() {
        assert_eq!(
            strip_prefix("ST:Setup Jira", Some("ST")),
            Some("Setup Jira")
        );
        assert_eq!(
            strip_prefix("ST: Setup Jira", Some("ST")),
            Some("Setup Jira")
        );
        // Another job's entry is excluded entirely, hours as well as description.
        assert_eq!(strip_prefix("MG:Mail", Some("ST")), None);
        // A bare prefix with no colon is not a match either.
        assert_eq!(strip_prefix("STuff", Some("ST")), None);
        // With no prefix configured, everything counts and nothing is rewritten.
        assert_eq!(strip_prefix("MG:Mail", None), Some("MG:Mail"));
    }

    #[test]
    fn the_default_week_is_the_one_in_progress_only_on_its_final_day() {
        let boundary = monday_boundary();
        // Sunday: the week in progress ends today, so it is the one to submit.
        assert_eq!(
            default_week(at(2026, 8, 2, 18, 0), boundary),
            at(2026, 7, 27, 0, 0)
        );
        // Monday morning: report the week that just finished, not the empty new one.
        assert_eq!(
            default_week(at(2026, 8, 3, 9, 0), boundary),
            at(2026, 7, 27, 0, 0)
        );
        // Mid-week likewise looks back.
        assert_eq!(
            default_week(at(2026, 8, 5, 9, 0), boundary),
            at(2026, 7, 27, 0, 0)
        );
    }

    #[test]
    fn the_busiest_week_wins_when_a_file_straddles_two() {
        let boundary = monday_boundary();
        let work = vec![
            Session {
                start: at(2026, 7, 26, 9, 0), // Sunday of the earlier week
                seconds: 600,
                activity: "ST:a".into(),
            },
            Session {
                start: at(2026, 7, 28, 9, 0), // Tuesday of the later week
                seconds: 7200,
                activity: "ST:b".into(),
            },
        ];
        assert_eq!(busiest_week(&work, boundary), Some(at(2026, 7, 27, 0, 0)));
        assert_eq!(busiest_week(&[], boundary), None);
    }

    #[test]
    fn hours_round_to_the_cent_and_the_column_adds_up() {
        let settings = settings_for(BASE);
        let week_start = at(2026, 7, 27, 0, 0);
        let work = vec![
            Session {
                start: at(2026, 7, 27, 9, 0),
                seconds: 5400, // 1.50
                activity: "one".into(),
            },
            Session {
                start: at(2026, 7, 28, 9, 0),
                seconds: 1234, // 0.34
                activity: "two".into(),
            },
        ];
        let report = week_report(&work, week_start, &settings);
        assert_eq!(report.hours[0], "1.50");
        assert_eq!(report.hours[1], "0.34");
        assert_eq!(report.total, "1.84");
    }

    #[test]
    fn a_day_with_no_work_shows_the_configured_zero_text() {
        let blank = settings_for(BASE);
        let work = Vec::new();
        assert_eq!(
            week_report(&work, at(2026, 7, 27, 0, 0), &blank).hours[0],
            ""
        );
        let printed = settings_for(&format!("{}zero: \"0.00\"\n", BASE));
        assert_eq!(
            week_report(&work, at(2026, 7, 27, 0, 0), &printed).hours[0],
            "0.00"
        );
    }

    #[test]
    fn activities_rank_by_time_then_name_and_use_the_configured_format() {
        let settings = settings_for(&format!(
            "{}separator: \" | \"\nactivity: \"{{activity}} ({{hours}}h)\"\n",
            BASE
        ));
        let work = vec![
            Session {
                start: at(2026, 7, 27, 9, 0),
                seconds: 1800,
                activity: "beta".into(),
            },
            Session {
                start: at(2026, 7, 27, 10, 0),
                seconds: 3600,
                activity: "alpha".into(),
            },
            // Same total as "beta", so the two sort alphabetically against each other.
            Session {
                start: at(2026, 7, 27, 11, 0),
                seconds: 1800,
                activity: "aardvark".into(),
            },
        ];
        let report = week_report(&work, at(2026, 7, 27, 0, 0), &settings);
        assert_eq!(
            report.activities[0],
            "alpha (1.00h) | aardvark (0.50h) | beta (0.50h)"
        );
    }

    #[test]
    fn sessions_outside_the_week_are_ignored() {
        let settings = settings_for(BASE);
        let work = vec![Session {
            start: at(2026, 7, 26, 9, 0), // the day before the week starts
            seconds: 3600,
            activity: "outside".into(),
        }];
        let report = week_report(&work, at(2026, 7, 27, 0, 0), &settings);
        assert_eq!(report.total, "0.00");
    }

    #[test]
    fn prefixed_entries_are_stripped_and_others_excluded() {
        let settings = settings_for(&format!("{}prefix: ST\n", BASE));
        let work = vec![
            Session {
                start: at(2026, 7, 27, 9, 0),
                seconds: 3600,
                activity: "ST:mine".into(),
            },
            Session {
                start: at(2026, 7, 27, 10, 0),
                seconds: 3600,
                activity: "MG:theirs".into(),
            },
        ];
        let report = week_report(&work, at(2026, 7, 27, 0, 0), &settings);
        assert_eq!(report.activities[0], "mine");
        assert_eq!(report.hours[0], "1.00"); // the other job's hour is not counted
    }

    #[test]
    fn day_slots_follow_the_weekday_not_the_row_index() {
        let settings = settings_for(BASE);
        let work = vec![Session {
            start: at(2026, 8, 2, 9, 0), // a Sunday
            seconds: 3600,
            activity: "sunday work".into(),
        }];
        // A Monday-start week puts Sunday in the last row...
        let monday_week = week_report(&work, at(2026, 7, 27, 0, 0), &settings);
        let values = field_values(
            &monday_week,
            at(2026, 7, 27, 0, 0).date_naive(),
            at(2026, 8, 2, 0, 0).date_naive(),
            &settings,
        );
        let lookup = |slot: &str| {
            values
                .iter()
                .find(|(k, _)| k == slot)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(lookup("sunday_hours"), "1.00");
        assert_eq!(lookup("monday_hours"), "");

        // ...and a Sunday-start week puts it in the first, yet the slot is still "sunday".
        let sunday_week = week_report(&work, at(2026, 8, 2, 0, 0), &settings);
        let values = field_values(
            &sunday_week,
            at(2026, 8, 2, 0, 0).date_naive(),
            at(2026, 8, 8, 0, 0).date_naive(),
            &settings,
        );
        let lookup = |slot: &str| {
            values
                .iter()
                .find(|(k, _)| k == slot)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(lookup("sunday_hours"), "1.00");
        assert_eq!(lookup("week_start_month"), "08");
        assert_eq!(lookup("week_start_day"), "02");
        assert_eq!(lookup("week_start_year"), "2026");
    }

    #[test]
    fn dash_t_is_the_template_for_pdf_and_the_recipient_for_email() {
        let args = vec!["-t".to_string(), "value".to_string()];
        let pdf = parse_args(&args, false).unwrap();
        assert_eq!(pdf.overrides.template.as_deref(), Some("value"));
        assert!(pdf.overrides.to.is_none());

        let email = parse_args(&args, true).unwrap();
        assert_eq!(email.overrides.to, Some(vec!["value".to_string()]));
        assert!(email.overrides.template.is_none());

        // `ts email` still reaches the template, by its long name or by -T.
        let args = vec!["-T".to_string(), "t.pdf".to_string()];
        assert_eq!(
            parse_args(&args, true)
                .unwrap()
                .overrides
                .template
                .as_deref(),
            Some("t.pdf")
        );
    }

    #[test]
    fn address_options_repeat_and_accept_comma_separated_lists() {
        let args = vec![
            "--to".to_string(),
            "a@x,b@x".to_string(),
            "-t".to_string(),
            "c@x".to_string(),
            "--cc=d@x".to_string(),
        ];
        let parsed = parse_args(&args, true).unwrap();
        assert_eq!(
            parsed.overrides.to,
            Some(vec![
                "a@x".to_string(),
                "b@x".to_string(),
                "c@x".to_string()
            ])
        );
        assert_eq!(parsed.overrides.cc, Some(vec!["d@x".to_string()]));
    }

    #[test]
    fn email_only_options_are_rejected_by_pdf() {
        let args = vec!["--cc".to_string(), "a@x".to_string()];
        assert!(parse_args(&args, false).is_err());
        assert!(parse_args(&args, true).is_ok());
    }

    #[test]
    fn the_week_selector_is_the_only_positional() {
        let parsed = parse_args(&["260727".to_string()], false).unwrap();
        assert_eq!(parsed.input.as_deref(), Some("260727"));
        // A negative rotated-log index is a selector, not an option.
        let parsed = parse_args(&["-1".to_string()], false).unwrap();
        assert_eq!(parsed.input.as_deref(), Some("-1"));
        assert!(parse_args(&["a".to_string(), "b".to_string()], false).is_err());
    }

    #[test]
    fn a_double_dash_ends_option_parsing() {
        let parsed = parse_args(&["--".to_string(), "-1".to_string()], false).unwrap();
        assert_eq!(parsed.input.as_deref(), Some("-1"));
    }

    #[test]
    fn a_lone_dash_selects_stdout_for_the_output() {
        let parsed = parse_args(&["-o".to_string(), "-".to_string()], false).unwrap();
        assert_eq!(parsed.overrides.output.as_deref(), Some("-"));
        let settings = Settings::resolve(&yaml::parse(BASE), &parsed.overrides).unwrap();
        assert!(settings.output.is_none());
    }
}
