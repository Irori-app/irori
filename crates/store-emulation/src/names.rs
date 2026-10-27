//! Clean titles from ROM file names, locally. No-Intro and Redump names
//! carry their details in brackets — `Chrono Trigger (USA) (Rev 1) [!]` —
//! which become a clean title plus badges: the region and the revision. Disc
//! numbers group the discs of one game.

use once_cell::sync::Lazy;
use regex::Regex;

/// What a ROM file's name says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomName {
    pub title: String,
    /// Region, revision and release kind badges, in name order.
    pub tags: Vec<String>,
    /// Disc number of a multi-disc game.
    pub disc: Option<u32>,
    /// The name without its disc marker, to group the discs of one game.
    pub group: String,
}

static BRACKETS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\s*[\(\[]([^\)\]]*)[\)\]]").expect("valid regex"));
static DISC: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?:disc|disk|cd)\s*([0-9]+|[a-d])(?:\s+of\s+[0-9]+)?$").expect("valid regex")
});
static REVISION: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?:rev\s*[0-9a-z.]+|v[0-9]+(?:\.[0-9a-z]+)*)$").expect("valid regex")
});

/// Regions and languages No-Intro and Redump use in names.
const REGIONS: &[&str] = &[
    "World",
    "USA",
    "Europe",
    "Japan",
    "Asia",
    "Australia",
    "Brazil",
    "Canada",
    "China",
    "France",
    "Germany",
    "Hong Kong",
    "Italy",
    "Korea",
    "Netherlands",
    "Russia",
    "Spain",
    "Sweden",
    "Taiwan",
    "UK",
    "Scandinavia",
    "Latin America",
];

/// Release kinds worth a badge.
const KINDS: &[&str] = &["Beta", "Proto", "Demo", "Sample", "Unl", "Pirate", "Hack"];

/// Parse the stem of a ROM file name (no extension).
pub fn parse(stem: &str) -> RomName {
    let mut tags = Vec::new();
    let mut disc = None;
    let mut group = String::with_capacity(stem.len());
    let mut last = 0;
    for m in BRACKETS.captures_iter(stem) {
        let whole = m.get(0).expect("match");
        let inner = m[1].trim();
        if let Some(d) = DISC.captures(inner) {
            let n = &d[1];
            disc = n.parse().ok().or_else(|| {
                n.chars()
                    .next()
                    .map(|c| (c.to_ascii_lowercase() as u32) - 96)
            });
            group.push_str(&stem[last..whole.start()]);
            last = whole.end();
            continue;
        }
        if inner
            .split(',')
            .map(str::trim)
            .any(|p| REGIONS.iter().any(|r| r.eq_ignore_ascii_case(p)))
        {
            tags.push(inner.to_string());
        } else if REVISION.is_match(inner) {
            tags.push(capitalize(inner));
        } else if let Some(kind) = KINDS.iter().find(|k| inner.eq_ignore_ascii_case(k)) {
            tags.push((*kind).to_string());
        }
    }
    group.push_str(&stem[last..]);
    let base = BRACKETS.replace_all(stem, "");
    RomName {
        title: tidy_title(&base),
        tags,
        disc,
        group: group.trim().to_lowercase(),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Underscores and dots as spaces, collapsed spaces, and a trailing article
/// moved to the front ("Legend of Zelda, The - …" → "The Legend of Zelda - …").
fn tidy_title(raw: &str) -> String {
    let spaced: String = if raw.contains(' ') {
        raw.to_string()
    } else {
        raw.replace(['_', '.'], " ")
    };
    let title = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let (head, tail) = match title.split_once(" - ") {
        Some((h, t)) => (h.to_string(), Some(t.to_string())),
        None => (title.clone(), None),
    };
    let head = ["The", "A", "An"]
        .iter()
        .find_map(|article| {
            head.strip_suffix(&format!(", {article}"))
                .map(|rest| format!("{article} {rest}"))
        })
        .unwrap_or(head);
    match tail {
        Some(t) => format!("{head} - {t}"),
        None => head,
    }
}

/// The libretro-thumbnails file name of a title: these characters become
/// underscores there.
pub fn thumbnail_name(stem: &str) -> String {
    stem.chars()
        .map(|c| if "&*/:`<>?\\|\"".contains(c) { '_' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_no_intro_names() {
        let n = parse("Chrono Trigger (USA) (Rev 1) [!]");
        assert_eq!(n.title, "Chrono Trigger");
        assert_eq!(n.tags, ["USA", "Rev 1"]);
        assert_eq!(n.disc, None);

        let n = parse("Legend of Zelda, The - A Link to the Past (USA, Europe)");
        assert_eq!(n.title, "The Legend of Zelda - A Link to the Past");
        assert_eq!(n.tags, ["USA, Europe"]);

        let n = parse("super_mario_world");
        assert_eq!(n.title, "super mario world");
        let n = parse("Pokemon - Red Version (USA, Europe) (SGB Enhanced)");
        assert_eq!(n.title, "Pokemon - Red Version");
        let n = parse("Star Fox 2 (Japan) (Proto)");
        assert_eq!(n.tags, ["Japan", "Proto"]);
    }

    #[test]
    fn finds_discs_and_groups_them() {
        let a = parse("Final Fantasy VII (USA) (Disc 1)");
        let b = parse("Final Fantasy VII (USA) (Disc 2)");
        assert_eq!(a.disc, Some(1));
        assert_eq!(b.disc, Some(2));
        assert_eq!(a.group, b.group);
        assert_eq!(a.title, "Final Fantasy VII");
        let c = parse("Riven (Disk 3 of 5)");
        assert_eq!(c.disc, Some(3));
        let d = parse("Game (Disc B)");
        assert_eq!(d.disc, Some(2));
    }

    #[test]
    fn thumbnail_names_replace_reserved_characters() {
        assert_eq!(thumbnail_name("Q*bert: Qubes"), "Q_bert_ Qubes");
    }
}
