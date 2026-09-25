# Contributing

Bug reports and pull requests are welcome. Please open an issue with the [bug report form](.github/ISSUE_TEMPLATE/bug_report.yml) so the version, camera model, exact command and trace log are all in one place.

Before opening a pull request, run the three checks CI enforces:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

[docs/development.md](docs/development.md) has the rest: the full command list, what needs checking against a real camera, the CI workflows and how releases are cut. [docs/architecture.md](docs/architecture.md) has the diagrams.

Commit subjects follow [Conventional Commits](https://www.conventionalcommits.org/): `feat`, `fix`, `perf`, `refactor`, `build`, `docs`, `ci`, `test`, `style`. Pull requests are squash-merged, so the pull request title becomes the commit subject and the changelog line. Never edit the version or `CHANGELOG.md` by hand; release-plz owns both.
