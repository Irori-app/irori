//! The downloads page ([`Route::Downloads`](irori_ui::Route::Downloads)).

mod queue;
mod speed;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, rems,
};
use irori_core::util::unix_now;
use irori_core::{GameId, GameStatus, StatusKind};
use irori_services::{DownloadKind, DownloadState, FileState};
use irori_ui::art::{LruImageCache, art_store};
use irori_ui::components::{EmptyState, GameCover, PageHeader, ProgressSummary, SectionTitle};
use irori_ui::i18n::{t, t_args};
use irori_ui::{Route, backend, models, navigate};

use crate::common::{backend_op, card, notify_error};
use crate::format;
pub(crate) use queue::{QueuePresentation, Step};
use speed::{SPEED_SAMPLES, SpeedHistory};

/// History rows rendered per "Show more" step.
const HISTORY_PAGE: usize = 25;

/// The download queue: the current download with live rates, the waiting
/// items (reorder, remove), and the finished history (retry, clear).
///
/// Shows items of every store, including ZOOM, manually added apps and
/// redistributables. Observes the downloads, library and status models.
pub struct DownloadsView {
    queue: QueuePresentation,
    speeds: SpeedHistory,
    /// Games with an update that are not queued yet.
    pending_updates: usize,
    queuing_updates: bool,
    history_shown: usize,
    images: Entity<LruImageCache>,
    _subscriptions: Vec<Subscription>,
}

impl DownloadsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::i18n::register();
        let m = models(cx).clone();
        let subscriptions = vec![
            cx.observe(&m.downloads, |this, _, cx| {
                this.rebuild(cx);
                cx.notify();
            }),
            cx.observe(&m.library, |this, _, cx| {
                this.rebuild(cx);
                cx.notify();
            }),
            cx.observe(&art_store(cx), |_, _, cx| cx.notify()),
            cx.observe(&m.statuses, |this, _, cx| {
                let changed = this.sample_speed(cx);
                if changed || this.queue.current.is_some() {
                    cx.notify();
                }
            }),
        ];
        let mut view = Self {
            queue: QueuePresentation::default(),
            speeds: SpeedHistory::new(SPEED_SAMPLES),
            pending_updates: 0,
            queuing_updates: false,
            history_shown: HISTORY_PAGE,
            images: LruImageCache::new(48, cx),
            _subscriptions: subscriptions,
        };
        view.rebuild(cx);
        view
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let m = models(cx).clone();
        let b = backend(cx);
        let library = m.library.read(cx);
        let snapshot = library.snapshot.clone();
        let updates = library.updates.clone();
        let downloads = &m.downloads.read(cx).queue;
        self.queue = QueuePresentation::build(downloads, |id| {
            snapshot.get(id).cloned().or_else(|| b.game(id))
        });
        self.pending_updates = updates
            .iter()
            .filter(|id| !downloads.active.iter().any(|i| &i.id == *id))
            .count();
        self.sample_speed(cx);
    }

    fn sample_speed(&mut self, cx: &mut Context<Self>) -> bool {
        let id = self
            .queue
            .current
            .as_ref()
            .filter(|c| !c.paused)
            .map(|c| c.item.id.clone());
        let progress = id
            .as_ref()
            .and_then(|id| models(cx).statuses.read(cx).get(id))
            .and_then(|s| s.progress);
        self.speeds.record(id.as_ref(), progress)
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        if self.queue.paused {
            backend_op(cx, |b| b.resume_downloads());
        } else {
            backend_op(cx, |b| b.pause_downloads());
        }
    }

    fn update_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.queuing_updates {
            return;
        }
        self.queuing_updates = true;
        cx.notify();
        let b = backend(cx);
        let task = irori_ui::run(cx, async move {
            b.enqueue_all_updates().await;
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.queuing_updates = false;
                if let Err(e) = result {
                    notify_error(t("manage.downloads.update_all_failed"), &e, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn move_item(&mut self, id: &GameId, step: Step, cx: &mut Context<Self>) {
        if let Some(target) = self.queue.step_target(id, step) {
            let id = id.clone();
            backend_op(cx, move |b| b.reorder_download(&id, target));
        }
    }

    fn drop_item(&mut self, dragged: &GameId, onto: &GameId, cx: &mut Context<Self>) {
        if let Some(target) = self.queue.drop_target(dragged, onto) {
            let id = dragged.clone();
            backend_op(cx, move |b| b.reorder_download(&id, target));
        }
    }

    /// Remove a waiting item, with an undo that puts it back in place.
    fn remove_item(&mut self, id: &GameId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.queue.waiting.iter().find(|r| &r.item.id == id) else {
            return;
        };
        let item = row.item.clone();
        let restore_at = self.queue.restore_index(row.waiting_ix);
        let remove_id = item.id.clone();
        backend_op(cx, move |b| b.cancel_download(&remove_id, true));
        let title = item.title.clone();
        window.push_notification(
            Notification::info(t_args("manage.downloads.removed", &[("title", &title)]))
                .action(move |_, _, cx| {
                    let note = cx.entity().downgrade();
                    let item = item.clone();
                    Button::new("undo-remove")
                        .label(t("manage.downloads.undo"))
                        .small()
                        .on_click(move |_, window, cx| {
                            let item = item.clone();
                            backend_op(cx, move |b| {
                                match item.kind.clone() {
                                    DownloadKind::Install(req) => b.enqueue_install(
                                        item.id.clone(),
                                        item.title.clone(),
                                        req,
                                        item.download_size,
                                    ),
                                    DownloadKind::Update(req) => {
                                        b.enqueue_update(item.id.clone(), item.title.clone(), req)
                                    }
                                }
                                b.reorder_download(&item.id, restore_at);
                                b.remove_finished_download(&item.id);
                            });
                            let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                        })
                })
                .autohide(true),
            cx,
        );
    }

    fn retry(&mut self, id: &GameId, cx: &mut Context<Self>) {
        let Some(row) = self.queue.history.iter().find(|r| &r.item.id == id) else {
            return;
        };
        let item = row.item.clone();
        backend_op(cx, move |b| {
            b.remove_finished_download(&item.id);
            match item.kind {
                DownloadKind::Install(req) => {
                    b.enqueue_install(item.id, item.title, req, item.download_size)
                }
                DownloadKind::Update(req) => b.enqueue_update(item.id, item.title, req),
            }
        });
    }

    fn confirm_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.queue.current.as_ref() else {
            return;
        };
        let id = current.item.id.clone();
        let title = current.item.title.clone();
        let is_install = matches!(current.item.kind, DownloadKind::Install(_));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let keep_id = id.clone();
            let delete_id = id.clone();
            let (heading, description, keep_label) = if is_install {
                (
                    t_args("manage.downloads.stop_install.title", &[("title", &title)]),
                    t("manage.downloads.stop_install.description"),
                    t("manage.downloads.stop_install.keep_files"),
                )
            } else {
                (
                    t_args("manage.downloads.stop_update.title", &[("title", &title)]),
                    t("manage.downloads.stop_update.description"),
                    t("manage.downloads.stop_update.stop"),
                )
            };
            alert.title(heading).description(description).footer(
                DialogFooter::new()
                    .child(
                        Button::new("keep-downloading")
                            .label(t("manage.downloads.keep_downloading"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("stop-keep")
                            .label(keep_label)
                            .when(!is_install, |b| b.danger())
                            .on_click(move |_, window, cx| {
                                let id = keep_id.clone();
                                backend_op(cx, move |b| b.cancel_download(&id, true));
                                window.close_dialog(cx);
                            }),
                    )
                    .when(is_install, |footer| {
                        footer.child(
                            Button::new("stop-delete")
                                .danger()
                                .label(t("manage.downloads.stop_install.delete_files"))
                                .on_click(move |_, window, cx| {
                                    let id = delete_id.clone();
                                    backend_op(cx, move |b| b.cancel_download(&id, false));
                                    window.close_dialog(cx);
                                }),
                        )
                    }),
            )
        });
    }

    /// Status line under the title: independent facts joined by " · ".
    fn summary(&self) -> String {
        let mut parts: Vec<SharedString> = Vec::new();
        if let Some(c) = &self.queue.current {
            parts.push(if c.paused {
                t("manage.downloads.summary.paused")
            } else {
                t("manage.downloads.summary.downloading")
            });
        }
        let waiting = self.queue.waiting.len();
        if waiting > 0 {
            let count = waiting.to_string();
            parts.push(if waiting == 1 {
                t_args("manage.downloads.summary.waiting_one", &[("count", &count)])
            } else {
                t_args(
                    "manage.downloads.summary.waiting_other",
                    &[("count", &count)],
                )
            });
        }
        if self.pending_updates > 0 {
            let count = self.pending_updates.to_string();
            parts.push(if self.pending_updates == 1 {
                t_args("manage.downloads.summary.updates_one", &[("count", &count)])
            } else {
                t_args(
                    "manage.downloads.summary.updates_other",
                    &[("count", &count)],
                )
            });
        }
        if parts.is_empty() {
            t("manage.downloads.summary.idle").to_string()
        } else {
            parts.join(" · ")
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut header = PageHeader::new(t("manage.downloads.title")).description(self.summary());
        if self.pending_updates > 0 {
            header = header.action(
                Button::new("update-all")
                    .icon(IconName::RefreshCw)
                    .label(t_args(
                        "manage.downloads.update_all",
                        &[("count", &self.pending_updates.to_string())],
                    ))
                    .loading(self.queuing_updates)
                    .on_click(cx.listener(|this, _, window, cx| this.update_all(window, cx))),
            );
        }
        if self.queue.has_active() {
            let paused = self.queue.paused;
            header = header.action(
                Button::new("toggle-pause")
                    .when(paused, |b| b.primary())
                    .icon(if paused {
                        IconName::Play
                    } else {
                        IconName::Pause
                    })
                    .label(if paused {
                        t("manage.downloads.resume")
                    } else {
                        t("manage.downloads.pause")
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx))),
            );
        }
        header
    }

    fn render_current(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(current) = self.queue.current.as_ref() else {
            return card(cx)
                .child(
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(IconName::CircleCheck).small())
                        .child(if self.queue.waiting.is_empty() {
                            t("manage.downloads.current.idle")
                        } else {
                            t("manage.downloads.current.paused_queue")
                        }),
                )
                .into_any_element();
        };
        let theme = cx.theme();
        let id = current.item.id.clone();
        let status = models(cx).statuses.read(cx).get(&id).cloned();
        let progress = status.as_ref().and_then(|s| s.progress);
        let meta = [
            Some(queue::kind_label(&current.item.kind).to_string()),
            Some(queue::store_label(id.runner).to_string()),
            format::size(current.item.download_size),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");

        let progress_block: AnyElement = if current.paused {
            h_flex()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::CirclePause).small())
                .child(t("manage.downloads.current.paused"))
                .into_any_element()
        } else {
            let status = status.clone().unwrap_or_else(|| {
                let kind = match current.item.kind {
                    DownloadKind::Install(_) => StatusKind::Installing,
                    DownloadKind::Update(_) => StatusKind::Updating,
                };
                GameStatus::new(id.clone(), kind)
            });
            ProgressSummary::new("current-progress", status).into_any_element()
        };

        let game_id = id.clone();
        card(cx)
            .id("current-download")
            .child(
                h_flex()
                    .items_start()
                    .gap_6()
                    .child(GameCover::new(current.game.clone()).width(rems(7.)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_4()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_semibold()
                                            .truncate()
                                            .child(current.item.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(meta),
                                    ),
                            )
                            .child(progress_block)
                            .when(!current.paused, |this| {
                                this.child(self.render_rates(progress, window, cx))
                            })
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("cancel-current")
                                            .outline()
                                            .label(t("manage.downloads.cancel"))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.confirm_cancel(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("open-current")
                                            .ghost()
                                            .label(t("manage.downloads.game_page"))
                                            .on_click(move |_, _, cx| {
                                                navigate(Route::Game(game_id.clone()), cx)
                                            }),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// Rate sparkline plus the current download/disk rates, time left and
    /// what limits the speed.
    fn render_rates(
        &self,
        progress: Option<irori_core::InstallProgress>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let progress = progress.unwrap_or_default();
        let stat = |label: SharedString, value: String| {
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().text_sm().font_medium().child(value))
        };
        let chart: AnyElement = if self.speeds.len() >= 2 {
            const MIB: f64 = 1024.0 * 1024.0;
            let data: Vec<(usize, f64, f64)> = self
                .speeds
                .samples()
                .enumerate()
                .map(|(ix, s)| (ix, s.download as f64 / MIB, s.disk as f64 / MIB))
                .collect();
            AreaChart::new(data)
                .id("download-rates")
                .x(|d: &(usize, f64, f64)| d.0.to_string())
                .y(|d: &(usize, f64, f64)| d.1)
                .stroke(theme.chart_1)
                .fill(theme.chart_1.opacity(0.25))
                .name(t("manage.downloads.chart.download"))
                .y(|d: &(usize, f64, f64)| d.2)
                .stroke(theme.chart_2)
                .fill(theme.chart_2.opacity(0.0))
                .name(t("manage.downloads.chart.disk"))
                .x_axis(false)
                .y_axis(false)
                .grid(false)
                .point_count(self.speeds.capacity())
                .appear(false)
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t("manage.downloads.measuring"))
                .into_any_element()
        };
        h_flex()
            .gap_6()
            .items_end()
            .child(div().flex_1().min_w_0().h_16().child(chart))
            .child(stat(
                t("manage.downloads.stat.download"),
                format::rate(progress.download_speed),
            ))
            .child(stat(
                t("manage.downloads.stat.disk"),
                format::rate(progress.disk_speed),
            ))
            .child(stat(
                t("manage.downloads.stat.time_left"),
                format::eta(progress.eta_secs),
            ))
            .child(stat(
                t("manage.downloads.stat.limit"),
                format::bottleneck(progress.bottleneck).to_string(),
            ))
    }

    fn render_queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = unix_now();
        let rows = self.queue.waiting.iter().map(|row| {
            let theme = cx.theme();
            let id = row.item.id.clone();
            let key = id.key();
            let meta = [
                Some(queue::kind_label(&row.item.kind).to_string()),
                Some(queue::store_label(id.runner).to_string()),
                format::size(row.item.download_size),
                Some(
                    t_args(
                        "manage.downloads.queue.added",
                        &[("time", &format::relative_time(row.item.added_at, now))],
                    )
                    .to_string(),
                ),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            let dragged = DraggedDownload {
                id: id.clone(),
                title: row.item.title.clone().into(),
            };
            let (up_id, down_id, remove_id, drop_id) =
                (id.clone(), id.clone(), id.clone(), id.clone());
            h_flex()
                .id(ElementId::Name(format!("queue-{key}").into()))
                .debug_selector(|| format!("queue-{key}"))
                .gap_3()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .hover(|s| s.bg(theme.muted))
                .on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
                .drag_over::<DraggedDownload>(|style, _, _, cx| style.bg(cx.theme().drop_target))
                .on_drop(cx.listener(move |this, d: &DraggedDownload, _, cx| {
                    this.drop_item(&d.id, &drop_id, cx)
                }))
                .child(
                    Icon::new(IconName::GripVertical)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(GameCover::new(row.game.clone()).width(rems(2.5)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().font_medium().truncate().child(row.item.title.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(meta),
                        ),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .flex_shrink_0()
                        .child(
                            Button::new("move-up")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowUp)
                                .tooltip(t("manage.downloads.queue.move_up"))
                                .disabled(row.is_first)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.move_item(&up_id, Step::Up, cx)
                                })),
                        )
                        .child(
                            Button::new("move-down")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowDown)
                                .tooltip(t("manage.downloads.queue.move_down"))
                                .disabled(row.is_last)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.move_item(&down_id, Step::Down, cx)
                                })),
                        )
                        .child(
                            Button::new("remove")
                                .ghost()
                                .small()
                                .icon(IconName::X)
                                .tooltip(t("manage.downloads.queue.remove"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.remove_item(&remove_id, window, cx)
                                })),
                        ),
                )
        });
        v_flex()
            .gap_2()
            .child(SectionTitle::new(t_args(
                "manage.downloads.queue.title",
                &[("count", &self.queue.waiting.len().to_string())],
            )))
            .child(v_flex().gap_1().children(rows))
    }

    /// Offline installers and goodies being or recently downloaded.
    fn render_files(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let files = models(cx).downloads.read(cx).files.clone();
        if files.is_empty() {
            return None;
        }
        let library = models(cx).library.read(cx).snapshot.clone();
        let any_finished = files.iter().any(|f| !f.is_active());
        let rows = files.iter().rev().map(|f| {
            let theme = cx.theme();
            let key = f.key.clone();
            let game = library.get(&f.game).cloned();
            let name = f.name.clone().unwrap_or_else(|| f.file.name.clone());
            let (state, color) = match f.state {
                FileState::Queued => (t("manage.downloads.files.queued"), theme.muted_foreground),
                FileState::Running => (t("manage.downloads.files.running"), theme.muted_foreground),
                FileState::Done => (t("manage.downloads.history.done"), theme.success),
                FileState::Failed => (t("manage.downloads.history.failed"), theme.danger),
                FileState::Canceled => (
                    t("manage.downloads.history.canceled"),
                    theme.muted_foreground,
                ),
            };
            let progress = (f.state == FileState::Running && f.file.size > 0)
                .then(|| (f.downloaded as f32 / f.file.size as f32 * 100.0).clamp(0.0, 100.0));
            let meta = [
                game.as_ref().map(|g| g.title.clone()),
                format::size(f.file.size),
                Some(match progress {
                    Some(p) => format!("{state} · {p:.0}%"),
                    None => state.to_string(),
                }),
                f.error.clone(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            let path = f.path().filter(|_| f.state == FileState::Done);
            let retry = (matches!(f.state, FileState::Failed | FileState::Canceled))
                .then(|| (f.game.clone(), f.file.clone(), f.folder.clone()));
            let cancel_key = key.clone();
            h_flex()
                .id(ElementId::Name(format!("file-{key}").into()))
                .gap_3()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .child(match game {
                    Some(g) => GameCover::new(g).width(rems(2.5)).into_any_element(),
                    None => div().w(rems(2.5)).into_any_element(),
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().font_medium().truncate().child(name))
                        .child(div().text_xs().text_color(color).truncate().child(meta))
                        .children(progress.map(|p| {
                            irori_ui::motion::gliding_progress(
                                ElementId::Name(format!("file-progress-{key}").into()),
                                Some(p),
                            )
                        })),
                )
                .when(f.is_active(), |d| {
                    d.child(
                        Button::new(ElementId::Name(format!("file-cancel-{key}").into()))
                            .ghost()
                            .small()
                            .label(t("manage.downloads.files.cancel"))
                            .on_click(move |_, _, cx| backend(cx).cancel_extra(&cancel_key)),
                    )
                })
                .children(path.map(|path| {
                    Button::new(ElementId::Name(format!("file-show-{key}").into()))
                        .ghost()
                        .small()
                        .label(t("manage.downloads.files.show"))
                        .on_click(move |_, _, cx| cx.reveal_path(&path))
                }))
                .children(retry.map(|(game, file, folder)| {
                    Button::new(ElementId::Name(format!("file-retry-{key}").into()))
                        .ghost()
                        .small()
                        .label(t("manage.downloads.history.retry"))
                        .on_click(move |_, _, cx| {
                            backend(cx).download_extra(game.clone(), file.clone(), folder.clone())
                        })
                }))
        });
        Some(
            v_flex()
                .gap_2()
                .child(SectionTitle::new(t("manage.downloads.files.title")).when(
                    any_finished,
                    |s| {
                        s.trailing(
                            Button::new("clear-files")
                                .ghost()
                                .small()
                                .label(t("manage.downloads.files.clear"))
                                .on_click(|_, _, cx| backend(cx).clear_finished_extras()),
                        )
                    },
                ))
                .child(v_flex().gap_1().children(rows))
                .into_any_element(),
        )
    }

    fn render_history(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = unix_now();
        let total = self.queue.history.len();
        let rows = self
            .queue
            .history
            .iter()
            .take(self.history_shown)
            .map(|row| {
                let theme = cx.theme();
                let id = row.item.id.clone();
                let key = format!("{}-{}", id.key(), row.item.finished_at.unwrap_or(0));
                let (icon, color, state) = match row.item.state {
                    DownloadState::Done => (
                        IconName::CircleCheck,
                        theme.success,
                        t("manage.downloads.history.done"),
                    ),
                    DownloadState::Failed => (
                        IconName::CircleX,
                        theme.danger,
                        t("manage.downloads.history.failed"),
                    ),
                    _ => (
                        IconName::Ban,
                        theme.muted_foreground,
                        t("manage.downloads.history.canceled"),
                    ),
                };
                let when = row
                    .item
                    .finished_at
                    .map(|at| format::relative_time(at, now).to_string())
                    .unwrap_or_default();
                let meta = [
                    queue::kind_label(&row.item.kind).to_string(),
                    queue::store_label(id.runner).to_string(),
                    state.to_string(),
                    when,
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
                let (retry_id, remove_id, page_id) = (id.clone(), id.clone(), id.clone());
                let retryable = row.is_retryable();
                let is_done = row.item.state == DownloadState::Done;
                h_flex()
                    .id(ElementId::Name(format!("history-{key}").into()))
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .child(GameCover::new(row.game.clone()).width(rems(2.5)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().font_medium().truncate().child(row.item.title.clone()))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(Icon::new(icon).xsmall().text_color(color))
                                    .child(div().truncate().child(meta)),
                            )
                            .when_some(row.item.error.clone(), |this, err| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.danger)
                                        .line_clamp(2)
                                        .child(err),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_shrink_0()
                            .when(retryable, |this| {
                                this.child(
                                    Button::new("retry")
                                        .small()
                                        .icon(IconName::RotateCcw)
                                        .label(t("manage.downloads.history.retry"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.retry(&retry_id, cx)
                                        })),
                                )
                            })
                            .when(is_done, |this| {
                                this.child(
                                    Button::new("open")
                                        .small()
                                        .ghost()
                                        .label(t("manage.downloads.game_page"))
                                        .on_click(move |_, _, cx| {
                                            navigate(Route::Game(page_id.clone()), cx)
                                        }),
                                )
                            })
                            .child(
                                Button::new("forget")
                                    .ghost()
                                    .small()
                                    .icon(IconName::X)
                                    .tooltip(t("manage.downloads.history.remove"))
                                    .on_click(move |_, _, cx| {
                                        let id = remove_id.clone();
                                        backend_op(cx, move |b| b.remove_finished_download(&id));
                                    }),
                            ),
                    )
            });
        v_flex()
            .gap_2()
            .child(
                SectionTitle::new(t("manage.downloads.history.title")).trailing(
                    Button::new("clear-history")
                        .ghost()
                        .small()
                        .label(t("manage.downloads.history.clear"))
                        .on_click(|_, _, cx| backend_op(cx, |b| b.clear_finished_downloads())),
                ),
            )
            .child(v_flex().gap_1().children(rows))
            .when(total > self.history_shown, |this| {
                this.child(
                    div().child(
                        Button::new("history-more")
                            .ghost()
                            .small()
                            .label(t_args(
                                "manage.downloads.history.show_more",
                                &[("count", &(total - self.history_shown).to_string())],
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.history_shown += HISTORY_PAGE;
                                cx.notify();
                            })),
                    ),
                )
            })
    }
}

impl Render for DownloadsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self.render_files(cx);
        let body: AnyElement = if self.queue.is_empty() && files.is_none() {
            EmptyState::new(IconName::Download, t("manage.downloads.empty.title"))
                .description(t("manage.downloads.empty.description"))
                .into_any_element()
        } else {
            v_flex()
                .gap_6()
                .child(self.render_current(window, cx))
                .when(!self.queue.waiting.is_empty(), |this| {
                    this.child(self.render_queue(cx))
                })
                .children(files)
                .when(!self.queue.history.is_empty(), |this| {
                    this.child(self.render_history(cx))
                })
                .into_any_element()
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(div().px_6().pt_6().pb_4().child(self.render_header(cx)))
            .child(
                div()
                    .id("downloads-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .px_6()
                            .pb_6()
                            .image_cache(self.images.clone())
                            .child(body),
                    ),
            )
    }
}

/// Drag payload and preview of a queued download.
#[derive(Clone)]
struct DraggedDownload {
    id: GameId,
    title: SharedString,
}

impl Render for DraggedDownload {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_1()
            .rounded(theme.radius)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .text_sm()
            .child(self.title.clone())
    }
}

#[cfg(test)]
impl DownloadsView {
    pub(crate) fn queue_for_tests(&self) -> &QueuePresentation {
        &self.queue
    }
}
