//! XP, nível e streak com as fórmulas do Monkeytype para o escopo local.

use serde::{Deserialize, Serialize};

use crate::typing::{Metrics, TestConfig, TestMode};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct XpState {
    pub total: u64,
    pub last_completed_day: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreakState {
    pub current: u16,
    pub longest: u16,
    pub last_completed_day: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XpGain {
    pub gained: u64,
    pub daily_bonus: u64,
    pub streak: u16,
}

pub fn award(
    xp: &mut XpState,
    streak: &mut StreakState,
    config: &TestConfig,
    metrics: &Metrics,
    local_day: i32,
) -> XpGain {
    update_streak(streak, local_day);
    let base = (metrics.duration_ms as f64 / 1_000.0 * 2.0).round();
    let mut modifier = 1.0;
    if metrics.accuracy == 100.0 {
        modifier += 0.5;
    } else if metrics.characters.incorrect == 0
        && metrics.characters.extra == 0
        && metrics.characters.missed == 0
    {
        modifier += 0.25;
    }
    match config.mode {
        TestMode::Quote => modifier += 0.5,
        _ => {
            if config.punctuation {
                modifier += 0.4;
            }
            if config.numbers {
                modifier += 0.1;
            }
        }
    }
    modifier += (f64::from(streak.current).min(100.0) / 100.0 * 2.0 * 10.0).round() / 10.0;
    let accuracy_modifier = ((metrics.accuracy - 50.0) / 50.0).max(0.0);
    let earned = (base * modifier).round() * accuracy_modifier;
    let daily_bonus = if xp.last_completed_day.is_some_and(|day| day != local_day) {
        ((xp.total as f64 * 0.05).round() as u64).clamp(100, 1_000)
    } else {
        0
    };
    let gained = earned.round() as u64 + daily_bonus;
    xp.total += gained;
    xp.last_completed_day = Some(local_day);
    XpGain {
        gained,
        daily_bonus,
        streak: streak.current,
    }
}

pub fn level_from_total_xp(total: u64) -> u64 {
    (((392.0 * total as f64 + 22_801.0).sqrt() - 53.0) / 98.0).floor() as u64
}

fn update_streak(streak: &mut StreakState, day: i32) {
    match streak.last_completed_day {
        Some(previous) if previous == day => {}
        Some(previous) if previous + 1 == day => streak.current += 1,
        _ => streak.current = 1,
    }
    streak.longest = streak.longest.max(streak.current);
    streak.last_completed_day = Some(day);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streak_e_bonus_diario_seguem_os_limites_da_referencia() {
        let mut xp = XpState::default();
        let mut streak = StreakState::default();
        let metrics = Metrics {
            duration_ms: 30_000,
            accuracy: 100.0,
            ..Metrics::default()
        };
        let first = award(&mut xp, &mut streak, &TestConfig::default(), &metrics, 10);
        let second = award(&mut xp, &mut streak, &TestConfig::default(), &metrics, 11);
        assert_eq!(first.streak, 1);
        assert_eq!(second.streak, 2);
        assert_eq!(second.daily_bonus, 100);
    }
}
