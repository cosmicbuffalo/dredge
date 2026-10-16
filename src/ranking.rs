//! Ranking constants are centralized here so evaluation can tune behavior.

pub const RELEVANCE_WEIGHT: f32 = 0.82;
pub const RECENCY_WEIGHT: f32 = 0.18;
pub const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;

pub fn recency(now: i64, updated_at: i64) -> f32 {
    let age_seconds = now.saturating_sub(updated_at).max(0) as f64;
    let age_days = age_seconds / 86_400.0;
    (-std::f64::consts::LN_2 * age_days / RECENCY_HALF_LIFE_DAYS)
        .exp()
        .clamp(0.0, 1.0) as f32
}

pub fn combine(relevance: f32, recency: f32) -> f32 {
    (RELEVANCE_WEIGHT * relevance.clamp(0.0, 1.0) + RECENCY_WEIGHT * recency.clamp(0.0, 1.0))
        .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recency_halves_in_thirty_days() {
        let now = 2_000_000_000;
        let value = recency(now, now - 30 * 86_400);
        assert!((value - 0.5).abs() < 0.0001);
    }

    #[test]
    fn weights_are_normalized() {
        assert_eq!(combine(1.0, 1.0), 1.0);
        assert_eq!(combine(0.0, 0.0), 0.0);
    }
}
