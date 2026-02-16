# Timesheet

Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>  
Co-author: Cursor-AI.

Korn shell scripts for tracking work start/stop and reporting time by activity and by day of week.

## Requirements

- `ksh` (Korn shell)
- `awk`
- Timesheet data file: `~/Documents/timesheet.log` (edit `TIMESHEET` in each script to change)

## Data format

The log file contains one entry per line:

- `START|unix_epoch|activity`
- `STOP|unix_epoch`

Start/stop pairs are matched in **LIFO order** (each STOP pairs with the most recent START). The report scripts use these pairs to compute duration and attribute time to activity and day of week.

## Scripts

### start

Records that work is starting **now**. Appends a single START entry.

- **Arguments:** Optional. Any arguments are the activity description; if omitted, activity is `misc/unspecified`.
- **Behavior:** Appends `START|$(date +%s)|activity` to the timesheet. Does not modify existing entries.

### started

Records a work start at a **past time**. First argument is required (the start time); remaining arguments are the activity.

- **Arguments:** `started <start_time> [activity...]`. `start_time` is required (e.g. `"2025-02-16 09:00"` or `"9:00 AM"`). Activity defaults to `misc/unspecified`.
- **Time formats:** GNU `date -d` style, or `YYYY-MM-DD HH:MM[:SS]`, or `HH:%M` (today).
- **Behavior:**
  - **Last entry is START (work in progress), and that START was recorded today:** Replaces that START with the new time and activity (adjusts the current session). Does not add a new line.
  - **Last entry is STOP, your start time is before that stop time, and that STOP was recorded today:** Inserts the new START line **before** that STOP so the pair is correct in the log. Then exits.
  - **Otherwise:** Appends the new START at the end of the file.
- **Constraint:** Does not adjust or insert relative to an entry that was **not** made on the current day (uses local date).

### stop

Records that work is stopping **now**.

- **Arguments:** None.
- **Behavior:**
  - **Last entry is STOP, and that stop was recorded today:** Does **not** simply append another STOP. Inserts a START one second after that stop time, then appends the new STOP. So you get a continuous session from “one second after last stop” until now.
  - **Last entry is STOP from a previous day:** Does **not** modify that entry. Only appends the new STOP.
  - **Last entry is START (or anything else):** Appends the new STOP (normal pairing with the most recent START).
- **Constraint:** Does not adjust an entry that was not made on the current day.

### timesheet

Prints a **plaintext report** to stdout. Does not change the log file.

- **Arguments:** None.
- **Behavior:**
  - **Last entry is START (work in progress):** Treats work as having just stopped for the report: uses the log plus a virtual `STOP|now` so the current session is included in the totals. The file is not modified.
  - **Otherwise:** Uses the log as-is.
- **Output (two parts):**
  1. **By activity:** Percentage of total time per activity, sorted from highest to lowest (e.g. `60.0%  coding`).
  2. **By day of week:** Total hours per weekday (Sunday through Saturday), two decimal places (e.g. `Monday  6.50`). The week is Sunday 00:00:00 through Saturday 23:59:59; each segment is attributed to the day of its start time.

### timeoff

Shows the **stop-work time** that would give an average of 8 hours per day worked (over every day that has at least one completed session).

- **Arguments:** None.
- **Behavior:**
  - If the last entry is STOP (work not in progress), runs `start` (from the same directory as the script) before doing the calculation, so the 8-hour average includes “work starting now.”
  - Computes total hours and number of distinct days with work. If the average is already ≥ 8 hours, prints a message and the current time. Otherwise prints the clock time at which stopping would make the average exactly 8 hours, and how many hours remain.

### install

Copies the five scripts (`start`, `started`, `stop`, `timesheet`, `timeoff`) into a directory and makes them executable.

- **Usage:** `./install [install_dir] [repo_path]`. Both arguments are optional.
- **Arguments:**
  - **install_dir:** If given, scripts are installed into this directory (created if needed). Must be writable.
  - **repo_path:** If given, path to the repository containing the scripts (default: directory of `install`).
- **Behavior when install_dir is omitted:** Iterates over directories in `PATH` in order and installs into the **first directory that is writable** by the current user. If none are writable, prints an error and exits without installing.
- **Behavior when install_dir is given:** Uses that directory (creates it with `mkdir -p` if it does not exist). Exits with an error if the directory cannot be created or is not writable.
- Exits with an error if any of the five scripts is missing in the repo directory.

## Install

From the repository directory:

```sh
./install
```

This installs into the first writable directory on your `PATH`. To install into a specific directory (e.g. `~/bin`):

```sh
./install ~/bin
```

Or copy and chmod manually:

```sh
cp start started stop timesheet timeoff ~/bin/
chmod +x ~/bin/start ~/bin/started ~/bin/stop ~/bin/timesheet ~/bin/timeoff
```

Ensure `TIMESHEET` in each script points to your log file (default: `~/Documents/timesheet.log`). For `timeoff` to invoke `start` when work is stopped, the scripts should be in the same directory (e.g. all in `~/bin`).
