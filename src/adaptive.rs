//! Inferência de dificuldade e seleção automática orientadas por exposições.
//!
//! O modelo separa taxa, incerteza e utilidade de treino. SQLite materializa
//! [`WordSkill`]; o sampler só recebe estado restaurado e um RNG injetável.

use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

use rand::{
    Rng, SeedableRng,
    distr::{Distribution, weighted::WeightedIndex},
    rngs::SmallRng,
};
use serde::{Deserialize, Serialize};
use statrs::distribution::{Beta, ContinuousCDF};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptivePolicy {
    /// Força do prior pessoal em exposições equivalentes.
    pub prior_strength: f64,
    /// Excesso mínimo de taxa que precisa ser educacionalmente relevante.
    pub minimum_error_effect: f64,
    pub minimum_correction_effect: f64,
    pub corrected_error_cost: f64,
    pub latency_cost: f64,
    pub maximum_boost: f64,
    pub representative_share: f64,
    pub targeted_share: f64,
    pub exploration_share: f64,
    pub transfer_share: f64,
}

impl Default for AdaptivePolicy {
    fn default() -> Self {
        Self {
            prior_strength: 24.0,
            minimum_error_effect: 0.02,
            minimum_correction_effect: 0.03,
            corrected_error_cost: 0.35,
            latency_cost: 0.18,
            maximum_boost: 3.0,
            representative_share: 0.55,
            targeted_share: 0.25,
            exploration_share: 0.10,
            transfer_share: 0.10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PersonalBaseline {
    pub uncorrected_error_rate: f64,
    pub corrected_error_rate: f64,
}

impl Default for PersonalBaseline {
    fn default() -> Self {
        Self {
            uncorrected_error_rate: 0.01,
            corrected_error_rate: 0.03,
        }
    }
}

/// Os primeiros campos preservam a leitura do estado v1. O modelo v2 nunca
/// interpreta essas contagens históricas como se fossem uma posterior precisa.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordSkill {
    pub confirmed_errors: f64,
    pub corrections: f64,
    pub fast_successes: f64,
    #[serde(default)]
    pub slowdowns: f64,
    pub observations: u32,
    #[serde(default)]
    pub model_version: u16,
    #[serde(default)]
    pub effective_exposures: f64,
    #[serde(default)]
    pub uncorrected_error_mass: f64,
    #[serde(default)]
    pub corrected_error_mass: f64,
    #[serde(default)]
    pub latency_log_residual_sum: f64,
    #[serde(default)]
    pub latency_weight: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyWordSkill {
    confirmed_errors: f64,
    corrections: f64,
    fast_successes: f64,
    #[serde(default)]
    slowdowns: f64,
    observations: u32,
}

impl WordSkill {
    pub fn decode(encoded: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(encoded).or_else(|_| {
            postcard::from_bytes::<LegacyWordSkill>(encoded).map(|legacy| Self {
                confirmed_errors: legacy.confirmed_errors,
                corrections: legacy.corrections,
                fast_successes: legacy.fast_successes,
                slowdowns: legacy.slowdowns,
                observations: legacy.observations,
                ..Self::default()
            })
        })
    }

    pub fn observe(&mut self, observation: Observation) {
        let weight = observation.evidence_weight.clamp(0.0, 1.0);
        self.model_version = 2;
        self.observations = self.observations.saturating_add(1);
        self.confirmed_errors += f64::from(observation.confirmed_error) * weight;
        self.corrections += f64::from(observation.corrected) * weight;
        self.fast_successes += f64::from(observation.fast_success) * weight;
        self.slowdowns += f64::from(observation.slow) * weight;
        self.effective_exposures += weight;
        self.uncorrected_error_mass += f64::from(observation.confirmed_error) * weight;
        self.corrected_error_mass +=
            f64::from(observation.corrected && !observation.confirmed_error) * weight;
        if let Some(ratio) = observation.latency_ratio.filter(|ratio| ratio.is_finite()) {
            self.latency_log_residual_sum += ratio.clamp(0.25, 4.0).ln() * weight;
            self.latency_weight += weight;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub confirmed_error: bool,
    pub corrected: bool,
    pub fast_success: bool,
    pub slow: bool,
    pub latency_ratio: Option<f64>,
    /// Exposições idênticas da mesma sessão dividem uma unidade de evidência.
    pub evidence_weight: f64,
}

impl Observation {
    pub fn regular(confirmed_error: bool, corrected: bool, fast_success: bool) -> Self {
        Self {
            confirmed_error,
            corrected,
            fast_success,
            slow: false,
            latency_ratio: None,
            evidence_weight: 1.0,
        }
    }
}

impl AdaptivePolicy {
    pub fn difficulty(&self, skill: &WordSkill) -> f64 {
        self.difficulty_with_baseline(skill, PersonalBaseline::default())
    }

    pub fn difficulty_with_baseline(&self, skill: &WordSkill, baseline: PersonalBaseline) -> f64 {
        if skill.model_version < 2 || skill.effective_exposures <= 0.0 {
            return 0.0;
        }
        let uncorrected = posterior_excess(
            baseline.uncorrected_error_rate,
            self.prior_strength,
            skill.uncorrected_error_mass,
            skill.effective_exposures,
            self.minimum_error_effect,
        );
        let corrected = posterior_excess(
            baseline.corrected_error_rate,
            self.prior_strength,
            skill.corrected_error_mass,
            skill.effective_exposures,
            self.minimum_correction_effect,
        ) * self.corrected_error_cost;
        let latency = if skill.latency_weight > 0.0 {
            let mean = skill.latency_log_residual_sum / skill.latency_weight;
            mean.max(0.0) * (1.0 - (-skill.latency_weight / 8.0).exp()) * self.latency_cost
        } else {
            0.0
        };
        let evidence_confidence = 1.0 - (-skill.effective_exposures / 8.0).exp();
        1.0 - (-(uncorrected + corrected + latency) * evidence_confidence * 12.0).exp()
    }

    pub fn weight_with_baseline(
        &self,
        skill: Option<&WordSkill>,
        baseline: PersonalBaseline,
    ) -> f64 {
        1.0 + skill.map_or(0.0, |skill| self.difficulty_with_baseline(skill, baseline))
            * self.maximum_boost
    }

    pub fn weight(&self, skill: Option<&WordSkill>) -> f64 {
        self.weight_with_baseline(skill, PersonalBaseline::default())
    }
}

fn posterior_excess(
    baseline: f64,
    prior_strength: f64,
    error_mass: f64,
    exposures: f64,
    minimum_effect: f64,
) -> f64 {
    let baseline = baseline.clamp(0.001, 0.999);
    let alpha = baseline * prior_strength + error_mass;
    let beta = (1.0 - baseline) * prior_strength + (exposures - error_mass).max(0.0);
    let posterior = Beta::new(alpha.max(f64::EPSILON), beta.max(f64::EPSILON))
        .expect("parâmetros da posterior beta são positivos");
    let threshold = (baseline + minimum_effect).min(0.999);
    let probability = 1.0 - posterior.cdf(threshold);
    let mean = alpha / (alpha + beta);
    probability * (mean - baseline).max(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    Representative,
    Targeted,
    Exploration,
    Transfer,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectedWord<'a> {
    pub word: &'a str,
    pub source: SelectionSource,
    /// Probabilidade marginal depois da exclusão das palavras anteriores.
    pub propensity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordSelection {
    pub word: String,
    pub source: SelectionSource,
    pub propensity: f64,
}

impl SelectedWord<'_> {
    pub fn to_owned(self) -> WordSelection {
        WordSelection {
            word: self.word.to_owned(),
            source: self.source,
            propensity: self.propensity,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AdaptiveSampler {
    policy: AdaptivePolicy,
    skills: HashMap<(String, String), WordSkill>,
    baselines: HashMap<String, PersonalBaseline>,
}

impl AdaptiveSampler {
    pub fn new(policy: AdaptivePolicy) -> Self {
        Self {
            policy,
            skills: HashMap::new(),
            baselines: HashMap::new(),
        }
    }

    pub fn from_skills(
        policy: AdaptivePolicy,
        skills: impl IntoIterator<Item = (String, String, WordSkill)>,
    ) -> Self {
        Self {
            policy,
            skills: skills
                .into_iter()
                .map(|(language, word, skill)| ((language, word), skill))
                .collect(),
            baselines: HashMap::new(),
        }
    }

    pub fn set_baseline(&mut self, language: impl Into<String>, baseline: PersonalBaseline) {
        self.baselines.insert(language.into(), baseline);
    }

    pub fn policy(&self) -> AdaptivePolicy {
        self.policy
    }

    pub fn skills_for_language(&self, language: &str) -> Vec<(String, WordSkill)> {
        self.skills
            .iter()
            .filter(|((skill_language, _), _)| skill_language == language)
            .map(|((_, word), skill)| (word.clone(), skill.clone()))
            .collect()
    }

    pub fn estimated_session_chances(
        &self,
        language: &str,
        targets: &[String],
        candidates: &[String],
        draws: usize,
    ) -> HashMap<String, f64> {
        if targets.is_empty() || candidates.is_empty() || draws == 0 {
            return HashMap::new();
        }
        const TRIALS: usize = 128;
        let target_set = targets.iter().map(String::as_str).collect::<HashSet<_>>();
        let mut counts = HashMap::<String, usize>::new();
        let mut hasher = DefaultHasher::new();
        language.hash(&mut hasher);
        targets.hash(&mut hasher);
        candidates.len().hash(&mut hasher);
        draws.hash(&mut hasher);
        let mut rng = SmallRng::seed_from_u64(hasher.finish());
        for _ in 0..TRIALS {
            let mut previous = Vec::<String>::new();
            let mut seen = HashSet::<String>::new();
            for _ in 0..draws {
                let guard = previous.iter().map(String::as_str).collect::<Vec<_>>();
                let selected = self.sample_with_provenance(language, candidates, &guard, &mut rng);
                if target_set.contains(selected.word) {
                    seen.insert(selected.word.to_owned());
                }
                previous.insert(0, selected.word.to_owned());
                previous.truncate(2);
            }
            for word in seen {
                *counts.entry(word).or_default() += 1;
            }
        }
        targets
            .iter()
            .map(|word| {
                (
                    word.clone(),
                    counts.get(word).copied().unwrap_or(0) as f64 / TRIALS as f64,
                )
            })
            .collect()
    }

    pub fn estimated_session_chance(
        &self,
        language: &str,
        word: &str,
        candidates: &[String],
        draws: usize,
    ) -> f64 {
        self.estimated_session_chances(language, &[word.to_owned()], candidates, draws)
            .get(word)
            .copied()
            .unwrap_or(0.0)
    }

    pub fn observe(&mut self, language: &str, word: &str, observation: Observation) {
        self.skills
            .entry((language.into(), word.into()))
            .or_default()
            .observe(observation);
    }

    pub fn skill(&self, language: &str, word: &str) -> Option<&WordSkill> {
        self.skills.get(&(language.into(), word.into()))
    }

    pub fn sample<'a, R: Rng>(
        &self,
        language: &str,
        candidates: &'a [String],
        previous: &[&str],
        rng: &mut R,
    ) -> &'a str {
        self.sample_with_provenance(language, candidates, previous, rng)
            .word
    }

    pub fn sample_with_provenance<'a, R: Rng>(
        &self,
        language: &str,
        candidates: &'a [String],
        previous: &[&str],
        rng: &mut R,
    ) -> SelectedWord<'a> {
        let eligible = candidates
            .iter()
            .filter(|word| !previous.contains(&word.as_str()))
            .collect::<Vec<_>>();
        let eligible = if eligible.is_empty() {
            candidates.iter().collect::<Vec<_>>()
        } else {
            eligible
        };
        let baseline = self.baseline(language);
        let uniform = vec![1.0 / eligible.len() as f64; eligible.len()];
        let targeted = normalized_or_uniform(
            eligible
                .iter()
                .map(|word| {
                    self.skill(language, word).map_or(0.0, |skill| {
                        self.policy.difficulty_with_baseline(skill, baseline)
                    })
                })
                .collect(),
            &uniform,
        );
        let exploration = normalized_or_uniform(
            eligible
                .iter()
                .map(|word| self.exploration_value(language, word))
                .collect(),
            &uniform,
        );
        let transfer_weights = self.transfer_weights(language);
        let transfer = normalized_or_uniform(
            eligible
                .iter()
                .map(|word| transfer_value(word, &transfer_weights))
                .collect(),
            &uniform,
        );
        let has_signal = targeted != uniform || exploration != uniform || transfer != uniform;
        let source = if !has_signal {
            SelectionSource::Representative
        } else {
            let roll: f64 = rng.random();
            if roll < self.policy.representative_share {
                SelectionSource::Representative
            } else if roll < self.policy.representative_share + self.policy.targeted_share {
                SelectionSource::Targeted
            } else if roll
                < self.policy.representative_share
                    + self.policy.targeted_share
                    + self.policy.exploration_share
            {
                SelectionSource::Exploration
            } else {
                SelectionSource::Transfer
            }
        };
        let selected_distribution = match source {
            SelectionSource::Representative => &uniform,
            SelectionSource::Targeted => &targeted,
            SelectionSource::Exploration => &exploration,
            SelectionSource::Transfer => &transfer,
        };
        let index = WeightedIndex::new(selected_distribution)
            .expect("distribuição adaptativa é finita e não vazia")
            .sample(rng);
        let propensity = self.policy.representative_share * uniform[index]
            + self.policy.targeted_share * targeted[index]
            + self.policy.exploration_share * exploration[index]
            + self.policy.transfer_share * transfer[index];
        SelectedWord {
            word: eligible[index],
            source,
            propensity,
        }
    }

    fn baseline(&self, language: &str) -> PersonalBaseline {
        self.baselines.get(language).copied().unwrap_or_default()
    }

    fn exploration_value(&self, language: &str, word: &str) -> f64 {
        let Some(skill) = self.skill(language, word) else {
            return 0.0;
        };
        if skill.model_version < 2 {
            return 0.0;
        }
        1.0 / (self.policy.prior_strength + skill.effective_exposures).sqrt()
    }

    fn transfer_weights(&self, language: &str) -> HashMap<String, f64> {
        let baseline = self.baseline(language);
        let mut difficult = self
            .skills_for_language(language)
            .into_iter()
            .filter_map(|(word, skill)| {
                let difficulty = self.policy.difficulty_with_baseline(&skill, baseline);
                (difficulty > 0.0).then_some((word, difficulty))
            })
            .collect::<Vec<_>>();
        difficult.sort_by(|left, right| right.1.total_cmp(&left.1));
        let mut weights = HashMap::new();
        for (word, difficulty) in difficult.into_iter().take(32) {
            for ngram in ngrams(&word) {
                *weights.entry(ngram).or_insert(0.0) += difficulty;
            }
        }
        weights
    }
}

fn normalized_or_uniform(mut values: Vec<f64>, uniform: &[f64]) -> Vec<f64> {
    let sum = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .sum::<f64>();
    if sum <= f64::EPSILON {
        return uniform.to_vec();
    }
    for value in &mut values {
        *value = if value.is_finite() { *value / sum } else { 0.0 };
    }
    values
}

fn transfer_value(word: &str, weights: &HashMap<String, f64>) -> f64 {
    ngrams(word)
        .into_iter()
        .map(|ngram| weights.get(&ngram).copied().unwrap_or(0.0))
        .sum()
}

fn ngrams(word: &str) -> Vec<String> {
    let graphemes = word.graphemes(true).collect::<Vec<_>>();
    (2..=3)
        .flat_map(|size| {
            graphemes
                .windows(size)
                .map(|window| window.concat())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::SmallRng};

    use super::*;

    #[derive(Serialize)]
    struct EncodedLegacyWordSkill {
        confirmed_errors: f64,
        corrections: f64,
        fast_successes: f64,
        slowdowns: f64,
        observations: u32,
    }

    fn observe(skill: &mut WordSkill, errors: usize, corrections: usize, successes: usize) {
        for index in 0..(errors + corrections + successes) {
            skill.observe(Observation {
                confirmed_error: index < errors,
                corrected: (errors..errors + corrections).contains(&index),
                fast_success: false,
                slow: false,
                latency_ratio: None,
                evidence_weight: 1.0,
            });
        }
    }

    #[test]
    fn inicio_frio_permanece_uniforme() {
        let policy = AdaptivePolicy::default();
        assert_eq!(policy.weight(None), 1.0);
        assert_eq!(policy.weight(Some(&WordSkill::default())), 1.0);
    }

    #[test]
    fn estado_v1_continua_legivel_sem_fingir_evidencia_v2() {
        let encoded = postcard::to_allocvec(&EncodedLegacyWordSkill {
            confirmed_errors: 2.0,
            corrections: 3.0,
            fast_successes: 4.0,
            slowdowns: 1.0,
            observations: 10,
        })
        .unwrap();
        let decoded = WordSkill::decode(&encoded).unwrap();
        assert_eq!(decoded.confirmed_errors, 2.0);
        assert_eq!(decoded.model_version, 0);
        assert_eq!(decoded.effective_exposures, 0.0);
    }

    #[test]
    fn taxa_e_denominador_importam_mais_que_contagem_absoluta() {
        let policy = AdaptivePolicy::default();
        let mut duas_em_duas = WordSkill::default();
        observe(&mut duas_em_duas, 0, 2, 0);
        let mut duas_em_cem = WordSkill::default();
        observe(&mut duas_em_cem, 0, 2, 98);
        assert!(
            policy.difficulty(&duas_em_duas) > policy.difficulty(&duas_em_cem),
            "a mesma contagem precisa produzir conclusões diferentes"
        );
    }

    #[test]
    fn uma_correcao_isolada_fica_proxima_do_prior() {
        let policy = AdaptivePolicy::default();
        let mut skill = WordSkill::default();
        observe(&mut skill, 0, 1, 0);
        assert!(policy.difficulty(&skill) < 0.05);
    }

    #[test]
    fn erro_recorrente_aumenta_prioridade_sem_explodir() {
        let policy = AdaptivePolicy::default();
        let mut skill = WordSkill::default();
        observe(&mut skill, 10, 0, 10);
        assert!(policy.weight(Some(&skill)) > 1.0);
        assert!(policy.weight(Some(&skill)) <= 1.0 + policy.maximum_boost);
    }

    #[test]
    fn sampler_respeita_as_duas_palavras_anteriores_e_registra_propensao() {
        let words = ["a", "b", "c"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let sampler = AdaptiveSampler::default();
        let mut rng = SmallRng::seed_from_u64(1);
        let selected = sampler.sample_with_provenance("english", &words, &["a", "b"], &mut rng);
        assert_eq!(selected.word, "c");
        assert_eq!(selected.source, SelectionSource::Representative);
        assert_eq!(selected.propensity, 1.0);
    }

    #[test]
    fn simulacao_da_sessao_respeita_o_sequenciador_real() {
        let words = ["a", "b", "c", "d"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let sampler = AdaptiveSampler::default();
        let chance = sampler.estimated_session_chance("english", "a", &words, 2);
        assert!((0.0..=1.0).contains(&chance));
    }
}
