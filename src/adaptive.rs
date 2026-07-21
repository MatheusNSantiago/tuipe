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
use special::Beta;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
#[path = "adaptive/simulation.rs"]
mod simulation;

pub const UNIFORM_POLICY_VERSION: u16 = 0;
pub const CURRENT_POLICY_VERSION: u16 = 2;
/// Sinal abaixo deste valor ainda é ruído e não deve ser apresentado como uma
/// dificuldade acionável para o usuário.
pub const MINIMUM_ACTIONABLE_DIFFICULTY: f64 = 0.01;

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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NgramSkill {
    pub effective_exposures: f64,
    pub uncorrected_error_mass: f64,
    pub corrected_error_mass: f64,
    /// Amostra limitada de palavras distintas que sustenta a generalização.
    pub distinct_words: Vec<String>,
}

/// Evidência de uma operação ortográfica que atravessa palavras diferentes.
/// Ela permanece separada da habilidade lexical e dos n-gramas motores.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MechanicSkill {
    pub effective_exposures: f64,
    pub uncorrected_error_mass: f64,
    pub corrected_error_mass: f64,
    pub distinct_words: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReviewState {
    pub last_seen_unix_s: i64,
    pub consecutive_clean_sessions: u16,
}

impl ReviewState {
    pub fn value_at(self, as_of_unix_s: i64) -> f64 {
        if self.consecutive_clean_sessions == 0 || as_of_unix_s <= self.last_seen_unix_s {
            return 0.0;
        }
        let interval_days = 2_u64
            .saturating_pow(u32::from(
                self.consecutive_clean_sessions.saturating_sub(1).min(5),
            ))
            .min(32) as f64;
        let age_days = (as_of_unix_s - self.last_seen_unix_s) as f64 / 86_400.0;
        if age_days <= interval_days {
            return 0.0;
        }
        1.0 - (-(age_days - interval_days) / interval_days.max(1.0)).exp()
    }
}

impl MechanicSkill {
    pub fn observe(
        &mut self,
        word: &str,
        confirmed_error: bool,
        corrected: bool,
        evidence_weight: f64,
    ) {
        let weight = evidence_weight.clamp(0.0, 1.0);
        if weight <= 0.0 {
            return;
        }
        if !self.distinct_words.iter().any(|seen| seen == word) && self.distinct_words.len() < 32 {
            self.distinct_words.push(word.to_owned());
        }
        self.effective_exposures += weight;
        self.uncorrected_error_mass += f64::from(confirmed_error) * weight;
        self.corrected_error_mass += f64::from(corrected && !confirmed_error) * weight;
    }
}

impl NgramSkill {
    pub fn observe(&mut self, word: &str, observation: Observation) {
        let weight = observation.evidence_weight.clamp(0.0, 1.0);
        if weight <= 0.0 {
            return;
        }
        if !self.distinct_words.iter().any(|seen| seen == word) && self.distinct_words.len() < 32 {
            self.distinct_words.push(word.to_owned());
        }
        self.effective_exposures += weight;
        self.uncorrected_error_mass += f64::from(observation.confirmed_error) * weight;
        self.corrected_error_mass +=
            f64::from(observation.corrected && !observation.confirmed_error) * weight;
    }
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyWordSkillV2 {
    confirmed_errors: f64,
    corrections: f64,
    fast_successes: f64,
    slowdowns: f64,
    observations: u32,
}

/// Estado gravado antes de o modelo registrar lentidão separadamente. Postcard
/// é posicional, então `serde(default)` não recupera um campo ausente no meio
/// da sequência; essa representação precisa permanecer explícita.
#[derive(Debug, Clone, Deserialize)]
struct LegacyWordSkillV1 {
    confirmed_errors: f64,
    corrections: f64,
    fast_successes: f64,
    observations: u32,
}

impl WordSkill {
    pub fn decode(encoded: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(encoded).or_else(|_| {
            postcard::from_bytes::<LegacyWordSkillV2>(encoded)
                .map(|legacy| Self {
                    confirmed_errors: legacy.confirmed_errors,
                    corrections: legacy.corrections,
                    fast_successes: legacy.fast_successes,
                    slowdowns: legacy.slowdowns,
                    observations: legacy.observations,
                    ..Self::default()
                })
                .or_else(|_| {
                    postcard::from_bytes::<LegacyWordSkillV1>(encoded).map(|legacy| Self {
                        confirmed_errors: legacy.confirmed_errors,
                        corrections: legacy.corrections,
                        fast_successes: legacy.fast_successes,
                        observations: legacy.observations,
                        ..Self::default()
                    })
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
        // Uma observação isolada é muito fácil de explicar por distração,
        // correção preventiva ou variação momentânea de ritmo. A confiança
        // cresce de forma quadrática para exigir recorrência antes de alterar
        // perceptivelmente o currículo.
        let evidence_confidence = (1.0 - (-skill.effective_exposures / 8.0).exp()).powi(2);
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

    pub fn ngram_difficulty(&self, skill: &NgramSkill, baseline: PersonalBaseline) -> f64 {
        if skill.distinct_words.len() < 3 || skill.effective_exposures <= 0.0 {
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
        let confidence = 1.0 - (-skill.effective_exposures / 12.0).exp();
        1.0 - (-(uncorrected + corrected) * confidence * 10.0).exp()
    }

    pub fn mechanic_difficulty(&self, skill: &MechanicSkill, baseline: PersonalBaseline) -> f64 {
        if skill.distinct_words.len() < 3 || skill.effective_exposures <= 0.0 {
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
        let confidence = 1.0 - (-skill.effective_exposures / 12.0).exp();
        1.0 - (-(uncorrected + corrected) * confidence * 8.0).exp()
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
    let threshold = (baseline + minimum_effect).min(0.999);
    let probability = 1.0 - threshold.inc_beta(alpha, beta, alpha.ln_beta(beta));
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
    ngram_skills: HashMap<(String, String), NgramSkill>,
    mechanic_skills: HashMap<(String, String), MechanicSkill>,
    review_states: HashMap<(String, String), ReviewState>,
    as_of_unix_s: i64,
    word_difficulties: HashMap<(String, String), f64>,
    ngram_difficulties: HashMap<(String, String), f64>,
    mechanic_difficulties: HashMap<(String, String), f64>,
}

impl AdaptiveSampler {
    pub fn new(policy: AdaptivePolicy) -> Self {
        Self {
            policy,
            skills: HashMap::new(),
            baselines: HashMap::new(),
            ngram_skills: HashMap::new(),
            mechanic_skills: HashMap::new(),
            review_states: HashMap::new(),
            as_of_unix_s: 0,
            word_difficulties: HashMap::new(),
            ngram_difficulties: HashMap::new(),
            mechanic_difficulties: HashMap::new(),
        }
    }

    pub fn from_skills(
        policy: AdaptivePolicy,
        skills: impl IntoIterator<Item = (String, String, WordSkill)>,
    ) -> Self {
        let mut sampler = Self {
            policy,
            skills: skills
                .into_iter()
                .map(|(language, word, skill)| ((language, word), skill))
                .collect(),
            baselines: HashMap::new(),
            ngram_skills: HashMap::new(),
            mechanic_skills: HashMap::new(),
            review_states: HashMap::new(),
            as_of_unix_s: 0,
            word_difficulties: HashMap::new(),
            ngram_difficulties: HashMap::new(),
            mechanic_difficulties: HashMap::new(),
        };
        sampler.rebuild_difficulty_cache();
        sampler
    }

    pub fn set_ngram_skills(
        &mut self,
        skills: impl IntoIterator<Item = (String, String, NgramSkill)>,
    ) {
        self.ngram_skills = skills
            .into_iter()
            .map(|(language, ngram, skill)| ((language, ngram), skill))
            .collect();
        self.rebuild_difficulty_cache();
    }

    pub fn set_mechanic_skills(
        &mut self,
        skills: impl IntoIterator<Item = (String, String, MechanicSkill)>,
    ) {
        self.mechanic_skills = skills
            .into_iter()
            .map(|(language, mechanic, skill)| ((language, mechanic), skill))
            .collect();
        self.rebuild_difficulty_cache();
    }

    pub fn set_review_states(
        &mut self,
        states: impl IntoIterator<Item = (String, String, ReviewState)>,
        as_of_unix_s: i64,
    ) {
        self.review_states = states
            .into_iter()
            .map(|(language, word, state)| ((language, word), state))
            .collect();
        self.as_of_unix_s = as_of_unix_s;
    }

    pub fn record_review(
        &mut self,
        language: &str,
        word: &str,
        clean: bool,
        observed_at_unix_s: i64,
    ) {
        let state = self
            .review_states
            .entry((language.into(), word.into()))
            .or_default();
        state.last_seen_unix_s = observed_at_unix_s;
        state.consecutive_clean_sessions = if clean {
            state.consecutive_clean_sessions.saturating_add(1)
        } else {
            0
        };
        self.as_of_unix_s = self.as_of_unix_s.max(observed_at_unix_s);
    }

    pub fn retention_candidates(&self, language: &str, candidates: &[String]) -> Vec<String> {
        let mut due = candidates
            .iter()
            .filter_map(|word| {
                let value = self.review_value(language, word);
                (value > 0.0).then_some((word.clone(), value))
            })
            .collect::<Vec<_>>();
        due.sort_by(|left, right| right.1.total_cmp(&left.1));
        due.into_iter().map(|(word, _)| word).collect()
    }

    pub fn set_baseline(&mut self, language: impl Into<String>, baseline: PersonalBaseline) {
        self.baselines.insert(language.into(), baseline);
        self.rebuild_difficulty_cache();
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
        self.estimated_session_chances_with_number_probability(
            language, targets, candidates, draws, 0.0,
        )
    }

    pub fn estimated_session_chances_with_number_probability(
        &self,
        language: &str,
        targets: &[String],
        candidates: &[String],
        draws: usize,
        number_probability: f64,
    ) -> HashMap<String, f64> {
        let groups = targets
            .iter()
            .map(|target| vec![target.clone()])
            .collect::<Vec<_>>();
        let chances = self.estimated_session_group_chances_with_number_probability(
            language,
            &groups,
            candidates,
            draws,
            number_probability,
        );
        targets.iter().cloned().zip(chances).collect()
    }

    fn estimated_session_group_chances_with_number_probability(
        &self,
        language: &str,
        target_groups: &[Vec<String>],
        candidates: &[String],
        draws: usize,
        number_probability: f64,
    ) -> Vec<f64> {
        if target_groups.is_empty() || candidates.is_empty() || draws == 0 {
            return vec![0.0; target_groups.len()];
        }
        const TRIALS: usize = 128;
        let number_probability = number_probability.clamp(0.0, 1.0);
        let target_sets = target_groups
            .iter()
            .map(|group| group.iter().map(String::as_str).collect::<HashSet<_>>())
            .collect::<Vec<_>>();
        let targeted = candidates
            .iter()
            .map(|word| 1.0 + self.policy.maximum_boost * self.candidate_priority(language, word))
            .collect::<Vec<_>>();
        let exploration = candidates
            .iter()
            .map(|word| self.exploration_value(language, word))
            .collect::<Vec<_>>();
        let transfer_weights = self.transfer_weights(language);
        let transfer = candidates
            .iter()
            .map(|word| {
                1.0 + self.policy.maximum_boost * transfer_value(word, &transfer_weights).min(1.0)
            })
            .collect::<Vec<_>>();
        let representative_distribution = WeightedIndex::new(vec![1.0; candidates.len()])
            .expect("o corpus não vazio forma uma distribuição uniforme");
        let targeted_distribution = WeightedIndex::new(&targeted)
            .expect("as prioridades direcionadas são positivas e finitas");
        let exploration_distribution = WeightedIndex::new(&exploration)
            .expect("as prioridades de exploração são positivas e finitas");
        let transfer_distribution = WeightedIndex::new(&transfer)
            .expect("as prioridades de transferência são positivas e finitas");
        let has_signal =
            weights_vary(&targeted) || weights_vary(&exploration) || weights_vary(&transfer);
        let mut counts = vec![0_usize; target_groups.len()];
        let mut hasher = DefaultHasher::new();
        language.hash(&mut hasher);
        target_groups.hash(&mut hasher);
        candidates.len().hash(&mut hasher);
        draws.hash(&mut hasher);
        number_probability.to_bits().hash(&mut hasher);
        let mut rng = SmallRng::seed_from_u64(hasher.finish());
        for _ in 0..TRIALS {
            let mut previous = Vec::<usize>::new();
            let mut seen = vec![false; target_groups.len()];
            for _ in 0..draws {
                if number_probability > 0.0 && rng.random_bool(number_probability) {
                    continue;
                }
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
                let distribution = match source {
                    SelectionSource::Representative => &representative_distribution,
                    SelectionSource::Targeted => &targeted_distribution,
                    SelectionSource::Exploration => &exploration_distribution,
                    SelectionSource::Transfer => &transfer_distribution,
                };
                let exclude_previous = previous.len() < candidates.len();
                let index = loop {
                    let index = distribution.sample(&mut rng);
                    if !exclude_previous || !previous.contains(&index) {
                        break index;
                    }
                };
                let selected = &candidates[index];
                for (group_index, target_set) in target_sets.iter().enumerate() {
                    seen[group_index] |= target_set.contains(selected.as_str());
                }
                previous.insert(0, index);
                previous.truncate(2);
            }
            for (count, seen) in counts.iter_mut().zip(seen) {
                *count += usize::from(seen);
            }
        }
        counts
            .into_iter()
            .map(|count| count as f64 / TRIALS as f64)
            .collect()
    }

    /// Estima somente o aumento causado pelo modelo adaptativo, descontando a
    /// chance que qualquer palavra já teria no sorteio representativo.
    pub fn estimated_session_uplifts_with_number_probability(
        &self,
        language: &str,
        targets: &[String],
        candidates: &[String],
        draws: usize,
        number_probability: f64,
    ) -> HashMap<String, f64> {
        let adaptive = self.estimated_session_chances_with_number_probability(
            language,
            targets,
            candidates,
            draws,
            number_probability,
        );
        let representative = Self::new(self.policy)
            .estimated_session_chances_with_number_probability(
                language,
                targets,
                candidates,
                draws,
                number_probability,
            );

        targets
            .iter()
            .map(|word| {
                let adaptive = adaptive.get(word).copied().unwrap_or(0.0);
                let representative = representative.get(word).copied().unwrap_or(0.0);
                (word.clone(), (adaptive - representative).max(0.0))
            })
            .collect()
    }

    /// Estima o aumento da chance de ao menos um item de cada grupo aparecer
    /// na sessão, sempre contra o mesmo sorteio representativo usado na tela.
    pub fn estimated_session_group_uplifts_with_number_probability(
        &self,
        language: &str,
        target_groups: &[Vec<String>],
        candidates: &[String],
        draws: usize,
        number_probability: f64,
    ) -> Vec<f64> {
        let adaptive = self.estimated_session_group_chances_with_number_probability(
            language,
            target_groups,
            candidates,
            draws,
            number_probability,
        );
        let representative = Self::new(self.policy)
            .estimated_session_group_chances_with_number_probability(
                language,
                target_groups,
                candidates,
                draws,
                number_probability,
            );
        adaptive
            .into_iter()
            .zip(representative)
            .map(|(adaptive, representative)| (adaptive - representative).max(0.0))
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
        for ngram in lexical_ngrams(word) {
            self.ngram_skills
                .entry((language.into(), ngram))
                .or_default()
                .observe(word, observation);
        }
        self.refresh_word_difficulty(language, word);
        for ngram in lexical_ngrams(word) {
            self.refresh_ngram_difficulty(language, &ngram);
        }
    }

    pub fn observe_mechanic(
        &mut self,
        language: &str,
        word: &str,
        mechanic: &str,
        confirmed_error: bool,
        corrected: bool,
        evidence_weight: f64,
    ) {
        self.mechanic_skills
            .entry((language.into(), mechanic.into()))
            .or_default()
            .observe(word, confirmed_error, corrected, evidence_weight);
        self.refresh_mechanic_difficulty(language, mechanic);
    }

    /// Multiplicador deliberadamente pequeno para formatadores opcionais. O
    /// currículo continua majoritariamente representativo.
    pub fn mechanic_boost(&self, language: &str, mechanic: &str) -> f64 {
        let difficulty = self
            .mechanic_difficulties
            .get(&(language.into(), mechanic.into()))
            .copied()
            .unwrap_or(0.0);
        1.0 + 0.5 * difficulty
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
        let uniform = vec![1.0 / eligible.len() as f64; eligible.len()];
        let targeted = normalized_or_uniform(
            eligible
                .iter()
                .map(|word| {
                    1.0 + self.policy.maximum_boost * self.candidate_priority(language, word)
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
                .map(|word| {
                    1.0 + self.policy.maximum_boost
                        * transfer_value(word, &transfer_weights).min(1.0)
                })
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
        let exposures = self
            .skill(language, word)
            .filter(|skill| skill.model_version >= 2)
            .map_or(0.0, |skill| skill.effective_exposures);
        1.0 / (self.policy.prior_strength + exposures).sqrt()
    }

    fn candidate_priority(&self, language: &str, word: &str) -> f64 {
        let lexical = self
            .word_difficulties
            .get(&(language.into(), word.into()))
            .copied()
            .unwrap_or(0.0);
        let motor = lexical_ngrams(word)
            .into_iter()
            .filter_map(|ngram| {
                self.ngram_difficulties
                    .get(&(language.into(), ngram))
                    .copied()
            })
            .fold(0.0, f64::max);
        let mechanics = mechanics_for_token(word)
            .into_iter()
            .filter_map(|mechanic| {
                self.mechanic_difficulties
                    .get(&(language.into(), mechanic))
                    .copied()
            })
            .fold(0.0, f64::max);
        let review = self.review_value(language, word);
        (lexical + motor * 0.45 + mechanics * 0.25 + review * 0.20).min(1.0)
    }

    fn review_value(&self, language: &str, word: &str) -> f64 {
        let Some(state) = self.review_states.get(&(language.into(), word.into())) else {
            return 0.0;
        };
        state.value_at(self.as_of_unix_s)
    }

    fn transfer_weights(&self, language: &str) -> HashMap<String, f64> {
        let mut weights = HashMap::new();
        for ((skill_language, ngram), difficulty) in &self.ngram_difficulties {
            if skill_language == language && *difficulty > 0.0 {
                weights.insert(ngram.clone(), *difficulty);
            }
        }
        weights
    }

    fn rebuild_difficulty_cache(&mut self) {
        self.word_difficulties.clear();
        self.ngram_difficulties.clear();
        self.mechanic_difficulties.clear();
        let words = self.skills.keys().cloned().collect::<Vec<_>>();
        let ngrams = self.ngram_skills.keys().cloned().collect::<Vec<_>>();
        let mechanics = self.mechanic_skills.keys().cloned().collect::<Vec<_>>();
        for (language, word) in words {
            self.refresh_word_difficulty(&language, &word);
        }
        for (language, ngram) in ngrams {
            self.refresh_ngram_difficulty(&language, &ngram);
        }
        for (language, mechanic) in mechanics {
            self.refresh_mechanic_difficulty(&language, &mechanic);
        }
    }

    fn refresh_word_difficulty(&mut self, language: &str, word: &str) {
        let difficulty = self
            .skills
            .get(&(language.into(), word.into()))
            .map_or(0.0, |skill| {
                self.policy
                    .difficulty_with_baseline(skill, self.baseline(language))
            });
        self.word_difficulties
            .insert((language.into(), word.into()), difficulty);
    }

    fn refresh_ngram_difficulty(&mut self, language: &str, ngram: &str) {
        let difficulty = self
            .ngram_skills
            .get(&(language.into(), ngram.into()))
            .map_or(0.0, |skill| {
                self.policy.ngram_difficulty(skill, self.baseline(language))
            });
        self.ngram_difficulties
            .insert((language.into(), ngram.into()), difficulty);
    }

    fn refresh_mechanic_difficulty(&mut self, language: &str, mechanic: &str) {
        let difficulty = self
            .mechanic_skills
            .get(&(language.into(), mechanic.into()))
            .map_or(0.0, |skill| {
                self.policy
                    .mechanic_difficulty(skill, self.baseline(language))
            });
        self.mechanic_difficulties
            .insert((language.into(), mechanic.into()), difficulty);
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

fn weights_vary(values: &[f64]) -> bool {
    values.first().is_some_and(|first| {
        values
            .iter()
            .any(|value| (value - first).abs() > f64::EPSILON)
    })
}

fn transfer_value(word: &str, weights: &HashMap<String, f64>) -> f64 {
    lexical_ngrams(word)
        .into_iter()
        .map(|ngram| weights.get(&ngram).copied().unwrap_or(0.0))
        .sum()
}

pub fn lexical_ngrams(word: &str) -> Vec<String> {
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

pub const MECHANIC_CAPITALIZATION: &str = "capitalizacao";
pub const MECHANIC_FINAL_PUNCTUATION: &str = "pontuacao_final";
pub const MECHANIC_COMMA: &str = "virgula";

/// Extrai somente operações que podem generalizar entre palavras. A palavra
/// acentuada continua sendo uma identidade lexical própria.
pub fn mechanics_for_token(token: &str) -> Vec<String> {
    let mut mechanics = Vec::<String>::new();
    if token.chars().any(char::is_uppercase) {
        mechanics.push(MECHANIC_CAPITALIZATION.into());
    }
    if token
        .chars()
        .any(|character| matches!(character, '.' | '?' | '!'))
    {
        mechanics.push(MECHANIC_FINAL_PUNCTUATION.into());
    }
    if token.contains(',') {
        mechanics.push(MECHANIC_COMMA.into());
    }
    for character in token.nfd() {
        let mechanic = match character {
            '\u{0301}' => Some("acento_agudo"),
            '\u{0302}' => Some("acento_circunflexo"),
            '\u{0303}' => Some("til"),
            '\u{0300}' => Some("acento_grave"),
            '\u{0327}' => Some("cedilha"),
            '\u{0308}' => Some("trema"),
            _ => None,
        };
        if let Some(mechanic) = mechanic
            && !mechanics.iter().any(|existing| existing == mechanic)
        {
            mechanics.push(mechanic.into());
        }
    }
    mechanics
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
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

    #[derive(Serialize)]
    struct EncodedLegacyWordSkillV1 {
        confirmed_errors: f64,
        corrections: f64,
        fast_successes: f64,
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
    fn estado_anterior_a_lentidao_separada_continua_legivel() {
        let encoded = postcard::to_allocvec(&EncodedLegacyWordSkillV1 {
            confirmed_errors: 0.0,
            corrections: 0.0,
            fast_successes: 1.0,
            observations: 1,
        })
        .unwrap();
        assert_eq!(encoded.len(), 25, "reproduz o blob encontrado em produção");

        let decoded = WordSkill::decode(&encoded).unwrap();
        assert_eq!(decoded.fast_successes, 1.0);
        assert_eq!(decoded.observations, 1);
        assert_eq!(decoded.slowdowns, 0.0);
        assert_eq!(decoded.model_version, 0);
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
        assert!(policy.difficulty(&skill) < MINIMUM_ACTIONABLE_DIFFICULTY);
    }

    #[test]
    fn uma_correcao_lenta_isolada_ainda_e_ruido() {
        let policy = AdaptivePolicy::default();
        let mut skill = WordSkill::default();
        skill.observe(Observation {
            corrected: true,
            slow: true,
            latency_ratio: Some(1.91),
            ..Observation::regular(false, false, false)
        });

        assert!(policy.difficulty(&skill) < MINIMUM_ACTIONABLE_DIFFICULTY);
    }

    #[test]
    fn uma_correcao_isolada_nao_cria_aumento_relevante_na_sessao() {
        let words = (0..200)
            .map(|index| format!("palavra{index}"))
            .collect::<Vec<_>>();
        let mut sampler = AdaptiveSampler::default();
        sampler.observe(
            "portuguese",
            &words[0],
            Observation::regular(false, true, false),
        );

        let uplifts = sampler.estimated_session_uplifts_with_number_probability(
            "portuguese",
            &[words[0].clone()],
            &words,
            24,
            0.0,
        );

        assert!(uplifts[&words[0]] <= 0.01);
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
    fn bons_resultados_posteriores_reduzem_a_dificuldade_gradualmente() {
        let policy = AdaptivePolicy::default();
        let mut skill = WordSkill::default();
        observe(&mut skill, 8, 0, 8);
        let before = policy.difficulty(&skill);
        observe(&mut skill, 0, 0, 32);
        let after = policy.difficulty(&skill);

        assert!(before > 0.0);
        assert!(after < before);
        assert!(
            after > 0.0,
            "a recuperação não deve apagar o histórico de uma vez"
        );
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

    #[test]
    fn aumento_do_padrao_mede_a_chance_do_grupo_na_sessao() {
        let observation = Observation::regular(true, false, false);
        let mut skill = NgramSkill::default();
        for word in ["primeiro", "principal", "privado"] {
            for _ in 0..8 {
                skill.observe(word, observation);
            }
        }
        let mut sampler = AdaptiveSampler::default();
        sampler.set_ngram_skills(vec![("portuguese".into(), "ri".into(), skill)]);
        let candidates = [
            "primeiro",
            "principal",
            "privado",
            "casa",
            "tempo",
            "mundo",
            "fazer",
            "estar",
            "coisa",
            "parte",
            "agora",
            "outro",
            "mesmo",
            "depois",
            "grande",
            "poder",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let groups = vec![vec![
            "primeiro".into(),
            "principal".into(),
            "privado".into(),
        ]];

        let uplift = sampler.estimated_session_group_uplifts_with_number_probability(
            "portuguese",
            &groups,
            &candidates,
            4,
            0.0,
        );

        assert!(uplift[0] > 0.0);
    }

    #[test]
    fn ngrama_so_generaliza_depois_de_palavras_distintas() {
        let policy = AdaptivePolicy::default();
        let observation = Observation {
            confirmed_error: true,
            corrected: false,
            fast_success: false,
            slow: false,
            latency_ratio: None,
            evidence_weight: 1.0,
        };
        let mut skill = NgramSkill::default();
        for _ in 0..10 {
            skill.observe("primeiro", observation);
        }
        assert_eq!(
            policy.ngram_difficulty(&skill, PersonalBaseline::default()),
            0.0
        );
        skill.observe("principal", observation);
        skill.observe("privado", observation);
        assert!(policy.ngram_difficulty(&skill, PersonalBaseline::default()) > 0.0);
    }

    #[test]
    fn evidencia_descartada_nao_ativa_generalizacao_compartilhada() {
        let discarded = Observation {
            confirmed_error: true,
            corrected: false,
            fast_success: false,
            slow: true,
            latency_ratio: Some(4.0),
            evidence_weight: 0.0,
        };
        let mut ngram = NgramSkill::default();
        ngram.observe("primeiro", discarded);
        assert!(ngram.distinct_words.is_empty());

        let mut mechanic = MechanicSkill::default();
        mechanic.observe("primeiro", true, false, 0.0);
        assert!(mechanic.distinct_words.is_empty());
    }

    #[test]
    fn mecanicas_sao_extraidas_sem_apagar_a_identidade_lexical() {
        assert_eq!(
            mechanics_for_token("Árvore,"),
            vec![
                MECHANIC_CAPITALIZATION.to_owned(),
                MECHANIC_COMMA.to_owned(),
                "acento_agudo".to_owned(),
            ]
        );
        assert!(mechanics_for_token("arvore").is_empty());
    }

    #[test]
    fn mecanica_so_inclina_o_curriculo_apos_contextos_distintos() {
        let mut sampler = AdaptiveSampler::default();
        for word in ["ação", "coração", "atenção"] {
            sampler.observe_mechanic("portuguese", word, "til", true, false, 1.0);
        }
        let boost = sampler.mechanic_boost("portuguese", "til");
        assert!(boost > 1.0);
        assert!(boost <= 1.5);
    }

    #[test]
    fn revisao_so_fica_elegivel_depois_do_intervalo() {
        let mut sampler = AdaptiveSampler::default();
        sampler.set_review_states(
            [(
                "portuguese".into(),
                "casa".into(),
                ReviewState {
                    last_seen_unix_s: 1_000,
                    consecutive_clean_sessions: 1,
                },
            )],
            1_000 + 12 * 60 * 60,
        );
        assert_eq!(sampler.review_value("portuguese", "casa"), 0.0);
        sampler.as_of_unix_s = 1_000 + 3 * 86_400;
        assert!(sampler.review_value("portuguese", "casa") > 0.0);
        assert_eq!(
            sampler.retention_candidates(
                "portuguese",
                &["casa".into(), "tempo".into(), "mundo".into()]
            ),
            vec!["casa"]
        );
    }

    #[test]
    fn passagem_do_tempo_nao_recria_dificuldade() {
        let mut sampler = AdaptiveSampler::default();
        sampler.set_review_states(
            [(
                "portuguese".into(),
                "casa".into(),
                ReviewState {
                    last_seen_unix_s: 1,
                    consecutive_clean_sessions: 5,
                },
            )],
            90 * 86_400,
        );
        assert_eq!(
            sampler.policy().difficulty(&WordSkill::default()),
            0.0,
            "recência controla revisão, não altera a posterior de erro"
        );
        assert!(sampler.review_value("portuguese", "casa") > 0.0);
    }

    #[test]
    fn posterior_beta_preserva_o_resultado_de_referencia() {
        let actual = posterior_excess(0.05, 8.0, 3.0, 12.0, 0.02);
        assert!((actual - 0.109_320_281_206_737_73).abs() < 1e-12);
    }

    proptest! {
        #[test]
        fn propensao_e_sempre_finita_e_valida(seed in any::<u64>(), size in 3_usize..80) {
            let words = (0..size).map(|index| format!("palavra{index}")).collect::<Vec<_>>();
            let sampler = AdaptiveSampler::default();
            let mut rng = SmallRng::seed_from_u64(seed);
            let selected = sampler.sample_with_provenance(
                "portuguese",
                &words,
                &[words[0].as_str(), words[1].as_str()],
                &mut rng,
            );
            prop_assert!(selected.propensity.is_finite());
            prop_assert!(selected.propensity > 0.0 && selected.propensity <= 1.0);
            prop_assert_ne!(selected.word, words[0].as_str());
            prop_assert_ne!(selected.word, words[1].as_str());
        }
    }
}
