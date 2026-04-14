<p align="center">
  <h1 align="center">Carapace</h1>
  <p align="center"><strong>The AI agent harness that learns what to verify from how your agent actually fails.</strong></p>
</p>

<p align="center">
  <a href="#tested-with-real-agents"><img alt="Claude Code" src="https://img.shields.io/badge/Claude%20Code-tested-blue"></a>
  <a href="#flywheel-experiment"><img alt="Flywheel" src="https://img.shields.io/badge/flywheel-80%25→100%25-brightgreen"></a>
  <a href="https://github.com/cheatthegod/carapace/blob/master/Cargo.toml"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
</p>

---

## What makes Carapace different

Every other guardrail system works the same way: **humans write rules, the system enforces them.** When a new failure mode appears, a human notices, writes a new rule, deploys it.

Carapace closes the loop:

```
Agent works → Carapace records every step
                          ↓
                    Failure happens
                          ↓
            Carapace analyzes: "consecutive writes
            without reading → 50% failure rate"
                          ↓
            Generates rule automatically
                          ↓
            Next session: rule prevents the same failure
```

The rules aren't static. They grow from your agent's real failure data. And they're not locked to one agent — they work across any MCP-compatible agent (Claude Code, Cursor, Goose, Aider, OpenHands).

## Flywheel experiment

Proven in [`flywheel.rs`](crates/carapace-core/tests/flywheel.rs):

```
Round 1:  5 sessions, no learned rules    →  80% step completion
Learn:    analyze traces → 3 patterns discovered → rules saved to disk
Round 2:  5 sessions, rules loaded from disk  →  100% step completion  (+20pp)

Rules discovered (not hand-written):
  "3+ consecutive writes without a read → 100% failure rate"
  "5+ steps without checkpoint → 100% had failures"
  "write actions → 33% failure rate"
```

The engine that ran Round 2 was a **new instance** that loaded rules from disk — proving persistence across restarts.

## Tested with real agents

Validated in 4 live Claude Code sessions, not simulations:

| Run | Verifier blocks | False positives | Result |
|-----|:---:|:---:|---|
| Demo 1 | 2 | 1 | Found first false positive (`.env` substring match) |
| Demo 2 | 2 | 1 | Found second false positive (`.*sh` regex too broad) |
| Demo 3 | 1 | 0 | Both fixes applied, 100% precision |
| A/B Eval | 1 | 0 | Honest result: simple tasks succeed with or without Carapace |

Each false positive was discovered from real data and fixed — exactly the iteration loop Carapace is designed for.

## Quick start

```bash
# Build
git clone https://github.com/cheatthegod/carapace.git
cd carapace && cargo build --release

# Add to any Claude Code project
cat > .mcp.json << 'EOF'
{
  "mcpServers": {
    "carapace": {
      "type": "stdio",
      "command": "target/release/carapace",
      "args": ["mcp", "serve"]
    }
  }
}
EOF

# That's it. Claude Code will now call Carapace tools automatically.
```

## How it works

Carapace exposes 7 MCP tools. The agent calls them as part of its normal workflow:

| Tool | When | What |
|------|------|------|
| `carapace_begin_session` | Start of task | Initialize session tracking |
| `carapace_verify_step` | Before each action | Check static rules + learned rules |
| `carapace_save_checkpoint` | Before risky writes | Git-backed snapshot for rollback |
| `carapace_record_step` | After each action | Record outcome for trace + learning |
| `carapace_rollback` | After failure | Restore checkpoint, try alternative |
| `carapace_session_summary` | End of task | Aggregate stats |
| `carapace_learn` | Periodically | Analyze all sessions, generate new rules, persist to disk |

The agent doesn't need special instructions. The MCP server's `instructions` field tells the agent the protocol:

> *Start with carapace_begin_session. Call carapace_verify_step before each action. If verification fails, skip and record as skipped. Call carapace_record_step after each action. Use carapace_learn to improve rules from past sessions.*

## The adaptive learning loop

```
              ┌─── verify_step ←── learned rules ←─┐
              │                                     │
Agent ────→ Carapace ────→ record_step ────→ trace DB
                                                │
                                          carapace_learn
                                                │
                                          pattern analysis
                                                │
                                      ┌─── 5 detectors ───┐
                                      │                    │
                                      │ consecutive writes │
                                      │ repeated edits     │
                                      │ missing tests      │
                                      │ no checkpoints     │
                                      │ high-fail actions  │
                                      └────────┬───────────┘
                                               │
                                       learned_rules.json
                                               │
                                    auto-loaded on next startup
```

Rules persist to `learned_rules.json` and are loaded automatically when the MCP server starts. The system gets better over time without human intervention.

## Architecture

```
carapace/
  crates/
    carapace-core/         # Engine, verifier, checkpoint, tracer, learner
      verifier/            # Static rules + threat patterns + consistency + learned rules
      checkpoint/          # Git-backed snapshots + Saga rollback
      tracer/              # SQLite traces + anomaly detection + export
      learner/             # Pattern discovery + rule generation + persistence
      engine/              # Orchestrates everything, holds learned state
    carapace-mcp/          # MCP server (7 tools, rmcp 0.1.5)
    carapace-cli/          # Binary: verify, wrap, learn, summary, trace, mcp
```

Single Rust binary. SQLite for traces. Git for checkpoints. No external services.

## When Carapace helps (and when it doesn't)

**Helps:**
- Tasks with 10+ steps where error compounding matters
- Irreversible operations (deletes, deployments, migrations)
- Teams switching between multiple agents (learned rules are agent-agnostic)
- Projects where the same failure patterns recur across sessions

**Doesn't help:**
- Simple 3-4 step tasks (baseline already succeeds, Carapace is overhead)
- Single-shot code generation (no multi-step execution to verify)

We tested this honestly: our A/B evaluation showed both groups succeed on a simple security-fix task. Carapace's value is in the tail — longer tasks, repeat failures, irreversible damage.

## Development

```bash
cargo build            # Build
cargo test             # 40 tests (34 unit + 3 integration + 3 MCP)
cargo run -- learn     # Analyze past sessions
cargo run -- verify execute "rm -rf /"   # Test verification
```

## Related work

- [Agent Harness for LLM Agents: A Survey](https://www.preprints.org/manuscript/202604.0428/v1) — formalizes the harness as the binding constraint
- [Towards a Science of AI Agent Reliability](https://arxiv.org/abs/2602.16666) — 12 reliability metrics
- [Galileo Agent Control](https://galileo.ai/) — eval-to-guardrail at enterprise scale (SaaS)
- [Agent Reliability Engineering](https://github.com/choutos/agent-reliability-engineering) — SRE principles for agents

## License

Apache-2.0
