//! Stall detection — tracks consecutive no-progress rounds.
//!
//! When the quality score stops improving across subtask completions, the
//! auto-runner triggers re-decomposition of remaining work.

/// Detects stagnation across consecutive subtask completions.
///
/// Each time a subtask finishes, the auto-runner records its final quality
/// score. If `stall_rounds` consecutive completions all have a quality score
/// below 70.0 — the same bar `AutoRunner` uses to accept a subtask — and the
/// sequence is not strictly improving, the detector signals a stall.
pub struct StallDetector {
    /// Number of consecutive no-progress rounds that trigger a stall.
    threshold: usize,
    /// Quality scores of the last `threshold` completions.
    recent_scores: Vec<f64>,
    /// Minimum quality considered "progress" (non-stalled).
    min_quality: f64,
}

impl StallDetector {
    /// Create a new stall detector that triggers after `threshold` consecutive
    /// rounds with no improvement.
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold: threshold.max(1),
            recent_scores: Vec::with_capacity(threshold),
            min_quality: 70.0,
        }
    }

    /// Record a new quality score from a subtask completion.
    pub fn record(&mut self, quality: f64) {
        self.recent_scores.push(quality);
        if self.recent_scores.len() > self.threshold {
            self.recent_scores.remove(0);
        }
    }

    /// Returns true if the auto-runner should be considered stalled.
    ///
    /// A stall is detected when we have `threshold` scores, all below
    /// [`Self::min_quality`], and the sequence is not monotonically improving.
    /// A strict monotone check (rather than an endpoint ratio) is required:
    /// `[10, 40, 30]` is not progress even though its endpoints rise 200%,
    /// because every score is still far below the quality bar.
    pub fn is_stalled(&self) -> bool {
        if self.recent_scores.len() < self.threshold {
            return false;
        }

        // Any score at/above the bar breaks the stall.
        let all_low = self.recent_scores.iter().all(|&s| s < self.min_quality);
        if !all_low {
            return false;
        }

        // Improving only if every step is strictly better than the last.
        let monotone_up = self
            .recent_scores
            .windows(2)
            .all(|w| w[1] > w[0]);
        if monotone_up {
            return false;
        }

        true
    }

    /// Number of consecutive stalled rounds observed.
    pub fn stalled_rounds(&self) -> usize {
        self.recent_scores.len()
    }

    /// Reset the detector (e.g. after re-decomposition).
    pub fn reset(&mut self) {
        self.recent_scores.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_stall_with_few_scores() {
        let mut d = StallDetector::new(3);
        d.record(50.0);
        d.record(50.0);
        assert!(!d.is_stalled()); // only 2 scores, threshold is 3
    }

    #[test]
    fn test_stall_with_all_low() {
        let mut d = StallDetector::new(3);
        d.record(50.0);
        d.record(40.0);
        d.record(30.0);
        assert!(d.is_stalled());
    }

    #[test]
    fn test_no_stall_when_improving() {
        let mut d = StallDetector::new(3);
        d.record(30.0);
        d.record(50.0);
        d.record(55.0); // trending up, last >= 50.0 -> no stall
        assert!(!d.is_stalled());
    }

    #[test]
    fn test_no_stall_with_high_score() {
        let mut d = StallDetector::new(3);
        d.record(30.0);
        d.record(80.0); // high score breaks the stall
        d.record(30.0);
        assert!(!d.is_stalled());
    }

    #[test]
    fn test_stall_on_non_monotone_noise() {
        // Endpoint ratio rises 200% but the window is not improving.
        let mut d = StallDetector::new(3);
        d.record(10.0);
        d.record(40.0);
        d.record(30.0);
        assert!(d.is_stalled());
    }

    #[test]
    fn test_reset() {
        let mut d = StallDetector::new(3);
        d.record(30.0);
        d.record(30.0);
        d.record(30.0);
        assert!(d.is_stalled());
        d.reset();
        assert!(!d.is_stalled());
        assert_eq!(d.stalled_rounds(), 0);
    }
}
