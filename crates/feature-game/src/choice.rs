//! A labelled option for `Select`s whose items are store data (languages,
//! branches, builds, Wine versions, launch options).

use gpui_kit::component::IndexPath;
use gpui_kit::component::select::{SelectItem, SelectState};
use gpui_kit::{AppContext as _, Context, Entity, SharedString, Window};

/// One option: a stable value and its display title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    value: SharedString,
    title: SharedString,
}

impl Choice {
    pub(crate) fn new(value: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            title: title.into(),
        }
    }
}

impl SelectItem for Choice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &SharedString {
        &self.value
    }
}

pub(crate) type ChoiceSelect = SelectState<Vec<Choice>>;

/// Create a select over `items` with `selected` (by value) chosen.
pub(crate) fn choice_select<V: 'static>(
    items: Vec<Choice>,
    selected: Option<&str>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Entity<ChoiceSelect> {
    let ix = selected
        .and_then(|s| items.iter().position(|c| c.value.as_ref() == s))
        .map(|row| IndexPath::default().row(row));
    cx.new(|cx| SelectState::new(items, ix, window, cx))
}

/// Replace the items of a select and choose `selected` (by value).
pub(crate) fn set_choices(
    select: &Entity<ChoiceSelect>,
    items: Vec<Choice>,
    selected: Option<&str>,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) {
    let ix = selected
        .and_then(|s| items.iter().position(|c| c.value.as_ref() == s))
        .map(|row| IndexPath::default().row(row));
    select.update(cx, |state, cx| {
        state.set_items(items, window, cx);
        state.set_selected_index(ix, window, cx);
    });
}
