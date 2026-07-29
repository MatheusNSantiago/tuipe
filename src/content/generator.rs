use rand::Rng;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::adaptive::{
    AdaptiveSampler, MECHANIC_CAPITALIZATION, MECHANIC_COMMA, MECHANIC_FINAL_PUNCTUATION,
    ReachProfile, SelectionSource, SessionWordSampler, WordSelection,
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
    remaining: Vec<usize>,
    rng: R,
}

impl<R: Rng> UniformWordGenerator<R> {
    pub fn new(words: &[String], rng: R) -> Self {
        assert!(!words.is_empty(), "a word pack cannot be empty");
        Self {
            words: words.to_vec(),
            remaining: (0..words.len()).collect(),
            rng,
        }
    }

    pub fn next_lexical(&mut self) -> String {
        self.next_lexical_with_provenance().word
    }

    pub fn next_lexical_with_provenance(&mut self) -> WordSelection {
        if self.remaining.is_empty() {
            self.remaining.extend(0..self.words.len());
        }
        let propensity = 1.0 / self.remaining.len() as f64;
        let remaining_index = self.rng.random_range(0..self.remaining.len());
        let word_index = self.remaining.swap_remove(remaining_index);
        let candidate = self.words[word_index].nfc().collect::<String>();
        WordSelection {
            word: candidate,
            source: SelectionSource::Representative,
            propensity,
        }
    }

    pub fn next_lexical_adaptive(
        &mut self,
        distribution: &mut SessionWordSampler,
    ) -> WordSelection {
        let selected = distribution.sample(&self.words, &mut self.rng);
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
struct AdaptiveGeneration {
    language: String,
    sampler: AdaptiveSampler,
    words: SessionWordSampler,
}

#[derive(Debug, Clone)]
pub struct WordGenerator<R> {
    uniform: UniformWordGenerator<R>,
    punctuation: bool,
    numbers: bool,
    sentence_start: bool,
    adaptive: Option<AdaptiveGeneration>,
}

impl<R: Rng> WordGenerator<R> {
    pub fn new(words: &[String], rng: R, punctuation: bool, numbers: bool) -> Self {
        Self {
            uniform: UniformWordGenerator::new(words, rng),
            punctuation,
            numbers,
            sentence_start: true,
            adaptive: None,
        }
    }

    pub fn with_adaptive(
        mut self,
        language: impl Into<String>,
        sampler: AdaptiveSampler,
        reach: ReachProfile,
    ) -> Self {
        let language = language.into();
        let number_probability = if self.numbers { 0.1 } else { 0.0 };
        let distribution = sampler.session_word_sampler(
            &language,
            &self.uniform.words,
            &reach,
            number_probability,
        );
        self.adaptive = Some(AdaptiveGeneration {
            language,
            sampler,
            words: distribution,
        });
        self
    }

    pub fn next_word(&mut self) -> String {
        self.next_generated().text
    }

    pub fn next_generated(&mut self) -> GeneratedWord {
        if self.numbers && self.uniform.rng_mut().random_bool(0.1) {
            return GeneratedWord {
                text: self.random_number(),
                selection: None,
            };
        }

        let selection = match &mut self.adaptive {
            Some(adaptive) => self.uniform.next_lexical_adaptive(&mut adaptive.words),
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

        let (final_boost, comma_boost) = self.adaptive.as_ref().map_or((1.0, 1.0), |adaptive| {
            (
                adaptive
                    .sampler
                    .mechanic_boost(&adaptive.language, MECHANIC_FINAL_PUNCTUATION)
                    .max(
                        adaptive
                            .sampler
                            .mechanic_boost(&adaptive.language, MECHANIC_CAPITALIZATION),
                    ),
                adaptive
                    .sampler
                    .mechanic_boost(&adaptive.language, MECHANIC_COMMA),
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
    fn sampler_uniforme_esgota_o_vocabulario_antes_de_reiniciar() {
        let words = ["um", "dois", "três", "quatro"].map(str::to_owned).to_vec();
        let mut generator = UniformWordGenerator::new(&words, SmallRng::seed_from_u64(7));
        let first_cycle = (0..words.len())
            .map(|_| generator.next_lexical())
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(first_cycle.len(), words.len());
        assert!(words.contains(&generator.next_lexical()));
    }

    #[test]
    fn gerador_adaptativo_nao_repete_antes_de_esgotar_o_vocabulario() {
        let words = (0..200)
            .map(|index| format!("palavra{index}"))
            .collect::<Vec<_>>();
        let mut sampler = AdaptiveSampler::default();
        for _ in 0..24 {
            sampler.observe(
                "portuguese",
                &words[0],
                crate::adaptive::Observation::regular(true, false, false),
            );
        }
        let mut generator = WordGenerator::new(&words, SmallRng::seed_from_u64(9), false, false)
            .with_adaptive("portuguese", sampler, ReachProfile::certain(120));
        let generated = (0..words.len())
            .map(|_| generator.next_word())
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(generated.len(), words.len());
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
