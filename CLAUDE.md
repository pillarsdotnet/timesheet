# Claude instructions for the `ts` timesheet project

These mirror the project's Cursor rules (`.cursor/rules/*.mdc`) as native Claude
instructions. Keep them in sync if either set changes.

## Commit workflow (always applies)

When the user says "commit these changes" (or equivalent):

1. **Update docs first.** Review the changes (e.g. `git diff`, `git status`). If
   they add or change behavior, options, or usage, update the relevant docs
   (e.g. `README.md`, the manpage in `src/main.rs`, `INSTALL.md`) so the docs
   match the code. Only then proceed to commit.
2. **Then commit.** Stage and commit with an appropriate message (conventional
   commit format, since this project uses it).

Do not commit until docs have been updated when the changes warrant it.

## Rebuild and reinstall

When the user says "rebuild and reinstall" (or equivalent) in this project:

1. Run `cargo build --release`
2. Run `./target/release/ts install ~/bin`

Install to `~/bin` specifically, not the first writable directory on PATH.
