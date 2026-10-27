//! Cyberpunk 2077 REDmod (GOG): which mods load and in what order.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
    v_virtual_list,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement,
    Pixels, Render, SharedString, Size, Styled, Window, div, px, rems, size,
};
use irori_core::{GameId, GameInfo, Runner};
use irori_gog::{CyberpunkConfig, GogStore};
use irori_ui::backend;
use irori_ui::i18n::{t, t_args};
use serde_json::Value;

use crate::hooks;
use crate::load::Load;
use crate::ops::{notify_error, notify_success, open_path};

/// GOG id of Cyberpunk 2077.
pub(crate) const CYBERPUNK_ID: &str = "1423049311";
/// GOG id of the REDmod DLC.
pub(crate) const REDMOD_ID: &str = "1597316373";
/// Height of one mod row, in rems.
const ROW_REMS: f32 = 2.75;
/// Rows visible before the list scrolls.
const VISIBLE_ROWS: usize = 10;

/// Whether REDmod management applies: installed Cyberpunk 2077 from GOG
/// with the REDmod DLC, and the GOG store handle installed.
pub(crate) fn applies(game: &GameInfo, cx: &App) -> bool {
    game.runner() == Runner::Gog
        && game.app_name == CYBERPUNK_ID
        && game.is_installed
        && game
            .install
            .as_ref()
            .is_some_and(|i| i.installed_dlcs.iter().any(|d| d == REDMOD_ID))
        && hooks::gog_store(cx).is_some()
}

/// One mod in load order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModEntry {
    pub name: String,
    pub enabled: bool,
}

/// Mods in load order: the selected ones that still exist (in their saved
/// order, enabled), then the rest (disabled). No selection means every mod
/// loads, so all are enabled.
pub(crate) fn arrange_mods(available: &[String], selected: &[String]) -> Vec<ModEntry> {
    let chosen: Vec<&String> = selected.iter().filter(|m| available.contains(m)).collect();
    if chosen.is_empty() {
        return available
            .iter()
            .map(|name| ModEntry {
                name: name.clone(),
                enabled: true,
            })
            .collect();
    }
    let mut out: Vec<ModEntry> = chosen
        .into_iter()
        .map(|name| ModEntry {
            name: name.clone(),
            enabled: true,
        })
        .collect();
    out.extend(
        available
            .iter()
            .filter(|m| !selected.contains(m))
            .map(|name| ModEntry {
                name: name.clone(),
                enabled: false,
            }),
    );
    out
}

/// The store config for this list: enabled mods in load order.
pub(crate) fn mods_config(mods_enabled: bool, entries: &[ModEntry]) -> CyberpunkConfig {
    CyberpunkConfig {
        mods_enabled,
        mods_to_load: entries
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.name.clone())
            .collect(),
    }
}

/// Whether the mod at `ix` may be turned off: at least one must stay on.
pub(crate) fn can_disable(entries: &[ModEntry], ix: usize) -> bool {
    entries.get(ix).is_some_and(|e| e.enabled) && entries.iter().filter(|e| e.enabled).count() > 1
}

/// Move the mod at `ix` one place earlier (`up`) or later in load order.
/// Returns whether it moved.
pub(crate) fn move_entry(entries: &mut [ModEntry], ix: usize, up: bool) -> bool {
    let target = if up { ix.checked_sub(1) } else { Some(ix + 1) };
    match target {
        Some(t) if t < entries.len() && ix < entries.len() => {
            entries.swap(ix, t);
            true
        }
        _ => false,
    }
}

/// The saved REDmod config of `app_name` in a GOG `installed.json`.
pub(crate) fn config_from_installed(json: &Value, app_name: &str) -> Option<CyberpunkConfig> {
    json.get("installed")?
        .as_array()?
        .iter()
        .find(|r| r.get("appName").and_then(Value::as_str) == Some(app_name))?
        .get("cyberpunk")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
}

async fn saved_config(gog: &GogStore, app_name: &str) -> CyberpunkConfig {
    let path = gog.paths().installed_file();
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return CyberpunkConfig::default();
    };
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| config_from_installed(&v, app_name))
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
struct ModList {
    mods_enabled: bool,
    entries: Vec<ModEntry>,
}

/// REDmod settings of an installed Cyberpunk 2077.
pub struct RedmodPanel {
    id: GameId,
    game: Arc<GameInfo>,
    list: Load<ModList>,
    dirty: bool,
    saving: bool,
    sizes: Rc<Vec<Size<Pixels>>>,
    sized_for: (usize, Pixels),
}

/// Open the REDmod sheet of Cyberpunk 2077.
pub fn open_redmod_sheet(id: GameId, window: &mut Window, cx: &mut App) {
    let Some(game) = backend(cx).game(&id) else {
        return;
    };
    let title: SharedString = format!("REDmod · {}", game.title).into();
    let panel = cx.new(|cx| RedmodPanel::new(game, window, cx));
    window.open_sheet(cx, move |sheet, _, _| {
        sheet
            .title(title.clone())
            .size(rems(30.))
            .child(panel.clone())
    });
}

impl RedmodPanel {
    pub fn new(game: Arc<GameInfo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            id: game.id(),
            game,
            list: Load::Idle,
            dirty: false,
            saving: false,
            sizes: Rc::new(Vec::new()),
            sized_for: (usize::MAX, px(0.)),
        };
        this.reload(window, cx);
        this
    }

    /// Re-read the available mods and the saved selection.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(gog) = hooks::gog_store_for(&self.id.app_name, cx) else {
            self.list = Load::Failed(t("game.redmod.gog_unavailable"));
            return;
        };
        self.list = Load::Loading;
        cx.notify();
        let app = self.id.app_name.to_string();
        let task = irori_ui::run(cx, async move {
            let available = gog.cyberpunk_mods(&app).await?;
            let saved = saved_config(&gog, &app).await;
            Ok(ModList {
                mods_enabled: saved.mods_enabled,
                entries: arrange_mods(&available, &saved.mods_to_load),
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.list = Load::from_result(result);
                this.dirty = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ModList) -> bool) {
        if let Load::Ready(list) = &mut self.list
            && f(list)
        {
            self.dirty = true;
            cx.notify();
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(gog), Some(list)) = (
            hooks::gog_store_for(&self.id.app_name, cx),
            self.list.ready(),
        ) else {
            return;
        };
        let config = mods_config(list.mods_enabled, &list.entries);
        let app = self.id.app_name.to_string();
        self.saving = true;
        cx.notify();
        let task = irori_ui::run(cx, async move {
            gog.set_cyberpunk_mod_config(&app, config).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.dirty = false;
                        notify_success(window, cx, t("game.redmod.saved"));
                    }
                    Err(e) => notify_error(window, cx, t("game.redmod.save_failed"), &e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(list) = self.list.ready() else {
            return Vec::new();
        };
        let entries = list.entries.clone();
        let count = entries.len();
        let busy = self.saving;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        range
            .filter_map(|ix| {
                let entry = entries.get(ix)?;
                let checked = entry.enabled;
                let toggle_locked = checked && !can_disable(&entries, ix);
                Some(
                    h_flex()
                        .id(("redmod-row", ix))
                        .w_full()
                        .h(rems(ROW_REMS))
                        .px_3()
                        .gap_2()
                        .items_center()
                        .when(ix > 0, |r| r.border_t_1().border_color(border))
                        .child(
                            div()
                                .w(rems(1.5))
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("{}", ix + 1)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(!checked, |d| d.text_color(muted))
                                .child(entry.name.clone()),
                        )
                        .child(
                            Button::new(("redmod-up", ix))
                                .xsmall()
                                .ghost()
                                .icon(IconName::ArrowUp)
                                .tooltip(t("game.redmod.load_earlier"))
                                .disabled(busy || ix == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.edit(cx, |l| move_entry(&mut l.entries, ix, true))
                                })),
                        )
                        .child(
                            Button::new(("redmod-down", ix))
                                .xsmall()
                                .ghost()
                                .icon(IconName::ArrowDown)
                                .tooltip(t("game.redmod.load_later"))
                                .disabled(busy || ix + 1 >= count)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.edit(cx, |l| move_entry(&mut l.entries, ix, false))
                                })),
                        )
                        .child(
                            Switch::new(("redmod-enabled", ix))
                                .small()
                                .checked(checked)
                                .accessibility_label(t_args(
                                    "game.redmod.load_mod",
                                    &[("name", &entry.name)],
                                ))
                                .disabled(busy || toggle_locked)
                                .on_change(cx.listener(move |this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.edit(cx, |l| {
                                        if !on && !can_disable(&l.entries, ix) {
                                            return false;
                                        }
                                        match l.entries.get_mut(ix) {
                                            Some(e) if e.enabled != on => {
                                                e.enabled = on;
                                                true
                                            }
                                            _ => false,
                                        }
                                    })
                                })),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for RedmodPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let row_h = window.rem_size() * ROW_REMS;
        let mods_dir = self.game.install_path().map(|p| p.join("mods"));
        let body: AnyElement = match &self.list {
            Load::Idle | Load::Loading => div()
                .text_sm()
                .text_color(muted)
                .child(t("game.redmod.scanning"))
                .into_any_element(),
            Load::Failed(e) => v_flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(t_args("game.redmod.read_failed", &[("error", e.as_ref())])),
                )
                .child(
                    Button::new("redmod-retry")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(t("game.common.retry"))
                        .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
                )
                .into_any_element(),
            Load::Ready(list) => {
                let count = list.entries.len();
                let mods_enabled = list.mods_enabled;
                if self.sized_for != (count, row_h) {
                    self.sizes = Rc::new(vec![size(px(0.), row_h); count]);
                    self.sized_for = (count, row_h);
                }
                let rows = v_virtual_list(
                    cx.entity(),
                    "redmod-rows",
                    self.sizes.clone(),
                    |this, range, _, cx| this.render_rows(range, cx),
                )
                .size_full();
                v_flex()
                    .gap_3()
                    .child(
                        Switch::new("redmod-mods-enabled")
                            .label(t("game.redmod.enable_mods"))
                            .checked(mods_enabled)
                            .disabled(self.saving)
                            .on_change(cx.listener(|this, on: &bool, _, cx| {
                                let on = *on;
                                this.edit(cx, |l| {
                                    let changed = l.mods_enabled != on;
                                    l.mods_enabled = on;
                                    changed
                                })
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t("game.redmod.help")),
                    )
                    .when(count == 0, |col| {
                        col.child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(t("game.redmod.empty")),
                        )
                    })
                    .when(count > 0, |col| {
                        col.child(
                            div()
                                .id("redmod-list")
                                .w_full()
                                .h(rems(ROW_REMS * count.min(VISIBLE_ROWS) as f32))
                                .rounded(cx.theme().radius_lg)
                                .border_1()
                                .border_color(cx.theme().border)
                                .overflow_hidden()
                                .child(rows),
                        )
                    })
                    .into_any_element()
            }
        };
        v_flex()
            .id("redmod-panel")
            .w_full()
            .gap_4()
            .py_2()
            .child(body)
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("redmod-save")
                            .primary()
                            .label(t("game.redmod.save"))
                            .loading(self.saving)
                            .disabled(!self.dirty || self.saving)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    )
                    .child(
                        Button::new("redmod-reload")
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .label(t("game.redmod.rescan"))
                            .disabled(self.saving)
                            .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
                    )
                    .when_some(mods_dir, |row, dir| {
                        row.child(
                            Button::new("redmod-folder")
                                .ghost()
                                .icon(IconName::FolderOpen)
                                .label(t("game.redmod.open_folder"))
                                .on_click(move |_, window, cx| open_path(dir.clone(), window, cx)),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn e(name: &str, enabled: bool) -> ModEntry {
        ModEntry {
            name: name.into(),
            enabled,
        }
    }

    #[test]
    fn arranges_saved_order_first() {
        let available = names(&["a", "b", "c"]);
        assert_eq!(
            arrange_mods(&available, &[]),
            vec![e("a", true), e("b", true), e("c", true)]
        );
        assert_eq!(
            arrange_mods(&available, &names(&["c", "gone", "a"])),
            vec![e("c", true), e("a", true), e("b", false)]
        );
        // Only missing mods selected: everything loads.
        assert_eq!(
            arrange_mods(&available, &names(&["gone"])),
            vec![e("a", true), e("b", true), e("c", true)]
        );
    }

    #[test]
    fn config_keeps_load_order() {
        let list = vec![e("c", true), e("a", false), e("b", true)];
        assert_eq!(
            mods_config(true, &list),
            CyberpunkConfig {
                mods_enabled: true,
                mods_to_load: names(&["c", "b"]),
            }
        );
    }

    #[test]
    fn one_mod_stays_enabled() {
        let list = vec![e("a", true), e("b", false)];
        assert!(!can_disable(&list, 0));
        assert!(!can_disable(&list, 1));
        let list = vec![e("a", true), e("b", true)];
        assert!(can_disable(&list, 1));
    }

    #[test]
    fn moves() {
        let mut list = vec![e("a", true), e("b", true), e("c", true)];
        assert!(!move_entry(&mut list, 0, true));
        assert!(move_entry(&mut list, 0, false));
        assert_eq!(list[1].name, "a");
        assert!(move_entry(&mut list, 2, true));
        assert_eq!(
            list.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
        assert!(!move_entry(&mut list, 2, false));
        assert!(!move_entry(&mut list, 7, true));
    }

    #[test]
    fn reads_saved_config() {
        let json = serde_json::json!({ "installed": [
            { "appName": "1", "platform": "windows" },
            { "appName": CYBERPUNK_ID, "cyberpunk": { "modsEnabled": true, "modsToLoad": ["x"] } }
        ]});
        assert_eq!(
            config_from_installed(&json, CYBERPUNK_ID),
            Some(CyberpunkConfig {
                mods_enabled: true,
                mods_to_load: names(&["x"]),
            })
        );
        assert_eq!(config_from_installed(&json, "1"), None);
        assert_eq!(config_from_installed(&serde_json::json!({}), "1"), None);
    }
}
