# Timesheet

Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
Co-author: Cursor-AI and GitHub Copilot.

CLI for tracking work start/stop and reporting time by activity and by day of week.

## Motivation

In the 90's, I had a boss who required me to turn in a detailed weekly timesheet
listing exactly how much time I spent on each task, assigned or unassigned. As
a borderline austistic, the idea of fudging or guessing at such a report was
deeply troubling. So I self-assigned a task to write quick-and-dirty program
that pops up every five minutes and asks what I've been doing. I called it
"bugme".

My present position has similar reporting requirements, so I have recreated that
old program with improvements. I took the opportunity to simultaneously scratch
two itches: AI and the Rust Programming Language. So I used an AI agent almost
exclusively to write the program code, both in its original form as a set of
Korn Shell scripts, and in its current form as a Rust program.

One of these days, when I find the time, I'll read through the code and try to
figure out how it works. For now I'm just glad that it does.

## Requirements

- Timesheet data file: `~/Documents/timesheet.log` (edit `DEFAULT_TIMESHEET` in `src/main.rs` and rebuild to change)
- **macOS:** no extra dependencies (reminder dialogs use built-in AppleScript/AppKit).
- **Linux (KDE/Ubuntu/etc.):** the reminder prompt uses, in order of preference:

  1. **A single-click chooser** built with **Python 3 + PyQt** (`python3` plus `python3-pyqt6` or `python3-pyqt5`). This is the preferred experience: each entry acts on a single click with no OK/Cancel buttons (Qt, native on Wayland). Install e.g. `sudo apt install python3-pyqt6`.
  2. **A fallback list dialog** via `kdialog` (KDE/Plasma) or `zenity` (GNOME/other) when PyQt is unavailable — a select-then-OK list. Install whichever matches your desktop, e.g. `sudo apt install kdialog` or `sudo apt install zenity`.

  `notify-send` (from `libnotify-bin`) is used for the "reminders stopped" notification, and `systemd --user` for `ts autostart`. With no chooser available at all, reminders fall back to closing the open session with a STOP at the next interval (one STOP, not one per interval) and `ts start` defaults to misc/unspecified instead of prompting.

- **Windows:** core commands (`start`, `stop`, `list`, `sprint`, `tail`, `alias`/`rename`/`prefix`, `rotate`, `migrate`, `timeoff`, `edit`, `pdf`, `email`) build and run with no extra dependencies; the log lives at `%USERPROFILE%\Documents\timesheet.log`. There is no reminder dialog, background reminder daemon, or `ts autostart` support yet, so `ts start` with no activity always defaults to misc/unspecified instead of prompting, and `ts autostart` errors as unsupported (matching its behavior on any platform other than macOS/Linux). `ts edit` falls back to `notepad` (instead of `vi`) when `$EDITOR`/`$VISUAL` are unset. `ts help` has no `groff`/`less` to page through, so it renders the man page as plain text through `more` instead.

- **`ts pdf` and `ts email`:** no extra runtime dependencies. PDF filling and SMTP (including STARTTLS, via rustls with bundled roots) are built into the binary; nothing needs to be installed alongside it. `ts email` runs `smtp_password_command` through `sh` (`cmd` on Windows), so whatever that command needs — `pass`, `secret-tool`, `op` — must be on PATH.

## Data format

The log file contains one entry per line. The timestamp is the **first** field, in strict ISO 8601 (RFC 3339) with microsecond precision and a local UTC offset:

- `ISO8601_timestamp|START|activity`
- `ISO8601_timestamp|STOP`

For example:

```text
2026-08-03T08:00:00.000000-04:00|START|ST:Welcome session
2026-08-03T09:00:00.000000-04:00|STOP
```

The wall-clock time in the recorded offset is read back as local time without converting through UTC, so a log stays readable after a timezone change.

Start/stop pairs are matched in **LIFO order** (each STOP pairs with the most recent START). A START also closes any session still open before it, so consecutive STARTs each contribute their own interval. The report uses these pairs to compute duration and attribute time to activity and day of week.

Earlier versions wrote the kind first, as `START|ISO8601_timestamp|activity` and `STOP|ISO8601_timestamp`. **`ts migrate`** converts every `timesheet.*` file in the log directory to the current field order; lines already in it are left alone.

## Configuration

Optional settings live in **`~/.config/timesheet.yml`** (or `$XDG_CONFIG_HOME/timesheet.yml`; `$TS_CONFIG` overrides both, and a `timesheet.yaml` sibling is used if no `.yml` exists). The file does not exist by default, and every setting except the `pdf`/`email` template has a default, so no configuration is needed to track time.

Only a small YAML subset is understood: `key: value` pairs, `#` comments, optional quotes, indented nesting, and sequences (either `- item` lines or `[a, b]`). Unknown keys are ignored, and a value that can't be understood prints a warning on stderr and falls back to the default. Quote a value whose leading or trailing spaces matter, such as `separator: "; "`.

### `rotate` — when a new timesheet week begins

`ts` rotates `timesheet.log` to `timesheet.YYMMDD` at the start of each week, so each rotated file holds exactly one work week. By default the week begins **Sunday at 00:00** local time. If your employer's week runs Monday through Sunday — rotating at midnight between Sunday night and Monday morning — say so:

```yaml
# ~/.config/timesheet.yml
rotate:
  day: monday
  time: "00:00"
```

- **`day`** — weekday name or three-letter abbreviation, any case (`monday`, `Mon`, `SUNDAY`). Default: `sunday`.
- **`time`** — `HH:MM`, `HH:MM:SS`, or a bare hour, in local time. Default: `00:00`.

A scalar shorthand works too: `rotate: monday`, or `rotate: "fri 17:00"` for a week that turns over Friday at 5 pm.

The rotation boundary is checked by `ts start`, `ts stop`, `ts started`, `ts timeoff` and the reminder daemon: if the log's last entry falls before the most recent boundary, the log is rotated before the new entry is recorded. `ts rotate` run by hand always rotates, whatever the boundary. The same boundary defines "this week" for `ts alias`, and the week that `ts pdf` and `ts email` report.

### Settings for `ts pdf` and `ts email`

Each of these may be written at the top level, or under `prefixes:` → _PREFIX_ so that it applies only when that prefix is in use. A per-prefix value beats the top-level one, and a command-line option beats both — which is how one log can serve several jobs, each tagging its activities (`ST:Setup Jira`) and keeping its own name, template, addresses and field map.

| Setting                                   | Meaning                                                                                                                                 |
| ----------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `name`                                    | Full name as it should appear on the timesheet. Required.                                                                               |
| `prefix`                                  | Default for `--prefix`. When absent and exactly one prefix is listed under `prefixes:`, that one is used.                               |
| `template`                                | Default for `--template`: path to the form-fillable PDF. No built-in default.                                                           |
| `output`                                  | Default for `--output`. When absent, `ts pdf` writes to stdout.                                                                         |
| `activity`, `separator`, `zero`           | Defaults for `--activity`, `--separator` and `--zero`.                                                                                  |
| `to`, `cc`                                | Default recipients; each is either one address or a sequence of them.                                                                   |
| `from`, `reply`                           | Default sender and Reply-To addresses.                                                                                                  |
| `subject`, `body`                         | Message templates, taking the same placeholders as `output` plus `{total_hours}`.                                                       |
| `min_font_size`, `max_font_size`          | Shrink-to-fit range in points (default 5 and 10).                                                                                       |
| `fields`                                  | Maps each timesheet slot to a form-field name. Defaults suit the stock form; anything listed here replaces only the slots it names.     |
| `smtp_host`, `smtp_port`, `smtp_starttls` | Relay to submit through (default `localhost:25`). `smtp_starttls` defaults to true on port 587 and false elsewhere.                     |
| `smtp_user`, `smtp_password_command`      | Credentials, if the relay wants them. `smtp_password_command` is a shell command that prints the password, so no secret is stored here. |

The slots that `fields` maps are `contractor_name`, `week_start_month`/`_day`/`_year`, `week_end_month`/`_day`/`_year`, `<weekday>_hours` and `<weekday>_activities` for each of the seven weekdays, and `total_hours`. The field names of a different form can be listed with `mutool show form.pdf form | grep Name:`.

```yaml
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
```

A Gmail or Google Workspace account needs `smtp.gmail.com` on port 587 with STARTTLS, the full address as `smtp_user`, and an **App Password** — an account password is rejected. Gmail also rewrites `From` to the authenticated account unless the address is a verified "Send mail as" alias, so set `reply` when the two differ; `ts email` warns if you have not.

## Filling and sending the timesheet

`ts pdf` aggregates one week and fills a form-fillable PDF with it; `ts email` does the same and mails the result as an attachment.

```console
ts pdf > timesheet.pdf          # the week just worked, to stdout
ts pdf -o ~/Documents           # into a directory, using the configured file name
ts pdf -1                       # the most recently rotated week
ts pdf 260727                   # the week containing 2026-07-27
ts email                        # fill and send in one step
```

The optional week argument takes the same forms as `ts list`: a log file path, `log` for the current log, a negative rotated-log index (`-1` is the most recently rotated), or a date (`YYYYMMDD`, `YYMMDD`, `M/D`). With no argument, the week in progress is reported on its final day and the most recently completed week on any other day — so a run late on the last day of the week, or at any time in the days after it, both report the week just worked.

Hours are credited to the day each session started on, exactly as `ts list` accounts for them, and the printed total is the sum of the day figures **as rounded**, so the column adds up on paper. Every log is read, so a week that straddles a rotation still reports in full.

`--prefix ST` reports only activities beginning `ST:` and strips that tag, so an entry logged as `ST:Setup Jira` is reported as `Setup Jira`. Entries without the tag belong to another job and are left out entirely — their hours as well as their descriptions. An empty prefix (`-p ""`) reports every entry unchanged, while still reading the settings of the prefix the configuration would otherwise have selected — so the template and addresses need not be restated.

`ts list` takes `-p/--prefix` too, filtering and stripping the same way, so `ts list -p ST` previews on screen the hours `ts pdf -p ST` will report. It reads no configuration, so the option is the only thing that filters: with no `--prefix`, `ts list` reports every activity as it always has, and an empty `-p ""` does the same.

`--output` accepts `{date}`, `{week_start}`, `{week_end}`, `{name}` and `{prefix}`, an existing directory (which receives the configured file name), or `-` for stdout. Writing a PDF to a terminal is refused.

Text is shrunk to fit its cell and, in the activity columns, wrapped; a description that cannot fit even at `min_font_size` warns on stderr and is clipped. Appearance streams are generated rather than left to the viewer, so the filled text shows up in every reader, when printed, and to text extractors.

If a send fails, the finished PDF is kept on disk rather than discarded, so the message can be retried without rebuilding a week that may have moved on.

## ts command

The **`ts`** command takes a required subcommand as its first argument. Full documentation: **`ts help`** or **`ts manpage`**.

Subcommands (alphabetical):

| Subcommand  | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ----------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `alias`     | Interactively replace activity text in START entries from the current week. Matches the search text literally first; if nothing matches and the search text is a valid regex, falls back to regex search-and-replace.                                                                                                                                                                                                                                                                                                                    |
| `autostart` | Register `ts start` on login and `ts stop` on logout/shutdown (macOS: LaunchAgents + logout hook; Linux: systemd user units + a system-level logout hook). Optional first argument: interval (e.g. `5s`, `3m`) to set reminder interval and start the daemon in this session. Without interval: starts the daemon if needed and shows the current reminder interval. Use `ts autostart uninstall` to remove. Not supported on Windows.                                                                                                                             |
| `edit`      | Open the timesheet log (`$HOME/Documents/timesheet.log`) in your editor, taken from `$EDITOR` (then `$VISUAL`, else `vi` — `notepad` on Windows).                                                                                                                                                                                                                                                                                                                                                                                                               |
| `email`     | Fill the timesheet PDF as `pdf` does and mail it as an attachment. Takes every `pdf` option, except that `-t` means `--to` here (the template is `--template` or `-T`), plus `-c/--cc`, `-f/--from` and `-r/--reply`. See [Filling and sending the timesheet](#filling-and-sending-the-timesheet).                                                                                                                                                                                                                                       |
| `help`      | Show the manual page in a pager (groff -man -Tascii \| less; on Windows, rendered as plain text and paged through `more`).                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `install`   | Copy the binary (and on macOS the embedded icon as `ts-icon.svg`) to a directory on PATH. Optional: `ts install [install_dir] [repo_path]`. Works without the source repo on macOS (icon is embedded).                                                                                                                                                                                                                                                                                                                                   |
| `interval`  | Set or show the reminder daemon interval (e.g. `3`, `3m`, `100s`, `1h30m`). With an argument, sets the interval and restarts the daemon.                                                                                                                                                                                                                                                                                                                                                                                                 |
| `list`      | Plaintext report: % time per activity, hours per day of week; optional file/extension, date, or negative rotated-log index (e.g. `ts list 2/19`, `ts list 260220`, `ts list -1`) to select a log. If work in progress, shows current task and duration. `-p/--prefix PREFIX` reports only one job's activities, as for `pdf`.                                                                                                                                                                                                            |
| `manpage`   | Output the Unix manual page in groff format to stdout.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `pdf`       | Fill a form-fillable PDF template with one week of the timesheet and write it to a file or to stdout. Optional file/extension, date, or negative rotated-log index selects the week, exactly as for `list`. Options: `-p/--prefix`, `-o/--output`, `-t/--template`, `-a/--activity`, `-s/--separator`, `-z/--zero`. See [Filling and sending the timesheet](#filling-and-sending-the-timesheet).                                                                                                                                         |
| `prefix`    | Prepend `<prefix>:` to this week's activities matching a pattern. `ts prefix foo bar` is equivalent to `ts alias bar foo:bar`, prompting per match just like `alias`.                                                                                                                                                                                                                                                                                                                                                                    |
| `rebuild`   | Build from source and install into the directory of the running binary. Optional directory argument; see `ts help`.                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `uninstall` | Stop the reminder daemon, remove autostart hooks, optionally remove timesheet log files, then remove `ts-icon.svg` and the `ts` binary from the install directory.                                                                                                                                                                                                                                                                                                                                                                       |
| `rename`    | Same as `alias`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `reminder`  | Alias for `interval`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `restart`   | Alias for `interval` (with no argument, reports current interval and restarts the daemon).                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `rotate`    | Rename `timesheet.log` to `timesheet.YYMMDD` using the earliest entry's date; if last entry is START, appends a STOP no later than one reminder interval after that entry first. If a file for that date already exists, appends to it. Happens automatically at the start of each week — see [Configuration](#rotate--when-a-new-timesheet-week-begins).                                                                                                                                                                                |
| `start`     | Record work start **now**. With no activity: shows the reminder dialog to pick/enter an activity (macOS, or Linux with `kdialog`/`zenity` installed); otherwise defaults to misc/unspecified (always, on Windows, which has no chooser yet). If a session is already open, a STOP is added only when that START is more than one reminder interval old (capped to one interval after it, leaving the time you were away unbilled) — otherwise the new START closes the previous session on its own, since pairs match in LIFO order. Starts the reminder daemon if not already running (no-op on Windows). |
| `started`   | Record a work start at a **past time**. Args: `ts started <start_time> [activity...]`. Time formats: e.g. `YYYY-MM-DD HH:MM`, `HH:MM`, or GNU date -d style.                                                                                                                                                                                                                                                                                                                                                                             |
| `stop`      | Record work stop at **now** or at an optional stop time. If the last entry is already STOP and no time is given, nothing happens; if a time is given, the last STOP is amended. If the last entry is START, appends the new STOP. When a stop is recorded, stops the reminder daemon and shows a dialog that reminders have been stopped (skipped during logout/shutdown).                                                                                                                                                               |
| `stopped`   | Alias for `stop`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `tail`      | Latest ten log entries with timestamps in local time; START lines show duration. Consecutive STARTs with the same activity are collapsed, then last 10 shown. Optional file/extension or date match to select a log.                                                                                                                                                                                                                                                                                                                     |
| `timeoff`   | Show the stop-work time for an 8 h/day average. Requires only a START entry (work in progress); no completed session on the current day is required. If the log is empty or the last entry is STOP, appends a START first.                                                                                                                                                                                                                                                                                                               |

### Reminder daemon

- **`ts start`** starts the reminder daemon if it is not already running. With no activity, `ts start` shows the reminder chooser immediately to pick/enter an activity (macOS via AppleScript/AppKit; Linux via the PyQt single-click chooser, falling back to `kdialog`/`zenity`). While this foreground chooser is open no daemon runs, so it cannot pop a second window; a fresh daemon starts once you pick. The daemon prompts “What are you working on?” at the configured interval.
- **Chooser (Linux, PyQt):** the window covers the full screen and stays on top, with the choices in a centered panel. A single click acts immediately — **Stop Work** records a STOP and stops reminders; an **activity** records a START for it and closes the window; **Enter new activity…** opens an input box where a non-empty entry (press Enter) records that activity and closes everything, while a blank entry returns you to the list.
- **`ts stop`** (when it records a stop) stops the reminder daemon and shows a dialog that reminders have been stopped (skipped during logout/shutdown).
- **`ts interval`** or **`ts restart [duration]`** sets or shows the interval and restarts the daemon.
- **Reminder behavior:** If a reminder goes unanswered for **one reminder interval**, a STOP is recorded at **the time the reminder appeared** — not when the interval expired. That timestamp is used exactly, without the one-interval cap: the reminder appears one interval after your previous entry, so it already marks the last moment you were known to be working. The reminder is then **left on screen** rather than dismissed (macOS also brings it back to the front). When you get back to your desk and pick an activity, that START is recorded at your return time, so the time you were away sits between the STOP and the new START — unbilled — and your return is logged accurately. No second STOP is added while work is already stopped, so leaving the screen unattended records a single STOP rather than one per interval. The reminder window **covers the full screen and stays on top** (macOS and Linux), so a mouse action in progress when it appears cannot accidentally hide it. If the reminder or “Enter new activity” dialog is dismissed without choosing (e.g. closed, Escape), it re-shows immediately. The “Enter new activity” text dialog has no timeout. At logout/shutdown the open session is stopped: on macOS the daemon records STOP when launchd sends it SIGTERM (capped to one interval after the latest entry); on Linux the systemd session unit’s `ExecStop` runs `ts stop` instead, and the daemon stays silent on SIGTERM (systemd may signal it during ordinary teardown, so a STOP there would be spurious).
- **Automatic STOP cap:** Whenever a STOP is added automatically (a missed shutdown reconciled at the next `ts start`/`ts autostart`, closing the previous session before a new START, or `ts rotate`), its timestamp is capped to no more than **one reminder interval** after the latest log entry — the interval is how often you’re prompted (default 5 minutes; see `ts interval`). So forgetting to stop never records work all night: the session ends at most one interval after your last logged activity. An unanswered reminder is the one exception, and needs no cap: its STOP is stamped at the moment the reminder appeared, which is already one interval after the previous entry.
- **`ts autostart [interval]`** (macOS/Linux) registers `ts start` at login and `ts stop` at logout/shutdown. An optional interval (e.g. `5s`, `3m`) sets the reminder interval and starts the daemon in this session so the reminder appears soon. Without interval: starts the daemon if needed and shows the current reminder interval. Startup skips a new START if the last log entry is a STOP less than 60 seconds old, and if startup finds a non-STOP event more than 5 minutes old it backfills a STOP one reminder interval after that event before recording the new START. It also installs a **logout hook** as a second guarantee that STOP is recorded at logout/shutdown: on macOS via `com.apple.loginwindow LogoutHook`, on Linux via a system-level systemd unit (`ts-logout-<uid>.service`) whose `ExecStop` runs `ts stop` before `shutdown.target`. Installing the hook needs administrator access, so `ts autostart` prints the `sudo` command and offers to run it; if you decline, run the printed command yourself. Once the hook is present, later runs skip it. `ts autostart uninstall` offers to remove it (also via `sudo`).

## Install

From the repository directory:

```sh
cargo build --release && ./target/release/ts install
```

To install into a specific directory (e.g. `~/bin`): `ts install ~/bin`. Or copy manually:

```sh
cp target/release/ts ~/bin/ts
chmod +x ~/bin/ts
```

The binary uses `$HOME/Documents/timesheet.log` by default (`%USERPROFILE%\Documents\timesheet.log` on Windows, where `ts install` writes `ts.exe` and no `chmod` step is needed).

## Build from source

Build with [Rust](https://rustup.rs) installed:

```sh
cargo build --release
```

The binary is produced at `target/release/ts` (or `target/debug/ts` for `cargo build`). See [INSTALL.md](INSTALL.md) for full instructions.

To set up the full toolchain (Rust components, git hooks) and run the checks, see [CONTRIBUTING.md](CONTRIBUTING.md).

### Commit messages

The CI lint workflow checks commit messages with [commitlint](https://commitlint.js.org/) (Conventional Commits). Use a leading type and optional scope, e.g. `feat(macos): add dock icon` or `fix: record STOP on shutdown`. See `.commitlintrc.yaml` and [Conventional Commits](https://www.conventionalcommits.org/).

### Documentation

[Rustdoc](https://doc.rust-lang.org/rustdoc/)-compatible comments are in the Rust source. Generate and open the docs with:

```sh
cargo doc --no-deps --open
```

Output is under `target/doc/ts/`.

For command-line usage, run **`ts help`** or **`ts manpage`**.
