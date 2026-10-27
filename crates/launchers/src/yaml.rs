//! A small reader for the YAML subset in Ubisoft Connect's product
//! configurations: block mappings, block sequences (including `- key: value`
//! items), plain and quoted scalars, and block scalars (`|`, `>`), which are
//! kept but never needed. Flow collections stay as their raw text.
//!
//! It is lenient by design: a line it can't place is skipped, never an error,
//! because one odd line must not hide a game.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Null,
    Scalar(String),
    Map(Vec<(String, Node)>),
    Seq(Vec<Node>),
}

impl Node {
    /// The value of a mapping key (exact match, as YAML keys are).
    pub(crate) fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The first item of a sequence.
    pub(crate) fn first(&self) -> Option<&Node> {
        match self {
            Node::Seq(items) => items.first(),
            _ => None,
        }
    }

    /// Follows mapping keys.
    pub(crate) fn path(&self, keys: &[&str]) -> Option<&Node> {
        keys.iter().try_fold(self, |node, key| node.get(key))
    }

    /// A scalar's text.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Node::Scalar(s) => Some(s),
            _ => None,
        }
    }
}

struct Line {
    indent: usize,
    text: String,
}

/// Parses a YAML document; anything unreadable becomes [`Node::Null`].
pub(crate) fn parse(text: &str) -> Node {
    let lines = text
        .lines()
        .filter_map(|raw| {
            // Ubisoft's files sometimes indent with tabs, which YAML forbids.
            let raw = raw.trim_end_matches('\r').replace('\t', " ");
            let text = raw.trim_start();
            if text.is_empty() || text.starts_with('#') || text == "---" || text == "..." {
                return None;
            }
            Some(Line {
                indent: raw.len() - text.len(),
                text: text.trim_end().to_string(),
            })
        })
        .collect();
    let mut parser = Parser { lines, pos: 0 };
    parser.node(0)
}

struct Parser {
    lines: Vec<Line>,
    pos: usize,
}

impl Parser {
    fn node(&mut self, min_indent: usize) -> Node {
        let Some(line) = self.lines.get(self.pos) else {
            return Node::Null;
        };
        if line.indent < min_indent {
            return Node::Null;
        }
        let indent = line.indent;
        if is_seq_item(&line.text) {
            self.seq(indent)
        } else {
            self.map(indent)
        }
    }

    fn seq(&mut self, indent: usize) -> Node {
        let mut items = Vec::new();
        while let Some(line) = self.lines.get(self.pos) {
            if line.indent != indent || !is_seq_item(&line.text) {
                break;
            }
            let rest = line.text[1..].trim_start().to_string();
            if rest.is_empty() {
                self.pos += 1;
                items.push(self.child(indent));
            } else if is_seq_item(&rest) || split_key(&rest).is_some() {
                // `- key: value` opens a mapping whose keys line up with `key`.
                let nested = indent + line.text.len() - rest.len();
                self.lines[self.pos] = Line {
                    indent: nested,
                    text: rest,
                };
                items.push(self.node(nested));
            } else {
                self.pos += 1;
                items.push(Node::Scalar(scalar(&rest)));
            }
        }
        Node::Seq(items)
    }

    fn map(&mut self, indent: usize) -> Node {
        let mut entries = Vec::new();
        while let Some(line) = self.lines.get(self.pos) {
            if line.indent < indent || (line.indent == indent && is_seq_item(&line.text)) {
                break;
            }
            let split = (line.indent == indent)
                .then(|| split_key(&line.text))
                .flatten();
            self.pos += 1;
            let Some((key, value)) = split else {
                // A stray deeper line or a bare scalar: nothing to attach it to.
                continue;
            };
            let node = if value.is_empty() {
                match self.lines.get(self.pos) {
                    Some(next) if next.indent > indent => self.child(indent),
                    // YAML lets a sequence sit at its key's indentation.
                    Some(next) if next.indent == indent && is_seq_item(&next.text) => {
                        self.seq(indent)
                    }
                    _ => Node::Null,
                }
            } else if is_block_scalar(&value) {
                let mut parts = Vec::new();
                while let Some(next) = self.lines.get(self.pos) {
                    if next.indent <= indent {
                        break;
                    }
                    parts.push(next.text.clone());
                    self.pos += 1;
                }
                let sep = if value.starts_with('|') { "\n" } else { " " };
                Node::Scalar(parts.join(sep))
            } else {
                Node::Scalar(scalar(&value))
            };
            entries.push((key, node));
        }
        Node::Map(entries)
    }

    fn child(&mut self, parent_indent: usize) -> Node {
        match self.lines.get(self.pos) {
            Some(line) if line.indent > parent_indent => {
                let indent = line.indent;
                self.node(indent)
            }
            _ => Node::Null,
        }
    }
}

fn is_seq_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn is_block_scalar(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some('|' | '>'))
        && chars.all(|c| c == '-' || c == '+' || c.is_ascii_digit())
}

/// `key: value` → `(key, value)`; `None` when the line isn't a mapping entry.
fn split_key(text: &str) -> Option<(String, String)> {
    if text.starts_with(['[', '{']) {
        return None;
    }
    let (key, after) = match text.chars().next()? {
        q @ ('"' | '\'') => {
            let end = text[1..].find(q)? + 1;
            (scalar(&text[..=end]), text[end + 1..].trim_start())
        }
        _ => {
            let bytes = text.as_bytes();
            let colon = (0..bytes.len())
                .find(|&i| bytes[i] == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' '))?;
            (text[..colon].trim_end().to_string(), &text[colon..])
        }
    };
    let value = after.strip_prefix(':')?;
    if !(value.is_empty() || value.starts_with(' ')) {
        return None;
    }
    Some((key, value.trim().to_string()))
}

/// A scalar's text with its quotes and escapes resolved.
fn scalar(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        return raw[1..raw.len() - 1].replace("''", "'");
    }
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        let mut out = String::new();
        let mut chars = raw[1..raw.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => {}
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        }
        return out;
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_maps_sequences_and_scalars() {
        let doc = parse(
            "version: 2.0\n\
             root:\n\
             \x20 name: Assassin's Creed® Origins\n\
             \x20 quoted: \"C:\\\\Games\\\\x\"\n\
             \x20 single: 'it''s'\n\
             \x20 start_game:\n\
             \x20   online:\n\
             \x20     executables:\n\
             \x20     - path:\n\
             \x20         relative: ACOrigins.exe\n\
             \x20       working_directory:\n\
             \x20         register: HKEY_LOCAL_MACHINE\\SOFTWARE\\Ubisoft\\Launcher\\Installs\\3539\\InstallDir\n\
             \x20     - path:\n\
             \x20         relative: Other.exe\n\
             \x20 description: |\n\
             \x20   line one\n\
             \x20   line two\n\
             \x20 tags: [a, b]\n\
             \x20 list:\n\
             \x20   - plain\n\
             \x20   -\n\
             \x20     nested: yes\n\
             localizations:\n\
             \x20 default:\n\
             \x20   l1: \"Far Cry® 5\"\n",
        );
        assert_eq!(doc.path(&["version"]).and_then(Node::as_str), Some("2.0"));
        let root = doc.get("root").expect("root");
        assert_eq!(
            root.get("name").and_then(Node::as_str),
            Some("Assassin's Creed® Origins")
        );
        assert_eq!(
            root.get("quoted").and_then(Node::as_str),
            Some("C:\\Games\\x")
        );
        assert_eq!(root.get("single").and_then(Node::as_str), Some("it's"));
        let exes = root
            .path(&["start_game", "online", "executables"])
            .expect("executables");
        let Node::Seq(items) = exes else {
            panic!("not a sequence: {exes:?}")
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].path(&["path", "relative"]).and_then(Node::as_str),
            Some("ACOrigins.exe")
        );
        assert_eq!(
            items[0]
                .path(&["working_directory", "register"])
                .and_then(Node::as_str),
            Some(r"HKEY_LOCAL_MACHINE\SOFTWARE\Ubisoft\Launcher\Installs\3539\InstallDir")
        );
        assert_eq!(
            root.get("description").and_then(Node::as_str),
            Some("line one\nline two")
        );
        assert_eq!(root.get("tags").and_then(Node::as_str), Some("[a, b]"));
        assert_eq!(
            root.get("list"),
            Some(&Node::Seq(vec![
                Node::Scalar("plain".into()),
                Node::Map(vec![("nested".into(), Node::Scalar("yes".into()))]),
            ]))
        );
        assert_eq!(
            doc.path(&["localizations", "default", "l1"])
                .and_then(Node::as_str),
            Some("Far Cry® 5")
        );
    }

    #[test]
    fn tabs_and_stray_lines_do_not_break_parsing() {
        let doc = parse("root:\n\tname: Tabbed\n\t\tstray deeper line\n\tspace_id: abc\n");
        assert_eq!(
            doc.path(&["root", "name"]).and_then(Node::as_str),
            Some("Tabbed")
        );
        assert_eq!(
            doc.path(&["root", "space_id"]).and_then(Node::as_str),
            Some("abc")
        );
    }
}
