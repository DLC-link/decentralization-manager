use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;

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
}
