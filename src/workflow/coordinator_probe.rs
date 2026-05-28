use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum BudgetState {
    Tolerate,
    Expired,
}

pub struct BudgetTracker {
    first_failure_at: Option<Instant>,
    budget: Duration,
}

impl BudgetTracker {
    pub fn new(budget: Duration) -> Self {
        Self {
            first_failure_at: None,
            budget,
        }
    }

    pub fn record_failure(&mut self) -> BudgetState {
        let now = Instant::now();
        match self.first_failure_at {
            None => {
                self.first_failure_at = Some(now);
                BudgetState::Tolerate
            }
            Some(t0) if now.duration_since(t0) > self.budget => BudgetState::Expired,
            Some(_) => BudgetState::Tolerate,
        }
    }

    pub fn reset(&mut self) {
        self.first_failure_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_failure_tolerates() {
        let mut t = BudgetTracker::new(Duration::from_secs(180));
        assert_eq!(t.record_failure(), BudgetState::Tolerate);
    }

    #[test]
    fn expires_after_budget() {
        let mut t = BudgetTracker::new(Duration::from_micros(1));
        assert_eq!(t.record_failure(), BudgetState::Tolerate);
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(t.record_failure(), BudgetState::Expired);
    }

    #[test]
    fn reset_clears_first_failure() {
        let mut t = BudgetTracker::new(Duration::from_micros(1));
        let _ = t.record_failure();
        std::thread::sleep(Duration::from_millis(2));
        t.reset();
        assert_eq!(t.record_failure(), BudgetState::Tolerate);
    }
}
