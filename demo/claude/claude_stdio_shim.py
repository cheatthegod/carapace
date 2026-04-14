#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import threading
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
START_SCRIPT = ROOT / "demo" / "claude" / "start_mcp.sh"
LOG_PATH = os.environ.get("CARAPACE_MCP_SHIM_LOG")


def log_line(direction: str, message: str) -> None:
    if not LOG_PATH:
        return

    log_path = Path(LOG_PATH)
    log_path.parent.mkdir(parents=True, exist_ok=True)

    with open(log_path, "a", encoding="utf-8") as handle:
        handle.write(f"{direction}: {message}\n")


def read_framed_message(stream) -> str | None:
    headers: dict[str, str] = {}

    while True:
        line = stream.readline()
        if not line:
            return None
        if line in (b"\n", b"\r\n"):
            break

        name, _, value = line.decode("utf-8").partition(":")
        headers[name.strip().lower()] = value.strip()

    content_length = headers.get("content-length")
    if content_length is None:
        raise ValueError("missing Content-Length header")

    body = stream.read(int(content_length))
    if not body:
        return None
    return body.decode("utf-8")


def write_framed_message(stream, message: str) -> None:
    payload = message.encode("utf-8")
    stream.write(f"Content-Length: {len(payload)}\r\n\r\n".encode("utf-8"))
    stream.write(payload)
    stream.flush()


def pump_parent_to_child(child: subprocess.Popen[str]) -> None:
    try:
        while True:
            message = read_framed_message(sys.stdin.buffer)
            if message is None:
                break
            log_line("IN", message)
            child.stdin.write(message)
            child.stdin.write("\n")
            child.stdin.flush()
    finally:
        if child.stdin:
            child.stdin.close()


def pump_child_to_parent(child: subprocess.Popen[str]) -> None:
    for raw_line in child.stdout:
        line = raw_line.strip()
        if not line:
            continue

        if not line.startswith("{"):
            print(line, file=sys.stderr, flush=True)
            continue

        try:
            json.loads(line)
        except json.JSONDecodeError:
            print(line, file=sys.stderr, flush=True)
            continue

        log_line("OUT", line)
        write_framed_message(sys.stdout.buffer, line)


def pump_stderr(child: subprocess.Popen[str]) -> None:
    for line in child.stderr:
        sys.stderr.write(line)
        sys.stderr.flush()


def main() -> int:
    env = os.environ.copy()
    env.setdefault("RUST_LOG", "error")

    child = subprocess.Popen(
        ["bash", str(START_SCRIPT)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )

    upstream = threading.Thread(target=pump_parent_to_child, args=(child,), daemon=True)
    downstream = threading.Thread(target=pump_child_to_parent, args=(child,), daemon=True)
    stderr_thread = threading.Thread(target=pump_stderr, args=(child,), daemon=True)

    upstream.start()
    downstream.start()
    stderr_thread.start()

    return child.wait()


if __name__ == "__main__":
    raise SystemExit(main())
