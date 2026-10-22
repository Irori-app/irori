use ember_core::util::{format_bytes, format_duration};
use ember_core::{GameStatus, InstallProgress};

use crate::i18n::{status_label, t_args};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{App, ElementId, IntoElement, ParentElement, RenderOnce, Styled, Window, div};

/// "42% · 1.2 GiB of 4.0 GiB · 18.3 MiB/s · 3m 10s left".
pub fn progress_text(p: &InstallProgress) -> String {
    let mut parts = Vec::with_capacity(4);
    if let Some(pc) = p.percent {
        parts.push(format!("{pc:.0}%"));
    }
    match (p.bytes_done, p.bytes_total) {
        (Some(d), Some(t)) if t > 0 => {
            parts.push(format!("{} of {}", format_bytes(d), format_bytes(t)))
        }
        (Some(d), _) if d > 0 => parts.push(format_bytes(d)),
        _ => {}
    }
    if let Some(s) = p.download_speed.or(p.disk_speed).filter(|s| *s > 0) {
        parts.push(t_args("ui.progress.speed", &[("speed", &format_bytes(s))]).to_string());
    }
    if let Some(eta) = p.eta_secs.filter(|e| *e > 0) {
        parts.push(t_args("ui.progress.left", &[("time", &format_duration(eta))]).to_string());
    }
    parts.join(" · ")
}

/// Status label, progress bar and detail text of a running operation.
#[derive(IntoElement)]
pub struct ProgressSummary {
    id: ElementId,
    status: GameStatus,
}

impl ProgressSummary {
    pub fn new(id: impl Into<ElementId>, status: GameStatus) -> Self {
        Self {
            id: id.into(),
            status,
        }
    }
}

impl RenderOnce for ProgressSummary {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let label = match &self.status.context {
            Some(c) => format!("{} — {c}", status_label(self.status.kind)),
            None => status_label(self.status.kind).to_string(),
        };
        let progress = self.status.progress.unwrap_or_default();
        let detail = progress_text(&progress);
        v_flex()
            .gap_1()
            .w_full()
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(label))
                    .child(div().text_color(cx.theme().muted_foreground).child(detail)),
            )
            .child(crate::motion::gliding_progress(self.id, progress.percent))
    }
}
