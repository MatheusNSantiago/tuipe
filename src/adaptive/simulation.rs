use std::collections::{HashMap, HashSet};

use rand::{Rng, SeedableRng, rngs::SmallRng};

use super::{AdaptiveSampler, Observation};

/// Simulação longitudinal determinística. Ela não pretende provar os
/// coeficientes; impede regressões óbvias de cobertura, explosão e aprendizado.
#[test]
fn duas_mil_sessoes_preservam_cobertura_e_encontram_dificuldade_real() {
    const SESSIONS: usize = 2_000;
    const DRAWS: usize = 12;
    let words = (0..80)
        // Grafemas sintéticos isolam a política lexical do custo dos n-gramas;
        // estes possuem uma simulação específica nos testes do modelo.
        .map(|index| char::from_u32(0x4e00 + index).unwrap().to_string())
        .collect::<Vec<_>>();
    let hard = words[17].clone();
    let mut sampler = AdaptiveSampler::default();
    let mut rng = SmallRng::seed_from_u64(0x5eed);
    let mut counts = HashMap::<String, usize>::new();
    let mut coverage = HashSet::<String>::new();

    for _ in 0..SESSIONS {
        let mut previous = Vec::<String>::new();
        for _ in 0..DRAWS {
            let guard = previous.iter().map(String::as_str).collect::<Vec<_>>();
            let selected = sampler
                .sample_with_provenance("portuguese", &words, &guard, &mut rng)
                .word
                .to_owned();
            *counts.entry(selected.clone()).or_default() += 1;
            coverage.insert(selected.clone());
            let error_rate = if selected == hard { 0.24 } else { 0.02 };
            let confirmed_error = rng.random_bool(error_rate);
            sampler.observe(
                "portuguese",
                &selected,
                Observation {
                    confirmed_error,
                    corrected: !confirmed_error && rng.random_bool(0.03),
                    fast_success: !confirmed_error,
                    slow: false,
                    latency_ratio: None,
                    evidence_weight: 1.0,
                },
            );
            previous.insert(0, selected);
            previous.truncate(2);
        }
    }

    let hard_count = counts[&hard];
    let average = counts.values().sum::<usize>() as f64 / words.len() as f64;
    let hard_share = hard_count as f64 / (SESSIONS * DRAWS) as f64;
    let session_chance = sampler.estimated_session_chance("portuguese", &hard, &words, DRAWS);
    assert_eq!(coverage.len(), words.len(), "a mistura perdeu cobertura");
    assert!(
        hard_count as f64 > average * 1.15,
        "o sinal recorrente não foi aprendido"
    );
    assert!(hard_share < 0.05, "uma palavra dominou o currículo");
    assert!(
        session_chance <= 0.40,
        "o sequenciador ultrapassou o teto de uma palavra"
    );
}
