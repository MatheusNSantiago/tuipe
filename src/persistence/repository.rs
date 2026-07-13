use std::{fs, path::Path};

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::typing::{Metrics, TestConfig, TestStatus};

pub struct Repository {
    connection: Connection,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&connection)?;
        Ok(Self { connection })
    }

    pub fn save_session(
        &self,
        config: &TestConfig,
        status: &TestStatus,
        metrics: Metrics,
    ) -> Result<i64> {
        let terminal_state = match status {
            TestStatus::Ready => "ready",
            TestStatus::Running { .. } => "restart",
            TestStatus::Completed { .. } => "completed",
            TestStatus::Failed { .. } => "failed",
        };
        self.connection.execute(
            "INSERT INTO sessions (
                terminal_state, config_toml, elapsed_ms, wpm, raw_wpm, accuracy,
                correct_chars, incorrect_chars, extra_chars, missed_chars,
                metrics_version, adaptive_version, codec_version
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, 1, 1)",
            params![
                terminal_state,
                toml::to_string(config)?,
                metrics.duration_ms as i64,
                metrics.wpm,
                metrics.raw_wpm,
                metrics.accuracy,
                metrics.characters.correct_word,
                metrics.characters.incorrect,
                metrics.characters.extra,
                metrics.characters.missed,
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }
}

fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
         INSERT INTO schema_version (version) SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version);
         CREATE TABLE IF NOT EXISTS sessions (
           id INTEGER PRIMARY KEY, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           terminal_state TEXT NOT NULL, config_toml TEXT NOT NULL, elapsed_ms INTEGER NOT NULL,
           wpm REAL NOT NULL, raw_wpm REAL NOT NULL, accuracy REAL NOT NULL,
           correct_chars INTEGER NOT NULL, incorrect_chars INTEGER NOT NULL,
           extra_chars INTEGER NOT NULL, missed_chars INTEGER NOT NULL,
           metrics_version INTEGER NOT NULL, adaptive_version INTEGER NOT NULL, codec_version INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS word_observations (
           id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES sessions(id),
           language TEXT NOT NULL, word TEXT NOT NULL, confirmed_error INTEGER NOT NULL,
           corrections INTEGER NOT NULL, active_ms INTEGER NOT NULL, afk_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS word_skill (language TEXT NOT NULL, word TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, word));
         CREATE TABLE IF NOT EXISTS ngram_skill (language TEXT NOT NULL, ngram TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, ngram));
         CREATE TABLE IF NOT EXISTS mechanic_skill (language TEXT NOT NULL, mechanic TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, mechanic));
         CREATE TABLE IF NOT EXISTS favorite_quotes (quote_id INTEGER PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS xp_state (id INTEGER PRIMARY KEY CHECK(id = 1), state BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS streak_state (id INTEGER PRIMARY KEY CHECK(id = 1), state BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS raw_events (session_id INTEGER PRIMARY KEY REFERENCES sessions(id), codec_version INTEGER NOT NULL, uncompressed_size INTEGER NOT NULL, blob BLOB NOT NULL);
         COMMIT;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::TestStatus;

    #[test]
    fn migrations_create_a_session_repository() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let id = repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 1 },
                Metrics::default(),
            )
            .unwrap();
        assert_eq!(id, 1);
    }
}
