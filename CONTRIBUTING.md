# Contributing to Irori

Thanks for helping. Bug reports, translations, docs and code are all welcome. This
page is a short introduction. [AGENTS.md](AGENTS.md) is the full guide: the
architecture, conventions and checks every change must pass. It applies to people as
much as to coding agents.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Reporting a bug or asking for a feature

Open an [issue](https://github.com/irori-app/irori/issues). For a bug, include:

- Your OS and version, how you installed Irori, and the Irori version
  (`irori --version`).
- The store and game, if it is about one.
- What you did, what you expected and what happened.
- The relevant part of the log: `data/logs/irori.log` under Irori's data directory
  (see [platform paths](docs/PLATFORMS.md#paths-and-desktop-behavior)), and any
  `crash-*.txt` beside it.

Logs can contain your user name, file paths and game titles. Read them before you post
them, and never post tokens, cookies or account ids.

## Setting up

You need Rust 1.95 or newer and the platform libraries listed in
[docs/PLATFORMS.md](docs/PLATFORMS.md). Then:

```bash
cargo run --release -p irori
```

Use `IRORI_HOME=<scratch dir>` while you experiment, so your real library, settings and
store sign-ins stay untouched.

## Making a change

1. Branch from an up-to-date `main` with a name such as `fix/epic-login` or
   `feat/big-picture-search` (`<type>/<short-kebab-description>`).
2. Keep the change to one topic and match the code around it.
3. Add tests: new behaviour needs them, and a bug fix needs a test that fails
   without the fix. Tests never touch the network or real user data.
4. Put every new or changed string into all 19 locale files, translated. The tests
   fail on a missing key. If you can't translate a language well, say so in the pull
   request and we'll help.
5. Run the checks:

   ```bash
   cargo fmt --all
   cargo clippy --locked --workspace --all-targets --profile ci -- -D warnings
   cargo test --locked --workspace --no-fail-fast
   ```

6. If the change shows up in the app, run Irori and try it. Include screenshots in
   light and dark for UI changes.

## Pull requests

- Title the pull request `<type>(<scope>): <summary>`, with the same type as the
  branch, for example `fix(epic): treat Mac ports released separately as native`.
  The types are `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `i18n`, `build`,
  `ci`, `chore` and `revert`. A CI check enforces the naming; you can run it locally:

  ```bash
  python3 .github/scripts/pr_rules.py --title "fix(epic): …" --head fix/epic-login
  ```

- Fill in the template: what changed and why, how you checked it, and which
  platforms you didn't try.
- Pull requests are squash-merged, with the title as the commit message.

## Licence

Irori is under the [MIT License](LICENSE). By contributing, you agree that your
contribution is licensed under it too.
