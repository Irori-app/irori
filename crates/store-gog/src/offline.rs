//! GOG's offline files: what the store offers beside a game (offline
//! installers, patches, language packs and goodies such as manuals and
//! soundtracks), and installing a game from an offline installer on disk.
//!
//! Installers are unpacked rather than run: Linux `.sh` installers are a
//! shell script with a ZIP archive appended (`unzip` reads it), Windows
//! `setup_*.exe` installers are Inno Setup archives (`innoextract` reads
//! them). The unpacked folder is then imported like any existing install.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use irori_core::Store as _;
use irori_core::process::{self, CommandSpec, RunOptions};
use irori_core::{ExtraFile, ExtraGroup, InstallPlatform, InstallRequest, OpContext, Outcome};

use crate::GogStore;

/// The offline files of a product, from `api.gog.com/products/<id>` with
/// `expand=downloads`: every part of every installer, patch and language
/// pack, then the goodies.
pub fn map_extras(product: &Value) -> Vec<ExtraFile> {
    let downloads = product.get("downloads");
    let mut out = Vec::new();
    for (key, group) in [
        ("installers", ExtraGroup::Installer),
        ("patches", ExtraGroup::Patch),
        ("language_packs", ExtraGroup::LanguagePack),
    ] {
        for item in list(downloads.and_then(|d| d.get(key))) {
            let files = list(item.get("files"));
            let count = files.len() as u32;
            for (ix, file) in files.iter().enumerate() {
                let Some(downlink) = text(file.get("downlink")) else {
                    continue;
                };
                out.push(ExtraFile {
                    id: downlink,
                    name: text(item.get("name")).unwrap_or_default(),
                    group,
                    category: String::new(),
                    platform: item.get("os").and_then(Value::as_str).and_then(platform),
                    language: text(item.get("language_full")),
                    version: text(item.get("version")),
                    part: (count > 1).then_some((ix as u32 + 1, count)),
                    size: file.get("size").and_then(Value::as_u64).unwrap_or(0),
                });
            }
        }
    }
    for item in list(downloads.and_then(|d| d.get("bonus_content"))) {
        let files = list(item.get("files"));
        let count = files.len() as u32;
        for (ix, file) in files.iter().enumerate() {
            let Some(downlink) = text(file.get("downlink")) else {
                continue;
            };
            out.push(ExtraFile {
                id: downlink,
                name: text(item.get("name")).unwrap_or_default(),
                group: ExtraGroup::Goodie,
                category: text(item.get("type")).unwrap_or_default(),
                part: (count > 1).then_some((ix as u32 + 1, count)),
                size: file.get("size").and_then(Value::as_u64).unwrap_or(0),
                ..Default::default()
            });
        }
    }
    out
}

fn list(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn platform(os: &str) -> Option<InstallPlatform> {
    match os {
        "windows" => Some(InstallPlatform::Windows),
        "linux" => Some(InstallPlatform::Linux),
        "mac" | "osx" => Some(InstallPlatform::Mac),
        _ => None,
    }
}

/// The file name of a resolved download address: its last path segment,
/// URL-decoded.
pub fn file_name_of(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let last = path.rsplit('/').next().filter(|s| !s.is_empty())?;
    let name: String = url::form_urlencoded::parse(format!("n={last}").as_bytes())
        .find(|(k, _)| k == "n")
        .map(|(_, v)| v.into_owned())?;
    // Never a path: a hostile name cannot escape the download folder.
    (!name.contains('/') && name != "." && name != "..").then_some(name)
}

/// An offline installer found in a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundInstaller {
    pub path: PathBuf,
    pub platform: InstallPlatform,
    /// The owned game it installs, when it could be told.
    pub app_name: Option<String>,
    /// Reading it needs this program, which is not installed.
    pub missing_tool: Option<&'static str>,
}

/// Whether `program` is on the `PATH`.
fn have(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// The tool that unpacks installers of `platform`.
fn tool_for(platform: InstallPlatform) -> &'static str {
    match platform {
        InstallPlatform::Linux => "unzip",
        _ => "innoextract",
    }
}

/// The platform of an installer file, or `None` for files that are not one
/// (split-installer `.bin` parts, patches, anything else).
pub fn installer_platform(file_name: &str) -> Option<InstallPlatform> {
    let lower = file_name.to_lowercase();
    if lower.starts_with("patch_") {
        return None;
    }
    if lower.ends_with(".sh") {
        Some(InstallPlatform::Linux)
    } else if lower.starts_with("setup_") && lower.ends_with(".exe") {
        Some(InstallPlatform::Windows)
    } else {
        None
    }
}

/// A title as GOG writes it in installer file names: lowercase words joined
/// by `_`, apostrophes dropped.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in title.chars().filter(|c| !matches!(c, '\'' | '’')) {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            out.extend(c.to_lowercase());
            gap = false;
        } else {
            gap = true;
        }
    }
    out
}

/// The game an installer file name belongs to: the one whose title slug
/// starts the name (after `setup_`), the longest such title winning
/// (`the_witcher_3_wild_hunt_game_of_the_year_edition` over
/// `the_witcher_3_wild_hunt`).
pub fn match_file_name<'a>(file_name: &str, titles: &'a [(String, String)]) -> Option<&'a str> {
    let stem = file_name.to_lowercase();
    let stem = stem.strip_prefix("setup_").unwrap_or(&stem);
    titles
        .iter()
        .filter_map(|(app, title)| {
            let s = slug(title);
            let fits = !s.is_empty()
                && stem.starts_with(&s)
                && stem[s.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| matches!(c, '_' | '.' | '-'));
            fits.then_some((s.len(), app.as_str()))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, app)| app)
}

/// A GOG product id in installer metadata: `goggame-<id>.info` in a Linux
/// installer's file list, or the number `innoextract --gog-game-id` prints.
fn product_id_in(text: &str) -> Option<String> {
    if let Some(at) = text.find("goggame-") {
        let id: String = text[at + 8..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !id.is_empty() {
            return Some(id);
        }
    }
    text.split(|c: char| !c.is_ascii_digit())
        .find(|run| run.len() >= 7)
        .map(str::to_string)
}

impl GogStore {
    /// The offline files GOG offers for a game.
    pub(crate) async fn list_extras(&self, app_name: &str) -> Result<Vec<ExtraFile>> {
        let client = self.content().await?;
        let product = client
            .product(app_name, &["downloads"])
            .await
            .context("Could not read the game's downloads")?;
        Ok(map_extras(&product))
    }

    /// The current address of an offline file, and its name.
    pub(crate) async fn extra_address(&self, file: &ExtraFile) -> Result<(String, String)> {
        let client = self.content().await?;
        let url = client.resolve_downlink(&file.id).await?;
        let name = file_name_of(&url).context("GOG gave a download without a file name")?;
        Ok((url, name))
    }

    /// GOG installers in `folder` (not its subfolders), each matched to an
    /// owned game where possible: by the product id in the installer, else
    /// by its file name.
    pub async fn scan_installers(&self, folder: &Path) -> Result<Vec<FoundInstaller>> {
        let mut entries = tokio::fs::read_dir(folder)
            .await
            .with_context(|| format!("Cannot read {}", folder.display()))?;
        let titles: Vec<(String, String)> = self
            .games()
            .iter()
            .filter(|g| !g.is_dlc)
            .map(|g| (g.app_name.clone(), g.title.clone()))
            .collect();
        let mut found = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(platform) = installer_platform(&name) else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let tool = tool_for(platform);
            let missing_tool = (!have(tool)).then_some(tool);
            let from_metadata = match missing_tool {
                None => read_product_id(&path, platform).await,
                Some(_) => None,
            };
            let app_name = from_metadata
                .filter(|id| titles.iter().any(|(app, _)| app == id))
                .or_else(|| match_file_name(&name, &titles).map(str::to_string));
            found.push(FoundInstaller {
                path,
                platform,
                app_name,
                missing_tool,
            });
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    /// Install `app_name` by unpacking `installer` into the install folder
    /// and importing it.
    pub(crate) async fn install_from_installer(
        &self,
        app_name: &str,
        request: &InstallRequest,
        installer: &Path,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let name = installer
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let platform = installer_platform(&name)
            .with_context(|| format!("{} is not a GOG installer", installer.display()))?;
        let tool = tool_for(platform);
        if !have(tool) {
            bail!("Unpacking this installer needs {tool}; install it from your distribution");
        }
        let folder = self
            .game(app_name)
            .and_then(|g| g.folder_name.clone())
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| self.title_of(app_name));
        let dest = request.path.join(irori_core::paths::sanitize(&folder));
        if dest.exists() {
            bail!(
                "{} already exists; choose another install folder",
                dest.display()
            );
        }
        let scratch = request.path.join(format!(".irori-unpack-{app_name}"));
        let _ = tokio::fs::remove_dir_all(&scratch).await;
        tokio::fs::create_dir_all(&scratch).await?;

        (ctx.step)("Unpacking");
        let unpacked = unpack(installer, platform, &scratch, &ctx).await;
        let result = match unpacked {
            Ok(None) => Ok(Outcome::Abort),
            Ok(Some(content)) => match tokio::fs::rename(&content, &dest).await {
                Ok(()) => {
                    (ctx.step)("Importing");
                    match self.import_game(app_name, &dest, platform).await {
                        Ok(()) => Ok(Outcome::Done),
                        Err(e) => {
                            // Not this game after all: leave nothing behind.
                            let _ = tokio::fs::remove_dir_all(&dest).await;
                            Err(e)
                        }
                    }
                }
                Err(e) => Err(e).with_context(|| format!("Moving the game to {}", dest.display())),
            },
            Err(e) => Err(e),
        };
        let _ = tokio::fs::remove_dir_all(&scratch).await;
        result
    }
}

/// Unpack an installer into `scratch` and return the folder holding the
/// game's files, or `None` when cancelled.
async fn unpack(
    installer: &Path,
    platform: InstallPlatform,
    scratch: &Path,
    ctx: &OpContext,
) -> Result<Option<PathBuf>> {
    let mut opts = RunOptions::default().cancel(ctx.cancel.clone());
    if let Some(log) = &ctx.log_file {
        opts = opts.log_file(log);
    }
    let spec = match platform {
        // `unzip` warns about the script before the archive (exit code 1).
        InstallPlatform::Linux => CommandSpec::new("unzip")
            .arg("-qq")
            .arg("-o")
            .arg(installer)
            .arg("data/noarch/*")
            .arg("-d")
            .arg(scratch),
        _ => CommandSpec::new("innoextract")
            .arg("--silent")
            .arg("--gog")
            .arg("--exclude-temp")
            .arg("--output-dir")
            .arg(scratch)
            .arg(installer),
    };
    let res = process::run(&spec, opts).await?;
    if res.aborted {
        return Ok(None);
    }
    let content = match platform {
        InstallPlatform::Linux => {
            if !matches!(res.code, Some(0 | 1)) {
                bail!("unzip could not read the installer: {}", res.stderr.trim());
            }
            scratch.join("data").join("noarch")
        }
        _ => {
            if res.code != Some(0) {
                bail!(
                    "innoextract could not read the installer: {}",
                    res.stderr.trim()
                );
            }
            // Older installers keep the game under `app/`.
            let app = scratch.join("app");
            if app.is_dir() {
                app
            } else {
                scratch.to_path_buf()
            }
        }
    };
    if !content.is_dir() {
        bail!("{} holds no game files", installer.display());
    }
    Ok(Some(content))
}

/// The GOG product id recorded in an installer, read without unpacking it.
async fn read_product_id(path: &Path, platform: InstallPlatform) -> Option<String> {
    let spec = match platform {
        InstallPlatform::Linux => CommandSpec::new("unzip").arg("-Z1").arg(path),
        _ => CommandSpec::new("innoextract")
            .arg("--gog-game-id")
            .arg(path),
    };
    let res = process::run(&spec, RunOptions::default()).await.ok()?;
    product_id_in(&res.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_installers_parts_and_goodies() {
        let product = json!({"downloads": {
            "installers": [{
                "id": "installer_windows_en", "name": "Cyberpunk 2077", "os": "windows",
                "language_full": "English", "version": "2.13",
                "files": [
                    {"id": "en1installer0", "size": 100, "downlink": "https://api.gog.com/d/1"},
                    {"id": "en1installer1", "size": 200, "downlink": "https://api.gog.com/d/2"}
                ]
            }, {
                "id": "installer_linux_en", "name": "Cyberpunk 2077", "os": "linux",
                "language_full": "English", "version": "",
                "files": [{"id": "en3installer0", "size": 50, "downlink": "https://api.gog.com/d/3"}]
            }],
            "patches": [],
            "bonus_content": [{
                "id": 7911, "name": "soundtrack (MP3)", "type": "audio",
                "files": [{"id": 7911, "size": 3000, "downlink": "https://api.gog.com/d/4"}]
            }, {"id": 1, "name": "broken", "type": "manuals", "files": [{"id": 1}]}]
        }});
        let files = map_extras(&product);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].part, Some((1, 2)));
        assert_eq!(files[0].platform, Some(InstallPlatform::Windows));
        assert_eq!(files[0].version.as_deref(), Some("2.13"));
        assert_eq!(files[2].part, None);
        assert_eq!(files[2].version, None);
        assert_eq!(files[3].group, ExtraGroup::Goodie);
        assert_eq!(files[3].category, "audio");
        assert_eq!(files[3].id, "https://api.gog.com/d/4");
    }

    #[test]
    fn file_names_come_from_the_address() {
        assert_eq!(
            file_name_of("https://cdn.gog.com/secure/x/setup_cyberpunk_2077_2.13_(78282).exe?a=1")
                .as_deref(),
            Some("setup_cyberpunk_2077_2.13_(78282).exe")
        );
        assert_eq!(
            file_name_of("https://cdn/x/cyberpunk%202077%20soundtrack.zip").as_deref(),
            Some("cyberpunk 2077 soundtrack.zip")
        );
        assert_eq!(file_name_of("https://cdn/x/..%2F..%2Fetc"), None);
        assert_eq!(file_name_of("https://cdn/x/"), None);
    }

    #[test]
    fn recognises_installers_and_matches_them_to_games() {
        assert_eq!(
            installer_platform("setup_cyberpunk_2077_2.13_(78282).exe"),
            Some(InstallPlatform::Windows)
        );
        assert_eq!(
            installer_platform("setup_cyberpunk_2077_2.13_(78282)-1.bin"),
            None
        );
        assert_eq!(
            installer_platform("patch_cyberpunk_2077_2.12_to_2.13.exe"),
            None
        );
        assert_eq!(
            installer_platform("stardew_valley_1_6_8_24119.sh"),
            Some(InstallPlatform::Linux)
        );
        let titles = vec![
            ("1".to_string(), "Cyberpunk 2077".to_string()),
            ("2".to_string(), "The Witcher 3: Wild Hunt".to_string()),
            (
                "3".to_string(),
                "The Witcher 3: Wild Hunt - Game of the Year Edition".to_string(),
            ),
            ("4".to_string(), "Baldur's Gate".to_string()),
            ("5".to_string(), "Stardew Valley".to_string()),
        ];
        assert_eq!(
            match_file_name("setup_cyberpunk_2077_2.13_(78282).exe", &titles),
            Some("1")
        );
        assert_eq!(
            match_file_name(
                "setup_the_witcher_3_wild_hunt_game_of_the_year_edition_4.04.exe",
                &titles
            ),
            Some("3")
        );
        assert_eq!(
            match_file_name("setup_baldurs_gate_2.6.exe", &titles),
            Some("4")
        );
        assert_eq!(
            match_file_name("stardew_valley_1_6_8_24119.sh", &titles),
            Some("5")
        );
        assert_eq!(match_file_name("setup_cyberpunkish_1.0.exe", &titles), None);
    }

    #[test]
    fn finds_product_ids_in_installer_metadata() {
        assert_eq!(
            product_id_in("data/noarch/gameinfo\ndata/noarch/goggame-1207665883.info\n").as_deref(),
            Some("1207665883")
        );
        assert_eq!(product_id_in("1423049311\n").as_deref(), Some("1423049311"));
        assert_eq!(product_id_in("data/noarch/start.sh\n"), None);
    }
}
