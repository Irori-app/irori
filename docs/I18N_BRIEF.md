# Translation brief

Read `docs/AGENT_BRIEF.md` and `docs/UI_BRIEF.md` first, then `crates/ui/src/i18n.rs`.

Your crate owns a key namespace (given in your task) and its catalogs in
`crates/<crate>/locales/<code>.json` — one flat JSON object per language for
every code in `irori_ui::i18n::LANGUAGES` (en, cs, de, es, fr, it, nl, pl, pt,
pt_BR, ro, sv, tr, ru, uk, ja, ko, zh_Hans, zh_Hant).

1. Replace every user-facing string in your crate (labels, button text,
   tooltips, placeholders, titles, descriptions, empty states, notifications,
   dialog copy, menu items, error prefixes shown to users) with
   `irori_ui::i18n::t("<ns>.<area>.<intent>")` or `t_args(key, &[("name", value)])`.
   Keys describe intent, never the English text. Never build sentences from
   fragments; give each full sentence its own key with `{placeholders}`.
   Do not translate: log messages, identifiers, store/brand names (Epic Games,
   GOG, Steam, Proton, Wine, DXVK…), file paths, keyboard key names.
   Plurals: use separate keys (`…_one`, `…_other`) and pick in code.
2. Write `locales/en.json` (source of truth) and translate it into every other
   language yourself, accurately and idiomatically, following each language's
   UI conventions (sentence case where usual, proper quotes, no trailing period
   on buttons). For established gaming-launcher terminology, consult Heroic's
   community translations at
   `/tmp/claude-1000/-home-radud--config-Claude-scratch-workspaces-be9c0f01-1ca9-426c-9323-c4f5fd7497b3-bb73d751-aec5-4f35-9234-f6306f8360c4-scratch-2026-10-22-dd28a5/7cad61c2-7751-448b-add9-7e546ed39814/scratchpad/ref/heroic/public/locales/<code>/translation.json`
   (directory codes match ours) for terminology only. They are GPL-3.0 and Irori
   is MIT, so write your own sentences rather than copying theirs.
3. Register: `pub static CATALOGS: irori_ui::i18n::Catalogs = &[("en", include_str!("../locales/en.json")), …];`
   and call `irori_ui::i18n::register(CATALOGS)` from the crate's `pub fn init(cx)`
   (create `init` if the crate has none, and say so in your report).
   Registration must happen once (guard with `std::sync::Once`).
4. Tests: (a) every language has every English key (`irori_ui::i18n::missing_keys`),
   no extra keys, and the same `{placeholder}` set per key; (b) every `t("…")` /
   `t_args("…"` key literal in your crate's sources exists in en.json (scan
   `src/**/*.rs` with a regex in the test); (c) all JSON parses.
5. Keep layout robust to longer text (German, Russian): no fixed-width text
   containers that would clip; `min_w_0` + truncation where needed.
6. fmt / clippy -D warnings / tests clean with `CARGO_TARGET_DIR=target/agents/<crate>`.
   Only edit your crate. Never enable gpui-kit's `test-support` feature.
Report: number of keys, languages, anything left untranslated and why.
