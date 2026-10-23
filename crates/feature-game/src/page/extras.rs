//! Extras tab: the files a store offers beside the game (GOG's offline
//! installers, patches, language packs and goodies such as manuals and
//! soundtracks), each downloadable into a folder of the game's own.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use ember_core::util::format_bytes;
use ember_core::{ExtraFile, ExtraGroup, GameInfo};
use ember_services::{FileDownload, FileState, file_download_key};
use ember_ui::i18n::{t, t_args};
use ember_ui::motion::gliding_progress;
use ember_ui::style::{caption, panel};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, rems,
};

use super::GamePage;
use crate::i18n::platform_label;
use crate::load::Load;
use crate::ops;

/// Installer files that belong together: one platform, language and version.
type InstallerKey = (String, String, String);

impl GamePage {
    /// Fetch the list of extra files (once; the tab shows it from then on).
    pub(super) fn load_extras(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.extras, Load::Idle | Load::Failed(_)) {
            return;
        }
        self.extras = Load::Loading;
        let b = backend(cx);
        let id = self.id.clone();
        let task = ember_ui::run(cx, async move {
            let store = b
                .store(id.runner)
                .ok_or_else(|| crate::i18n::store_unavailable(id.runner))?;
            store.extras(&id.app_name).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.extras = Load::from_result(result.map(Arc::new));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The folder this game's extras are saved in.
    fn extras_folder(&self, game: &GameInfo, cx: &App) -> PathBuf {
        let base = models(cx).settings.read(cx).app.extras_path.clone();
        PathBuf::from(base).join(ember_core::paths::sanitize(&game.title))
    }

    pub(super) fn render_extras(
        &mut self,
        game: &Arc<GameInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let files = match &self.extras {
            Load::Idle | Load::Loading => {
                return v_flex()
                    .gap_2()
                    .child(Skeleton::new().h_8().w_full())
                    .child(Skeleton::new().h_8().w_full())
                    .child(Skeleton::new().h_8().w(rems(20.)))
                    .into_any_element();
            }
            Load::Failed(e) => {
                return v_flex()
                    .gap_2()
                    .items_start()
                    .child(div().text_sm().child(t("game.extras.load_failed")))
                    .child(div().text_xs().text_color(muted).child(e.clone()))
                    .child(
                        Button::new("extras-retry")
                            .outline()
                            .small()
                            .label(t("game.extras.retry"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.load_extras(window, cx)),
                            ),
                    )
                    .into_any_element();
            }
            Load::Ready(files) => files.clone(),
        };
        if files.is_empty() {
            return div()
                .text_sm()
                .text_color(muted)
                .child(t("game.extras.empty"))
                .into_any_element();
        }

        let folder = self.extras_folder(game, cx);
        let downloads: Vec<FileDownload> = models(cx).downloads.read(cx).files.clone();
        let id = self.id.clone();
        let state_of = |file: &ExtraFile| {
            let key = file_download_key(&id, file);
            downloads.iter().find(|d| d.key == key).cloned()
        };

        let mut sections: Vec<AnyElement> = vec![self.render_folder_bar(&folder, window, cx)];

        // Installers: one card per platform, language and version, with
        // every part listed and a button for all of them.
        let mut installers: BTreeMap<InstallerKey, Vec<&ExtraFile>> = BTreeMap::new();
        for f in files.iter().filter(|f| f.group == ExtraGroup::Installer) {
            let key = (
                f.platform
                    .map(|p| platform_label(p).to_string())
                    .unwrap_or_default(),
                f.language.clone().unwrap_or_default(),
                f.version.clone().unwrap_or_default(),
            );
            installers.entry(key).or_default().push(f);
        }
        if !installers.is_empty() {
            let cards: Vec<AnyElement> = installers
                .into_iter()
                .enumerate()
                .map(|(ix, ((platform, language, version), parts))| {
                    let heading = [platform, language, version]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let total: u64 = parts.iter().map(|f| f.size).sum();
                    let all: Vec<ExtraFile> = parts.iter().map(|f| (*f).clone()).collect();
                    let (game_id, dest) = (self.id.clone(), folder.clone());
                    let any_idle = parts.iter().any(|f| {
                        state_of(f).is_none_or(|d| !d.is_active() && d.state != FileState::Done)
                    });
                    panel(cx)
                        .gap_2()
                        .child(
                            h_flex()
                                .w_full()
                                .gap_3()
                                .child(div().flex_1().min_w_0().font_semibold().child(heading))
                                .when(parts.len() > 1 && any_idle, |d| {
                                    d.child(
                                        Button::new(("extras-all", ix))
                                            .small()
                                            .outline()
                                            .icon(IconName::Download)
                                            .label(t_args(
                                                "game.extras.download_all",
                                                &[("size", &format_bytes(total))],
                                            ))
                                            .on_click(move |_, _, cx| {
                                                let b = backend(cx);
                                                for f in &all {
                                                    b.download_extra(
                                                        game_id.clone(),
                                                        f.clone(),
                                                        dest.clone(),
                                                    );
                                                }
                                            }),
                                    )
                                }),
                        )
                        .children(parts.iter().map(|f| {
                            let label = match f.part {
                                Some((part, count)) => t_args(
                                    "game.extras.part",
                                    &[("part", &part.to_string()), ("count", &count.to_string())],
                                ),
                                None => SharedString::from(f.name.clone()),
                            };
                            self.render_file_row(f, label, state_of(f), &folder, cx)
                        }))
                        .into_any_element()
                })
                .collect();
            sections.push(group(t("game.extras.group.installers"), cards, cx));
        }

        for (kind, title) in [
            (ExtraGroup::Patch, "game.extras.group.patches"),
            (ExtraGroup::LanguagePack, "game.extras.group.language_packs"),
            (ExtraGroup::Goodie, "game.extras.group.goodies"),
        ] {
            let rows: Vec<AnyElement> = files
                .iter()
                .filter(|f| f.group == kind)
                .map(|f| {
                    let mut label = f.name.clone();
                    if let Some((part, count)) = f.part {
                        label = format!("{label} · {part}/{count}");
                    }
                    if let Some(lang) = &f.language {
                        label = format!("{label} · {lang}");
                    }
                    self.render_file_row(f, label.into(), state_of(f), &folder, cx)
                })
                .collect();
            if !rows.is_empty() {
                let card = panel(cx).gap_2().children(rows).into_any_element();
                sections.push(group(t(title), vec![card], cx));
            }
        }

        v_flex().gap_6().children(sections).into_any_element()
    }

    /// Where files go, with buttons to open the folder and choose another.
    fn render_folder_bar(
        &self,
        folder: &std::path::Path,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let open = folder.to_path_buf();
        h_flex()
            .w_full()
            .gap_2()
            .flex_wrap()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .text_sm()
                    .text_color(muted)
                    .child(Icon::new(IconName::FolderOpen).small())
                    .child(div().min_w_0().truncate().child(t_args(
                        "game.extras.save_to",
                        &[("folder", &folder.display().to_string())],
                    ))),
            )
            .child(
                Button::new("extras-open-folder")
                    .ghost()
                    .small()
                    .label(t("game.extras.open_folder"))
                    .on_click(move |_, _, cx| {
                        let _ = std::fs::create_dir_all(&open);
                        cx.open_url(&format!("file://{}", open.display()));
                    }),
            )
            .child(
                Button::new("extras-change-folder")
                    .ghost()
                    .small()
                    .label(t("game.extras.change_folder"))
                    .on_click(|_, window, cx| {
                        let pick = ops::pick_path(window, cx, true, t("game.extras.pick_folder"));
                        window
                            .spawn(cx, async move |cx| {
                                if let Some(dir) = pick.await {
                                    let _ = cx.update(|_, cx| {
                                        backend(cx).settings.update_app(|s| {
                                            s.extras_path = dir.to_string_lossy().into_owned()
                                        });
                                    });
                                }
                            })
                            .detach();
                    }),
            )
            .into_any_element()
    }

    /// One file: its name and size, then what can be done with it now.
    fn render_file_row(
        &self,
        file: &ExtraFile,
        label: SharedString,
        state: Option<FileDownload>,
        folder: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let key = file_download_key(&self.id, file);
        let row_id = ElementId::Name(format!("extra:{key}").into());
        let start = {
            let (game_id, file, dest) = (self.id.clone(), file.clone(), folder.to_path_buf());
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                backend(cx).download_extra(game_id.clone(), file.clone(), dest.clone())
            }
        };
        let size = format_bytes(file.size);
        let action: AnyElement = match state.as_ref().map(|s| s.state) {
            Some(FileState::Queued | FileState::Running) => {
                let s = state.clone().unwrap_or_else(|| unreachable!());
                let running = s.state == FileState::Running;
                let percent = (file.size > 0 && running)
                    .then(|| (s.downloaded as f32 / file.size as f32 * 100.0).clamp(0.0, 100.0));
                let cancel_key = key.clone();
                h_flex()
                    .gap_2()
                    .w(rems(18.))
                    .child(div().flex_1().child(gliding_progress(
                        ElementId::Name(format!("extra-progress:{key}").into()),
                        percent,
                    )))
                    .child(
                        div()
                            .w(rems(4.))
                            .text_xs()
                            .text_right()
                            .text_color(muted)
                            .child(match percent {
                                Some(p) => format!("{p:.0}%"),
                                None => t("game.extras.queued").to_string(),
                            }),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("extra-cancel:{key}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip(t("game.extras.cancel"))
                            .on_click(move |_, _, cx| backend(cx).cancel_extra(&cancel_key)),
                    )
                    .into_any_element()
            }
            Some(FileState::Done) => {
                let path = state.as_ref().and_then(FileDownload::path);
                h_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(theme.success)
                            .child(Icon::new(IconName::Check).xsmall())
                            .child(t("game.extras.done")),
                    )
                    .children(path.map(|path| {
                        Button::new(ElementId::Name(format!("extra-show:{key}").into()))
                            .ghost()
                            .xsmall()
                            .label(t("game.extras.show"))
                            .on_click(move |_, _, cx| cx.reveal_path(&path))
                    }))
                    .into_any_element()
            }
            Some(FileState::Failed) => h_flex()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(t("game.extras.failed")),
                )
                .child(
                    Button::new(ElementId::Name(format!("extra-retry:{key}").into()))
                        .outline()
                        .xsmall()
                        .label(t("game.extras.retry"))
                        .on_click(start),
                )
                .into_any_element(),
            Some(FileState::Canceled) | None => {
                Button::new(ElementId::Name(format!("extra-get:{key}").into()))
                    .outline()
                    .small()
                    .icon(IconName::Download)
                    .label(t("game.extras.download"))
                    .on_click(start)
                    .into_any_element()
            }
        };
        h_flex()
            .id(row_id)
            .w_full()
            .gap_3()
            .py_1()
            .text_sm()
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .w(rems(6.))
                    .flex_shrink_0()
                    .text_right()
                    .text_color(muted)
                    .child(size),
            )
            .child(
                h_flex()
                    .w(rems(18.))
                    .flex_shrink_0()
                    .justify_end()
                    .child(action),
            )
            .into_any_element()
    }
}

/// A titled group of cards.
fn group(title: SharedString, cards: Vec<AnyElement>, cx: &App) -> AnyElement {
    v_flex()
        .gap_2()
        .child(caption(title, cx))
        .children(cards)
        .into_any_element()
}
