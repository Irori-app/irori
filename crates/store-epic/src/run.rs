//! Launching games.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ember_core::{LaunchOption, LaunchPlan, LaunchRequest, OpContext, Store as _};
use ember_epic_api::launch::{LaunchInput, launch_params};

use crate::EpicStore;
use crate::launch::{self, CommandInput};

impl EpicStore {
    pub(crate) async fn launch_plan(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: &OpContext,
    ) -> Result<LaunchPlan> {
        let record = self.require_record(app_name)?;
        let game = self.require_game(app_name)?;
        // Launchable add-ons run from their base game's install.
        let (launch_app, addon) = match (&request.option, self.installed_entry(app_name)) {
            (Some(LaunchOption::Dlc { dlc_app_name, .. }), _) => (dlc_app_name.clone(), None),
            (_, None) if record.is_launchable_addon() => (
                record
                    .main_game_app()
                    .context("The add-on's base game is unknown")?
                    .to_string(),
                Some(record.clone()),
            ),
            _ => (app_name.to_string(), None),
        };
        let installed = self
            .installed_entry(&launch_app)
            .with_context(|| format!("{} is not installed", game.title))?;
        let needs_wine = installed.install_platform().needs_wine();
        launch::require_wine(&request.environment, needs_wine)?;
        let offline = request.offline || self.epic_service_offline().await;

        let mut env = request.environment.env.clone();
        env.insert("HEROIC_APP_NAME".into(), app_name.into());
        env.insert("HEROIC_APP_RUNNER".into(), "legendary".into());
        env.insert("HEROIC_APP_SOURCE".into(), "epic".into());
        env.insert("STORE".into(), "egs".into());
        env.entry("GAMEID".into()).or_insert_with(|| "umu-0".into());

        if cfg!(windows) && installed.install_platform() == ember_core::InstallPlatform::Windows {
            self.first_launch_setup(&game, &installed, request, None, ctx)
                .await;
        }
        let mut wrapper_exe = None;
        if let (Some(wine), true) = (&request.environment.wine, needs_wine) {
            self.first_launch_setup(&game, &installed, request, Some(wine), ctx)
                .await;
            wrapper_exe = self.install_fake_exe(wine, &env).await;
            if wine.uses_umu
                && env.get("GAMEID").is_none_or(|g| g == "umu-0")
                && let Some(id) = self.umu_id(app_name).await
            {
                env.insert("GAMEID".into(), id);
            }
        }

        if installed.third_party {
            return self
                .third_party_plan(&game, &record, request, env, offline)
                .await;
        }
        if installed.is_dlc && installed.executable.is_empty() {
            bail!(
                "{} is DLC without an executable; launch the base game instead",
                game.title
            );
        }
        if !Path::new(&installed.install_path).is_dir() {
            bail!(
                "The install folder {} is missing. Reconnect the drive, or uninstall and reinstall the game.",
                installed.install_path
            );
        }
        if !offline
            && !request.skip_version_check
            && let Some(latest) = self
                .record(&launch_app)
                .and_then(|r| r.version(installed.platform_arg()).map(str::to_string))
            && latest != installed.version
        {
            bail!(
                "{} has an update. Update it first, or launch it with the update check skipped.",
                game.title
            );
        }

        let session = if offline {
            None
        } else {
            Some(self.session().await?)
        };
        let exchange_code = match &session {
            Some(s) => Some(self.client().await?.exchange_code(s).await?),
            None => None,
        };
        let meta = addon.unwrap_or_else(|| self.record(&launch_app).unwrap_or(record.clone()));
        let ownership_token = match (&session, installed.requires_ot) {
            (Some(s), true) => {
                let token = self
                    .client()
                    .await?
                    .ownership_token(s, meta.namespace(), meta.catalog_item_id())
                    .await?;
                let dir = self.layout.token_dir();
                let path: PathBuf = dir.join(format!(
                    "{}{}.ovt",
                    meta.namespace(),
                    meta.catalog_item_id()
                ));
                tokio::fs::create_dir_all(&dir).await?;
                tokio::fs::write(&path, token).await?;
                Some(path)
            }
            _ => None,
        };
        let settings = &request.settings;
        let executable = launch::override_exe(request.option.as_ref(), &settings.target_exe)
            .unwrap_or(&installed.executable)
            .to_string();
        let user = self.user().unwrap_or(ember_core::UserInfo {
            account_id: String::new(),
            display_name: String::new(),
        });
        let language = self.language(request);
        let params = launch_params(&LaunchInput {
            install_dir: Path::new(&installed.install_path),
            executable: &executable,
            launch_parameters: &installed.launch_parameters,
            additional_command_line: meta.additional_command_line(),
            app_name: &launch_app,
            exchange_code: exchange_code.as_deref(),
            ownership_token: ownership_token.as_deref(),
            account_id: &user.account_id,
            display_name: &user.display_name,
            language: &language,
            namespace: meta.namespace(),
            deployment_id: meta.deployment_id(),
        })?;
        if !params.executable.is_file()
            && !ember_epic_api::saves::case_insensitive(&params.executable).is_file()
        {
            bail!(
                "The game's executable {} is missing; repair the game",
                params.executable.display()
            );
        }
        let user_args = launch::user_args(
            request.option.as_ref(),
            &settings.launcher_args,
            &request.extra_args,
        );
        let command = launch::build_command(&CommandInput {
            params: &params,
            user_args: &user_args,
            environment: &request.environment,
            env,
            wrapper_exe,
            needs_wine,
        })?;
        Ok(LaunchPlan {
            command,
            track_wineserver: needs_wine,
        })
    }
}
