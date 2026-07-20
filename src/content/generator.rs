use std::collections::VecDeque;

use rand::{Rng, seq::IndexedRandom};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::adaptive::{
    AdaptiveSampler, MECHANIC_CAPITALIZATION, MECHANIC_COMMA, MECHANIC_FINAL_PUNCTUATION,
    SelectionSource, WordSelection,
};

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedWord {
    pub text: String,
    pub selection: Option<WordSelection>,
}

/// Amostragem uniforme com a mesma proteção das duas palavras anteriores do Monkeytype.
#[derive(Debug, Clone)]
pub struct UniformWordGenerator<R> {
    words: Vec<String>,
    rng: R,
    previous: [Option<String>; 2],
    anchor_position: usize,
}

impl<R: Rng> UniformWordGenerator<R> {
    pub fn new(words: &[String], rng: R) -> Self {
        assert!(!words.is_empty(), "a word pack cannot be empty");
        Self {
            words: words.to_vec(),
            rng,
            previous: [None, None],
            anchor_position: 0,
        }
    }

    pub fn next_lexical(&mut self) -> String {
        self.next_lexical_with_provenance().word
    }

    pub fn next_lexical_with_provenance(&mut self) -> WordSelection {
        let eligible = self
            .words
            .iter()
            .filter(|word| {
                !self
                    .previous
                    .iter()
                    .flatten()
                    .any(|previous| previous == *word)
            })
            .collect::<Vec<_>>();
        let eligible = if eligible.is_empty() {
            self.words.iter().collect::<Vec<_>>()
        } else {
            eligible
        };
        let candidate = eligible
            .choose(&mut self.rng)
            .expect("pacote não vazio")
            .nfc()
            .collect::<String>();
        let propensity = 1.0 / eligible.len() as f64;
        self.previous.rotate_right(1);
        self.previous[0] = Some(candidate.clone());
        WordSelection {
            word: candidate,
            source: SelectionSource::Representative,
            propensity,
        }
    }

    /// Forma âncora estratificada por faixa de frequência do pack e tamanho.
    /// O ciclo cobre doze células antes de repetir e nunca consulta habilidade.
    pub fn next_anchor(&mut self) -> WordSelection {
        let rank_band = self.anchor_position % 4;
        let length_band = (self.anchor_position * 2) % 3;
        self.anchor_position = self.anchor_position.saturating_add(1);
        let band_start = self.words.len() * rank_band / 4;
        let band_end = self.words.len() * (rank_band + 1) / 4;
        let matches_length = |word: &str| match length_band {
            0 => word.graphemes(true).count() <= 4,
            1 => (5..=7).contains(&word.graphemes(true).count()),
            _ => word.graphemes(true).count() >= 8,
        };
        let mut eligible = self
            .words
            .iter()
            .enumerate()
            .filter(|(index, word)| {
                (band_start..band_end).contains(index)
                    && matches_length(word)
                    && !self
                        .previous
                        .iter()
                        .flatten()
                        .any(|previous| previous == *word)
            })
            .map(|(_, word)| word)
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            eligible = self
                .words
                .iter()
                .enumerate()
                .filter(|(index, word)| {
                    (band_start..band_end).contains(index)
                        && !self
                            .previous
                            .iter()
                            .flatten()
                            .any(|previous| previous == *word)
                })
                .map(|(_, word)| word)
                .collect();
        }
        if eligible.is_empty() {
            return self.next_lexical_with_provenance();
        }
        let candidate = eligible
            .choose(&mut self.rng)
            .expect("estrato não vazio")
            .nfc()
            .collect::<String>();
        let propensity = 1.0 / eligible.len() as f64;
        self.previous.rotate_right(1);
        self.previous[0] = Some(candidate.clone());
        WordSelection {
            word: candidate,
            source: SelectionSource::Representative,
            propensity,
        }
    }

    pub fn next_lexical_adaptive(
        &mut self,
        sampler: &AdaptiveSampler,
        language: &str,
    ) -> WordSelection {
        let previous = self
            .previous
            .iter()
            .flatten()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let selected =
            sampler.sample_with_provenance(language, &self.words, &previous, &mut self.rng);
        let candidate = selected.word.nfc().collect::<String>();
        self.previous.rotate_right(1);
        self.previous[0] = Some(candidate.clone());
        WordSelection {
            word: candidate,
            source: selected.source,
            propensity: selected.propensity,
        }
    }

    pub fn rng_mut(&mut self) -> &mut R {
        &mut self.rng
    }

    fn force(&mut self, mut selection: WordSelection) -> WordSelection {
        selection.word = selection.word.nfc().collect();
        self.previous.rotate_right(1);
        self.previous[0] = Some(selection.word.clone());
        selection
    }
}

/// Aplica os modificadores do Monkeytype suportados pelo tuipe a um fluxo léxico
/// uniforme ou adaptativo. A escolha léxica continua isolada dos modificadores.
#[derive(Debug, Clone)]
pub struct WordGenerator<R> {
    uniform: UniformWordGenerator<R>,
    punctuation: bool,
    numbers: bool,
    sentence_start: bool,
    adaptive: Option<(String, AdaptiveSampler)>,
    assessment: bool,
    forced: VecDeque<WordSelection>,
}

impl<R: Rng> WordGenerator<R> {
    pub fn new(words: &[String], rng: R, punctuation: bool, numbers: bool) -> Self {
        Self {
            uniform: UniformWordGenerator::new(words, rng),
            punctuation,
            numbers,
            sentence_start: true,
            adaptive: None,
            assessment: false,
            forced: VecDeque::new(),
        }
    }

    pub fn with_adaptive(mut self, language: impl Into<String>, sampler: AdaptiveSampler) -> Self {
        self.adaptive = Some((language.into(), sampler));
        self
    }

    pub fn with_assessment(mut self) -> Self {
        self.assessment = true;
        self
    }

    pub fn with_forced_words(mut self, words: Vec<String>) -> Self {
        self.forced = words
            .into_iter()
            .map(|word| WordSelection {
                word,
                source: SelectionSource::Targeted,
                propensity: 1.0,
            })
            .collect();
        self
    }

    pub fn next_word(&mut self) -> String {
        self.next_generated().text
    }

    pub fn next_generated(&mut self) -> GeneratedWord {
        if self.forced.is_empty() && self.numbers && self.uniform.rng_mut().random_bool(0.1) {
            return GeneratedWord {
                text: self.random_number(),
                selection: None,
            };
        }

        let selection = if let Some(selection) = self.forced.pop_front() {
            self.uniform.force(selection)
        } else {
            match (&self.adaptive, self.assessment) {
                (_, true) => self.uniform.next_anchor(),
                (Some((language, sampler)), false) => {
                    self.uniform.next_lexical_adaptive(sampler, language)
                }
                (None, false) => self.uniform.next_lexical_with_provenance(),
            }
        };
        let mut word = selection.word.clone();
        if !self.punctuation {
            return GeneratedWord {
                text: word,
                selection: Some(selection),
            };
        }

        if self.sentence_start {
            word = capitalize_first_grapheme(&word);
        }

        let (final_boost, comma_boost) =
            self.adaptive
                .as_ref()
                .map_or((1.0, 1.0), |(language, sampler)| {
                    (
                        sampler
                            .mechanic_boost(language, MECHANIC_FINAL_PUNCTUATION)
                            .max(sampler.mechanic_boost(language, MECHANIC_CAPITALIZATION)),
                        sampler.mechanic_boost(language, MECHANIC_COMMA),
                    )
                });
        let final_chance = (0.1 * final_boost).min(0.15);
        let comma_chance = (0.01 * comma_boost).min(0.015);
        let roll: f64 = self.uniform.rng_mut().random();
        if roll < final_chance {
            let position = roll / final_chance;
            let punctuation = if position <= 0.8 {
                '.'
            } else if position < 0.9 {
                '?'
            } else {
                '!'
            };
            word.push(punctuation);
            self.sentence_start = true;
        } else if roll < final_chance + comma_chance {
            word.push(',');
            self.sentence_start = false;
        } else {
            self.sentence_start = false;
        }
        GeneratedWord {
            text: word,
            selection: Some(selection),
        }
    }

    fn random_number(&mut self) -> String {
        let digits = self.uniform.rng_mut().random_range(1..=4);
        (0..digits)
            .map(|index| {
                let lower = if index == 0 { 1 } else { 0 };
                char::from_digit(self.uniform.rng_mut().random_range(lower..=9), 10)
                    .expect("decimal digit")
            })
            .collect()
    }
}

fn capitalize_first_grapheme(word: &str) -> String {
    let mut graphemes = word.graphemes(true);
    let Some(first) = graphemes.next() else {
        return String::new();
    };
    format!("{}{}", first.to_uppercase(), graphemes.collect::<String>())
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::SmallRng};

    use super::*;

    #[test]
    fn uniform_sampler_does_not_repeat_one_of_the_two_previous_words() {
        let words = ["um", "dois", "três", "quatro"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut generator = UniformWordGenerator::new(&words, SmallRng::seed_from_u64(7));
        let mut produced = Vec::new();
        for _ in 0..100 {
            let word = generator.next_lexical();
            assert!(
                !produced
                    .iter()
                    .rev()
                    .take(2)
                    .any(|previous| previous == &word)
            );
            produced.push(word);
        }
    }

    #[test]
    fn numbers_have_one_to_four_digits() {
        let words = vec!["word".into()];
        let mut generator = WordGenerator::new(&words, SmallRng::seed_from_u64(3), false, true);
        for _ in 0..100 {
            let word = generator.next_word();
            if word.chars().all(char::is_numeric) {
                assert!((1..=4).contains(&word.len()));
                assert_ne!(word.chars().next(), Some('0'));
            }
        }
    }

    #[test]
    fn palavras_de_revisao_aparecem_antes_do_fluxo_uniforme() {
        let words = vec!["casa".into(), "tempo".into(), "mundo".into()];
        let mut generator = WordGenerator::new(&words, SmallRng::seed_from_u64(3), false, true)
            .with_forced_words(vec!["tempo".into(), "casa".into()]);

        for expected in ["tempo", "casa"] {
            let generated = generator.next_generated();
            assert_eq!(generated.text, expected);
            assert_eq!(
                generated.selection.unwrap().source,
                SelectionSource::Targeted
            );
        }
    }

    #[test]
    fn avaliacao_ancora_cobre_frequencia_e_tamanho_sem_habilidade() {
        let mut words = Vec::new();
        for band in 0..4 {
            words.extend((0..4).map(|index| format!("a{band}{index}")));
            words.extend((0..4).map(|index| format!("medio{band}{index}")));
            words.extend((0..4).map(|index| format!("comprida{band}{index}")));
        }
        let mut generator = UniformWordGenerator::new(&words, SmallRng::seed_from_u64(9));
        for position in 0..12 {
            let selected = generator.next_anchor();
            let index = words
                .iter()
                .position(|word| word == &selected.word)
                .unwrap();
            assert_eq!(index / 12, position % 4);
            let expected_length_band = (position * 2) % 3;
            let length = selected.word.graphemes(true).count();
            assert!(match expected_length_band {
                0 => length <= 4,
                1 => (5..=7).contains(&length),
                _ => length >= 8,
            });
            assert_eq!(selected.source, SelectionSource::Representative);
        }
    }
}
