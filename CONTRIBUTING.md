# Contributing

Thanks for contributing to **ts** (the timesheet CLI). This document explains how to install the full toolchain so the build, the tests, and the pre-commit hooks all run, and how those checks map to CI.

## Toolchain and dependencies

The same checks run on every commit (via the git hooks) and in CI. Install everything once.

### Rust — build, `cargo fmt`, `cargo clippy`, `cargo test`

You need a Rust toolchain with the **rustfmt** and **clippy** components.

- With [rustup](https://rustup.rs) (recommended):

  ```sh
  rustup component add rustfmt clippy
  ```

- On Debian/Ubuntu using system packages instead of rustup:

  ```sh
  sudo apt install rustc cargo rustfmt rust-clippy
  ```

  `rust-clippy` is the metapackage that puts `cargo-clippy` on your `PATH`; installing only the versioned `rust-<N>-clippy` package is not enough for `cargo clippy` to work.

Verify:

```sh
cargo --version && cargo fmt --version && cargo clippy --version
```

### Git hooks — file hygiene, Prettier, markdownlint, commitlint

The hooks are defined in `.pre-commit-config.yaml`, the single source of truth used by both the local hooks and CI. Install them with either tool:

- [prek](https://prek.j178.dev/) (Rust-based; reads `.pre-commit-config.yaml` natively):

  ```sh
  cargo install prek
  prek install -f
  ```

- Python [pre-commit](https://pre-commit.com/):

  ```sh
  pip install pre-commit
  pre-commit install
  ```

Either command installs both the `pre-commit` and `commit-msg` hooks. The Prettier, markdownlint, and commitlint hooks run under Node.js, which the hook manager downloads and manages for you — you do not need to install Node yourself.

### Optional — running the app

- `groff` and `less` to render `ts help`.
- The Linux reminder chooser: `python3` with `python3-pyqt6` (or `python3-pyqt5`), falling back to `kdialog`/`zenity`, plus `notify-send`. See the [README](README.md) "Requirements" section for details.

## Running the checks

Run the entire suite exactly as CI does:

```sh
pre-commit run --all-files
# or, with prek:
prek run --all-files
```

This runs: merge-conflict / TOML / YAML checks, the end-of-file and trailing-whitespace fixers, Prettier, markdownlint, `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test`.

You can also run the individual tools directly:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

## Building and installing

```sh
cargo build --release
./target/release/ts install ~/bin
```

See [INSTALL.md](INSTALL.md) for more detail.

## Commit messages

Commits must follow [Conventional Commits](https://www.conventionalcommits.org/) (e.g. `feat: ...`, `fix(macos): ...`). The `commit-msg` hook and CI both run commitlint against `.commitlintrc.yaml`.

## Continuous integration

CI (`.github/workflows/linter.yaml`) installs Rust and pre-commit and runs `pre-commit run --all-files` — the same hooks you run locally — plus a commitlint check across every commit in the pull request. Because both sides run the one `.pre-commit-config.yaml`, local checks and CI cannot drift apart.
