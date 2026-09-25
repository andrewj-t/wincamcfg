# CLAUDE.md

Guidance for Claude Code in this repository. Everything a human contributor also needs lives in [docs/development.md](docs/development.md) (build, test and lint commands, code layout, CI, dependencies, releases) and [docs/architecture.md](docs/architecture.md) (diagrams; keep them in step with the types when you change them). Read those first, plus the module docs at the top of each file under `src/`.

## Working agreement

- **Push the branch and stop; the maintainer opens PRs.** Never open, merge or close a pull request, and never delete a remote branch, without being asked.
- **Never edit the version or CHANGELOG.md by hand.** release-plz owns both. See the Releases section of [docs/development.md](docs/development.md).
- **Get the commit subject right.** Conventional Commits, and with squash merges the PR title becomes the subject and the changelog line: `feat` bumps the minor version and lands under Added; `fix` under Fixed; `perf`, `refactor` and `build` under Changed; `docs` under Documentation; `build(deps)` under Dependencies; `ci`, `test` and `style` are omitted; a `!` or `BREAKING CHANGE` bumps the major.
- **Keep the shape.** Five source files, no lib/bin split, no mocking trait, no extra crates unless they remove code.

## What you cannot check yourself

- **Verify against a real camera.** The unit tests cover parsing, validation and output; everything that touches COM has to be checked by hand with `cargo run -- list`, `get` and `set`. The `dialog` subcommand shows the driver's own property pages, which is the reference for what "correct" looks like. This machine has a Logitech C920 and an OBS virtual camera. Ask the maintainer to run anything you cannot verify.
- **Do not remove the re-bind in write verification.** `Device::write_all` reads every accepted write back through a fresh handle (`read_back`) on purpose. Some cameras (C920, PowerlineFrequency) report a written value through the same handle and drop it when the last handle closes; only a fresh handle reveals that. The UVC class driver stores the value in the device's registry parameters and applies it at the next device start, which is what `stored_value`, `Persistence::Stored` and `--restart-device` are for. Details and the experiments behind them are in [docs/troubleshooting.md](docs/troubleshooting.md) and the 0.4.0 changelog entry.

## House rules for the code

`#[expect(lint, reason = "...")]` instead of `#[allow]`, one FFI call per `unsafe` block with a `// SAFETY:` comment, no `RUSTFLAGS` in CI. The reasons are in the "Things the code cannot tell you" section of [docs/development.md](docs/development.md); read it before touching lints, `unsafe` or the build configuration.

## Documentation

User docs are split by audience: [README.md](README.md) is the pitch and the quick start, `docs/usage.md`, `docs/properties.md`, `docs/reference.md` and `docs/troubleshooting.md` are for users, `docs/development.md` and `docs/architecture.md` are for contributors, and [docs/README.md](docs/README.md) indexes them. Put new prose in the file that matches its audience rather than growing the README.
