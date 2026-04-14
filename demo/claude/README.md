# Claude Code Demo

This folder contains a reproducible Claude Code demo for Carapace's current MCP path.

What it shows:
- Claude Code connects to Carapace through the project-scoped `.mcp.json`.
- Claude starts a Carapace session, verifies each step, saves checkpoints for risky edits, records step outcomes, and ends with a session summary.
- A risky delete against `demo/workspace/secrets/secret.env` is expected to be blocked, after which Claude should switch to a safe alternative.

## Prerequisites

- `claude` CLI installed and authenticated
- a built Carapace binary at `target/debug/carapace`

If the binary is missing or stale, rebuild in the repo root:

```bash
cargo build -p carapace-cli
```

## Files

- `.mcp.json`: project-scoped Claude MCP configuration
- `demo/claude/start_mcp.sh`: launches Carapace with a writable SQLite path inside `.carapace/`
- `demo/claude/run_demo.sh`: starts Claude with the demo prompt and MCP config
- `demo/claude/check_shim.py`: verifies the Claude-facing transport shim locally
- `demo/claude/system_prompt.txt`: forces the begin/verify/record/summary protocol
- `demo/claude/task_prompt.txt`: the bounded demo task
- `demo/workspace/`: fixture workspace for the live demo

## Run

From the repo root:

```bash
bash demo/claude/run_demo.sh
```

To verify the Claude-facing MCP transport without starting a Claude session:

```bash
python3 demo/claude/check_shim.py
```

Expected behavior:
- Claude proposes deleting `secrets/secret.env`
- Carapace blocks the delete
- Claude saves checkpoints before risky edits and can roll back if a checkpointed step fails
- Claude updates `config/example.env`, `README.md`, and `src/app.py` instead
- Claude ends with a Carapace session summary

Note:
- In locked-down sandboxes, `claude mcp get carapace` may still report a timeout even when the shim itself is healthy. `check_shim.py` is the reliable local preflight for this repo.
