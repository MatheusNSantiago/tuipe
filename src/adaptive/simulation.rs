use std::collections::{HashMap, HashSet};

use rand::{Rng, SeedableRng, rngs::SmallRng};

use super::{AdaptiveSampler, Observation, ReachProfile};

/// Simulação longitudinal determinística. Ela não pretende provar os
/// coeficientes; impede regressões óbvias de cobertura, explosão e aprendizado.
#[test]
fn duas_mil_sessoes_preservam_cobertura_e_encontram_dificuldade_real() {
    const SESSIONS: usize = 2_000;
    const MAX_POSITIONS: usize = 14;
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
    let reach = ReachProfile::from_reached_counts([8, 10, 12, 14], MAX_POSITIONS);
    let mut reached_total = 0_usize;

    for _ in 0..SESSIONS {
        let reached = reach.sample_reached(&mut rng);
        reached_total += reached;
        for position in 0..reached {
            let selected = sampler
                .sample_with_provenance_at_reach(
                    "portuguese",
                    &words,
                    reach.probability(position),
                    &mut rng,
                )
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
                    corrections: 0,
                    corrective_events: 0,
                    correction_ms: 0,
                    correction_burden: 0.0,
                    fast_success: !confirmed_error,
                    slow: false,
                    latency_ratio: None,
                    evidence_weight: 1.0,
                },
            );
        }
    }

    let hard_count = counts[&hard];
    let average = counts.values().sum::<usize>() as f64 / words.len() as f64;
    let hard_share = hard_count as f64 / reached_total as f64;
    let exposure_uplift = sampler.estimated_reached_uplifts_with_number_probability(
        "portuguese",
        std::slice::from_ref(&hard),
        &words,
        &reach,
        0.0,
    )[&hard];
    assert_eq!(coverage.len(), words.len(), "a mistura perdeu cobertura");
    assert!(
        hard_count as f64 > average * 1.15,
        "o sinal recorrente não foi aprendido"
    );
    assert!(hard_share < 0.25, "uma palavra dominou o currículo");
    assert!(
        exposure_uplift <= 0.55,
        "o sequenciador ultrapassou o teto de uma palavra: {exposure_uplift}"
    );
}
