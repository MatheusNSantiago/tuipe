use std::{fs, path::Path};

use anyhow::Result;
use chrono::{Datelike, Local};
use rusqlite::{Connection, OptionalExtension, params};

use crate::adaptive::{Observation, WordSkill};
use crate::gamification::{StreakState, XpGain, XpState, award};
use crate::typing::{Metrics, TestConfig, TestStatus};

pub struct Repository {
    connection: Connection,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatisticsOverview {
    pub completed_tests: u64,
    pub active_ms: u64,
    pub average_wpm: f64,
    pub average_accuracy: f64,
    pub best_wpm: f64,
    pub recent_tests: Vec<SessionSummary>,
    pub priority_words: Vec<PriorityWord>,
    pub total_xp: u64,
    pub level: u64,
    pub streak: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: u64,
    pub elapsed_ms: u64,
    pub wpm: f64,
    pub accuracy: f64,
    pub raw_wpm: f64,
    pub correct_chars: u32,
    pub incorrect_chars: u32,
    pub extra_chars: u32,
    pub config: TestConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriorityWord {
    pub word: String,
    pub difficulty: f64,
    pub confirmed_errors: f64,
    pub corrections: f64,
    pub observations: u32,
    pub estimated_session_chance: f64,
}

/// Evidência consultável de uma palavra observada durante uma sessão terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct WordObservationRecord {
    pub language: String,
    pub word: String,
    pub confirmed_error: bool,
    pub corrections: u32,
    pub active_ms: u64,
    pub afk_ms: u64,
    pub fast_success: bool,
    pub repeat_discount: f64,
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
        self.save_session_with_observations(config, status, metrics, &[])
    }

    /// Persiste sessão, evidências brutas e a projeção materializada do modelo
    /// adaptativo em uma única transação curta.
    pub fn save_session_with_observations(
        &self,
        config: &TestConfig,
        status: &TestStatus,
        metrics: Metrics,
        observations: &[WordObservationRecord],
    ) -> Result<i64> {
        let terminal_state = match status {
            TestStatus::Ready => "ready",
            TestStatus::Running { .. } => "restart",
            TestStatus::Completed { .. } => "completed",
            TestStatus::Failed { .. } => "failed",
        };
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
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
        let session_id = transaction.last_insert_rowid();
        for record in observations {
            transaction.execute(
                "INSERT INTO word_observations (
                    session_id, language, word, confirmed_error, corrections,
                    active_ms, afk_ms, fast_success
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_id,
                    record.language,
                    record.word,
                    record.confirmed_error,
                    record.corrections,
                    record.active_ms as i64,
                    record.afk_ms as i64,
                    record.fast_success,
                ],
            )?;

            let previous = transaction
                .query_row(
                    "SELECT state FROM word_skill WHERE language = ?1 AND word = ?2",
                    params![record.language, record.word],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?
                .map(|bytes| postcard::from_bytes::<WordSkill>(&bytes))
                .transpose()?
                .unwrap_or_default();
            let mut skill = previous;
            skill.observe(Observation {
                confirmed_error: record.confirmed_error,
                corrected: record.corrections > 0,
                fast_success: record.fast_success,
                repeat_discount: record.repeat_discount,
            });
            let state = postcard::to_allocvec(&skill)?;
            transaction.execute(
                "INSERT INTO word_skill (language, word, state) VALUES (?1, ?2, ?3)
                 ON CONFLICT(language, word) DO UPDATE SET state = excluded.state",
                params![record.language, record.word, state],
            )?;
        }
        transaction.commit()?;
        if matches!(status, TestStatus::Completed { .. }) {
            self.award_completed_session(config, &metrics)?;
        }
        Ok(session_id)
    }

    pub fn progress(&self) -> Result<(XpState, StreakState)> {
        Ok((
            self.load_state("xp_state")?,
            self.load_state("streak_state")?,
        ))
    }

    fn award_completed_session(&self, config: &TestConfig, metrics: &Metrics) -> Result<XpGain> {
        let (mut xp, mut streak) = self.progress()?;
        let day = Local::now().date_naive().num_days_from_ce();
        let gain = award(&mut xp, &mut streak, config, metrics, day);
        self.save_state("xp_state", &xp)?;
        self.save_state("streak_state", &streak)?;
        Ok(gain)
    }

    fn load_state<T: serde::de::DeserializeOwned + Default>(&self, table: &str) -> Result<T> {
        let sql = format!("SELECT state FROM {table} WHERE id = 1");
        let encoded = self
            .connection
            .query_row(&sql, [], |row| row.get::<_, Vec<u8>>(0))
            .optional()?;
        Ok(encoded
            .map(|data| postcard::from_bytes(&data))
            .transpose()?
            .unwrap_or_default())
    }

    fn save_state<T: serde::Serialize>(&self, table: &str, state: &T) -> Result<()> {
        let sql = format!(
            "INSERT INTO {table} (id, state) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET state = excluded.state"
        );
        self.connection
            .execute(&sql, [postcard::to_allocvec(state)?])?;
        Ok(())
    }

    pub fn load_word_skills(&self, language: &str) -> Result<Vec<(String, String, WordSkill)>> {
        let mut statement = self
            .connection
            .prepare("SELECT language, word, state FROM word_skill WHERE language = ?1")?;
        Ok(statement
            .query_map([language], |row| {
                let encoded = row.get::<_, Vec<u8>>(2)?;
                let skill = postcard::from_bytes(&encoded).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        encoded.len(),
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
                Ok((row.get(0)?, row.get(1)?, skill))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn load_all_word_skills(&self) -> Result<Vec<(String, String, WordSkill)>> {
        let mut statement = self
            .connection
            .prepare("SELECT language, word, state FROM word_skill")?;
        Ok(statement
            .query_map([], |row| {
                let encoded = row.get::<_, Vec<u8>>(2)?;
                let skill = postcard::from_bytes(&encoded).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        encoded.len(),
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
                Ok((row.get(0)?, row.get(1)?, skill))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn statistics_overview(&self) -> Result<StatisticsOverview> {
        let mut overview = self.connection.query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(elapsed_ms), 0),
                COALESCE(AVG(wpm), 0),
                COALESCE(AVG(accuracy), 0),
                COALESCE(MAX(wpm), 0)
             FROM sessions
             WHERE terminal_state = 'completed'",
            [],
            |row| {
                Ok(StatisticsOverview {
                    completed_tests: row.get(0)?,
                    active_ms: row.get::<_, i64>(1)? as u64,
                    average_wpm: row.get(2)?,
                    average_accuracy: row.get(3)?,
                    best_wpm: row.get(4)?,
                    recent_tests: Vec::new(),
                    priority_words: Vec::new(),
                    total_xp: 0,
                    level: 0,
                    streak: 0,
                })
            },
        )?;
        let mut statement = self.connection.prepare(
            "SELECT id, elapsed_ms, wpm, accuracy, raw_wpm, correct_chars,
                    incorrect_chars, extra_chars, config_toml
             FROM sessions
             WHERE terminal_state = 'completed'
             ORDER BY id DESC
             LIMIT 12",
        )?;
        overview.recent_tests = statement
            .query_map([], |row| {
                Ok(SessionSummary {
                    id: row.get::<_, i64>(0)? as u64,
                    elapsed_ms: row.get::<_, i64>(1)? as u64,
                    wpm: row.get(2)?,
                    accuracy: row.get(3)?,
                    raw_wpm: row.get(4)?,
                    correct_chars: row.get::<_, i64>(5)? as u32,
                    incorrect_chars: row.get::<_, i64>(6)? as u32,
                    extra_chars: row.get::<_, i64>(7)? as u32,
                    config: toml::from_str(&row.get::<_, String>(8)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        overview.recent_tests.reverse();
        overview.priority_words = self.priority_words()?;
        let (xp, streak) = self.progress()?;
        overview.total_xp = xp.total;
        overview.level = crate::gamification::level_from_total_xp(xp.total);
        overview.streak = streak.current;
        Ok(overview)
    }

    fn priority_words(&self) -> Result<Vec<PriorityWord>> {
        let policy = crate::adaptive::AdaptivePolicy::default();
        let mut skills = self.load_all_word_skills()?;
        skills.sort_by(|left, right| {
            policy
                .difficulty(&right.2)
                .total_cmp(&policy.difficulty(&left.2))
        });
        Ok(skills
            .into_iter()
            .filter_map(|(_, word, skill)| {
                let difficulty = policy.difficulty(&skill);
                (difficulty > 0.0).then_some(PriorityWord {
                    word,
                    difficulty,
                    confirmed_errors: skill.confirmed_errors,
                    corrections: skill.corrections,
                    observations: skill.observations,
                    estimated_session_chance: 0.0,
                })
            })
            .take(8)
            .collect())
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
           corrections INTEGER NOT NULL, active_ms INTEGER NOT NULL, afk_ms INTEGER NOT NULL,
           fast_success INTEGER NOT NULL DEFAULT 0
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
    if !table_has_column(connection, "word_observations", "fast_success")? {
        connection.execute(
            "ALTER TABLE word_observations ADD COLUMN fast_success INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    Ok(())
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column))
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

    #[test]
    fn overview_uses_only_completed_tests() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let completed = Metrics {
            duration_ms: 12_000,
            wpm: 80.0,
            accuracy: 95.0,
            ..Metrics::default()
        };
        repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Completed {
                    ended_at_ms: 12_000,
                },
                completed,
            )
            .unwrap();
        repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Failed {
                    ended_at_ms: 1_000,
                    word_index: 0,
                },
                Metrics::default(),
            )
            .unwrap();

        assert_eq!(
            repository.statistics_overview().unwrap(),
            StatisticsOverview {
                completed_tests: 1,
                active_ms: 12_000,
                average_wpm: 80.0,
                average_accuracy: 95.0,
                best_wpm: 80.0,
                recent_tests: vec![SessionSummary {
                    id: 1,
                    elapsed_ms: 12_000,
                    wpm: 80.0,
                    accuracy: 95.0,
                    raw_wpm: 0.0,
                    correct_chars: 0,
                    incorrect_chars: 0,
                    extra_chars: 0,
                    config: TestConfig::default(),
                }],
                priority_words: Vec::new(),
                total_xp: 27,
                level: 1,
                streak: 1,
            }
        );
    }

    #[test]
    fn observacoes_atualizam_e_restauram_a_habilidade_da_palavra() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        repository
            .save_session_with_observations(
                &TestConfig::default(),
                &TestStatus::Failed {
                    ended_at_ms: 1_000,
                    word_index: 0,
                },
                Metrics::default(),
                &[WordObservationRecord {
                    language: "portuguese".into(),
                    word: "difícil".into(),
                    confirmed_error: true,
                    corrections: 2,
                    active_ms: 320,
                    afk_ms: 0,
                    fast_success: false,
                    repeat_discount: 1.0,
                }],
            )
            .unwrap();

        assert_eq!(
            repository.load_word_skills("portuguese").unwrap(),
            vec![(
                "portuguese".into(),
                "difícil".into(),
                WordSkill {
                    confirmed_errors: 1.0,
                    corrections: 1.0,
                    fast_successes: 0.0,
                    observations: 1,
                },
            )]
        );
        let priority = repository.statistics_overview().unwrap().priority_words;
        assert_eq!(priority.len(), 1);
        assert_eq!(priority[0].word, "difícil");
        assert_eq!(priority[0].confirmed_errors, 1.0);
    }
}
