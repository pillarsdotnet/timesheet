# Building and installing from source

Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
Co-author: Cursor-AI.

This document describes how to build and install the **timesheet** CLI from the repository.

## Prerequisites

- **Rust** toolchain (install from [rustup.rs](https://rustup.rs))
- A directory on your `PATH` (e.g. `~/bin`)

## Getting the source

Clone the repository (or download and extract an archive):

```sh
git clone https://github.com/pillarsdotnet/timesheet.git
cd timesheet
```

## Build and install

### 1. Build the release binary

```sh
cargo build --release
```

The binary is written to `target/release/timesheet` (or `target/debug/timesheet` if you use `cargo build` without `--release`).

The first build fetches crates from crates.io, so it needs network access; later builds do not. `timesheet pdf` and `timesheet email` bring in PDF and SMTP support (`lopdf` and `lettre`, the latter with rustls and bundled roots), which is most of the binary's size and of the build time. TLS is pure Rust, so no system OpenSSL or certificate store is required, on Linux, macOS, or Windows.

On Windows, `cargo build --release` produces `target\release\timesheet.exe`. Core commands (`start`, `stop`, `list`, `sprint`, `tail`, `alias`/`rename`/`prefix`, `rotate`, `migrate`, `timeoff`, `edit`, `pdf`, `email`) work the same as on Linux/macOS. The reminder daemon, reminder dialog, and `timesheet autostart` are not implemented on Windows yet, so `timesheet start` with no activity always defaults to misc/unspecified rather than prompting.

### 2. Install the binary

**Using the binary's install subcommand** (run from the repo so it can find itself):

```sh
./target/release/timesheet install
# or into a specific directory:
./target/release/timesheet install ~/bin
```

**Or copy manually:**

```sh
cp target/release/timesheet ~/bin/timesheet
chmod +x ~/bin/timesheet
```

Ensure `~/bin` (or your chosen directory) is on your `PATH`.

The `install` subcommand (not the manual copy) also registers a point-and-click way to start work:
on Linux a "Timesheet" application-menu entry (`~/.local/share/applications/timesheet.desktop`) that
runs `timesheet start`, and on Windows a "Start Timesheet" Start Menu shortcut. Re-running `install`
after moving the binary rewrites them to the new location.

## Autostart (optional, macOS/Linux only)

Not available on Windows (`timesheet autostart` errors as unsupported there). To run **`timesheet start`** at login and **`timesheet stop`** at logout/shutdown on macOS or Linux:

```sh
timesheet autostart
```

You can pass an interval (e.g. **`timesheet autostart 5s`**) to set the reminder interval and start the daemon in this session. Startup skips a new START if the last log entry is a STOP less than 60 seconds old, and if it finds a non-STOP event more than 5 minutes old it backfills a STOP 5 minutes after that event before recording the new START. On macOS this uses LaunchAgents and a logout hook. If the installer prints a `sudo defaults write com.apple.loginwindow LogoutHook ...` command, run it once (it requires your password) so that STOP is recorded when you log out or shut down. To remove: **`timesheet autostart uninstall`**.

## Configuration

The default log file is **`$HOME/Documents/timesheet.log`**. To change it, edit `DEFAULT_TIMESHEET` in `src/main.rs` and rebuild.

Runtime settings are optional and live in **`$HOME/.config/timesheet.yml`** (no file is created by the install). `rotate` controls when a new timesheet week starts — for example, a work week that runs Monday through Sunday:

```yaml
rotate:
  day: monday
  time: "00:00"
```

Everything else in that file supplies defaults for `timesheet pdf` and `timesheet email` — the contractor name, the PDF template, the recipients, and the relay to send through — and may be written per job under `prefixes:`. Those two subcommands are the only ones that need any configuration at all: `template:` has no default, so name it there or pass `--template`.

See the **Configuration** section of `README.md`, or `timesheet help`, for the full description.

## Verifying the installation

From any directory (with the install directory on your `PATH`):

```sh
timesheet list
```

If the log file does not exist yet, you should see "No timesheet data found." Otherwise you'll see the report. You can also run:

```sh
timesheet start "test activity"
timesheet list
timesheet stop
```

Once `name:` and `template:` are configured, check that the PDF side works too:

```sh
timesheet pdf -o /tmp/timesheet.pdf
```

## Building Rust documentation

To generate and open the crate documentation:

```sh
cargo doc --no-deps --open
```

Output is under `target/doc/timesheet/`.

## Running tests (Rust)

To run the Rust unit tests:

```sh
cargo test
```

To run the linter:

```sh
cargo clippy --all-targets -- -D warnings
```

## Linting and Git hooks

Hooks are defined in a single file, `.pre-commit-config.yaml`, which is the one source of truth used by the local hooks **and** by CI (the lint workflow runs these same hooks), so the two cannot drift apart.

Run the hooks with either tool:

- [prek](https://prek.j178.dev/) (a Rust-based pre-commit alternative; reads `.pre-commit-config.yaml` natively):

  ```sh
  cargo install prek   # or see the prek docs for prebuilt binaries
  prek install -f
  ```

- Python [pre-commit](https://pre-commit.com/):

  ```sh
  pip install pre-commit
  pre-commit install
  ```

Either command installs both the `pre-commit` and `commit-msg` hooks (so commitlint catches non-Conventional Commit messages locally before CI does). Run all hooks manually with `prek run --all-files` / `pre-commit run --all-files`, or a single hook with `prek run <hook-id>` / `pre-commit run <hook-id>`.
