use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    adaptive::{WordSelection, mechanics_for_token},
    typing::{RecordedInputKind, TestEngine, TestStatus},
};

use super::{MechanicObservationRecord, PersonalBaselineProfile, WordObservationRecord};

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
            let fast_success = attempt.committed
                && !confirmed_error
                && attempt.corrections == 0
                && latency_baseline.is_some_and(|baseline| active_per_grapheme <= baseline * 0.8);
            let slow =
                latency_baseline.is_some_and(|baseline| active_per_grapheme >= baseline * 1.5);
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
                                RecordedInputKind::Insert {
                                    expected: Some(expected),
                                    correct: false,
                                    ..
                                }
                                | RecordedInputKind::InsertDelta {
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
                latency_ratio: latency_baseline.map(|baseline| active_per_grapheme / baseline),
                evidence_weight,
                selection_source: selection.as_ref().map(|selection| selection.source),
                selection_propensity: selection.map(|selection| selection.propensity),
                mechanics,
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
            RecordedInputKind::Insert { .. }
            | RecordedInputKind::Delete { .. }
            | RecordedInputKind::InsertDelta { .. }
            | RecordedInputKind::DeleteDelta { .. } => {
                let current_delete = matches!(
                    event.kind,
                    RecordedInputKind::Delete { .. } | RecordedInputKind::DeleteDelta { .. }
                );
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
            RecordedInputKind::Paste { .. } | RecordedInputKind::PasteRedacted { .. } => {}
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
