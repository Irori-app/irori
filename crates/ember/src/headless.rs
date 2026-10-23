//! Command-line operations that run without a window: `ember <command>`.
//! Useful for scripts, SSH sessions and troubleshooting.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ember_core::util::{format_bytes, format_duration};
use ember_core::{GameId, InstallInfoQuery, InstallPlatform, InstallRequest, RemoveArgs, Runner};
use ember_services::{Backend, DownloadState};

pub const COMMANDS: &[&str] = &[
    "import-heroic",
    "list",
    "refresh",
    "info",
    "playtime",
    "install",
    "uninstall",
    "accounts",
];

pub const HELP: &str = "Commands (run without opening a window):
  ember accounts                      Show which stores are logged in
  ember import-heroic                 Import accounts, installs and settings from Heroic
  ember refresh                       Refresh every store's library
  ember list [--installed]            List the library
  ember info <runner>/<app> [--platform windows|linux]
                                      Show download size, languages, DLCs and builds
  ember playtime <runner>/<app>       Show recorded playtime (and GOG's server total)
  ember install <runner>/<app> [--path DIR] [--platform windows|linux]
                                      Install a game (Ctrl+C stops; run again to resume)
  ember uninstall <runner>/<app> [--keep-files]
                                      Uninstall a game, or discard a stopped install";

/// Run a headless command. Returns the process exit code.
pub fn run(args: &[String]) -> Result<i32> {
    let backend = Backend::start_headless(crate::stores::build)?;
    let rt = backend.runtime();
    rt.block_on(async {
        wait_for_stores(&backend).await;
        match args[0].as_str() {
            "accounts" => accounts(&backend),
            "import-heroic" => import_heroic(&backend).await,
            "refresh" => {
                backend.refresh_all(false).await;
                println!("{} games", backend.library().len());
                Ok(0)
            }
            "list" => list(&backend, args.iter().any(|a| a == "--installed")),
            "info" => info(&backend, &args[1..]).await,
            "playtime" => playtime(&backend, &args[1..]).await,
            "install" => install(&backend, &args[1..]).await,
            "uninstall" => uninstall(&backend, &args[1..]).await,
            other => bail!("unknown command {other}"),
        }
    })
}

/// Stores initialise in the background at startup; wait until the first
/// library build happened.
async fn wait_for_stores(backend: &Backend) {
    for _ in 0..200 {
        if backend.library().revision > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn parse_id(s: Option<&String>) -> Result<GameId> {
    let s = s.context("missing <runner>/<app>")?;
    let (r, app) = s.split_once('/').context("expected <runner>/<app>")?;
    let runner = Runner::from_id(r).with_context(|| format!("unknown runner {r}"))?;
    Ok(GameId::new(runner, app))
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
}

fn accounts(backend: &Backend) -> Result<i32> {
    for store in backend.stores() {
        let state = match store.user() {
            Some(u) => format!("logged in as {}", u.display_name),
            None => "not logged in".into(),
        };
        let count = backend.library().count(store.runner());
        println!(
            "{:<16} {state} ({count} games)",
            store.runner().store_name()
        );
    }
    Ok(0)
}

async fn import_heroic(backend: &Backend) -> Result<i32> {
    let dir =
        ember_core::paths::heroic_config_dir().context("No Heroic configuration was found")?;
    let imports = crate::stores::HEROIC_IMPORTS;
    for (name, import) in imports {
        let d = dir.clone();
        match tokio::task::spawn_blocking(move || import(&d)).await? {
            Ok(()) => println!("✓ {name}"),
            Err(e) => println!("✗ {name}: {e:#}"),
        }
    }
    for store in backend.stores() {
        store.init().await?;
    }
    let mut summary = ember_ui::hooks::ImportSummary::default();
    crate::import::apply_launcher_data(backend, &dir, &mut summary).await?;
    println!(
        "Settings for {} games, {} favourites, {} categories",
        summary.games_with_settings, summary.favourites, summary.categories
    );
    backend.refresh_all(false).await;
    accounts(backend)
}

fn list(backend: &Backend, installed_only: bool) -> Result<i32> {
    for g in backend
        .library()
        .games
        .iter()
        .filter(|g| !installed_only || g.is_installed)
    {
        let state = match &g.install {
            Some(i) if g.is_installed => format!("installed {} {}", i.platform, i.install_size),
            _ => String::new(),
        };
        println!(
            "{:<10} {:<28} {:<40} {state}",
            g.runner().short_name(),
            g.app_name,
            g.title
        );
    }
    Ok(0)
}

async fn info(backend: &Backend, args: &[String]) -> Result<i32> {
    let id = parse_id(args.first())?;
    let store = backend.store(id.runner).context("store not available")?;
    let platform = flag_value(args, "--platform")
        .and_then(|p| InstallPlatform::parse(p))
        .unwrap_or_default();
    let i = store
        .install_info(&id.app_name, platform, &InstallInfoQuery::default())
        .await?;
    println!("{} ({platform})", i.title);
    println!("  download   {}", format_bytes(i.download_size));
    println!("  on disk    {}", format_bytes(i.disk_size));
    println!("  version    {}", i.version.unwrap_or_default());
    println!("  languages  {}", i.languages.join(", "));
    println!("  native     {}", i.is_native);
    for d in &i.dlcs {
        println!("  dlc        {} ({})", d.title, d.app_name);
    }
    for b in i.builds.iter().take(5) {
        println!("  build      {} {} {}", b.build_id, b.version_name, b.date);
    }
    if !i.branches.is_empty() {
        println!("  branches   {}", i.branches.join(", "));
    }
    if !i.dependencies.is_empty() {
        println!("  requires   {}", i.dependencies.join(", "));
    }
    Ok(0)
}

async fn playtime(backend: &Backend, args: &[String]) -> Result<i32> {
    let id = parse_id(args.first())?;
    let user = backend.userdata.snapshot();
    let rec = user.playtime.get(&id.key()).cloned().unwrap_or_default();
    println!(
        "Ember:  {} min (last played {})",
        rec.total_minutes,
        rec.last_played.as_deref().unwrap_or("never")
    );
    if id.runner == Runner::Gog
        && let Some(gog) = crate::stores::GOG.get()
    {
        match gog.gog_playtime(&id.app_name).await {
            Some(m) => println!("GOG:    {m} min"),
            None => println!("GOG:    unavailable"),
        }
    }
    Ok(0)
}

async fn install(backend: &Backend, args: &[String]) -> Result<i32> {
    let id = parse_id(args.first())?;
    let game = backend
        .game(&id)
        .with_context(|| format!("{id} is not in the library"))?;
    if game.is_installed {
        bail!("{} is already installed", game.title);
    }
    let store = backend.store(id.runner).context("store not available")?;
    let platform = flag_value(args, "--platform")
        .and_then(|p| InstallPlatform::parse(p))
        .unwrap_or_else(|| {
            if game.platforms.contains(&InstallPlatform::Linux) {
                InstallPlatform::Linux
            } else {
                InstallPlatform::Windows
            }
        });
    let path = flag_value(args, "--path")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&backend.settings.app().default_install_path));
    let info = store
        .install_info(&id.app_name, platform, &InstallInfoQuery::default())
        .await?;
    println!(
        "Installing {} ({platform}) to {} — download {}, on disk {}",
        game.title,
        path.display(),
        format_bytes(info.download_size),
        format_bytes(info.disk_size)
    );
    let request = InstallRequest {
        path,
        platform,
        language: preferred_language(&info.languages, &backend.settings.app().ui_language),
        dependencies: info.dependencies.clone(),
        ..Default::default()
    };
    backend.enqueue_install(id.clone(), game.title.clone(), request, info.download_size);

    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    let mut stopped = false;
    loop {
        tokio::select! {
            _ = &mut stop, if !stopped => {
                println!("\nStopping (run the same command to resume)…");
                backend.cancel_download(&id, true);
                stopped = true;
            }
            _ = tokio::time::sleep(Duration::from_millis(1000)) => {}
        }
        if let Some(s) = backend.statuses.get(&id)
            && let Some(p) = s.progress
        {
            print!(
                "\r{} {:>5.1}%  {} / {}  {}/s  eta {}      ",
                s.kind.label(),
                p.percent.unwrap_or(0.0),
                format_bytes(p.bytes_done.unwrap_or(0)),
                format_bytes(p.bytes_total.unwrap_or(info.download_size)),
                format_bytes(p.download_speed.unwrap_or(0)),
                format_duration(p.eta_secs.unwrap_or(0)),
            );
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
        }
        if !backend.is_queued(&id) {
            break;
        }
    }
    let finished = backend
        .downloads()
        .finished
        .into_iter()
        .find(|i| i.id == id);
    println!();
    match finished.map(|i| (i.state, i.error)) {
        Some((DownloadState::Done, _)) => {
            println!("Installed.");
            Ok(0)
        }
        Some((DownloadState::Canceled, _)) => {
            println!("Stopped. Partial files kept.");
            Ok(130)
        }
        Some((_, err)) => {
            println!("Failed: {}", err.unwrap_or_default());
            Ok(1)
        }
        None => Ok(1),
    }
}

async fn uninstall(backend: &Backend, args: &[String]) -> Result<i32> {
    let id = parse_id(args.first())?;
    let keep = args.iter().any(|a| a == "--keep-files");
    let installed = backend.game(&id).is_some_and(|g| g.is_installed);
    let remove = RemoveArgs {
        delete_files: !keep,
        ..Default::default()
    };
    if installed {
        backend.uninstall(&id, remove).await?;
    } else {
        // A stopped install: discard its partial download.
        let store = backend.store(id.runner).context("store not available")?;
        store.uninstall(&id.app_name, remove).await?;
    }
    println!("Done.");
    Ok(0)
}

/// The game language matching the interface language (`de` → `de-DE`),
/// else English, else the store's first.
fn preferred_language(languages: &[String], ui: &str) -> Option<String> {
    let base = |s: &str| s.split(['-', '_']).next().unwrap_or(s).to_ascii_lowercase();
    let want = base(ui);
    languages
        .iter()
        .find(|l| base(l) == want)
        .or_else(|| languages.iter().find(|l| l.eq_ignore_ascii_case("en-US")))
        .or_else(|| languages.iter().find(|l| base(l) == "en"))
        .or_else(|| languages.first())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::preferred_language;

    #[test]
    fn picks_ui_language_then_english() {
        let langs: Vec<String> = ["ja-JP", "de-DE", "en-US"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(preferred_language(&langs, "de").as_deref(), Some("de-DE"));
        assert_eq!(preferred_language(&langs, "fr").as_deref(), Some("en-US"));
        assert_eq!(preferred_language(&[], "fr"), None);
    }
}
