use rand::{Rng, seq::IndexedRandom};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// Amostragem uniforme com a mesma proteção das duas palavras anteriores do Monkeytype.
#[derive(Debug, Clone)]
pub struct UniformWordGenerator<R> {
    words: Vec<String>,
    rng: R,
    previous: [Option<String>; 2],
}

impl<R: Rng> UniformWordGenerator<R> {
    pub fn new(words: &[String], rng: R) -> Self {
        assert!(!words.is_empty(), "a word pack cannot be empty");
        Self {
            words: words.to_vec(),
            rng,
            previous: [None, None],
        }
    }

    pub fn next_lexical(&mut self) -> String {
        let mut candidate = self
            .words
            .choose(&mut self.rng)
            .expect("nonempty pack")
            .nfc()
            .collect::<String>();
        for _ in 0..100 {
            if !self
                .previous
                .iter()
                .flatten()
                .any(|word| word == &candidate)
            {
                break;
            }
            candidate = self
                .words
                .choose(&mut self.rng)
                .expect("nonempty pack")
                .nfc()
                .collect();
        }
        self.previous.rotate_right(1);
        self.previous[0] = Some(candidate.clone());
        candidate
    }

    pub fn rng_mut(&mut self) -> &mut R {
        &mut self.rng
    }
}

/// Aplica os modificadores do Monkeytype suportados pelo tuipe a um fluxo léxico
/// uniforme. A seleção adaptativa fica intencionalmente em outro sampler.
#[derive(Debug, Clone)]
pub struct WordGenerator<R> {
    uniform: UniformWordGenerator<R>,
    punctuation: bool,
    numbers: bool,
    sentence_start: bool,
}

impl<R: Rng> WordGenerator<R> {
    pub fn new(words: &[String], rng: R, punctuation: bool, numbers: bool) -> Self {
        Self {
            uniform: UniformWordGenerator::new(words, rng),
            punctuation,
            numbers,
            sentence_start: true,
        }
    }

    pub fn next_word(&mut self) -> String {
        if self.numbers && self.uniform.rng_mut().random_bool(0.1) {
            return self.random_number();
        }

        let mut word = self.uniform.next_lexical();
        if !self.punctuation {
            return word;
        }

        if self.sentence_start {
            word = capitalize_first_grapheme(&word);
        }

        let roll: f64 = self.uniform.rng_mut().random();
        if roll < 0.1 {
            let punctuation = if roll <= 0.08 {
                '.'
            } else if roll < 0.09 {
                '?'
            } else {
                '!'
            };
            word.push(punctuation);
            self.sentence_start = true;
        } else if roll < 0.11 {
            word.push(',');
            self.sentence_start = false;
        } else {
            self.sentence_start = false;
        }
        word
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
}
