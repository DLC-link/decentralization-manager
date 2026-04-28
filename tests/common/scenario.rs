use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

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
    ThenEventually,
}

impl StepKind {
    pub fn label(self) -> &'static str {
        match self {
            StepKind::Given => "GIVEN",
            StepKind::When => "WHEN ",
            StepKind::Then => "THEN ",
            StepKind::ThenEventually => "THEN_EVENTUALLY",
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
                    name,
                    deadline,
                    probe,
                } => {
                    info!(
                        step_kind = ?StepKind::ThenEventually,
                        step_name = %name,
                        "  THEN  eventually {}",
                        name
                    );
                    let step_start = Instant::now();
                    let outcome: Result<()> = loop {
                        match probe(f, &mut self.ctx).await {
                            Some(Ok(())) => break Ok(()),
                            Some(Err(e)) => break Err(e),
                            None => {
                                if step_start.elapsed() >= *deadline {
                                    break Err(anyhow::anyhow!(
                                        "polling timed out after {:?}",
                                        *deadline
                                    ));
                                }
                                tokio::time::sleep(POLL_INTERVAL).await;
                            }
                        }
                    };
                    let took = step_start.elapsed();
                    match outcome {
                        Ok(()) => info!("    ✓ (took {:.1}s)", took.as_secs_f64()),
                        Err(e) => {
                            error!(
                                scenario = %self.name,
                                "Scenario \"{}\" failed at THEN_EVENTUALLY \"{}\"",
                                self.name,
                                name
                            );
                            return Err(e
                                .context(format!(
                                    "THEN eventually \"{}\" timed out after {:?}",
                                    name, *deadline
                                ))
                                .context(format!("scenario \"{}\" failed", self.name)));
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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn steps_execute_in_order() {
        let mut f = Fixture::for_test();
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
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
        let counter = std::sync::Arc::new(AtomicU32::new(0));
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

        assert!(started.elapsed() >= Duration::from_millis(100));
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
