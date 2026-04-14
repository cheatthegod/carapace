#!/usr/bin/env python3
import json
import re
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = ROOT / "demo" / "workspace"
SHIM = ROOT / "demo" / "claude" / "claude_stdio_shim.py"


def send(proc: subprocess.Popen[bytes], message: dict) -> None:
    body = json.dumps(message).encode("utf-8")
    proc.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode("utf-8"))
    proc.stdin.write(body)
    proc.stdin.flush()


def recv_one(proc: subprocess.Popen[bytes], timeout: float = 3.0) -> dict:
    start = time.time()
    header = b""

    while b"\r\n\r\n" not in header:
        if time.time() - start > timeout:
            raise TimeoutError("timed out waiting for framed response header")
        chunk = proc.stdout.read(1)
        if not chunk:
            raise EOFError("shim stdout closed unexpectedly")
        header += chunk

    match = re.search(br"Content-Length: (\d+)", header, re.IGNORECASE)
    if not match:
        raise RuntimeError(f"invalid framed header: {header!r}")

    body = proc.stdout.read(int(match.group(1)))
    return json.loads(body.decode("utf-8"))


def main() -> int:
    proc = subprocess.Popen(
        ["python3", str(SHIM)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        send(
            proc,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "carapace-demo-check", "version": "0.1.0"},
                },
            },
        )
        init = recv_one(proc)

        send(proc, {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
        send(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        tools = recv_one(proc)

        send(
            proc,
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "carapace_begin_session",
                    "arguments": {
                        "session_id": "demo-session",
                        "agent_name": "carapace-demo-check",
                        "working_dir": str(WORKSPACE),
                    },
                },
            },
        )
        begin = recv_one(proc)

        send(
            proc,
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "carapace_verify_step",
                    "arguments": {
                        "session_id": "demo-session",
                        "action_type": "delete",
                        "tool_name": "remove_file",
                        "description": "Delete demo secret env",
                        "target_files": [str(WORKSPACE / "secrets" / "secret.env")],
                        "arguments": {"path": str(WORKSPACE / "secrets" / "secret.env")},
                    },
                },
            },
        )
        verify = recv_one(proc)
    finally:
        proc.kill()

    tool_names = [tool["name"] for tool in tools["result"]["tools"]]
    verify_text = verify["result"]["content"][0]["text"]

    expected = {
        "carapace_begin_session",
        "carapace_verify_step",
        "carapace_save_checkpoint",
        "carapace_record_step",
        "carapace_rollback",
        "carapace_session_summary",
    }
    missing = expected.difference(tool_names)
    if missing:
        print(f"Missing tools from shim handshake: {sorted(missing)}", file=sys.stderr)
        return 1

    if "blocked path" not in verify_text.lower():
        print("Expected blocked-path verification failure was not observed", file=sys.stderr)
        return 1

    print("Carapace Claude shim preflight passed.")
    print(f"Protocol: {init['result']['protocolVersion']}")
    print(f"Tools: {', '.join(tool_names)}")
    print(begin["result"]["content"][0]["text"])
    print(verify_text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
