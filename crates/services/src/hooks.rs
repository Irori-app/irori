//! Runtime launch hooks: apply known fixes (environment, winetricks verbs,
//! files to run) published for a game.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use ember_core::{GameId, GameInfo, InstallPlatform, OpContext, async_trait};
use ember_runtime::{LaunchHooks, Target};

use crate::Backend;

pub(crate) struct KnownFixHooks {
    pub(crate) backend: Backend,
}

#[async_trait]
impl LaunchHooks for KnownFixHooks {
    async fn known_fixes_env(&self, id: &GameId) -> BTreeMap<String, String> {
        ember_extras::known_fixes::lookup(id.runner, &id.app_name)
            .await
            .map(|f| f.env_variables)
            .unwrap_or_default()
    }

    async fn prefix_initialized(
        &self,
        id: &GameId,
        game: &GameInfo,
        _prefix: &Path,
        ctx: &OpContext,
    ) -> Result<()> {
        let Some(fix) = ember_extras::known_fixes::lookup(id.runner, &id.app_name).await else {
            return Ok(());
        };
        if !fix.has_prefix_fixes() {
            return Ok(());
        }
        let app = self.backend.settings.app();
        let settings = self.backend.settings.game(id);
        let target = Target {
            id,
            game,
            platform: InstallPlatform::Windows,
        };
        if !fix.winetricks.is_empty() {
            (ctx.step)("Applying known fixes");
            ember_runtime::winetricks(&app, &settings, target, fix.winetricks.clone(), ctx.clone())
                .await?;
        }
        if let Some(dir) = game.install_path() {
            for file in &fix.run_in_prefix {
                let path = dir.join(file);
                if !path.is_file() {
                    tracing::warn!("known fix for {id}: {} not found", path.display());
                    continue;
                }
                (ctx.step)("Running setup files");
                ember_runtime::run_in_prefix(
                    &app,
                    &settings,
                    target,
                    vec![path.to_string_lossy().into_owned()],
                    true,
                    ctx.clone(),
                )
                .await?;
            }
        }
        Ok(())
    }
}
