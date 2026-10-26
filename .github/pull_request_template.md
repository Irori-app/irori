<!--
Title: <type>(<scope>): <summary>, the same type as the branch (<type>/<kebab-name>).
Types: feat, fix, perf, refactor, docs, test, i18n, build, ci, chore, revert.
The "PR rules" check enforces this; see AGENTS.md, "Branches, commits and pull requests".
-->

## What and why

<!-- The problem, what changes for the user, and why this approach. Link issues: "Fixes #123". -->

## How it was checked

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --locked --workspace --all-targets -- -D warnings`
- [ ] `cargo test --locked --workspace` (or the crates touched: `-p ...`)
- [ ] Ran Ember and exercised the change: <!-- what you did, on which OS -->
- [ ] Packaging tests, if `packaging/` changed: `python3 -m unittest discover -s packaging -p "test_*.py"`

Platforms tried: <!-- Linux (Wayland/X11) / macOS / Windows; say which were not -->

## Checklist

- [ ] New or changed user-facing text is in every locale (`crates/<crate>/locales/*.json`, all 19 languages)
- [ ] Docs updated where behaviour changed (README, `docs/`, AGENTS.md)
- [ ] No secrets, tokens or real account data in code, fixtures or logs
- [ ] Breaking change (settings, on-disk data, CLI) marked with `!` and described above

## Screenshots

<!-- For UI changes: before and after, light and dark theme. Delete this section otherwise. -->
