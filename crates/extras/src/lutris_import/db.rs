//! Reads Lutris's `pga.db` without touching it: the `games` table and
//! category membership. The file is opened read-only and immutable, so a
//! running Lutris is never locked out and the file is never modified, not
//! even by a journal or WAL checkpoint.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OpenFlags};

/// One row of the `games` table. Every column but `id` may be missing in
/// old schemas (Lutris migrations only add columns) and reads as `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DbGame {
    pub id: i64,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub runner: Option<String>,
    pub platform: Option<String>,
    pub directory: Option<String>,
    pub configpath: Option<String>,
    pub installed: bool,
    pub installed_at: Option<i64>,
    pub lastplayed: Option<i64>,
    /// Hours, fractional.
    pub playtime: Option<f64>,
    pub service: Option<String>,
    pub service_id: Option<String>,
    /// The legacy `hidden` column; newer versions use the `.hidden`
    /// category instead.
    pub hidden: bool,
}

/// What the importer needs from the database.
#[derive(Debug, Default)]
pub(crate) struct DbData {
    pub games: Vec<DbGame>,
    /// Category names by game id.
    pub categories: HashMap<i64, Vec<String>>,
}

/// Columns read from `games`, in the order [`row_to_game`] expects.
const COLUMNS: [&str; 14] = [
    "id",
    "name",
    "slug",
    "runner",
    "platform",
    "directory",
    "configpath",
    "installed",
    "installed_at",
    "lastplayed",
    "playtime",
    "service",
    "service_id",
    "hidden",
];

/// Read every game and its categories from the database at `path`.
pub(crate) fn read(path: &Path) -> Result<DbData> {
    let conn = open(path)?;
    let tables = names(&conn, "SELECT name FROM sqlite_master WHERE type = 'table'")
        .context("Could not read the Lutris game database")?;
    if !tables.contains("games") {
        anyhow::bail!("{} is not a Lutris game database", path.display());
    }

    let present = names(&conn, "SELECT name FROM pragma_table_info('games')")
        .context("Could not read the Lutris game database")?;
    // Column names come from `COLUMNS`, never from the file, so formatting
    // them into the query is safe.
    let select: Vec<String> = COLUMNS
        .iter()
        .map(|c| match (*c, present.contains(*c)) {
            (_, true) => format!("\"{c}\""),
            ("id", false) => "rowid".to_string(),
            (_, false) => "NULL".to_string(),
        })
        .collect();
    let sql = format!("SELECT {} FROM games ORDER BY 1", select.join(", "));
    let mut stmt = conn
        .prepare(&sql)
        .context("Could not read the Lutris game list")?;
    let games = stmt
        .query_map([], |row| {
            let mut values = Vec::with_capacity(COLUMNS.len());
            for i in 0..COLUMNS.len() {
                values.push(row.get::<_, Sql>(i)?);
            }
            Ok(row_to_game(&values))
        })
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .context("Could not read the Lutris game list")?;

    let mut categories: HashMap<i64, Vec<String>> = HashMap::new();
    if tables.contains("categories") && tables.contains("games_categories") {
        let mut stmt = conn
            .prepare(
                "SELECT gc.game_id, c.name FROM games_categories gc \
                 JOIN categories c ON c.id = gc.category_id \
                 ORDER BY gc.game_id, c.name",
            )
            .context("Could not read the Lutris categories")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, Sql>(0)?, row.get::<_, Sql>(1)?)))
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .context("Could not read the Lutris categories")?;
        for (game, name) in rows {
            if let (Some(game), Some(name)) = (int(&game), text(&name)) {
                let list = categories.entry(game).or_default();
                if !list.contains(&name) {
                    list.push(name);
                }
            }
        }
    }
    Ok(DbData { games, categories })
}

fn open(path: &Path) -> Result<Connection> {
    let absolute = std::path::absolute(path)
        .with_context(|| format!("Could not find the Lutris database {}", path.display()))?;
    let mut uri = url::Url::from_file_path(&absolute)
        .map_err(|()| anyhow!("Could not open the Lutris database {}", path.display()))?;
    // `immutable=1`: no locks, no journal, no WAL; SQLite treats the file as
    // read-only media.
    uri.set_query(Some("immutable=1"));
    let conn = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Could not open the Lutris database {}", path.display()))?;
    conn.pragma_update(None, "query_only", true)
        .context("Could not open the Lutris database read-only")?;
    Ok(conn)
}

fn names(conn: &Connection, sql: &str) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

fn row_to_game(v: &[Sql]) -> DbGame {
    DbGame {
        id: int(&v[0]).unwrap_or_default(),
        name: text(&v[1]),
        slug: text(&v[2]),
        runner: text(&v[3]),
        platform: text(&v[4]),
        directory: text(&v[5]),
        configpath: text(&v[6]),
        installed: int(&v[7]).is_some_and(|i| i != 0),
        installed_at: int(&v[8]).filter(|t| *t > 0),
        lastplayed: int(&v[9]).filter(|t| *t > 0),
        playtime: real(&v[10]),
        service: text(&v[11]),
        service_id: text(&v[12]),
        hidden: int(&v[13]).is_some_and(|i| i != 0),
    }
}

/// SQLite is dynamically typed and Lutris has written numbers as text in some
/// versions, so every column is coerced instead of trusting its type.
fn int(v: &Sql) -> Option<i64> {
    match v {
        Sql::Integer(i) => Some(*i),
        Sql::Real(f) if f.is_finite() => Some(*f as i64),
        Sql::Text(s) => {
            let s = s.trim();
            s.parse()
                .ok()
                .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
        }
        _ => None,
    }
}

fn real(v: &Sql) -> Option<f64> {
    match v {
        Sql::Integer(i) => Some(*i as f64),
        Sql::Real(f) => Some(*f),
        Sql::Text(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

fn text(v: &Sql) -> Option<String> {
    match v {
        Sql::Text(s) => Some(s.trim().to_string()),
        Sql::Integer(i) => Some(i.to_string()),
        Sql::Real(f) => Some(f.to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}
