//! Inferência de dificuldade e seleção automática orientadas por exposições.
//!
//! O modelo separa taxa, incerteza e utilidade de treino. SQLite materializa
//! [`WordSkill`]; o sampler só recebe estado restaurado e um RNG injetável.

use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

use rand::{Rng, SeedableRng, rngs::SmallRng};
use serde::{Deserialize, Serialize};
use special::Beta;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
#[path = "adaptive/simulation.rs"]
mod simulation;

pub const UNIFORM_POLICY_VERSION: u16 = 0;
pub const CURRENT_POLICY_VERSION: u16 = 7;
/// Sinal abaixo deste valor ainda é ruído e não deve ser apresentado como uma
/// dificuldade acionável para o usuário.
pub const MINIMUM_ACTIONABLE_DIFFICULTY: f64 = 0.01;

/// Probabilidade de uma pessoa alcançar cada posição do teste na próxima
/// sessão. A curva é monotônica por construção: alcançar a posição `i + 1`
/// implica ter alcançado `i`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReachProfile {
    survival: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReachObservation {
    pub reached: usize,
    /// `true` quando a sessão realmente terminou nessa posição; `false`
    /// representa apenas censura porque o teste observado era mais curto.
    pub terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WordPerformance {
    pub terminal_error_probability: f64,
    pub expected_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReachForecast {
    pub positions: usize,
    pub duration_ms: Option<u64>,
    pub stops_on_error: bool,
    pub trials: usize,
}

impl ReachProfile {
    /// Constrói a curva empírica a partir da quantidade de posições realmente
    /// iniciadas em sessões comparáveis.
    pub fn from_reached_counts(
        reached_counts: impl IntoIterator<Item = usize>,
        positions: usize,
    ) -> Self {
        Self::from_observations(
            reached_counts.into_iter().map(|reached| ReachObservation {
                reached,
                terminal: true,
            }),
            positions,
        )
    }

    /// Estima a sobrevivência sem interpretar sessões mais curtas como falha
    /// em posições que elas nunca tiveram tempo de alcançar.
    pub fn from_observations(
        observations: impl IntoIterator<Item = ReachObservation>,
        positions: usize,
    ) -> Self {
        let observations = observations.into_iter().collect::<Vec<_>>();
        if observations.is_empty() || positions == 0 {
            return Self::default();
        }
        let mut probability = 1.0;
        let mut survival = Vec::with_capacity(positions);
        for position in 0..positions {
            let at_risk = observations
                .iter()
                .filter(|observation| {
                    observation.reached > position
                        || observation.terminal && observation.reached == position
                })
                .count();
            if at_risk == 0 {
                probability = 0.0;
            } else {
                let endings = observations
                    .iter()
                    .filter(|observation| observation.terminal && observation.reached == position)
                    .count();
                probability *= 1.0 - endings as f64 / at_risk as f64;
            }
            survival.push(probability);
        }
        Self { survival }
    }

    /// Perfil usado quando todas as posições informadas são necessariamente
    /// alcançadas, principalmente em testes unitários do sequenciador.
    pub fn certain(positions: usize) -> Self {
        Self {
            survival: vec![1.0; positions],
        }
    }

    pub fn probability(&self, position: usize) -> f64 {
        self.survival.get(position).copied().unwrap_or(0.0)
    }

    pub fn positions(&self) -> usize {
        self.survival.len()
    }

    fn sample_reached<R: Rng>(&self, rng: &mut R) -> usize {
        let threshold: f64 = rng.random();
        self.survival
            .iter()
            .take_while(|probability| threshold < **probability)
            .count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptivePolicy {
    /// Força do prior pessoal em exposições equivalentes.
    pub prior_strength: f64,
    /// Excesso mínimo de taxa que precisa ser educacionalmente relevante.
    pub minimum_error_effect: f64,
    pub minimum_correction_effect: f64,
    pub correction_cost: f64,
    pub latency_cost: f64,
    /// Maior chance desejada para uma palavra de dificuldade máxima aparecer
    /// em uma sessão. A distribuição continua probabilística e preserva chance
    /// positiva para todas as candidatas.
    pub maximum_session_exposure: f64,
}

impl Default for AdaptivePolicy {
    fn default() -> Self {
        Self {
            prior_strength: 16.0,
            minimum_error_effect: 0.0,
            minimum_correction_effect: 0.0,
            correction_cost: 0.9,
            latency_cost: 0.22,
            maximum_session_exposure: 0.40,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordSkill {
    pub confirmed_errors: f64,
    pub corrections: f64,
    pub fast_successes: f64,
    pub slowdowns: f64,
    pub observations: u32,
    pub model_version: u16,
    pub effective_exposures: f64,
    pub uncorrected_error_mass: f64,
    pub corrected_error_mass: f64,
    pub correction_burden_mass: f64,
    pub corrected_graphemes: f64,
    pub corrective_events: f64,
    pub correction_ms: f64,
    pub latency_log_residual_sum: f64,
    pub latency_weight: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NgramSkill {
    pub effective_exposures: f64,
    pub uncorrected_error_mass: f64,
    pub corrected_error_mass: f64,
    pub correction_burden_mass: f64,
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
    pub correction_burden_mass: f64,
    pub distinct_words: Vec<String>,
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
        self.corrected_error_mass += f64::from(corrected) * weight;
        self.correction_burden_mass += f64::from(corrected) * weight;
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
        self.corrected_error_mass += f64::from(observation.corrected) * weight;
        self.correction_burden_mass += observation.correction_burden * weight;
    }
}

impl WordSkill {
    pub fn decode(encoded: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(encoded)
    }

    pub fn observe(&mut self, observation: Observation) {
        let weight = observation.evidence_weight.clamp(0.0, 1.0);
        self.model_version = 3;
        self.observations = self.observations.saturating_add(1);
        self.confirmed_errors += f64::from(observation.confirmed_error) * weight;
        self.corrections += f64::from(observation.corrected) * weight;
        self.fast_successes += f64::from(observation.fast_success) * weight;
        self.slowdowns += f64::from(observation.slow) * weight;
        self.effective_exposures += weight;
        self.uncorrected_error_mass += f64::from(observation.confirmed_error) * weight;
        self.corrected_error_mass += f64::from(observation.corrected) * weight;
        self.correction_burden_mass += observation.correction_burden * weight;
        self.corrected_graphemes += f64::from(observation.corrections) * weight;
        self.corrective_events += f64::from(observation.corrective_events) * weight;
        self.correction_ms += observation.correction_ms as f64 * weight;
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
    pub corrections: u32,
    pub corrective_events: u16,
    pub correction_ms: u64,
    pub correction_burden: f64,
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
            corrections: u32::from(corrected),
            corrective_events: u16::from(corrected),
            correction_ms: 0,
            correction_burden: f64::from(corrected),
            fast_success,
            slow: false,
            latency_ratio: None,
            evidence_weight: 1.0,
        }
    }
}

/// Resume o trabalho gasto para recuperar uma palavra sem tratar nove
/// backspaces como se fossem uma correção trivial. O resultado satura em um
/// para que uma única tentativa difícil não finja ser várias exposições.
pub fn correction_burden(
    corrected_graphemes: u32,
    corrective_events: u16,
    correction_ms: u64,
    fluent_ms: u64,
    grapheme_count: u16,
) -> f64 {
    if corrected_graphemes == 0 && corrective_events == 0 {
        return 0.0;
    }
    let length = f64::from(grapheme_count.max(1));
    let grapheme_ratio = f64::from(corrected_graphemes) / length;
    let event_ratio = f64::from(corrective_events) / length;
    let execution_ms = correction_ms.saturating_add(fluent_ms).max(1);
    let correction_share = correction_ms as f64 / execution_ms as f64;
    (1.0 - (-(0.85 * grapheme_ratio + 0.35 * event_ratio + 0.75 * correction_share)).exp())
        .clamp(0.0, 1.0)
}

impl AdaptivePolicy {
    pub fn difficulty(&self, skill: &WordSkill) -> f64 {
        self.difficulty_with_baseline(skill, PersonalBaseline::default())
    }

    pub fn difficulty_with_baseline(&self, skill: &WordSkill, baseline: PersonalBaseline) -> f64 {
        if skill.model_version < 3 || skill.effective_exposures <= 0.0 {
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
        );
        let mean_burden = if skill.corrected_error_mass > 0.0 {
            skill.correction_burden_mass / skill.corrected_error_mass
        } else {
            0.0
        };
        let corrected = corrected * self.correction_cost * (0.5 + 1.5 * mean_burden);
        let latency = if skill.latency_weight > 0.0 {
            let mean = skill.latency_log_residual_sum / skill.latency_weight;
            mean.max(0.0) * (1.0 - (-skill.latency_weight / 8.0).exp()) * self.latency_cost
        } else {
            0.0
        };
        // Confiança exige repetição entre exposições, mas a severidade dentro
        // de uma tentativa também conta. Muitas correções na mesma palavra
        // deixam de ser reduzidas ao mesmo sinal de um único backspace.
        // Uma ocorrência isolada é ruído, mesmo quando a recuperação foi
        // trabalhosa. A recorrência transforma essa severidade em evidência.
        let recurrence_confidence = 1.0 - (-(skill.effective_exposures - 1.0).max(0.0) / 2.5).exp();
        let signal_mass = skill.uncorrected_error_mass + skill.correction_burden_mass;
        let signal_confidence = 1.0 - (-signal_mass / 1.5).exp();
        1.0 - (-(uncorrected + corrected + latency)
            * recurrence_confidence
            * signal_confidence
            * 6.0)
            .exp()
    }

    pub fn ngram_difficulty(&self, skill: &NgramSkill, baseline: PersonalBaseline) -> f64 {
        if skill.effective_exposures <= 0.0 {
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
        ) * self.correction_cost
            * correction_severity(skill.corrected_error_mass, skill.correction_burden_mass);
        let confidence = 1.0 - (-skill.effective_exposures / 12.0).exp();
        1.0 - (-(uncorrected + corrected) * confidence * 6.0).exp()
    }

    pub fn mechanic_difficulty(&self, skill: &MechanicSkill, baseline: PersonalBaseline) -> f64 {
        if skill.effective_exposures <= 0.0 {
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
        ) * self.correction_cost
            * correction_severity(skill.corrected_error_mass, skill.correction_burden_mass);
        let confidence = 1.0 - (-skill.effective_exposures / 12.0).exp();
        1.0 - (-(uncorrected + corrected) * confidence * 5.0).exp()
    }
}

fn correction_severity(corrected_exposures: f64, burden: f64) -> f64 {
    if corrected_exposures > 0.0 {
        0.5 + 1.5 * (burden / corrected_exposures).clamp(0.0, 1.0)
    } else {
        0.5
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

fn normalize(mut weights: Vec<f64>) -> Vec<f64> {
    let total = weights.iter().sum::<f64>();
    weights.iter_mut().for_each(|weight| *weight /= total);
    weights
}

fn softmax(scores: &[f64], temperature: f64) -> Vec<f64> {
    let maximum = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    normalize(
        scores
            .iter()
            .map(|score| ((score - maximum) * temperature).exp())
            .collect(),
    )
}

#[cfg(test)]
fn estimated_inclusion_chance(
    probabilities: &[f64],
    target_index: usize,
    reach: &ReachProfile,
    lexical_probability: f64,
    trials: usize,
    seed: u64,
) -> f64 {
    let distribution = SessionWordSampler::new(
        probabilities.to_vec(),
        vec![SelectionSource::Representative; probabilities.len()],
    );
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut included = 0_usize;
    for _ in 0..trials {
        let mut distribution = distribution.clone();
        let reached = reach.sample_reached(&mut rng);
        let mut seen = false;
        for _ in 0..reached {
            if lexical_probability < 1.0 && !rng.random_bool(lexical_probability) {
                continue;
            }
            seen |= distribution.sample_index(&mut rng).0 == target_index;
        }
        included += usize::from(seen);
    }
    included as f64 / trials as f64
}

fn approximate_inclusion_chance(
    probabilities: &[f64],
    target_index: usize,
    reach: &ReachProfile,
    lexical_probability: f64,
) -> f64 {
    let target_weight = probabilities[target_index];
    let mut other_remaining = vec![1.0; probabilities.len()];
    other_remaining[target_index] = 0.0;
    let mut target_survival = 1.0;
    let mut inclusion = 0.0;
    for reached in &reach.survival {
        if *reached <= f64::EPSILON || target_survival <= f64::EPSILON {
            break;
        }
        let other_total = probabilities
            .iter()
            .zip(&other_remaining)
            .map(|(weight, remaining)| weight * remaining)
            .sum::<f64>();
        let target_chance = target_weight / (target_weight + other_total);
        inclusion += reached * target_survival * lexical_probability * target_chance;
        target_survival *= 1.0 - lexical_probability * target_chance;
        if other_total > 0.0 {
            for (index, remaining) in other_remaining.iter_mut().enumerate() {
                if index != target_index {
                    *remaining *=
                        (1.0 - lexical_probability * probabilities[index] / other_total).max(0.0);
                }
            }
        }
    }
    inclusion.clamp(0.0, 1.0)
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
    /// Probabilidade marginal na distribuição usada para esta posição.
    pub propensity: f64,
}

/// Distribuição lexical calibrada uma vez para toda a sessão.
///
/// O alcance previsto determina a intensidade do treino; o sorteio de cada
/// palavra permanece independente e probabilístico.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionWordSampler {
    probabilities: Vec<f64>,
    remaining: Vec<f64>,
    remaining_total: f64,
    remaining_words: usize,
    sources: Vec<SelectionSource>,
}

impl SessionWordSampler {
    fn new(probabilities: Vec<f64>, sources: Vec<SelectionSource>) -> Self {
        let remaining_total = probabilities.iter().sum();
        let remaining_words = probabilities.len();
        Self {
            remaining: probabilities.clone(),
            probabilities,
            remaining_total,
            remaining_words,
            sources,
        }
    }

    pub fn sample<'a, R: Rng>(
        &mut self,
        candidates: &'a [String],
        rng: &mut R,
    ) -> SelectedWord<'a> {
        assert!(
            !candidates.is_empty(),
            "não é possível sortear sem candidatas"
        );
        assert_eq!(
            candidates.len(),
            self.probabilities.len(),
            "a distribuição pertence a outro conjunto de candidatas"
        );
        let (index, propensity) = self.sample_index(rng);
        SelectedWord {
            word: &candidates[index],
            source: self.sources[index],
            propensity,
        }
    }

    fn sample_index<R: Rng>(&mut self, rng: &mut R) -> (usize, f64) {
        if self.remaining_words == 0 {
            self.remaining.clone_from(&self.probabilities);
            self.remaining_total = self.probabilities.iter().sum();
            self.remaining_words = self.probabilities.len();
        }
        let roll = rng.random::<f64>() * self.remaining_total;
        let mut cumulative = 0.0;
        let index = self
            .remaining
            .iter()
            .enumerate()
            .filter(|(_, probability)| **probability > 0.0)
            .find_map(|(index, probability)| {
                cumulative += probability;
                (roll < cumulative).then_some(index)
            })
            .or_else(|| {
                self.remaining
                    .iter()
                    .rposition(|probability| *probability > 0.0)
            })
            .expect("a distribuição possui ao menos uma candidata restante");
        let propensity = self.remaining[index] / self.remaining_total;
        self.remaining_total -= self.remaining[index];
        self.remaining[index] = 0.0;
        self.remaining_words -= 1;
        (index, propensity)
    }

    #[cfg(test)]
    fn probability(&self, index: usize) -> f64 {
        self.probabilities[index]
    }
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

    pub fn estimated_generated_chances(
        &self,
        language: &str,
        targets: &[String],
        candidates: &[String],
        draws: usize,
    ) -> HashMap<String, f64> {
        self.estimated_generated_chances_with_number_probability(
            language, targets, candidates, draws, 0.0,
        )
    }

    pub fn estimated_generated_chances_with_number_probability(
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
        let chances = self.estimated_generated_group_chances_with_number_probability(
            language,
            &groups,
            candidates,
            draws,
            number_probability,
        );
        targets.iter().cloned().zip(chances).collect()
    }

    fn estimated_generated_group_chances_with_number_probability(
        &self,
        language: &str,
        target_groups: &[Vec<String>],
        candidates: &[String],
        draws: usize,
        number_probability: f64,
    ) -> Vec<f64> {
        self.estimated_reached_group_chances_with_number_probability(
            language,
            target_groups,
            candidates,
            &ReachProfile::certain(draws),
            number_probability,
        )
    }

    fn estimated_reached_group_chances_with_number_probability(
        &self,
        language: &str,
        target_groups: &[Vec<String>],
        candidates: &[String],
        reach: &ReachProfile,
        number_probability: f64,
    ) -> Vec<f64> {
        if target_groups.is_empty() || candidates.is_empty() || reach.positions() == 0 {
            return vec![0.0; target_groups.len()];
        }
        const TRIALS: usize = 4_096;
        let number_probability = number_probability.clamp(0.0, 1.0);
        let distribution =
            self.session_word_sampler(language, candidates, reach, number_probability);
        let target_sets = target_groups
            .iter()
            .map(|group| group.iter().map(String::as_str).collect::<HashSet<_>>())
            .collect::<Vec<_>>();
        let mut hasher = DefaultHasher::new();
        language.hash(&mut hasher);
        target_groups.hash(&mut hasher);
        candidates.hash(&mut hasher);
        reach
            .survival
            .iter()
            .for_each(|probability| probability.to_bits().hash(&mut hasher));
        number_probability.to_bits().hash(&mut hasher);
        let mut rng = SmallRng::seed_from_u64(hasher.finish());
        let mut counts = vec![0_usize; target_groups.len()];
        for _ in 0..TRIALS {
            let mut distribution = distribution.clone();
            let reached = reach.sample_reached(&mut rng);
            let mut seen = vec![false; target_groups.len()];
            for _ in 0..reached {
                if number_probability > 0.0 && rng.random_bool(number_probability) {
                    continue;
                }
                let selected = distribution.sample(candidates, &mut rng).word;
                for (group_index, targets) in target_sets.iter().enumerate() {
                    seen[group_index] |= targets.contains(selected);
                }
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

    /// Estima a chance de a pessoa realmente começar a digitar cada palavra,
    /// em vez da mera presença em alguma posição do buffer gerado.
    pub fn estimated_reached_uplifts_with_number_probability(
        &self,
        language: &str,
        targets: &[String],
        candidates: &[String],
        reach: &ReachProfile,
        number_probability: f64,
    ) -> HashMap<String, f64> {
        let groups = targets
            .iter()
            .map(|target| vec![target.clone()])
            .collect::<Vec<_>>();
        let uplifts = self.estimated_reached_group_uplifts_with_number_probability(
            language,
            &groups,
            candidates,
            reach,
            number_probability,
        );
        targets.iter().cloned().zip(uplifts).collect()
    }

    /// Estima a chance absoluta de começar cada palavra com a distribuição
    /// natural e com o treino adaptativo. Expor as duas medidas evita que a
    /// interface apresente um aumento sem contexto.
    pub fn estimated_reached_chances_with_number_probability(
        &self,
        language: &str,
        targets: &[String],
        candidates: &[String],
        reach: &ReachProfile,
        number_probability: f64,
    ) -> (HashMap<String, f64>, HashMap<String, f64>) {
        let groups = targets
            .iter()
            .map(|target| vec![target.clone()])
            .collect::<Vec<_>>();
        let adaptive = self.estimated_reached_group_chances_with_number_probability(
            language,
            &groups,
            candidates,
            reach,
            number_probability,
        );
        let representative = Self::new(self.policy)
            .estimated_reached_group_chances_with_number_probability(
                language,
                &groups,
                candidates,
                reach,
                number_probability,
            );
        (
            targets.iter().cloned().zip(representative).collect(),
            targets.iter().cloned().zip(adaptive).collect(),
        )
    }

    /// Estima o aumento da exposição real a qualquer palavra de cada grupo.
    pub fn estimated_reached_group_uplifts_with_number_probability(
        &self,
        language: &str,
        target_groups: &[Vec<String>],
        candidates: &[String],
        reach: &ReachProfile,
        number_probability: f64,
    ) -> Vec<f64> {
        let adaptive = self.estimated_reached_group_chances_with_number_probability(
            language,
            target_groups,
            candidates,
            reach,
            number_probability,
        );
        let representative = Self::new(self.policy)
            .estimated_reached_group_chances_with_number_probability(
                language,
                target_groups,
                candidates,
                reach,
                number_probability,
            );
        adaptive
            .into_iter()
            .zip(representative)
            .map(|(adaptive, representative)| (adaptive - representative).max(0.0))
            .collect()
    }

    /// Mede presença entre posições geradas e certamente alcançadas. Esta API
    /// existe para validação do sampler; a interface usa exposição alcançada.
    pub fn estimated_generated_uplifts_with_number_probability(
        &self,
        language: &str,
        targets: &[String],
        candidates: &[String],
        draws: usize,
        number_probability: f64,
    ) -> HashMap<String, f64> {
        let adaptive = self.estimated_generated_chances_with_number_probability(
            language,
            targets,
            candidates,
            draws,
            number_probability,
        );
        let representative = Self::new(self.policy)
            .estimated_generated_chances_with_number_probability(
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

    /// Equivalente agrupado da presença em posições certamente alcançadas.
    pub fn estimated_generated_group_uplifts_with_number_probability(
        &self,
        language: &str,
        target_groups: &[Vec<String>],
        candidates: &[String],
        draws: usize,
        number_probability: f64,
    ) -> Vec<f64> {
        let adaptive = self.estimated_generated_group_chances_with_number_probability(
            language,
            target_groups,
            candidates,
            draws,
            number_probability,
        );
        let representative = Self::new(self.policy)
            .estimated_generated_group_chances_with_number_probability(
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

    pub fn estimated_generated_chance(
        &self,
        language: &str,
        word: &str,
        candidates: &[String],
        draws: usize,
    ) -> f64 {
        self.estimated_generated_chances(language, &[word.to_owned()], candidates, draws)
            .get(word)
            .copied()
            .unwrap_or(0.0)
    }

    pub fn observe(&mut self, language: &str, word: &str, observation: Observation) {
        self.skills
            .entry((language.into(), word.into()))
            .or_default()
            .observe(observation);
        self.refresh_word_difficulty(language, word);
    }

    pub fn observe_pattern(
        &mut self,
        language: &str,
        word: &str,
        pattern: &str,
        observation: Observation,
    ) {
        self.ngram_skills
            .entry((language.into(), pattern.into()))
            .or_default()
            .observe(word, observation);
        self.refresh_ngram_difficulty(language, pattern);
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

    /// Calibra uma única distribuição contínua para a sessão. Uma palavra com
    /// dificuldade máxima tende ao teto de exposição da política; sinais
    /// menores interpolam entre a chance natural e esse teto.
    pub fn session_word_sampler(
        &self,
        language: &str,
        candidates: &[String],
        reach: &ReachProfile,
        number_probability: f64,
    ) -> SessionWordSampler {
        if candidates.is_empty() {
            return SessionWordSampler::default();
        }
        let priorities = candidates
            .iter()
            .map(|word| self.candidate_priority(language, word))
            .collect::<Vec<_>>();
        let (hardest_index, hardest_priority) = priorities
            .iter()
            .copied()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .expect("a lista de candidatas não está vazia");
        let uniform_probability = 1.0 / candidates.len() as f64;
        let lexical_probability = 1.0 - number_probability.clamp(0.0, 1.0);
        let expected_lexical_words = reach.survival.iter().sum::<f64>() * lexical_probability;
        let natural_exposure = (expected_lexical_words * uniform_probability).clamp(0.0, 1.0);
        if expected_lexical_words == 0.0 {
            return SessionWordSampler::new(
                vec![uniform_probability; candidates.len()],
                vec![SelectionSource::Representative; candidates.len()],
            );
        }
        let maximum_exposure = self
            .policy
            .maximum_session_exposure
            .clamp(natural_exposure, 1.0);
        let target_exposure =
            natural_exposure + hardest_priority * (maximum_exposure - natural_exposure);

        let probabilities = if hardest_priority < MINIMUM_ACTIONABLE_DIFFICULTY
            || target_exposure <= natural_exposure
        {
            self.exploration_probabilities(language, candidates)
        } else {
            self.calibrated_probabilities(
                &priorities,
                hardest_index,
                reach,
                lexical_probability,
                target_exposure,
            )
        };
        let sources = priorities
            .iter()
            .zip(&probabilities)
            .map(|(priority, probability)| {
                if *priority >= MINIMUM_ACTIONABLE_DIFFICULTY {
                    SelectionSource::Targeted
                } else if *probability > uniform_probability {
                    SelectionSource::Exploration
                } else {
                    SelectionSource::Representative
                }
            })
            .collect();
        SessionWordSampler::new(probabilities, sources)
    }

    /// Estima a curva de alcance usando a mesma distribuição lexical do
    /// treino e o desempenho previsto de cada candidata.
    pub fn forecast_reach(
        &self,
        language: &str,
        candidates: &[String],
        performances: &HashMap<String, WordPerformance>,
        selection_reach: &ReachProfile,
        forecast: ReachForecast,
    ) -> ReachProfile {
        if candidates.is_empty() || forecast.positions == 0 || forecast.trials == 0 {
            return ReachProfile::default();
        }
        let distribution = self.session_word_sampler(language, candidates, selection_reach, 0.0);
        let mut hasher = DefaultHasher::new();
        language.hash(&mut hasher);
        candidates.hash(&mut hasher);
        forecast.hash(&mut hasher);
        for candidate in candidates {
            let performance = performances
                .get(candidate)
                .expect("toda candidata precisa de uma previsão");
            performance
                .terminal_error_probability
                .to_bits()
                .hash(&mut hasher);
            performance.expected_ms.to_bits().hash(&mut hasher);
        }
        let mut rng = SmallRng::seed_from_u64(hasher.finish());
        let mut reached_counts = Vec::with_capacity(forecast.trials);
        for _ in 0..forecast.trials {
            let mut distribution = distribution.clone();
            let mut elapsed_ms = 0.0;
            let mut reached = 0;
            for _ in 0..forecast.positions {
                let word = distribution.sample(candidates, &mut rng).word;
                let performance = performances[word];
                elapsed_ms += performance.expected_ms.max(0.0);
                reached += 1;
                let failed = forecast.stops_on_error
                    && rng.random_bool(performance.terminal_error_probability.clamp(0.0, 1.0));
                let timed_out = forecast
                    .duration_ms
                    .is_some_and(|limit| elapsed_ms >= limit as f64);
                if failed || timed_out {
                    break;
                }
            }
            reached_counts.push(reached);
        }
        ReachProfile::from_reached_counts(reached_counts, forecast.positions)
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
        // A palavra observada domina o peso. Padrões compartilhados só
        // inclinam candidatas vizinhas, evitando que um n-grama dilua a
        // dificuldade concreta que o usuário acabou de demonstrar.
        (lexical.powi(2) + motor.powi(2) * 0.24 + mechanics.powi(2) * 0.08).min(1.0)
    }

    fn exploration_probabilities(&self, language: &str, candidates: &[String]) -> Vec<f64> {
        let weights = candidates
            .iter()
            .map(|word| {
                // A exploração apenas impede que poucas observações se tornem
                // uma certeza. Ela é uma inclinação pequena na mesma
                // distribuição, não um modo de teste separado.
                1.0 + self.exploration_value(language, word) * 0.08
            })
            .collect::<Vec<_>>();
        normalize(weights)
    }

    fn calibrated_probabilities(
        &self,
        priorities: &[f64],
        target_index: usize,
        reach: &ReachProfile,
        lexical_probability: f64,
        target_exposure: f64,
    ) -> Vec<f64> {
        const MAXIMUM_TEMPERATURE: f64 = 64.0;
        const ITERATIONS: usize = 24;

        let exposure_at = |temperature| {
            let probabilities = softmax(priorities, temperature);
            approximate_inclusion_chance(&probabilities, target_index, reach, lexical_probability)
        };
        if exposure_at(MAXIMUM_TEMPERATURE) < target_exposure {
            return softmax(priorities, MAXIMUM_TEMPERATURE);
        }
        let mut lower = 0.0;
        let mut upper = MAXIMUM_TEMPERATURE;
        for _ in 0..ITERATIONS {
            let midpoint = (lower + upper) / 2.0;
            if exposure_at(midpoint) < target_exposure {
                lower = midpoint;
            } else {
                upper = midpoint;
            }
        }
        softmax(priorities, upper)
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

    fn observe(skill: &mut WordSkill, errors: usize, corrections: usize, successes: usize) {
        for index in 0..(errors + corrections + successes) {
            skill.observe(Observation {
                confirmed_error: index < errors,
                corrected: (errors..errors + corrections).contains(&index),
                corrections: u32::from((errors..errors + corrections).contains(&index)),
                corrective_events: u16::from((errors..errors + corrections).contains(&index)),
                correction_ms: 0,
                correction_burden: f64::from((errors..errors + corrections).contains(&index)),
                fast_success: false,
                slow: false,
                latency_ratio: None,
                evidence_weight: 1.0,
            });
        }
    }

    #[test]
    fn inicio_frio_permanece_uniforme() {
        let words = ["um", "dois", "três", "quatro"].map(str::to_owned).to_vec();
        let distribution = AdaptiveSampler::default().session_word_sampler(
            "portuguese",
            &words,
            &ReachProfile::certain(8),
            0.0,
        );

        for index in 0..words.len() {
            assert_eq!(distribution.probability(index), 0.25);
        }
    }

    #[test]
    fn estado_serializado_fora_do_modelo_atual_e_rejeitado() {
        let formato_incompleto =
            postcard::to_allocvec(&(0.0_f64, 0.0_f64, 1.0_f64, 1_u32)).unwrap();

        assert!(WordSkill::decode(&formato_incompleto).is_err());
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
    fn reconstrucao_da_palavra_pesa_mais_que_um_backspace() {
        let policy = AdaptivePolicy::default();
        let mut leve = WordSkill::default();
        let mut intensa = WordSkill::default();
        for _ in 0..5 {
            leve.observe(Observation {
                corrected: true,
                corrections: 1,
                corrective_events: 1,
                correction_ms: 120,
                correction_burden: correction_burden(1, 1, 120, 700, 8),
                ..Observation::regular(false, false, false)
            });
            intensa.observe(Observation {
                corrected: true,
                corrections: 7,
                corrective_events: 3,
                correction_ms: 1_800,
                correction_burden: correction_burden(7, 3, 1_800, 700, 8),
                ..Observation::regular(false, false, false)
            });
        }

        assert!(
            policy.difficulty(&intensa) > policy.difficulty(&leve) * 1.25,
            "a intensidade da recuperação precisa alterar o treino"
        );
    }

    #[test]
    fn correcao_e_falha_da_mesma_tentativa_contam_juntas() {
        let mut skill = WordSkill::default();
        skill.observe(Observation {
            confirmed_error: true,
            corrected: true,
            corrections: 4,
            corrective_events: 2,
            correction_ms: 900,
            correction_burden: correction_burden(4, 2, 900, 500, 7),
            ..Observation::regular(false, false, false)
        });

        assert_eq!(skill.confirmed_errors, 1.0);
        assert_eq!(skill.corrections, 1.0);
        assert_eq!(skill.corrected_graphemes, 4.0);
        assert!(skill.correction_burden_mass > 0.0);
    }

    #[test]
    fn uma_correcao_isolada_nao_cria_aumento_relevante_no_sorteio() {
        let words = (0..200)
            .map(|index| format!("palavra{index}"))
            .collect::<Vec<_>>();
        let mut sampler = AdaptiveSampler::default();
        sampler.observe(
            "portuguese",
            &words[0],
            Observation::regular(false, true, false),
        );

        let uplifts = sampler.estimated_generated_uplifts_with_number_probability(
            "portuguese",
            &[words[0].clone()],
            &words,
            24,
            0.0,
        );

        assert!(uplifts[&words[0]] <= 0.01);
    }

    #[test]
    fn erro_recorrente_aumenta_dificuldade_sem_explodir() {
        let policy = AdaptivePolicy::default();
        let mut skill = WordSkill::default();
        observe(&mut skill, 10, 0, 10);
        let difficulty = policy.difficulty(&skill);
        assert!(difficulty > 0.0);
        assert!(difficulty <= 1.0);
    }

    #[test]
    fn dificuldade_extrema_aparece_em_dezenas_de_porcento_das_sessoes_curtas() {
        let words = (0..200)
            .map(|index| format!("palavra{index}"))
            .collect::<Vec<_>>();
        let target = words[0].clone();
        let mut sampler = AdaptiveSampler::default();
        for _ in 0..24 {
            sampler.observe(
                "portuguese",
                &target,
                Observation::regular(true, false, false),
            );
        }
        let reach = ReachProfile::certain(8);

        let (_, adaptive) = sampler.estimated_reached_chances_with_number_probability(
            "portuguese",
            std::slice::from_ref(&target),
            &words,
            &reach,
            0.0,
        );
        let chance = adaptive[&target];
        let natural = 8.0 / words.len() as f64;
        let difficulty = sampler.candidate_priority("portuguese", &target);
        let expected = natural + difficulty * (sampler.policy().maximum_session_exposure - natural);

        assert!(
            (chance - expected).abs() < 0.03,
            "chance calibrada {chance} divergiu do alvo {expected}"
        );
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
    fn sampler_esgota_as_candidatas_antes_de_repetir_e_registra_propensao() {
        let words = ["a", "b", "c"].map(str::to_owned).to_vec();
        let sampler = AdaptiveSampler::default();
        let mut rng = SmallRng::seed_from_u64(1);
        let mut distribution =
            sampler.session_word_sampler("english", &words, &ReachProfile::certain(3), 0.0);
        let selected = (0..words.len())
            .map(|_| distribution.sample(&words, &mut rng))
            .collect::<Vec<_>>();

        assert_eq!(
            selected
                .iter()
                .map(|word| word.word)
                .collect::<HashSet<_>>(),
            words.iter().map(String::as_str).collect()
        );
        assert!(
            selected
                .iter()
                .all(|word| word.source == SelectionSource::Representative)
        );
        assert!(
            selected
                .iter()
                .all(|word| word.propensity > 0.0 && word.propensity <= 1.0)
        );
    }

    #[test]
    fn curva_de_alcance_e_a_distribuicao_empirica_das_posicoes_digitadas() {
        let profile = ReachProfile::from_reached_counts([2, 4], 5);

        assert_eq!(profile.probability(0), 1.0);
        assert_eq!(profile.probability(1), 1.0);
        assert_eq!(profile.probability(2), 0.5);
        assert_eq!(profile.probability(3), 0.5);
        assert_eq!(profile.probability(4), 0.0);
    }

    #[test]
    fn previsao_de_alcance_aplica_o_desempenho_das_palavras() {
        let words = vec!["uma".to_owned()];
        let sampler = AdaptiveSampler::default();
        let performance = HashMap::from([(
            "uma".to_owned(),
            WordPerformance {
                terminal_error_probability: 0.0,
                expected_ms: 100.0,
            },
        )]);
        let forecast = ReachForecast {
            positions: 20,
            duration_ms: Some(1_000),
            stops_on_error: true,
            trials: 128,
        };

        let timed = sampler.forecast_reach(
            "portuguese",
            &words,
            &performance,
            &ReachProfile::certain(20),
            forecast,
        );
        assert_eq!(
            (0..20)
                .map(|position| timed.probability(position))
                .sum::<f64>(),
            10.0
        );

        let failure = HashMap::from([(
            "uma".to_owned(),
            WordPerformance {
                terminal_error_probability: 1.0,
                expected_ms: 100.0,
            },
        )]);
        let failed = sampler.forecast_reach(
            "portuguese",
            &words,
            &failure,
            &ReachProfile::certain(20),
            forecast,
        );
        assert_eq!(
            (0..20)
                .map(|position| failed.probability(position))
                .sum::<f64>(),
            1.0
        );
    }

    #[test]
    fn sessao_curta_e_censura_em_vez_de_falha_de_alcance() {
        let profile = ReachProfile::from_observations(
            [
                ReachObservation {
                    reached: 2,
                    terminal: false,
                },
                ReachObservation {
                    reached: 4,
                    terminal: true,
                },
            ],
            5,
        );

        assert_eq!(profile.probability(2), 1.0);
        assert_eq!(profile.probability(3), 1.0);
        assert_eq!(profile.probability(4), 0.0);
    }

    #[test]
    fn posicao_inalcancavel_preserva_a_distribuicao_base() {
        let words = ["alvo", "casa", "tempo", "mundo"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut sampler = AdaptiveSampler::default();
        for _ in 0..16 {
            sampler.observe(
                "portuguese",
                "alvo",
                Observation::regular(true, false, false),
            );
        }
        let mut rng = SmallRng::seed_from_u64(7);

        let mut distribution =
            sampler.session_word_sampler("portuguese", &words, &ReachProfile::default(), 0.0);
        let selected = distribution.sample(&words, &mut rng);

        assert_eq!(selected.source, SelectionSource::Representative);
        assert_eq!(selected.propensity, 0.25);
    }

    #[test]
    fn chance_de_exposicao_ignora_palavras_que_ficam_so_no_buffer() {
        let words = (0..64)
            .map(|index| format!("palavra{index}"))
            .collect::<Vec<_>>();
        let target = words[0].clone();
        let mut sampler = AdaptiveSampler::default();
        for _ in 0..24 {
            sampler.observe(
                "portuguese",
                &target,
                Observation::regular(true, false, false),
            );
        }
        let reach = ReachProfile::from_reached_counts([1], 20);
        let distribution = sampler.session_word_sampler("portuguese", &words, &reach, 0.0);
        let reached =
            estimated_inclusion_chance(&distribution.probabilities, 0, &reach, 1.0, 4_096, 7);
        let merely_generated = estimated_inclusion_chance(
            &distribution.probabilities,
            0,
            &ReachProfile::certain(20),
            1.0,
            4_096,
            7,
        );

        assert!(reached < merely_generated);
    }

    #[test]
    fn simulacao_da_sessao_respeita_o_sequenciador_real() {
        let words = ["a", "b", "c", "d"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let sampler = AdaptiveSampler::default();
        let chance = sampler.estimated_generated_chance("english", "a", &words, 2);
        assert!((0.0..=1.0).contains(&chance));
    }

    #[test]
    fn presenca_gerada_do_padrao_mede_o_grupo_inteiro() {
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

        let uplift = sampler.estimated_generated_group_uplifts_with_number_probability(
            "portuguese",
            &groups,
            &candidates,
            4,
            0.0,
        );

        assert!(uplift[0] > 0.0);
    }

    #[test]
    fn ngrama_acumula_evidencia_de_forma_continua() {
        let policy = AdaptivePolicy::default();
        let observation = Observation {
            confirmed_error: true,
            corrected: false,
            corrections: 0,
            corrective_events: 0,
            correction_ms: 0,
            correction_burden: 0.0,
            fast_success: false,
            slow: false,
            latency_ratio: None,
            evidence_weight: 1.0,
        };
        let mut skill = NgramSkill::default();
        skill.observe("primeiro", observation);
        assert!(policy.ngram_difficulty(&skill, PersonalBaseline::default()) > 0.0);
    }

    #[test]
    fn evidencia_descartada_nao_ativa_generalizacao_compartilhada() {
        let discarded = Observation {
            confirmed_error: true,
            corrected: false,
            corrections: 0,
            corrective_events: 0,
            correction_ms: 0,
            correction_burden: 0.0,
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
            let mut distribution = sampler.session_word_sampler(
                "portuguese",
                &words,
                &ReachProfile::certain(8),
                0.0,
            );
            let selected = distribution.sample(&words, &mut rng);
            prop_assert!(selected.propensity.is_finite());
            prop_assert!(selected.propensity > 0.0 && selected.propensity <= 1.0);
        }
    }
}
