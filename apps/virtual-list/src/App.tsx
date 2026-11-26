// SPDX-License-Identifier: MIT
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef } from "react";

const ROW_COUNT = 10_000;
const ROW_HEIGHT = 40;
const CATEGORIES = ["Canvas", "Layout", "Style", "Text", "GPU", "Events"];

interface FrameTimelineProbe {
  start(): void;
  stop(): FrameTimelineSummary;
  summary(): FrameTimelineSummary;
}

interface FrameTimelineSummary {
  frameCount: number;
  budgetMs: number;
  p50Ms: number;
  p95Ms: number;
  maxMs: number;
  onBudgetRatio: number;
}

declare global {
  interface Window {
    __squarkFrameTimeline?: FrameTimelineProbe;
  }
}

function summarizeDeltas(deltas: number[]): FrameTimelineSummary {
  const budgetMs = 1000 / 60;
  if (deltas.length === 0) {
    return { frameCount: 0, budgetMs, p50Ms: 0, p95Ms: 0, maxMs: 0, onBudgetRatio: 1 };
  }
  const sorted = [...deltas].sort((a, b) => a - b);
  const p50Ms = sorted[Math.floor(sorted.length * 0.5)];
  const p95Ms = sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))];
  const maxMs = sorted[sorted.length - 1];
  const onBudget = deltas.filter((d) => d <= budgetMs * 1.1).length;
  return {
    frameCount: deltas.length,
    budgetMs: Math.round(budgetMs * 100) / 100,
    p50Ms: Math.round(p50Ms * 100) / 100,
    p95Ms: Math.round(p95Ms * 100) / 100,
    maxMs: Math.round(maxMs * 100) / 100,
    onBudgetRatio: Math.round((onBudget / deltas.length) * 1000) / 1000,
  };
}

export function App() {
  const scrollRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let rafId: number | null = null;
    let lastTs: number | null = null;
    let deltas: number[] = [];

    const tick = (ts: number) => {
      if (lastTs !== null) {
        deltas.push(ts - lastTs);
      }
      lastTs = ts;
      rafId = requestAnimationFrame(tick);
    };

    window.__squarkFrameTimeline = {
      start() {
        if (rafId !== null) cancelAnimationFrame(rafId);
        deltas = [];
        lastTs = null;
        rafId = requestAnimationFrame(tick);
      },
      stop() {
        if (rafId !== null) {
          cancelAnimationFrame(rafId);
          rafId = null;
        }
        return summarizeDeltas(deltas);
      },
      summary() {
        return summarizeDeltas(deltas);
      },
    };

    return () => {
      if (rafId !== null) cancelAnimationFrame(rafId);
      delete window.__squarkFrameTimeline;
    };
  }, []);

  const virtualizer = useVirtualizer({
    count: ROW_COUNT,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 6,
  });

  const items = virtualizer.getVirtualItems();
  const firstVisible = items.length > 0 ? items[0].index : 0;

  return (
    <main className="shell">
      <header className="header">
        <h1 className="title">TanStack Virtual 10k Rows</h1>
        <div className="stats">
          <span className="badge" data-testid="row-count">
            {ROW_COUNT.toLocaleString("en-US")} rows
          </span>
          <span className="badge" data-testid="rendered-dom-count">
            {items.length} mounted
          </span>
          <span className="badge" data-testid="first-visible-row">
            top #{firstVisible}
          </span>
        </div>
      </header>

      <div ref={scrollRef} data-testid="scroll-container" className="scroller">
        <div
          style={{
            height: `${virtualizer.getTotalSize()}px`,
            width: "100%",
            position: "relative",
          }}
        >
          {items.map((row) => (
            <div
              key={row.key}
              data-testid="virtual-row"
              data-index={row.index}
              className={`row ${row.index % 2 === 0 ? "even" : "odd"}`}
              style={{
                transform: `translateY(${row.start}px)`,
              }}
            >
              <span className="row-id">#{row.index}</span>
              <span className="row-label">Scene element record {row.index}</span>
              <span className="row-tag">{CATEGORIES[row.index % CATEGORIES.length]}</span>
            </div>
          ))}
        </div>
      </div>
    </main>
  );
}
