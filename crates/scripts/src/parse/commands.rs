//! Installer steps: each command and task into its typed form, refusing
//! names and parameters Ember does not know.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{Params, invalid, missing, scalar};
use crate::error::{ScriptError, step_loc};
use crate::model::*;

/// Lutris's command-name normalisation: lower case, `-` as `_`, outer
/// underscores dropped.
fn command_name(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .trim_matches('_')
        .to_string()
}

pub(super) fn parse_step(
    number: usize,
    item: &Value,
    info: &ScriptInfo,
) -> Result<Step, ScriptError> {
    let (raw_name, params) = match item {
        Value::String(name) => (name.as_str(), &Value::Null),
        Value::Object(m) if m.len() == 1 => {
            let (k, v) = m.iter().next().expect("one entry");
            (k.as_str(), v)
        }
        _ => {
            return Err(invalid(
                &format!("Step {number}"),
                "a step is one command name with its parameters".into(),
            ));
        }
    };
    let name = command_name(raw_name);
    let location = step_loc(number, &name);
    let description = match params {
        Value::Object(m) => m.get("description").and_then(scalar),
        _ => None,
    };
    let command = parse_command(number, &name, params, &location, info)?;
    Ok(Step {
        number,
        description,
        command,
    })
}

fn parse_command(
    number: usize,
    name: &str,
    params: &Value,
    location: &str,
    info: &ScriptInfo,
) -> Result<Command, ScriptError> {
    let loc = location.to_string();
    // Commands whose parameter may be a bare string.
    let bare = scalar(params);
    Ok(match name {
        "chmodx" => match bare {
            Some(file) => Command::Chmodx { file },
            None => {
                let mut p = Params::of(params, loc)?;
                p.ignore("description");
                let file = p.req_str("file")?;
                p.finish()?;
                Command::Chmodx { file }
            }
        },
        "mkdir" | "create_dir" => match bare {
            Some(path) => Command::Mkdir { path },
            None => {
                let mut p = Params::of(params, loc)?;
                p.ignore("description");
                let path = p.req_str("path")?;
                p.finish()?;
                Command::Mkdir { path }
            }
        },
        "substitute_vars" => match bare {
            Some(file) => Command::SubstituteVars { file },
            None => {
                let mut p = Params::of(params, loc)?;
                p.ignore("description");
                let file = p.req_str("file")?;
                p.finish()?;
                Command::SubstituteVars { file }
            }
        },
        "execute" => match bare {
            Some(command) => Command::Execute(Execute {
                program: Program::Shell(command),
                args: String::new(),
                terminal: false,
                working_dir: None,
                env: BTreeMap::new(),
                include_processes: Vec::new(),
                exclude_processes: Vec::new(),
                return_code: None,
                disable_runtime: false,
            }),
            None => {
                let mut p = Params::of(params, loc.clone())?;
                p.ignore("description");
                let program = match (p.str("file")?, p.str("command")?) {
                    (Some(f), None) => Program::File(f),
                    (None, Some(c)) => Program::Shell(c),
                    (Some(_), Some(_)) => {
                        return Err(invalid(&loc, "use “file” or “command”, not both".into()));
                    }
                    (None, None) => return Err(missing(&loc, "file")),
                };
                let e = Execute {
                    program,
                    args: p.str("args")?.unwrap_or_default(),
                    terminal: p.bool("terminal")?.unwrap_or(false),
                    working_dir: p.str("working_dir")?,
                    env: p.map("env")?,
                    include_processes: p.words("include_processes")?,
                    exclude_processes: p.words("exclude_processes")?,
                    return_code: p.return_code()?,
                    disable_runtime: p.bool("disable_runtime")?.unwrap_or(false),
                };
                p.finish()?;
                Command::Execute(e)
            }
        },
        "autosetup_gog_game" | "autosetup_zoom_platform" => {
            let (store, command) = if name == "autosetup_gog_game" {
                (AutosetupStore::Gog, "autosetup_gog_game")
            } else {
                (AutosetupStore::Zoom, "autosetup_zoom_platform")
            };
            let (file, silent) = match bare {
                Some(f) => (f, false),
                None => {
                    let mut p = Params::of(params, loc)?;
                    p.ignore("description");
                    let f = p.req_str("file_id")?;
                    let s = p.bool("silent")?.unwrap_or(false);
                    p.finish()?;
                    (f, s)
                }
            };
            let id = match store {
                AutosetupStore::Gog => info.gogid.clone(),
                _ => None,
            }
            .or_else(|| info.service_id.clone())
            .unwrap_or_else(|| info.game_slug.clone());
            Command::StoreAutosetup(StoreAutosetup {
                store,
                command,
                id,
                file: Some(file.replace('-', "_")),
                silent,
                params: params.clone(),
            })
        }
        "gogdl_setup" => {
            let mut p = Params::of(params, loc)?;
            p.ignore("description");
            let id = p.str("game_id")?;
            for k in ["platform", "lang", "dlcs", "preserve_manifest", "command"] {
                p.ignore(k);
            }
            p.finish()?;
            let id = id
                .or_else(|| info.gogid.clone())
                .or_else(|| info.service_id.clone())
                .ok_or_else(|| missing(location, "game_id"))?;
            Command::StoreAutosetup(StoreAutosetup {
                store: AutosetupStore::Gog,
                command: "gogdl_setup",
                id,
                file: None,
                silent: true,
                params: params.clone(),
            })
        }
        "autosetup_amazon" => {
            let mut p = Params::of(params, loc)?;
            p.ignore("description");
            p.ignore("files");
            p.ignore("directories");
            p.finish()?;
            let id = info
                .service_id
                .clone()
                .unwrap_or_else(|| info.game_slug.clone());
            Command::StoreAutosetup(StoreAutosetup {
                store: AutosetupStore::Amazon,
                command: "autosetup_amazon",
                id,
                file: None,
                silent: true,
                params: params.clone(),
            })
        }
        "task" => Command::Task(parse_task(number, params, location)?),
        "extract" | "merge" | "copy" | "move" | "rename" | "write_file" | "write_json"
        | "write_config" | "input_menu" | "insert_disc" => {
            let mut p = Params::of(params, loc.clone())?;
            p.ignore("description");
            let cmd = match name {
                "extract" => {
                    let file = match (p.str("file")?, p.str("src")?) {
                        (Some(f), _) | (None, Some(f)) => f,
                        (None, None) => return Err(missing(&loc, "file")),
                    };
                    let format = p
                        .str("format")?
                        .map(|f| {
                            archive_format(&f).ok_or_else(|| {
                                invalid(&loc, format!("“{f}” is not an archive format Ember knows"))
                            })
                        })
                        .transpose()?;
                    Command::Extract(Extract {
                        file,
                        dst: p.str("dst")?,
                        format,
                        merge_single: p.raw("nomerge").is_none(),
                    })
                }
                "merge" | "copy" => {
                    let c = CopyParams {
                        src: p.req_str("src")?,
                        dst: p.req_str("dst")?,
                        optional: p.bool("optional")?.unwrap_or(false),
                        remove_source: p.bool("remove_source")?.unwrap_or(false),
                    };
                    if name == "merge" {
                        Command::Merge(c)
                    } else {
                        Command::Copy(c)
                    }
                }
                "move" => Command::Move(MoveParams {
                    src: p.req_str("src")?,
                    dst: p.req_str("dst")?,
                    optional: p.bool("optional")?.unwrap_or(false),
                }),
                "rename" => Command::Rename {
                    src: p.req_str("src")?,
                    dst: p.req_str("dst")?,
                },
                "write_file" => {
                    let file = p.req_str("file")?;
                    let content = p.req_str("content")?;
                    let append = match p.str("mode")?.as_deref() {
                        None => false,
                        Some(m) if m.starts_with('w') => false,
                        Some(m) if m.starts_with('a') => true,
                        Some(m) => return Err(invalid(&loc, format!("“{m}” is not a write mode"))),
                    };
                    Command::WriteFile {
                        file,
                        content,
                        append,
                    }
                }
                "write_json" => Command::WriteJson {
                    file: p.req_str("file")?,
                    data: p
                        .raw("data")
                        .cloned()
                        .ok_or_else(|| missing(&loc, "data"))?,
                    merge: p.bool("merge")?.unwrap_or(true),
                },
                "write_config" => {
                    let file = p.req_str("file")?;
                    let merge = p.bool("merge")?.unwrap_or(true);
                    let entries = match p.raw("data") {
                        Some(Value::Object(sections)) => {
                            let mut out = Vec::new();
                            for (section, keys) in sections {
                                let Value::Object(keys) = keys else {
                                    return Err(invalid(
                                        &loc,
                                        format!("section “{section}” must be a mapping"),
                                    ));
                                };
                                for (key, value) in keys {
                                    out.push(ConfigEntry {
                                        section: section.clone(),
                                        key: key.clone(),
                                        value: match value {
                                            Value::Null => String::new(),
                                            v => scalar(v).ok_or_else(|| {
                                                invalid(&loc, format!("“{key}” must be text"))
                                            })?,
                                        },
                                    });
                                }
                            }
                            out
                        }
                        Some(_) => return Err(invalid(&loc, "“data” must be a mapping".into())),
                        None => vec![ConfigEntry {
                            section: p.req_str("section")?,
                            key: p.req_str("key")?,
                            value: p.str("value")?.unwrap_or_default(),
                        }],
                    };
                    p.ignore("section");
                    p.ignore("key");
                    p.ignore("value");
                    Command::WriteConfig {
                        file,
                        entries,
                        merge,
                    }
                }
                "input_menu" => {
                    let options = match p.raw("options") {
                        Some(Value::Array(items)) => items
                            .iter()
                            .map(|o| match o {
                                Value::Object(m) if m.len() == 1 => {
                                    let (k, v) = m.iter().next().expect("one entry");
                                    Ok(MenuOption {
                                        value: k.clone(),
                                        label: scalar(v).unwrap_or_else(|| k.clone()),
                                    })
                                }
                                v => scalar(v)
                                    .map(|s| MenuOption {
                                        value: s.clone(),
                                        label: s,
                                    })
                                    .ok_or_else(|| {
                                        invalid(&loc, "a menu option must be text".into())
                                    }),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        _ => return Err(missing(&loc, "options")),
                    };
                    if options.is_empty() {
                        return Err(invalid(&loc, "the menu has no options".into()));
                    }
                    Command::InputMenu(InputMenu {
                        id: p.str("id")?.map(|s| s.replace('-', "_")),
                        description: p.str("description")?,
                        options,
                        preselect: p.str("preselect")?,
                    })
                }
                _ => Command::InsertDisc {
                    requires: p.req_str("requires")?,
                    message: p.str("message")?,
                },
            };
            p.finish()?;
            cmd
        }
        _ => {
            return Err(ScriptError::UnknownCommand {
                step: number,
                name: name.to_string(),
            });
        }
    })
}

/// Lutris's extractor names.
fn archive_format(name: &str) -> Option<ArchiveFormat> {
    Some(match name.trim() {
        "tar" | "tgz" | "txz" | "tbz2" | "bz2" | "tzst" | "tar.gz" | "tar.xz" | "tar.bz2" => {
            ArchiveFormat::Tar
        }
        "zip" => ArchiveFormat::Zip,
        "gzip" | "gz" => ArchiveFormat::Gzip,
        n @ ("7z" | "7zip" | "rar" | "gog" | "innoextract" | "innosetup" | "exe" | "msi"
        | "deb" | "AppImage" | "appimage" | "cab" | "uha") => ArchiveFormat::External(n.into()),
        _ => return None,
    })
}

fn parse_task(number: usize, params: &Value, location: &str) -> Result<Task, ScriptError> {
    let Value::Object(m) = params else {
        return Err(invalid(
            location,
            "a task needs a mapping with “name”".into(),
        ));
    };
    let raw = m
        .get("name")
        .and_then(scalar)
        .ok_or_else(|| missing(location, "name"))?;
    let raw = raw.trim();
    let task = match raw.split_once('.') {
        Some(("wine" | "dosbox", task)) => task,
        Some(_) => {
            return Err(ScriptError::UnknownTask {
                step: number,
                name: raw.to_string(),
            });
        }
        None => raw,
    };
    let loc = step_loc(number, task);
    let mut p = Params::new(m, loc.clone());
    p.ignore("name");
    p.ignore("description");
    let t = match task {
        "create_prefix" => Task::CreatePrefix(PrefixTask {
            prefix: p.str("prefix")?,
            arch: p.str("arch")?,
            overrides: p.map("overrides")?,
            install_gecko: p.bool("install_gecko")?,
            install_mono: p.bool("install_mono")?,
        }),
        "wineexec" => Task::WineExec(WineExecTask {
            executable: p.req_str("executable")?,
            args: p.str("args")?.unwrap_or_default(),
            prefix: p.str("prefix")?,
            arch: p.str("arch")?,
            working_dir: p.str("working_dir")?,
            blocking: p.bool("blocking")?.unwrap_or(false),
            env: p.map("env")?,
            overrides: p.map("overrides")?,
            include_processes: p.words("include_processes")?,
            exclude_processes: p.words("exclude_processes")?,
            disable_runtime: p.bool("disable_runtime")?.unwrap_or(false),
            return_code: p.return_code()?,
        }),
        "winetricks" => Task::Winetricks(WinetricksTask {
            app: p.req_str("app")?,
            prefix: p.str("prefix")?,
            arch: p.str("arch")?,
            silent: p.bool("silent")?.unwrap_or(true),
        }),
        "set_regedit" => {
            let kind = match p.str("type")? {
                None => RegType::Sz,
                Some(t) => RegType::from_name(&t)
                    .ok_or_else(|| invalid(&loc, format!("“{t}” is not a registry type")))?,
            };
            Task::SetRegedit(RegeditTask {
                path: p.req_str("path")?,
                key: p.req_str("key")?,
                value: p.str("value")?.unwrap_or_default(),
                kind,
                prefix: p.str("prefix")?,
                arch: p.str("arch")?,
            })
        }
        "winekill" => Task::WineKill(WineKillTask {
            prefix: p.str("prefix")?,
            arch: p.str("arch")?,
        }),
        "dosexec" => Task::DosExec(DosExecTask {
            config_file: p.str("config_file")?,
            executable: p.str("executable")?,
            args: p.str("args")?.unwrap_or_default(),
            exit: p.bool("exit")?.unwrap_or(true),
            working_dir: p.str("working_dir")?,
        }),
        _ => {
            return Err(ScriptError::UnknownTask {
                step: number,
                name: task.to_string(),
            });
        }
    };
    p.finish()?;
    Ok(t)
}

pub(super) fn parse_game(m: &Map<String, Value>) -> Result<GameSection, ScriptError> {
    let mut game = GameSection::default();
    for (k, v) in m {
        let text =
            || scalar(v).ok_or_else(|| invalid("Game section", format!("“{k}” must be text")));
        let slot = match k.as_str() {
            "exe" => &mut game.exe,
            "main_file" => &mut game.main_file,
            "iso" => &mut game.iso,
            "rom" => &mut game.rom,
            "path" => &mut game.path,
            "args" => &mut game.args,
            "working_dir" => &mut game.working_dir,
            "prefix" => &mut game.prefix,
            "arch" => &mut game.arch,
            _ => {
                game.options.insert(k.clone(), v.clone());
                continue;
            }
        };
        if !v.is_null() {
            *slot = Some(text()?);
        }
    }
    Ok(game)
}
