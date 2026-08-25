// Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
// Co-author: Claude-AI.

//! Settings for `ts pdf` and `ts email`.
//!
//! A value is looked up in three places, each overriding the one before it: the built-in
//! defaults here, the top level of `~/.config/timesheet.yml`, and the `prefixes:` entry for
//! the prefix in use. Command-line options override all three.
//!
//! The per-prefix layer is what lets one log serve several jobs: each job tags its
//! activities (`ST:`), and its own name, template, addresses and field map live under that
//! tag.

use crate::yaml::Yaml;
use std::collections::HashMap;
use std::path::PathBuf;

/// Timesheet rows, Monday first to match the form's row order. `ts list` reports Sunday
/// first, so the two orderings are deliberately separate.
pub const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// Filename template used when one is needed but none is configured — an email attachment
/// must be named even when `ts pdf` would have written to stdout.
pub const DEFAULT_OUTPUT: &str = "timesheet_{week_end}.pdf";

/// Where a `sendmail`-compatible binary lives on a Unix system, whether the local MTA is
/// Postfix, sendmail, ssmtp or msmtp.
#[cfg(not(windows))]
pub const DEFAULT_SENDMAIL: &str = "/usr/sbin/sendmail";

/// Windows has no MTA and no conventional path for one, so the default is a bare name for
/// `PATH` to resolve — msmtp copied to `sendmail.exe` being the usual way to supply it.
#[cfg(windows)]
pub const DEFAULT_SENDMAIL: &str = "sendmail.exe";

/// Every slot the filled timesheet writes to. Each must resolve to a PDF field name.
pub fn slots() -> Vec<String> {
    let mut out = vec![
        "contractor_name".to_string(),
        "week_start_month".to_string(),
        "week_start_day".to_string(),
        "week_start_year".to_string(),
        "week_end_month".to_string(),
        "week_end_day".to_string(),
        "week_end_year".to_string(),
    ];
    for day in DAYS {
        out.push(format!("{}_hours", day));
        out.push(format!("{}_activities", day));
    }
    out.push("total_hours".to_string());
    out
}

/// Slot-to-field mapping for the stock Strategio form, used when the config names no
/// `fields:` of its own. A different form is adopted by pointing `template:` at it and
/// listing its field names under `fields:`.
const DEFAULT_FIELDS: [(&str, &str); 22] = [
    ("contractor_name", "text_1_16"),
    ("week_start_month", "date_1_17_month"),
    ("week_start_day", "date_1_18_day"),
    ("week_start_year", "date_1_19_year"),
    ("week_end_month", "date_1_20_month"),
    ("week_end_day", "date_1_21_day"),
    ("week_end_year", "date_1_22_year"),
    ("monday_hours", "cell_1_0"),
    ("monday_activities", "cell_1_1"),
    ("tuesday_hours", "cell_1_2"),
    ("tuesday_activities", "cell_1_3"),
    ("wednesday_hours", "cell_1_4"),
    ("wednesday_activities", "cell_1_5"),
    ("thursday_hours", "cell_1_6"),
    ("thursday_activities", "cell_1_7"),
    ("friday_hours", "cell_1_8"),
    ("friday_activities", "cell_1_9"),
    ("saturday_hours", "cell_1_10"),
    ("saturday_activities", "cell_1_11"),
    ("sunday_hours", "cell_1_12"),
    ("sunday_activities", "cell_1_13"),
    ("total_hours", "cell_1_14"),
];

/// Command-line values, each `None` when the option was not given.
#[derive(Default)]
pub struct Overrides {
    pub prefix: Option<String>,
    pub output: Option<String>,
    pub template: Option<String>,
    pub activity: Option<String>,
    pub separator: Option<String>,
    pub zero: Option<String>,
    pub to: Option<Vec<String>>,
    pub cc: Option<Vec<String>>,
    pub from: Option<String>,
    pub reply: Option<String>,
}

/// Everything the two subcommands need, with every fallback already applied.
pub struct Settings {
    pub name: String,
    pub prefix: Option<String>,
    /// Output filename template, or `None` to write the PDF to stdout.
    pub output: Option<String>,
    pub template: PathBuf,
    pub activity: String,
    pub separator: String,
    pub zero: String,
    pub min_font_size: f64,
    pub max_font_size: f64,
    pub fields: HashMap<String, String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub from: Option<String>,
    pub reply: Option<String>,
    pub subject: String,
    pub body: String,
    /// The sendmail(8)-compatible binary the message is piped to.
    pub sendmail: PathBuf,
}

/// Expands a leading `~` to `$HOME` (or the platform home directory, e.g. `%USERPROFILE%` on
/// Windows). Paths in the config are written by hand, so `~` is expected to work there even
/// though no shell has touched them.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir);
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Chooses the prefix to report on: the `-p` option, else the config's `prefix:`, else the
/// sole entry under `prefixes:` when there is exactly one. A config listing several
/// prefixes and naming no default leaves this `None`, and the caller reports every activity.
pub fn resolve_prefix(doc: &Yaml, override_prefix: Option<&str>) -> Option<String> {
    if let Some(p) = override_prefix {
        let p = p.trim();
        return if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        };
    }
    if let Some(p) = doc.get_str("prefix") {
        return Some(p.to_string());
    }
    match doc.get("prefixes") {
        Some(prefixes) => match prefixes.keys().as_slice() {
            [only] => Some(only.to_string()),
            _ => None,
        },
        None => None,
    }
}

/// A scalar looked up in the prefix's section first, then at the top level.
fn scoped<'a>(doc: &'a Yaml, section: Option<&'a Yaml>, key: &str) -> Option<&'a str> {
    section
        .and_then(|s| s.get_str(key))
        .or_else(|| doc.get_str(key))
}

/// A list looked up in the prefix's section first, then at the top level.
fn scoped_list(doc: &Yaml, section: Option<&Yaml>, key: &str) -> Option<Vec<String>> {
    section
        .and_then(|s| s.get_list(key))
        .or_else(|| doc.get_list(key))
}

/// A scalar that may legitimately be empty, so an empty value overrides rather than falls
/// through. `zero: ""` blanks the hours cell on a day with no work, and must not be
/// mistaken for "unset".
fn scoped_allowing_empty<'a>(
    doc: &'a Yaml,
    section: Option<&'a Yaml>,
    key: &str,
) -> Option<&'a str> {
    section
        .and_then(|s| s.get(key))
        .and_then(Yaml::as_str)
        .or_else(|| doc.get(key).and_then(Yaml::as_str))
}

fn scoped_number(doc: &Yaml, section: Option<&Yaml>, key: &str, fallback: f64) -> f64 {
    scoped(doc, section, key)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(fallback)
}

impl Settings {
    /// Merges the config document with the command-line overrides.
    ///
    /// Fails only for `template`, which has no sensible default, and for `name`, which the
    /// form's signature line requires; everything else falls back.
    pub fn resolve(doc: &Yaml, over: &Overrides) -> Result<Settings, String> {
        let prefix = resolve_prefix(doc, over.prefix.as_deref());
        // An explicit empty `--prefix` turns filtering off, but the configured section is
        // still where the template and addresses live, so keep reading it rather than making
        // the user restate all of it on the command line.
        let section_prefix = prefix.clone().or_else(|| {
            over.prefix
                .as_deref()
                .filter(|p| p.trim().is_empty())
                .and_then(|_| resolve_prefix(doc, None))
        });
        let section = section_prefix
            .as_deref()
            .and_then(|p| doc.get("prefixes").and_then(|m| m.get(p)));

        let name = scoped(doc, section, "name")
            .map(str::to_string)
            .ok_or_else(|| {
                "ts: no name configured; set \"name:\" in the config file".to_string()
            })?;

        let template =
            match over
                .template
                .as_deref()
                .or(scoped(doc, section, "template"))
            {
                Some(t) => expand_tilde(t),
                None => return Err(
                    "ts: no PDF template configured; use --template or set \"template:\" in the \
                     config file"
                        .to_string(),
                ),
            };

        // The field map merges rather than replaces, so a config that renames one slot need
        // not restate the other twenty-one.
        let mut fields: HashMap<String, String> = DEFAULT_FIELDS
            .iter()
            .map(|(slot, field)| (slot.to_string(), field.to_string()))
            .collect();
        for source in [doc.get("fields"), section.and_then(|s| s.get("fields"))]
            .into_iter()
            .flatten()
        {
            for slot in source.keys() {
                if let Some(field) = source.get_str(slot) {
                    fields.insert(slot.to_ascii_lowercase(), field.to_string());
                }
            }
        }
        let missing: Vec<String> = slots()
            .into_iter()
            .filter(|slot| !fields.contains_key(slot))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "ts: the \"fields:\" config is missing: {}",
                missing.join(", ")
            ));
        }

        let min_font_size = scoped_number(doc, section, "min_font_size", 5.0);
        let max_font_size = scoped_number(doc, section, "max_font_size", 10.0);
        if min_font_size <= 0.0 || max_font_size < min_font_size {
            return Err(format!(
                "ts: min_font_size ({}) and max_font_size ({}) must be positive with \
                 min_font_size no larger than max_font_size",
                min_font_size, max_font_size
            ));
        }

        Ok(Settings {
            name,
            prefix,
            // No default: with neither `--output` nor `output:` the PDF goes to stdout.
            output: over
                .output
                .clone()
                .or_else(|| scoped(doc, section, "output").map(str::to_string))
                .filter(|o| o != "-"),
            template,
            activity: over
                .activity
                .clone()
                .or_else(|| scoped(doc, section, "activity").map(str::to_string))
                .unwrap_or_else(|| "{activity}".to_string()),
            separator: over
                .separator
                .clone()
                .or_else(|| scoped_allowing_empty(doc, section, "separator").map(str::to_string))
                .unwrap_or_else(|| "; ".to_string()),
            zero: over
                .zero
                .clone()
                .or_else(|| scoped_allowing_empty(doc, section, "zero").map(str::to_string))
                .unwrap_or_default(),
            min_font_size,
            max_font_size,
            fields,
            to: over
                .to
                .clone()
                .or_else(|| scoped_list(doc, section, "to"))
                .unwrap_or_default(),
            cc: over
                .cc
                .clone()
                .or_else(|| scoped_list(doc, section, "cc"))
                .unwrap_or_default(),
            from: over
                .from
                .clone()
                .or_else(|| scoped(doc, section, "from").map(str::to_string)),
            reply: over
                .reply
                .clone()
                .or_else(|| scoped(doc, section, "reply").map(str::to_string)),
            subject: scoped(doc, section, "subject")
                .unwrap_or("Weekly timesheet: {contractor_name}, {week_start} to {week_end}")
                .to_string(),
            body: scoped(doc, section, "body")
                .unwrap_or("Attached is my timesheet for {week_start} through {week_end}.")
                .to_string(),
            sendmail: scoped(doc, section, "sendmail")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map_or_else(|| PathBuf::from(DEFAULT_SENDMAIL), expand_tilde),
        })
    }
}

/// Substitutes `{placeholder}` occurrences. An unknown placeholder is left as written, so a
/// typo shows up in the output instead of aborting a send.
pub fn expand_placeholders(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let name = &rest[open + 1..open + close];
        match values.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&rest[open..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml;

    const CONFIG: &str = "\
name: Top Level Name
template: /tmp/top.pdf
from: me@example.com
prefixes:
  ST:
    name: Prefixed Name
    separator: \"; \"
    zero: \"\"
    to: hr@example.com
    fields:
      total_hours: custom_total
  OT:
    name: Other Name
";

    #[test]
    fn prefix_section_overrides_top_level() {
        let doc = yaml::parse(CONFIG);
        let over = Overrides {
            prefix: Some("ST".to_string()),
            ..Default::default()
        };
        let s = Settings::resolve(&doc, &over).unwrap();
        assert_eq!(s.name, "Prefixed Name");
        assert_eq!(s.from.as_deref(), Some("me@example.com"));
        assert_eq!(s.to, vec!["hr@example.com".to_string()]);
        assert_eq!(s.template, PathBuf::from("/tmp/top.pdf"));
    }

    #[test]
    fn command_line_beats_the_config() {
        let doc = yaml::parse(CONFIG);
        let over = Overrides {
            prefix: Some("ST".to_string()),
            template: Some("/tmp/cli.pdf".to_string()),
            separator: Some(" / ".to_string()),
            to: Some(vec!["someone@example.com".to_string()]),
            ..Default::default()
        };
        let s = Settings::resolve(&doc, &over).unwrap();
        assert_eq!(s.template, PathBuf::from("/tmp/cli.pdf"));
        assert_eq!(s.separator, " / ");
        assert_eq!(s.to, vec!["someone@example.com".to_string()]);
    }

    #[test]
    fn field_map_merges_over_the_defaults() {
        let doc = yaml::parse(CONFIG);
        let over = Overrides {
            prefix: Some("ST".to_string()),
            ..Default::default()
        };
        let s = Settings::resolve(&doc, &over).unwrap();
        assert_eq!(s.fields.get("total_hours").unwrap(), "custom_total");
        assert_eq!(s.fields.get("monday_hours").unwrap(), "cell_1_0");
    }

    #[test]
    fn an_empty_prefix_disables_filtering_but_keeps_the_section() {
        let doc = yaml::parse(CONFIG);
        let over = Overrides {
            prefix: Some(String::new()),
            ..Default::default()
        };
        let s = Settings::resolve(&doc, &over).unwrap();
        // Nothing is filtered out...
        assert_eq!(s.prefix, None);
        // ...but with two prefixes and no `prefix:` key there is no section to fall back to.
        assert_eq!(s.name, "Top Level Name");

        // With a single configured prefix, its settings still apply.
        let doc = yaml::parse(
            "name: Top\ntemplate: /t.pdf\nprefixes:\n  ST:\n    name: Prefixed\n    to: hr@example.com\n",
        );
        let s = Settings::resolve(&doc, &over).unwrap();
        assert_eq!(s.prefix, None);
        assert_eq!(s.name, "Prefixed");
        assert_eq!(s.to, vec!["hr@example.com".to_string()]);
    }

    #[test]
    fn a_sole_configured_prefix_is_the_default() {
        let doc =
            yaml::parse("name: N\ntemplate: /tmp/t.pdf\nprefixes:\n  ST:\n    zero: \"0.00\"\n");
        assert_eq!(resolve_prefix(&doc, None).as_deref(), Some("ST"));
        // With more than one and no `prefix:` key, nothing is assumed.
        let many = yaml::parse(CONFIG);
        assert_eq!(resolve_prefix(&many, None), None);
        // An explicit `prefix:` wins over counting the sections.
        let named = yaml::parse("prefix: OT\nprefixes:\n  ST:\n  OT:\n");
        assert_eq!(resolve_prefix(&named, None).as_deref(), Some("OT"));
    }

    #[test]
    fn an_empty_zero_string_is_kept_rather_than_defaulted() {
        let doc = yaml::parse("name: N\ntemplate: /t.pdf\nzero: \"0.00\"\n");
        let s = Settings::resolve(&doc, &Overrides::default()).unwrap();
        assert_eq!(s.zero, "0.00");
        let s = Settings::resolve(
            &doc,
            &Overrides {
                zero: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.zero, "");
    }

    #[test]
    fn sendmail_is_per_prefix_and_tilde_expanded() {
        let doc = yaml::parse(
            "name: N\ntemplate: /t.pdf\nsendmail: /usr/local/bin/msmtp\nprefixes:\n  ST:\n    sendmail: \"~/.local/bin/sendmail-st\"\n  OT:\n",
        );
        let with_prefix = Settings::resolve(
            &doc,
            &Overrides {
                prefix: Some("ST".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            with_prefix.sendmail,
            expand_tilde("~/.local/bin/sendmail-st")
        );
        // A prefix that names none falls back to the top level...
        let inherited = Settings::resolve(
            &doc,
            &Overrides {
                prefix: Some("OT".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(inherited.sendmail, PathBuf::from("/usr/local/bin/msmtp"));
        // ...and with the key absent entirely, the system binary is used.
        let none = yaml::parse("name: N\ntemplate: /t.pdf\n");
        assert_eq!(
            Settings::resolve(&none, &Overrides::default())
                .unwrap()
                .sendmail,
            PathBuf::from(DEFAULT_SENDMAIL)
        );
    }

    #[test]
    fn missing_template_and_name_are_reported() {
        let doc = yaml::parse("name: N\n");
        assert!(Settings::resolve(&doc, &Overrides::default())
            .err()
            .is_some_and(|e| e.contains("template")));
        let doc = yaml::parse("template: /t.pdf\n");
        assert!(Settings::resolve(&doc, &Overrides::default())
            .err()
            .is_some_and(|e| e.contains("name")));
    }

    #[test]
    fn placeholders_expand_and_unknown_ones_survive() {
        let out = expand_placeholders(
            "timesheet_{name}_{week_end}.pdf",
            &[("name", "Bob"), ("week_end", "2026-08-09")],
        );
        assert_eq!(out, "timesheet_Bob_2026-08-09.pdf");
        assert_eq!(expand_placeholders("a{nope}b", &[]), "a{nope}b");
        assert_eq!(expand_placeholders("plain", &[]), "plain");
    }
}
