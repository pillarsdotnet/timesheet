# Timesheet

Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>  
Co-author: Cursor-AI.

CLI for tracking work start/stop and reporting time by activity and by day of week.

## Requirements

- Timesheet data file: `~/Documents/timesheet.log` (edit `DEFAULT_TIMESHEET` in `src/main.rs` and rebuild to change)

## Data format

The log file contains one entry per line:

- `START|unix_epoch|activity`
- `STOP|unix_epoch`

Start/stop pairs are matched in **LIFO order** (each STOP pairs with the most recent START). The report uses these pairs to compute duration and attribute time to activity and day of week.

## ts command

The **`ts`** command takes a required subcommand as its first argument:

| Subcommand   | Description |
|-------------|-------------|
| `start`     | Record work start **now**. Optional args = activity (default: misc/unspecified). |
| `stop`      | Record work stop **now**. |
| `list`      | Plaintext report: % time per activity, hours per day of week; if work in progress, show current task/duration. |
| `started`   | Record a work start at a **past time**. Args: `ts started <start_time> [activity...]`. |
| `timeoff`   | Show stop-work time for 8h/day average; if last entry is STOP, starts work for the calculation. |
| `workalias` | Interactively replace activity text in START entries from the current week. Args: `ts workalias <pattern> <replacement>`. |
| `install`   | Copy the binary to a directory on PATH. Args: `ts install [install_dir] [repo_path]`. |
| `rotate`    | Rename `timesheet.log` to `timesheet.YYMMDDHHMM` where the timestamp is the date/time of its most recent entry. |

### start

Appends `START|$(date +%s)|activity` to the timesheet. Does not modify existing entries.

### started

- **Time formats:** GNU `date -d` style, or `YYYY-MM-DD HH:MM[:SS]`, or `HH:%M` (today).
- **Last entry is START recorded today:** Replaces that START with the new time and activity.
- **Last entry is STOP recorded today and start time &lt; stop time:** Inserts the new START before that STOP.
- **Otherwise:** Appends the new START at the end. Only adjusts entries made on the current day.

### stop

Records that work is stopping **now**.

- **Arguments:** None.
- **Behavior:**
  - **Last entry is STOP, and that stop was recorded today:** Does **not** simply append another STOP. Inserts a START one second after that stop time, then appends the new STOP. So you get a continuous session from “one second after last stop” until now.
  - **Last entry is STOP from a previous day:** Does **not** modify that entry. Only appends the new STOP.
  - **Last entry is START (or anything else):** Appends the new STOP (normal pairing with the most recent START).
- **Constraint:** Does not adjust an entry that was not made on the current day.

### list

- **Last entry is START (work in progress):** Uses log plus a virtual `STOP|now` for the report; file not modified. Appends a line with current task, start time, and duration.
- **Output:** (1) By activity: percentage per activity, high to low. (2) By day of week: hours per weekday (Sun–Sat). Week = Sunday 00:00:00 through Saturday 23:59:59.

### timeoff

Shows the **stop-work time** that would give an average of 8 hours per day worked (over every day that has at least one completed session).

- **Arguments:** None.
- **Behavior:**
  - If the last entry is STOP (work not in progress), runs `start` (from the same directory as the script) before doing the calculation, so the 8-hour average includes “work starting now.”
  - Computes total hours and number of distinct days with work. If the average is already ≥ 8 hours, prints a message and the current time. Otherwise prints the clock time at which stopping would make the average exactly 8 hours, and how many hours remain.

### workalias

Searches for START entries from the current week whose activity matches the pattern. For each match, echoes original and replaced form, prompts `Replace (y/n)`; `y`/`Y` applies the replacement. Errors if no matches this week.

### install

- **install_dir omitted:** Installs the binary into the first writable directory on `PATH`.
- **install_dir given:** Installs into that directory (created if needed). Exits with an error if the binary is missing in the repo path. Usage: `ts install [install_dir] [repo_path]`.

### rotate

Renames the timesheet log to `timesheet.YYMMDDHHMM` using the timestamp of the log's most recent entry (START or STOP). Errors if the log is missing or has no valid entries.

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

### Documentation

[Rustdoc](https://doc.rust-lang.org/rustdoc/)-compatible comments are in the Rust source. Generate and open the docs with:

```sh
cargo doc --no-deps --open
```

Output is under `target/doc/ts/`.
