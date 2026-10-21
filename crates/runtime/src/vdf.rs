//! Minimal parser for Valve's text KeyValues format (`libraryfolders.vdf`,
//! `config.vdf`) and Steam library discovery on top of it.

use std::path::{Path, PathBuf};

/// A parsed KeyValues node. Objects keep their key order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vdf {
    Str(String),
    Obj(Vec<(String, Vdf)>),
}

impl Vdf {
    /// First child with `key` (case-insensitive, as Steam treats keys).
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Obj(items) => items
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Obj(_) => None,
        }
    }

    /// Children of an object, in file order.
    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Obj(items) => items,
            Vdf::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
            }
            '{' => {
                chars.next();
                out.push(Token::Open);
            }
            '}' => {
                chars.next();
                out.push(Token::Close);
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => break,
                        },
                        c => s.push(c),
                    }
                }
                out.push(Token::Str(s));
            }
            '[' => {
                // Conditional suffixes like `[$WIN32]` are ignored.
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                }
            }
            _ => {
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                out.push(Token::Str(s));
            }
        }
    }
    out
}

/// Parse a text VDF document into its root object. Malformed trailing input
/// is ignored rather than rejected, as Steam itself tolerates it.
pub fn parse(input: &str) -> Vdf {
    let tokens = tokenize(input);
    let mut pos = 0;
    Vdf::Obj(parse_object(&tokens, &mut pos))
}

fn parse_object(tokens: &[Token], pos: &mut usize) -> Vec<(String, Vdf)> {
    let mut items = Vec::new();
    while *pos < tokens.len() {
        match &tokens[*pos] {
            Token::Close => {
                *pos += 1;
                break;
            }
            Token::Open => {
                *pos += 1;
            }
            Token::Str(key) => {
                *pos += 1;
                match tokens.get(*pos) {
                    Some(Token::Str(value)) => {
                        *pos += 1;
                        items.push((key.clone(), Vdf::Str(value.clone())));
                    }
                    Some(Token::Open) => {
                        *pos += 1;
                        let child = parse_object(tokens, pos);
                        items.push((key.clone(), Vdf::Obj(child)));
                    }
                    _ => break,
                }
            }
        }
    }
    items
}

/// Library folder paths listed in a `libraryfolders.vdf` document, in file
/// order. Supports both the current format (`"0" { "path" "…" }`) and the old
/// one (`"1" "/path"`).
pub fn library_folders(doc: &Vdf) -> Vec<PathBuf> {
    let Some(root) = doc
        .get("libraryfolders")
        .or_else(|| doc.get("LibraryFolders"))
    else {
        return Vec::new();
    };
    root.entries()
        .iter()
        .filter(|(k, _)| k.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|(_, v)| match v {
            Vdf::Str(path) => Some(PathBuf::from(path)),
            Vdf::Obj(_) => v.get("path").and_then(Vdf::as_str).map(PathBuf::from),
        })
        .collect()
}

/// Steam library roots: `/usr/share/steam` (system Steam packages), the Steam
/// installation itself, and every folder from its `libraryfolders.vdf`.
/// Only existing directories are returned, de-duplicated by canonical path.
pub fn steam_libraries(steam_path: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("/usr/share/steam"), steam_path.to_path_buf()];
    let vdf = steam_path.join("steamapps").join("libraryfolders.vdf");
    match std::fs::read_to_string(&vdf) {
        Ok(text) => candidates.extend(library_folders(&parse(&text))),
        Err(_) => tracing::debug!("no Steam library list at {}", vdf.display()),
    }
    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|p| p.is_dir())
        .filter(|p| seen.insert(crate::locations::canonical(p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/u/.local/share/Steam"
		"label"		""
		"apps"
		{
			"228980"		"128606066"
		}
	}
	// second drive
	"1"
	{
		"path"		"/mnt/games/Steam\\Library"
	}
}
"#;

    #[test]
    fn parses_library_folders() {
        let doc = parse(LIBRARY);
        assert_eq!(
            library_folders(&doc),
            vec![
                PathBuf::from("/home/u/.local/share/Steam"),
                PathBuf::from("/mnt/games/Steam\\Library")
            ]
        );
        let apps = doc
            .get("libraryfolders")
            .and_then(|l| l.get("0"))
            .and_then(|l| l.get("apps"))
            .unwrap();
        assert_eq!(apps.get("228980").and_then(Vdf::as_str), Some("128606066"));
    }

    #[test]
    fn parses_old_format_and_unquoted_tokens() {
        let doc = parse(
            "LibraryFolders { \"TimeNextStatsReport\" \"1\" \"1\" \"/data/steam\" key value }",
        );
        assert_eq!(library_folders(&doc), vec![PathBuf::from("/data/steam")]);
        assert_eq!(
            doc.get("libraryfolders")
                .and_then(|l| l.get("key"))
                .and_then(Vdf::as_str),
            Some("value")
        );
    }

    #[test]
    fn tolerates_garbage() {
        assert_eq!(library_folders(&parse("")), Vec::<PathBuf>::new());
        assert_eq!(
            library_folders(&parse("\"libraryfolders\" { \"0\" {")),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn libraries_are_existing_and_unique() {
        let dir = tempfile::tempdir().unwrap();
        let steam = dir.path().join("steam");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        let other = dir.path().join("lib2");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} \"2\" {{ \"path\" \"/nonexistent/x\" }} }}",
                steam.display(),
                other.display()
            ),
        )
        .unwrap();
        let libs = steam_libraries(&steam);
        let ours: Vec<_> = libs.iter().filter(|p| p.starts_with(dir.path())).collect();
        assert_eq!(ours, vec![&steam, &other]);
    }
}
