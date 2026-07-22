use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    adaptive::{WordSelection, correction_burden, mechanics_for_token},
    typing::{RecordedInputKind, TestEngine, TestStatus},
};

use super::{
    MechanicObservationRecord, PatternObservationRecord, PersonalBaselineProfile,
    WordObservationRecord,
};

#[derive(Debug, Clone, Copy, Default)]
struct WordTiming {
    fluent_ms: u64,
    correction_ms: u64,
    planning_ms: u64,
    afk_ms: u64,
    input_events: u16,
    corrective_events: u16,
}

impl WordTiming {
    fn execution_ms(self) -> u64 {
        self.fluent_ms.saturating_add(self.correction_ms)
    }
}

/// Materializa as evidências consultáveis usando apenas o estado reproduzível
/// do motor, o baseline anterior à sessão e a proveniência da amostragem.
pub fn derive_word_observations(
    engine: &TestEngine,
    baseline: &PersonalBaselineProfile,
    repeated_test: bool,
    interrupted: bool,
    selections: &[Option<WordSelection>],
) -> Vec<WordObservationRecord> {
    let timings = word_timings(engine);
    let mut occurrences = HashMap::<String, usize>::new();
    for (word_index, (target, attempt)) in
        engine.targets().iter().zip(engine.attempts()).enumerate()
    {
        let terminal_failure = matches!(
            engine.status(),
            TestStatus::Failed { word_index: failed_index, .. } if *failed_index == word_index
        );
        let censored = !attempt.committed
            && !terminal_failure
            && (interrupted || matches!(engine.status(), TestStatus::Completed { .. }))
            && !attempt.input.is_empty();
        if (attempt.committed || terminal_failure || censored)
            && let Some(word) = lexical_word(&target.text)
        {
            *occurrences.entry(word).or_default() += 1;
        }
    }

    engine
        .targets()
        .iter()
        .enumerate()
        .zip(engine.attempts())
        .filter_map(|((word_index, target), attempt)| {
            let terminal_failure = matches!(
                engine.status(),
                TestStatus::Failed { word_index: failed_index, .. } if *failed_index == word_index
            );
            let censored = !attempt.committed
                && !terminal_failure
                && (interrupted || matches!(engine.status(), TestStatus::Completed { .. }))
                && !attempt.input.is_empty();
            if !attempt.committed && !terminal_failure && !censored {
                return None;
            }
            let word = lexical_word(&target.text)?;
            let timing = timings.get(word_index).copied().unwrap_or_default();
            let active_ms = timing.execution_ms();
            let grapheme_count = word.graphemes(true).count().try_into().unwrap_or(u16::MAX);
            let active_per_grapheme = active_ms as f64 / f64::from(grapheme_count.max(1));
            let latency_baseline = baseline.latency_ms_per_grapheme(grapheme_count);
            let typed = attempt.without_commit();
            let expected_prefix = target
                .text
                .graphemes(true)
                .take(typed.graphemes(true).count())
                .collect::<String>();
            let confirmed_error = terminal_failure
                || (attempt.committed && typed != target.text)
                || (censored && typed != expected_prefix);
            let evidence_weight = if repeated_test || (censored && !confirmed_error) {
                0.0
            } else {
                let occurrence_weight =
                    1.0 / occurrences.get(&word).copied().unwrap_or(1).max(1) as f64;
                if censored {
                    let observed_fraction =
                        typed.graphemes(true).count() as f64 / f64::from(grapheme_count.max(1));
                    occurrence_weight * observed_fraction.min(1.0) * 0.5
                } else {
                    occurrence_weight
                }
            };
            let has_timing_evidence = evidence_weight > 0.0 && !censored;
            let fast_success = has_timing_evidence
                && attempt.committed
                && !confirmed_error
                && attempt.corrections == 0
                && latency_baseline.is_some_and(|baseline| active_per_grapheme <= baseline * 0.8);
            let slow = has_timing_evidence
                && latency_baseline.is_some_and(|baseline| active_per_grapheme >= baseline * 1.5);
            let selection = (!repeated_test)
                .then(|| selections.get(word_index).cloned().flatten())
                .flatten();
            let final_mechanics = mechanics_for_token(&attempt.without_commit());
            let mechanics = mechanics_for_token(&target.text)
                .into_iter()
                .map(|mechanic| {
                    let had_mistake = engine.recorded_events().iter().any(|event| {
                        event.word_index == word_index
                            && matches!(
                                &event.kind,
                                RecordedInputKind::InsertDelta {
                                    expected: Some(expected),
                                    correct: false,
                                    ..
                                } if mechanics_for_token(expected).contains(&mechanic)
                            )
                    });
                    let present_at_end = final_mechanics.contains(&mechanic);
                    MechanicObservationRecord {
                        mechanic,
                        confirmed_error: !present_at_end,
                        corrected: had_mistake && present_at_end,
                    }
                })
                .collect();
            let burden = correction_burden(
                attempt.corrections,
                timing.corrective_events,
                timing.correction_ms,
                timing.fluent_ms,
                grapheme_count,
            );
            Some(WordObservationRecord {
                language: engine.config().language.clone(),
                word,
                confirmed_error,
                corrections: attempt.corrections,
                active_ms,
                afk_ms: timing.afk_ms,
                planning_ms: timing.planning_ms,
                fluent_ms: timing.fluent_ms,
                correction_ms: timing.correction_ms,
                input_events: timing.input_events,
                corrective_events: timing.corrective_events,
                censored,
                grapheme_count,
                fast_success,
                slow,
                latency_ratio: has_timing_evidence
                    .then(|| latency_baseline.map(|baseline| active_per_grapheme / baseline))
                    .flatten(),
                evidence_weight,
                selection_source: selection.as_ref().map(|selection| selection.source),
                selection_propensity: selection.map(|selection| selection.propensity),
                mechanics,
                patterns: pattern_observations(engine, word_index, &target.text, burden),
            })
        })
        .collect()
}

fn word_timings(engine: &TestEngine) -> Vec<WordTiming> {
    #[derive(Clone, Copy)]
    struct Gap {
        word_index: usize,
        elapsed_ms: u64,
        interrupted: bool,
        same_word: bool,
        correction: bool,
    }

    let mut gaps = Vec::new();
    let mut previous_key = None::<(u64, usize, bool)>;
    let mut interrupted = false;
    let mut event_counts = vec![(0_u16, 0_u16); engine.targets().len()];
    for event in engine.recorded_events() {
        match &event.kind {
            RecordedInputKind::Focus { gained } => {
                if !gained {
                    interrupted = true;
                }
            }
            RecordedInputKind::InsertDelta { .. } | RecordedInputKind::DeleteDelta { .. } => {
                let current_delete = matches!(event.kind, RecordedInputKind::DeleteDelta { .. });
                let Some(counts) = event_counts.get_mut(event.word_index) else {
                    continue;
                };
                counts.0 = counts.0.saturating_add(1);
                counts.1 = counts.1.saturating_add(u16::from(current_delete));
                if let Some((previous_at, previous_word, previous_delete)) = previous_key {
                    gaps.push(Gap {
                        word_index: event.word_index,
                        elapsed_ms: event.at_ms.saturating_sub(previous_at),
                        interrupted,
                        same_word: previous_word == event.word_index,
                        correction: current_delete || previous_delete,
                    });
                }
                previous_key = Some((event.at_ms, event.word_index, current_delete));
                interrupted = false;
            }
            RecordedInputKind::PasteRedacted { .. } => {}
        }
    }

    let mut log_intervals = gaps
        .iter()
        .filter(|gap| gap.same_word && !gap.interrupted && gap.elapsed_ms > 0)
        .map(|gap| (gap.elapsed_ms as f64).ln())
        .collect::<Vec<_>>();
    let pause_threshold = if log_intervals.len() >= 12 {
        let median_value = median(&mut log_intervals);
        let mut deviations = log_intervals
            .iter()
            .map(|value| (value - median_value).abs())
            .collect::<Vec<_>>();
        let mad = median(&mut deviations);
        (mad > f64::EPSILON).then_some(median_value + 3.5 * 1.4826 * mad)
    } else {
        None
    };

    let mut timings = vec![WordTiming::default(); engine.targets().len()];
    for gap in gaps {
        let is_pause = gap.interrupted
            || pause_threshold.is_some_and(|threshold| {
                gap.elapsed_ms > 0 && (gap.elapsed_ms as f64).ln() > threshold
            });
        let Some(timing) = timings.get_mut(gap.word_index) else {
            continue;
        };
        if is_pause {
            timing.afk_ms = timing.afk_ms.saturating_add(gap.elapsed_ms);
        } else if !gap.same_word {
            timing.planning_ms = timing.planning_ms.saturating_add(gap.elapsed_ms);
        } else if gap.correction {
            timing.correction_ms = timing.correction_ms.saturating_add(gap.elapsed_ms);
        } else {
            timing.fluent_ms = timing.fluent_ms.saturating_add(gap.elapsed_ms);
        }
    }
    for (timing, (input_events, corrective_events)) in timings.iter_mut().zip(event_counts) {
        timing.input_events = input_events;
        timing.corrective_events = corrective_events;
    }
    timings
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn lexical_word(value: &str) -> Option<String> {
    let lexical = value
        .trim_matches(|character: char| !character.is_alphabetic())
        .to_lowercase();
    (!lexical.is_empty()).then_some(lexical)
}

fn pattern_observations(
    engine: &TestEngine,
    word_index: usize,
    target: &str,
    correction_burden: f64,
) -> Vec<PatternObservationRecord> {
    #[derive(Clone, Copy)]
    struct BufferedGrapheme {
        target_index: usize,
        correct: bool,
    }

    let mut buffer = Vec::<BufferedGrapheme>::new();
    let mut corrected_positions = std::collections::HashSet::<usize>::new();
    for event in engine
        .recorded_events()
        .iter()
        .filter(|event| event.word_index == word_index)
    {
        match &event.kind {
            RecordedInputKind::InsertDelta { correct, .. } => {
                buffer.push(BufferedGrapheme {
                    target_index: buffer.len(),
                    correct: *correct,
                });
            }
            RecordedInputKind::DeleteDelta {
                deleted_graphemes, ..
            } => {
                for _ in 0..*deleted_graphemes {
                    let Some(deleted) = buffer.pop() else {
                        break;
                    };
                    if !deleted.correct {
                        corrected_positions.insert(deleted.target_index);
                    }
                }
            }
            RecordedInputKind::Focus { .. } | RecordedInputKind::PasteRedacted { .. } => {}
        }
    }
    let failed_positions = buffer
        .iter()
        .filter(|grapheme| !grapheme.correct)
        .map(|grapheme| grapheme.target_index)
        .collect::<std::collections::HashSet<_>>();

    let target_graphemes = target.graphemes(true).collect::<Vec<_>>();
    let lexical_start = target_graphemes
        .iter()
        .position(|grapheme| grapheme.chars().any(char::is_alphabetic))
        .unwrap_or(0);
    let lexical_end = target_graphemes
        .iter()
        .rposition(|grapheme| grapheme.chars().any(char::is_alphabetic))
        .map_or(lexical_start, |index| index + 1);
    let lexical = &target_graphemes[lexical_start..lexical_end];
    let mut patterns = HashMap::<String, (bool, bool)>::new();
    for size in 2..=3 {
        for (offset, window) in lexical.windows(size).enumerate() {
            let mut positions = lexical_start + offset..lexical_start + offset + size;
            let confirmed_error = positions
                .clone()
                .any(|position| failed_positions.contains(&position));
            let corrected = positions.any(|position| corrected_positions.contains(&position));
            let pattern = window.concat().to_lowercase();
            patterns
                .entry(pattern)
                .and_modify(|evidence| {
                    evidence.0 |= confirmed_error;
                    evidence.1 |= corrected;
                })
                .or_insert((confirmed_error, corrected));
        }
    }
    let mut patterns = patterns
        .into_iter()
        .map(
            |(pattern, (confirmed_error, corrected))| PatternObservationRecord {
                pattern,
                confirmed_error,
                corrected,
                correction_burden: if corrected { correction_burden } else { 0.0 },
            },
        )
        .collect::<Vec<_>>();
    patterns.sort_by(|left, right| left.pattern.cmp(&right.pattern));
    patterns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::{Difficulty, InputEvent, KeyAction, TestConfig, TestMode};

    #[test]
    fn correcao_so_afeta_sequencias_que_cruzam_o_caractere_refeito() {
        let config = TestConfig {
            mode: TestMode::Words { count: 1 },
            difficulty: Difficulty::Normal,
            ..TestConfig::default()
        };
        let mut engine = TestEngine::new(config, ["criança ".into()]);
        for (index, grapheme) in ["c", "r", "i", "a", "n", "x"].into_iter().enumerate() {
            engine.update(InputEvent::Key {
                action: KeyAction::Text(grapheme.into()),
                at_ms: 100 + index as u64 * 100,
            });
        }
        engine.update(InputEvent::Key {
            action: KeyAction::Backspace,
            at_ms: 750,
        });
        for (at_ms, grapheme) in [(850, "ç"), (950, "a"), (1_050, " ")] {
            engine.update(InputEvent::Key {
                action: KeyAction::Text(grapheme.into()),
                at_ms,
            });
        }

        let observation = derive_word_observations(
            &engine,
            &PersonalBaselineProfile::default(),
            false,
            false,
            &[],
        )
        .remove(0);
        let corrected = observation
            .patterns
            .iter()
            .filter(|pattern| pattern.corrected)
            .map(|pattern| pattern.pattern.as_str())
            .collect::<Vec<_>>();

        assert!(corrected.contains(&"nç"));
        assert!(corrected.contains(&"nça"));
        assert!(!corrected.contains(&"cri"));
        assert!(!observation.confirmed_error);
        assert_eq!(observation.corrections, 1);
    }
}
