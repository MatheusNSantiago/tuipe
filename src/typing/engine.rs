use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use super::{CharacterStats, Difficulty, Metrics, TargetWord, TestConfig, TestMode, WordAttempt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    Text(String),
    Backspace,
    DeleteWordBackward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key { action: KeyAction, at_ms: u64 },
    External { event: ExternalEvent, at_ms: u64 },
    Tick { at_ms: u64 },
}

/// Eventos do terminal que afetam a interpretação da sessão, mas não inserem
/// texto no teste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEvent {
    Focus { gained: bool },
    Paste { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStatus {
    Ready,
    Running { started_at_ms: u64 },
    Completed { ended_at_ms: u64 },
    Failed { ended_at_ms: u64, word_index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    Started,
    Advanced { from: usize, to: usize },
    Completed,
    Failed { word_index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedInputEvent {
    pub at_ms: u64,
    pub word_index: usize,
    pub kind: RecordedInputKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordedInputKind {
    /// Formato legado do codec 2. Mantido apenas para ler históricos antigos.
    Insert {
        grapheme: String,
        expected: Option<String>,
        input_before: String,
        input_after: String,
        correct: bool,
    },
    Delete {
        deleted: String,
        input_before: String,
        input_after: String,
        corrected_graphemes: u16,
        whole_word: bool,
    },
    Focus {
        gained: bool,
    },
    Paste {
        text: String,
    },
    InsertDelta {
        grapheme: String,
        expected: Option<String>,
        correct: bool,
    },
    DeleteDelta {
        deleted_graphemes: u16,
        corrected_graphemes: u16,
        whole_word: bool,
    },
    PasteRedacted {
        graphemes: u32,
    },
}

#[derive(Debug, Clone)]
enum CharacterEdit {
    Insert(String),
    Delete(u16),
}

#[derive(Debug, Clone)]
struct CharacterEvent {
    word_index: usize,
    edit: CharacterEdit,
    correct: Option<bool>,
    at_ms: u64,
}

/// Redutor puro para a parte da entrada do Monkeytype suportada pelo tuipe.
///
/// Ele não possui estado do terminal, relógio de parede nem gerador. O chamador
/// fornece timestamps monotônicos e acrescenta alvos suficientes para que um
/// teste cronometrado não os esgote.
#[derive(Debug, Clone)]
pub struct TestEngine {
    config: TestConfig,
    targets: Vec<TargetWord>,
    attempts: Vec<WordAttempt>,
    active_word: usize,
    status: TestStatus,
    character_events: Vec<CharacterEvent>,
    recorded_events: Vec<RecordedInputEvent>,
    current_at_ms: u64,
}

impl TestEngine {
    pub fn new(config: TestConfig, generated_words: impl IntoIterator<Item = String>) -> Self {
        let targets: Vec<_> = generated_words
            .into_iter()
            .map(TargetWord::from_generated)
            .collect();
        assert!(
            !targets.is_empty(),
            "a test needs at least one generated word"
        );

        let attempts = vec![WordAttempt::default(); targets.len()];
        Self {
            config,
            targets,
            attempts,
            active_word: 0,
            status: TestStatus::Ready,
            character_events: Vec::new(),
            recorded_events: Vec::new(),
            current_at_ms: 0,
        }
    }

    pub fn config(&self) -> &TestConfig {
        &self.config
    }

    pub fn targets(&self) -> &[TargetWord] {
        &self.targets
    }

    pub fn append_words(&mut self, generated_words: impl IntoIterator<Item = String>) {
        let targets = generated_words
            .into_iter()
            .map(TargetWord::from_generated)
            .collect::<Vec<_>>();
        self.attempts
            .extend((0..targets.len()).map(|_| WordAttempt::default()));
        self.targets.extend(targets);
    }

    pub fn attempts(&self) -> &[WordAttempt] {
        &self.attempts
    }

    pub fn active_word(&self) -> usize {
        self.active_word
    }

    pub fn status(&self) -> &TestStatus {
        &self.status
    }

    pub fn recorded_events(&self) -> &[RecordedInputEvent] {
        &self.recorded_events
    }

    pub fn update(&mut self, event: InputEvent) -> Vec<Transition> {
        if matches!(
            self.status,
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        ) {
            return Vec::new();
        }

        let at_ms = match &event {
            InputEvent::Tick { at_ms }
            | InputEvent::Key { at_ms, .. }
            | InputEvent::External { at_ms, .. } => *at_ms,
        };
        self.current_at_ms = self.current_at_ms.max(at_ms);

        match event {
            InputEvent::Tick { at_ms } => self.advance_deadline(at_ms),
            InputEvent::Key { action, at_ms } => {
                let mut transitions = self.advance_deadline(at_ms);
                if !self.is_terminal() {
                    transitions.extend(self.handle_key(action, at_ms));
                }
                transitions
            }
            InputEvent::External { event, at_ms } => {
                let transitions = self.advance_deadline(at_ms);
                if !self.is_terminal() {
                    self.record_external(event, at_ms);
                }
                transitions
            }
        }
    }

    fn record_external(&mut self, event: ExternalEvent, at_ms: u64) {
        let kind = match event {
            ExternalEvent::Focus { gained } => RecordedInputKind::Focus { gained },
            ExternalEvent::Paste { text } => RecordedInputKind::PasteRedacted {
                graphemes: text.graphemes(true).count().try_into().unwrap_or(u32::MAX),
            },
        };
        self.recorded_events.push(RecordedInputEvent {
            at_ms,
            word_index: self.active_word,
            kind,
        });
    }

    fn advance_deadline(&mut self, at_ms: u64) -> Vec<Transition> {
        let TestStatus::Running { started_at_ms } = self.status else {
            return Vec::new();
        };
        let TestMode::Time { seconds } = self.config.mode else {
            return Vec::new();
        };

        let deadline_ms = started_at_ms.saturating_add(u64::from(seconds) * 1_000);
        if at_ms >= deadline_ms {
            self.status = TestStatus::Completed {
                ended_at_ms: deadline_ms,
            };
            return vec![Transition::Completed];
        }
        Vec::new()
    }

    fn handle_key(&mut self, action: KeyAction, at_ms: u64) -> Vec<Transition> {
        match action {
            KeyAction::Text(text) => self.insert_text(&text, at_ms),
            KeyAction::Backspace => {
                self.backspace(false, at_ms);
                Vec::new()
            }
            KeyAction::DeleteWordBackward => {
                self.backspace(true, at_ms);
                Vec::new()
            }
        }
    }

    fn insert_text(&mut self, text: &str, at_ms: u64) -> Vec<Transition> {
        let mut transitions = Vec::new();
        let normalized = text.nfc().collect::<String>();
        for grapheme in normalized.graphemes(true) {
            if self.is_terminal() {
                break;
            }
            transitions.extend(self.insert_grapheme(grapheme, at_ms));
        }
        transitions
    }

    fn insert_grapheme(&mut self, grapheme: &str, at_ms: u64) -> Vec<Transition> {
        let mut transitions = self.start_if_needed(at_ms);
        let word_index = self.active_word;
        let input_length = grapheme_count(&self.attempts[word_index].input);

        // O handler before-insert do Monkeytype descarta um separador inicial no
        // modo normal não estrito. O tuipe não possui a opção strict-space.
        if input_length == 0
            && is_separator(grapheme)
            && self.config.difficulty == Difficulty::Normal
        {
            return transitions;
        }

        let target = self.targets[word_index].with_commit();
        let expected = grapheme_at(&target, input_length).map(str::to_owned);
        let correct = expected
            .as_deref()
            .is_some_and(|expected| expected == grapheme);
        let commit = is_separator(grapheme);
        let input_limit = grapheme_count(&target).saturating_add(20);
        if input_length >= input_limit && !commit {
            return transitions;
        }

        let attempt = &mut self.attempts[word_index];
        if let Some(last_keypress_ms) = attempt.last_keypress_ms {
            let elapsed = at_ms.saturating_sub(last_keypress_ms);
            attempt.active_ms += elapsed;
        }
        attempt.input.push_str(grapheme);
        attempt.first_keypress_ms.get_or_insert(at_ms);
        attempt.last_keypress_ms = Some(at_ms);

        self.character_events.push(CharacterEvent {
            word_index,
            edit: CharacterEdit::Insert(grapheme.to_owned()),
            correct: Some(correct),
            at_ms,
        });
        self.recorded_events.push(RecordedInputEvent {
            at_ms,
            word_index,
            kind: RecordedInputKind::InsertDelta {
                grapheme: grapheme.to_owned(),
                expected,
                correct,
            },
        });
        let can_advance =
            commit && !(input_length == 0 && self.config.difficulty != Difficulty::Normal);
        let last_word = self.active_word + 1 == self.targets.len();
        let completes_bounded_test = last_word
            && !matches!(self.config.mode, TestMode::Time { .. })
            && (self.attempts[word_index].input == target || can_advance);

        // O handler upstream avança primeiro e avalia a dificuldade depois.
        // Preservamos a tentativa confirmada antes de expor a falha terminal.
        if can_advance {
            self.attempts[self.active_word].committed = true;
            if self.active_word + 1 < self.targets.len() {
                let from = self.active_word;
                self.active_word += 1;
                transitions.push(Transition::Advanced {
                    from,
                    to: self.active_word,
                });
            }
        }

        let failed = match self.config.difficulty {
            Difficulty::Normal => false,
            Difficulty::Expert => {
                commit && input_length > 0 && self.attempts[word_index].input != target
            }
            Difficulty::Master => !correct,
        };

        if failed {
            let failed_word = if can_advance {
                self.active_word.saturating_sub(1)
            } else {
                self.active_word
            };
            self.status = TestStatus::Failed {
                ended_at_ms: at_ms,
                word_index: failed_word,
            };
            transitions.push(Transition::Failed {
                word_index: failed_word,
            });
        } else if completes_bounded_test {
            self.attempts[self.active_word].committed = true;
            self.status = TestStatus::Completed { ended_at_ms: at_ms };
            transitions.push(Transition::Completed);
        }

        transitions
    }

    fn start_if_needed(&mut self, at_ms: u64) -> Vec<Transition> {
        if self.status == TestStatus::Ready {
            self.status = TestStatus::Running {
                started_at_ms: at_ms,
            };
            vec![Transition::Started]
        } else {
            Vec::new()
        }
    }

    fn backspace(&mut self, delete_word: bool, at_ms: u64) {
        if !matches!(self.status, TestStatus::Running { .. }) {
            return;
        }

        if self.attempts[self.active_word].input.is_empty() {
            if self.active_word == 0 {
                return;
            }
            self.active_word -= 1;
            let attempt = &mut self.attempts[self.active_word];
            let input_before = attempt.input.clone();
            attempt.committed = false;
            if delete_word {
                attempt.input.clear();
            } else {
                attempt.input = attempt.without_commit();
            }
            self.recorded_events.push(RecordedInputEvent {
                at_ms,
                word_index: self.active_word,
                kind: RecordedInputKind::DeleteDelta {
                    deleted_graphemes: grapheme_count(&removed_suffix(
                        &input_before,
                        &attempt.input,
                    ))
                    .try_into()
                    .unwrap_or(u16::MAX),
                    corrected_graphemes: 0,
                    whole_word: delete_word,
                },
            });
            self.character_events.push(CharacterEvent {
                word_index: self.active_word,
                edit: CharacterEdit::Delete(
                    grapheme_count(&removed_suffix(&input_before, &attempt.input))
                        .try_into()
                        .unwrap_or(u16::MAX),
                ),
                correct: None,
                at_ms,
            });
            return;
        }

        let word_index = self.active_word;
        let input_before = self.attempts[word_index].input.clone();
        if delete_word {
            self.attempts[word_index].input.clear();
        } else {
            self.attempts[word_index].pop_grapheme();
        }
        let input_after = self.attempts[word_index].input.clone();
        let target = self.targets[word_index].with_commit();
        let corrected_graphemes = corrected_suffix_count(&input_before, &input_after, &target);
        let deleted_graphemes = grapheme_count(&removed_suffix(&input_before, &input_after))
            .try_into()
            .unwrap_or(u16::MAX);
        self.attempts[word_index].corrections += u32::from(corrected_graphemes);
        self.character_events.push(CharacterEvent {
            word_index,
            edit: CharacterEdit::Delete(deleted_graphemes),
            correct: None,
            at_ms,
        });
        self.recorded_events.push(RecordedInputEvent {
            at_ms,
            word_index,
            kind: RecordedInputKind::DeleteDelta {
                deleted_graphemes,
                corrected_graphemes,
                whole_word: delete_word,
            },
        });
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
    }

    pub fn metrics(&self) -> Metrics {
        Metrics::from_engine(self, self.elapsed_ms())
    }

    /// Tempo da sessão sem reconstruir métricas, históricos ou alocações.
    pub fn elapsed_ms(&self) -> u64 {
        match self.status {
            TestStatus::Ready => 0,
            TestStatus::Running { started_at_ms } => {
                self.current_at_ms.saturating_sub(started_at_ms)
            }
            TestStatus::Completed { ended_at_ms } | TestStatus::Failed { ended_at_ms, .. } => {
                let started_at_ms = match self.status {
                    TestStatus::Running { started_at_ms } => started_at_ms,
                    _ => self
                        .attempts
                        .iter()
                        .filter_map(|attempt| attempt.first_keypress_ms)
                        .min()
                        .unwrap_or(ended_at_ms),
                };
                ended_at_ms.saturating_sub(started_at_ms)
            }
        }
    }

    pub fn reset_with_same_words(&self) -> Self {
        Self::new(
            self.config.clone(),
            self.targets.iter().map(TargetWord::with_commit),
        )
    }

    pub(crate) fn character_stats(&self) -> CharacterStats {
        let include_partial = matches!(self.config.mode, TestMode::Time { .. });
        CharacterStats::from_attempts(&self.targets, &self.attempts, include_partial)
    }

    pub(crate) fn accuracy_counts(&self) -> (u32, u32) {
        self.character_events
            .iter()
            .fold((0, 0), |(correct, incorrect), event| match event.correct {
                Some(true) => (correct + 1, incorrect),
                Some(false) => (correct, incorrect + 1),
                None => (correct, incorrect),
            })
    }

    pub(crate) fn metric_histories(&self, duration_ms: u64) -> (Vec<f64>, Vec<f64>, Vec<u32>) {
        let started_at = self
            .attempts
            .iter()
            .filter_map(|attempt| attempt.first_keypress_ms)
            .min()
            .unwrap_or(0);
        let bucket_count = duration_ms.div_ceil(1_000).max(1) as usize;
        let mut keypresses = vec![0_u32; bucket_count];
        let mut errors = vec![0_u32; bucket_count];
        for event in &self.character_events {
            let elapsed = event.at_ms.saturating_sub(started_at);
            let index = (elapsed.saturating_sub(1) / 1_000) as usize;
            let index = index.min(bucket_count - 1);
            keypresses[index] += 1;
            if event.correct == Some(false) {
                errors[index] += 1;
            }
        }
        let wpm = keypresses
            .into_iter()
            .map(|count| f64::from(count) / 5.0 * 60.0)
            .collect();
        (self.wpm_history(started_at, duration_ms), wpm, errors)
    }

    fn wpm_history(&self, started_at: u64, duration_ms: u64) -> Vec<f64> {
        let bucket_count = duration_ms.div_ceil(1_000).max(1) as usize;
        let mut inputs = vec![String::new(); self.targets.len()];
        let mut event_index = 0;

        (0..bucket_count)
            .map(|bucket_index| {
                let regular_boundary = (bucket_index as u64 + 1) * 1_000;
                let boundary_ms = regular_boundary.min(duration_ms.max(1));
                while let Some(event) = self.character_events.get(event_index) {
                    if event.at_ms.saturating_sub(started_at) > boundary_ms {
                        break;
                    }
                    match &event.edit {
                        CharacterEdit::Insert(grapheme) => {
                            inputs[event.word_index].push_str(grapheme);
                        }
                        CharacterEdit::Delete(count) => {
                            for _ in 0..*count {
                                pop_grapheme(&mut inputs[event.word_index]);
                            }
                        }
                    }
                    event_index += 1;
                }

                let correct_characters = inputs
                    .iter()
                    .rposition(|input| !input.is_empty())
                    .map_or(0, |active_word| {
                        correct_word_characters(&self.targets, &inputs, active_word)
                    });
                let seconds = boundary_ms as f64 / 1_000.0;
                f64::from(correct_characters) / 5.0 / (seconds / 60.0)
            })
            .collect()
    }
}

fn removed_suffix(before: &str, after: &str) -> String {
    before.strip_prefix(after).unwrap_or(before).to_owned()
}

fn corrected_suffix_count(before: &str, after: &str, target: &str) -> u16 {
    let start = grapheme_count(after);
    before
        .graphemes(true)
        .enumerate()
        .skip(start)
        .filter(|(index, grapheme)| grapheme_at(target, *index) != Some(*grapheme))
        .count()
        .try_into()
        .unwrap_or(u16::MAX)
}

fn correct_word_characters(targets: &[TargetWord], inputs: &[String], active_word: usize) -> u32 {
    targets
        .iter()
        .zip(inputs)
        .enumerate()
        .take(active_word + 1)
        .map(|(word_index, (target, input))| {
            let target = target.with_commit();
            let earns_credit = if word_index == active_word {
                target.starts_with(input)
            } else {
                input == &target
            };
            if earns_credit {
                input.graphemes(true).count() as u32
            } else {
                0
            }
        })
        .sum()
}

fn is_separator(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

fn grapheme_count(text: &str) -> usize {
    text.graphemes(true).count()
}

fn grapheme_at(text: &str, index: usize) -> Option<&str> {
    text.graphemes(true).nth(index)
}

fn pop_grapheme(text: &mut String) {
    if let Some((index, _)) = text.grapheme_indices(true).next_back() {
        text.truncate(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(difficulty: Difficulty, words: &[&str]) -> TestEngine {
        let config = TestConfig {
            difficulty,
            mode: TestMode::Words {
                count: words.len() as u16,
            },
            ..TestConfig::default()
        };
        TestEngine::new(config, words.iter().map(|word| (*word).into()))
    }

    #[test]
    fn normal_ignores_a_leading_space() {
        let mut engine = engine(Difficulty::Normal, &["word "]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text(" ".into()),
            at_ms: 10,
        });
        assert_eq!(engine.attempts()[0].input, "");
        assert!(matches!(engine.status(), TestStatus::Running { .. }));
    }

    #[test]
    fn entrada_extra_respeita_o_limite_da_referencia() {
        let mut engine = engine(Difficulty::Normal, &["casa "]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("x".repeat(10_000)),
            at_ms: 10,
        });

        assert_eq!(grapheme_count(&engine.attempts()[0].input), 25);
        assert_eq!(engine.recorded_events().len(), 25);
    }

    #[test]
    fn colagem_registra_apenas_tamanho_e_nunca_o_conteudo() {
        let mut engine = engine(Difficulty::Normal, &["casa "]);
        engine.update(InputEvent::External {
            event: ExternalEvent::Paste {
                text: "segredo-123".into(),
            },
            at_ms: 10,
        });

        assert_eq!(
            engine.recorded_events()[0].kind,
            RecordedInputKind::PasteRedacted { graphemes: 11 }
        );
    }

    #[test]
    fn entrada_unicode_equivalente_e_normalizada_antes_da_comparacao() {
        let mut engine = engine(Difficulty::Master, &["á "]);

        engine.update(InputEvent::Key {
            action: KeyAction::Text("a\u{301}".into()),
            at_ms: 10,
        });

        assert_eq!(engine.attempts()[0].input, "á");
        assert!(matches!(engine.status(), TestStatus::Running { .. }));
    }

    #[test]
    fn motor_preserva_intervalo_bruto_sem_limiar_universal() {
        let mut engine = engine(Difficulty::Normal, &["casa "]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("c".into()),
            at_ms: 10,
        });
        engine.update(InputEvent::Key {
            action: KeyAction::Text("a".into()),
            at_ms: 4_010,
        });
        assert_eq!(engine.attempts()[0].active_ms, 4_000);
        assert_eq!(engine.attempts()[0].afk_ms, 0);
    }

    #[test]
    fn expert_fails_only_when_an_incorrect_word_is_committed() {
        let mut engine = engine(Difficulty::Expert, &["word ", "next "]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("wore".into()),
            at_ms: 10,
        });
        assert!(matches!(engine.status(), TestStatus::Running { .. }));

        let transitions = engine.update(InputEvent::Key {
            action: KeyAction::Text(" ".into()),
            at_ms: 20,
        });
        assert_eq!(engine.attempts()[0].input, "wore ");
        assert!(matches!(
            engine.status(),
            TestStatus::Failed { word_index: 0, .. }
        ));
        assert!(transitions.contains(&Transition::Advanced { from: 0, to: 1 }));
    }

    #[test]
    fn master_fails_on_the_first_incorrect_grapheme() {
        let mut engine = engine(Difficulty::Master, &["word "]);
        let transitions = engine.update(InputEvent::Key {
            action: KeyAction::Text("x".into()),
            at_ms: 10,
        });
        assert!(matches!(
            engine.status(),
            TestStatus::Failed { word_index: 0, .. }
        ));
        assert_eq!(
            transitions,
            vec![Transition::Started, Transition::Failed { word_index: 0 }]
        );
    }

    #[test]
    fn wpm_history_is_cumulative_instead_of_a_per_second_burst() {
        let mut engine = engine(Difficulty::Normal, &["word ", "next "]);
        engine.config.mode = TestMode::Time { seconds: 30 };
        engine.update(InputEvent::Key {
            action: KeyAction::Text("word ".into()),
            at_ms: 100,
        });
        engine.update(InputEvent::Key {
            action: KeyAction::Text("next".into()),
            at_ms: 1_200,
        });
        engine.update(InputEvent::Tick { at_ms: 2_100 });

        let metrics = engine.metrics();
        assert_eq!(metrics.wpm_history, vec![60.0, 54.0]);
        assert_eq!(metrics.burst_history, vec![60.0, 48.0]);
    }

    #[test]
    fn prazo_cronometrado_ignora_entrada_no_limite_e_depois_dele() {
        for at_ms in [30_100, 30_101] {
            let mut engine = engine(Difficulty::Normal, &["casa "]);
            engine.config.mode = TestMode::Time { seconds: 30 };
            engine.update(InputEvent::Key {
                action: KeyAction::Text("c".into()),
                at_ms: 100,
            });

            let transitions = engine.update(InputEvent::Key {
                action: KeyAction::Text("x".into()),
                at_ms,
            });

            assert_eq!(engine.attempts()[0].input, "c");
            assert_eq!(engine.elapsed_ms(), 30_000);
            assert_eq!(
                engine.status(),
                &TestStatus::Completed {
                    ended_at_ms: 30_100
                }
            );
            assert_eq!(transitions, vec![Transition::Completed]);
        }
    }

    #[test]
    fn prazo_cronometrado_aceita_entrada_um_milissegundo_antes_do_limite() {
        let mut engine = engine(Difficulty::Normal, &["casa "]);
        engine.config.mode = TestMode::Time { seconds: 30 };
        engine.update(InputEvent::Key {
            action: KeyAction::Text("c".into()),
            at_ms: 100,
        });

        engine.update(InputEvent::Key {
            action: KeyAction::Text("a".into()),
            at_ms: 30_099,
        });

        assert_eq!(engine.attempts()[0].input, "ca");
        assert!(matches!(engine.status(), TestStatus::Running { .. }));
    }

    #[test]
    fn backspace_returns_to_the_previous_word_without_its_separator() {
        let mut engine = engine(Difficulty::Normal, &["one ", "two "]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("one ".into()),
            at_ms: 10,
        });
        engine.update(InputEvent::Key {
            action: KeyAction::Backspace,
            at_ms: 20,
        });
        assert_eq!(engine.active_word(), 0);
        assert_eq!(engine.attempts()[0].input, "one");
        assert!(!engine.attempts()[0].committed);
    }

    #[test]
    fn delete_word_clears_the_current_word_and_returns_to_the_previous_one() {
        let mut engine = engine(Difficulty::Normal, &["uma ", "palavra "]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("uma pal".into()),
            at_ms: 10,
        });
        engine.update(InputEvent::Key {
            action: KeyAction::DeleteWordBackward,
            at_ms: 20,
        });
        assert_eq!(engine.active_word(), 1);
        assert_eq!(engine.attempts()[1].input, "");

        engine.update(InputEvent::Key {
            action: KeyAction::DeleteWordBackward,
            at_ms: 30,
        });
        assert_eq!(engine.active_word(), 0);
        assert_eq!(engine.attempts()[0].input, "");
        assert!(!engine.attempts()[0].committed);
    }

    #[test]
    fn bounded_test_finishes_on_the_last_character_without_a_separator() {
        let mut engine = engine(Difficulty::Expert, &["one ", "two"]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("one two".into()),
            at_ms: 10,
        });
        assert!(matches!(engine.status(), TestStatus::Completed { .. }));
        assert_eq!(engine.attempts()[1].input, "two");
    }
}
