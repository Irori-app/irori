//! Game log viewer: a virtualized tail of the last session's log with
//! refresh, follow, copy, "open in editor" and upload (with the list of
//! recent uploads).

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::link::Link;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, VirtualListScrollHandle,
    WindowExt as _, h_flex, v_flex, v_virtual_list,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, Render, SharedString, Size, Styled, Task, Window, div, px, rems, size,
};
use irori_core::GameId;
use irori_extras::logs::{self as log_uploads, UploadedLog};
use irori_ui::backend;
use irori_ui::i18n::{t, t_args};

use crate::load::Load;
use crate::ops::{notify_error, notify_success, open_path};

/// Bytes read from the end of the log file.
const TAIL_BYTES: u64 = 1024 * 1024;
/// Lines kept for display.
const MAX_LINES: usize = 5000;
const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// Severity of a log line, for colouring and the errors filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineLevel {
    Error,
    Warning,
    Info,
}

impl LineLevel {
    pub fn of(line: &str) -> Self {
        let lower = line.to_ascii_lowercase();
        if lower.contains("err") || lower.contains("fatal") || lower.contains("panic") {
            LineLevel::Error
        } else if lower.contains("warn") {
            LineLevel::Warning
        } else {
            LineLevel::Info
        }
    }
}

/// A loaded log tail.
#[derive(Debug, Clone, Default)]
pub struct LogTail {
    pub lines: Vec<(LineLevel, SharedString)>,
    /// Earlier content was cut off.
    pub truncated: bool,
}

/// Split the tail of a log into lines, dropping a partial first line when
/// the read started mid-file, and keeping at most `max` lines.
pub fn parse_tail(text: &str, started_mid_file: bool, max: usize) -> LogTail {
    let mut lines: Vec<&str> = text.lines().collect();
    let mut truncated = started_mid_file;
    if started_mid_file && !lines.is_empty() {
        lines.remove(0);
    }
    if lines.len() > max {
        lines.drain(..lines.len() - max);
        truncated = true;
    }
    LogTail {
        lines: lines
            .into_iter()
            .map(|l| (LineLevel::of(l), SharedString::from(l.to_string())))
            .collect(),
        truncated,
    }
}

/// "Uploaded 5 minutes ago" for an upload at `uploaded_at` (unix seconds).
pub(crate) fn uploaded_ago(uploaded_at: u64, now: u64) -> SharedString {
    let secs = now.saturating_sub(uploaded_at);
    let plural = |n: u64, one: &str, other: &str| {
        t_args(
            if n == 1 { one } else { other },
            &[("count", &n.to_string())],
        )
    };
    match secs {
        s if s < 60 => t("game.logs.uploaded_just_now"),
        s if s < 3600 => plural(
            s / 60,
            "game.logs.uploaded_minutes_one",
            "game.logs.uploaded_minutes_other",
        ),
        s if s < 86_400 => plural(
            s / 3600,
            "game.logs.uploaded_hours_one",
            "game.logs.uploaded_hours_other",
        ),
        s => plural(
            s / 86_400,
            "game.logs.uploaded_days_one",
            "game.logs.uploaded_days_other",
        ),
    }
}

pub(crate) async fn read_tail(path: PathBuf) -> anyhow::Result<Option<LogTail>> {
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::anyhow!(t_args(
                "game.logs.reading_path",
                &[
                    ("path", &path.display().to_string()),
                    ("error", &e.to_string())
                ]
            )));
        }
    };
    let len = file.metadata().await?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf);
    Ok(Some(parse_tail(&text, start > 0, MAX_LINES)))
}

/// Log viewer of one file.
pub struct LogViewer {
    path: PathBuf,
    /// Name the log is uploaded under; uploads are offered when set.
    upload_name: Option<SharedString>,
    uploading: bool,
    uploads: Load<Vec<UploadedLog>>,
    /// Uploads whose deletion is in flight (by URL).
    deleting: HashSet<String>,
    tail: Load<Option<Arc<LogTail>>>,
    errors_only: bool,
    follow: bool,
    /// Indices into `tail.lines` currently shown.
    visible: Rc<Vec<usize>>,
    sizes: Rc<Vec<Size<Pixels>>>,
    sized_for: (usize, Pixels),
    scroll: VirtualListScrollHandle,
    _refresh: Task<()>,
}

/// Open the log of a game's last session in a sheet.
pub fn open_log_sheet(id: GameId, window: &mut Window, cx: &mut App) {
    let game_title = backend(cx).game(&id).map(|g| g.title.clone());
    let title = match &game_title {
        Some(title) => t_args("game.logs.title", &[("title", title)]),
        None => t("game.logs.title_plain"),
    };
    let upload_name = t_args(
        "game.logs.upload_name",
        &[(
            "title",
            &game_title.unwrap_or_else(|| id.app_name.to_string()),
        )],
    );
    let path = backend(cx).game_log(&id);
    let viewer = cx.new(|cx| LogViewer::new(path, window, cx).with_upload_name(upload_name));
    window.open_sheet(cx, move |sheet, _, _| {
        sheet
            .title(title.clone())
            .size(rems(48.))
            .child(viewer.clone())
    });
}

impl LogViewer {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn_in(window, async move |this, cx| {
            // Runs after construction, so `with_upload_name` has been applied.
            let _ = this.update(cx, |this, cx| this.load_uploads(cx));
            loop {
                let ok = this.update(cx, |this, cx| this.reload(cx)).is_ok();
                if !ok {
                    break;
                }
                cx.background_executor().timer(REFRESH_EVERY).await;
            }
        });
        Self {
            path,
            upload_name: None,
            uploading: false,
            uploads: Load::Idle,
            deleting: HashSet::new(),
            tail: Load::Loading,
            errors_only: false,
            follow: true,
            visible: Rc::new(Vec::new()),
            sizes: Rc::new(Vec::new()),
            sized_for: (usize::MAX, px(0.)),
            scroll: VirtualListScrollHandle::new(),
            _refresh: refresh,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Offer uploading the log under `name` ("Game log of Hades"), and list
    /// recent uploads.
    pub fn with_upload_name(mut self, name: impl Into<SharedString>) -> Self {
        self.upload_name = Some(name.into());
        self.uploads = Load::Loading;
        self
    }

    fn load_uploads(&mut self, cx: &mut Context<Self>) {
        if self.upload_name.is_none() {
            return;
        }
        let task = irori_ui::run(cx, log_uploads::list_uploaded());
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.uploads = Load::from_result(result);
                cx.notify();
            });
        })
        .detach();
    }

    /// Ask before sharing the log publicly, then upload it.
    pub fn request_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.upload_name.clone() else {
            return;
        };
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title(t_args("game.logs.upload_title", &[("name", &name)]))
                .description(t_args(
                    "game.logs.upload_description",
                    &[("days", &log_uploads::EXPIRY_DAYS.to_string())],
                ))
                .confirm()
                .ok_text(t("game.logs.upload"))
                .on_ok(move |_, window, cx| {
                    this.update(cx, |v, cx| v.upload(window, cx));
                    true
                })
        });
    }

    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.upload_name.clone() else {
            return;
        };
        if self.uploading {
            return;
        }
        self.uploading = true;
        cx.notify();
        let path = self.path.clone();
        let task = irori_ui::run(cx, async move {
            log_uploads::upload(&path, name.as_ref()).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.uploading = false;
                match result {
                    Ok(entry) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(entry.url.clone()));
                        notify_success(
                            window,
                            cx,
                            t_args("game.logs.uploaded", &[("url", &entry.url)]),
                        );
                    }
                    Err(e) => notify_error(
                        window,
                        cx,
                        t("game.logs.upload_failed"),
                        &e.context(t("game.logs.upload_manual_hint")),
                    ),
                }
                this.load_uploads(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_upload(&mut self, entry: &UploadedLog, window: &mut Window, cx: &mut Context<Self>) {
        if !self.deleting.insert(entry.url.clone()) {
            return;
        }
        cx.notify();
        let token = entry.delete_token.clone();
        let url = entry.url.clone();
        let task = irori_ui::run(
            cx,
            async move { log_uploads::delete_uploaded(&token).await },
        );
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.deleting.remove(&url);
                if let Err(e) = result {
                    notify_error(window, cx, t("game.logs.delete_upload_failed"), &e);
                }
                this.load_uploads(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_uploads(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let list = self.uploads.ready().filter(|l| !l.is_empty())?;
        let muted = cx.theme().muted_foreground;
        let now = irori_core::util::unix_now();
        let rows = list.iter().enumerate().map(|(ix, entry)| {
            let copy_url = entry.url.clone();
            let del = entry.clone();
            h_flex()
                .id(("log-upload", ix))
                .w_full()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(entry.name.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(muted)
                        .child(uploaded_ago(entry.uploaded_at, now)),
                )
                .child(
                    Link::new(("log-upload-open", ix))
                        .href(entry.url.clone())
                        .child(t("game.logs.open")),
                )
                .child(
                    Button::new(("log-upload-copy", ix))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Copy)
                        .tooltip(t("game.logs.copy_link"))
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                            window.push_notification(t("game.logs.link_copied"), cx);
                        }),
                )
                .child(
                    Button::new(("log-upload-delete", ix))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Trash)
                        .tooltip(t("game.logs.delete_upload"))
                        .loading(self.deleting.contains(&entry.url))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.delete_upload(&del, window, cx)
                        })),
                )
        });
        Some(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(muted)
                        .child(t("game.logs.recent_uploads")),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    /// Re-read the tail of the file.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let task = irori_ui::run(cx, read_tail(path));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tail = Load::from_result(result.map(|t| t.map(Arc::new)));
                this.rebuild_visible();
                if this.follow {
                    this.scroll.scroll_to_bottom();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn rebuild_visible(&mut self) {
        let lines = self
            .tail
            .ready()
            .and_then(|t| t.as_ref())
            .map(|t| t.lines.as_slice())
            .unwrap_or_default();
        self.visible = Rc::new(
            lines
                .iter()
                .enumerate()
                .filter(|(_, (lvl, _))| !self.errors_only || *lvl != LineLevel::Info)
                .map(|(ix, _)| ix)
                .collect(),
        );
        self.sized_for = (usize::MAX, px(0.));
    }

    fn copy(&self, cx: &mut App) {
        let Some(Some(tail)) = self.tail.ready() else {
            return;
        };
        let text = self
            .visible
            .iter()
            .filter_map(|ix| tail.lines.get(*ix))
            .map(|(_, l)| l.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let Some(Some(tail)) = self.tail.ready() else {
            return Vec::new();
        };
        let theme = cx.theme();
        range
            .filter_map(|row| {
                let ix = *self.visible.get(row)?;
                let (level, text) = tail.lines.get(ix)?;
                let color = match level {
                    LineLevel::Error => theme.danger,
                    LineLevel::Warning => theme.warning,
                    LineLevel::Info => theme.foreground,
                };
                Some(
                    div()
                        .id(ix)
                        .px_2()
                        .w_full()
                        .whitespace_nowrap()
                        .text_color(color)
                        .child(text.clone())
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for LogViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Row height follows the rem size; rebuild the measurement when either
        // the line count or the zoom changes.
        let line_height = window.rem_size() * 1.25;
        if self.sized_for != (self.visible.len(), line_height) {
            self.sizes = Rc::new(vec![size(px(0.), line_height); self.visible.len()]);
            self.sized_for = (self.visible.len(), line_height);
        }
        let theme = cx.theme();
        let path_label = self.path.display().to_string();
        let exists = matches!(self.tail, Load::Ready(Some(_)));
        let truncated = self
            .tail
            .ready()
            .and_then(|t| t.as_ref())
            .is_some_and(|t| t.truncated);
        let body = match &self.tail {
            Load::Loading | Load::Idle => div()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t("game.logs.reading"))
                .into_any_element(),
            Load::Failed(e) => div()
                .p_4()
                .text_sm()
                .text_color(theme.danger)
                .child(t_args("game.logs.read_failed", &[("error", e.as_ref())]))
                .into_any_element(),
            Load::Ready(None) => div()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t("game.logs.empty"))
                .into_any_element(),
            Load::Ready(Some(_)) => {
                let view = cx.entity();
                v_virtual_list(
                    view,
                    "log-lines",
                    self.sizes.clone(),
                    |this, range, _, cx| this.render_rows(range, cx),
                )
                .track_scroll(&self.scroll)
                .size_full()
                .py_1()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .into_any_element()
            }
        };
        let uploads = self.render_uploads(cx);
        let theme = cx.theme();
        let path_for_open = self.path.clone();
        let dir_for_open = self.path.parent().map(|p| p.to_path_buf());
        v_flex()
            .id("log-viewer")
            .w_full()
            .h(rems(36.))
            .gap_2()
            .py_2()
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("log-refresh")
                            .small()
                            .outline()
                            .icon(IconName::RefreshCw)
                            .label(t("game.logs.refresh"))
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(
                        Button::new("log-copy")
                            .small()
                            .outline()
                            .icon(IconName::Copy)
                            .label(t("game.logs.copy"))
                            .disabled(!exists)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.copy(cx);
                                window.push_notification(t("game.logs.copied"), cx);
                            })),
                    )
                    .when(self.upload_name.is_some(), |row| {
                        row.child(
                            Button::new("log-upload")
                                .small()
                                .outline()
                                .icon(IconName::Upload)
                                .label(t("game.logs.upload_ellipsis"))
                                .loading(self.uploading)
                                .disabled(!exists || self.uploading)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.request_upload(window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("log-open")
                            .small()
                            .outline()
                            .icon(IconName::ExternalLink)
                            .label(t("game.logs.open_in_editor"))
                            .disabled(!exists)
                            .on_click(move |_, window, cx| {
                                open_path(path_for_open.clone(), window, cx)
                            }),
                    )
                    .when_some(dir_for_open, |row, dir| {
                        row.child(
                            Button::new("log-folder")
                                .small()
                                .ghost()
                                .icon(IconName::FolderOpen)
                                .label(t("game.logs.show_folder"))
                                .on_click(move |_, window, cx| open_path(dir.clone(), window, cx)),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Checkbox::new("log-errors-only")
                            .label(t("game.logs.errors_only"))
                            .checked(self.errors_only)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                this.errors_only = *v;
                                this.rebuild_visible();
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("log-follow")
                            .label(t("game.logs.follow"))
                            .checked(self.follow)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                this.follow = *v;
                                if *v {
                                    this.scroll.scroll_to_bottom();
                                }
                                cx.notify();
                            })),
                    ),
            )
            .when(truncated, |col| {
                col.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t_args(
                            "game.logs.truncated",
                            &[("lines", &MAX_LINES.to_string())],
                        )),
                )
            })
            .child(
                div()
                    .id("log-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .overflow_hidden()
                    .child(body),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(path_label),
            )
            .children(uploads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert_eq!(LineLevel::of("err:module:load failed"), LineLevel::Error);
        assert_eq!(LineLevel::of("WARN something"), LineLevel::Warning);
        assert_eq!(LineLevel::of("Started"), LineLevel::Info);
    }

    #[test]
    fn upload_ages() {
        crate::i18n::register();
        assert_eq!(uploaded_ago(100, 130), "Uploaded just now");
        assert_eq!(uploaded_ago(0, 60), "Uploaded 1 minute ago");
        assert_eq!(uploaded_ago(0, 5 * 60 + 3), "Uploaded 5 minutes ago");
        assert_eq!(uploaded_ago(0, 2 * 3600), "Uploaded 2 hours ago");
        assert_eq!(uploaded_ago(0, 86_400), "Uploaded 1 day ago");
        assert_eq!(uploaded_ago(200, 100), "Uploaded just now");
    }

    #[test]
    fn tail_parsing() {
        let t = parse_tail("partial\na\nb\nc", true, 10);
        assert!(t.truncated);
        assert_eq!(t.lines.len(), 3);
        assert_eq!(t.lines[0].1.as_ref(), "a");
        let t = parse_tail("a\nb\nc\nd", false, 2);
        assert!(t.truncated);
        assert_eq!(
            t.lines.iter().map(|l| l.1.as_ref()).collect::<Vec<_>>(),
            vec!["c", "d"]
        );
        let t = parse_tail("a\nb", false, 10);
        assert!(!t.truncated);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reads_missing_and_present_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.log");
        assert!(read_tail(path.clone()).await.unwrap().is_none());
        std::fs::write(&path, "one\nerr: two\n").unwrap();
        let tail = read_tail(path).await.unwrap().unwrap();
        assert_eq!(tail.lines.len(), 2);
        assert_eq!(tail.lines[1].0, LineLevel::Error);
    }
}
