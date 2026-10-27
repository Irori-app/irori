//! Executor tests against an in-process HTTP server and a recording host,
//! plus the real-shaped script fixtures (GOG autosetup, Wine setup, DOSBox,
//! Linux) shared with `parse_tests`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ember_core::process::CancellationToken;
use ember_depot::test_server::{Resp, TestServer, serve_bytes};

use crate::model::*;
use crate::*;

pub(crate) const EXAMPLE: &str = "https://downloads.example.org";

pub(crate) fn gog_fixture() -> String {
    r#"
name: Darkest Dungeon
game_slug: darkest-dungeon
version: GOG
slug: darkest-dungeon-gog
runner: wine
gogid: 1719198803
user: tester
created_at: "2021-03-04T05:06:07.000Z"
script:
  files:
  - installer: "N/A:Select the GOG setup file"
  game:
    exe: $GAMEDIR/drive_c/GOG Games/Darkest Dungeon/_windows/Darkest.exe
    prefix: $GAMEDIR
  installer:
  - autosetup_gog_game: installer
  wine:
    version: lutris-GE-Proton8-26-x86_64
"#
    .into()
}

pub(crate) fn wine_fixture(base: &str, patch_checksum: &str) -> String {
    format!(
        r#"
name: Example Game
game_slug: example-game
version: Setup
slug: example-game-setup
runner: wine
script:
  files:
  - setup: {base}/example_setup.exe
  - patch:
      url: {base}/patch-1.1.zip
      filename: patch.zip
      checksum: {patch_checksum}
  game:
    exe: drive_c/Program Files/Example/game.exe
    prefix: $GAMEDIR
    arch: win64
  installer:
  - task:
      name: create_prefix
      arch: win64
  - task:
      name: winetricks
      app: d3dx9 vcrun2019
  - task:
      description: Running the setup
      name: wine.wineexec
      executable: setup
      args: /SILENT /DIR="C:\Program Files\Example"
  - extract:
      file: patch
      dst: $GAMEDIR/drive_c/Program Files/Example
  - task:
      name: set_regedit
      path: HKEY_CURRENT_USER\Software\Wine\Direct3D
      key: MaxVersionGL
      value: 30002
      type: REG_DWORD
  wine:
    version: wine-ge-8-26
  system:
    env:
      DXVK_HUD: fps
"#
    )
}

pub(crate) fn dosbox_fixture(base: &str) -> String {
    format!(
        r#"
name: Quake
game_slug: quake
version: DOSBox
slug: quake-dosbox
runner: dosbox
script:
  files:
  - gamezip: {base}/quake106.zip
  game:
    main_file: $GAMEDIR/dosbox.conf
  installer:
  - extract:
      file: gamezip
      dst: $GAMEDIR
  - write_config:
      file: $GAMEDIR/dosbox.conf
      section: autoexec
      key: mount
      value: c "$GAMEDIR"
  - write_config:
      file: $GAMEDIR/dosbox.conf
      data:
        sdl:
          fullscreen: true
          output: opengl
        cpu:
          cycles: max
  dosbox:
    version: staging
"#
    )
}

pub(crate) fn linux_fixture(base: &str) -> String {
    format!(
        r#"
name: Open Game
game_slug: open-game
version: Linux
slug: open-game-linux
runner: linux
script:
  files:
  - tarball: {base}/open-game-1.0.tar.gz
  game:
    exe: bin/open-game
    working_dir: $GAMEDIR
  installer:
  - extract:
      file: tarball
  - chmodx: $GAMEDIR/bin/open-game
  - input_menu:
      id: lang
      description: Choose the language
      options:
      - en: English
      - de: Deutsch
      preselect: en
  - write_file:
      file: $GAMEDIR/lang.cfg
      content: lang=$INPUT_lang
  - execute:
      command: cd "$GAMEDIR" && for f in bin/*; do echo "$f"; done
  - execute:
      file: $GAMEDIR/bin/post-install.sh
      args: --quiet --lang $INPUT_lang
"#
    )
}

/// A minimal script with the given installer steps (YAML list items).
pub(crate) fn steps_script(runner: &str, files: &str, steps: &str, game: &str) -> String {
    let block = |key: &str, content: &str| {
        if content.trim() == "[]" {
            format!("  {key}: []\n")
        } else {
            format!("  {key}:\n{content}\n")
        }
    };
    format!(
        "name: T\ngame_slug: t\nversion: v\nslug: t-v\nrunner: {runner}\nscript:\n{}{}{}",
        block("files", files),
        block("game", game),
        block("installer", steps)
    )
}

pub(crate) fn err(text: &str) -> ScriptError {
    Script::parse(text).expect_err("script should be refused")
}

pub(crate) fn ctx(tmp: &Path) -> Context {
    Context::new(tmp.join("games/g"), tmp.join("cache/g"), tmp.join("home"))
}

// ----- Executor -----

#[derive(Default)]
struct Host {
    calls: Mutex<Vec<String>>,
    shell: Mutex<Vec<ShellRequest>>,
    exec: Mutex<Vec<ExecRequest>>,
    wine: Mutex<Vec<WineExecRequest>>,
    picked: Option<PathBuf>,
    choice: Option<String>,
    exit_code: i32,
    /// Created (relative to the prefix) by `wineexec`, like a setup would.
    setup_creates: Option<&'static str>,
    /// Fired by `choose` to simulate the user cancelling mid-install.
    cancel_on_choose: Option<CancellationToken>,
}

impl Host {
    fn call(&self, s: impl Into<String>) {
        self.calls.lock().unwrap().push(s.into());
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ScriptHost for Host {
    async fn pick_file(&self, r: &FileRequest) -> anyhow::Result<Option<PathBuf>> {
        self.call(format!("pick_file {}", r.file_id));
        Ok(self.picked.clone())
    }
    async fn steam_file(
        &self,
        appid: &str,
        path: &str,
        _: &CancellationToken,
    ) -> anyhow::Result<PathBuf> {
        self.call(format!("steam_file {appid} {path}"));
        anyhow::bail!("Steam is not installed")
    }
    async fn choose(&self, menu: &MenuRequest) -> anyhow::Result<Option<String>> {
        self.call(format!("choose {}", menu.id.clone().unwrap_or_default()));
        if let Some(t) = &self.cancel_on_choose {
            t.cancel();
            return Ok(None);
        }
        Ok(self.choice.clone().or_else(|| menu.preselect.clone()))
    }
    async fn insert_disc(&self, _: &DiscRequest) -> anyhow::Result<Option<PathBuf>> {
        self.call("insert_disc");
        Ok(None)
    }
    async fn wine_bin(&self) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from("/opt/wine/bin/wine"))
    }
    async fn create_prefix(&self, r: &PrefixRequest, _: &CancellationToken) -> anyhow::Result<()> {
        self.call(format!(
            "create_prefix {}",
            r.arch.clone().unwrap_or_default()
        ));
        std::fs::create_dir_all(r.prefix.join("drive_c"))?;
        Ok(())
    }
    async fn wineexec(&self, r: &WineExecRequest, _: &CancellationToken) -> anyhow::Result<i32> {
        self.call("wineexec");
        if let Some(rel) = self.setup_creates {
            let p = r.prefix.join(rel);
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(p, b"MZ")?;
        }
        self.wine.lock().unwrap().push(r.clone());
        Ok(self.exit_code)
    }
    async fn winetricks(&self, r: &WinetricksRequest, _: &CancellationToken) -> anyhow::Result<()> {
        self.call(format!("winetricks {}", r.verbs.join(" ")));
        Ok(())
    }
    async fn set_regedit(&self, r: &RegistryRequest, _: &CancellationToken) -> anyhow::Result<()> {
        self.call(format!(
            "set_regedit {} {} {}",
            r.key,
            r.value,
            r.kind.name()
        ));
        Ok(())
    }
    async fn winekill(&self, _: &WineKillRequest, _: &CancellationToken) -> anyhow::Result<()> {
        self.call("winekill");
        Ok(())
    }
    async fn dosexec(&self, _: &DosExecRequest, _: &CancellationToken) -> anyhow::Result<i32> {
        self.call("dosexec");
        Ok(0)
    }
    async fn execute(&self, r: &ExecRequest, _: &CancellationToken) -> anyhow::Result<i32> {
        self.call("execute");
        self.exec.lock().unwrap().push(r.clone());
        Ok(self.exit_code)
    }
    async fn run_shell(&self, r: &ShellRequest, _: &CancellationToken) -> anyhow::Result<i32> {
        self.call("run_shell");
        self.shell.lock().unwrap().push(r.clone());
        Ok(self.exit_code)
    }
    async fn store_autosetup(
        &self,
        r: &AutosetupRequest,
        _: &CancellationToken,
    ) -> anyhow::Result<()> {
        self.call(format!("store_autosetup {:?} {}", r.store, r.id));
        let exe = r
            .game_dir
            .join("drive_c/GOG Games/Darkest Dungeon/_windows/Darkest.exe");
        std::fs::create_dir_all(exe.parent().unwrap())?;
        std::fs::write(exe, b"MZ")?;
        Ok(())
    }
}

fn no_progress() -> ProgressFn {
    Arc::new(|_| {})
}

/// Serve `files` (path → bytes), with ranges, HEAD sizes and 404 otherwise.
fn serve(files: Vec<(&'static str, Vec<u8>)>) -> TestServer {
    TestServer::start(move |req| {
        let path = req.path.split('?').next().unwrap_or_default();
        match files.iter().find(|(p, _)| *p == path) {
            Some((_, data)) => serve_bytes(req, data, true),
            None => Resp::status(404),
        }
    })
}

fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("a.zip");
    crate::archive::tests::make_zip(&p, files);
    std::fs::read(p).unwrap()
}

fn tar_gz_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("a.tar.gz");
    crate::archive::tests::make_tar_gz(&p, files);
    std::fs::read(p).unwrap()
}

fn md5_hex(data: &[u8]) -> String {
    use md5::Digest as _;
    hex::encode(md5::Md5::digest(data))
}

async fn install(script: &Script, ctx: &Context, host: &Host) -> anyhow::Result<Outcome> {
    let approvals = Approvals::all(&consent(script, ctx));
    run(
        script,
        ctx,
        host,
        &approvals,
        &CancellationToken::new(),
        no_progress(),
    )
    .await
}

#[tokio::test]
async fn runs_wine_setup_script() {
    let patch = zip_bytes(&[("patch/data.pak", b"patched")]);
    let checksum = format!("md5:{}", md5_hex(&patch));
    let server = serve(vec![
        ("/example_setup.exe", b"MZsetup".to_vec()),
        ("/patch-1.1.zip", patch),
    ]);
    let script = Script::parse(&wine_fixture(&server.base(), &checksum)).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let host = Host {
        setup_creates: Some("drive_c/Program Files/Example/game.exe"),
        ..Default::default()
    };
    let out = install(&script, &ctx, &host).await.unwrap();

    assert_eq!(
        host.calls(),
        [
            "create_prefix win64",
            "winetricks d3dx9 vcrun2019",
            "wineexec",
            "set_regedit MaxVersionGL 30002 REG_DWORD"
        ]
    );
    let w = host.wine.lock().unwrap()[0].clone();
    assert_eq!(w.executable, ctx.cache_dir.join("setup/example_setup.exe"));
    assert_eq!(w.args, ["/SILENT", r"/DIR=C:\Program Files\Example"]);
    assert_eq!(w.prefix, ctx.game_dir);
    assert_eq!(w.env.get("DXVK_HUD").map(String::as_str), Some("fps"));
    // The zip had one top folder, merged into the destination.
    let example = ctx.game_dir.join("drive_c/Program Files/Example");
    assert_eq!(std::fs::read(example.join("data.pak")).unwrap(), b"patched");
    assert!(!example.join(".ember-extract-0").exists());

    let game = out.game.unwrap();
    assert_eq!(game.exe, Some(example.join("game.exe")));
    assert_eq!(game.prefix, Some(ctx.game_dir.clone()));
    assert_eq!(game.wine_version.as_deref(), Some("wine-ge-8-26"));
    assert_eq!(out.record.slug, "example-game");
    assert_eq!(
        out.record.installer_slug.as_deref(),
        Some("example-game-setup")
    );
    assert_eq!(
        out.record.source_url.as_deref(),
        Some("https://lutris.net/games/example-game/")
    );
    assert!(ctx.cache_dir.join("patch/patch.zip").is_file());
}

#[tokio::test]
async fn runs_dosbox_script() {
    let zip = zip_bytes(&[("QUAKE/QUAKE.EXE", b"MZ"), ("QUAKE/ID1/PAK0.PAK", b"pak")]);
    let server = serve(vec![("/quake106.zip", zip)]);
    let script = Script::parse(&dosbox_fixture(&server.base())).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let out = install(&script, &ctx, &Host::default()).await.unwrap();
    assert!(ctx.game_dir.join("QUAKE.EXE").is_file());
    assert!(ctx.game_dir.join("ID1/PAK0.PAK").is_file());
    let conf = std::fs::read_to_string(ctx.game_dir.join("dosbox.conf")).unwrap();
    assert!(conf.contains(&format!(
        "[autoexec]\nmount=c \"{}\"",
        ctx.game_dir.display()
    )));
    assert!(conf.contains("[sdl]\nfullscreen=true\noutput=opengl"));
    assert!(conf.contains("[cpu]\ncycles=max"));
    let game = out.game.unwrap();
    assert_eq!(game.main_file, Some(ctx.game_dir.join("dosbox.conf")));
    assert_eq!(game.runner, "dosbox");
}

#[tokio::test]
async fn runs_linux_script_with_menu_and_shell() {
    let tarball = tar_gz_bytes(&[
        ("open-game/bin/open-game", b"#!/bin/sh\n"),
        ("open-game/bin/post-install.sh", b"#!/bin/sh\n"),
    ]);
    let server = serve(vec![("/open-game-1.0.tar.gz", tarball)]);
    let script = Script::parse(&linux_fixture(&server.base())).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let host = Host {
        choice: Some("de".into()),
        ..Default::default()
    };

    // Shell commands need their own acknowledgement.
    let mut approvals = Approvals::all(&consent(&script, &ctx));
    approvals.shell_acknowledged = false;
    let e = run(
        &script,
        &ctx,
        &host,
        &approvals,
        &CancellationToken::new(),
        no_progress(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        script_error(&e),
        Some(ScriptError::NotApproved(_))
    ));
    assert!(host.calls().is_empty());

    let out = install(&script, &ctx, &host).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(ctx.game_dir.join("lang.cfg")).unwrap(),
        "lang=de"
    );
    let shell = host.shell.lock().unwrap()[0].clone();
    assert_eq!(
        shell.command,
        format!(
            r#"cd "{}" && for f in bin/*; do echo "$f"; done"#,
            ctx.game_dir.display()
        )
    );
    let exec = host.exec.lock().unwrap()[0].clone();
    assert_eq!(exec.program, ctx.game_dir.join("bin/post-install.sh"));
    assert_eq!(exec.args, ["--quiet", "--lang", "de"]);
    assert_eq!(exec.working_dir, ctx.game_dir);
    let game = out.game.unwrap();
    assert_eq!(game.exe, Some(ctx.game_dir.join("bin/open-game")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(ctx.game_dir.join("bin/open-game"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}

#[tokio::test]
async fn gog_autosetup_goes_to_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("setup_darkest_dungeon.exe");
    std::fs::write(&picked, b"MZ").unwrap();
    let script = Script::parse(&gog_fixture()).unwrap();
    let ctx = ctx(tmp.path());
    let host = Host {
        picked: Some(picked),
        ..Default::default()
    };
    let out = install(&script, &ctx, &host).await.unwrap();
    assert_eq!(
        host.calls(),
        ["pick_file installer", "store_autosetup Gog 1719198803"]
    );
    assert!(out.game.unwrap().exe.unwrap().is_file());
}

#[tokio::test]
async fn checksum_mismatch_fails_and_rolls_back() {
    let server = serve(vec![("/f.zip", b"not what was promised".to_vec())]);
    let text = steps_script(
        "linux",
        &format!(
            "  - f:\n      url: {}/f.zip\n      filename: f.zip\n      checksum: sha256:{}",
            server.base(),
            "a".repeat(64)
        ),
        "  - extract: {file: f}",
        "    exe: a",
    );
    let script = Script::parse(&text).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let e = install(&script, &ctx, &Host::default()).await.unwrap_err();
    assert!(matches!(
        script_error(&e),
        Some(ScriptError::ChecksumMismatch { file, .. }) if file == "f.zip"
    ));
    // The damaged download is gone; the game folder this install created
    // is rolled back.
    assert!(!ctx.cache_dir.join("f/f.zip").exists());
    assert!(!ctx.game_dir.exists());
}

#[tokio::test]
async fn picked_files_are_verified_and_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("setup.exe");
    std::fs::write(&picked, b"abc").unwrap();
    let files = format!(
        "  - setup:\n      url: \"N/A:Choose setup.exe\"\n      filename: setup.exe\n      checksum: sha1:{}",
        "0".repeat(40)
    );
    let text = steps_script("linux", &files, "  []", "    exe: a");
    let script = Script::parse(&text).unwrap();
    let host = Host {
        picked: Some(picked.clone()),
        ..Default::default()
    };
    let e = install(&script, &ctx(tmp.path()), &host).await.unwrap_err();
    assert!(matches!(
        script_error(&e),
        Some(ScriptError::ChecksumMismatch { .. })
    ));
    assert!(picked.exists());

    let good = files.replace(&"0".repeat(40), "a9993e364706816aba3e25717850c26c9cd0d89d");
    let text = steps_script(
        "linux",
        &good,
        "  - copy: {src: setup, dst: $GAMEDIR}",
        "    exe: setup.exe",
    );
    let script = Script::parse(&text).unwrap();
    let ctx = ctx(tmp.path());
    let out = install(&script, &ctx, &host).await.unwrap();
    assert_eq!(out.files["setup"], ctx.game_dir.join("setup.exe"));
    assert!(picked.exists());
}

#[tokio::test]
async fn writes_outside_allowed_folders_are_refused_unless_approved() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let escape = steps_script(
        "linux",
        "  []",
        "  - write_file:\n      file: $GAMEDIR/../../escape.txt\n      content: x",
        "    exe: a",
    );
    let script = Script::parse(&escape).unwrap();
    // Listed for consent, but refused if not approved.
    let plan = consent(&script, &ctx);
    assert_eq!(plan.outside_writes.len(), 1);
    let approvals = Approvals {
        accepted: true,
        ..Default::default()
    };
    let e = run(
        &script,
        &ctx,
        &Host::default(),
        &approvals,
        &CancellationToken::new(),
        no_progress(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        script_error(&e),
        Some(ScriptError::OutsideAllowedFolders { location, .. }) if location == "Step 1 (write_file)"
    ));
    assert!(!tmp.path().join("escape.txt").exists());

    let elsewhere = steps_script(
        "linux",
        "  []",
        "  - mkdir: $HOME/../not-allowed",
        "    exe: a",
    );
    let script = Script::parse(&elsewhere).unwrap();
    let e = run(
        &script,
        &ctx,
        &Host::default(),
        &approvals,
        &CancellationToken::new(),
        no_progress(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        script_error(&e),
        Some(ScriptError::OutsideAllowedFolders { .. })
    ));
    assert!(!tmp.path().join("not-allowed").exists());

    // Approved: the write goes through.
    let home_write = steps_script(
        "linux",
        "  []",
        "  - write_file:\n      file: $HOME/.config/game/settings.ini\n      content: x\n  - write_file:\n      file: $GAMEDIR/a\n      content: a",
        "    exe: a",
    );
    let script = Script::parse(&home_write).unwrap();
    install(&script, &ctx, &Host::default()).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(ctx.home.join(".config/game/settings.ini")).unwrap(),
        "x"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_cannot_lead_writes_out() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&ctx.game_dir).unwrap();
    std::os::unix::fs::symlink(&outside, ctx.game_dir.join("link")).unwrap();
    let text = steps_script(
        "linux",
        "  []",
        "  - write_file:\n      file: $GAMEDIR/link/pwned.txt\n      content: x",
        "    exe: a",
    );
    let script = Script::parse(&text).unwrap();
    let e = install(&script, &ctx, &Host::default()).await.unwrap_err();
    assert!(matches!(
        script_error(&e),
        Some(ScriptError::OutsideAllowedFolders { .. })
    ));
    assert!(!outside.join("pwned.txt").exists());
}

#[tokio::test]
async fn failure_rolls_back_only_what_the_install_created() {
    let server = serve(vec![("/data.bin", b"data".to_vec())]);
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    std::fs::create_dir_all(&ctx.game_dir).unwrap();
    std::fs::write(ctx.game_dir.join("keep.txt"), b"mine").unwrap();
    let text = steps_script(
        "linux",
        &format!("  - data: {}/data.bin", server.base()),
        "  - mkdir: $GAMEDIR/new/deeper\n  - write_file:\n      file: $GAMEDIR/new/a.txt\n      content: a\n  - copy:\n      src: data\n      dst: $GAMEDIR\n  - execute:\n      file: $GAMEDIR/data.bin",
        "    exe: data.bin",
    );
    let script = Script::parse(&text).unwrap();
    let host = Host {
        exit_code: 3,
        ..Default::default()
    };
    let e = install(&script, &ctx, &host).await.unwrap_err();
    assert!(matches!(
        script_error(&e),
        Some(ScriptError::ExitCode { code: 3, location }) if location == "Step 4 (execute)"
    ));
    assert_eq!(
        std::fs::read(ctx.game_dir.join("keep.txt")).unwrap(),
        b"mine"
    );
    assert!(!ctx.game_dir.join("new").exists());
    assert!(!ctx.game_dir.join("data.bin").exists());
    // The download stays for a retry.
    assert!(ctx.cache_dir.join("data/data.bin").is_file());
}

#[tokio::test]
async fn cancel_rolls_back_and_reports_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let text = steps_script(
        "linux",
        "  []",
        "  - write_file:\n      file: $GAMEDIR/a.txt\n      content: a\n  - input_menu:\n      id: q\n      options: [low, high]",
        "    exe: a.txt",
    );
    let script = Script::parse(&text).unwrap();
    let cancel = CancellationToken::new();
    let host = Host {
        cancel_on_choose: Some(cancel.clone()),
        ..Default::default()
    };
    let approvals = Approvals::all(&consent(&script, &ctx));
    let e = run(&script, &ctx, &host, &approvals, &cancel, no_progress())
        .await
        .unwrap_err();
    assert!(ember_runtime::is_cancelled(&e));
    assert!(!ctx.game_dir.exists());
}

#[tokio::test]
async fn missing_executable_is_a_typed_error() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let text = steps_script(
        "linux",
        "  []",
        "  - mkdir: $GAMEDIR/bin",
        "    exe: bin/game",
    );
    let script = Script::parse(&text).unwrap();
    let e = install(&script, &ctx, &Host::default()).await.unwrap_err();
    assert_eq!(
        script_error(&e),
        Some(&ScriptError::MissingExecutable {
            expected: Some(ctx.game_dir.join("bin/game"))
        })
    );
    let auto = steps_script(
        "wine",
        "  []",
        "  - mkdir: $GAMEDIR/x",
        "    exe: _xXx_AUTO_WIN32_xXx_",
    );
    let script = Script::parse(&auto).unwrap();
    let e = install(&script, &ctx, &Host::default()).await.unwrap_err();
    assert_eq!(
        script_error(&e),
        Some(&ScriptError::MissingExecutable { expected: None })
    );
}

#[tokio::test]
async fn zip_entries_cannot_escape() {
    let evil = zip_bytes(&[("../evil.txt", b"x"), ("ok.txt", b"y")]);
    let server = serve(vec![("/evil.zip", evil)]);
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let text = steps_script(
        "linux",
        &format!("  - z: {}/evil.zip", server.base()),
        "  - extract: {file: z}",
        "    exe: ok.txt",
    );
    let script = Script::parse(&text).unwrap();
    assert!(install(&script, &ctx, &Host::default()).await.is_err());
    assert!(!ctx.game_dir.parent().unwrap().join("evil.txt").exists());
    assert!(!ctx.game_dir.exists());
}

#[tokio::test]
async fn move_from_cache_copies_and_move_inside_moves() {
    let server = serve(vec![("/a.dat", b"a".to_vec())]);
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let text = steps_script(
        "linux",
        &format!("  - a: {}/a.dat", server.base()),
        "  - move: {src: a, dst: $GAMEDIR/data}\n  - write_file: {file: $GAMEDIR/tmp.txt, content: t}\n  - move: {src: $GAMEDIR/tmp.txt, dst: $GAMEDIR/final.txt}",
        "    exe: final.txt",
    );
    let script = Script::parse(&text).unwrap();
    let out = install(&script, &ctx, &Host::default()).await.unwrap();
    assert!(ctx.cache_dir.join("a/a.dat").is_file());
    assert_eq!(out.files["a"], ctx.game_dir.join("data"));
    assert!(ctx.game_dir.join("data").is_file());
    assert!(!ctx.game_dir.join("tmp.txt").exists());
    assert!(ctx.game_dir.join("final.txt").is_file());
}

#[tokio::test]
async fn unknown_runtime_choice_values_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let text = steps_script(
        "linux",
        "  []",
        "  - input_menu:\n      id: q\n      options: [low, high]\n  - write_file: {file: $GAMEDIR/q, content: $INPUT_q}",
        "    exe: q",
    );
    let script = Script::parse(&text).unwrap();
    let host = Host {
        choice: Some("ultra".into()),
        ..Default::default()
    };
    let e = install(&script, &ctx(tmp.path()), &host).await.unwrap_err();
    assert!(matches!(
        script_error(&e),
        Some(ScriptError::Invalid { .. })
    ));
}
