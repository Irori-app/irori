//! Settings → Backup: save what the user set up in Ember to a file, restore
//! it (on this or another computer), and export the game list.

use std::path::{Path, PathBuf};

use ember_services::{Backend, Backup, BackupParts, ExportFormat};
use ember_ui::backend;
use ember_ui::i18n::{t, t_args};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AsyncWindowContext, Context, IntoElement, ParentElement as _, PathPromptOptions, Render,
    SharedString, Styled as _, WeakEntity, Window,
};

use crate::widgets::{group_heading, muted, panel};

/// The Backup section's state.
pub struct BackupPanel {
    /// What a new backup holds.
    parts: BackupParts,
    /// A backup chosen for restoring, and the parts to restore from it.
    chosen: Option<(PathBuf, Backup)>,
    restore_parts: BackupParts,
}

impl BackupPanel {
    pub fn new() -> Self {
        Self {
            parts: BackupParts::ALL,
            chosen: None,
            restore_parts: BackupParts::ALL,
        }
    }

    /// Ask where to save a backup, then write it.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parts = self.parts;
        let name = format!("ember-backup-{}.json", today());
        let rx = cx.prompt_for_new_path(&start_dir(), Some(&name));
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(path))) = rx.await else { return };
                let result = run_backend(cx, move |b| b.write_backup(&path, parts)).await;
                report(
                    cx,
                    result.map(|()| t("settings.backup.saved")),
                    "settings.backup.save_failed",
                );
            })
            .detach();
    }

    /// Ask for a backup file and read it, ready to restore.
    fn choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t("settings.backup.choose_prompt")),
        });
        let view = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(mut paths))) = rx.await else {
                    return;
                };
                let Some(path) = paths.pop() else { return };
                let read_path = path.clone();
                let result = run_backend(cx, move |_| Backend::read_backup(&read_path)).await;
                match result {
                    Ok(backup) => {
                        let _ = view.update(cx, |this, cx| {
                            this.restore_parts = backup.parts();
                            this.chosen = Some((path, backup));
                            cx.notify();
                        });
                    }
                    Err(e) => report::<SharedString>(cx, Err(e), "settings.backup.read_failed"),
                }
            })
            .detach();
    }

    /// Restore the chosen parts of the chosen backup.
    fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, backup)) = self.chosen.clone() else {
            return;
        };
        let parts = self.restore_parts;
        let view = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let result = run_backend(cx, move |b| b.restore_backup(&backup, parts)).await;
                if result.is_ok() {
                    clear_choice(&view, cx);
                }
                report(
                    cx,
                    result.map(|_| t("settings.backup.restored")),
                    "settings.backup.restore_failed",
                );
            })
            .detach();
    }

    /// Ask where to save the game list, then write it.
    fn export(&mut self, format: ExportFormat, window: &mut Window, cx: &mut Context<Self>) {
        let ext = match format {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
        };
        let name = format!("ember-games-{}.{ext}", today());
        let rx = cx.prompt_for_new_path(&start_dir(), Some(&name));
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(path))) = rx.await else { return };
                let result = run_backend(cx, move |b| b.export_games(&path, format)).await;
                report(
                    cx,
                    result
                        .map(|n| t_args("settings.backup.exported", &[("count", &n.to_string())])),
                    "settings.backup.export_failed",
                );
            })
            .detach();
    }
}

impl Default for BackupPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for BackupPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let parts = self.parts;
        let backup = panel(cx)
            .p_4()
            .gap_3()
            .child(muted(t("settings.backup.backup_description"), cx))
            .child(part_checkboxes("backup", parts, None, cx, |this, parts| {
                this.parts = parts
            }))
            .child(
                h_flex().child(
                    Button::new("backup-save")
                        .primary()
                        .icon(IconName::Save)
                        .label(t("settings.backup.save"))
                        .disabled(!parts.any())
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                ),
            );

        let restore = match &self.chosen {
            None => panel(cx)
                .p_4()
                .gap_3()
                .child(muted(t("settings.backup.restore_description"), cx))
                .child(
                    h_flex().child(
                        Button::new("backup-choose")
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label(t("settings.backup.choose"))
                            .on_click(cx.listener(|this, _, window, cx| this.choose(window, cx))),
                    ),
                ),
            Some((path, backup)) => {
                let available = backup.parts();
                let created = backup.created.chars().take(10).collect::<String>();
                panel(cx)
                    .p_4()
                    .gap_3()
                    .child(v_flex().gap_0p5().child(file_name(path)).child(muted(
                        t_args("settings.backup.made_on", &[("date", &created)]),
                        cx,
                    )))
                    .child(part_checkboxes(
                        "restore",
                        self.restore_parts,
                        Some(available),
                        cx,
                        |this, parts| this.restore_parts = parts,
                    ))
                    .child(muted(t("settings.backup.restore_note"), cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("backup-restore")
                                    .primary()
                                    .icon(IconName::ArchiveRestore)
                                    .label(t("settings.backup.restore"))
                                    .disabled(!self.restore_parts.any())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.restore(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new("backup-cancel")
                                    .ghost()
                                    .label(t("settings.backup.cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.chosen = None;
                                        cx.notify();
                                    })),
                            ),
                    )
            }
        };

        let export = panel(cx)
            .p_4()
            .gap_3()
            .child(muted(t("settings.backup.export_description"), cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("export-csv")
                            .outline()
                            .icon(IconName::FileSpreadsheet)
                            .label(t("settings.backup.export_csv"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.export(ExportFormat::Csv, window, cx)
                            })),
                    )
                    .child(
                        Button::new("export-json")
                            .outline()
                            .icon(IconName::FileBraces)
                            .label(t("settings.backup.export_json"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.export(ExportFormat::Json, window, cx)
                            })),
                    ),
            );

        v_flex()
            .gap_2()
            .child(group_heading(t("settings.backup.backup_title"), cx))
            .child(backup)
            .child(group_heading(t("settings.backup.restore_title"), cx))
            .child(restore)
            .child(group_heading(t("settings.backup.export_title"), cx))
            .child(export)
    }
}

/// One part's checkbox: label key, checked, enabled, and how to set it.
type PartEntry = (&'static str, bool, bool, fn(&mut BackupParts, bool));

/// A checkbox per part. With `available`, parts the backup lacks are off and
/// disabled.
fn part_checkboxes(
    prefix: &'static str,
    parts: BackupParts,
    available: Option<BackupParts>,
    cx: &mut Context<BackupPanel>,
    set: fn(&mut BackupPanel, BackupParts),
) -> impl IntoElement {
    let has = available.unwrap_or(BackupParts::ALL);
    let entries: [PartEntry; 4] = [
        (
            "settings.backup.part_settings",
            parts.settings,
            has.settings,
            |p, v| p.settings = v,
        ),
        (
            "settings.backup.part_game_settings",
            parts.game_settings,
            has.game_settings,
            |p, v| p.game_settings = v,
        ),
        (
            "settings.backup.part_library",
            parts.library,
            has.library,
            |p, v| p.library = v,
        ),
        (
            "settings.backup.part_playtime",
            parts.playtime,
            has.playtime,
            |p, v| p.playtime = v,
        ),
    ];
    v_flex()
        .gap_2()
        .children(entries.into_iter().enumerate().map(
            move |(ix, (label, checked, enabled, apply))| {
                Checkbox::new(SharedString::from(format!("{prefix}-part-{ix}")))
                    .label(t(label))
                    .checked(checked && enabled)
                    .disabled(!enabled)
                    .small()
                    .on_change(cx.listener(move |this, v: &bool, _, cx| {
                        let mut next = match prefix {
                            "restore" => this.restore_parts,
                            _ => this.parts,
                        };
                        apply(&mut next, *v);
                        set(this, next);
                        cx.notify();
                    }))
            },
        ))
}

/// Run blocking backend work off the UI thread.
async fn run_backend<T: Send + 'static>(
    cx: &mut AsyncWindowContext,
    f: impl FnOnce(Backend) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let task = cx.update(|_, cx| {
        let b = backend(cx);
        ember_ui::run(cx, async move { f(b) })
    })?;
    task.await
}

/// Tell the user how an operation went.
fn report<S: Into<SharedString>>(
    cx: &mut AsyncWindowContext,
    result: anyhow::Result<S>,
    failed_key: &'static str,
) {
    let _ = cx.update(|window, cx| match result {
        Ok(message) => window.push_notification(Notification::success(message.into()), cx),
        Err(e) => window.push_notification(
            Notification::error(format!("{e:#}")).title(t(failed_key)),
            cx,
        ),
    });
}

fn clear_choice(view: &WeakEntity<BackupPanel>, cx: &mut AsyncWindowContext) {
    let _ = view.update(cx, |this, cx| {
        this.chosen = None;
        cx.notify();
    });
}

/// Where save dialogs open: Documents when there is one, else home.
fn start_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let documents = home.join("Documents");
    if documents.is_dir() { documents } else { home }
}

/// Today as `YYYY-MM-DD`, for file names.
fn today() -> String {
    ember_core::util::now_rfc3339().chars().take(10).collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
