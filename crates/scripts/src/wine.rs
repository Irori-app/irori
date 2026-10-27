//! Mapping the Wine build a script names to one Ember has. Scripts pin
//! Lutris build names that disappear from its server; instead of failing
//! (or silently using another build), the caller gets a typed answer it can
//! show: the exact build, a close one with the same family and version, or
//! none.

/// How a script's Wine version maps to the builds available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WineMatch {
    /// The same name.
    Exact(String),
    /// A build of the same family and version under another name
    /// (`wine-ge-8-26` → `GE-Proton8-26`), or failing that the nearest
    /// version of the same family and major version.
    Close {
        requested: String,
        available: String,
    },
    /// Nothing comparable; use the default build and say so.
    NotAvailable { requested: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// GloriousEggroll builds (`lutris-ge-…`, `wine-ge-…`, `GE-Proton…`).
    Ge,
    Proton,
    Lutris,
    Staging,
    Tkg,
    Wine,
    Other,
}

const ARCH: &[&str] = &["x86_64", "amd64", "i386", "i686", "win64", "win32", "x86"];

fn describe(name: &str) -> (Family, Vec<u32>) {
    let mut lower = name.to_ascii_lowercase();
    for a in ARCH {
        lower = lower.replace(a, "-");
    }
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut digits = false;
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            if !cur.is_empty() && c.is_ascii_digit() != digits {
                tokens.push(std::mem::take(&mut cur));
            }
            digits = c.is_ascii_digit();
            cur.push(c);
        } else if !cur.is_empty() {
            tokens.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let has = |t: &str| tokens.iter().any(|x| x == t);
    let family = if has("ge") {
        Family::Ge
    } else if has("proton") {
        Family::Proton
    } else if has("lutris") {
        Family::Lutris
    } else if has("staging") {
        Family::Staging
    } else if has("tkg") {
        Family::Tkg
    } else if has("wine") {
        Family::Wine
    } else {
        Family::Other
    };
    let numbers = tokens.iter().filter_map(|t| t.parse().ok()).collect();
    (family, numbers)
}

/// Map `requested` (a script's `wine: version`) to one of `available`.
pub fn match_wine_version<S: AsRef<str>>(requested: &str, available: &[S]) -> WineMatch {
    let requested = requested.trim();
    if let Some(exact) = available
        .iter()
        .map(AsRef::as_ref)
        .find(|a| a.eq_ignore_ascii_case(requested))
    {
        return WineMatch::Exact(exact.to_string());
    }
    let (family, numbers) = describe(requested);
    let close = |available: &str| WineMatch::Close {
        requested: requested.to_string(),
        available: available.to_string(),
    };
    if numbers.is_empty() {
        return WineMatch::NotAvailable {
            requested: requested.to_string(),
        };
    }
    let same_family: Vec<(&str, Vec<u32>)> = available
        .iter()
        .map(AsRef::as_ref)
        .filter_map(|a| {
            let (f, n) = describe(a);
            (f == family).then_some((a, n))
        })
        .collect();
    if let Some((a, _)) = same_family.iter().find(|(_, n)| *n == numbers) {
        return close(a);
    }
    // Nearest build with the same major version: most leading numbers in
    // common, then the newest.
    let common = |n: &[u32]| n.iter().zip(&numbers).take_while(|(a, b)| a == b).count();
    let nearest = same_family
        .iter()
        .filter(|(_, n)| n.first() == numbers.first())
        .max_by(|(_, a), (_, b)| common(a).cmp(&common(b)).then_with(|| a.cmp(b)));
    match nearest {
        Some((a, _)) => close(a),
        None => WineMatch::NotAvailable {
            requested: requested.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_names_match_ignoring_case() {
        assert_eq!(
            match_wine_version("GE-Proton8-26", &["ge-proton8-26"]),
            WineMatch::Exact("ge-proton8-26".into())
        );
    }

    #[test]
    fn ge_builds_match_across_naming_schemes() {
        let available = ["wine-9.0", "GE-Proton8-26", "GE-Proton9-1"];
        assert_eq!(
            match_wine_version("wine-ge-8-26", &available),
            WineMatch::Close {
                requested: "wine-ge-8-26".into(),
                available: "GE-Proton8-26".into()
            }
        );
        assert_eq!(
            match_wine_version("lutris-GE-Proton8-26-x86_64", &available),
            WineMatch::Close {
                requested: "lutris-GE-Proton8-26-x86_64".into(),
                available: "GE-Proton8-26".into()
            }
        );
    }

    #[test]
    fn nearest_version_of_the_same_major() {
        let available = ["GE-Proton8-25", "GE-Proton8-32", "GE-Proton9-1"];
        assert_eq!(
            match_wine_version("wine-ge-8-26", &available),
            WineMatch::Close {
                requested: "wine-ge-8-26".into(),
                available: "GE-Proton8-32".into()
            }
        );
    }

    #[test]
    fn missing_lutris_builds_are_not_available() {
        let available = ["GE-Proton8-26", "wine-9.0", "Proton 9.0"];
        assert_eq!(
            match_wine_version("lutris-7.2-2-x86_64", &available),
            WineMatch::NotAvailable {
                requested: "lutris-7.2-2-x86_64".into()
            }
        );
        assert!(matches!(
            match_wine_version("lutris-7.2-2-x86_64", &[] as &[&str]),
            WineMatch::NotAvailable { .. }
        ));
    }
}
