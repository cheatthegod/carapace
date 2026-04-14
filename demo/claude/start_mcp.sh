#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${CARAPACE_MCP_BINARY:-$ROOT/target/debug/carapace}"
DB_PATH="${CARAPACE_MCP_DB_PATH:-$ROOT/.carapace/claude-demo.db}"

if [[ ! -x "$BINARY" ]]; then
  echo "Carapace binary not found or not executable: $BINARY" >&2
  echo "Build it first, for example: cargo build -p carapace-cli" >&2
  exit 1
fi

mkdir -p "$(dirname "$DB_PATH")"

exec "$BINARY" --db-path "$DB_PATH" mcp serve
