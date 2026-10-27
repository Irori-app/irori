//! Store construction and the cross-store Heroic import.

use std::path::PathBuf;
use std::sync::Arc;

use ember_core::{DynStore, EventBus, SettingsFn, Store};
use ember_services::{MultiAccountStore, SlotDirs};
use ember_ui::hooks::{Hooks, ImportSummary};
use gpui_kit::App;

/// A store crate's constructor for one more account (`for_account`).
type ForAccount<S> = fn(EventBus, SettingsFn, PathBuf, PathBuf) -> Arc<S>;

/// `main` plus the store's other accounts.
fn with_accounts<S: Store>(
    main: Arc<S>,
    bus: &EventBus,
    settings: &SettingsFn,
    for_account: ForAccount<S>,
) -> Arc<MultiAccountStore<S>> {
    let (slot_bus, settings) = (bus.clone(), settings.clone());
    MultiAccountStore::new(
        main,
        bus.clone(),
        Arc::new(move |dirs: &SlotDirs| {
            for_account(
                slot_bus.clone(),
                settings.clone(),
                dirs.config.clone(),
                dirs.cache.clone(),
            )
        }),
    )
}

/// Every store backend, in sidebar order. Stores with accounts take several.
pub fn build(bus: EventBus, settings: SettingsFn) -> Vec<DynStore> {
    let mut stores: Vec<DynStore> = vec![
        {
            let epic = with_accounts(
                ember_epic::EpicStore::new(bus.clone(), settings.clone()),
                &bus,
                &settings,
                ember_epic::EpicStore::for_account,
            );
            let _ = EPIC.set(epic.clone());
            epic
        },
        {
            let gog = with_accounts(
                ember_gog::GogStore::new(bus.clone(), settings.clone()),
                &bus,
                &settings,
                ember_gog::GogStore::for_account,
            );
            let _ = GOG.set(gog.clone());
            gog
        },
        with_accounts(
            ember_amazon::AmazonStore::new(bus.clone(), settings.clone()),
            &bus,
            &settings,
            ember_amazon::AmazonStore::for_account,
        ),
    ];
    if settings().zoom_platform {
        stores.push(with_accounts(
            ember_zoom::ZoomStore::new(bus.clone(), settings.clone()),
            &bus,
            &settings,
            ember_zoom::ZoomStore::for_account,
        ));
    }
    // Always present so it can be turned on from its account card; it
    // shows nothing while `itch_io` is off.
    stores.push(with_accounts(
        ember_itch::ItchStore::new(bus.clone(), settings.clone()),
        &bus,
        &settings,
        ember_itch::ItchStore::for_account,
    ));
    stores.push(ember_steam::SteamStore::new(bus.clone(), settings.clone()));
    // Always present so its switch on the Stores page can turn it on; it
    // lists nothing while `third_party_launchers` is off.
    let launchers = ember_launchers::LaunchersStore::new(bus.clone(), settings.clone());
    let _ = LAUNCHERS.set(launchers.clone());
    stores.push(launchers);
    let emulation = ember_emulation::EmulationStore::new(bus.clone(), settings.clone());
    let _ = EMULATION.set(emulation.clone());
    stores.push(emulation);
    let sideload = ember_sideload::SideloadStore::new(bus, settings);
    let _ = SIDELOAD.set(sideload.clone());
    stores.push(sideload);
    stores
}

/// The concrete Epic store (EOS overlay, EGL sync use its main account).
pub static EPIC: once_cell::sync::OnceCell<Arc<MultiAccountStore<ember_epic::EpicStore>>> =
    once_cell::sync::OnceCell::new();

/// The concrete GOG store (private-branch passwords, REDmod).
pub static GOG: once_cell::sync::OnceCell<Arc<MultiAccountStore<ember_gog::GogStore>>> =
    once_cell::sync::OnceCell::new();

/// Hooks of the game feature that need concrete store crates.
pub fn game_hooks() -> ember_game::GameHooks {
    let mut hooks = ember_game::GameHooks::new();
    if let Some(gog) = GOG.get().cloned() {
        hooks = hooks.with_branch_password(Arc::new(move |id, password, cx| {
            let gog = gog.store_for(&id.app_name);
            ember_ui::run(cx, async move {
                gog.set_private_branch_password(&id.app_name, &password)
                    .await
            })
        }));
    }
    hooks
}

/// The third-party launchers store (setup and scanning on the Stores page).
pub static LAUNCHERS: once_cell::sync::OnceCell<Arc<ember_launchers::LaunchersStore>> =
    once_cell::sync::OnceCell::new();

/// The ROM library store (scan state for the setup guide).
pub static EMULATION: once_cell::sync::OnceCell<Arc<ember_emulation::EmulationStore>> =
    once_cell::sync::OnceCell::new();

/// The concrete sideload store (the library's add/edit app dialogs need its
/// own API, which `dyn Store` does not expose).
pub static SIDELOAD: once_cell::sync::OnceCell<Arc<ember_sideload::SideloadStore>> =
    once_cell::sync::OnceCell::new();

pub fn hooks() -> Hooks {
    Hooks {
        import_from_heroic: Some(Arc::new(import_from_heroic)),
        open_emulation_setup: Some(Arc::new(ember_manage::open_emulation_setup)),
        import_from_lutris: Some(Arc::new(|cx| {
            let backend = ember_ui::backend(cx);
            ember_ui::run(cx, async move {
                crate::lutris::import(&backend, SIDELOAD.get().cloned(), EMULATION.get().cloned())
                    .await
            })
        })),
    }
}

/// A store crate's Heroic import entry point.
pub type ImportFn = fn(&std::path::Path) -> anyhow::Result<()>;

/// Every store's Heroic import, with its display name.
pub const HEROIC_IMPORTS: [(&str, ImportFn); 5] = [
    ("Epic Games", ember_epic::import_from_heroic),
    ("GOG", ember_gog::import_from_heroic),
    ("Amazon Games", ember_amazon::import_from_heroic),
    ("ZOOM Platform", ember_zoom::import_from_heroic),
    ("Added manually", ember_sideload::import_from_heroic),
];

/// Copy credentials and installs from each store, then launcher data.
fn import_from_heroic(cx: &mut App) -> gpui_kit::Task<anyhow::Result<ImportSummary>> {
    let backend = ember_ui::backend(cx);
    ember_ui::run(cx, async move {
        let dir = ember_core::paths::heroic_config_dir()
            .ok_or_else(|| anyhow::anyhow!("No Heroic configuration was found"))?;
        let mut summary = ImportSummary::default();
        let imports = HEROIC_IMPORTS;
        for (name, import) in imports {
            let d = dir.clone();
            match tokio::task::spawn_blocking(move || import(&d)).await? {
                Ok(()) => summary.stores.push(name.to_string()),
                Err(e) => tracing::warn!("importing {name} from Heroic: {e:#}"),
            }
        }
        for store in backend.stores() {
            if let Err(e) = store.init().await {
                tracing::warn!("re-initialising {}: {e:#}", store.runner().store_name());
            }
        }
        crate::import::apply_launcher_data(&backend, &dir, &mut summary).await?;
        backend.refresh_all(false).await;
        Ok(summary)
    })
}
