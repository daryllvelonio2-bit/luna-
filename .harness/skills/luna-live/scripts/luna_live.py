#!/usr/bin/env python3
"""Entrypoint for the luna-live skill: CLI, daemon, and MCP server.

Three ways in, one implementation:

* ``luna_live.py mcp`` — MCP stdio server (what the harness spawns from config).
* ``luna_live.py <action> [args]`` — one-shot CLI. Talks to the background
  daemon over a unix socket and starts it if it is not up, so a skill with a
  shell command can drive a session that outlives the call.
* ``luna_live.py serve`` — the daemon itself, in the foreground.

Parameters are gathered from the same three places the other skills use — a
JSON object on stdin, ``SKILL_ARGS``, and argv — with argv winning, so an agent
can pass structured arguments and a person can still type a command.
"""

from __future__ import annotations

import json
import os
import select
import subprocess
import sys
from typing import Any, Optional

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import lived  # noqa: E402  (path set up above)
import mcpserver  # noqa: E402

VERSION = "1.0.0"

# action -> daemon method. `type` is `send` without the Enter.
ACTIONS: dict[str, str] = {
    "start": "start",
    "send": "send",
    "type": "send",
    "keys": "keys",
    "wait": "wait",
    "idle": "idle",
    "assert": "assert",
    "screen": "screen",
    "screenshot": "screenshot",
    "shot": "screenshot",
    "resize": "resize",
    "list": "list",
    "ls": "list",
    "close": "close",
    "run": "run",
    "ping": "ping",
    "shutdown": "shutdown",
}

LOCAL_ACTIONS = {"serve", "mcp", "selftest", "help", "version"}

# Where a bare positional argument lands, per action.
POSITIONAL_TARGET: dict[str, str] = {
    "start": "command",
    "send": "text",
    "type": "text",
    "keys": "keys",
    "wait": "pattern",
    "assert": "pattern",
    "screen": "session_id",
    "screenshot": "path",
    "shot": "path",
    "close": "session_id",
    "resize": "session_id",
}

FLAG_ALIASES = {
    "session": "session_id",
    "id": "session_id",
    "shot_path": "path",
    "out": "path",
    "quiet_ms": "quiet_ms",
    "font_size": "font_size",
    "keep_going": "keep_going",
    "inline_image": "inline_image",
    "no_enter": "enter",
    "socket": "socket",
}

TYPED_FLAGS: dict[str, str] = {
    "session_id": "str",
    "text": "str",
    "keys": "str",
    "pattern": "str",
    "path": "str",
    "label": "str",
    "cwd": "str",
    "command": "str",
    "caption": "str",
    "scope": "str",
    "socket": "str",
    "timeout": "float",
    "cols": "int",
    "rows": "int",
    "font_size": "int",
    "quiet_ms": "int",
    "scrollback": "bool",
    "ansi": "bool",
    "json": "bool",
    "enter": "bool",
    "idle": "bool",
    "cursor": "bool",
    "regex": "bool",
    "present": "bool",
    "keep_going": "bool",
    "all": "bool",
    "inline_image": "bool",
    "argv": "json",
    "steps": "json",
    "env": "env",
}

BOOL_FLAGS = {name for name, kind in TYPED_FLAGS.items() if kind == "bool"}

USAGE = """luna-live — drive a real `luna` process and verify what a user sees

Usage:
  luna_live.py mcp                          MCP stdio server (for mcp.servers)
  luna_live.py serve [--socket PATH]        run the session daemon
  luna_live.py selftest                     check the plumbing end to end

  luna_live.py start [command] [--cols N --rows N --label NAME --cwd DIR]
                          [--env K=V] [--wait-for REGEX] [--timeout S]
  luna_live.py send  <text>   [--session ID] [--no-enter] [--wait-for REGEX]
  luna_live.py keys  <keys>   [--session ID]        e.g. "Ctrl+C" or "Down,Enter"
  luna_live.py wait  <regex>  [--session ID] [--timeout S] [--scope screen|raw]
  luna_live.py idle           [--session ID] [--quiet-ms N]
  luna_live.py assert <regex> [--session ID] [--absent] [--timeout S]
  luna_live.py screen         [--session ID] [--scrollback] [--ansi] [--json]
  luna_live.py screenshot     [--session ID] [--path FILE.png] [--font-size N]
  luna_live.py run            --steps '[{"action":"send","text":"/help"}]'
  luna_live.py list | close [--session ID | --all] | ping | shutdown

Output is JSON on stdout; `screen` prints the plain screen unless --json.
Exit code is 1 when an assert, wait, or run step fails, with the report on
stderr — a verification that lies about failing is worse than no verification.
"""


# ---------------------------------------------------------------- parsing


def _coerce(name: str, text: str) -> Any:
    kind = TYPED_FLAGS.get(name, "str")
    if kind == "str":
        return text
    if kind == "bool":
        low = text.strip().lower()
        if low in ("false", "0", "no", "off"):
            return False
        if low in ("true", "1", "yes", "on", ""):
            return True
        return bool(text)
    if kind == "int":
        return int(float(text))
    if kind == "float":
        return float(text)
    if kind == "json":
        return json.loads(text)
    return text


def _stdin_json() -> dict[str, Any]:
    if sys.stdin.isatty():
        return {}
    try:
        raw = sys.stdin.read().strip()
    except (OSError, ValueError):
        return {}
    if not raw:
        return {}
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {"input": parsed}


def _env_json() -> dict[str, Any]:
    raw = os.environ.get("SKILL_ARGS", "").strip()
    if not raw:
        return {}
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _parse_flags(argv: list[str]) -> tuple[Optional[str], dict[str, Any], list[str]]:
    action: Optional[str] = None
    params: dict[str, Any] = {}
    positionals: list[str] = []
    index = 0
    while index < len(argv):
        token = argv[index]
        if token == "--":
            positionals.extend(argv[index + 1 :])
            break
        if token.startswith("--"):
            name, _, inline = token[2:].partition("=")
            key = name.replace("-", "_")
            negated = False
            if key.startswith("no_") and key[3:] in BOOL_FLAGS:
                key, negated = key[3:], True
            key = FLAG_ALIASES.get(key, key)

            if inline:
                value: Any = _coerce(key, inline)
                if negated:
                    value = not value
            elif key in BOOL_FLAGS:
                value = not negated
            else:
                index += 1
                if index >= len(argv):
                    raise ValueError(f"flag --{name} needs a value")
                value = _coerce(key, argv[index])

            if TYPED_FLAGS.get(key) == "env" and isinstance(value, str):
                env_key, _, env_value = value.partition("=")
                params.setdefault("env", {})[env_key] = env_value
            else:
                params[key] = value
            index += 1
            continue
        if action is None:
            action = token.lower()
        else:
            positionals.append(token)
        index += 1
    return action, params, positionals


def _apply_positionals(
    action: str, params: dict[str, Any], positionals: list[str]
) -> None:
    if not positionals:
        return
    target = POSITIONAL_TARGET.get(action)
    if target is None:
        params.setdefault("input", " ".join(positionals))
        return
    if target in params and action in ("start",):
        return
    params[target] = " ".join(positionals)


def parse(argv: list[str]) -> tuple[str, dict[str, Any]]:
    action, params, positionals = _parse_flags(argv)
    resolved = (action or "").lower()
    if resolved in LOCAL_ACTIONS:
        # `mcp` and `serve` own the stdin stream: the one speaks JSON-RPC over
        # it. Reading it for argument JSON would swallow the first request and
        # hang the client, so these modes take their parameters from argv only.
        return resolved, params

    merged: dict[str, Any] = {}
    merged.update(_stdin_json())
    merged.update(_env_json())
    merged.update(params)
    resolved = (action or str(merged.get("action") or "")).lower()
    if action is None:
        positionals = []
    _apply_positionals(resolved, merged, positionals)
    return resolved, merged


# ---------------------------------------------------------------- output


def _emit(action: str, result: Any, params: dict[str, Any]) -> int:
    """Print the result and return the exit code."""
    as_json = bool(params.get("json"))
    failed = False
    if isinstance(result, dict):
        if action in ("assert", "run"):
            failed = not bool(result.get("passed", False))
        elif action == "wait":
            failed = not bool(result.get("matched", False))

    if action == "screen" and not as_json and isinstance(result, dict):
        payload = result.get("text", "")
    else:
        payload = json.dumps(result, indent=2, ensure_ascii=False, default=str)

    if failed:
        # The report is the evidence, so it goes out on stderr where a non-zero
        # exit is surfaced by the caller instead of being dropped.
        print(payload, file=sys.stderr)
        return 1
    print(payload)
    return 0


# ---------------------------------------------------------------- commands


def cmd_serve(params: dict[str, Any]) -> int:
    path = params.get("socket") or lived.socket_path()
    sys.stderr.write(f"luna-live daemon listening on {path}\n")
    sys.stderr.flush()
    try:
        lived.serve(path)
    except lived.LiveError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


def cmd_selftest(params: dict[str, Any]) -> int:
    return selftest(verbose=bool(params.get("verbose")))


CHILD_PROGRAM = r"""
import sys
print("\x1b[36m╭──── selftest ────╮\x1b[0m")
print("\x1b[36m│\x1b[0m \x1b[32m✔\x1b[0m ok  \x1b[31m✖\x1b[0m fail  \x1b[33m⏸\x1b[0m wait \x1b[36m│\x1b[0m")
print("\x1b[36m╰──────────────────╯\x1b[0m")
print("READY")
sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    print("BYE" if line == "quit" else "ECHO:" + line)
    sys.stdout.flush()
    if line == "quit":
        break
"""


def selftest(*, verbose: bool = False) -> int:
    """Exercise the whole path: daemon, pty, keys, render, screenshot, MCP."""
    from PIL import Image

    checks: list[tuple[str, bool, str]] = []

    def check(name: str, condition: bool, detail: str = "") -> None:
        checks.append((name, bool(condition), detail))
        mark = "PASS" if condition else "FAIL"
        line = f"[{mark}] {name}"
        if detail:
            line += f" — {detail}"
        print(line, flush=True)

    client = lived.LiveClient()
    check("daemon reachable", True, lived.socket_path())
    check("ping", client.call("ping", {}).get("pong") is True)

    session_id = f"selftest-{os.getpid()}"
    started = client.call(
        "start",
        {
            "argv": [sys.executable, "-c", CHILD_PROGRAM],
            "session_id": session_id,
            "cols": 40,
            "rows": 8,
            "wait_for": "READY",
            "timeout": 10,
        },
    )
    check("spawn on a pty", started["info"]["pid"] > 0, f"pid {started['info']['pid']}")
    check("child sees a tty", "READY" in started["screen"], "waited for READY")
    check(
        "box drawing captured",
        "╭" in started["screen"] and "╯" in started["screen"],
        "unicode survives the pty",
    )

    echoed = client.call("send", {"session_id": session_id, "text": "hello"})
    check("send + echo", "ECHO:hello" in echoed.get("screen", ""))

    client.call("keys", {"session_id": session_id, "keys": "char:quit,Enter"})
    match = client.call(
        "wait", {"session_id": session_id, "pattern": "BYE", "timeout": 10}
    )
    check("keys + wait", match["matched"], f"{match.get('elapsed_ms')}ms")

    shot = client.call("screenshot", {"session_id": session_id})
    info = shot["screenshot"]
    check("screenshot written", os.path.exists(info["path"]), info["path"])
    with open(info["path"], "rb") as handle:
        check("png is a png", handle.read(4) == b"\x89PNG")
    image = Image.open(info["path"]).convert("RGB")
    check(
        "png has real content",
        len(image.getcolors(maxcolors=300000) or []) > 5,
        f"{image.size[0]}x{image.size[1]}, {len(image.getcolors(maxcolors=300000) or [])} colours",
    )
    check("ansi reconstruction", "\x1b[" in client.call("screen", {"session_id": session_id, "ansi": True})["text"])

    passed_gate = client.call(
        "assert", {"session_id": session_id, "pattern": "selftest", "timeout": 3}
    )
    check("assert present passes", passed_gate["passed"] is True)
    failed_gate = client.call(
        "assert",
        {"session_id": session_id, "pattern": "nothing-like-this", "timeout": 1},
    )
    check("assert present fails when absent", failed_gate["passed"] is False)
    absent_gate = client.call(
        "assert",
        {"session_id": session_id, "pattern": "nothing-like-this", "present": False, "timeout": 1},
    )
    check("assert absent passes", absent_gate["passed"] is True)

    closed = client.call("close", {"session_id": session_id})
    check("close reaps the process", closed["closed"][0]["running"] is False)
    client.close()

    # The path the harness actually takes: spawn the MCP server and speak to it.
    check("mcp handshake", *mcp_selftest())

    failures = [name for name, ok, _ in checks if not ok]
    print("")
    if failures:
        print(f"SELFTEST FAILED: {len(failures)} of {len(checks)} checks failed")
        return 1
    print(f"SELFTEST PASSED: {len(checks)} checks")
    return 0


def mcp_selftest() -> tuple[bool, str]:
    entry = os.path.join(os.path.dirname(os.path.abspath(__file__)), "luna_live.py")
    proc = subprocess.Popen(
        [sys.executable, entry, "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        assert proc.stdin and proc.stdout

        def exchange(payload: dict[str, Any]) -> dict[str, Any]:
            proc.stdin.write((json.dumps(payload) + "\n").encode())
            proc.stdin.flush()
            # Bounded read: a server that stops answering must fail this check,
            # not hang the self-test.
            ready, _, _ = select.select([proc.stdout], [], [], 15.0)
            if not ready:
                method = payload.get("method")
                raise RuntimeError(f"no reply to {method!r} within 15s")
            line = proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP server closed stdout")
            return json.loads(line.decode())

        init = exchange(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "selftest"}},
            }
        )
        if init.get("result", {}).get("serverInfo", {}).get("name") != mcpserver.SERVER_NAME:
            return False, "initialize returned no serverInfo"
        listed = exchange({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        tools = [t["name"] for t in listed.get("result", {}).get("tools", [])]
        if "luna_start" not in tools or len(tools) < 10:
            return False, f"tools/list returned {tools}"
        plain = exchange(
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "luna_list", "arguments": {}},
            }
        )
        if plain.get("result", {}).get("isError"):
            return False, "tools/call luna_list reported an error"
        # The harness registers tools as mcp__<server>__<tool>; the server has
        # to accept that spelling or every call fails on the real path.
        namespaced = exchange(
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {"name": "mcp__luna-live__luna_list", "arguments": {}},
            }
        )
        if namespaced.get("result", {}).get("isError"):
            return False, "namespaced tool name was rejected"
        unknown = exchange(
            {
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "luna_nope", "arguments": {}},
            }
        )
        if not unknown.get("result", {}).get("isError"):
            return False, "unknown tool did not report an error"
        return True, f"{len(tools)} tools, namespace-tolerant"
    except Exception as exc:  # noqa: BLE001 - reported as a failed check
        return False, f"{type(exc).__name__}: {exc}"
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()


# ---------------------------------------------------------------- main


def main(argv: list[str]) -> int:
    try:
        action, params = parse(argv)
    except (ValueError, json.JSONDecodeError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    if action in ("", "help"):
        print(USAGE)
        return 0
    if action == "version":
        print(f"luna-live {VERSION}")
        return 0
    if action == "serve":
        return cmd_serve(params)
    if action == "mcp":
        return mcpserver.run(params.get("socket"))
    if action == "selftest":
        return cmd_selftest(params)
    if action not in ACTIONS:
        print(f"error: unknown action '{action}'\n", file=sys.stderr)
        print(USAGE, file=sys.stderr)
        return 2

    method = ACTIONS[action]
    if action == "type":
        params["enter"] = False
    if action == "close" and params.get("all"):
        params["all"] = True

    try:
        with lived.LiveClient(params.pop("socket", None)) as client:
            result = client.call(method, params)
    except lived.LiveError as exc:
        print(json.dumps({"error": str(exc)}, indent=2), file=sys.stderr)
        return 1
    return _emit(action, result, params)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
