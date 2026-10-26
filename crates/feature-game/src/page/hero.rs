//! Page header: backdrop, toolbar, cover, title, facts, the primary action and the
//! sticky bar that keeps it in reach after scrolling.

use std::sync::Arc;

use ember_core::{GameInfo, StatusKind};
use ember_services::LaunchParams;
use ember_ui::components::{GameCover, ProgressSummary, art_img};
use ember_ui::i18n::{t, t_args};
use ember_ui::style::{art_tint, caption, stat};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _, DropdownButton};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::select::Select;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, Context, Hsla, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement, Styled, StyledImage as _, Window, div, linear_color_stop, linear_gradient,
    point, px, rems,
};

use super::GamePage;
use crate::hooks::SteamDeckStatus;
use crate::state::PrimaryAction;
use crate::{manage, ops, saves};

/// How far the sticky bar slides down as it appears, in pixels.
const STICKY_DROP_PX: f32 = 24.0;
/// Width of the cover column.
const COVER_WIDTH: f32 = 15.;
/// Cover width on narrow pages, where the summary needs the room.
const COMPACT_COVER_WIDTH: f32 = 10.;
/// Pages narrower than this (in rem) get the compact cover.
const COMPACT_PAGE_REM: f32 = 52.;
/// Pages narrower than this (in rem) stack the cover above the title.
const STACKED_PAGE_REM: f32 = 36.;

impl GamePage {
    /// Faded, blurred art behind the top of the page. The art is decoded tiny and
    /// stretched, which GPUI's linear filtering turns into a blur.
    pub(super) fn render_backdrop(&self, game: &GameInfo, cx: &mut Context<Self>) -> AnyElement {
        let url = game.hero_art().map(str::to_owned);
        let ambient = url
            .as_deref()
            .and_then(|url| ember_ui::art::ambient(url, cx));
        // Until the blurred copy exists, a tiny thumbnail stretched up stands in.
        let art = match &ambient {
            Some(a) => Some(gpui_kit::img(a.backdrop.clone())),
            None => url.and_then(|url| art_img(&url, 48, false, cx)),
        };
        let tint = ambient.map(|a| art_tint(a.rgb, cx));
        let bg = cx.theme().background;
        let fade = linear_gradient(
            180.,
            linear_color_stop(bg.opacity(0.55), 0.),
            linear_color_stop(bg, 1.),
        );
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(rems(44.))
            .overflow_hidden()
            .when_some(art, |d, img| {
                d.child(
                    img.image_cache(&self.image_cache)
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .opacity(0.6),
                )
            })
            .child(div().absolute().top_0().left_0().size_full().bg(fade))
            // The art's own colour, washing down from the top of the page.
            .when_some(tint, |d, tint| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .bg(linear_gradient(
                            180.,
                            linear_color_stop(tint.opacity(0.22), 0.),
                            linear_color_stop(tint.opacity(0.), 0.75),
                        )),
                )
            })
            .into_any_element()
    }

    /// The colour of the game's backdrop art, once known.
    fn art_tint(&self, game: &GameInfo, cx: &mut Context<Self>) -> Option<Hsla> {
        let url = game.hero_art()?.to_owned();
        ember_ui::art::ambient(&url, cx).map(|a| art_tint(a.rgb, cx))
    }

    /// The page header: back link, the cover beside the title and summary, then the
    /// play bar with the primary action, stats and the game's tools.
    pub(super) fn render_header(
        &mut self,
        game: &Arc<GameInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page_rem = self.page_rem(window).unwrap_or(f32::MAX);
        let stacked = page_rem < STACKED_PAGE_REM;
        let cover_width = if page_rem < COMPACT_PAGE_REM {
            COMPACT_COVER_WIDTH
        } else {
            COVER_WIDTH
        };
        let cover = self.render_cover(game, cover_width);
        let info = self.render_info(game, cx);
        let play_bar = self.render_play_bar(game, window, cx);
        let status = self.render_status_line(game, cx);
        let menu_id = self.id.clone();
        let shortcuts = self.shortcuts_exist;
        let store_url = self.store_url(game);
        let page = cx.entity().downgrade();
        v_flex()
            .id("game-header")
            .w_full()
            .px_8()
            .pt_4()
            .pb_6()
            .gap_6()
            .child(h_flex().child(back_button(cx)))
            .child(
                div()
                    .w_full()
                    .flex()
                    .when_else(
                        stacked,
                        |d| d.flex_col().gap_6(),
                        |d| d.flex_row().items_end().gap_8(),
                    )
                    .child(cover)
                    .child(info),
            )
            .child(play_bar)
            .children(status)
            .context_menu(move |menu, window, cx| {
                super::menu::build_menu(
                    menu,
                    &menu_id,
                    shortcuts,
                    store_url.clone(),
                    Some(page.clone()),
                    window,
                    cx,
                )
            })
            .into_any_element()
    }

    /// The portrait cover.
    fn render_cover(&self, game: &Arc<GameInfo>, width: f32) -> AnyElement {
        div()
            .w(rems(width))
            .flex_shrink_0()
            .rounded_lg()
            .overflow_hidden()
            .shadow_lg()
            .child(GameCover::new(game.clone()).width(rems(width)))
            .into_any_element()
    }

    /// Store, title, credits, tags and the summary.
    fn render_info(&mut self, game: &Arc<GameInfo>, cx: &mut Context<Self>) -> AnyElement {
        let logo = game
            .art_logo
            .clone()
            .filter(|l| !l.is_empty())
            .and_then(|url| art_img(&url, 720, true, cx));
        let heading = match logo {
            Some(logo) => div()
                .h(rems(6.))
                .max_w(rems(26.))
                .child(
                    logo.image_cache(&self.image_cache)
                        .h_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            None => div()
                .text_size(rems(2.5))
                .line_height(rems(2.9))
                .font_extrabold()
                .line_clamp(2)
                .child(game.title.clone())
                .into_any_element(),
        };
        let credits = self.render_credits(game, cx);
        let tags = self.render_tags(game, cx);
        let summary = self.render_summary(game, cx);
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(caption(game.runner().store_name(), cx))
            .child(heading)
            .children(credits)
            .children(tags)
            .children(summary)
            .into_any_element()
    }

    /// "Developer · release date".
    fn render_credits(&self, game: &GameInfo, cx: &Context<Self>) -> Option<AnyElement> {
        let release = self
            .extra
            .ready()
            .and_then(|e| e.release_date.clone())
            .or_else(|| game.release_date.clone())
            .map(|d| d.chars().take(10).collect::<String>())
            .filter(|d| d.len() >= 4);
        let developer = game.developer.clone().or_else(|| game.publisher.clone());
        let parts: Vec<String> = developer.into_iter().chain(release).collect();
        (!parts.is_empty()).then(|| {
            div()
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(parts.join(" · "))
                .into_any_element()
        })
    }

    /// Genres, then ProtonDB and Steam Deck ratings in words.
    fn render_tags(&self, game: &GameInfo, cx: &Context<Self>) -> Option<AnyElement> {
        let genres = self
            .extra
            .ready()
            .map(|e| e.genres.clone())
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| game.genres.clone());
        let compat = self.compat.ready();
        let protondb = compat.and_then(|c| c.protondb_tier.clone());
        let deck = compat
            .and_then(|c| c.steam_deck)
            .filter(|d| *d != SteamDeckStatus::Unknown);
        if genres.is_empty() && protondb.is_none() && deck.is_none() {
            return None;
        }
        let _ = cx;
        Some(
            h_flex()
                .gap_2()
                .flex_wrap()
                .children(
                    genres
                        .into_iter()
                        .take(4)
                        .map(|g| Tag::secondary().child(g)),
                )
                .when_some(protondb, |row, tier| {
                    let tag = match tier.as_str() {
                        "platinum" | "gold" | "native" => Tag::success(),
                        "silver" | "bronze" => Tag::warning(),
                        "borked" => Tag::danger(),
                        _ => Tag::secondary(),
                    };
                    row.child(tag.child(t_args(
                        "game.header.protondb",
                        &[("tier", &super::compat::tier_label(&tier))],
                    )))
                })
                .when_some(deck, |row, deck| {
                    let tag = match deck {
                        SteamDeckStatus::Verified => Tag::success(),
                        SteamDeckStatus::Playable => Tag::warning(),
                        _ => Tag::danger(),
                    };
                    row.child(tag.child(t_args("game.header.deck", &[("status", &deck.label())])))
                })
                .into_any_element(),
        )
    }

    /// The store's short description, with a link to the full text.
    fn render_summary(&self, game: &GameInfo, cx: &mut Context<Self>) -> Option<AnyElement> {
        let extra = self.extra.ready();
        let short = extra.and_then(|e| e.short_description.clone());
        let full = extra
            .and_then(|e| e.description.clone())
            .or_else(|| game.description.clone());
        // GOG's short "lead" is often a promotion (pre-order offers), not a summary.
        let text = if game.runner() == ember_core::Runner::Gog {
            full.or(short)
        } else {
            short.or(full)
        }
        .map(|t| super::info::plain_text(&t))
        .filter(|t| !t.is_empty())?;
        Some(
            v_flex()
                .max_w(rems(48.))
                .gap_1()
                .items_start()
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .line_clamp(3)
                        .child(text),
                )
                .child(
                    Button::new("read-more")
                        .link()
                        .small()
                        .label(t("game.header.read_more"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.tab = super::InfoTab::About;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// The primary action, the game's stats under captions, and its tools as icons.
    fn render_play_bar(
        &mut self,
        game: &Arc<GameInfo>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let action = self.primary_action(cx).unwrap_or(PrimaryAction::Install);
        let halo = matches!(action, PrimaryAction::Play)
            .then(|| self.art_tint(game, cx))
            .flatten();
        let primary = self.render_primary(action, game, cx);
        // Play glows in the game's own colour; other actions stay plain.
        let primary = div()
            .rounded(cx.theme().radius)
            .when_some(halo, |d, tint| {
                d.shadow(vec![BoxShadow {
                    color: tint.opacity(0.45),
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(24.),
                    spread_radius: px(0.),
                    inset: false,
                }])
            })
            .child(primary);
        let stats = self.render_stats(game, cx);
        let tools = self.render_tools(game, cx);
        let theme = cx.theme();
        h_flex()
            .w_full()
            .flex_wrap()
            .gap_x_8()
            .gap_y_3()
            .px_4()
            .py_3()
            .rounded(theme.radius_lg)
            .bg(theme.background.opacity(0.72))
            .border_1()
            .border_color(theme.border)
            .child(primary)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_8()
                    .flex_wrap()
                    .children(stats),
            )
            .child(tools)
            .into_any_element()
    }

    /// Play time, last played, achievement progress and size on disk.
    fn render_stats(&self, game: &GameInfo, cx: &Context<Self>) -> Vec<AnyElement> {
        let record = models(cx)
            .library
            .read(cx)
            .user
            .playtime
            .get(&self.id.key())
            .cloned();
        let minutes = record.as_ref().map(|r| r.total_minutes).unwrap_or(0);
        let last = record.and_then(|r| r.last_played);
        let now = ember_core::util::unix_now() as i64;
        let mut stats = Vec::new();
        stats.push(
            stat(
                t("game.info.playtime"),
                crate::i18n::playtime_label(minutes, last.is_some()),
                cx,
            )
            .into_any_element(),
        );
        if let Some(last) = last {
            stats.push(
                stat(
                    t("game.info.last_played"),
                    crate::load::format_day(&last, now),
                    cx,
                )
                .into_any_element(),
            );
        }
        if let Some(list) = self.achievements.ready().filter(|a| !a.is_empty()) {
            let unlocked = list.iter().filter(|a| a.unlocked_at.is_some()).count();
            let percent = crate::state::completion_percent(unlocked, list.len());
            stats.push(
                v_flex()
                    .gap_0p5()
                    .child(caption(t("game.header.achievements"), cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .font_semibold()
                            .child(format!("{unlocked} / {}", list.len()))
                            .child(div().w(rems(6.)).child(
                                Progress::new("achievement-progress").value(percent as f32),
                            )),
                    )
                    .into_any_element(),
            );
        }
        if let Some(size) = game
            .install
            .as_ref()
            .filter(|_| game.is_installed)
            .map(|i| {
                if i.install_size_bytes > 0 {
                    ember_core::util::format_bytes(i.install_size_bytes)
                } else {
                    i.install_size.clone()
                }
            })
            .filter(|s| !s.is_empty())
        {
            stats.push(stat(t("game.info.size"), size, cx).into_any_element());
        }
        stats
    }

    /// Cloud saves, settings, favourite and more actions, as icon buttons.
    fn render_tools(&self, game: &Arc<GameInfo>, cx: &mut Context<Self>) -> AnyElement {
        let fav = models(cx).library.read(cx).is_favourite(&self.id);
        let settings_id = self.id.clone();
        let saves_id = self.id.clone();
        let menu_id = self.id.clone();
        let shortcuts = self.shortcuts_exist;
        let store_url = self.store_url(game);
        let page = cx.entity().downgrade();
        let store_supports = backend(cx)
            .store(self.id.runner)
            .is_some_and(|s| s.supports_cloud_saves());
        let cloud = saves::CloudSupport::of(game, store_supports);
        h_flex()
            .gap_1()
            .when(cloud.offers_sheet(), |row| {
                let tooltip = cloud
                    .explanation(game)
                    .unwrap_or_else(|| t("game.hero.cloud_saves").to_string());
                row.child(
                    Button::new("game-cloud-saves")
                        .ghost()
                        .icon(IconName::Cloud)
                        .accessibility_label(t("game.hero.cloud_saves"))
                        .tooltip(tooltip)
                        .disabled(cloud != saves::CloudSupport::Supported)
                        .on_click(move |_, window, cx| {
                            saves::open_cloud_saves_sheet(saves_id.clone(), window, cx)
                        }),
                )
            })
            // Steam applies its own launch settings to its games.
            .when(self.id.runner.manages_installs(), |row| {
                row.child(
                    Button::new("game-settings")
                        .ghost()
                        .icon(IconName::Settings2)
                        .accessibility_label(t("game.hero.settings"))
                        .tooltip(t("game.hero.settings"))
                        .on_click(move |_, _, cx| ops::open_settings(&settings_id, "general", cx)),
                )
            })
            .child(
                Button::new("game-favourite")
                    .ghost()
                    .icon(if fav {
                        IconName::StarFill
                    } else {
                        IconName::Star
                    })
                    .accessibility_label(if fav {
                        t("game.hero.favourite")
                    } else {
                        t("game.hero.add_favourite")
                    })
                    .tooltip(if fav {
                        t("game.hero.favourite")
                    } else {
                        t("game.hero.add_favourite")
                    })
                    .selected(fav)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_favourite(cx))),
            )
            .child(
                Button::new("game-more")
                    .ghost()
                    .icon(IconName::Ellipsis)
                    .accessibility_label(t("game.hero.more_actions"))
                    .tooltip(t("game.hero.more_actions"))
                    .dropdown_menu(move |menu, window, cx| {
                        super::menu::build_menu(
                            menu,
                            &menu_id,
                            shortcuts,
                            store_url.clone(),
                            Some(page.clone()),
                            window,
                            cx,
                        )
                    }),
            )
            .into_any_element()
    }

    /// Slim bar pinned to the top once the header scrolled away: cover, title, action.
    pub(super) fn render_sticky_bar(
        &mut self,
        game: &Arc<GameInfo>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let action = self.primary_action(cx).unwrap_or(PrimaryAction::Install);
        let primary = self.render_primary(action, game, cx);
        let thumb = game
            .cover_art()
            .map(str::to_owned)
            .and_then(|url| art_img(&url, 96, false, cx));
        let theme = cx.theme();
        let bar = h_flex()
            .id("game-sticky-bar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .px_8()
            .py_2()
            .gap_3()
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border)
            .shadow_md()
            .when_some(thumb, |row, img| {
                row.child(
                    img.image_cache(&self.image_cache)
                        .h(rems(3.))
                        .w(rems(2.))
                        .rounded_sm()
                        .object_fit(ObjectFit::Cover),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_semibold()
                    .child(game.title.clone()),
            )
            .child(primary);
        ember_ui::motion::drop_in("game-sticky-bar-in", bar, STICKY_DROP_PX)
    }

    fn store_url(&self, game: &GameInfo) -> Option<String> {
        self.extra
            .ready()
            .and_then(|e| e.store_url.clone())
            .or_else(|| game.store_url.clone())
    }

    fn render_primary(
        &mut self,
        action: PrimaryAction,
        game: &GameInfo,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = self.id.clone();
        let row = h_flex().gap_2();
        match action {
            PrimaryAction::Install => row
                .child(
                    Button::new("primary-action")
                        .primary()
                        .large()
                        .icon(IconName::Download)
                        .label(t("game.common.install_ellipsis"))
                        .on_click(cx.listener(|this, _, window, cx| this.run_primary(window, cx))),
                )
                .into_any_element(),
            PrimaryAction::Play => row
                .child(self.render_play_button(cx))
                .when(self.launch_options.len() > 1, |r| {
                    r.child(
                        div().w(rems(14.)).child(
                            Select::new(&self.launch_select)
                                .placeholder(t("game.hero.launch_option_placeholder")),
                        ),
                    )
                })
                .into_any_element(),
            PrimaryAction::Update => row
                .child(
                    Button::new("primary-action")
                        .primary()
                        .large()
                        .icon(IconName::CloudDownload)
                        .label(t("game.action.update"))
                        .on_click(cx.listener(|this, _, window, cx| this.run_primary(window, cx))),
                )
                .child(
                    Button::new("play-without-update")
                        .large()
                        .outline()
                        .icon(IconName::Play)
                        .label(t("game.action.play"))
                        .tooltip(t("game.hero.play_without_update"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            let params = LaunchParams {
                                option: this.selected_option.clone(),
                                skip_version_check: true,
                                ..Default::default()
                            };
                            this.play_with(params, window, cx);
                        })),
                )
                .into_any_element(),
            PrimaryAction::Stop => row
                .child(
                    Button::new("primary-action")
                        .large()
                        .icon(IconName::Square)
                        .label(t("game.action.stop"))
                        .loading(self.pending.contains("stop"))
                        .on_click(cx.listener(|this, _, window, cx| this.run_primary(window, cx))),
                )
                .into_any_element(),
            PrimaryAction::Launching => row
                .child(
                    Button::new("primary-action")
                        .primary()
                        .large()
                        .label(t("game.action.launching"))
                        .loading(true)
                        .disabled(true),
                )
                .into_any_element(),
            PrimaryAction::Downloading { update } => row
                .child(
                    Button::new("primary-action")
                        .large()
                        .outline()
                        .icon(IconName::Pause)
                        .label(t("game.hero.pause"))
                        .tooltip(t("game.hero.pause_tooltip"))
                        .on_click(|_, _, cx| ops::pause_downloads(cx)),
                )
                .child(
                    Button::new("cancel-download")
                        .large()
                        .ghost()
                        .icon(IconName::X)
                        .label(if update {
                            t("game.hero.cancel_update")
                        } else {
                            t("game.hero.cancel_install")
                        })
                        .on_click(move |_, window, cx| {
                            if update {
                                ops::cancel_download(&id, true, cx);
                            } else {
                                manage::open_cancel_download_dialog(id.clone(), window, cx);
                            }
                        }),
                )
                .into_any_element(),
            PrimaryAction::Resume => {
                let installed = game.is_installed;
                row.child(
                    Button::new("primary-action")
                        .primary()
                        .large()
                        .icon(IconName::Play)
                        .label(t("game.action.resume"))
                        .on_click(cx.listener(|this, _, window, cx| this.run_primary(window, cx))),
                )
                .child(
                    Button::new("cancel-download")
                        .large()
                        .ghost()
                        .icon(IconName::X)
                        .label(t("game.hero.cancel_install"))
                        .on_click(move |_, window, cx| {
                            if installed {
                                ops::cancel_download(&id, true, cx);
                            } else {
                                manage::open_cancel_download_dialog(id.clone(), window, cx);
                            }
                        }),
                )
                .into_any_element()
            }
            PrimaryAction::Queued => {
                let position = models(cx)
                    .downloads
                    .read(cx)
                    .queue
                    .active
                    .iter()
                    .position(|i| i.id == self.id);
                row.child(
                    Button::new("primary-action")
                        .large()
                        .outline()
                        .icon(IconName::X)
                        .label(t("game.action.remove_from_queue"))
                        .on_click(cx.listener(|this, _, window, cx| this.run_primary(window, cx))),
                )
                .when_some(position, |r, p| {
                    r.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(t_args(
                                "game.hero.queue_position",
                                &[("position", &(p + 1).to_string())],
                            )),
                    )
                })
                .into_any_element()
            }
            PrimaryAction::Busy(kind) => row
                .child(
                    Button::new("primary-action")
                        .large()
                        .label(crate::i18n::status_label(kind))
                        .loading(true)
                        .disabled(true),
                )
                .into_any_element(),
            PrimaryAction::Missing => row
                .child(
                    Button::new("primary-action")
                        .primary()
                        .large()
                        .icon(IconName::FolderSearch)
                        .label(t("game.action.locate_files"))
                        .on_click(cx.listener(|this, _, window, cx| this.run_primary(window, cx))),
                )
                .into_any_element(),
            PrimaryAction::NotInstallable | PrimaryAction::Unsupported => row
                .child(
                    Button::new("primary-action")
                        .large()
                        .icon(IconName::Ban)
                        .label(action.label())
                        .disabled(true),
                )
                .into_any_element(),
        }
    }

    /// Play as a split button: launch options (when the store offers
    /// several) and one-off variants (verbose logs, no update check).
    fn render_play_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let play = Button::new("primary-action")
            .primary()
            .large()
            .icon(IconName::Play)
            .label(t("game.action.play"))
            .on_click(cx.listener(|this, _, window, cx| this.run_primary(window, cx)));
        let page = cx.entity();
        let options = if self.launch_options.len() > 1 {
            self.launch_options.clone()
        } else {
            Vec::new()
        };
        if options.is_empty() && !self.id.runner.manages_installs() {
            return play.into_any_element();
        }
        let current = self.selected_option.clone();
        DropdownButton::new("play-split")
            .primary()
            .large()
            .button(play)
            .menu_accessibility_label(t("game.hero.play_with"))
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.label(t("game.hero.play_with"));
                for option in &options {
                    let page = page.clone();
                    let chosen = option.clone();
                    menu = menu.item(
                        PopupMenuItem::new(option.label())
                            .checked(current.as_ref() == Some(option))
                            .on_click(move |_, window, cx| {
                                let chosen = chosen.clone();
                                page.update(cx, |p, cx| p.play(Some(chosen), window, cx));
                            }),
                    );
                }
                if !options.is_empty() {
                    menu = menu.separator();
                }
                for (label, icon, params) in super::menu::PLAY_VARIANTS {
                    let page = page.clone();
                    let option = current.clone();
                    menu = menu.item(PopupMenuItem::new(t(label)).icon(icon).on_click(
                        move |_, window, cx| {
                            let p = params(LaunchParams {
                                option: option.clone(),
                                ..Default::default()
                            });
                            page.update(cx, |page, cx| page.play_with(p, window, cx));
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }

    /// Progress, busy detail or hints below the actions.
    fn render_status_line(&self, game: &GameInfo, cx: &Context<Self>) -> Option<AnyElement> {
        let m = models(cx);
        let status = m.statuses.read(cx).get(&self.id).cloned();
        if let Some(status) = status.filter(|s| {
            s.kind.is_busy() && !matches!(s.kind, StatusKind::Playing | StatusKind::Queued)
        }) {
            return Some(
                div()
                    .max_w(rems(36.))
                    .child(ProgressSummary::new("game-progress", status))
                    .into_any_element(),
            );
        }
        let muted = cx.theme().muted_foreground;
        let online = m.accounts.read(cx).online;
        if game.is_installed && !online && !game.can_run_offline {
            return Some(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .child(Icon::new(IconName::WifiOff).small())
                    .child(t("game.page.offline_warning"))
                    .into_any_element(),
            );
        }
        if !self.files_available && game.is_installed {
            let path = game
                .install_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            return Some(
                div()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .child(t_args("game.hero.files_missing", &[("path", &path)]))
                    .into_any_element(),
            );
        }
        if game.is_installed && online && !game.can_run_offline {
            return Some(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t("game.hero.needs_connection"))
                    .into_any_element(),
            );
        }
        None
    }
}

/// Returns to where the page was opened from: the library when that is where
/// the user came from or there is no history (opened from a link), "Back"
/// otherwise.
fn back_button(cx: &App) -> Button {
    let previous = ember_ui::nav::navigator(cx).read(cx).previous().cloned();
    let button = Button::new("game-back").ghost().icon(IconName::ArrowLeft);
    match previous {
        None => button
            .label(t("game.page.library"))
            .tooltip(t("game.page.back_to_library"))
            .on_click(|_, _, cx| ember_ui::navigate(ember_ui::Route::Library, cx)),
        Some(route) => button
            .label(if route == ember_ui::Route::Library {
                t("game.page.library")
            } else {
                t("game.page.back")
            })
            .tooltip(t("game.page.back"))
            .on_click(|_, _, cx| ember_ui::nav::go_back(cx)),
    }
}
