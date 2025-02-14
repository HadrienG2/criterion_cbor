//! SQLite database that is automatically derived from Criterion's CBOR output

use crate::Search;
use chrono::DateTime;
use rusqlite::{types::FromSqlError, OpenFlags};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
use thiserror::Error;

/// Connection to the SQLite database
pub struct Connection(rusqlite::Connection);
//
impl Connection {
    /// Load the SQLite database, create it if it does not exist, and update
    /// it with new data if available
    pub fn setup(cargo_root: impl AsRef<Path>) -> Result<Self, SetupError> {
        // Determine where the database should be located
        let mut db_path = cargo_root.as_ref().to_owned();
        db_path.push("target");
        db_path.push("criterion");
        db_path.push("data.sqlite");

        // Determine the set of open flags that we're always going to use
        let common_open_flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_EXRESCODE;

        // If the database does not exist yet, create it
        let mut new_connection = None;
        if !db_path.exists() {
            std::fs::create_dir_all(db_path.parent().unwrap());
            let mut connection = rusqlite::Connection::open_with_flags(
                &db_path,
                common_open_flags | OpenFlags::SQLITE_OPEN_CREATE,
            )?;
            connection.execute_batch(SCHEMA)?;
            new_connection = Some(connection);
        }

        // Ensure the database is opened in read/write mode and determine which
        // files it already knows about
        let (mut connection, known_measurements) = match new_connection {
            // A newly created database knows about no measurements...
            Some(new_connection) => (new_connection, HashMap::new()),
            // ...but an existing one may do so
            None => {
                // Open the database in R/W mode and query known measurements
                let mut connection =
                    rusqlite::Connection::open_with_flags(&db_path, common_open_flags)?;
                let paths_and_file_ids =
                    connection.prepare("SELECT relative_path, file_id FROM measurement")?;
                let mut rows = paths_and_file_ids.query([])?;

                // Collect list in a layout suitable for fast file filtering: a
                // map keyed by relative parent directory path containing a set
                // of all known files within that directory.
                let mut known_measurements = HashMap::<Box<str>, HashSet<Box<str>>>::new();
                while let Some(row) = rows.next()? {
                    let path = row.get_ref(0)?.as_str()?;
                    let file_id: Box<str> = row.get(1)?;
                    if let Some(set) = known_measurements.get_mut(path) {
                        debug_assert!(set.insert(file_id));
                    } else {
                        debug_assert!(known_measurements
                            .insert(path.to_owned().into_boxed_str(), HashSet::from([file_id]))
                            .is_none());
                    }
                }
                (connection, known_measurements)
            }
        };

        // Now update the database
        let check_mtime =
            connection.prepare("SELECT modified FROM benchmark WHERE relative_path = ?1")?;
        for benchmark in Search::in_cargo_root(&cargo_root).find_all() {
            // Have we seen this benchmark before?
            let benchmark = benchmark?;
            let relative_path = benchmark
                .path_from_data_root()
                .to_str()
                .expect("Criterion should produce Unicode paths");
            let known_measurements = known_measurements.get(relative_path);

            // First, make the benchmark's database entry right
            if let Some(known_measurements) = known_measurements {
                // Has its metadata been updated since?
                let database_mtime = check_mtime
                    .query_row([relative_path], |row| {
                        Ok(DateTime::parse_from_rfc3339(row.get_ref(0)?.as_str()?))
                    })?
                    .expect("Database records should have an ISO-8601 format");
                let file_mtime = DateTime::from(benchmark.metadata.metadata()?.modified()?);
                if file_mtime > database_mtime {
                    let metadata = benchmark.metadata()?;
                    // TODO: Database entry about benchmark is stale, update it
                }
            } else {
                // TODO: Benchmark not known, read metadata and create entry
            }

            // TODO: Next, add measurements not in known_measurements
        }

        // TODO: Switch database to query_only mode with pragma after updating
        // TODO: Once I'm done, split this into sub-functions
    }
}
//
impl Drop for Connection {
    fn drop(&mut self) {
        self.0
            .execute("PRAGMA optimize", ())
            .expect("Failed to optimize SQLite database");
    }
}

/// Error while updating the sqlite database
#[derive(Debug, Error)]
enum SetupError {
    #[error("failed to manipulate the sqlite database")]
    Sqlite(#[from] rusqlite::Error),
    #[error("failed to convert some SQL data to supposedly matching Rust types")]
    SqliteFromSql(#[from] FromSqlError),
    #[error("failed to enumerate CBOR files")]
    Walkdir(#[from] walkdir::Error),
    #[error("failed to perform some I/O")]
    Io(#[from] std::io::Error),
}

/// Database schema definition
///
/// Stored in a different file so it can be viewed with SQL syntax highlighting.
static SCHEMA: &str = include_str!("schema.sql");
