//! Galaxy common redistributables (`gog-redist`) and the one-time setup of a
//! Windows game inside its prefix (support scripts, ScriptInterpreter,
//! redistributable installers, the GalaxyCommunication dummy for comet).
//!
//! The services layer is store-agnostic, so this runs from the store's own
//! `install`/`update` (downloads) and `launch` (prefix setup).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use ember_core::process::split_args;
use ember_core::{OpContext, Outcome};

use ember_gog_api::content::{Client, Repository};
use ember_gog_api::plan::{self as gplan, BuildFiles};

use crate::build;
use crate::installed::InstalledRecord;
use crate::manifest::{self, language_name};
use crate::wine::{WineContext, to_windows_path};
use crate::{GogStore, api, load_json, save_json};

/// Script interpreter id, always required.
pub const ISI: &str = "ISI";

/// A redistributable of GOG's dependency repository.
pub type RedistDepot = ember_gog_api::content::DependencyDepot;

/// What is downloaded of the redistributables (`.ember-redist.json`).
/// Earlier versions' gogdl file (`.gogdl-redist-manifest`) reads as one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RedistRecord {
    #[serde(deserialize_with = "de_id")]
    pub build_id: String,
    pub depots: Vec<RedistDepot>,
    #[serde(alias = "HGLInstalled")]
    pub installed: Vec<String>,
}

fn de_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(api::id_string(&Value::deserialize(d)?))
}

impl RedistRecord {
    pub fn depot(&self, id: &str) -> Option<&RedistDepot> {
        self.depots.iter().find(|d| d.dependency_id == id)
    }
}

/// `ISI` plus every dependency of the build records in `builds_dir`.
pub fn required_redist_list(builds_dir: &Path) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    out.insert(ISI.into());
    for r in build::read_all(builds_dir) {
        out.extend(r.dependencies());
    }
    out.into_iter().collect()
}

/// Ids whose installers are not downloaded yet (only Galaxy-hosted ones count).
pub fn missing_redist(record: &RedistRecord, wanted: &[String]) -> Vec<String> {
    wanted
        .iter()
        .filter(|id| {
            record.depot(id).is_some_and(RedistDepot::is_global) && !record.installed.contains(id)
        })
        .cloned()
        .collect()
}

/// Command line of a redistributable installer.
pub fn redist_command(redist_dir: &Path, depot: &RedistDepot) -> Option<(String, Vec<String>)> {
    if depot.executable.path.is_empty() {
        return None;
    }
    let exe = redist_dir
        .join(&depot.executable.path)
        .to_string_lossy()
        .into_owned();
    let args = split_args(&depot.executable.arguments);
    if depot.dependency_id == "PHYSXLEGACY" {
        let mut a = vec!["/i".to_string(), exe];
        a.extend(args);
        a.push("/qb".into());
        Some(("msiexec".into(), a))
    } else {
        Some((exe, args))
    }
}

/// One Windows command of a game's setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCommand {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

/// Inputs of [`setup_commands`].
pub struct SetupInputs<'a> {
    pub manifest: &'a Value,
    pub rec: &'a InstalledRecord,
    pub support_dir: &'a Path,
    pub redist_dir: &'a Path,
}

/// The setup executables of a game, from its build meta (v1 support
/// commands, v2 ScriptInterpreter or per-product temp executables).
pub fn setup_commands(i: &SetupInputs<'_>) -> Vec<SetupCommand> {
    let m = i.manifest;
    let rec = i.rec;
    let deps = manifest::manifest_dependencies(m);
    let scummvm = deps.iter().any(|d| d.eq_ignore_ascii_case("scummvm"));
    let win = |p: &Path| {
        if scummvm {
            p.to_string_lossy().into_owned()
        } else {
            to_windows_path(p)
        }
    };
    let game_dir = win(&rec.path());
    let support_win = win(i.support_dir);
    let install_lang = rec
        .language
        .clone()
        .filter(|l| !l.is_empty() && l != "undefined");
    let lang_name = install_lang.as_deref().and_then(language_name);
    let lang_code = install_lang.clone().unwrap_or_else(|| "en-US".into());
    let build_id = rec.build_id().unwrap_or_default().to_string();
    let relevant = |pid: &str| pid == rec.app_name || rec.installed_dlcs.iter().any(|d| d == pid);
    let tail = [
        "/nodesktopshorctut".to_string(),
        "/nodesktopshortcut".to_string(),
    ];
    let mut out = Vec::new();

    if m.get("version").and_then(Value::as_u64) == Some(1) {
        if !i.support_dir.is_dir() {
            return out;
        }
        let l = lang_name
            .map(str::to_string)
            .or(install_lang.clone())
            .unwrap_or_else(|| "English".into())
            .to_lowercase();
        let wanted = install_lang.clone().unwrap_or_else(|| "English".into());
        for cmd in m
            .pointer("/product/support_commands")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let langs: Vec<&str> = cmd
                .get("languages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if !langs.contains(&wanted.as_str()) && !langs.contains(&"Neutral") {
                continue;
            }
            let game_id = cmd.get("gameID").map(api::id_string).unwrap_or_default();
            let Some(exe) = cmd.get("executable").and_then(Value::as_str) else {
                continue;
            };
            let mut args = vec![
                "/VERYSILENT".into(),
                format!("/DIR={game_dir}"),
                format!("/Language={l}"),
                format!("/LANG={l}"),
                format!(
                    "/ProductId={}",
                    if game_id.is_empty() {
                        &rec.app_name
                    } else {
                        &game_id
                    }
                ),
                "/galaxyclient".into(),
                format!(
                    "/buildId={}",
                    m.pointer("/product/timestamp")
                        .map(api::id_string)
                        .unwrap_or_default()
                ),
                format!("/versionName={}", rec.version),
            ];
            args.extend(tail.iter().cloned());
            out.push(SetupCommand {
                exe: i.support_dir.join(&game_id).join(exe),
                args,
            });
        }
        return out;
    }

    let products = m
        .get("products")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if m.get("scriptInterpreter")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let isi = i
            .redist_dir
            .join("__redist")
            .join("ISI")
            .join("scriptinterpreter.exe");
        let l = lang_name.unwrap_or("English");
        for p in &products {
            let pid = p.get("productId").map(api::id_string).unwrap_or_default();
            if !relevant(&pid) {
                continue;
            }
            let mut args = vec![
                "/VERYSILENT".into(),
                format!("/DIR={game_dir}"),
                format!("/Language={l}"),
                format!("/LANG={l}"),
                format!("/ProductId={pid}"),
                "/galaxyclient".into(),
                format!("/buildId={build_id}"),
                format!("/versionName={}", rec.version),
                format!("/lang-code={lang_code}"),
                format!("/supportDir={support_win}"),
            ];
            args.extend(tail.iter().cloned());
            out.push(SetupCommand {
                exe: isi.clone(),
                args,
            });
        }
    } else {
        let l = lang_name.unwrap_or("English").to_lowercase();
        for p in &products {
            let pid = p.get("productId").map(api::id_string).unwrap_or_default();
            let exe = p
                .get("temp_executable")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !relevant(&pid) || exe.is_empty() {
                continue;
            }
            let mut args = vec![
                "/VERYSILENT".into(),
                format!("/DIR={game_dir}"),
                format!("/Language={l}"),
                format!("/LANG={l}"),
                format!("/lang-code={lang_code}"),
                format!("/ProductId={pid}"),
                "/galaxyclient".into(),
                format!("/buildId={build_id}"),
                format!("/versionName={}", rec.version),
            ];
            args.extend(tail.iter().cloned());
            out.push(SetupCommand {
                exe: i.support_dir.join(&pid).join(exe),
                args,
            });
        }
    }
    out
}

/// What has been set up in one prefix.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PrefixSetup {
    /// `app → "<build>|<install path>"` of the last completed game setup.
    pub games: BTreeMap<String, String>,
    /// Redistributables installed into the prefix.
    pub redist: BTreeSet<String>,
    /// GalaxyCommunication service registered.
    pub comet: bool,
}

/// Key that changes whenever a game's setup must run again.
pub fn setup_key(rec: &InstalledRecord) -> String {
    format!(
        "{}|{}",
        rec.build_id().unwrap_or(&rec.version),
        rec.install_path
    )
}

type PrefixSetupFile = HashMap<String, PrefixSetup>;

impl GogStore {
    async fn redist_record(&self) -> Option<RedistRecord> {
        let path = self.paths.redist_record();
        tokio::task::spawn_blocking(move || serde_json::from_slice(&std::fs::read(path).ok()?).ok())
            .await
            .ok()
            .flatten()
    }

    async fn required_redist(&self) -> Vec<String> {
        let dir = self.paths.builds_dir();
        tokio::task::spawn_blocking(move || required_redist_list(&dir))
            .await
            .unwrap_or_else(|_| vec![ISI.into()])
    }

    /// Whether the downloaded redistributables are missing or outdated.
    pub(crate) async fn redist_needs_update(&self) -> bool {
        let Some(m) = self.redist_record().await else {
            return true;
        };
        let required: Vec<String> = self
            .required_redist()
            .await
            .into_iter()
            .filter(|r| m.depot(r).is_some_and(RedistDepot::is_global))
            .collect();
        if !missing_redist(&m, &required).is_empty() {
            return true;
        }
        match self.content.repository_build().await {
            Ok(b) => !b.is_empty() && b != m.build_id,
            Err(e) => {
                tracing::warn!("checking GOG redistributables: {e:#}");
                false
            }
        }
    }

    /// Download (or update) the redistributables needed by every installed game plus `extra`.
    pub(crate) async fn update_redist(&self, extra: &[String], ctx: &OpContext) -> Result<Outcome> {
        let mut ids: BTreeSet<String> = self.required_redist().await.into_iter().collect();
        ids.extend(extra.iter().filter(|s| !s.is_empty()).cloned());
        let client = self.content().await?;
        (ctx.step)("Downloading GOG redistributables");
        match self.download_redist(&client, ids, ctx).await {
            Ok(outcome) => Ok(outcome),
            Err(_) if ctx.cancel.is_cancelled() => Ok(Outcome::Abort),
            Err(e) => Ok(Outcome::Error(format!("{e:#}"))),
        }
    }

    async fn download_redist(
        &self,
        client: &Client,
        ids: BTreeSet<String>,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        let repo = client.repository().await?;
        let wanted: Vec<String> = ids
            .into_iter()
            .filter(|id| {
                repo.depots
                    .iter()
                    .any(|d| &d.dependency_id == id && d.is_global())
            })
            .collect();
        let new = BuildFiles {
            generation: 2,
            items: gplan::resolve_dependencies(client, &repo, &wanted, true).await?,
            ..Default::default()
        };
        let old = match self.redist_record().await {
            Some(o) => {
                let old_repo = Repository {
                    build_id: o.build_id.clone(),
                    depots: o.depots.clone(),
                };
                gplan::resolve_dependencies(client, &old_repo, &o.installed, true)
                    .await
                    .ok()
                    .map(|items| BuildFiles {
                        generation: 2,
                        items,
                        ..Default::default()
                    })
            }
            None => None,
        };
        let root = self.paths.redist.clone();
        let plan = {
            let exists = |_: bool, rel: &str| ember_depot::path::join(&root, rel).is_file();
            gplan::plan(&new, old.as_ref(), &exists)?
        };
        let settings = (self.settings)();
        let finished = ember_depot::download(
            &Client::download_client()?,
            ember_depot::Job {
                root: &root,
                files: &plan.game,
                remove: &plan.remove_game,
            },
            &ember_depot::Options {
                workers: settings.max_workers as usize,
                max_bytes_per_sec: u64::from(settings.max_download_speed) * 1024,
                journal: Some(self.paths.resume_file(crate::library::REDIST_APP)),
                signer: Some(ember_depot::Signer(Arc::new(gplan::Signer::new(
                    client.clone(),
                )))),
                ..Default::default()
            },
            crate::install::progress_sink(ctx),
            &ctx.cancel,
        )
        .await?;
        if finished == ember_depot::Finished::Cancelled {
            return Ok(Outcome::Abort);
        }
        let record = RedistRecord {
            build_id: repo.build_id,
            depots: repo.depots,
            installed: wanted,
        };
        save_json(self.paths.redist_record(), record).await?;
        Ok(Outcome::Done)
    }

    /// Make sure `deps` (and the script interpreter) are downloaded.
    pub(crate) async fn ensure_redist(&self, deps: &[String], ctx: &OpContext) -> Result<()> {
        let mut wanted: Vec<String> = vec![ISI.into()];
        wanted.extend(deps.iter().cloned());
        let needed = match self.redist_record().await {
            None => true,
            Some(m) => !missing_redist(&m, &wanted).is_empty(),
        };
        if !needed {
            return Ok(());
        }
        match self.update_redist(deps, ctx).await? {
            Outcome::Done | Outcome::Abort => Ok(()),
            Outcome::Error(e) => anyhow::bail!("Could not download the GOG redistributables: {e}"),
        }
    }

    async fn load_prefix_setup(&self, prefix: &Path) -> PrefixSetup {
        let all: PrefixSetupFile = load_json(self.paths.prefix_setup_file()).await;
        all.get(&prefix.to_string_lossy().into_owned())
            .cloned()
            .unwrap_or_default()
    }

    async fn store_prefix_setup(&self, prefix: &Path, setup: PrefixSetup) {
        let path = self.paths.prefix_setup_file();
        let mut all: PrefixSetupFile = load_json(path.clone()).await;
        all.insert(prefix.to_string_lossy().into_owned(), setup);
        if let Err(e) = save_json(path, all).await {
            tracing::warn!("{e:#}");
        }
    }

    /// Prepare a Windows game's prefix before launch: download missing
    /// redistributables, run the game's setup once per build and location,
    /// install redistributables not yet in this prefix and register the
    /// GalaxyCommunication dummy service for comet.
    pub(crate) async fn prepare_prefix(
        &self,
        rec: &InstalledRecord,
        wine: Option<&WineContext>,
        ctx: &OpContext,
        log_file: &Path,
    ) -> Result<()> {
        if !self.paths.build_file(&rec.app_name).exists()
            && let Err(e) = self.restore_build_record(&rec.app_name).await
        {
            tracing::warn!("recording the installed build of {}: {e:#}", rec.app_name);
        }
        let manifest = self.build_record(&rec.app_name).await.map(|b| b.meta);
        let deps = manifest
            .as_ref()
            .map(manifest::manifest_dependencies)
            .unwrap_or_default();
        if let Err(e) = self.ensure_redist(&deps, ctx).await {
            tracing::warn!("{e:#}");
        }

        let setup_path = wine.map(|w| w.prefix.clone()).unwrap_or_else(|| rec.path());
        let mut setup = self.load_prefix_setup(&setup_path).await;
        let key = setup_key(rec);
        let mut changed = false;

        if setup.games.get(&rec.app_name) != Some(&key)
            && let Some(m) = &manifest
        {
            (ctx.step)("Setting up the game");
            let support = self.paths.support_dir(&rec.app_name);
            let cmds = setup_commands(&SetupInputs {
                manifest: m,
                rec,
                support_dir: &support,
                redist_dir: &self.paths.redist,
            });
            let mut complete = true;
            for c in cmds {
                if !c.exe.is_file() {
                    tracing::warn!("setup executable {} is missing", c.exe.display());
                    complete = false;
                    continue;
                }
                let res = crate::wine::run_windows(
                    wine,
                    &c.exe.to_string_lossy(),
                    &c.args,
                    Some(&rec.path()),
                    Some(ctx.cancel.clone()),
                    Some(log_file),
                )
                .await;
                match res {
                    Ok(r) if r.aborted => anyhow::bail!("aborted"),
                    Ok(r) if r.success() || matches!(r.code, Some(3010 | 1641)) => {}
                    Ok(r) => {
                        complete = false;
                        tracing::warn!("setup of {}: {}", rec.app_name, r.error_summary());
                    }
                    Err(e) => {
                        complete = false;
                        tracing::warn!("setup of {}: {e:#}", rec.app_name);
                    }
                }
            }
            if complete {
                setup.games.insert(rec.app_name.clone(), key);
                changed = true;
            }
        }

        let pending: Vec<String> = deps
            .iter()
            .filter(|d| !setup.redist.contains(*d))
            .cloned()
            .collect();
        if !pending.is_empty()
            && let Some(rm) = self.redist_record().await
        {
            for dep in pending {
                let Some(depot) = rm.depot(&dep) else {
                    tracing::warn!("redistributable {dep} is not in the GOG repository");
                    setup.redist.insert(dep);
                    changed = true;
                    continue;
                };
                let Some((exe, args)) = redist_command(&self.paths.redist, depot) else {
                    setup.redist.insert(dep);
                    continue;
                };
                (ctx.step)(&format!(
                    "Installing {}",
                    if depot.readable_name.is_empty() {
                        &dep
                    } else {
                        &depot.readable_name
                    }
                ));
                match crate::wine::run_windows(
                    wine,
                    &exe,
                    &args,
                    Some(&self.paths.redist),
                    Some(ctx.cancel.clone()),
                    Some(log_file),
                )
                .await
                {
                    Ok(r) if r.aborted => anyhow::bail!("aborted"),
                    Ok(r) if r.success() || matches!(r.code, Some(3010 | 1641)) => {
                        setup.redist.insert(dep);
                    }
                    Ok(r) => tracing::warn!("installing {dep}: {}", r.error_summary()),
                    Err(e) => tracing::warn!("installing {dep}: {e:#}"),
                }
                changed = true;
            }
        }

        if (self.settings)().comet_support
            && !setup.comet
            && let Some(wine) = wine
        {
            match self.install_galaxy_communication(wine).await {
                Ok(()) => {
                    setup.comet = true;
                    changed = true;
                }
                Err(e) => tracing::warn!("GalaxyCommunication dummy: {e:#}"),
            }
        }

        if changed {
            self.store_prefix_setup(&setup_path, setup).await;
        }
        Ok(())
    }

    /// Copy `GalaxyCommunication.exe` into the prefix and register it as a
    /// service so games' Galaxy SDK reaches comet.
    async fn install_galaxy_communication(&self, wine: &WineContext) -> Result<()> {
        const WIN_PATH: &str = "C:\\ProgramData\\GOG.com\\Galaxy\\redists\\GalaxyCommunication.exe";
        let dest = wine.to_unix(WIN_PATH);
        if !dest.is_file() {
            let src = galaxy_communication_exe()
                .context("GalaxyCommunication.exe was not found (it ships with Heroic)")?;
            if let Some(dir) = dest.parent() {
                tokio::fs::create_dir_all(dir).await?;
            }
            tokio::fs::copy(&src, &dest)
                .await
                .with_context(|| format!("copying {}", src.display()))?;
            wine.run(
                "sc",
                &[
                    "create".into(),
                    "GalaxyCommunication".into(),
                    format!("binpath={WIN_PATH}"),
                ],
                None,
                None,
                None,
            )
            .await?;
        }
        wine.run(
            "reg",
            &[
                "add".into(),
                "HKLM\\SOFTWARE\\WOW6432Node\\GOG.com\\GalaxyClient\\paths".into(),
                "/v".into(),
                "client".into(),
                "/t".into(),
                "REG_SZ".into(),
                "/d".into(),
                "C:\\Program Files\\GOG Galaxy".into(),
                "/f".into(),
            ],
            None,
            None,
            None,
        )
        .await?;
        Ok(())
    }
}

/// Locate the GalaxyCommunication dummy service binary (Ember's bin dir or a
/// Heroic installation).
fn galaxy_communication_exe() -> Option<PathBuf> {
    const NAME: &str = "GalaxyCommunication.exe";
    const SUFFIX: &str = "resources/app.asar.unpacked/build/bin/x64/win32";
    let own = ember_core::Paths::get().bin_dir().join(NAME);
    if own.is_file() {
        return Some(own);
    }
    let mut roots: Vec<PathBuf> = [
        "/usr/lib64/heroic",
        "/usr/lib/heroic",
        "/opt/Heroic",
        "/opt/heroic",
        "/usr/share/heroic",
        "/var/lib/flatpak/app/com.heroicgameslauncher.hgl/current/active/files/bin/heroic",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(
            ".local/share/flatpak/app/com.heroicgameslauncher.hgl/current/active/files/bin/heroic",
        ));
    }
    roots
        .into_iter()
        .map(|r| r.join(SUFFIX).join(NAME))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> InstalledRecord {
        InstalledRecord {
            app_name: "1423049311".into(),
            install_path: "/g/CP".into(),
            language: Some("en-US".into()),
            version: "2.5".into(),
            build_id: Some("600".into()),
            installed_dlcs: vec!["1256837418".into()],
            ..Default::default()
        }
    }

    #[test]
    fn script_interpreter_setup() {
        let m = serde_json::json!({"version": 2, "scriptInterpreter": true,
            "dependencies": ["MSVC2015"],
            "products": [{"productId": "1423049311"}, {"productId": "1256837418"}, {"productId": "999"}]});
        let cmds = setup_commands(&SetupInputs {
            manifest: &m,
            rec: &rec(),
            support_dir: Path::new("/s/1423049311"),
            redist_dir: Path::new("/r"),
        });
        assert_eq!(cmds.len(), 2);
        assert_eq!(
            cmds[0].exe,
            PathBuf::from("/r/__redist/ISI/scriptinterpreter.exe")
        );
        assert!(
            cmds[0].args.contains(
                &(if cfg!(windows) {
                    r"/DIR=\g\CP"
                } else {
                    r"/DIR=Z:\g\CP"
                })
                .to_string()
            )
        );
        assert!(cmds[0].args.contains(&"/Language=English".to_string()));
        assert!(cmds[0].args.contains(&"/ProductId=1423049311".to_string()));
        assert!(cmds[0].args.contains(&"/buildId=600".to_string()));
        assert!(
            cmds[0].args.contains(
                &(if cfg!(windows) {
                    r"/supportDir=\s\1423049311"
                } else {
                    r"/supportDir=Z:\s\1423049311"
                })
                .to_string()
            )
        );
        assert!(cmds[1].args.contains(&"/ProductId=1256837418".to_string()));
    }

    #[test]
    fn temp_executable_and_v1_setup() {
        let m = serde_json::json!({"version": 2, "products": [
            {"productId": "1423049311", "temp_executable": "setup.exe"},
            {"productId": "1256837418", "temp_executable": ""}]});
        let cmds = setup_commands(&SetupInputs {
            manifest: &m,
            rec: &rec(),
            support_dir: Path::new("/s"),
            redist_dir: Path::new("/r"),
        });
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].exe, PathBuf::from("/s/1423049311/setup.exe"));
        assert!(cmds[0].args.contains(&"/Language=english".to_string()));

        let d = tempfile::tempdir().unwrap();
        let v1 = serde_json::json!({"version": 1, "product": {"timestamp": 123, "support_commands": [
            {"languages": ["Neutral"], "gameID": "1423049311", "executable": "/support.exe"},
            {"languages": ["German"], "gameID": "1", "executable": "x.exe"}]}});
        let cmds = setup_commands(&SetupInputs {
            manifest: &v1,
            rec: &rec(),
            support_dir: d.path(),
            redist_dir: Path::new("/r"),
        });
        assert_eq!(cmds.len(), 1);
        assert!(cmds[0].args.contains(&"/ProductId=1423049311".to_string()));
        assert!(cmds[0].args.contains(&"/buildId=123".to_string()));
    }

    #[test]
    fn redist_manifest_logic() {
        let m: RedistRecord = serde_json::from_value(serde_json::json!({
            "build_id": 5,
            "depots": [
                {"dependencyId": "ISI", "executable": {"path": "__redist/ISI/scriptinterpreter.exe", "arguments": ""}, "readableName": "ISI"},
                {"dependencyId": "MSVC2015", "executable": {"path": "__redist/MSVC2015/vc_redist.x86.exe", "arguments": "/install /quiet"}, "readableName": "VC 2015"},
                {"dependencyId": "PHYSXLEGACY", "executable": {"path": "__redist/PhysX/p.msi", "arguments": "/x"}},
                {"dependencyId": "DOSBOX", "executable": {"path": "", "arguments": ""}}],
            "HGLInstalled": ["ISI"]})).unwrap();
        assert_eq!(m.build_id, "5");
        assert_eq!(
            missing_redist(&m, &["ISI".into(), "MSVC2015".into(), "DOSBOX".into()]),
            vec!["MSVC2015"]
        );
        let (exe, args) = redist_command(Path::new("/r"), m.depot("MSVC2015").unwrap()).unwrap();
        assert_eq!(exe, "/r/__redist/MSVC2015/vc_redist.x86.exe");
        assert_eq!(args, vec!["/install", "/quiet"]);
        let (exe, args) = redist_command(Path::new("/r"), m.depot("PHYSXLEGACY").unwrap()).unwrap();
        assert_eq!(exe, "msiexec");
        assert_eq!(args, vec!["/i", "/r/__redist/PhysX/p.msi", "/x", "/qb"]);
        assert!(redist_command(Path::new("/r"), m.depot("DOSBOX").unwrap()).is_none());
    }

    #[test]
    fn required_list_from_build_records() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("1.json"),
            r#"{"meta":{"version":2,"dependencies":["MSVC2015"]}}"#,
        )
        .unwrap();
        std::fs::write(
            d.path().join("2.json"),
            r#"{"meta":{"version":1,"product":{"depots":[{"redist":"DX9"}]}}}"#,
        )
        .unwrap();
        assert_eq!(
            required_redist_list(d.path()),
            vec!["DX9", "ISI", "MSVC2015"]
        );
        assert_eq!(required_redist_list(&d.path().join("none")), vec!["ISI"]);
        assert_eq!(setup_key(&rec()), "600|/g/CP");
    }
}
