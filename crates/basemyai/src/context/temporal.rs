// SPDX-License-Identifier: BUSL-1.1
//! Statut temporel et facteur de fraicheur conservateur.

use super::ContextTemporalStatus;
use crate::Validity;

const FRESHNESS_FLOOR: f64 = 0.90;
const FRESHNESS_HORIZON_SECONDS: f64 = 30.0 * 24.0 * 60.0 * 60.0;

pub(super) fn status(validity: Validity, at: i64) -> ContextTemporalStatus {
    if validity.valid_until.is_some_and(|until| until <= at) {
        ContextTemporalStatus::Expired
    } else if validity.valid_from > at {
        ContextTemporalStatus::Scheduled
    } else {
        ContextTemporalStatus::Current
    }
}

/// Facteur borne dans `[0.9, 1.0]` : la recence departage doucement sans
/// ecraser la pertinence du recall. Ce n'est ni un TTL ni une preuve de verite.
pub(super) fn freshness_weight(validity: Validity, at: i64) -> f64 {
    let age_seconds = at.saturating_sub(validity.valid_from).max(0) as f64;
    FRESHNESS_FLOOR + (1.0 - FRESHNESS_FLOOR) / (1.0 + age_seconds / FRESHNESS_HORIZON_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_distinguishes_current_scheduled_and_expired() {
        assert_eq!(status(Validity::since(10), 20), ContextTemporalStatus::Current);
        assert_eq!(status(Validity::since(30), 20), ContextTemporalStatus::Scheduled);
        assert_eq!(
            status(
                Validity {
                    valid_from: 0,
                    valid_until: Some(20),
                },
                20,
            ),
            ContextTemporalStatus::Expired
        );
    }

    #[test]
    fn freshness_is_bounded_and_rewards_recency_softly() {
        let recent = freshness_weight(Validity::since(100), 100);
        let old = freshness_weight(Validity::since(0), 100 * 365 * 24 * 60 * 60);
        assert!((recent - 1.0).abs() < f64::EPSILON);
        assert!((FRESHNESS_FLOOR..recent).contains(&old));
    }
}
