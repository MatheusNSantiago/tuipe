use rand::{Rng, seq::IndexedRandom};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::adaptive::{
    AdaptiveSampler, MECHANIC_CAPITALIZATION, MECHANIC_COMMA, MECHANIC_FINAL_PUNCTUATION,
    ReachProfile, SelectionSource, WordSelection,
};

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedWord {
    pub text: String,
    pub selection: Option<WordSelection>,
}

/// Amostragem uniforme do vocabulário configurado.
#[derive(Debug, Clone)]
pub struct UniformWordGenerator<R> {
    words: Vec<String>,
    rng: R,
}

impl<R: Rng> UniformWordGenerator<R> {
    pub fn new(words: &[String], rng: R) -> Self {
        assert!(!words.is_empty(), "a word pack cannot be empty");
        Self {
            words: words.to_vec(),
            rng,
        }
    }

    pub fn next_lexical(&mut self) -> String {
        self.next_lexical_with_provenance().word
    }

    pub fn next_lexical_with_provenance(&mut self) -> WordSelection {
        let candidate = self
            .words
            .choose(&mut self.rng)
            .expect("pacote não vazio")
            .nfc()
            .collect::<String>();
        let propensity = 1.0 / self.words.len() as f64;
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
        reach_probability: f64,
    ) -> WordSelection {
        let selected = sampler.sample_with_provenance_at_reach(
            language,
            &self.words,
            reach_probability,
            &mut self.rng,
        );
        let candidate = selected.word.nfc().collect::<String>();
        WordSelection {
            word: candidate,
            source: selected.source,
            propensity: selected.propensity,
        }
    }

    pub fn rng_mut(&mut self) -> &mut R {
        &mut self.rng
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
    adaptive: Option<(String, AdaptiveSampler, ReachProfile)>,
    position: usize,
}

impl<R: Rng> WordGenerator<R> {
    pub fn new(words: &[String], rng: R, punctuation: bool, numbers: bool) -> Self {
        Self {
            uniform: UniformWordGenerator::new(words, rng),
            punctuation,
            numbers,
            sentence_start: true,
            adaptive: None,
            position: 0,
        }
    }

    pub fn with_adaptive(
        mut self,
        language: impl Into<String>,
        sampler: AdaptiveSampler,
        reach: ReachProfile,
    ) -> Self {
        self.adaptive = Some((language.into(), sampler, reach));
        self
    }

    pub fn next_word(&mut self) -> String {
        self.next_generated().text
    }

    pub fn next_generated(&mut self) -> GeneratedWord {
        let position = self.position;
        self.position = self.position.saturating_add(1);
        if self.numbers && self.uniform.rng_mut().random_bool(0.1) {
            return GeneratedWord {
                text: self.random_number(),
                selection: None,
            };
        }

        let selection = match &self.adaptive {
            Some((language, sampler, reach)) => {
                self.uniform
                    .next_lexical_adaptive(sampler, language, reach.probability(position))
            }
            None => self.uniform.next_lexical_with_provenance(),
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
                .map_or((1.0, 1.0), |(language, sampler, _)| {
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
    fn sampler_uniforme_pode_repetir_quando_o_sorteio_pedir() {
        let words = vec!["um".to_owned()];
        let mut generator = UniformWordGenerator::new(&words, SmallRng::seed_from_u64(7));
        assert_eq!(generator.next_lexical(), "um");
        assert_eq!(generator.next_lexical(), "um");
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
}
