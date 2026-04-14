<p align="center">
  <h1 align="center">Carapace</h1>
  <p align="center"><strong>Reliable execution infrastructure for AI agents.</strong></p>
  <p align="center">Step verification &middot; Saga rollback &middot; Execution tracing</p>
</p>

<p align="center">
  <a href="https://github.com/cheatthegod/carapace/actions"><img alt="CI" src="https://img.shields.io/badge/tests-17%2F17%20passing-brightgreen"></a>
  <a href="https://github.com/cheatthegod/carapace/blob/master/Cargo.toml"><img alt="Rust" src="https://img.shields.io/badge/rust-2024%20edition-orange"></a>
  <a href="https://github.com/cheatthegod/carapace/blob/master/Cargo.toml"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
</p>

---

## The Problem

AI agents are unreliable on multi-step tasks. Not because the model is dumb, but because errors compound:

| Per-step accuracy | 5 steps | 10 steps | 20 steps |
|:-:|:-:|:-:|:-:|
| 95% | 77% | 60% | **36%** |
| 99% | 95% | 90% | **82%** |

Raising per-step accuracy from 95% to 99% through verification more than doubles the 20-step success rate (36% to 82%). That is what Carapace does.

**Evidence:** Pi Research changed only the execution harness (model untouched) and SWE-bench jumped from 6.7% to 68.3%. The bottleneck is not the brain, it is the skeleton. ([Meng et al., "Agent Harness for LLM Agents: A Survey", 2026](https://www.preprints.org/manuscript/202604.0428/v1))

## What Carapace Does

Carapace is a lightweight middleware that wraps around **any** AI coding agent and provides three guarantees:

### 1. Every step is verified

Before an action takes effect, Carapace checks it against configurable rules, threat patterns, and consistency constraints.

```
$ carapace verify execute "rm -rf /"
Verification: fail
- Blocked command pattern detected: rm -rf /

$ carapace verify read "Read source file" --file src/main.rs
Verification: pass

$ carapace verify execute "git status"
Verification: warn
- Action type 'execute' requires confirmation
```

**Verification layers** (cost-ascending, applied adaptively):

| Layer | Latency | What it catches |
|---|---|---|
| Rule checks | <1ms | Blocked commands, sensitive paths, file count limits |
| Threat patterns | <1ms | Reverse shells, credential exfiltration, crypto miners, fork bombs |
| Consistency checks | <1ms | Contradictory actions, loop traps, plan deviation |
| Confirmation gate | human | High-risk operations (delete, execute) |

### 2. Failures roll back, not restart

When a step fails, Carapace does not throw away all previous work. It rolls back to the nearest checkpoint and tries an alternative path.

```
Traditional agent:
  Step 1 -> Step 2 -> Step 3 -> FAIL -> "Start over"  (all tokens wasted)

Carapace agent:
  Step 1 -> save -> Step 2 -> save -> Step 3 -> FAIL
                                                  |
                                        roll back to Step 2
                                                  |
                                        Step 3' (alternative) -> OK
```

This is the **Saga pattern** from distributed systems: each step registers a forward action and a compensating action. On failure, compensations execute in reverse order.

- **Git-based checkpoints**: file changes are stashed/committed automatically before risky steps
- **Configurable depth**: keep the last N checkpoints, prune older ones
- **Smart triggers**: only checkpoint on writes, deletes, and executions (reads are free)

### 3. Every decision is traceable

Full audit trail of what the agent did, why, and at what cost.

```
Step 1 | READ  auth/config.ts         | pass   | $0.02 | 2,340 tok
Step 2 | EDIT  auth/middleware.ts      | pass   | $0.12 | 8,721 tok
Step 3 | EDIT  auth/session.ts         | warn   | $0.08 | 4,102 tok
       |       "hardcoded secret"      | -> user notified, fixed
Step 4 | EDIT  auth/refresh.ts         | fail   | $0.00 | 0 tok
       |       test failure: race cond | -> rolled back to Step 3
Step 4'| EDIT  auth/refresh.ts (v2)    | pass   | $0.08 | 6,210 tok

Total: 5 steps (1 rollback recovery) | $0.30 | 21,373 tok
```

**Anomaly detection** runs continuously:
- **Token spike**: current step uses 3x+ the rolling average
- **Loop trap**: same action repeated 3+ times in a window
- **Goal drift**: action pattern shifts significantly from initial behavior

---

## Architecture

```
                    Carapace
  +-----------------------------------------+
  |  Verifier    Checkpoint     Tracer      |
  |  (rules,     (git stash,   (SQLite,    |
  |   patterns,   saga txn,     anomaly,   |
  |   consistency) rollback)    export)    |
  +------|-------------|------------|-------+
         |             |            |
         +------+------+------+----+
                |
         ExecutionEngine
         (orchestrates all three)
                |
    +-----------+-----------+
    |           |           |
 MCP Server  CLI Wrapper  HTTP Proxy
    |           |         (planned)
    v           v
 Any MCP     Any CLI
 agent       agent
```

### Crate structure

| Crate | Purpose |
|---|---|
| `carapace-core` | Types, config, verifier, checkpoint, tracer, engine |
| `carapace-mcp` | MCP server integration (tool manifest, stdio transport) |
| `carapace-cli` | Binary with `init`, `wrap`, `verify`, `summary`, `trace`, `mcp` subcommands |
| `carapace-test` | Test harness utilities |

---

## Quick Start

### Install

```bash
# From source
git clone https://github.com/cheatthegod/carapace.git
cd carapace
cargo build --release

# The binary is at target/release/carapace
```

### Zero-config usage

```bash
# Verify a proposed action
carapace verify write "Update auth module" --file src/auth.rs

# Wrap an agent command with verification + tracing
carapace wrap -- claude-code --task "refactor auth"

# Generate a default config file
carapace init
```

### View results

```bash
# Print session summary
carapace summary <session-id>

# Export full trace
carapace trace <session-id> --format json
carapace trace <session-id> --format csv --output trace.csv
```

### MCP mode

```bash
# Print the MCP tool manifest
carapace mcp manifest

# Start the MCP stdio server
carapace mcp serve
```

---

## Configuration

Carapace works with zero configuration. To customize, run `carapace init` and edit `~/.config/carapace/config.yaml`:

```yaml
verification:
  enabled: true
  rules_enabled: true
  consistency_enabled: true

  # Commands that are always blocked (supports regex)
  blocked_commands:
    - "rm -rf /"
    - "mkfs"
    - "curl.*|.*sh"

  # Paths the agent must never touch
  blocked_paths:
    - "/etc/shadow"
    - "~/.ssh"
    - ".env"

  # Max files an agent can modify in one step
  max_files_per_step: 20

  # Action types that require human confirmation
  require_confirmation_for:
    - delete
    - execute

checkpoint:
  enabled: true
  strategy: git          # git | file_copy | none
  auto_save: true
  max_rollback_depth: 10
  auto_save_on:          # only checkpoint these action types
    - write
    - delete
    - execute

trace:
  enabled: true
  retention_days: 30
  detect_anomalies: true
  token_spike_threshold: 3.0   # flag if >3x rolling average
  loop_detection_window: 5     # check last 5 steps for repeats

cost:
  track_tokens: true
  daily_limit_usd: null        # set to e.g. 20.0 for a hard cap
  monthly_limit_usd: null
  alert_threshold: 0.8         # alert at 80% of limit
```

---

## How It Works

### Verification flow

```
Agent proposes an action
        |
        v
  +-- Rule checks --------+
  |  blocked commands?     |  <1ms
  |  blocked paths?        |
  |  too many files?       |
  |  threat patterns?      |
  +-----|------------------+
        |
  +-- Consistency checks --+
  |  contradicts prev step?|  <1ms
  |  loop trap detected?   |
  |  deviates from plan?   |
  +-----|------------------+
        |
  +-- Confirmation gate ---+
  |  high-risk action type?|  human latency
  +-----|------------------+
        v
  pass / warn / fail
```

### Threat patterns (built-in)

| Pattern | Risk | Example |
|---|---|---|
| Reverse shell | Critical | `bash -i >& /dev/tcp/...` |
| Credential exfiltration | Critical | `curl ... $API_KEY` |
| Crypto miner | Critical | `xmrig`, `stratum+tcp://` |
| Privilege escalation | High | `sudo chmod 4777 /` |
| Data exfiltration | Critical | `cat /etc/shadow \| nc ...` |
| Code injection | High | `eval(base64decode(...))` |
| Fork bomb | Critical | `:(){ :\|:& };:` |
| Disk destruction | Critical | `dd if=/dev/zero` |
| SSH key theft | Critical | `cp ~/.ssh/id_rsa ...` |
| Environment dump | High | `printenv > /tmp/...` |

### Saga rollback

Each step that modifies state registers a compensating action:

```
Saga history:
  Step 1: edit config.ts    -> compensate: git stash apply stash@{2}
  Step 2: edit middleware.ts -> compensate: git stash apply stash@{1}
  Step 3: edit session.ts   -> compensate: git stash apply stash@{0}
  Step 4: edit refresh.ts   -> FAILED (test failure)
                                |
                                v
                         Execute compensations in reverse:
                           undo Step 4 (nothing saved)
                           undo Step 3 (restore stash@{0})
                         Agent retries Step 3 with alternative approach
```

---

## Integration Modes

| Mode | How | Best for |
|---|---|---|
| **CLI `verify`** | `carapace verify <type> <desc>` | Quick one-off checks from scripts |
| **CLI `wrap`** | `carapace wrap -- <agent command>` | Wrapping any CLI agent with tracing |
| **MCP Server** | `carapace mcp serve` (stdio) | Agents that support MCP tool calls |
| **Library** | `use carapace_core::ExecutionEngine` | Embedding in Rust agent code |

### Supported agents

Carapace is agent-agnostic. It works with any agent that runs in a terminal or speaks MCP:

- Claude Code
- Aider
- Hermes Agent
- OpenClaw
- Goose
- OpenHands
- Codex
- Any MCP-compatible agent

---

## Project Structure

```
carapace/
  Cargo.toml                          # Workspace root
  crates/
    carapace-core/
      src/
        types.rs                      # Shared types (StepAction, TraceEntry, etc.)
        config/
          schema.rs                   # Config structs with serde
          mod.rs                      # YAML loading, XDG paths
        verifier/
          rules.rs                    # Rule engine (commands, paths, files, threats)
          patterns.rs                 # 10 compiled threat patterns
          consistency.rs              # Contradiction / loop / drift detection
          mod.rs                      # CompositeVerifier
        checkpoint/
          git.rs                      # Git stash/commit checkpoints
          saga.rs                     # Saga coordinator (forward + compensate)
          mod.rs                      # CheckpointManager
        tracer/
          store.rs                    # SQLite WAL storage
          anomaly.rs                  # Token spike / loop / drift detection
          export.rs                   # JSON and CSV export
          mod.rs                      # Tracer
        engine/
          mod.rs                      # ExecutionEngine (verify -> checkpoint -> trace)
        storage/
          sqlite.rs                   # Connection pool, migrations, CRUD
          migrations/
            001_initial.sql           # Schema: sessions, steps, checkpoints, anomalies
    carapace-mcp/
      src/lib.rs                      # MCP server with tool manifest
    carapace-cli/
      src/main.rs                     # CLI: init, wrap, verify, summary, trace, mcp
    carapace-test/
      src/lib.rs                      # Test harness utilities
```

---

## Development

### Prerequisites

- Rust stable (2024 edition)
- Git (for checkpoint backend)
- SQLite (bundled via sqlx)

### Build and test

```bash
cargo build
cargo test

# Run with tracing output
RUST_LOG=debug cargo run -- verify execute "test command"
```

### Running tests

```
$ cargo test

running 17 tests
test checkpoint::git::tests::detect_git_repo ... ok
test checkpoint::git::tests::save_and_restore_checkpoint ... ok
test checkpoint::saga::tests::max_depth_pruning ... ok
test checkpoint::saga::tests::partial_rollback ... ok
test checkpoint::saga::tests::rollback_executes_in_reverse ... ok
test checkpoint::saga::tests::rollback_to_checkpoint ... ok
test storage::sqlite::tests::empty_summary_defaults_to_zero ... ok
test storage::sqlite::tests::previous_summaries_preserve_step_order ... ok
test verifier::patterns::tests::detects_credential_exfil ... ok
test verifier::patterns::tests::detects_reverse_shell ... ok
test verifier::patterns::tests::ignores_safe_commands ... ok
test verifier::rules::tests::allows_safe_action ... ok
test verifier::rules::tests::blocks_dangerous_path ... ok
test verifier::rules::tests::blocks_home_shorthand_sensitive_paths ... ok
test verifier::rules::tests::blocks_regex_style_command_patterns ... ok
test verifier::rules::tests::blocks_too_many_files ... ok
test verifier::rules::tests::warns_when_confirmation_is_required ... ok

test result: ok. 17 passed; 0 failed; 0 ignored
```

---

## Roadmap

### Phase 1 (current) -- Verifiable

- [x] Rule-based verification engine (commands, paths, file limits)
- [x] Threat pattern matching (10 built-in patterns)
- [x] Consistency checking (contradictions, loops, plan drift)
- [x] Git-based checkpoints (stash/commit)
- [x] Saga transaction rollback
- [x] SQLite trace storage with anomaly detection
- [x] CLI with verify / wrap / summary / trace / mcp
- [x] MCP server tool manifest

### Phase 2 -- Recoverable

- [ ] Lightweight model review (use cheap model to verify expensive model output)
- [ ] Auto-test execution after code changes
- [ ] Alternative path selection on rollback (failure analysis -> retry strategy)
- [ ] Full MCP protocol handling (JSON-RPC over stdio)
- [ ] Cost tracking with budget enforcement and auto-downgrade

### Phase 3 -- Observable

- [ ] Web dashboard (session viewer, step timeline, cost charts)
- [ ] Trace replay (re-walk any failure path)
- [ ] CI/CD integration (GitHub Actions)
- [ ] Verification strategy marketplace (community rule sets)
- [ ] Multi-agent saga coordination

---

## Why "Carapace"?

A carapace is the hard protective shell of a crustacean. It does not replace the animal inside -- it protects it. Carapace does the same for AI agents: it wraps around any agent and shields the execution from compounding errors, unsafe actions, and silent failures.

---

## Related Work

- [Agent Harness for LLM Agents: A Survey](https://www.preprints.org/manuscript/202604.0428/v1) -- the paper that formalized the harness as the binding constraint
- [Towards a Science of AI Agent Reliability](https://arxiv.org/abs/2602.16666) -- 12 metrics across 4 reliability dimensions
- [SWE-bench](https://www.swebench.com/) / [Terminal-Bench](https://www.tbench.ai/) -- benchmarks for measuring agent task completion
- [Microsoft Agent Governance Toolkit](https://github.com/microsoft/agent-governance-toolkit) -- enterprise-grade agent policy enforcement

---

## License

Apache-2.0
