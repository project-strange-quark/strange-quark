// SPDX-License-Identifier: MIT
import { useState } from "react";

const STEPS = [1, 5, 10] as const;

export function App() {
  const [count, setCount] = useState(0);
  const [step, setStep] = useState<number>(1);
  const [history, setHistory] = useState<string[]>([]);

  function applyDelta(delta: number) {
    setCount((prev) => {
      const next = prev + delta;
      const sign = delta >= 0 ? `+${delta}` : `${delta}`;
      setHistory((entries) => [...entries, `${sign} → ${next}`]);
      return next;
    });
  }

  function reset() {
    setCount(0);
    setHistory([]);
  }

  return (
    <main className="shell">
      <section className="card">
        <h1 className="title">Counter</h1>
        <div className="readout" data-testid="count">
          {count}
        </div>
        <div className="controls">
          <button
            type="button"
            className="btn secondary"
            data-testid="dec"
            onClick={() => applyDelta(-step)}
          >
            -{step}
          </button>
          <button
            type="button"
            className="btn primary"
            data-testid="inc"
            onClick={() => applyDelta(step)}
          >
            +{step}
          </button>
          <button
            type="button"
            className="btn ghost"
            data-testid="reset"
            onClick={reset}
          >
            Reset
          </button>
        </div>
        <div className="steps" role="group" aria-label="Step size">
          <span className="steps-label">Step:</span>
          {STEPS.map((value) => (
            <button
              key={value}
              type="button"
              className={step === value ? "step-btn active" : "step-btn"}
              data-testid={`step-${value}`}
              aria-pressed={step === value}
              onClick={() => setStep(value)}
            >
              {value}
            </button>
          ))}
        </div>
        <div className="history-wrap">
          <div className="history-title">History ({history.length})</div>
          <ul className="history" data-testid="history">
            {history.map((entry, idx) => (
              <li key={idx} className="history-item" data-testid="history-item">
                {entry}
              </li>
            ))}
          </ul>
        </div>
      </section>
    </main>
  );
}
