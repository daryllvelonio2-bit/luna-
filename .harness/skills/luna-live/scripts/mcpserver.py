"""MCP stdio server exposing live `luna` verification as tools.

Speaks JSON-RPC 2.0 over stdin/stdout, one JSON object per line, and forwards
each call to the session daemon. Two rules shape the whole file:

* **stdout carries protocol and nothing else.** A stray log line on stdout is
  parsed as a response by the client and desynchronises the session, so every
  diagnostic goes to stderr.
* **Text is the payload.** The harness's MCP client reads `content[].text` and
  ignores other block types, so the screen, the screenshot path, and the verdict
  are all in text. An `image` block is attached as well for clients that render
  it, never as the only copy of anything.
"""

from __future__ import annotations

import base64
import json
import os
import sys
from typing import Any, Optional

import lived

SERVER_NAME = "luna-live"
SERVER_VERSION = "1.0.0"
PROTOCOL_VERSION = "2024-11-05"
MAX_INLINE_IMAGE_BYTES = 4_000_000

SESSION_ARG = {
    "type": "string",
    "description": "Target session id from luna_start (omit if exactly one is live).",
}

TOOLS: list[dict[str, Any]] = [
    {
        "name": "luna_start",
        "description": (
            "Start a live `luna` process on a real pty and return its first rendered "
            "screen. Defaults to running `luna` (the interactive chat). Use argv for a "
            "specific subcommand, e.g. [\"luna\",\"--help\"] or [\"luna\",\"app\"]. "
            "Every other luna_* tool targets the session this returns."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "argv": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": 'Command and arguments, e.g. ["luna","chat"]. Default ["luna"].',
                },
                "command": {
                    "type": "string",
                    "description": "Shell string alternative to argv, run via `sh -lc`.",
                },
                "cwd": {"type": "string", "description": "Working directory for the process."},
                "env": {
                    "type": "object",
                    "description": "Extra environment variables for the child.",
                },
                "cols": {"type": "integer", "description": "Terminal width (default 120)."},
                "rows": {"type": "integer", "description": "Terminal height (default 34)."},
                "label": {"type": "string", "description": "Human label, also used as session id when free."},
                "wait_for": {
                    "type": "string",
                    "description": "Regex to wait for before returning instead of waiting for the screen to settle.",
                },
                "timeout": {"type": "number", "description": "Seconds to wait (default 20)."},
                "quiet_ms": {
                    "type": "integer",
                    "description": "Milliseconds of silence that counts as started (default 400).",
                },
            },
        },
    },
    {
        "name": "luna_send",
        "description": (
            "Type text into the live session and press Enter (set enter=false to type "
            "without submitting). Returns the settled screen, so a prompt's effect is "
            "visible in the same call."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "text": {"type": "string", "description": "Text to type."},
                "enter": {"type": "boolean", "description": "Submit with Enter (default true)."},
                "wait_for": {"type": "string", "description": "Regex to wait for after sending."},
                "timeout": {"type": "number", "description": "Seconds to wait (default 20)."},
                "idle": {"type": "boolean", "description": "Wait for the screen to settle (default true)."},
                "quiet_ms": {"type": "integer", "description": "Settle threshold in ms."},
                "screen": {"type": "boolean", "description": "Include the screen in the result (default true)."},
            },
            "required": ["text"],
        },
    },
    {
        "name": "luna_keys",
        "description": (
            "Send named keys to the live session: Enter, Tab, BackTab, Esc, Up/Down/Left/"
            "Right, Home, End, PageUp, PageDown, Delete, F1..F12, ctrl+<letter> (Ctrl+C, "
            "Ctrl+O), alt+<key>, or char:<literal>. Comma or space separated, e.g. "
            "\"Ctrl+C\", \"Down,Enter\", \"char:/help,Enter\"."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "keys": {"type": "string", "description": "Key spec, e.g. \"Ctrl+C\" or \"Up,Up,Enter\"."},
                "wait_for": {"type": "string", "description": "Regex to wait for after sending."},
                "timeout": {"type": "number", "description": "Seconds to wait (default 20)."},
                "idle": {"type": "boolean", "description": "Wait for the screen to settle (default true)."},
                "quiet_ms": {"type": "integer", "description": "Settle threshold in ms."},
                "screen": {"type": "boolean", "description": "Include the screen in the result (default true)."},
            },
            "required": ["keys"],
        },
    },
    {
        "name": "luna_screen",
        "description": (
            "Read what the user's terminal is showing: the rendered viewport (and the "
            "scrollback above it when requested). This is the text a person sees, not "
            "the raw byte stream. Use ansi=true to keep the colour escape sequences."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "scrollback": {"type": "boolean", "description": "Include scrolled-off lines (default false)."},
                "ansi": {"type": "boolean", "description": "Return colour escape sequences (default false)."},
            },
        },
    },
    {
        "name": "luna_screenshot",
        "description": (
            "Render the live screen to a PNG file with real colours, box-drawing and "
            "Nerd Font glyphs — the artefact to show a human. Returns the absolute path "
            "plus the screen text. This is how a claim about the UI gets evidence."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "path": {"type": "string", "description": "Output .png path (default: a numbered file in the shots dir)."},
                "font_size": {"type": "integer", "description": "Font size in px (default 15)."},
                "cursor": {"type": "boolean", "description": "Draw the cursor block (default true)."},
                "caption": {"type": "string", "description": "Caption bar text (default: label, size, status)."},
                "inline_image": {
                    "type": "boolean",
                    "description": "Also return the image as an MCP image block (default true).",
                },
                "idle": {"type": "boolean", "description": "Wait for the screen to settle first (default true)."},
                "quiet_ms": {"type": "integer", "description": "Settle threshold in ms."},
            },
        },
    },
    {
        "name": "luna_wait",
        "description": (
            "Wait until a pattern appears on the rendered screen (or the scrollback). "
            "Returns matched=true/false with the elapsed time, never raises — use it to "
            "let a slow turn finish before judging the screen."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "pattern": {"type": "string", "description": "Regex (or literal when regex=false)."},
                "timeout": {"type": "number", "description": "Seconds to wait (default 15)."},
                "regex": {"type": "boolean", "description": "Treat pattern as a regex (default true)."},
                "scope": {
                    "type": "string",
                    "enum": ["screen", "raw"],
                    "description": "'screen' = what is rendered (default), 'raw' = unstripped child output.",
                },
                "scrollback": {"type": "boolean", "description": "Search scrolled-off lines too (default true)."},
            },
            "required": ["pattern"],
        },
    },
    {
        "name": "luna_idle",
        "description": (
            "Wait until the session has produced no output for a moment. The reliable way "
            "to let a redraw finish before screenshotting instead of guessing a sleep."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "quiet_ms": {"type": "integer", "description": "Silence threshold in ms (default 400)."},
                "timeout": {"type": "number", "description": "Give up after this many seconds (default 20)."},
            },
        },
    },
    {
        "name": "luna_assert",
        "description": (
            "Gate a workflow on what is on screen: assert a pattern is present (or absent "
            "with present=false). Returns passed=true/false; a failure includes the screen "
            "that failed it. This is the check to end a verification with."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "pattern": {"type": "string", "description": "Regex (or literal when regex=false)."},
                "present": {"type": "boolean", "description": "Require presence (default true); false requires absence."},
                "timeout": {"type": "number", "description": "Seconds to wait before failing (default 10)."},
                "regex": {"type": "boolean", "description": "Treat pattern as a regex (default true)."},
                "scope": {"type": "string", "enum": ["screen", "raw"], "description": "Where to look."},
                "scrollback": {"type": "boolean", "description": "Search scrolled-off lines too (default true)."},
            },
            "required": ["pattern"],
        },
    },
    {
        "name": "luna_run",
        "description": (
            "Drive a whole workflow in one call: start a session (or reuse one), run a "
            "list of steps, assert along the way, and return a pass/fail report with a "
            "final screenshot. Steps use the same verbs — send, keys, wait, idle, "
            "screenshot, assert, sleep, screen — e.g. "
            "[{\"action\":\"wait\",\"pattern\":\"luna\"},{\"action\":\"send\",\"text\":\"/help\"},"
            "{\"action\":\"assert\",\"pattern\":\"Available skills\"}]. Stops at the first "
            "failed step unless keep_going is set."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "steps": {
                    "type": "array",
                    "description": "Steps to execute in order; each is an object with an 'action'.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": [
                                    "send",
                                    "keys",
                                    "wait",
                                    "idle",
                                    "assert",
                                    "screen",
                                    "screenshot",
                                    "sleep",
                                ],
                            },
                            "note": {"type": "string", "description": "Label for this step in the report."},
                        },
                        "required": ["action"],
                    },
                },
                "keep_going": {"type": "boolean", "description": "Continue after a failed step (default false)."},
                "screenshot": {"type": "boolean", "description": "Capture a final screenshot (default true)."},
                "argv": {"type": "array", "items": {"type": "string"}, "description": "Command when starting a new session."},
                "command": {"type": "string", "description": "Shell string when starting a new session."},
                "cwd": {"type": "string", "description": "Working directory when starting a new session."},
                "env": {"type": "object", "description": "Environment when starting a new session."},
                "cols": {"type": "integer", "description": "Terminal width when starting."},
                "rows": {"type": "integer", "description": "Terminal height when starting."},
                "label": {"type": "string", "description": "Session label when starting."},
            },
            "required": ["steps"],
        },
    },
    {
        "name": "luna_list",
        "description": "List live sessions with pid, size, age, and running state.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "luna_close",
        "description": "Close a live session (or every session with all=true) and reap the process.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": SESSION_ARG,
                "all": {"type": "boolean", "description": "Close every live session."},
            },
        },
    },
]

TOOL_METHODS: dict[str, str] = {
    "luna_start": "start",
    "luna_send": "send",
    "luna_keys": "keys",
    "luna_screen": "screen",
    "luna_screenshot": "screenshot",
    "luna_wait": "wait",
    "luna_idle": "idle",
    "luna_assert": "assert",
    "luna_run": "run",
    "luna_list": "list",
    "luna_close": "close",
}


def _log(message: str) -> None:
    """Diagnostics go to stderr; stdout is the protocol channel."""
    sys.stderr.write(f"[{SERVER_NAME}] {message}\n")
    sys.stderr.flush()


def strip_namespace(name: str) -> str:
    """Accept ``mcp__<server>__<tool>`` as well as the bare tool name.

    The harness registers MCP tools under a namespaced name; whether the
    namespaced or the original name arrives depends on the client, and a server
    that only understands one of them fails for the other.
    """
    if name.startswith("mcp__"):
        parts = name.split("__", 2)
        if len(parts) == 3 and parts[2]:
            return parts[2]
    return name


class Server:
    def __init__(self, socket: Optional[str] = None) -> None:
        self.socket = socket
        self._client: Optional[lived.LiveClient] = None

    @property
    def client(self) -> lived.LiveClient:
        # Connected lazily: a server that cannot reach the daemon should still
        # answer initialize and tools/list, so the failure lands on the tool
        # call that needed it rather than on session startup.
        if self._client is None:
            self._client = lived.LiveClient(self.socket)
        return self._client

    def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        tool = strip_namespace(name)
        method = TOOL_METHODS.get(tool)
        if method is None:
            raise lived.LiveError(
                f"unknown tool '{name}'; available: {', '.join(sorted(TOOL_METHODS))}"
            )
        result = self.client.call(method, arguments or {})
        return self._to_content(tool, result, arguments or {})

    def _to_content(
        self, tool: str, result: Any, arguments: dict[str, Any]
    ) -> dict[str, Any]:
        text = format_result(tool, result)
        content: list[dict[str, Any]] = [{"type": "text", "text": text}]
        is_error = False

        if tool == "luna_assert":
            is_error = not bool(result.get("passed", False))
        elif tool == "luna_run":
            is_error = not bool(result.get("passed", False))
        elif tool == "luna_wait":
            is_error = not bool(result.get("matched", False))

        if tool == "luna_screenshot" and arguments.get("inline_image", True):
            block = _image_block(result.get("screenshot") or {})
            if block is not None:
                content.append(block)
        if tool == "luna_run" and arguments.get("inline_image", False):
            block = _image_block((result or {}).get("screenshot") or {})
            if block is not None:
                content.append(block)

        return {"content": content, "isError": is_error}


def _image_block(info: dict[str, Any]) -> Optional[dict[str, Any]]:
    path = info.get("path")
    if not path or not os.path.exists(path):
        return None
    if os.path.getsize(path) > MAX_INLINE_IMAGE_BYTES:
        return None
    try:
        with open(path, "rb") as handle:
            encoded = base64.b64encode(handle.read()).decode("ascii")
    except OSError:
        return None
    return {"type": "image", "data": encoded, "mimeType": "image/png"}


def _json(value: Any) -> str:
    return json.dumps(value, indent=2, ensure_ascii=False, default=str)


def format_result(tool: str, result: Any) -> str:
    """Turn a daemon result into text an agent can act on."""
    if not isinstance(result, dict):
        return _json(result)

    if tool == "luna_start":
        info = result.get("info", {})
        return (
            f"session {result.get('session_id')} started "
            f"(pid {info.get('pid')}, {info.get('cols')}x{info.get('rows')})\n"
            f"argv: {' '.join(info.get('argv') or [])}\n"
            f"wait: {_json(result.get('wait'))}\n\n"
            f"--- screen ---\n{result.get('screen', '')}"
        )

    if tool == "luna_send":
        return _json(result)

    if tool == "luna_keys":
        return _json(result)

    if tool == "luna_screen":
        header = (
            f"session {result.get('session_id')} "
            f"({'running' if result.get('running') else 'exited ' + str(result.get('exit_code'))}) "
            f"cursor ({result.get('cursor', {}).get('x')},{result.get('cursor', {}).get('y')})"
        )
        return f"{header}\n--- screen ---\n{result.get('text', '')}"

    if tool == "luna_screenshot":
        info = result.get("screenshot", {})
        return (
            f"screenshot: {info.get('path')}\n"
            f"size: {info.get('width')}x{info.get('height')} "
            f"({info.get('cols')}x{info.get('rows')} cells, {info.get('bytes')} bytes)\n"
            f"sha256: {info.get('sha256')}\n\n"
            f"--- screen ---\n{result.get('text', '')}"
        )

    if tool == "luna_wait":
        if result.get("matched"):
            return (
                f"matched '{result.get('pattern')}' after {result.get('elapsed_ms')}ms\n"
                f"line: {result.get('line')}"
            )
        return (
            f"NOT matched '{result.get('pattern')}' after {result.get('elapsed_ms')}ms\n"
            f"exited: {result.get('exited')} (code {result.get('exit_code')})\n\n"
            f"--- screen tail ---\n{result.get('tail', '')}"
        )

    if tool == "luna_idle":
        return _json(result)

    if tool == "luna_assert":
        verdict = "PASS" if result.get("passed") else "FAIL"
        verb = "present" if result.get("present", True) else "absent"
        lines = [
            f"{verdict}: expected '{result.get('pattern')}' to be {verb} "
            f"(found={result.get('found')}, {result.get('elapsed_ms')}ms)"
        ]
        if result.get("reason"):
            lines.append(result["reason"])
        lines.append("")
        lines.append("--- screen ---")
        lines.append(result.get("screen", ""))
        return "\n".join(lines)

    if tool == "luna_run":
        lines = [
            f"{'PASS' if result.get('passed') else 'FAIL'}: "
            f"{len(result.get('steps') or [])} step(s) on session {result.get('session_id')}"
        ]
        for step in result.get("steps") or []:
            mark = "ok  " if step.get("ok", True) else "FAIL"
            detail = step.get("note") or step.get("pattern") or step.get("text") or ""
            if step.get("reason"):
                detail = step["reason"]
            lines.append(f"  [{mark}] {step.get('action')} {detail}".rstrip())
        shot = (result.get("screenshot") or {}).get("path")
        if shot:
            lines.append(f"screenshot: {shot}")
        lines.append("")
        lines.append("--- final screen ---")
        lines.append(result.get("screen", ""))
        return "\n".join(lines)

    return _json(result)


def handle(server: Server, request: dict[str, Any]) -> Optional[dict[str, Any]]:
    """Dispatch one JSON-RPC message. Returns None for notifications."""
    request_id = request.get("id")
    method = request.get("method")
    params = request.get("params") or {}

    if request_id is None:
        # A notification needs no reply; `notifications/initialized` is the
        # only one a client is expected to send and it carries no work.
        return None

    def ok(result: Any) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": request_id, "result": result}

    def fail(code: int, message: str) -> dict[str, Any]:
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": code, "message": message},
        }

    if method == "initialize":
        requested = params.get("protocolVersion") or PROTOCOL_VERSION
        return ok(
            {
                "protocolVersion": requested,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            }
        )
    if method in ("notifications/initialized", "initialized"):
        return None
    if method == "ping":
        return ok({})
    if method == "tools/list":
        return ok({"tools": TOOLS})
    if method == "tools/call":
        name = params.get("name")
        if not name:
            return fail(-32602, "tools/call requires 'name'")
        arguments = params.get("arguments") or {}
        try:
            return ok(server.call_tool(str(name), arguments))
        except lived.LiveError as exc:
            return ok(
                {
                    "content": [{"type": "text", "text": f"error: {exc}"}],
                    "isError": True,
                }
            )
        except Exception as exc:  # noqa: BLE001 - the client must get an answer
            return ok(
                {
                    "content": [
                        {"type": "text", "text": f"error: {type(exc).__name__}: {exc}"}
                    ],
                    "isError": True,
                }
            )
    return fail(-32601, f"method not found: {method}")


def run(socket: Optional[str] = None) -> int:
    server = Server(socket)
    for raw in sys.stdin.buffer:
        line = raw.decode("utf-8", "replace").strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            _log(f"ignoring malformed input: {exc}")
            continue
        if not isinstance(request, dict):
            _log("ignoring non-object message")
            continue
        response = handle(server, request)
        if response is None:
            continue
        try:
            sys.stdout.write(json.dumps(response, ensure_ascii=False, default=str) + "\n")
            sys.stdout.flush()
        except (BrokenPipeError, ValueError):
            return 0
    if server._client is not None:
        server._client.close()
    return 0
