//! Installing and removing DLCs of an installed game, routed per store.

use gpui_kit::{App, Window};
use irori_core::{DlcInfo, GameId, GameInfo, InstallRequest, UpdateRequest};
use irori_ui::backend;
use irori_ui::i18n::{t, t_args};

use crate::state::{DlcChange, plan_dlc_change};

/// Request installing (`install`) or removing a DLC of `base`.
pub(crate) fn toggle_dlc(
    base: &GameInfo,
    dlc: &DlcInfo,
    install: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let installed = base
        .install
        .as_ref()
        .map(|i| i.installed_dlcs.clone())
        .unwrap_or_default();
    let b = backend(cx);
    match plan_dlc_change(base.runner(), &installed, &dlc.app_name, install) {
        DlcChange::InstallSeparately => {
            let path = base
                .install_path()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            let request = InstallRequest {
                path,
                platform: base.installed_platform().unwrap_or_default(),
                ..Default::default()
            };
            b.enqueue_install(
                GameId::new(base.runner(), dlc.app_name.as_str()),
                dlc.title.clone(),
                request,
                0,
            );
        }
        DlcChange::UninstallSeparately => {
            crate::manage::open_uninstall_dialog(
                GameId::new(base.runner(), dlc.app_name.as_str()),
                window,
                cx,
            );
        }
        DlcChange::UpdateBase(dlcs) => {
            b.enqueue_update(
                base.id(),
                base.title.clone(),
                UpdateRequest {
                    dlcs: Some(dlcs),
                    ..Default::default()
                },
            );
        }
        DlcChange::Unsupported => crate::ops::notify_error(
            window,
            cx,
            t("game.dlc.change_failed"),
            &anyhow::anyhow!(t_args(
                "game.dlc.not_editable",
                &[("store", base.runner().store_name())]
            )),
        ),
    }
}

/// Whether DLCs of this store can be changed after install.
pub(crate) fn dlcs_editable(base: &GameInfo) -> bool {
    !matches!(
        plan_dlc_change(base.runner(), &[], "", true),
        DlcChange::Unsupported
    )
}
