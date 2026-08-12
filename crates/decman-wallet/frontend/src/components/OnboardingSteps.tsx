/** Where the onboarding protocol stands. The three signing steps happen inside
 *  one call to the wallet, so they advance together; authorization is separate
 *  because Canton only clears it once every host has signed. */
export type Stage = "idle" | "signing" | "authorizing" | "live";

interface Step {
  glyph: string;
  title: string;
  detail: string;
}

const STEPS: Step[] = [
  {
    glyph: "1",
    title: "Prepare the topology",
    detail: "One host turns the party's public key into an unsigned multi-host topology.",
  },
  {
    glyph: "2",
    title: "Sign the multi-hash — locally",
    detail: "The wallet signs with its own key. No host sees the private half.",
  },
  {
    glyph: "3",
    title: "Onboard on every host",
    detail: "The wallet submits the same signed bundle to each host itself. No host relays.",
  },
  {
    glyph: "4",
    title: "Await authorization",
    detail: "The topology stays a proposal until the last host has signed it.",
  },
];

function stateFor(index: number, stage: Stage): "idle" | "active" | "done" {
  const signingSteps = index < 3;
  switch (stage) {
    case "idle":
      return "idle";
    case "signing":
      return signingSteps ? "active" : "idle";
    case "authorizing":
      return signingSteps ? "done" : "active";
    case "live":
      return "done";
  }
}

export function OnboardingSteps({ stage }: { stage: Stage }) {
  return (
    <ol className="steps">
      {STEPS.map((step, index) => {
        const state = stateFor(index, stage);
        return (
          <li className={`step step-${state}`} key={step.title}>
            <span className="step-glyph">{state === "done" ? "✓" : step.glyph}</span>
            <div className="step-body">
              <strong className="t-sm" style={{ color: "var(--fg)" }}>
                {step.title}
              </strong>
              <span className="t-sm">{step.detail}</span>
            </div>
          </li>
        );
      })}
    </ol>
  );
}
