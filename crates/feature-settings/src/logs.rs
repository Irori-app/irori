//! Log viewer: tail of a log file with search, level filter, follow mode,
//! copy and reveal.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use ember_extras::logs::UploadedLog;
use ember_ui::i18n::{t, t_args};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, ElementId, Entity, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString, Styled as _,
    Subscription, Task, UniformListScrollHandle, Window, div, uniform_list,
};

/// Bytes read from the end of the file.
const TAIL_BYTES: u64 = 1024 * 1024;
/// Lines kept after reading the tail.
const MAX_LINES: usize = 5000;

/// Severity guessed from a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LineLevel {
    Info,
    Warning,
    Error,
}

/// Level filter of the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelFilter {
    All,
    Warnings,
    Errors,
}

impl LevelFilter {
    fn admits(self, level: LineLevel) -> bool {
        match self {
            LevelFilter::All => true,
            LevelFilter::Warnings => level >= LineLevel::Warning,
            LevelFilter::Errors => level == LineLevel::Error,
        }
    }
}

/// Classify a line by the level markers written by `tracing`, Wine and
/// games.
pub fn classify(line: &str) -> LineLevel {
    let upper = line.to_ascii_uppercase();
    if upper.contains("ERROR")
        || upper.contains(":ERR:")
        || upper.contains("[ERR")
        || upper.contains("PANIC")
    {
        LineLevel::Error
    } else if upper.contains("WARN") || upper.contains("FIXME:") {
        LineLevel::Warning
    } else {
        LineLevel::Info
    }
}

/// Decode the tail of a file: drop a partial UTF-8 sequence and a partial
/// first line when the read did not start at the beginning of the file.
pub fn decode_tail(bytes: &[u8], from_start: bool) -> String {
    let skip = bytes
        .iter()
        .take(4)
        .take_while(|b| (**b & 0xC0) == 0x80)
        .count();
    let text = String::from_utf8_lossy(&bytes[skip..]);
    if from_start {
        return text.into_owned();
    }
    match text.find('\n') {
        Some(i) => text[i + 1..].to_string(),
        None => text.into_owned(),
    }
}

/// Indices of the lines admitted by a level filter and a case-insensitive
/// query.
pub fn filter_lines(lines: &[(LineLevel, String)], query: &str, level: LevelFilter) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    lines
        .iter()
        .enumerate()
        .filter(|(_, (lvl, text))| {
            level.admits(*lvl) && (q.is_empty() || text.to_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect()
}

enum LoadState {
    Idle,
    Loading,
    Missing,
    Failed(SharedString),
    Loaded,
}

type Stamp = (u64, Option<SystemTime>);

use crate::i18n::LanguageStamp;

/// A retained log viewer for one file. Reads the last mebibyte, keeps the
/// last lines, and while following re-reads the file whenever its size or
/// modification time changes.
pub struct LogViewer {
    path: PathBuf,
    /// Name given to uploads ("Ember log", "Log of Hades").
    name: SharedString,
    uploading: bool,
    uploads: Vec<UploadedLog>,
    lines: Arc<Vec<(LineLevel, String)>>,
    visible: Arc<Vec<usize>>,
    stamp: Option<Stamp>,
    state: LoadState,
    truncated: bool,
    query: Entity<InputState>,
    level: LevelFilter,
    follow: bool,
    copied: bool,
    poll: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    language: LanguageStamp,
    _subscriptions: Vec<Subscription>,
}

impl LogViewer {
    /// A viewer for `path`, uploaded under `name` (an identifier kept in
    /// the upload history, not translated). Nothing is read until
    /// [`LogViewer::start`].
    pub fn new(
        path: PathBuf,
        name: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t("settings.logs.search")));
        let sub = cx.subscribe(&query, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.refilter(cx);
            }
        });
        Self {
            path,
            name: name.into(),
            uploading: false,
            uploads: Vec::new(),
            lines: Arc::default(),
            visible: Arc::default(),
            stamp: None,
            state: LoadState::Idle,
            truncated: false,
            query,
            level: LevelFilter::All,
            follow: true,
            copied: false,
            poll: None,
            scroll: UniformListScrollHandle::new(),
            language: LanguageStamp::new(),
            _subscriptions: vec![sub],
        }
    }

    /// The file shown.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Show another file.
    pub fn set_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.path {
            return;
        }
        self.path = path;
        self.stamp = None;
        self.lines = Arc::default();
        self.visible = Arc::default();
        self.state = LoadState::Idle;
        if self.poll.is_some() {
            self.start(window, cx);
        }
        cx.notify();
    }

    /// Load the file and keep following it while the viewer is shown.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state, LoadState::Idle) {
            self.state = LoadState::Loading;
        }
        self.load_uploads(window, cx);
        self.poll = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok((path, stamp)) = this.update(cx, |this, _| (this.path.clone(), this.stamp))
                else {
                    break;
                };
                let result = cx
                    .background_executor()
                    .spawn(async move { read_tail(&path, stamp) })
                    .await;
                let Ok(follow) = this.update(cx, |this, cx| {
                    this.apply(result, cx);
                    this.follow
                }) else {
                    break;
                };
                if !follow {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        }));
    }

    /// Stop following (the view is hidden).
    pub fn stop(&mut self) {
        self.poll = None;
    }

    fn apply(&mut self, result: anyhow::Result<Option<(Stamp, String)>>, cx: &mut Context<Self>) {
        match result {
            Ok(None) => return,
            Ok(Some((stamp, text))) => {
                self.stamp = Some(stamp);
                if stamp.0 == 0 && stamp.1.is_none() {
                    self.state = LoadState::Missing;
                    self.lines = Arc::default();
                } else {
                    let all: Vec<&str> = text.lines().collect();
                    self.truncated = all.len() > MAX_LINES || stamp.0 > TAIL_BYTES;
                    let start = all.len().saturating_sub(MAX_LINES);
                    self.lines = Arc::new(
                        all[start..]
                            .iter()
                            .map(|l| (classify(l), l.to_string()))
                            .collect(),
                    );
                    self.state = LoadState::Loaded;
                }
            }
            Err(e) => self.state = LoadState::Failed(format!("{e:#}").into()),
        }
        self.refilter(cx);
        if self.follow && !self.visible.is_empty() {
            self.scroll
                .scroll_to_item(self.visible.len() - 1, ScrollStrategy::Bottom);
        }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value();
        self.visible = Arc::new(filter_lines(&self.lines, &query, self.level));
        cx.notify();
    }

    fn set_level(&mut self, level: LevelFilter, cx: &mut Context<Self>) {
        self.level = level;
        self.refilter(cx);
    }

    fn toggle_follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.follow = !self.follow;
        if self.follow {
            self.start(window, cx);
        } else {
            self.poll = None;
        }
        cx.notify();
    }

    fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text: String = self
            .visible
            .iter()
            .filter_map(|&i| self.lines.get(i))
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Uploads made in the last two days that belong to this file's name.
    fn load_uploads(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = ember_ui::run(cx, async move { ember_extras::logs::list_uploaded().await });
        cx.spawn_in(window, async move |this, cx| {
            let uploads = task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.uploads = uploads
                    .into_iter()
                    .filter(|u| u.name == this.name.as_ref())
                    .collect();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn confirm_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let weak = weak.clone();
            alert
                .title(t("settings.logs.upload.title"))
                .description(t("settings.logs.upload.description"))
                .show_cancel(true)
                .cancel_text(t("settings.common.cancel"))
                .ok_text(t("settings.logs.upload.confirm"))
                .on_ok(move |_, window, cx| {
                    weak.update(cx, |this, cx| this.upload(window, cx)).ok();
                    true
                })
        });
    }

    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.uploading {
            return;
        }
        self.uploading = true;
        let (path, name) = (self.path.clone(), self.name.to_string());
        let task = ember_ui::run(
            cx,
            async move { ember_extras::logs::upload(&path, &name).await },
        );
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.uploading = false;
                match result {
                    Ok(log) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(log.url.clone()));
                        window.push_notification(
                            Notification::success(t_args(
                                "settings.logs.upload.link_copied",
                                &[("url", &log.url)],
                            ))
                            .title(t("settings.logs.upload.done")),
                            cx,
                        );
                        this.uploads.insert(0, log);
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}"))
                            .title(t("settings.logs.upload.failed")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn forget_upload(&mut self, token: String, window: &mut Window, cx: &mut Context<Self>) {
        let task = ember_ui::run(cx, {
            let token = token.clone();
            async move { ember_extras::logs::delete_uploaded(&token).await }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(_) => this.uploads.retain(|u| u.delete_token != token),
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}"))
                            .title(t("settings.logs.upload.delete_failed")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_uploads(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.uploads.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (border, muted, radius) = (theme.border, theme.muted_foreground, theme.radius_lg);
        let now = ember_core::util::unix_now();
        let rows = self.uploads.iter().enumerate().map(|(ix, u)| {
            let age_h = now.saturating_sub(u.uploaded_at) / 3600;
            let age = if age_h == 0 {
                t("settings.logs.upload.age_recent")
            } else {
                t_args(
                    "settings.logs.upload.age_hours",
                    &[("hours", &age_h.to_string())],
                )
            };
            let (url, token) = (u.url.clone(), u.delete_token.clone());
            h_flex()
                .gap_2()
                .px_3()
                .py_1()
                .when(ix > 0, |r| r.border_t_1().border_color(border))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(u.url.clone()),
                )
                .child(div().flex_shrink_0().text_xs().text_color(muted).child(age))
                .child(
                    Button::new(ElementId::Name(format!("upload-copy-{ix}").into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Copy)
                        .tooltip(t("settings.logs.upload.copy_link"))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))
                        }),
                )
                .child(
                    Button::new(ElementId::Name(format!("upload-delete-{ix}").into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Trash)
                        .tooltip(t("settings.logs.upload.delete"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.forget_upload(token.clone(), window, cx)
                        })),
                )
        });
        Some(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t("settings.logs.upload.recent")),
                )
                .child(
                    v_flex()
                        .rounded(radius)
                        .border_1()
                        .border_color(border)
                        .children(rows),
                ),
        )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let level = self.level;
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                Input::new(&self.query)
                    .id("log-search")
                    .aria_label(t("settings.logs.search"))
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .w_64(),
            )
            .child(
                ButtonGroup::new("log-level")
                    .outline()
                    .small()
                    .child(
                        Button::new("level-all")
                            .label(t("settings.common.all"))
                            .selected(level == LevelFilter::All),
                    )
                    .child(
                        Button::new("level-warnings")
                            .label(t("settings.logs.level.warnings"))
                            .selected(level == LevelFilter::Warnings),
                    )
                    .child(
                        Button::new("level-errors")
                            .label(t("settings.logs.level.errors"))
                            .selected(level == LevelFilter::Errors),
                    )
                    .on_click(cx.listener(|this, ix: &Vec<usize>, _, cx| {
                        let level = match ix.first() {
                            Some(1) => LevelFilter::Warnings,
                            Some(2) => LevelFilter::Errors,
                            _ => LevelFilter::All,
                        };
                        this.set_level(level, cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("log-follow")
                    .small()
                    .outline()
                    .icon(if self.follow {
                        IconName::Pause
                    } else {
                        IconName::Play
                    })
                    .label(if self.follow {
                        t("settings.logs.pause")
                    } else {
                        t("settings.logs.follow")
                    })
                    .tooltip(t("settings.logs.follow_tooltip"))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_follow(window, cx))),
            )
            .child(
                Button::new("log-copy")
                    .small()
                    .outline()
                    .icon(if self.copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .label(if self.copied {
                        t("settings.common.copied")
                    } else {
                        t("settings.common.copy")
                    })
                    .disabled(self.visible.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.copy(window, cx))),
            )
            .child(
                Button::new("log-upload")
                    .small()
                    .outline()
                    .icon(IconName::Upload)
                    .label(t("settings.logs.upload.action"))
                    .loading(self.uploading)
                    .disabled(self.uploading || !matches!(self.state, LoadState::Loaded))
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_upload(window, cx))),
            )
            .child(
                Button::new("log-reveal")
                    .small()
                    .outline()
                    .icon(IconName::FolderOpen)
                    .label(t("settings.logs.show_file"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.path.exists() {
                            cx.reveal_path(&this.path);
                        } else if let Some(dir) = this.path.parent() {
                            cx.open_with_system(dir);
                        }
                    })),
            )
    }
}

/// Read the last [`TAIL_BYTES`] of `path` unless its stamp is unchanged.
/// A missing file yields an empty stamp and text.
fn read_tail(
    path: &std::path::Path,
    previous: Option<Stamp>,
) -> anyhow::Result<Option<(Stamp, String)>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let stamp = (0, None);
            return Ok((previous != Some(stamp)).then(|| (stamp, String::new())));
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let stamp = (meta.len(), meta.modified().ok());
    if previous == Some(stamp) {
        return Ok(None);
    }
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let start = meta.len().saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((meta.len() - start) as usize);
    file.read_to_end(&mut bytes)?;
    Ok(Some((stamp, decode_tail(&bytes, start == 0))))
}

impl Render for LogViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language.changed() {
            cx.defer_in(window, |this, window, cx| {
                let placeholder = t("settings.logs.search");
                this.query
                    .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            });
        }
        let theme = cx.theme();
        let (muted, warning, danger, border) = (
            theme.muted_foreground,
            theme.warning,
            theme.danger,
            theme.border,
        );
        let mono = theme.mono_font_family.clone();
        let radius = theme.radius_lg;
        let status: Option<SharedString> = match &self.state {
            LoadState::Idle | LoadState::Loading => Some(t("settings.common.loading")),
            LoadState::Missing => Some(t_args(
                "settings.logs.missing",
                &[("path", &self.path.display().to_string())],
            )),
            LoadState::Failed(e) => Some(t_args("settings.logs.read_failed", &[("error", e)])),
            LoadState::Loaded if self.visible.is_empty() => Some(t("settings.logs.no_match")),
            LoadState::Loaded => None,
        };
        let lines = self.lines.clone();
        let visible = self.visible.clone();
        let color = move |level: LineLevel| -> Option<Hsla> {
            match level {
                LineLevel::Info => None,
                LineLevel::Warning => Some(warning),
                LineLevel::Error => Some(danger),
            }
        };
        let body = match status {
            Some(text) => div()
                .flex_1()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(text)
                .into_any_element(),
            None => uniform_list("log-lines", visible.len(), move |range, _, _| {
                range
                    .filter_map(|i| visible.get(i).and_then(|&ix| lines.get(ix)))
                    .map(|(level, text)| {
                        div()
                            .px_3()
                            .w_full()
                            .text_xs()
                            .whitespace_nowrap()
                            .truncate()
                            .font_family(mono.clone())
                            .when_some(color(*level), |d, c| d.text_color(c))
                            .child(SharedString::from(text.clone()))
                    })
                    .collect()
            })
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .py_2()
            .into_any_element(),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .gap_3()
            .child(self.render_toolbar(cx))
            .when(self.truncated, |v| {
                v.child(div().text_xs().text_color(muted).child(t_args(
                    "settings.logs.truncated",
                    &[("path", &self.path.display().to_string())],
                )))
            })
            .child(
                v_flex()
                    .id("log-body")
                    .flex_1()
                    .min_h_0()
                    .rounded(radius)
                    .border_1()
                    .border_color(border)
                    .overflow_hidden()
                    .child(body),
            )
            .children(self.render_uploads(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_levels() {
        assert_eq!(
            classify("2026-10-22T10:00:00Z ERROR ember: boom"),
            LineLevel::Error
        );
        assert_eq!(
            classify("0024:err:module:load_dll failed"),
            LineLevel::Error
        );
        assert_eq!(classify("[ERROR]: launch failed"), LineLevel::Error);
        assert_eq!(classify("  WARN ember_services: slow"), LineLevel::Warning);
        assert_eq!(classify("0024:fixme:ntdll:stub"), LineLevel::Warning);
        assert_eq!(classify("INFO started"), LineLevel::Info);
    }

    #[test]
    fn decodes_tails() {
        assert_eq!(decode_tail(b"a\nb\n", true), "a\nb\n");
        assert_eq!(decode_tail(b"partial\nfull\n", false), "full\n");
        // A cut multi-byte sequence at the start is dropped.
        let mut bytes = "é".as_bytes()[1..].to_vec();
        bytes.extend_from_slice(b"x\ny");
        assert_eq!(decode_tail(&bytes, false), "y");
    }

    #[test]
    fn filters_by_level_and_query() {
        let lines: Vec<(LineLevel, String)> = ["INFO a", "WARN b", "ERROR c", "INFO Boom"]
            .iter()
            .map(|l| (classify(l), l.to_string()))
            .collect();
        assert_eq!(filter_lines(&lines, "", LevelFilter::All), vec![0, 1, 2, 3]);
        assert_eq!(filter_lines(&lines, "", LevelFilter::Warnings), vec![1, 2]);
        assert_eq!(filter_lines(&lines, "", LevelFilter::Errors), vec![2]);
        assert_eq!(filter_lines(&lines, "boom", LevelFilter::All), vec![3]);
    }
}
