//! Per-game Winetricks components: a searchable list of every verb
//! winetricks offers, with an install button each, plus the verbs already
//! installed in the game's prefix.
//!
//! The verb catalogue is listed once per process by running the winetricks
//! script Irori manages (`list-all`) against an empty scratch prefix, so
//! listing never touches a game's prefix.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use irori_core::process::{self, CommandSpec};
use irori_core::{GameId, Paths, StatusKind};
use irori_ui::i18n::{t, t_args};
use irori_ui::{backend, models};

use crate::i18n::LanguageStamp;
use crate::widgets::{muted, panel};

/// Most rows rendered at once; the search narrows the rest.
const MAX_ROWS: usize = 50;

/// Winetricks verb categories offered for installation (benchmarks, games
/// and prefix verbs are left out).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum VerbCategory {
    Dlls,
    Fonts,
    Settings,
    Apps,
}

impl VerbCategory {
    pub(crate) const ALL: [VerbCategory; 4] = [
        VerbCategory::Dlls,
        VerbCategory::Fonts,
        VerbCategory::Settings,
        VerbCategory::Apps,
    ];

    /// The category of a `===== name =====` section header.
    fn from_header(name: &str) -> Option<Self> {
        match name {
            "dlls" => Some(VerbCategory::Dlls),
            "fonts" => Some(VerbCategory::Fonts),
            "settings" => Some(VerbCategory::Settings),
            "apps" => Some(VerbCategory::Apps),
            _ => None,
        }
    }

    pub(crate) fn label(self) -> SharedString {
        t(match self {
            VerbCategory::Dlls => "settings.winetricks.category.dlls",
            VerbCategory::Fonts => "settings.winetricks.category.fonts",
            VerbCategory::Settings => "settings.winetricks.category.settings",
            VerbCategory::Apps => "settings.winetricks.category.apps",
        })
    }
}

/// One installable winetricks verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WinetricksVerb {
    pub name: String,
    pub description: String,
    pub category: VerbCategory,
    /// Lowercased name and description, for search.
    haystack: String,
}

impl WinetricksVerb {
    fn new(name: &str, description: &str, category: VerbCategory) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            category,
            haystack: format!("{name}\n{description}").to_lowercase(),
        }
    }
}

/// Parse `winetricks list-all` output: `===== category =====` headers
/// followed by `verb   description [downloadable]` lines.
pub(crate) fn parse_list_all(output: &str) -> Vec<WinetricksVerb> {
    let mut verbs = Vec::new();
    let mut seen = HashSet::new();
    let mut category = None;
    for line in output.lines() {
        let line = line.trim();
        if let Some(header) = line
            .strip_prefix("=====")
            .and_then(|r| r.strip_suffix("====="))
        {
            category = VerbCategory::from_header(header.trim());
            continue;
        }
        let Some(cat) = category else { continue };
        if line.is_empty() || line.starts_with("Using winetricks") || line.starts_with("Executing ")
        {
            continue;
        }
        let (name, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let mut description = rest.trim();
        for suffix in ["[downloadable]", "[cached]"] {
            description = description
                .strip_suffix(suffix)
                .unwrap_or(description)
                .trim();
        }
        if description.starts_with("Fake verb") || !seen.insert(name.to_string()) {
            continue;
        }
        verbs.push(WinetricksVerb::new(name, description, cat));
    }
    verbs
}

/// Verbs recorded in a prefix's `winetricks.log`, in order, without
/// duplicates.
pub(crate) fn parse_winetricks_log(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('-') && !l.starts_with('#'))
        .filter(|l| seen.insert(l.to_string()))
        .map(str::to_string)
        .collect()
}

/// Indices of the verbs matching every token of `query` (case-insensitive,
/// name or description) in `category`; verbs whose name starts with the
/// first token come first.
pub(crate) fn filter_verbs(
    verbs: &[WinetricksVerb],
    query: &str,
    category: Option<VerbCategory>,
) -> Vec<usize> {
    let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<(bool, usize)> = verbs
        .iter()
        .enumerate()
        .filter(|(_, v)| category.is_none_or(|c| v.category == c))
        .filter(|(_, v)| tokens.iter().all(|t| v.haystack.contains(t.as_str())))
        .map(|(i, v)| {
            let prefix = tokens
                .first()
                .is_some_and(|t| v.name.to_lowercase().starts_with(t.as_str()));
            (!prefix, i)
        })
        .collect();
    hits.sort_by_key(|(rank, _)| *rank);
    hits.into_iter().map(|(_, i)| i).collect()
}

/// Process-wide verb catalogue (it doesn't depend on the game).
static CATALOG: Mutex<Option<Arc<Vec<WinetricksVerb>>>> = Mutex::new(None);

fn cached_catalog() -> Option<Arc<Vec<WinetricksVerb>>> {
    CATALOG.lock().ok().and_then(|c| c.clone())
}

/// List every verb winetricks offers.
///
/// Errors carry `messages` (already translated, since this runs off the UI
/// thread): `[unavailable, timeout, failed, empty]`.
async fn list_verbs(messages: [SharedString; 4]) -> Result<Arc<Vec<WinetricksVerb>>> {
    if let Some(c) = cached_catalog() {
        return Ok(c);
    }
    let [unavailable, timeout, failed, empty] = messages;
    let script = irori_runtime::tools::ensure_winetricks(false)
        .await
        .context(unavailable)?;
    // An empty scratch prefix: listing must not touch (or create) a real one.
    let scratch = Paths::get().cache.join("winetricks-list");
    let spec = CommandSpec::new(script)
        .arg("list-all")
        .env("WINEPREFIX", scratch.to_string_lossy())
        .env("WINETRICKS_LATEST_VERSION_CHECK", "disabled")
        .env("WINEDEBUG", "-all");
    let output = tokio::time::timeout(Duration::from_secs(90), process::output(&spec))
        .await
        .context(timeout)?
        .context(failed)?;
    let verbs = parse_list_all(&output);
    if verbs.is_empty() {
        bail!("{empty}");
    }
    let verbs = Arc::new(verbs);
    if let Ok(mut c) = CATALOG.lock() {
        *c = Some(verbs.clone());
    }
    Ok(verbs)
}

/// Verbs installed in a prefix: Proton keeps the Wine prefix in `pfx`, so
/// both locations are read.
async fn read_installed(prefix: &Path) -> Vec<String> {
    let mut text = String::new();
    for log in [
        prefix.join("pfx").join("winetricks.log"),
        prefix.join("winetricks.log"),
    ] {
        if let Ok(t) = tokio::fs::read_to_string(&log).await {
            text.push_str(&t);
            text.push('\n');
        }
    }
    parse_winetricks_log(&text)
}

enum Catalog {
    Idle,
    Loading,
    Ready(Arc<Vec<WinetricksVerb>>),
    Failed(SharedString),
}

/// Searchable winetricks verbs for one game's prefix.
pub(crate) struct WinetricksPanel {
    id: GameId,
    search: Entity<InputState>,
    query: String,
    category: Option<VerbCategory>,
    catalog: Catalog,
    /// Indices into the catalogue matching the query, recomputed only when
    /// the query, category or catalogue changes.
    filtered: Vec<usize>,
    installed: Vec<String>,
    installing: Option<String>,
    language: LanguageStamp,
    _subscriptions: Vec<Subscription>,
}

impl WinetricksPanel {
    pub(crate) fn new(id: GameId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("settings.winetricks.search_placeholder"))
        });
        let statuses = models(cx).statuses.clone();
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().to_string();
                    this.refilter();
                    cx.notify();
                }
            }),
            cx.observe(&statuses, |_, _, cx| cx.notify()),
        ];
        Self {
            id,
            search,
            query: String::new(),
            category: None,
            catalog: Catalog::Idle,
            filtered: Vec::new(),
            installed: Vec::new(),
            installing: None,
            language: LanguageStamp::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Load the catalogue and installed verbs the first time the panel is
    /// shown.
    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.catalog, Catalog::Idle) {
            self.load_catalog(window, cx);
            self.load_installed(window, cx);
        }
    }

    fn load_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.catalog = Catalog::Loading;
        let messages = [
            t("settings.winetricks.error.unavailable"),
            t("settings.winetricks.error.timeout"),
            t("settings.winetricks.error.failed"),
            t("settings.winetricks.error.empty"),
        ];
        let task = irori_ui::run(cx, list_verbs(messages));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.catalog = match result {
                    Ok(verbs) => Catalog::Ready(verbs),
                    Err(e) => Catalog::Failed(format!("{e:#}").into()),
                };
                this.refilter();
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn load_installed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        let id = self.id.clone();
        let missing = t("settings.error.game_missing");
        let task = irori_ui::run(cx, async move {
            let game = b.game(&id).context(missing)?;
            let prefix =
                irori_runtime::prefix_path(&b.settings.app(), &b.settings.game(&id), &game);
            Ok(read_installed(&prefix).await)
        });
        cx.spawn_in(window, async move |this, cx| {
            let installed = task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.installed = installed;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refilter(&mut self) {
        self.filtered = match &self.catalog {
            Catalog::Ready(verbs) => filter_verbs(verbs, &self.query, self.category),
            _ => Vec::new(),
        };
    }

    fn set_category(&mut self, category: Option<VerbCategory>, cx: &mut Context<Self>) {
        self.category = category;
        self.refilter();
        cx.notify();
    }

    fn busy(&self, cx: &Context<Self>) -> bool {
        self.installing.is_some()
            || models(cx).statuses.read(cx).kind(&self.id) == Some(StatusKind::Winetricks)
    }

    fn install(&mut self, verb: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy(cx) {
            return;
        }
        self.installing = Some(verb.clone());
        let b = backend(cx);
        let id = self.id.clone();
        let v = verb.clone();
        let task = irori_ui::run(cx, async move {
            b.winetricks(&id, vec![v]).await?.check()?;
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.installing = None;
                match result {
                    Ok(()) => window.push_notification(
                        Notification::success(t_args(
                            "settings.winetricks.verb_installed",
                            &[("verb", &verb)],
                        )),
                        cx,
                    ),
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}")).title(t_args(
                            "settings.winetricks.install_failed",
                            &[("verb", &verb)],
                        )),
                        cx,
                    ),
                }
                this.load_installed(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn render_rows(&self, verbs: &[WinetricksVerb], cx: &mut Context<Self>) -> gpui_kit::Div {
        let theme = cx.theme();
        let (border, muted_fg) = (theme.border, theme.muted_foreground);
        let mono = theme.mono_font_family.clone();
        let busy = self.busy(cx);
        let installed: HashSet<&str> = self.installed.iter().map(String::as_str).collect();
        if self.filtered.is_empty() {
            return panel(cx).child(div().p_3().child(muted(
                t_args(
                    "settings.winetricks.no_match",
                    &[("query", self.query.trim())],
                ),
                cx,
            )));
        }
        let all = self.category.is_none();
        self.filtered
            .iter()
            .take(MAX_ROWS)
            .enumerate()
            .fold(panel(cx), |list, (row, &ix)| {
                let verb = &verbs[ix];
                let name = verb.name.clone();
                let is_installed = installed.contains(verb.name.as_str());
                let installing = self.installing.as_deref() == Some(verb.name.as_str());
                let action = if is_installed && !installing {
                    Tag::success()
                        .xsmall()
                        .child(t("settings.winetricks.installed"))
                        .into_any_element()
                } else {
                    Button::new(ElementId::Name(format!("winetricks-install-{name}").into()))
                        .xsmall()
                        .outline()
                        .label(t("settings.common.install"))
                        .loading(installing)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.install(name.clone(), window, cx)
                        }))
                        .into_any_element()
                };
                list.child(
                    h_flex()
                        .id(ElementId::Name(format!("winetricks-row-{ix}").into()))
                        .gap_3()
                        .px_3()
                        .py_1()
                        .when(row > 0, |r| r.border_t_1().border_color(border))
                        .child(
                            div()
                                .w_40()
                                .flex_shrink_0()
                                .truncate()
                                .text_sm()
                                .font_family(mono.clone())
                                .child(verb.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(muted_fg)
                                .child(verb.description.clone()),
                        )
                        .when(all, |r| {
                            r.child(
                                Tag::secondary()
                                    .xsmall()
                                    .outline()
                                    .child(verb.category.label()),
                            )
                        })
                        .child(div().flex_shrink_0().child(action)),
                )
            })
    }
}

impl Render for WinetricksPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language.changed() {
            cx.defer_in(window, |this, window, cx| {
                let placeholder = t("settings.winetricks.search_placeholder");
                this.search
                    .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            });
        }
        let categories = h_flex()
            .gap_1()
            .flex_wrap()
            .child(
                Button::new("winetricks-cat-all")
                    .ghost()
                    .xsmall()
                    .label(t("settings.common.all"))
                    .selected(self.category.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.set_category(None, cx))),
            )
            .children(VerbCategory::ALL.map(|c| {
                Button::new(ElementId::Name(format!("winetricks-cat-{c:?}").into()))
                    .ghost()
                    .xsmall()
                    .label(c.label())
                    .selected(self.category == Some(c))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_category(Some(c), cx)))
            }));
        let installed_line = if self.installed.is_empty() {
            t("settings.winetricks.installed_none")
        } else {
            t_args(
                "settings.winetricks.installed_list",
                &[("verbs", &self.installed.join(", "))],
            )
        };
        let progress = self.installing.clone().map(|verb| {
            let detail = models(cx)
                .statuses
                .read(cx)
                .get(&self.id)
                .and_then(|s| s.context.clone())
                .unwrap_or_default();
            h_flex()
                .gap_2()
                .text_sm()
                .child(Icon::new(IconName::LoaderCircle).small())
                .child(t_args("settings.winetricks.installing", &[("verb", &verb)]))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                )
        });
        let body = match &self.catalog {
            Catalog::Idle | Catalog::Loading => {
                muted(t("settings.winetricks.loading"), cx).into_any_element()
            }
            Catalog::Failed(e) => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(e.clone()),
                )
                .child(
                    Button::new("winetricks-retry")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(t("settings.common.try_again"))
                        .on_click(cx.listener(|this, _, window, cx| this.load_catalog(window, cx))),
                )
                .into_any_element(),
            Catalog::Ready(verbs) => {
                let verbs = verbs.clone();
                let shown = self.filtered.len().min(MAX_ROWS);
                let more = (self.filtered.len() > shown).then(|| {
                    muted(
                        t_args(
                            "settings.winetricks.showing",
                            &[
                                ("shown", &shown.to_string()),
                                ("total", &self.filtered.len().to_string()),
                            ],
                        ),
                        cx,
                    )
                    .text_xs()
                });
                v_flex()
                    .gap_2()
                    .child(self.render_rows(&verbs, cx))
                    .children(more)
                    .into_any_element()
            }
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(muted(installed_line, cx).text_xs())
            .children(progress)
            .child(
                Input::new(&self.search)
                    .id("winetricks-search")
                    .aria_label(t("settings.winetricks.search_label"))
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true),
            )
            .child(categories)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_ALL: &str = "Using winetricks 20250102 - sha256sum: abc
===== apps =====
7zip                     7-Zip 24.09 (Igor Pavlov, 2024) [downloadable]
===== benchmarks =====
3dmark03                 3D Mark 03 (Futuremark, 2003)
===== dlls =====
d3dx9                    MS d3dx9_??.dll from DirectX 9 redistributable (Microsoft, 2010) [downloadable]
vcrun2019                Visual C++ 2015-2019 libraries (Microsoft, 2019) [downloadable]
===== fonts =====
corefonts                MS Arial, Courier, Times fonts (Microsoft, 2008) [downloadable]
===== prefix =====
apps
dlls
===== settings =====
autostart_winedbg=disabled Prevent winedbg from launching when an unhandled exception occurs
bad                      Fake verb that always returns false
win10                    Set Windows version to Windows 10
";

    #[test]
    fn list_all_keeps_installable_categories() {
        let verbs = parse_list_all(LIST_ALL);
        let names: Vec<&str> = verbs.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "7zip",
                "d3dx9",
                "vcrun2019",
                "corefonts",
                "autostart_winedbg=disabled",
                "win10"
            ]
        );
        let vc = &verbs[2];
        assert_eq!(vc.category, VerbCategory::Dlls);
        assert_eq!(
            vc.description,
            "Visual C++ 2015-2019 libraries (Microsoft, 2019)"
        );
        assert_eq!(
            verbs[4].description,
            "Prevent winedbg from launching when an unhandled exception occurs"
        );
    }

    #[test]
    fn search_matches_all_tokens_and_ranks_name_prefixes_first() {
        let verbs = parse_list_all(LIST_ALL);
        let names = |q: &str, c| {
            filter_verbs(&verbs, q, c)
                .into_iter()
                .map(|i| verbs[i].name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("VCRUN", None), vec!["vcrun2019"]);
        assert_eq!(names("microsoft fonts", None), vec!["corefonts"]);
        assert_eq!(names("", Some(VerbCategory::Settings)).len(), 2);
        // "win10" starts with "win"; "autostart_winedbg" only contains it.
        assert_eq!(
            names("win", Some(VerbCategory::Settings)),
            vec!["win10", "autostart_winedbg=disabled"]
        );
        assert!(names("nothing like this", None).is_empty());
    }

    #[test]
    fn winetricks_log_is_deduplicated() {
        assert_eq!(
            parse_winetricks_log("vcrun2019\n\ncorefonts\nvcrun2019\n-q\n"),
            vec!["vcrun2019".to_string(), "corefonts".to_string()]
        );
    }
}
