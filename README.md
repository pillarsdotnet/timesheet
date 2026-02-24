# Timesheet

Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>  
Co-author: Cursor-AI.

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
two itches: AI and the Rust Programming Language. So I used Cursor-AI almost
exclusively to write the program code, both in its original form as a set of
Korn Shell scripts, and in its current form as a Rust program.

One of these days, when I find the time, I'll read through the code and try to
figure out how it works. For now I'm just glad that it does.

## Requirements

- Timesheet data file: `~/Documents/timesheet.log` (edit `DEFAULT_TIMESHEET` in `src/main.rs` and rebuild to change)

## Data format

The log file contains one entry per line:

- `START|unix_epoch|activity`
- `STOP|unix_epoch`

Start/stop pairs are matched in **LIFO order** (each STOP pairs with the most recent START). The report uses these pairs to compute duration and attribute time to activity and day of week.

## ts command

The **`ts`** command takes a required subcommand as its first argument. Full documentation: **`ts help`** or **`ts manpage`**.

Subcommands (alphabetical):

| Subcommand  | Description                                                                                                                                                                                                                                                                           |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `alias`     | Interactively replace activity text in START entries from the current week (regex pattern and replacement).                                                                                                                                                                           |
| `autostart` | Register `ts start` on login and `ts stop` on logout/shutdown (macOS: LaunchAgents + logout hook; Linux: systemd user). Optional first argument: interval (e.g. `5s`, `3m`) to set reminder interval and start the daemon in this session. Use `ts autostart uninstall` to remove. |
| `help`      | Show the manual page in a pager (groff -man -Tascii \| less).                                                                                                                                                                                                                         |
| `install`   | Copy the binary (and on macOS the embedded icon as `ts-icon.svg`) to a directory on PATH. Optional: `ts install [install_dir] [repo_path]`. Works without the source repo on macOS (icon is embedded).                                                                                |
| `interval`  | Set or show the reminder daemon interval (e.g. `3`, `3m`, `100s`, `1h30m`). With an argument, sets the interval and restarts the daemon.                                                                                                                                              |
| `list`      | Plaintext report: % time per activity, hours per day of week; optional file/extension or date (e.g. `ts list 2/19` or `ts list 260220`) to select a log. If work in progress, shows current task and duration.                                                                        |
| `manpage`   | Output the Unix manual page in groff format to stdout.                                                                                                                                                                                                                                |
| `rebuild`   | Build from source and install into the directory of the running binary. Optional directory argument; see `ts help`.                                                                                                                                                                   |
| `uninstall` | Stop the reminder daemon, remove autostart hooks, optionally remove timesheet log files, then remove `ts-icon.svg` and the `ts` binary from the install directory.                                                                                                                    |
| `rename`    | Same as `alias`.                                                                                                                                                                                                                                                                      |
| `reminder`  | Alias for `interval`.                                                                                                                                                                                                                                                                 |
| `restart`   | Alias for `interval` (with no argument, reports current interval and restarts the daemon).                                                                                                                                                                                            |
| `rotate`    | Rename `timesheet.log` to `timesheet.YYMMDD` using the most recent entry's date; if last entry is START, appends a STOP first. If a file for that date already exists, appends to it.                                                                                                 |
| `start`     | Record work start **now**. Optional activity (default: misc/unspecified). Starts the reminder daemon if not already running.                                                                                                                                                          |
| `started`   | Record a work start at a **past time**. Args: `ts started <start_time> [activity...]`. Time formats: e.g. `YYYY-MM-DD HH:MM`, `HH:MM`, or GNU date -d style.                                                                                                                          |
| `stop`      | Record work stop at **now** or at an optional stop time. If the last entry is already STOP and no time is given, nothing happens; if a time is given, the last STOP is amended. If the last entry is START, appends the new STOP. When a stop is recorded, stops the reminder daemon. |
| `stopped`   | Alias for `stop`.                                                                                                                                                                                                                                                                     |
| `tail`      | Latest ten log entries with timestamps in local time; START lines show duration (or \"in progress\"). Consecutive STARTs with the same activity are collapsed, then last 10 shown. Optional file/extension or date (same as `ts list`) to select a log.                                                                                                                                                            |
| `timeoff`   | Show the stop-work time for an 8 h/day average. Requires only a START entry (work in progress); no completed session on the current day is required. If the log is empty or the last entry is STOP, appends a START first.                                                            |

### Reminder daemon

- **`ts start`** starts the reminder daemon if it is not already running (it prompts “What are you working on?” at the configured interval).
- **`ts stop`** (when it records a stop) stops the reminder daemon.
- **`ts interval`** or **`ts restart [duration]`** sets or shows the interval and restarts the daemon.
- **`ts autostart [interval]`** (macOS/Linux) registers `ts start` at login and `ts stop` at logout/shutdown. An optional interval (e.g. `5s`, `3m`) sets the reminder interval and starts the daemon in this session so the reminder appears soon. On macOS it also installs a **logout hook** so STOP is recorded when you log out or shut down; if the hook cannot be set (sudo required), the command to run manually is printed—run it once to enable STOP on logout/shutdown. Once the hook is registered, later runs skip the sudo prompt.

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

The binary uses `$HOME/Documents/timesheet.log` by default.

## Build from source

Build with [Rust](https://rustup.rs) installed:

```sh
cargo build --release
```

The binary is produced at `target/release/ts` (or `target/debug/ts` for `cargo build`). See [INSTALL.md](INSTALL.md) for full instructions.

### Commit messages

The CI lint workflow checks commit messages with [commitlint](https://commitlint.js.org/) (Conventional Commits). Use a leading type and optional scope, e.g. `feat(macos): add dock icon` or `fix: record STOP on shutdown`. See `.commitlintrc.yaml` and [Conventional Commits](https://www.conventionalcommits.org/).

### Documentation

[Rustdoc](https://doc.rust-lang.org/rustdoc/)-compatible comments are in the Rust source. Generate and open the docs with:

```sh
cargo doc --no-deps --open
```

Output is under `target/doc/ts/`.

For command-line usage, run **`ts help`** or **`ts manpage`**.
