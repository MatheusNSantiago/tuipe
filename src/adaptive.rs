//! Modelo limitado de prioridade de palavras orientado por evidências.
//!
//! Este módulo não conhece SQLite nem eventos do terminal. A persistência
//! materializa [`WordSkill`], enquanto o sampler recebe um RNG determinístico.

use std::collections::HashMap;

use rand::{
    Rng,
    distr::{Distribution, weighted::WeightedIndex},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptivePolicy {
    /// Sinal mais forte: palavra confirmada incorretamente sem correção.
    pub confirmed_error_weight: f64,
    /// Correções recorrentes são relevantes; as duas primeiras são ruído.
    pub correction_weight: f64,
    /// O sucesso pesa menos de propósito: a recuperação exige boas sessões repetidas.
    pub fast_success_weight: f64,
    /// Contribuição multiplicativa máxima acima da base uniforme.
    pub maximum_boost: f64,
    /// Escala de evidência na qual a sigmoide deixa sua região conservadora.
    pub evidence_midpoint: f64,
}

impl Default for AdaptivePolicy {
    fn default() -> Self {
        Self {
            confirmed_error_weight: 1.0,
            correction_weight: 0.22,
            fast_success_weight: 0.16,
            maximum_boost: 3.0,
            evidence_midpoint: 3.0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordSkill {
    pub confirmed_errors: f64,
    pub corrections: f64,
    pub fast_successes: f64,
    #[serde(default)]
    pub slowdowns: f64,
    pub observations: u32,
}

impl WordSkill {
    pub fn observe(&mut self, observation: Observation) {
        self.observations += 1;
        self.confirmed_errors += f64::from(observation.confirmed_error);
        self.corrections += f64::from(observation.corrected) * observation.repeat_discount;
        self.fast_successes += f64::from(observation.fast_success) * observation.repeat_discount;
        self.slowdowns += f64::from(observation.slow) * observation.repeat_discount;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub confirmed_error: bool,
    pub corrected: bool,
    pub fast_success: bool,
    pub slow: bool,
    /// Repetir o mesmo teste reduz toda evidência não terminal.
    pub repeat_discount: f64,
}

impl Observation {
    pub fn regular(confirmed_error: bool, corrected: bool, fast_success: bool) -> Self {
        Self {
            confirmed_error,
            corrected,
            fast_success,
            slow: false,
            repeat_discount: 1.0,
        }
    }
}

impl AdaptivePolicy {
    pub fn difficulty(&self, skill: &WordSkill) -> f64 {
        // Uma correção eventual é comum e não deve transformar uma palavra em
        // prioridade. O sinal só começa na terceira ocorrência e cresce de
        // maneira gradual; erros confirmados permanecem independentes.
        let recurring_corrections = (skill.corrections - 2.0).max(0.0).powf(1.35);
        let evidence = skill.confirmed_errors * self.confirmed_error_weight
            + recurring_corrections * self.correction_weight
            + skill.slowdowns * 0.25
            - skill.fast_successes * self.fast_success_weight;
        let confidence = 1.0 - (-(skill.observations as f64) / self.evidence_midpoint).exp();
        let sigmoid = 1.0 / (1.0 + (-evidence).exp());
        ((sigmoid - 0.5) * 2.0 * confidence).max(0.0)
    }

    pub fn weight(&self, skill: Option<&WordSkill>) -> f64 {
        let difficulty = skill.map_or(0.0, |skill| self.difficulty(skill));
        1.0 + difficulty * self.maximum_boost
    }
}

#[derive(Debug, Clone, Default)]
pub struct AdaptiveSampler {
    policy: AdaptivePolicy,
    skills: HashMap<(String, String), WordSkill>,
}

impl AdaptiveSampler {
    pub fn new(policy: AdaptivePolicy) -> Self {
        Self {
            policy,
            skills: HashMap::new(),
        }
    }

    /// Restaura o estado materializado no SQLite sem expor sua representação
    /// interna ao restante da aplicação.
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
        }
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

    /// Aproxima a chance de a palavra aparecer em uma sessão. O cálculo parte
    /// da distribuição ponderada de cada sorteio; a proteção contra repetição
    /// só reduz levemente essa estimativa e não altera a ordem de prioridade.
    pub fn estimated_session_chance(
        &self,
        language: &str,
        word: &str,
        candidates: &[String],
        draws: usize,
    ) -> f64 {
        let total = candidates
            .iter()
            .map(|candidate| self.policy.weight(self.skill(language, candidate)))
            .sum::<f64>();
        if total == 0.0 || draws == 0 || !candidates.iter().any(|candidate| candidate == word) {
            return 0.0;
        }
        let per_draw = self.policy.weight(self.skill(language, word)) / total;
        1.0 - (1.0 - per_draw).powi(draws.min(i32::MAX as usize) as i32)
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

    /// Sorteia de uma mistura limitada, excluindo as duas palavras anteriores.
    /// Sem histórico, todo peso vale exatamente um; portanto o início frio usa
    /// a mesma distribuição uniforme do Monkeytype.
    pub fn sample<'a, R: Rng>(
        &self,
        language: &str,
        candidates: &'a [String],
        previous: &[&str],
        rng: &mut R,
    ) -> &'a str {
        let eligible = candidates
            .iter()
            .filter(|word| !previous.contains(&word.as_str()))
            .collect::<Vec<_>>();
        let eligible = if eligible.is_empty() {
            candidates.iter().collect::<Vec<_>>()
        } else {
            eligible
        };
        let weights = eligible
            .iter()
            .map(|word| self.policy.weight(self.skill(language, word)))
            .collect::<Vec<_>>();
        let index = WeightedIndex::new(weights)
            .expect("word packs are nonempty and adaptive weights are finite")
            .sample(rng);
        eligible[index]
    }
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::SmallRng};

    use super::*;

    #[test]
    fn cold_start_weights_are_uniform() {
        let policy = AdaptivePolicy::default();
        assert_eq!(policy.weight(None), 1.0);
        assert_eq!(policy.weight(Some(&WordSkill::default())), 1.0);
    }

    #[test]
    fn recurring_errors_raise_priority_but_the_curve_is_bounded() {
        let policy = AdaptivePolicy::default();
        let skill = WordSkill {
            confirmed_errors: 100.0,
            observations: 100,
            ..WordSkill::default()
        };
        assert!(policy.weight(Some(&skill)) > policy.weight(None));
        assert!(policy.weight(Some(&skill)) <= 1.0 + policy.maximum_boost);
    }

    #[test]
    fn uma_ou_duas_correcoes_nao_criam_prioridade() {
        let policy = AdaptivePolicy::default();
        for corrections in [1.0, 2.0] {
            let skill = WordSkill {
                corrections,
                observations: corrections as u32,
                ..WordSkill::default()
            };
            assert_eq!(policy.weight(Some(&skill)), 1.0);
        }
    }

    #[test]
    fn correcoes_recorrentes_aumentam_prioridade_gradualmente() {
        let policy = AdaptivePolicy::default();
        let three = WordSkill {
            corrections: 3.0,
            observations: 3,
            ..WordSkill::default()
        };
        let eight = WordSkill {
            corrections: 8.0,
            observations: 8,
            ..WordSkill::default()
        };
        assert!(policy.weight(Some(&three)) > 1.0);
        assert!(policy.weight(Some(&eight)) > policy.weight(Some(&three)));
    }

    #[test]
    fn sampler_honors_the_two_previous_word_guard() {
        let words = ["a", "b", "c"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let sampler = AdaptiveSampler::default();
        let mut rng = SmallRng::seed_from_u64(1);
        assert_eq!(
            sampler.sample("english", &words, &["a", "b"], &mut rng),
            "c"
        );
    }
}
