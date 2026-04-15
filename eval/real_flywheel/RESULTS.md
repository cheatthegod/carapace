# Real Flywheel Experiment Results

> **Date**: 2026-04-14 to 2026-04-15  
> **Agent**: Claude Code v2.1.101 (Opus 4.6)  
> **Sessions**: 6 real Claude Code sessions  
> **Total API cost**: ~$1.73

## Data collected

| Session | Task | Steps recorded | Verifier blocks | Failures |
|---------|------|:-:|:-:|:-:|
| 1 | Refactor config.py | 1 | 0 | 0 |
| 2 | Add logging to api.py | 1 | 0 | 0 |
| 3 | Fix bugs + run tests | 4 | 1 | 0 |
| 4 | Security headers | 1 | 0 | 0 |
| 5 | Fix data pipeline | 8 | 2 | 0 |
| 6 | Multi-file refactor | 5 | 1 | 0 |
| **Total** | | **20** | **4** | **0** |

## What the learner discovered

```
Patterns discovered:
  [16% confidence] frequently_blocked_actions:
    4/20 steps blocked by verifier (20%) — agent frequently attempts risky actions

Learned rule (at min_confidence=0.1):
  learned_frequently_blocked_actions:
    Suggest checkpoint before write/delete/execute actions
```

## Key findings

### 1. Claude (Opus 4.6) doesn't fail on these tasks

Zero execution failures across 6 sessions, 20 steps. The model is strong enough to complete all tasks correctly. This means **the learner's failure-pattern detectors don't fire** — there's no failure data to learn from.

This is an honest and important finding: on single-developer coding tasks with a frontier model, step-level failure is rare. The error compounding thesis (95% per step → 60% for 10 steps) applies more to longer autonomous workflows and weaker models.

### 2. The learner CAN learn from verifier blocks (near-misses)

Even without failures, the new `frequently_blocked_actions` detector found a real pattern: 20% of steps triggered a verifier warning or block. This means Claude regularly attempts actions that static rules consider risky — and the system correctly catches them.

### 3. Step recording density depends on prompt strength

| System prompt | Avg steps/session |
|---|---|
| Basic protocol instruction | 1.3 steps |
| Strengthened "MUST record EVERY action" | 6.5 steps |

The first 4 sessions averaged 1.3 steps because Claude batched multiple actions into one `record_step` call. After strengthening the system prompt to emphasize per-action recording, session 5-6 averaged 6.5 steps.

### 4. The confidence threshold matters

The discovered pattern has 16% confidence — below the default 30% threshold. With more data (more sessions, more steps), confidence would increase as the pattern is confirmed repeatedly. This is the flywheel working as designed: more usage → more data → higher confidence → rules activate.

## What this means for the adaptive learning story

The flywheel IS working — it discovered a real pattern from real data. But the current detector suite was designed for failure-heavy scenarios that don't occur with frontier models on simple tasks. To make the learning more impactful in practice:

1. **Broaden "signal" beyond hard failures** — verifier blocks, high token usage, repeated edits, and session duration are all learnable signals even when the agent never outright fails.
2. **Need more sessions** — 6 sessions with 20 steps is below the statistical threshold for high-confidence rules. Real-world usage over days/weeks would accumulate enough data.
3. **Harder tasks** — multi-repo refactors, migration workflows, CI/CD pipelines — these are where frontier models actually fail and where learned rules would have the highest value.

## Haiku Model Experiment (2026-04-15)

Ran the same 5 tasks with `--model haiku` (Claude Haiku 4.5).

| Metric | Opus (6 sessions) | Haiku (5 sessions) |
|--------|:-:|:-:|
| Total steps | 20 | 11 |
| Verifier blocks | 4 (20%) | 3 (27%) |
| Failures | 0 | 0 |
| Avg cost/task | $0.26 | $0.09 |

**Haiku also produces zero failures.** The tasks are too short (2-4 actual steps each) for error compounding to manifest. Haiku does trigger more verifier blocks (27% vs 20%) — it more frequently attempts operations that static rules consider risky.

### Honest conclusion

The "weaker model = more failures" hypothesis didn't hold on these tasks. Both Opus and Haiku complete 3-4 step coding tasks without errors. To genuinely test the error compounding thesis, we need:

- **30+ step autonomous workflows** (not 3-step file edits)
- **Multi-dependency tasks** where step N depends on step N-3 being correct
- **Tasks with genuine ambiguity** where the model might take a wrong path

These are hard to construct in a `claude -p` one-shot. They represent real production agent workflows — CI/CD pipelines, multi-service deployments, large codebase refactors — which is exactly where Carapace's value proposition lives.
