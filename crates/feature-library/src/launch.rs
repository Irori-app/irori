//! Choosing how to start a game that offers several launch options (Epic
//! launch parameters, GOG play tasks, DLCs that start on their own…).
//!
//! Games with one option or none start with one click. With several, Play
//! opens a small picker with the last used option preselected; the backend
//! remembers the choice (`lastUsedLaunchOption`) when the game starts.
//! Unlike Heroic there is no auto-selecting countdown: the picker waits for
//! the user and Escape cancels.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, ElementId, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, StatefulInteractiveElement as _,
    Styled as _, Window, div,
};
use irori_core::LaunchOption;

use irori_ui::i18n::{t, t_args};

use crate::dialogs::dialog_width;

/// What Play does for a game with these launch options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchChoice {
    /// Start right away, with the only option when there is one.
    Direct(Option<LaunchOption>),
    /// Ask which option to use; `preselected` is the last used one (else
    /// the first).
    Ask {
        options: Vec<LaunchOption>,
        preselected: usize,
        /// `preselected` is the option used last time.
        remembered: bool,
    },
}

/// Decide between starting directly and asking.
pub fn decide(mut options: Vec<LaunchOption>, last: Option<&LaunchOption>) -> LaunchChoice {
    match options.len() {
        0 => LaunchChoice::Direct(None),
        1 => LaunchChoice::Direct(options.pop()),
        _ => {
            let last = last.and_then(|l| options.iter().position(|o| o == l));
            LaunchChoice::Ask {
                preselected: last.unwrap_or(0),
                remembered: last.is_some(),
                options,
            }
        }
    }
}

/// Label and optional detail line of an option.
pub fn option_text(option: &LaunchOption) -> (String, Option<String>) {
    match option {
        LaunchOption::Basic { name, parameters } => {
            let name = name.trim();
            let params = parameters.trim();
            let label = if name.is_empty() {
                t("library.launch.default_option").to_string()
            } else {
                name.to_string()
            };
            (label, (!params.is_empty()).then(|| params.to_string()))
        }
        LaunchOption::AltExe { executable } => {
            let file = Path::new(executable)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .filter(|f| !f.is_empty())
                .unwrap_or_else(|| executable.clone());
            (file, Some(executable.clone()))
        }
        LaunchOption::Dlc { dlc_title, .. } => {
            (dlc_title.clone(), Some(t("library.launch.dlc").to_string()))
        }
    }
}

type OnChoose = Rc<dyn Fn(LaunchOption, &mut Window, &mut App)>;

/// The picker list. Arrow keys move the selection; Enter (the dialog's
/// confirm action) or Space starts the game.
pub struct LaunchPicker {
    focus: FocusHandle,
    options: Vec<LaunchOption>,
    selected: usize,
    remembered: Option<usize>,
    on_choose: OnChoose,
}

impl Focusable for LaunchPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl LaunchPicker {
    fn choose(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(option) = self.options.get(ix).cloned() else {
            return;
        };
        window.close_dialog(cx);
        (self.on_choose)(option, window, cx);
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.options.len().saturating_sub(1);
        match event.keystroke.key.as_str() {
            "up" => self.selected = self.selected.saturating_sub(1),
            "down" => self.selected = (self.selected + 1).min(last),
            "home" => self.selected = 0,
            "end" => self.selected = last,
            "space" => {
                cx.stop_propagation();
                self.choose(self.selected, window, cx);
                return;
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for LaunchPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let rows = self.options.iter().enumerate().map(|(ix, option)| {
            let (label, detail) = option_text(option);
            ListItem::new(ElementId::Name(format!("launch-option-{ix}").into()))
                .py_2()
                .selected(ix == self.selected)
                .accessibility_label(label.clone())
                .on_click(cx.listener(move |this, _, window, cx| this.choose(ix, window, cx)))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().truncate().child(label))
                                .when_some(detail, |el, d| {
                                    el.child(div().text_xs().text_color(muted).truncate().child(d))
                                }),
                        )
                        .when(self.remembered == Some(ix), |el| {
                            el.child(
                                Tag::secondary()
                                    .xsmall()
                                    .child(t("library.launch.last_used")),
                            )
                        }),
                )
        });
        v_flex()
            .id("launch-picker")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t("library.launch.intro")),
            )
            .child(
                v_flex()
                    .id("launch-options")
                    .max_h_96()
                    .overflow_y_scroll()
                    .gap_1()
                    .children(rows),
            )
            .child(
                h_flex().justify_end().child(
                    Button::new("launch-picker-cancel")
                        .ghost()
                        .label(t("library.common.cancel"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    }
}

/// Ask which launch option to start `title` with. `on_choose` runs after the
/// dialog closed.
pub fn open_launch_picker(
    title: &str,
    options: Vec<LaunchOption>,
    preselected: usize,
    remembered: bool,
    on_choose: impl Fn(LaunchOption, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let picker = cx.new(|cx| LaunchPicker {
        focus: cx.focus_handle(),
        selected: preselected.min(options.len().saturating_sub(1)),
        remembered: remembered.then_some(preselected),
        options,
        on_choose: Rc::new(on_choose),
    });
    let width = dialog_width(window, 26.);
    let heading = t_args("library.launch.title", &[("title", title)]);
    let focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus.replace(false) {
            picker.focus_handle(cx).focus(window, cx);
        }
        let confirm = picker.clone();
        dialog
            .title(heading.clone())
            .w(width)
            .on_ok(move |_, window, cx| {
                let (option, on_choose) = {
                    let p = confirm.read(cx);
                    (p.options.get(p.selected).cloned(), p.on_choose.clone())
                };
                if let Some(option) = option {
                    on_choose(option, window, cx);
                }
                true
            })
            .child(picker.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic(name: &str, parameters: &str) -> LaunchOption {
        LaunchOption::Basic {
            name: name.into(),
            parameters: parameters.into(),
        }
    }

    #[test]
    fn one_or_no_option_starts_directly() {
        assert_eq!(decide(Vec::new(), None), LaunchChoice::Direct(None));
        let only = basic("Default", "");
        assert_eq!(
            decide(vec![only.clone()], Some(&basic("Gone", ""))),
            LaunchChoice::Direct(Some(only))
        );
    }

    #[test]
    fn several_options_ask_with_last_used_preselected() {
        let options = vec![basic("Default", ""), basic("Safe mode", "-safe")];
        assert_eq!(
            decide(options.clone(), None),
            LaunchChoice::Ask {
                options: options.clone(),
                preselected: 0,
                remembered: false
            }
        );
        assert_eq!(
            decide(options.clone(), Some(&options[1])),
            LaunchChoice::Ask {
                options: options.clone(),
                preselected: 1,
                remembered: true
            }
        );
        // A remembered option that no longer exists falls back to the first.
        assert!(matches!(
            decide(options, Some(&basic("Old", ""))),
            LaunchChoice::Ask {
                preselected: 0,
                remembered: false,
                ..
            }
        ));
    }

    #[test]
    fn labels_options() {
        crate::i18n::register();
        assert_eq!(option_text(&basic(" ", "")), ("Default".into(), None));
        assert_eq!(
            option_text(&basic("Safe mode", " -safe ")),
            ("Safe mode".into(), Some("-safe".into()))
        );
        assert_eq!(
            option_text(&LaunchOption::AltExe {
                executable: "bin/x64/Editor.exe".into()
            }),
            ("Editor.exe".into(), Some("bin/x64/Editor.exe".into()))
        );
        assert_eq!(
            option_text(&LaunchOption::Dlc {
                dlc_app_name: "d".into(),
                dlc_title: "Expansion".into()
            })
            .0,
            "Expansion"
        );
    }
}
