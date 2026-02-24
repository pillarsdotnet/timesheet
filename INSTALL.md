# Building and installing from source

This document describes how to build and install the **ts** timesheet CLI from the repository.

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

The binary is written to `target/release/ts` (or `target/debug/ts` if you use `cargo build` without `--release`).

### 2. Install the binary

**Using the binary's install subcommand** (run from the repo so it can find itself):

```sh
./target/release/ts install
# or into a specific directory:
./target/release/ts install ~/bin
```

**Or copy manually:**

```sh
cp target/release/ts ~/bin/ts
chmod +x ~/bin/ts
```

Ensure `~/bin` (or your chosen directory) is on your `PATH`.

## Autostart (optional)

To run **`ts start`** at login and **`ts stop`** at logout/shutdown:

```sh
ts autostart
```

You can pass an interval (e.g. **`ts autostart 5s`**) to set the reminder interval and start the daemon in this session. On macOS this uses LaunchAgents and a logout hook. If the installer prints a `sudo defaults write com.apple.loginwindow LogoutHook ...` command, run it once (it requires your password) so that STOP is recorded when you log out or shut down. To remove: **`ts autostart uninstall`**.

## Configuration

The default log file is **`$HOME/Documents/timesheet.log`**. To change it, edit `DEFAULT_TIMESHEET` in `src/main.rs` and rebuild.

## Verifying the installation

From any directory (with the install directory on your `PATH`):

```sh
ts list
```

If the log file does not exist yet, you should see "No timesheet data found." Otherwise you'll see the report. You can also run:

```sh
ts start "test activity"
ts list
ts stop
```

## Building Rust documentation

To generate and open the crate documentation:

```sh
cargo doc --no-deps --open
```

Output is under `target/doc/ts/`.

## Running tests (Rust)

To run the Rust unit tests:

```sh
cargo test
```

To run the linter:

```sh
cargo clippy --all-targets -- -D warnings
```

## Linting and Git hooks (prek)

This project uses [prek](https://prek.j178.dev/) (a Rust-based pre-commit alternative). Configuration is in `prek.toml`.

Install prek (e.g. `cargo install prek`), then install git hooks:

```sh
prek install -f
```

This installs both pre-commit and commit-msg hooks. Run all hooks manually with `prek run`, or run a single hook with `prek run <hook-id>`.
