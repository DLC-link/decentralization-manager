use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

use anyhow::Result;
use tracing::{error, info};

use super::Fixture;

pub type BoxFut<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub type BoxProbe<'a> = Pin<Box<dyn Future<Output = Option<Result<()>>> + Send + 'a>>;

#[derive(Debug, Clone, Copy)]
pub enum StepKind {
    Given,
    When,
    Then,
    GivenEventually,
    ThenEventually,
}

impl StepKind {
    pub fn label(self) -> &'static str {
        match self {
            StepKind::Given => "GIVEN",
            StepKind::When => "WHEN ",
            StepKind::Then => "THEN ",
            StepKind::GivenEventually => "GIVEN_EVENTUALLY",
            StepKind::ThenEventually => "THEN_EVENTUALLY",
        }
    }

    /// For eventually-style steps, the bare kind word ("GIVEN" or "THEN")
    /// used in log lines and error context, without the `_EVENTUALLY` suffix.
    /// Panics if called on an immediate kind (Given/When/Then).
    fn eventually_word(self) -> &'static str {
        match self {
            StepKind::GivenEventually => "GIVEN",
            StepKind::ThenEventually => "THEN",
            _ => unreachable!("eventually_word called on non-eventually kind: {self:?}"),
        }
    }
}

type ImmediateBody<Ctx> =
    Box<dyn for<'a> FnMut(&'a mut Fixture, &'a mut Ctx) -> BoxFut<'a> + Send + 'static>;

type ProbeBody<Ctx> =
    Box<dyn for<'a> FnMut(&'a mut Fixture, &'a mut Ctx) -> BoxProbe<'a> + Send + 'static>;

enum Step<Ctx> {
    Immediate {
        kind: StepKind,
        name: String,
        body: ImmediateBody<Ctx>,
    },
    Eventually {
        kind: StepKind,
        name: String,
        deadline: Duration,
        probe: ProbeBody<Ctx>,
    },
}

pub struct Scenario<Ctx> {
    name: String,
    ctx: Ctx,
    steps: Vec<Step<Ctx>>,
}

impl Scenario<()> {
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_ctx(name, ())
    }
}

impl<Ctx: Send + 'static> Scenario<Ctx> {
    pub fn with_ctx(name: impl Into<String>, ctx: Ctx) -> Self {
        Self {
            name: name.into(),
            ctx,
            steps: Vec::new(),
        }
    }

    pub fn given<F>(mut self, name: impl Into<String>, body: F) -> Self
    where
        F: for<'a> FnMut(&'a mut Fixture, &'a mut Ctx) -> BoxFut<'a> + Send + 'static,
    {
        self.steps.push(Step::Immediate {
            kind: StepKind::Given,
            name: name.into(),
            body: Box::new(body),
        });
        self
    }

    pub fn when<F>(mut self, name: impl Into<String>, body: F) -> Self
    where
        F: for<'a> FnMut(&'a mut Fixture, &'a mut Ctx) -> BoxFut<'a> + Send + 'static,
    {
        self.steps.push(Step::Immediate {
            kind: StepKind::When,
            name: name.into(),
            body: Box::new(body),
        });
        self
    }

    pub fn then<F>(mut self, name: impl Into<String>, body: F) -> Self
    where
        F: for<'a> FnMut(&'a mut Fixture, &'a mut Ctx) -> BoxFut<'a> + Send + 'static,
    {
        self.steps.push(Step::Immediate {
            kind: StepKind::Then,
            name: name.into(),
            body: Box::new(body),
        });
        self
    }

    /// Add a polling precondition step. Same shape as `then_eventually` but
    /// conceptually a Given — the scenario waits for `probe` to return
    /// `Some(Ok(()))` before any subsequent step runs. Used when a precondition
    /// is async (e.g. waiting for state to propagate via a background channel).
    /// In logs the kind word is `GIVEN`; in error context it's `GIVEN_EVENTUALLY`.
    pub fn given_eventually<F>(
        mut self,
        name: impl Into<String>,
        deadline: Duration,
        probe: F,
    ) -> Self
    where
        F: for<'a> FnMut(&'a mut Fixture, &'a mut Ctx) -> BoxProbe<'a> + Send + 'static,
    {
        self.steps.push(Step::Eventually {
            kind: StepKind::GivenEventually,
            name: name.into(),
            deadline,
            probe: Box::new(probe),
        });
        self
    }

    pub fn then_eventually<F>(
        mut self,
        name: impl Into<String>,
        deadline: Duration,
        probe: F,
    ) -> Self
    where
        F: for<'a> FnMut(&'a mut Fixture, &'a mut Ctx) -> BoxProbe<'a> + Send + 'static,
    {
        self.steps.push(Step::Eventually {
            kind: StepKind::ThenEventually,
            name: name.into(),
            deadline,
            probe: Box::new(probe),
        });
        self
    }

    pub async fn run(mut self, f: &mut Fixture) -> Result<()> {
        info!(scenario = %self.name, "Scenario \"{}\"", self.name);
        let scenario_start = Instant::now();

        const POLL_INTERVAL: Duration = Duration::from_secs(2);

        for step in &mut self.steps {
            match step {
                Step::Immediate { kind, name, body } => {
                    info!(step_kind = ?kind, step_name = %name, "  {} {}", kind.label(), name);
                    let fut = body(f, &mut self.ctx);
                    match fut.await {
                        Ok(()) => {}
                        Err(e) => {
                            error!(
                                scenario = %self.name,
                                "Scenario \"{}\" failed at {} \"{}\"",
                                self.name,
                                kind.label(),
                                name
                            );
                            return Err(e
                                .context(format!("{} \"{}\"", kind.label(), name))
                                .context(format!("scenario \"{}\" failed", self.name)));
                        }
                    }
                }
                Step::Eventually {
                    kind,
                    name,
                    deadline,
                    probe,
                } => {
                    info!(
                        step_kind = ?kind,
                        step_name = %name,
                        "  {:<5} eventually {}",
                        kind.eventually_word(),
                        name
                    );
                    let step_start = Instant::now();
                    let outcome: Result<()> = loop {
                        match probe(f, &mut self.ctx).await {
                            Some(Ok(())) => break Ok(()),
                            Some(Err(e)) => {
                                break Err(e.context(format!(
                                    "{} eventually \"{}\" failed",
                                    kind.eventually_word(),
                                    name
                                )));
                            }
                            None => {
                                let elapsed = step_start.elapsed();
                                if elapsed >= *deadline {
                                    break Err(anyhow::anyhow!(
                                        "{} eventually \"{}\" timed out after {:?}",
                                        kind.eventually_word(),
                                        name,
                                        *deadline
                                    ));
                                }
                                let remaining = *deadline - elapsed;
                                tokio::time::sleep(std::cmp::min(POLL_INTERVAL, remaining)).await;
                            }
                        }
                    };
                    let took = step_start.elapsed();
                    match outcome {
                        Ok(()) => info!("    ✓ (took {:.1}s)", took.as_secs_f64()),
                        Err(e) => {
                            error!(
                                scenario = %self.name,
                                "Scenario \"{}\" failed at {} \"{}\"",
                                self.name,
                                kind.label(),
                                name
                            );
                            return Err(e.context(format!("scenario \"{}\" failed", self.name)));
                        }
                    }
                }
            }
        }

        info!(
            scenario = %self.name,
            "Scenario \"{}\" complete ({:.1}s)",
            self.name,
            scenario_start.elapsed().as_secs_f64()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicU32, Ordering},
        },
        time::{Duration, Instant},
    };

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn steps_execute_in_order() {
        let mut f = Fixture::for_test();
        let order = Arc::new(Mutex::new(Vec::<u32>::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        let o3 = order.clone();

        Scenario::new("ordering")
            .given("g", move |_f, _c| {
                let o = o1.clone();
                Box::pin(async move {
                    o.lock().unwrap().push(1);
                    Ok(())
                })
            })
            .when("w", move |_f, _c| {
                let o = o2.clone();
                Box::pin(async move {
                    o.lock().unwrap().push(2);
                    Ok(())
                })
            })
            .then("t", move |_f, _c| {
                let o = o3.clone();
                Box::pin(async move {
                    o.lock().unwrap().push(3);
                    Ok(())
                })
            })
            .run(&mut f)
            .await
            .unwrap();

        assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn step_error_wraps_with_scenario_and_step_context() {
        let mut f = Fixture::for_test();
        let err = Scenario::new("my scn")
            .when("the failing step", |_f, _c| {
                Box::pin(async move { Err(anyhow::anyhow!("boom")) })
            })
            .run(&mut f)
            .await
            .unwrap_err();

        let chain = format!("{err:#}");
        assert!(chain.contains("scenario \"my scn\" failed"), "got: {chain}");
        assert!(
            chain.contains("WHEN") && chain.contains("the failing step"),
            "got: {chain}"
        );
        assert!(chain.contains("boom"), "got: {chain}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn then_eventually_retries_until_some_ok() {
        let mut f = Fixture::for_test();
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        Scenario::new("retry")
            .then_eventually(
                "eventually ready",
                Duration::from_secs(10),
                move |_f, _c| {
                    let c = c.clone();
                    Box::pin(async move {
                        let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                        if n >= 3 { Some(Ok(())) } else { None }
                    })
                },
            )
            .run(&mut f)
            .await
            .unwrap();
        assert!(counter.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn then_eventually_fail_fasts_on_some_err() {
        let mut f = Fixture::for_test();
        let started = Instant::now();
        let err = Scenario::new("fast-fail")
            .then_eventually("explodes", Duration::from_secs(30), |_f, _c| {
                Box::pin(async move { Some(Err(anyhow::anyhow!("kaboom"))) })
            })
            .run(&mut f)
            .await
            .unwrap_err();

        // Should NOT wait for the 30s deadline.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "elapsed: {:?}",
            started.elapsed()
        );
        let chain = format!("{err:#}");
        assert!(chain.contains("kaboom"), "got: {chain}");
        assert!(chain.contains("scenario \"fast-fail\""), "got: {chain}");
        // Probe-error path must wrap with "failed", not "timed out".
        assert!(
            chain.contains("THEN eventually \"explodes\" failed"),
            "got: {chain}"
        );
        assert!(!chain.contains("timed out"), "got: {chain}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn then_eventually_times_out_when_probe_only_returns_none() {
        let mut f = Fixture::for_test();
        let started = Instant::now();
        let err = Scenario::new("never-ready")
            .then_eventually("eternal None", Duration::from_millis(100), |_f, _c| {
                Box::pin(async move { None })
            })
            .run(&mut f)
            .await
            .unwrap_err();

        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(100),
            "elapsed: {elapsed:?}"
        );
        // Sleep must respect the remaining deadline, not always wait the full
        // POLL_INTERVAL. Loose upper bound to avoid flakes on slow CI.
        assert!(
            elapsed < Duration::from_secs(1),
            "deadline overshoot: {elapsed:?}"
        );
        let chain = format!("{err:#}");
        assert!(
            chain.contains("scenario \"never-ready\" failed"),
            "got: {chain}"
        );
        assert!(chain.contains("eternal None"), "got: {chain}");
        assert!(chain.contains("timed out"), "got: {chain}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ctx_mutations_are_visible_to_later_steps() {
        let mut f = Fixture::for_test();
        #[derive(Default)]
        struct C {
            n: u32,
        }

        Scenario::with_ctx("ctx", C::default())
            .when("write", |_f, c| {
                Box::pin(async move {
                    c.n = 42;
                    Ok(())
                })
            })
            .then("read", |_f, c| {
                Box::pin(async move {
                    assert_eq!(c.n, 42);
                    Ok(())
                })
            })
            .run(&mut f)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn given_eventually_runs_before_when() {
        // Verifies that `given_eventually` polls until success and that the
        // step it gates (the When that follows) only runs after the precondition
        // holds. Also verifies the kind word in the failure error context is
        // GIVEN, not THEN.
        let mut f = Fixture::for_test();
        let probe_count = Arc::new(AtomicU32::new(0));
        let when_count = Arc::new(AtomicU32::new(0));
        let pc = probe_count.clone();
        let wc = when_count.clone();

        Scenario::new("given-eventually")
            .given_eventually(
                "precondition holds eventually",
                Duration::from_secs(10),
                move |_f, _c| {
                    let pc = pc.clone();
                    Box::pin(async move {
                        let n = pc.fetch_add(1, Ordering::SeqCst) + 1;
                        if n >= 2 { Some(Ok(())) } else { None }
                    })
                },
            )
            .when("after precondition", move |_f, _c| {
                let wc = wc.clone();
                Box::pin(async move {
                    wc.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .run(&mut f)
            .await
            .unwrap();

        assert!(probe_count.load(Ordering::SeqCst) >= 2);
        assert_eq!(when_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn given_eventually_timeout_error_uses_given_word() {
        let mut f = Fixture::for_test();
        let err = Scenario::new("never-precondition")
            .given_eventually("never holds", Duration::from_millis(50), |_f, _c| {
                Box::pin(async move { None })
            })
            .run(&mut f)
            .await
            .unwrap_err();

        let chain = format!("{err:#}");
        // Error context must say GIVEN eventually, not THEN eventually.
        assert!(
            chain.contains("GIVEN eventually \"never holds\""),
            "expected GIVEN in chain, got: {chain}"
        );
        assert!(!chain.contains("THEN eventually"), "got: {chain}");
        assert!(
            chain.contains("scenario \"never-precondition\" failed"),
            "got: {chain}"
        );
    }

    /// Smoke test for the HRTB closure pattern used by all phase tasks:
    /// a closure that captures a non-static `String` AND borrows the
    /// `&mut Fixture` parameter inside the future. If this fails to compile,
    /// every phase in plan-pt2 will also fail. Locks the type-system shape
    /// before phase work begins.
    #[tokio::test(flavor = "multi_thread")]
    async fn closure_can_capture_string_and_borrow_fixture() {
        let mut f = Fixture::for_test();
        let label = String::from("hello");

        Scenario::new("hrtb-smoke")
            .when("uses captured string and borrows f", {
                let label = label.clone();
                move |f, _c| {
                    let label = label.clone();
                    Box::pin(async move {
                        assert_eq!(f.p1.http, 8081);
                        assert_eq!(label, "hello");
                        Ok(())
                    })
                }
            })
            .then_eventually(
                "captured string still readable in eventually",
                Duration::from_secs(1),
                {
                    let label = label.clone();
                    move |f, _c| {
                        let label = label.clone();
                        Box::pin(async move {
                            assert_eq!(f.p2.http, 8082);
                            if label == "hello" { Some(Ok(())) } else { None }
                        })
                    }
                },
            )
            .run(&mut f)
            .await
            .unwrap();
    }
}
