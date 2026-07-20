use unicode_segmentation::UnicodeSegmentation;

use super::{TargetWord, TestEngine, WordAttempt};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CharacterStats {
    pub all_correct: u32,
    pub correct_word: u32,
    pub incorrect: u32,
    pub extra: u32,
    pub missed: u32,
}

impl CharacterStats {
    pub(crate) fn from_attempts(
        targets: &[TargetWord],
        attempts: &[WordAttempt],
        include_partial: bool,
    ) -> Self {
        targets
            .iter()
            .zip(attempts)
            .filter(|(_, attempt)| {
                attempt.committed || include_partial && !attempt.input.is_empty()
            })
            .fold(Self::default(), |mut total, (target, attempt)| {
                let current = count_chars(&attempt.input, &target.with_commit(), include_partial);
                total.all_correct += current.all_correct;
                total.correct_word += current.correct_word;
                total.incorrect += current.incorrect;
                total.extra += current.extra;
                total.missed += current.missed;
                total
            })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    pub duration_ms: u64,
    pub wpm: f64,
    pub raw_wpm: f64,
    pub accuracy: f64,
    pub consistency: f64,
    pub characters: CharacterStats,
    pub wpm_history: Vec<f64>,
    pub raw_wpm_history: Vec<f64>,
    pub burst_history: Vec<f64>,
    pub error_history: Vec<u32>,
}

impl Metrics {
    pub(super) fn from_engine(engine: &TestEngine, duration_ms: u64) -> Self {
        let characters = engine.character_stats();
        let (correct_keypresses, incorrect_keypresses) = engine.accuracy_counts();
        let seconds = duration_ms as f64 / 1_000.0;
        let accuracy = if correct_keypresses + incorrect_keypresses == 0 {
            0.0
        } else {
            correct_keypresses as f64 / (correct_keypresses + incorrect_keypresses) as f64 * 100.0
        };
        let (wpm_history, raw_wpm_history, burst_history, error_history) =
            engine.metric_histories(duration_ms);
        let consistency = consistency(&burst_history);
        Self {
            duration_ms,
            wpm: round2(calculate_wpm(characters.correct_word, seconds)),
            raw_wpm: round2(calculate_wpm(
                characters.all_correct + characters.incorrect + characters.extra,
                seconds,
            )),
            accuracy: round2(accuracy),
            consistency: round2(consistency),
            characters,
            wpm_history,
            raw_wpm_history,
            burst_history,
            error_history,
        }
    }
}

fn consistency(history: &[f64]) -> f64 {
    if history.is_empty() {
        return 0.0;
    }
    let mean = history.iter().sum::<f64>() / history.len() as f64;
    if mean <= 0.0 {
        return 0.0;
    }
    let variance = history
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / history.len() as f64;
    let coefficient = variance.sqrt() / mean;
    100.0 * (1.0 - (coefficient + coefficient.powi(3) / 3.0 + coefficient.powi(5) / 5.0).tanh())
}

/// Unidade exata do Monkeytype: cinco caracteres por palavra.
pub fn calculate_wpm(character_count: u32, duration_seconds: f64) -> f64 {
    if duration_seconds <= 0.0 {
        return 0.0;
    }
    character_count as f64 / 5.0 / (duration_seconds / 60.0)
}

fn count_chars(input: &str, target: &str, credit_partial: bool) -> CharacterStats {
    let input: Vec<_> = input.graphemes(true).collect();
    let target: Vec<_> = target.graphemes(true).collect();
    let word_correct = input == target;
    let word_partially_correct = target.starts_with(&input);
    let mut stats = CharacterStats::default();

    for index in 0..input.len().max(target.len()) {
        match (input.get(index), target.get(index)) {
            (Some(actual), Some(expected)) if actual == expected => {
                if *expected == " " && !word_correct {
                    stats.extra += 1;
                } else {
                    stats.all_correct += 1;
                }
                if word_correct || credit_partial && word_partially_correct {
                    stats.correct_word += 1;
                }
            }
            (None, Some(_)) if !credit_partial => stats.missed += 1,
            (Some(_), None) => {
                stats.extra += 1;
            }
            (Some(actual), Some(expected))
                if *expected == " " && *actual != " " && !input.contains(&" ") =>
            {
                stats.extra += 1;
            }
            (Some(_), Some(_)) => stats.incorrect += 1,
            (None, Some(_)) | (None, None) => {}
        }
    }
    stats
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_after_a_wrong_word_is_extra_not_correct() {
        let chars = count_chars("worx ", "word ", false);
        assert_eq!(chars.all_correct, 3);
        assert_eq!(chars.incorrect, 1);
        assert_eq!(chars.extra, 1);
    }

    #[test]
    fn timed_partial_prefix_earns_wpm_credit() {
        let chars = count_chars("wor", "word ", true);
        assert_eq!(chars.correct_word, 3);
        assert_eq!(chars.missed, 0);
    }

    #[test]
    fn consistency_matches_monkeytypes_kogasa_mapping() {
        assert_eq!(consistency(&[60.0, 60.0, 60.0]), 100.0);
        assert!(consistency(&[20.0, 60.0, 100.0]) < 100.0);
    }
}
