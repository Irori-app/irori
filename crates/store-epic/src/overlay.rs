//! The Epic Online Services overlay: installed once under Ember's tools
//! directory (downloaded like a game, anonymously) and enabled per Wine
//! prefix through a registry value.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use ember_core::{OpContext, Outcome, json_store};
use ember_epic_api::install::{Mode as PlanMode, PlanInput, plan};
use ember_epic_api::{Manifest, overlay, registry};

use crate::EpicStore;
use crate::deploy::log_line;
use crate::state::{OverlayInstall, real_prefix, remove_quietly};

/// Epic's app name of the overlay (used as the id of its install operation).
pub const EOS_OVERLAY_APP_NAME: &str = overlay::APP_NAME;

/// Installed state of the EOS overlay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EosOverlayStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub install_path: Option<String>,
}

impl EpicStore {
    fn overlay_install(&self) -> Option<OverlayInstall> {
        json_store::read::<OverlayInstall>(&self.layout.overlay_json())
            .ok()
            .flatten()
            .filter(|o| overlay::is_install(Path::new(&o.install_path)))
    }

    /// Whether the overlay is installed, and its version and location.
    pub async fn eos_overlay_status(&self) -> EosOverlayStatus {
        let Some(info) = self.overlay_install() else {
            return EosOverlayStatus::default();
        };
        if Path::new(&info.install_path) != self.layout.overlay_dir() {
            tracing::warn!(
                "EOS Overlay is not installed in default location, permission issues might arise"
            );
        }
        EosOverlayStatus {
            installed: true,
            version: Some(info.version).filter(|v| !v.is_empty()),
            install_path: Some(info.install_path),
        }
    }

    /// Forget the cached latest overlay version.
    pub async fn eos_overlay_refresh_info(&self) -> Result<()> {
        self.builds
            .lock()
            .remove(&format!("{}:Windows", overlay::APP_NAME));
        Ok(())
    }

    /// The overlay's current build (anonymous session).
    async fn overlay_build(&self) -> Result<(ember_epic_api::ManifestInfo, Vec<u8>, Manifest)> {
        let client = self.client().await?;
        let session = client.client_credentials().await?;
        let info = client
            .manifest_info(
                &session,
                overlay::NAMESPACE,
                overlay::CATALOG_ITEM_ID,
                overlay::APP_NAME,
                "Windows",
            )
            .await?;
        if info.use_signed_url {
            bail!(
                "The EOS overlay download needs signed addresses, which are not supported for it"
            );
        }
        let bytes = client.download_manifest(&info).await?;
        let manifest = Manifest::parse(&bytes, &info.secrets)?;
        Ok((info, bytes, manifest))
    }

    /// Latest available overlay version; `None` when unknown.
    pub async fn eos_overlay_latest_version(&self) -> Result<Option<String>> {
        if let Some(v) = self.install_info.get::<String>("eosOverlayVersion").await {
            return Ok(Some(v));
        }
        let (info, _, _) = self.overlay_build().await?;
        self.install_info
            .set("eosOverlayVersion", &info.build_version)
            .await;
        Ok(Some(info.build_version).filter(|v| !v.is_empty()))
    }

    /// Download and install the overlay into Ember's tools directory, with
    /// progress. Updates it when already installed.
    pub async fn eos_overlay_install(&self, ctx: OpContext) -> Result<Outcome> {
        match self.overlay_download(&ctx).await {
            Ok(o) => Ok(o),
            Err(_) if ctx.cancel.is_cancelled() => Ok(Outcome::Abort),
            Err(e) => {
                log_line(&ctx, &format!("EOS overlay: {e:#}"));
                Ok(Outcome::Error(format!("{e:#}")))
            }
        }
    }

    /// Update an installed overlay to the latest version.
    pub async fn eos_overlay_update(&self, ctx: OpContext) -> Result<Outcome> {
        if !self.eos_overlay_status().await.installed {
            bail!("The EOS overlay is not installed");
        }
        self.eos_overlay_install(ctx).await
    }

    async fn overlay_download(&self, ctx: &OpContext) -> Result<Outcome> {
        (ctx.step)("Fetching file list");
        let (info, bytes, manifest) = self.overlay_build().await?;
        let dir: PathBuf = self
            .overlay_install()
            .map(|o| PathBuf::from(o.install_path))
            .unwrap_or_else(|| self.layout.overlay_dir());
        let old = std::fs::read(self.layout.build_manifest(overlay::APP_NAME))
            .ok()
            .and_then(|b| Manifest::parse(&b, &info.secrets).ok());
        let exists = |rel: &str| {
            ember_depot::path::normalize(rel)
                .map(|r| ember_depot::path::join(&dir, &r).is_file())
                .unwrap_or(false)
        };
        let p = plan(
            &PlanInput {
                manifest: &manifest,
                old: old.as_ref(),
                base_urls: &info.base_urls,
                secrets: Arc::new(info.secrets.clone()),
                install_tags: &[],
                exists: &exists,
            },
            if old.is_some() {
                PlanMode::Update
            } else {
                PlanMode::Install
            },
        )?;
        (ctx.step)("Downloading");
        let client = self.client().await?;
        let sink = ctx.progress.clone();
        let finished = ember_depot::download(
            &client.download_client()?,
            ember_depot::Job {
                root: &dir,
                files: &p.files,
                remove: &p.remove,
            },
            &ember_depot::Options {
                journal: Some(self.layout.resume_file(overlay::APP_NAME)),
                ..Default::default()
            },
            Arc::new(move |p: ember_depot::Progress| {
                sink(ember_core::InstallProgress {
                    percent: Some(p.percent),
                    bytes_done: Some(p.bytes_done),
                    bytes_total: Some(p.bytes_total),
                    eta_secs: p.eta_secs,
                    download_speed: Some(p.download_speed),
                    disk_speed: Some(p.disk_speed),
                    bottleneck: p.bottleneck,
                    preparing: p.preparing,
                })
            }),
            &ctx.cancel,
        )
        .await?;
        if finished == ember_depot::Finished::Cancelled {
            return Ok(Outcome::Abort);
        }
        json_store::write_bytes(&self.layout.build_manifest(overlay::APP_NAME), &bytes)?;
        json_store::write(
            &self.layout.overlay_json(),
            &OverlayInstall {
                version: manifest.meta.build_version.clone(),
                install_path: dir.to_string_lossy().into_owned(),
            },
        )?;
        Ok(Outcome::Done)
    }

    /// Delete the overlay installation (the UI asks for confirmation first).
    pub async fn eos_overlay_remove(&self) -> Result<()> {
        if let Some(info) = self.overlay_install() {
            let dir = PathBuf::from(&info.install_path);
            if dir.is_dir() {
                tokio::fs::remove_dir_all(&dir)
                    .await
                    .with_context(|| format!("Could not delete {}", dir.display()))?;
            }
        }
        remove_quietly(&self.layout.overlay_json());
        remove_quietly(&self.layout.build_manifest(overlay::APP_NAME));
        Ok(())
    }

    fn overlay_prefix(prefix: &Path) -> Result<PathBuf> {
        real_prefix(prefix).with_context(|| {
            format!(
                "{} is not an initialised Wine prefix. Launch the game once first.",
                prefix.display()
            )
        })
    }

    /// Enable the overlay in a game's Wine prefix (Proton prefixes are
    /// resolved to their `pfx` directory).
    pub async fn eos_overlay_enable(&self, prefix: &Path) -> Result<()> {
        let info = self
            .overlay_install()
            .context("The EOS overlay is not installed. Install it first.")?;
        let pfx = Self::overlay_prefix(prefix)?;
        Self::blocking(move || registry::set_overlay_path(&pfx, Path::new(&info.install_path)))
            .await?
            .context("Enabling the EOS overlay failed")
    }

    /// Remove the overlay's registry value from a game's prefix.
    pub async fn eos_overlay_disable(&self, prefix: &Path) -> Result<()> {
        let Some(pfx) = real_prefix(prefix) else {
            return Ok(());
        };
        Self::blocking(move || registry::remove_overlay_path(&pfx))
            .await?
            .context("Disabling the EOS overlay failed")
    }

    /// Whether the overlay is enabled in a game's prefix.
    pub async fn eos_overlay_is_enabled(&self, prefix: &Path) -> Result<bool> {
        let Some(pfx) = real_prefix(prefix) else {
            return Ok(false);
        };
        let path = Self::blocking(move || registry::overlay_path(&pfx)).await??;
        Ok(path.is_some_and(|p| overlay::is_install(&p)))
    }
}
