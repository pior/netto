# Design

## Dual purpose

Netto serves two distinct purposes that pull in opposite directions:

1. **Connectivity monitor** — "is my internet up?" A robust aggregate signal
   that should not flip red just because one target is slow.
2. **Service monitor** — "is the specific service I rely on working?"
   Per-target visibility for diagnosing individual services.

A single combined indicator collapses both into a less-useful one. The
answer is to display them as separate signals.

## UX

Three layers, top-down:

- **Internet** — aggregate "best-of-N" across external targets. Answers
  "is my ISP / internet up?" Only goes red when *all* external targets
  degrade together (which is what an actual outage looks like).
- **Router** — separate row for local-network health (Wi-Fi, cable). Lets
  the user distinguish "my side" from "their side."
- **Per-target rows** — each user-configured service, with its own
  sparkline and current latency.

```
🟩🟩🟩🟩🟨🟩🟩🟩🟩🟩  Internet
🟩🟩🟩🟩🟩🟩🟩🟩🟩🟩  Router 10.0.0.1: 1.2ms
─────────────────────────────────
🟩🟩🟩🟩🟩🟩🟩🟩🟩🟩  google.com: 12ms
🟩🟩🟨🟥🟥🟨🟩🟩🟩🟩  1.1.1.1: 120ms
🟩🟩🟩🟩🟩🟩🟩🟩🟩🟩  apple.com: 18ms
```

Status-bar title tracks the **Internet** aggregate (best-of) — the
question users want answered at a glance.

## Why best-of-N for "Internet" (not median, not average)

- **best-of-N**: "your connectivity is at least this good." If even one
  well-known external endpoint responds fast, the network is fine.
  Correlated failure of *all* targets — which is what an actual outage
  looks like — still degrades the signal correctly.
- **median**: a single flaky target out of three can tip the signal.
- **average**: a slow outlier drags the signal; same problem as median,
  worse.

Cost of best-of: hides per-target degradation in the aggregate. That's
fine — per-target rows below already cover that case, so the split is
clean.

## Data model

- Per-target ring buffer of `(timestamp, PingResult)` samples, bounded by
  retention window. Each target loop pushes a sample as it produces one;
  old samples are evicted by age.
- Sparkline rendered from time-bucketed slots (default: 30 × 10 s = 5
  min). Each bucket aggregates samples within that window (worst-of, so
  a single bad sample within the window shows up).
- **"Internet" sparkline computed at render time** by taking best-of
  across external-target buckets per time slot. No separate accumulator.
- Empty buckets (gaps in coverage) shown gray, not green — honest about
  what we don't know.

Variable ping cadence (10 s when menu closed, 1 s when menu open) is
handled naturally by time-bucketing — closed-menu periods produce sparser
buckets, open-menu periods denser ones.

## Color thresholds

- 🟩 green: < 50 ms
- 🟨 yellow: 50–200 ms
- 🟥 red: > 200 ms or timeout/error
- ⬜ gray: no data in bucket

Absolute thresholds first. Per-target baselines (what's "normal" for
each target) can come later if needed.
