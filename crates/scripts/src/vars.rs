//! Variable substitution. `$NAME` and `${NAME}` are replaced, `$$` is a
//! literal `$`, and a `$` not followed by a name stays as it is (`$1`,
//! `$(…)`). Unlike Lutris, a name nobody defined is an error instead of
//! literal text, and the built-ins cannot be redefined.

use std::collections::HashMap;

use crate::error::ScriptError;

/// Variables Irori provides. `INPUT_<id>` exists for every `input_menu`
/// with an id.
pub const BUILTINS: &[&str] = &[
    "GAMEDIR",
    "CACHE",
    "HOME",
    "USER",
    "DISC",
    "INPUT",
    "VERSION",
    "RESOLUTION",
    "RESOLUTION_WIDTH",
    "RESOLUTION_HEIGHT",
    "RESOLUTION_WIDTH_HEX",
    "RESOLUTION_HEIGHT_HEX",
    "WINEBIN",
];

/// Whether a script variable or file id named `name` would replace a
/// built-in.
pub fn is_reserved(name: &str) -> bool {
    BUILTINS.contains(&name) || name.starts_with("INPUT_")
}

/// Environment variables a shell command may use without the script
/// defining them; they are left for the shell.
const SHELL_ENV: &[&str] = &["PATH", "PWD", "LD_LIBRARY_PATH", "LANG", "DISPLAY", "TERM"];

/// Whether an undefined `$name` in a shell command belongs to the shell:
/// lower-case names (loop variables, locals) and common environment
/// variables. Upper-case unknown names stay errors, so a typo such as
/// `$GAMEDRI` never expands to an empty path inside `rm -rf`.
pub(crate) fn is_shell_name(name: &str) -> bool {
    SHELL_ENV.contains(&name)
        || name.starts_with("XDG_")
        || name.chars().all(|c| !c.is_ascii_uppercase())
}

/// A piece of scanned text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece {
    Lit(String),
    /// A variable name, with `-` turned into `_`.
    Var(String),
}

fn ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn ident_char(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

/// Split `text` into literal text and variable references. `known` lets an
/// unbraced name continue over `-` (Lutris file ids may contain dashes) when
/// the longer name exists.
pub(crate) fn scan(text: &str, known: &dyn Fn(&str) -> bool) -> Vec<Piece> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    let flush = |lit: &mut String, out: &mut Vec<Piece>| {
        if !lit.is_empty() {
            out.push(Piece::Lit(std::mem::take(lit)));
        }
    };
    while i < b.len() {
        if b[i] != b'$' {
            let next = text[i..].find('$').map_or(b.len(), |p| i + p);
            lit.push_str(&text[i..next]);
            i = next;
            continue;
        }
        match b.get(i + 1) {
            Some(b'$') => {
                lit.push('$');
                i += 2;
            }
            Some(b'{') => {
                let braced = text[i + 2..].find('}').and_then(|end| {
                    let name = &text[i + 2..i + 2 + end];
                    let ok = name.as_bytes().first().is_some_and(|&c| ident_start(c))
                        && name.bytes().all(|c| ident_char(c) || c == b'-');
                    ok.then(|| (name.replace('-', "_"), i + 3 + end))
                });
                match braced {
                    Some((name, next)) => {
                        flush(&mut lit, &mut out);
                        out.push(Piece::Var(name));
                        i = next;
                    }
                    None => {
                        lit.push('$');
                        i += 1;
                    }
                }
            }
            Some(&c) if ident_start(c) => {
                let start = i + 1;
                let mut end = start;
                while end < b.len() && ident_char(b[end]) {
                    end += 1;
                }
                let mut best = end;
                let mut k = end;
                while k + 1 < b.len() && b[k] == b'-' && ident_char(b[k + 1]) {
                    k += 1;
                    while k < b.len() && ident_char(b[k]) {
                        k += 1;
                    }
                    if known(&text[start..k].replace('-', "_")) {
                        best = k;
                    }
                }
                flush(&mut lit, &mut out);
                out.push(Piece::Var(text[start..best].replace('-', "_")));
                i = best;
            }
            _ => {
                lit.push('$');
                i += 1;
            }
        }
    }
    flush(&mut lit, &mut out);
    out
}

/// Names referenced by `text`.
pub(crate) fn references(text: &str, known: &dyn Fn(&str) -> bool) -> Vec<String> {
    scan(text, known)
        .into_iter()
        .filter_map(|p| match p {
            Piece::Var(n) => Some(n),
            Piece::Lit(_) => None,
        })
        .collect()
}

/// How undefined names are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Every name must exist.
    Strict,
    /// Shell command lines: [`is_shell_name`] names are left for the shell.
    Shell,
    /// File contents (`substitute_vars`): names the script does not know are
    /// left as they are, since the file is not script text.
    Lenient,
}

/// Values at run time. `None` is a known name without a value yet (`$DISC`
/// before `insert_disc`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Vars {
    values: HashMap<String, Option<String>>,
    /// File ids, so a parameter that is exactly a file id means its path.
    file_ids: Vec<String>,
}

impl Vars {
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        self.values.insert(name.to_string(), Some(value.into()));
    }

    pub fn declare(&mut self, name: &str) {
        self.values.entry(name.to_string()).or_insert(None);
    }

    pub fn set_file(&mut self, id: &str, path: &str) {
        if !self.file_ids.iter().any(|f| f == id) {
            self.file_ids.push(id.to_string());
        }
        self.set(id, path);
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(|v| v.as_deref())
    }

    pub fn is_known(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// `text` with every variable replaced.
    pub fn substitute(
        &self,
        text: &str,
        mode: Mode,
        location: &str,
    ) -> Result<String, ScriptError> {
        let known = |n: &str| self.is_known(n);
        let mut out = String::with_capacity(text.len());
        for piece in scan(text, &known) {
            match piece {
                Piece::Lit(s) => out.push_str(&s),
                Piece::Var(name) => match self.values.get(&name) {
                    Some(Some(v)) => out.push_str(v),
                    Some(None) => {
                        return Err(ScriptError::UnsetVariable {
                            location: location.into(),
                            name,
                        });
                    }
                    None if mode == Mode::Lenient
                        || (mode == Mode::Shell && is_shell_name(&name)) =>
                    {
                        out.push('$');
                        out.push_str(&name);
                    }
                    None => {
                        return Err(ScriptError::UnknownVariable {
                            location: location.into(),
                            name,
                        });
                    }
                },
            }
        }
        Ok(out)
    }

    /// Like [`Vars::substitute`], but a parameter that is exactly a file id
    /// (Lutris lets scripts write `file: installer`) means that file's path.
    pub fn substitute_ref(&self, text: &str, location: &str) -> Result<String, ScriptError> {
        let id = text.trim().replace('-', "_");
        if self.file_ids.contains(&id)
            && let Some(path) = self.get(&id)
        {
            return Ok(path.to_string());
        }
        self.substitute(text, Mode::Strict, location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn scans_both_forms_and_escapes() {
        assert_eq!(
            scan("$GAMEDIR/a ${CACHE}b $$HOME $1 $(x) $", &none),
            vec![
                Piece::Var("GAMEDIR".into()),
                Piece::Lit("/a ".into()),
                Piece::Var("CACHE".into()),
                Piece::Lit("b $HOME $1 $(x) $".into()),
            ]
        );
    }

    #[test]
    fn dashed_names_extend_only_when_known() {
        let known = |n: &str| n == "setup_exe";
        assert_eq!(
            references("$setup-exe and $other-thing", &known),
            vec!["setup_exe".to_string(), "other".to_string()]
        );
        assert_eq!(references("${a-b}", &none), vec!["a_b".to_string()]);
    }

    #[test]
    fn strict_unknown_and_unset_are_errors() {
        let mut v = Vars::default();
        v.set("GAMEDIR", "/g");
        v.declare("DISC");
        assert_eq!(
            v.substitute("$GAMEDIR/x", Mode::Strict, "t").unwrap(),
            "/g/x"
        );
        assert!(matches!(
            v.substitute("$GAMEDRI/x", Mode::Strict, "t"),
            Err(ScriptError::UnknownVariable { name, .. }) if name == "GAMEDRI"
        ));
        assert!(matches!(
            v.substitute("$DISC/x", Mode::Strict, "t"),
            Err(ScriptError::UnsetVariable { .. })
        ));
    }

    #[test]
    fn shell_and_lenient_modes_keep_foreign_names() {
        let mut v = Vars::default();
        v.set("GAMEDIR", "/g");
        assert_eq!(
            v.substitute("cd $GAMEDIR && echo $f $PATH", Mode::Shell, "t")
                .unwrap(),
            "cd /g && echo $f $PATH"
        );
        assert!(v.substitute("rm -rf $GAMEDRI/", Mode::Shell, "t").is_err());
        assert_eq!(
            v.substitute("X=$FOO", Mode::Lenient, "t").unwrap(),
            "X=$FOO"
        );
    }

    #[test]
    fn bare_file_id_means_its_path() {
        let mut v = Vars::default();
        v.set_file("setup_exe", "/cache/setup.exe");
        assert_eq!(
            v.substitute_ref("setup-exe", "t").unwrap(),
            "/cache/setup.exe"
        );
        assert_eq!(
            v.substitute_ref("$setup_exe", "t").unwrap(),
            "/cache/setup.exe"
        );
    }
}
