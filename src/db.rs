use std::{fs, path::Path};

use rusqlite::{Connection, params};

use crate::error::Result;

const CURRENT_SCHEMA_VERSION: i32 = 1;

pub(crate) fn open_connection(db_file: &Path) -> std::result::Result<Connection, rusqlite::Error> {
    let conn = Connection::open(db_file)?;
    conn.execute("PRAGMA foreign_keys = ON;", [])?;
    Ok(conn)
}

pub fn open_and_prepare_db(dbpath: &Path) -> Result<Connection> {
    let db_file = dbpath.join("fonts.db");

    if db_file.exists() {
        match check_schema_version(&db_file) {
            Ok(true) => {
                // Version matches! Connect.
                return Ok(open_connection(&db_file)?);
            }
            _ => {
                // Outdated, corrupt, or table missing. Rebuild.
                let _ = fs::remove_file(&db_file);
            }
        }
    }

    // Connect and build tables
    let mut conn = open_connection(&db_file)?;
    create_tables(&mut conn)?;
    Ok(conn)
}

/// One font row as shown in the GUI font table: one entry per
/// `(path, face_index)`, with all of that face's aliases merged into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontIndexRow {
    pub path: String,
    pub face_index: u32,
    pub display_name: String,
    pub normalized_name: String,
    pub inferred_weight: i32,
    pub aliases: Vec<FontIndexAlias>,
}

/// One `(alias, normalized_alias)` pair belonging to a [`FontIndexRow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontIndexAlias {
    pub alias: String,
    pub normalized_alias: String,
}

/// Read every font and its aliases from an already open database connection.
pub(crate) fn read_font_index(conn: &Connection) -> Result<Vec<FontIndexRow>> {
    let mut stmt = conn.prepare(
        "SELECT f.path, f.face_index, f.display_name, f.normalized_name, f.inferred_weight,
                a.alias, a.normalized_alias
         FROM fonts f
         LEFT JOIN aliases a ON a.path = f.path AND a.face_index = f.face_index
         ORDER BY f.path COLLATE NOCASE, f.face_index, a.alias COLLATE NOCASE",
    )?;
    let mut rows = stmt.query([])?;
    let mut out: Vec<FontIndexRow> = Vec::new();
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let face_index = u32::try_from(row.get::<_, i64>(1)?).unwrap_or(0);
        let same_face = out
            .last()
            .is_some_and(|last| last.path == path && last.face_index == face_index);
        if !same_face {
            out.push(FontIndexRow {
                path: path.clone(),
                face_index,
                display_name: row.get(2)?,
                normalized_name: row.get(3)?,
                inferred_weight: row.get(4)?,
                aliases: Vec::new(),
            });
        }
        let alias: Option<String> = row.get(5)?;
        let normalized_alias: Option<String> = row.get(6)?;
        if let (Some(alias), Some(normalized_alias)) = (alias, normalized_alias)
            && let Some(last) = out.last_mut()
        {
            last.aliases.push(FontIndexAlias {
                alias,
                normalized_alias,
            });
        }
    }
    Ok(out)
}

/// Read every font (and its aliases) from the `fonts.db` inside `dbpath`.
///
/// A missing database or one written by an older schema yields an empty list
/// instead of an error: the GUI shows that as "index not built yet".
pub fn load_font_index(dbpath: &Path) -> Result<Vec<FontIndexRow>> {
    let db_file = dbpath.join("fonts.db");
    if !db_file.exists() || !check_schema_version(&db_file)? {
        return Ok(Vec::new());
    }
    let conn = open_connection(&db_file)?;
    read_font_index(&conn)
}

fn check_schema_version(db_file: &Path) -> std::result::Result<bool, rusqlite::Error> {
    let conn = open_connection(db_file)?;

    // Check if meta table exists and version matches
    let table_exists: i32 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='meta'",
        [],
        |row| row.get(0),
    )?;

    if table_exists == 0 {
        return Ok(false);
    }

    let version_str: String = conn.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )?;

    if let Ok(ver) = version_str.parse::<i32>() {
        Ok(ver == CURRENT_SCHEMA_VERSION)
    } else {
        Ok(false)
    }
}

fn create_tables(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;

    tx.execute(
        "CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
        [],
    )?;

    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1);",
        params![CURRENT_SCHEMA_VERSION.to_string()],
    )?;

    tx.execute(
        "CREATE TABLE IF NOT EXISTS fonts (
            path TEXT NOT NULL,
            face_index INTEGER NOT NULL,
            display_name TEXT NOT NULL,
            normalized_name TEXT NOT NULL,
            inferred_weight INTEGER NOT NULL,
            is_italic INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            file_size INTEGER NOT NULL,
            PRIMARY KEY (path, face_index)
        );",
        [],
    )?;

    tx.execute(
        "CREATE INDEX IF NOT EXISTS idx_fonts_normalized_name ON fonts(normalized_name);",
        [],
    )?;

    tx.execute(
        "CREATE TABLE IF NOT EXISTS aliases (
            path TEXT NOT NULL,
            face_index INTEGER NOT NULL,
            alias TEXT NOT NULL,
            normalized_alias TEXT NOT NULL,
            PRIMARY KEY (path, face_index, alias),
            FOREIGN KEY (path, face_index) REFERENCES fonts(path, face_index) ON DELETE CASCADE
        );",
        [],
    )?;

    tx.execute(
        "CREATE INDEX IF NOT EXISTS idx_aliases_normalized ON aliases(normalized_alias);",
        [],
    )?;

    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    fn insert_font(conn: &Connection, path: &str, face: u32, display: &str, normalized: &str) {
        conn.execute(
            "INSERT INTO fonts (path, face_index, display_name, normalized_name, inferred_weight, is_italic, mtime, file_size)
             VALUES (?1, ?2, ?3, ?4, 400, 0, 100, 100)",
            rusqlite::params![path, face, display, normalized],
        )
        .expect("font insert");
    }

    fn insert_alias(conn: &Connection, path: &str, face: u32, alias: &str) {
        conn.execute(
            "INSERT INTO aliases (path, face_index, alias, normalized_alias)
             VALUES (?1, ?2, ?3, ?3)",
            rusqlite::params![path, face, alias],
        )
        .expect("alias insert");
    }

    #[test]
    fn read_font_index_merges_aliases_into_one_row_per_face() -> Result<()> {
        let mut conn = Connection::open_in_memory()?;
        create_tables(&mut conn)?;
        insert_font(&conn, "C:/fonts/a.ttc", 0, "Alpha", "alpha");
        insert_font(&conn, "C:/fonts/a.ttc", 1, "Beta", "beta");
        insert_font(&conn, "C:/fonts/b.otf", 0, "Gamma", "gamma");
        insert_alias(&conn, "C:/fonts/a.ttc", 0, "Alpha Alias");
        insert_alias(&conn, "C:/fonts/a.ttc", 0, "Second Alias");

        let rows = read_font_index(&conn)?;
        assert_eq!(rows.len(), 3, "one row per (path, face_index)");
        assert_eq!(rows[0].path, "C:/fonts/a.ttc");
        assert_eq!(rows[0].face_index, 0);
        assert_eq!(rows[0].display_name, "Alpha");
        assert_eq!(
            rows[0].aliases,
            vec![
                FontIndexAlias {
                    alias: "Alpha Alias".to_string(),
                    normalized_alias: "Alpha Alias".to_string(),
                },
                FontIndexAlias {
                    alias: "Second Alias".to_string(),
                    normalized_alias: "Second Alias".to_string(),
                },
            ],
            "aliases of the same face are merged into the row"
        );
        assert_eq!(rows[1].face_index, 1);
        assert!(rows[1].aliases.is_empty());
        assert_eq!(rows[2].display_name, "Gamma");
        assert!(rows[2].aliases.is_empty());
        Ok(())
    }

    #[test]
    fn load_font_index_returns_empty_for_a_missing_database() -> Result<()> {
        let rows = load_font_index(Path::new("assfonts-missing-fonts-db-dir"))?;
        assert!(rows.is_empty());
        Ok(())
    }

    #[test]
    fn test_foreign_key_cascade() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // 1. Test when foreign_keys is disabled (explicitly set to OFF), deleting a font won't cascade delete its aliases
        let mut conn = Connection::open_in_memory()?;
        conn.execute("PRAGMA foreign_keys = OFF;", [])?;
        create_tables(&mut conn)?;

        // Insert font
        conn.execute(
            "INSERT INTO fonts (path, face_index, display_name, normalized_name, inferred_weight, is_italic, mtime, file_size)
             VALUES ('/path/to/font.ttf', 0, 'Test Font', 'testfont', 400, 0, 100, 100)",
            [],
        )?;

        // Insert alias
        conn.execute(
            "INSERT INTO aliases (path, face_index, alias, normalized_alias)
             VALUES ('/path/to/font.ttf', 0, 'Test Font Alias', 'testfontalias')",
            [],
        )?;

        // Delete font
        conn.execute("DELETE FROM fonts WHERE path = '/path/to/font.ttf'", [])?;

        // Check if the alias still exists (it should, because foreign_keys = OFF)
        let count: i32 = conn.query_row(
            "SELECT count(*) FROM aliases WHERE path = '/path/to/font.ttf'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            count, 1,
            "Without foreign_keys = ON, cascade delete should not work"
        );

        // 2. Test when foreign_keys is enabled, cascade delete works as expected
        let mut conn = Connection::open_in_memory()?;
        conn.execute("PRAGMA foreign_keys = ON;", [])?;
        create_tables(&mut conn)?;

        // Insert font
        conn.execute(
            "INSERT INTO fonts (path, face_index, display_name, normalized_name, inferred_weight, is_italic, mtime, file_size)
             VALUES ('/path/to/font.ttf', 0, 'Test Font', 'testfont', 400, 0, 100, 100)",
            [],
        )?;

        // Insert alias
        conn.execute(
            "INSERT INTO aliases (path, face_index, alias, normalized_alias)
             VALUES ('/path/to/font.ttf', 0, 'Test Font Alias', 'testfontalias')",
            [],
        )?;

        // Delete font
        conn.execute("DELETE FROM fonts WHERE path = '/path/to/font.ttf'", [])?;

        // Check if the alias still exists (it should be deleted by cascade)
        let count: i32 = conn.query_row(
            "SELECT count(*) FROM aliases WHERE path = '/path/to/font.ttf'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            count, 0,
            "With foreign_keys = ON, cascade delete should work"
        );

        Ok(())
    }
}
