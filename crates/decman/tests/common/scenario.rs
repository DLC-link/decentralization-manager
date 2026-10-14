//! The [`penelope`] Given/When/Then runner, bound to decman's [`Fixture`].

use super::Fixture;

/// A scenario that runs against the three-participant [`Fixture`]. Naming the
/// fixture here lets a phase write `Scenario::new(..)` and still read
/// `f.p1` inside its step closures.
pub type Scenario<Ctx = ()> = penelope::Scenario<Fixture, Ctx>;

impl penelope::Fixture for Fixture {
    fn probe_diag(&self) -> &penelope::ProbeDiag {
        &self.probe_diag
    }
}
