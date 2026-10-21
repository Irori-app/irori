//! GOG cloud saves: locations from remote-config, GOG path variables resolved
//! inside the Wine prefix, and syncing with GOG's cloud storage.
//!
//! A game can have several save locations, but the services layer passes one
//! `save_path` string. Several locations are therefore encoded one per line as
//! `name=path`; a single location is a bare path.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;

use ember_core::{CloudSaveLocation, InstallPlatform, OpContext, SyncDirection, util};
use ember_gog_api::saves::{Action, Preference, Storage};

use crate::installed::InstalledRecord;
use crate::{GogStore, api, load_json, save_json};
use ember_core::Store as _;

/// Cloud folder name used when GOG lists no location (as Galaxy does).
pub const DEFAULT_LOCATION: &str = "__default";

/// `drive_c` and the user profile directory of a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixDirs {
    pub drive_c: PathBuf,
    pub user_dir: PathBuf,
}

impl PrefixDirs {
    pub fn of(prefix: &Path) -> Self {
        let pfx = prefix.join("pfx").join("drive_c");
        let drive_c = if pfx.is_dir() {
            pfx
        } else {
            prefix.join("drive_c")
        };
        let users = drive_c.join("users");
        let steam = users.join("steamuser");
        let user_dir = if steam.is_dir() {
            steam
        } else {
            let own = users.join(std::env::var("USER").unwrap_or_else(|_| "user".into()));
            if own.is_dir() {
                own
            } else {
                std::fs::read_dir(&users)
                    .ok()
                    .and_then(|it| {
                        it.flatten()
                            .map(|e| e.path())
                            .find(|p| p.is_dir() && !p.ends_with("Public"))
                    })
                    .unwrap_or(steam)
            }
        };
        Self { drive_c, user_dir }
    }
}

/// Default location template when GOG lists none.
pub fn default_location(client_id: &str) -> String {
    format!("%LocalAppData%/GOG.com/Galaxy/Applications/{client_id}/Storage/Shared/Files")
}

fn normalize(p: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Expand GOG's `<?VAR?>` variables and Windows `%VARS%` to a host path.
pub fn resolve_location(location: &str, install_path: &Path, dirs: &PrefixDirs) -> PathBuf {
    static GOG_VAR: Lazy<Regex> = Lazy::new(|| Regex::new(r"<\?(\w+)\?>").expect("valid regex"));
    static WIN_VAR: Lazy<Regex> = Lazy::new(|| Regex::new(r"%(\w+)%").expect("valid regex"));
    let install = install_path.to_string_lossy();
    let with_gog = GOG_VAR.replace_all(location, |c: &regex::Captures| match &c[1] {
        "INSTALL" => install.to_string(),
        "SAVED_GAMES" => "%USERPROFILE%/Saved Games".into(),
        "APPLICATION_DATA_LOCAL" => "%LOCALAPPDATA%".into(),
        "APPLICATION_DATA_LOCAL_LOW" => "%USERPROFILE%/AppData/LocalLow".into(),
        "APPLICATION_DATA_ROAMING" => "%APPDATA%".into(),
        "DOCUMENTS" => "%USERPROFILE%/Documents".into(),
        "APPLICATION_SUPPORT" => "%USERPROFILE%/Library/Application Support".into(),
        other => {
            tracing::warn!("unknown GOG save path variable {other}");
            other.to_string()
        }
    });
    let user = dirs.user_dir.to_string_lossy();
    let expanded = WIN_VAR.replace_all(&with_gog, |c: &regex::Captures| {
        match c[1].to_ascii_uppercase().as_str() {
            "USERPROFILE" => user.to_string(),
            "LOCALAPPDATA" => format!("{user}/AppData/Local"),
            "APPDATA" => format!("{user}/AppData/Roaming"),
            "PROGRAMDATA" | "ALLUSERSPROFILE" => {
                format!("{}/ProgramData", dirs.drive_c.to_string_lossy())
            }
            "PUBLIC" => format!("{}/users/Public", dirs.drive_c.to_string_lossy()),
            "WINDIR" | "SYSTEMROOT" => format!("{}/windows", dirs.drive_c.to_string_lossy()),
            _ => c[0].to_string(),
        }
    });
    let unixish = expanded.replace('\\', "/");
    let path = match unixish.split_once(':') {
        Some((d, rest)) if d.len() == 1 && d.eq_ignore_ascii_case("c") => {
            dirs.drive_c.join(rest.trim_start_matches('/'))
        }
        Some((d, rest)) if d.len() == 1 && d.eq_ignore_ascii_case("z") => {
            Path::new("/").join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(unixish),
    };
    normalize(path)
}

/// Encode resolved locations into the single `save_path` string.
pub fn encode_locations(locs: &[CloudSaveLocation]) -> String {
    match locs {
        [one] => one.location.clone(),
        many => many
            .iter()
            .map(|l| format!("{}={}", l.name, l.location))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Decode `save_path` against the game's location names. Bare paths take the
/// name of the location at their position.
pub fn decode_locations(save_path: &str, names: &[String]) -> Vec<CloudSaveLocation> {
    let mut out: Vec<CloudSaveLocation> = Vec::new();
    for (i, line) in save_path
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .enumerate()
    {
        let named = line
            .split_once('=')
            .filter(|(n, _)| names.iter().any(|k| k == n) || *n == DEFAULT_LOCATION);
        let (name, location) = match named {
            Some((n, p)) => (n.to_string(), p.trim().to_string()),
            None => (
                names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_LOCATION.to_string()),
                line.to_string(),
            ),
        };
        if !out.iter().any(|l| l.name == name) {
            out.push(CloudSaveLocation { name, location });
        }
    }
    out
}

/// How a sync direction maps onto the cloud-save sync.
pub fn preference(d: SyncDirection) -> Preference {
    match d {
        SyncDirection::Auto => Preference::Auto,
        SyncDirection::Download => Preference::Download,
        SyncDirection::Upload => Preference::Upload,
        SyncDirection::ForceDownload => Preference::ForceDownload,
        SyncDirection::ForceUpload => Preference::ForceUpload,
    }
}

/// Seconds since the epoch of a stored sync time (`"1729600000.123"`).
pub fn parse_timestamp(ts: &str) -> u64 {
    ts.trim().parse::<f64>().map_or(0, |t| t.max(0.0) as u64)
}

type Timestamps = HashMap<String, HashMap<String, String>>;

impl GogStore {
    /// Cloud-save locations of an installed game. `Ok(None)` when it has none.
    pub(crate) async fn save_locations_for(
        &self,
        rec: &InstalledRecord,
    ) -> Result<Option<Vec<CloudSaveLocation>>> {
        if rec.platform == InstallPlatform::Linux {
            return Ok(None);
        }
        let Some(client) = self.client_id(&rec.app_name).await else {
            return Ok(None);
        };
        api::cloud_save_locations(&client, rec.platform_str()).await
    }

    async fn locations(&self, rec: &InstalledRecord) -> Result<Vec<CloudSaveLocation>> {
        let title = self.title_of(&rec.app_name);
        if rec.platform == InstallPlatform::Linux {
            bail!("Cloud saves are not available for the Linux version of {title}");
        }
        let known = self
            .game(&rec.app_name)
            .filter(|g| g.cloud_save_enabled)
            .map(|g| g.gog_save_locations.clone());
        let locs = match known {
            Some(l) => l,
            None => {
                let l = self
                    .save_locations_for(rec)
                    .await?
                    .with_context(|| format!("{title} does not support cloud saves"))?;
                let stored = l.clone();
                self.update_catalogue_entry(&rec.app_name, move |g| {
                    g.cloud_save_enabled = true;
                    g.gog_save_locations = stored;
                })
                .await;
                l
            }
        };
        if locs.is_empty() {
            let client = self
                .client_id(&rec.app_name)
                .await
                .context("The game's Galaxy client id is unknown")?;
            return Ok(vec![CloudSaveLocation {
                name: DEFAULT_LOCATION.into(),
                location: default_location(&client),
            }]);
        }
        Ok(locs)
    }

    pub(crate) async fn resolve_default_save_path(
        &self,
        app_name: &str,
        prefix: Option<&Path>,
    ) -> Result<String> {
        let rec = self.installed_record(app_name)?;
        let locs = self.locations(&rec).await?;
        let prefix = match prefix {
            Some(p) => p.to_path_buf(),
            None => self
                .wine_context(app_name)
                .await
                .map(|w| w.prefix)
                .context("Launch the game once so its Wine prefix exists")?,
        };
        let install = rec.path();
        let resolved = tokio::task::spawn_blocking(move || {
            let dirs = PrefixDirs::of(&prefix);
            locs.into_iter()
                .map(|l| CloudSaveLocation {
                    location: resolve_location(&l.location, &install, &dirs)
                        .to_string_lossy()
                        .into_owned(),
                    name: l.name,
                })
                .collect::<Vec<_>>()
        })
        .await?;
        Ok(encode_locations(&resolved))
    }

    pub(crate) async fn sync_game_saves(
        &self,
        app_name: &str,
        direction: SyncDirection,
        save_path: &str,
        ctx: OpContext,
    ) -> Result<String> {
        let rec = self.installed_record(app_name)?;
        let names: Vec<String> = self
            .locations(&rec)
            .await?
            .into_iter()
            .map(|l| l.name)
            .collect();
        let resolved;
        let save_path = if save_path.trim().is_empty() {
            resolved = self.resolve_default_save_path(app_name, None).await?;
            resolved.as_str()
        } else {
            save_path
        };
        let targets = decode_locations(save_path, &names);
        if targets.is_empty() {
            bail!("No save folder is set for {}", self.title_of(app_name));
        }
        let galaxy = self.auth.credentials().await?;
        let (client_id, secret) = self.game_client(&rec).await?;
        let creds = self
            .auth
            .client_credentials(&client_id, &secret)
            .await
            .context("Could not sign in to the game's cloud storage")?;
        let ts_path = self.paths.save_timestamps_file();
        let mut report = Vec::new();
        let mut failures = 0;
        for loc in &targets {
            let _ = tokio::fs::create_dir_all(&loc.location).await;
            let last = {
                let all: Timestamps = load_json(ts_path.clone()).await;
                all.get(app_name)
                    .and_then(|m| m.get(&loc.name))
                    .map_or(0, |t| parse_timestamp(t))
            };
            (ctx.step)(&format!("Syncing {}", loc.name));
            let storage = Storage::new(
                &self.content.endpoints.cloud_storage,
                &creds.access_token,
                &galaxy.user_id,
                &client_id,
                &loc.name,
            )?;
            let started = util::unix_now();
            let result = tokio::select! {
                r = storage.sync(Path::new(&loc.location), last, preference(direction)) => r,
                _ = ctx.cancel.cancelled() => bail!("aborted"),
            };
            match result {
                Ok(r) if r.action == Action::Conflict => {
                    failures += 1;
                    report.push(format!(
                        "{}: local and cloud saves both changed; choose force upload or force download",
                        loc.name
                    ));
                }
                Ok(r) => {
                    let mut all: Timestamps = load_json(ts_path.clone()).await;
                    all.entry(app_name.to_string())
                        .or_default()
                        .insert(loc.name.clone(), started.to_string());
                    save_json(ts_path.clone(), all).await?;
                    report.push(match r.action {
                        Action::Upload => format!("{}: uploaded {} files", loc.name, r.uploaded),
                        Action::Download => {
                            format!("{}: downloaded {} files", loc.name, r.downloaded)
                        }
                        _ => format!("{}: up to date", loc.name),
                    });
                }
                Err(e) => {
                    failures += 1;
                    report.push(format!("{}: {e:#}", loc.name));
                }
            }
        }
        let text = report.join("\n");
        if failures == targets.len() {
            bail!("{text}");
        }
        Ok(text)
    }

    /// The game's own Galaxy client id and secret (from its build meta).
    async fn game_client(&self, rec: &InstalledRecord) -> Result<(String, String)> {
        if let Some(meta) = self
            .build_record(&rec.app_name)
            .await
            .and_then(|b| b.parsed_meta())
            .filter(|m| !m.client_id.is_empty() && !m.client_secret.is_empty())
        {
            return Ok((meta.client_id, meta.client_secret));
        }
        let client = self.content().await?;
        let password = self.private_branch_password(&rec.app_name).await;
        let builds = client
            .builds(&rec.app_name, rec.platform_str(), Some(&password))
            .await?;
        let build = builds
            .latest(rec.branch())
            .or_else(|| builds.items.first())
            .context("GOG lists no builds of the game")?;
        let (meta, _) = client.build_meta(build).await?;
        if meta.client_id.is_empty() || meta.client_secret.is_empty() {
            bail!(
                "{} does not support cloud saves",
                self.title_of(&rec.app_name)
            );
        }
        Ok((meta.client_id, meta.client_secret))
    }

    /// Forget sync timestamps of a game.
    pub(crate) async fn clear_save_timestamps(&self, app_name: &str) {
        let path = self.paths.save_timestamps_file();
        let mut all: Timestamps = load_json(path.clone()).await;
        if all.remove(app_name).is_some() {
            let _ = save_json(path, all).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> PrefixDirs {
        PrefixDirs {
            drive_c: "/p/pfx/drive_c".into(),
            user_dir: "/p/pfx/drive_c/users/steamuser".into(),
        }
    }

    #[test]
    fn resolves_gog_variables() {
        let d = dirs();
        let inst = Path::new("/games/CP");
        assert_eq!(
            resolve_location("<?SAVED_GAMES?>/CD Projekt Red/Cyberpunk 2077", inst, &d),
            PathBuf::from(
                "/p/pfx/drive_c/users/steamuser/Saved Games/CD Projekt Red/Cyberpunk 2077"
            )
        );
        assert_eq!(
            resolve_location("<?INSTALL?>/saves", inst, &d),
            PathBuf::from("/games/CP/saves")
        );
        assert_eq!(
            resolve_location("<?APPLICATION_DATA_LOCAL_LOW?>\\Studio\\Game", inst, &d),
            PathBuf::from("/p/pfx/drive_c/users/steamuser/AppData/LocalLow/Studio/Game")
        );
        assert_eq!(
            resolve_location(&default_location("123"), inst, &d),
            PathBuf::from(
                "/p/pfx/drive_c/users/steamuser/AppData/Local/GOG.com/Galaxy/Applications/123/Storage/Shared/Files"
            )
        );
        assert_eq!(
            resolve_location("C:\\ProgramData\\x\\..\\y", inst, &d),
            PathBuf::from("/p/pfx/drive_c/ProgramData/y")
        );
    }

    #[test]
    fn prefix_dirs_detect_layout() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("drive_c/users/Public")).unwrap();
        std::fs::create_dir_all(t.path().join("drive_c/users/alice")).unwrap();
        let d = PrefixDirs::of(t.path());
        assert_eq!(d.drive_c, t.path().join("drive_c"));
        assert!(
            d.user_dir.ends_with("alice")
                || d.user_dir
                    .ends_with(std::env::var("USER").unwrap_or_default())
        );
    }

    #[test]
    fn encodes_and_decodes_locations() {
        let one = vec![CloudSaveLocation {
            name: "saves".into(),
            location: "/a".into(),
        }];
        assert_eq!(encode_locations(&one), "/a");
        let names = vec!["saves".to_string(), "config".to_string()];
        assert_eq!(decode_locations("/a", &names)[0].name, "saves");
        let two = vec![
            one[0].clone(),
            CloudSaveLocation {
                name: "config".into(),
                location: "/b=c".into(),
            },
        ];
        let enc = encode_locations(&two);
        assert_eq!(enc, "saves=/a\nconfig=/b=c");
        assert_eq!(decode_locations(&enc, &names), two);
        assert_eq!(decode_locations("/x", &[])[0].name, DEFAULT_LOCATION);
        assert!(decode_locations("  \n", &names).is_empty());
    }

    #[test]
    fn sync_preferences_and_timestamps() {
        assert_eq!(preference(SyncDirection::Auto), Preference::Auto);
        assert_eq!(
            preference(SyncDirection::ForceUpload),
            Preference::ForceUpload
        );
        assert_eq!(parse_timestamp("1729600000.123"), 1_729_600_000);
        assert_eq!(parse_timestamp("nope"), 0);
    }
}
