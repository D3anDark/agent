# Contributing

Thank you for your help. Bug reports, fixes and small features are welcome.

## Before you start

- For a bug, open an issue with the steps that show it, the `agent --version` output and your OS.
- For a feature or a large change, open an issue first. The project keeps the agent small, so not every feature goes in.
- For a security problem, do not open an issue. Read [SECURITY.md](SECURITY.md).

## Build and test

You need a stable Rust toolchain.

```sh
cd source
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI runs the same four commands on each pull request.

## Pull requests

- Keep one change in one pull request.
- Add a test for each new behavior or bug fix, next to the code it tests.
- Use [Conventional Commits](https://www.conventionalcommits.org) for commit messages, for example `fix(tui): keep queued images`.
- Do not add a dependency for something a few lines of code can do.
- Update `README.md` or `plugins/README.md` if the change is visible to users.

## License

The project uses the [GNU General Public License v3.0 or later](LICENSE). When you send a contribution, you agree to license it under the same terms.

## Code of conduct

All contributors must follow the [Code of Conduct](CODE_OF_CONDUCT.md).
