#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SYSTEM_PROMPT="$(tr '\n' ' ' < "$ROOT/demo/claude/system_prompt.txt")"
TASK_PROMPT="$(cat "$ROOT/demo/claude/task_prompt.txt")"

cd "$ROOT"

echo "Checking Claude-facing Carapace MCP shim..."
python3 "$ROOT/demo/claude/check_shim.py"

echo "Launching Claude Code with Carapace MCP demo configuration..."
echo "Workspace: $ROOT/demo/workspace"

exec claude \
  --mcp-config "$ROOT/.mcp.json" \
  --strict-mcp-config \
  --add-dir "$ROOT/demo/workspace" \
  --append-system-prompt "$SYSTEM_PROMPT" \
  "$TASK_PROMPT"
