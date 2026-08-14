use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::models::{Release, StoredRelease};

pub fn open_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("open SQLite database {}", path.display()))?;
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND lower(name) = 'movies')",
        [],
        |row| row.get(0),
    )?;

    if exists {
        migrate_legacy_database(&connection)?;
    } else {
        connection.execute_batch(
            "
            CREATE TABLE movies (
                name TEXT PRIMARY KEY NOT NULL,
                url TEXT NOT NULL,
                first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                status TEXT NOT NULL DEFAULT 'pending',
                queued_at TEXT,
                attempts INTEGER NOT NULL DEFAULT 0,
                last_error TEXT
            );
            CREATE INDEX movies_status_idx ON movies(status);
            ",
        )?;
    }
    Ok(connection)
}

fn migrate_legacy_database(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(movies)")?;
    let columns: HashSet<String> = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<_, _>>()?;

    for (name, definition) in [
        ("first_seen_at", "TEXT"),
        ("status", "TEXT"),
        ("queued_at", "TEXT"),
        ("attempts", "INTEGER"),
        ("last_error", "TEXT"),
    ] {
        if !columns.contains(name) {
            connection.execute_batch(&format!(
                "ALTER TABLE movies ADD COLUMN {name} {definition}"
            ))?;
        }
    }
    // Legacy rows represent releases the Python process had already seen. Do not enqueue history.
    connection.execute_batch(
        "
        UPDATE movies
        SET first_seen_at = COALESCE(first_seen_at, CURRENT_TIMESTAMP),
            status = COALESCE(status, 'queued'),
            attempts = COALESCE(attempts, 0);
        CREATE INDEX IF NOT EXISTS movies_status_idx ON movies(status);
        ",
    )?;
    Ok(())
}

pub fn record_releases(
    connection: &mut Connection,
    releases: &[Release],
    status: &str,
) -> Result<usize> {
    let transaction = connection.transaction()?;
    let mut added = 0;
    for release in releases {
        added += transaction.execute(
            "INSERT OR IGNORE INTO movies (name, url, status) VALUES (?1, ?2, ?3)",
            params![release.name, release.url, status],
        )?;
    }
    transaction.commit()?;
    Ok(added)
}

pub fn pending_releases(connection: &Connection) -> Result<Vec<Release>> {
    let mut statement = connection.prepare(
        "SELECT name, url FROM movies WHERE status = 'pending' ORDER BY first_seen_at, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Release {
            name: row.get(0)?,
            url: row.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn mark_queued(connection: &Connection, name: &str) -> Result<()> {
    connection.execute(
        "UPDATE movies
         SET status = 'queued', queued_at = CURRENT_TIMESTAMP,
             attempts = attempts + 1, last_error = NULL
         WHERE name = ?1",
        params![name],
    )?;
    Ok(())
}

pub fn record_queue_error(connection: &Connection, name: &str, error: &str) -> Result<()> {
    connection.execute(
        "UPDATE movies
         SET attempts = attempts + 1, last_error = ?2
         WHERE name = ?1",
        params![name, error],
    )?;
    Ok(())
}

pub fn print_database(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT name, url, first_seen_at, status, queued_at, attempts, last_error
         FROM movies ORDER BY first_seen_at, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StoredRelease {
            name: row.get(0)?,
            url: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            first_seen_at: row.get(2)?,
            status: row.get(3)?,
            queued_at: row.get(4)?,
            attempts: row.get(5)?,
            last_error: row.get(6)?,
        })
    })?;

    let mut count = 0;
    for release in rows {
        let release = release?;
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            release.status,
            release.first_seen_at,
            release.attempts,
            release.queued_at.unwrap_or_default(),
            release.last_error.unwrap_or_default(),
            release.name,
            release.url,
        );
        count += 1;
    }
    println!("{count} release(s) in movies");
    Ok(())
}

pub fn export_queue(path: Option<&Path>, releases: &[Release]) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create queue directory {}", parent.display()))?;
    }
    let mut content = String::new();
    for release in releases {
        content.push_str(&release.name);
        content.push('\n');
        content.push_str(&release.url);
        content.push('\n');
    }
    fs::write(path, content).with_context(|| format!("write pending queue {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn migrates_legacy_database_without_requeueing_old_rows() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("legacy.db");
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute(
                "CREATE TABLE MOVIES (name TEXT PRIMARY KEY NOT NULL, url TEXT)",
                [],
            )
            .unwrap();
        legacy
            .execute("INSERT INTO MOVIES VALUES ('Old Movie', 'magnet:?old')", [])
            .unwrap();
        drop(legacy);

        let connection = open_database(&path).unwrap();
        assert!(pending_releases(&connection).unwrap().is_empty());
        let status: String = connection
            .query_row(
                "SELECT status FROM movies WHERE name = 'Old Movie'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "queued");
    }

    #[test]
    fn stores_new_releases_as_pending() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("movies.db");
        let mut connection = open_database(&path).unwrap();
        let releases = vec![Release {
            name: "New Movie 2026 1080p".to_owned(),
            url: "magnet:?new".to_owned(),
        }];

        assert_eq!(
            record_releases(&mut connection, &releases, "pending").unwrap(),
            1
        );
        assert_eq!(
            record_releases(&mut connection, &releases, "pending").unwrap(),
            0
        );
        assert_eq!(pending_releases(&connection).unwrap(), releases);
    }
}
