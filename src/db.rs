use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::feed::normalise_name;
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
                movie_key TEXT NOT NULL,
                imdb_id TEXT NOT NULL,
                imdb_rating REAL NOT NULL,
                first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                status TEXT NOT NULL DEFAULT 'pending',
                queued_at TEXT,
                attempts INTEGER NOT NULL DEFAULT 0,
                last_error TEXT
            );
            CREATE INDEX movies_status_idx ON movies(status);
            CREATE INDEX movies_movie_key_idx ON movies(movie_key);
            CREATE INDEX movies_imdb_id_idx ON movies(imdb_id);
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
        ("movie_key", "TEXT"),
        ("imdb_id", "TEXT"),
        ("imdb_rating", "REAL"),
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
        CREATE INDEX IF NOT EXISTS movies_movie_key_idx ON movies(movie_key);
        CREATE INDEX IF NOT EXISTS movies_imdb_id_idx ON movies(imdb_id);
        ",
    )?;
    backfill_legacy_movie_keys(connection)?;
    Ok(())
}

fn backfill_legacy_movie_keys(connection: &Connection) -> Result<()> {
    let mut statement = connection
        .prepare("SELECT rowid, name FROM movies WHERE movie_key IS NULL OR movie_key = ''")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);

    for (row_id, name) in rows {
        if let Some(movie_key) = inferred_movie_key(&name) {
            connection.execute(
                "UPDATE movies SET movie_key = ?1 WHERE rowid = ?2",
                params![movie_key, row_id],
            )?;
        }
    }
    Ok(())
}

fn inferred_movie_key(name: &str) -> Option<String> {
    let normalized = normalise_name(name);
    let words: Vec<_> = normalized.split_whitespace().collect();
    let (year_index, year) = words.iter().enumerate().find_map(|(index, word)| {
        (index > 0)
            .then(|| word.parse::<i32>().ok())
            .flatten()
            .filter(|year| (1900..=2100).contains(year))
            .map(|year| (index, year))
    })?;
    let title = words[..year_index].join(" ").to_ascii_lowercase();
    (!title.is_empty()).then(|| format!("{title} {year}"))
}

pub fn record_releases(
    connection: &mut Connection,
    releases: &[Release],
    status: &str,
) -> Result<usize> {
    let transaction = connection.transaction()?;
    let mut added = 0;
    for release in releases {
        let existing: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM movies WHERE movie_key = ?1 OR imdb_id = ?2
            )",
            params![release.movie_key, release.imdb_id],
            |row| row.get(0),
        )?;
        if existing {
            continue;
        }
        added += transaction.execute(
            "INSERT OR IGNORE INTO movies
                (name, url, movie_key, imdb_id, imdb_rating, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                release.name,
                release.url,
                release.movie_key,
                release.imdb_id,
                release.imdb_rating,
                status,
            ],
        )?;
    }
    transaction.commit()?;
    Ok(added)
}

pub fn pending_releases(connection: &Connection) -> Result<Vec<Release>> {
    let mut statement = connection.prepare(
        "SELECT name, url, movie_key, imdb_id, imdb_rating
         FROM movies WHERE status = 'pending' ORDER BY first_seen_at, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Release {
            name: row.get(0)?,
            url: row.get(1)?,
            movie_key: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            imdb_id: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            imdb_rating: row.get::<_, Option<f64>>(4)?.unwrap_or_default(),
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
        "SELECT name, url, movie_key, imdb_id, imdb_rating,
                first_seen_at, status, queued_at, attempts, last_error
         FROM movies ORDER BY first_seen_at, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StoredRelease {
            name: row.get(0)?,
            url: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            movie_key: row.get(2)?,
            imdb_id: row.get(3)?,
            imdb_rating: row.get(4)?,
            first_seen_at: row.get(5)?,
            status: row.get(6)?,
            queued_at: row.get(7)?,
            attempts: row.get(8)?,
            last_error: row.get(9)?,
        })
    })?;

    let mut count = 0;
    for release in rows {
        let release = release?;
        let rating = release
            .imdb_rating
            .map(|rating| format!("{rating:.1}"))
            .unwrap_or_default();
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            release.status,
            release.first_seen_at,
            release.attempts,
            release.queued_at.unwrap_or_default(),
            release.last_error.unwrap_or_default(),
            release.imdb_id.unwrap_or_default(),
            rating,
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
            .execute(
                "INSERT INTO MOVIES VALUES ('Old Movie 2024 2160p', 'magnet:?old')",
                [],
            )
            .unwrap();
        drop(legacy);

        let connection = open_database(&path).unwrap();
        assert!(pending_releases(&connection).unwrap().is_empty());
        let status: String = connection
            .query_row(
                "SELECT status FROM movies WHERE name = 'Old Movie 2024 2160p'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "queued");
        let movie_key: Option<String> = connection
            .query_row(
                "SELECT movie_key FROM movies WHERE name = 'Old Movie 2024 2160p'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(movie_key.as_deref(), Some("old movie 2024"));
    }

    #[test]
    fn stores_new_releases_as_pending() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("movies.db");
        let mut connection = open_database(&path).unwrap();
        let releases = vec![Release {
            name: "New Movie 2026 1080p".to_owned(),
            url: "magnet:?new".to_owned(),
            movie_key: "new movie 2026".to_owned(),
            imdb_id: "tt1234567".to_owned(),
            imdb_rating: 7.5,
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

    #[test]
    fn stores_only_one_selected_release_per_canonical_movie() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("movies.db");
        let mut connection = open_database(&path).unwrap();
        let dolby_vision = Release {
            name: "Example Movie 2026 2160p DV WEB DL".to_owned(),
            url: "magnet:?dolby".to_owned(),
            movie_key: "example movie 2026".to_owned(),
            imdb_id: "tt7654321".to_owned(),
            imdb_rating: 8.0,
        };
        let duplicate_variant = Release {
            name: "Example Movie 2026 2160p BluRay REMUX".to_owned(),
            url: "magnet:?remux".to_owned(),
            ..dolby_vision.clone()
        };

        assert_eq!(
            record_releases(&mut connection, &[dolby_vision.clone()], "pending").unwrap(),
            1
        );
        assert_eq!(
            record_releases(&mut connection, &[duplicate_variant], "pending").unwrap(),
            0
        );
        assert_eq!(pending_releases(&connection).unwrap(), vec![dolby_vision]);
    }
}
