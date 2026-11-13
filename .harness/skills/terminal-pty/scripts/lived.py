"""A background daemon that owns live pty sessions, and a client for it.

A session has to outlive the call that made it: start a chat, type into it,
screenshot it, close it — four separate tool calls, one process. So the
sessions live in a long-running daemon behind a unix socket, and every caller
(the one-shot CLI the skill wraps, the MCP stdio server, anything else) is a
thin client. That also means two agents can look at the same live session,
which is the difference between "I ran it" and "here is the screen".

Protocol: newline-delimited JSON. Request `{"id": N, "method": ..., "params":
{...}}`, response `{"id": N, "ok": true, "result": ...}` or `{"ok": false,
"error": ...}`. One line each, so both ends can stay dumb.
"""

from __future__ import annotations

import json
import os
import re
import socket
import subprocess
import sys
import threading
import time
import uuid
from typing import Any, Callable, Optional

import pngshot
from terminals import DEFAULT_COLS, DEFAULT_ROWS, DEFAULT_SCROLLBACK, PtySession

DEFAULT_DIR = "~/.terminal-pty"
SOCKET_NAME = "live.sock"
LOG_NAME = "daemon.log"
SHOTS_DIRNAME = "shots"
DAEMON_STARTUP_TIMEOUT = 10.0
CLIENT_TIMEOUT = 600.0


class LiveError(RuntimeError):
    """A request the daemon refused, or a daemon that could not be reached."""


# ---------------------------------------------------------------- paths


def live_dir() -> str:
    override = os.environ.get("TERMINAL_PTY_DIR")
    return os.path.abspath(os.path.expanduser(override or DEFAULT_DIR))


def socket_path() -> str:
    override = os.environ.get("TERMINAL_PTY_SOCKET")
    if override:
        return os.path.abspath(os.path.expanduser(override))
    return os.path.join(live_dir(), SOCKET_NAME)


def shots_dir() -> str:
    return os.path.join(live_dir(), SHOTS_DIRNAME)


def log_path() -> str:
    return os.path.join(live_dir(), LOG_NAME)


def _ensure_dir(path: str) -> str:
    os.makedirs(path, exist_ok=True)
    return path


# ---------------------------------------------------------------- registry


class Registry:
    """Owns the live sessions. Every mutation happens under one lock."""

    def __init__(self) -> None:
        self._sessions: dict[str, PtySession] = {}
        self._lock = threading.RLock()
        self._counter = 0
        self._shot_seq: dict[str, int] = {}
        self.stopping = threading.Event()

    # -- helpers -------------------------------------------------

    def _mint_id(self, label: Optional[str]) -> str:
        self._counter += 1
        if label:
            slug = re.sub(r"[^A-Za-z0-9._-]+", "-", label).strip("-")[:32]
            if slug and slug not in self._sessions:
                return slug
            if slug:
                return f"{slug}-{self._counter}"
        return f"s{self._counter}"

    def get(self, session_id: Optional[str]) -> PtySession:
        with self._lock:
            if session_id and session_id in self._sessions:
                return self._sessions[session_id]
            if session_id is None and len(self._sessions) == 1:
                return next(iter(self._sessions.values()))
            if session_id is None and not self._sessions:
                raise LiveError("no live sessions; call pty_start first")
            available = ", ".join(sorted(self._sessions)) or "none"
            raise LiveError(f"unknown session '{session_id}'; live sessions: {available}")

    def list(self) -> list[dict[str, Any]]:
        with self._lock:
            sessions = list(self._sessions.values())
        return [
            {**session.info(), "screenshot_dir": shots_dir()} for session in sessions
        ]

    def close_all(self) -> None:
        with self._lock:
            sessions = list(self._sessions.values())
            self._sessions.clear()
        for session in sessions:
            try:
                session.close()
            except Exception:
                pass

    def default_shot_path(self, session_id: str) -> str:
        _ensure_dir(shots_dir())
        with self._lock:
            seq = self._shot_seq.get(session_id, 0) + 1
            self._shot_seq[session_id] = seq
        stamp = time.strftime("%Y%m%d-%H%M%S", time.localtime())
        return os.path.join(shots_dir(), f"{session_id}-{seq:03d}-{stamp}.png")

    # -- methods -------------------------------------------------

    def start(self, params: dict[str, Any]) -> dict[str, Any]:
        argv = self._resolve_argv(params)
        label = params.get("label")
        session_id = params.get("session_id") or self._mint_id(
            label or (os.path.basename(argv[0]) if argv else None)
        )
        env = {str(k): str(v) for k, v in (params.get("env") or {}).items()}
        session = PtySession(
            session_id,
            argv,
            cwd=params.get("cwd") or None,
            env=env,
            cols=int(params.get("cols") or DEFAULT_COLS),
            rows=int(params.get("rows") or DEFAULT_ROWS),
            scrollback=int(params.get("scrollback") or DEFAULT_SCROLLBACK),
            label=label,
        )
        session.start()
        with self._lock:
            self._sessions[session_id] = session

        timeout = float(params.get("timeout") or 20.0)
        if params.get("wait_for"):
            wait = session.wait_for(
                str(params["wait_for"]),
                timeout=timeout,
                regex=bool(params.get("regex", True)),
                scope=str(params.get("scope") or "screen"),
            )
        else:
            # Waiting out the first burst beats a fixed sleep: TUIs clear the
            # screen and paint their first frame on startup, and a screenshot
            # taken mid-paint is a screenshot of nothing.
            wait = session.wait_idle(
                quiet_ms=int(params.get("quiet_ms") or 400), timeout=timeout
            )
        return {
            "session_id": session_id,
            "info": session.info(),
            "wait": wait,
            "screen": session.screen_text(),
        }

    @staticmethod
    def _resolve_argv(params: dict[str, Any]) -> list[str]:
        argv = params.get("argv")
        if argv:
            return [str(a) for a in argv]
        command = params.get("command")
        if command:
            if isinstance(command, str):
                # A shell string is convenient but opaque; wrap it so quoting
                # still behaves the way the caller wrote it.
                return ["sh", "-lc", command]
            return [str(a) for a in command]
        return [os.environ.get("SHELL") or "/bin/sh"]

    def send(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self.get(params.get("session_id"))
        text = str(params.get("text", ""))
        enter = bool(params.get("enter", True))
        session.send_text(text, enter=enter)
        return self._after_input(session, params)

    def keys(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self.get(params.get("session_id"))
        spec = str(params.get("keys", ""))
        session.send_keys(spec)
        return self._after_input(session, params)

    def _after_input(
        self, session: PtySession, params: dict[str, Any]
    ) -> dict[str, Any]:
        result: dict[str, Any] = {"session_id": session.id, "sent": True}
        if params.get("wait_for"):
            result["wait"] = session.wait_for(
                str(params["wait_for"]),
                timeout=float(params.get("timeout") or 20.0),
                regex=bool(params.get("regex", True)),
                scope=str(params.get("scope") or "screen"),
            )
        elif params.get("idle", True):
            result["idle"] = session.wait_idle(
                quiet_ms=int(params.get("quiet_ms") or 250),
                timeout=float(params.get("timeout") or 20.0),
            )
        if params.get("screen", True):
            result["screen"] = session.screen_text()
        return result

    def idle(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self.get(params.get("session_id"))
        result = session.wait_idle(
            quiet_ms=int(params.get("quiet_ms") or 400),
            timeout=float(params.get("timeout") or 20.0),
        )
        if params.get("screen", True):
            result["screen"] = session.screen_text()
        return result

    def wait(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self.get(params.get("session_id"))
        pattern = params.get("pattern")
        if not pattern:
            raise LiveError("wait requires 'pattern'")
        return session.wait_for(
            str(pattern),
            timeout=float(params.get("timeout") or 15.0),
            regex=bool(params.get("regex", True)),
            scope=str(params.get("scope") or "screen"),
            scrollback=bool(params.get("scrollback", True)),
        )

    def assert_(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self.get(params.get("session_id"))
        pattern = params.get("pattern")
        if not pattern:
            raise LiveError("assert requires 'pattern'")
        present = bool(params.get("present", True))
        timeout = float(params.get("timeout") or 10.0)
        wait = session.wait_for(
            str(pattern),
            timeout=timeout,
            regex=bool(params.get("regex", True)),
            scope=str(params.get("scope") or "screen"),
            scrollback=bool(params.get("scrollback", True)),
        )
        found = bool(wait["matched"])
        passed = found if present else not found
        result: dict[str, Any] = {
            "passed": passed,
            "present": present,
            "found": found,
            "pattern": str(pattern),
            "elapsed_ms": wait.get("elapsed_ms"),
            "session_id": session.id,
        }
        if passed:
            result["screen"] = session.screen_text()
        else:
            # A failed gate is only actionable with the screen that failed it.
            wanted = "present" if present else "absent"
            result["reason"] = f"expected {wanted} on screen, saw the opposite"
            result["screen"] = session.screen_text(scrollback=True)
        return result

    def screen(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self.get(params.get("session_id"))
        scrollback = bool(params.get("scrollback", False))
        if params.get("ansi"):
            text = session.screen_ansi(scrollback=scrollback)
        else:
            text = session.screen_text(scrollback=scrollback)
        return {
            "session_id": session.id,
            "text": text,
            "cols": session.cols,
            "rows": session.rows,
            "cursor": session.cursor(),
            "running": not session.exited(),
            "exit_code": session.exit_code(),
        }

    def screenshot(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self.get(params.get("session_id"))
        if params.get("idle", True):
            session.wait_idle(
                quiet_ms=int(params.get("quiet_ms") or 250),
                timeout=float(params.get("timeout") or 20.0),
            )
        path = params.get("path") or self.default_shot_path(session.id)
        path = os.path.abspath(os.path.expanduser(str(path)))
        cells = session.snapshot_cells()
        caption = params.get("caption")
        if caption is None:
            status = (
                "running" if not session.exited() else f"exit {session.exit_code()}"
            )
            caption = (
                f"{session.label} — {session.cols}x{session.rows} — {status}"
            )
        info = pngshot.render_png(
            cells,
            path,
            font_size=int(params.get("font_size") or 15),
            cursor=session.cursor() if params.get("cursor", True) else None,
            caption=str(caption) if caption else None,
        )
        return {
            "session_id": session.id,
            "screenshot": info,
            "text": session.screen_text(),
            "cursor": session.cursor(),
        }

    def resize(self, params: dict[str, Any]) -> dict[str, Any]:
        session = self.get(params.get("session_id"))
        session.resize(
            int(params.get("cols") or session.cols),
            int(params.get("rows") or session.rows),
        )
        session.settle(200)
        return {**session.info(), "screen": session.screen_text()}

    def close(self, params: dict[str, Any]) -> dict[str, Any]:
        if params.get("all"):
            with self._lock:
                sessions = list(self._sessions.values())
                self._sessions.clear()
            return {"closed": [session.close() for session in sessions]}
        session = self.get(params.get("session_id"))
        with self._lock:
            self._sessions.pop(session.id, None)
        return {"closed": [session.close()]}

    def run(self, params: dict[str, Any]) -> dict[str, Any]:
        """Run a list of steps, so a whole workflow is one tool call.

        Steps are the same verbs as the methods above. Execution stops at the
        first failing assertion unless `keep_going` is set — a workflow check
        that continues past a failed gate reports noise, not a failure.
        """
        steps = params.get("steps") or []
        if not isinstance(steps, list):
            raise LiveError("run requires 'steps' as a list")
        keep_going = bool(params.get("keep_going", False))

        session_id = params.get("session_id")
        if not session_id:
            start_params = {
                key: params[key]
                for key in ("argv", "command", "cwd", "env", "cols", "rows", "label")
                if key in params
            }
            started = self.start({**start_params, "quiet_ms": params.get("quiet_ms", 400)})
            session_id = started["session_id"]
            results: list[dict[str, Any]] = [
                {"action": "start", "ok": True, "session_id": session_id}
            ]
        else:
            results = []

        passed = True
        for index, raw in enumerate(steps):
            if not isinstance(raw, dict):
                raise LiveError(f"step {index} must be an object")
            action = str(raw.get("action") or "").strip()
            if not action:
                raise LiveError(f"step {index} has no 'action'")
            step_params = {k: v for k, v in raw.items() if k != "action"}
            step_params.setdefault("session_id", session_id)
            entry: dict[str, Any] = {"action": action, "step": index}
            if raw.get("note"):
                entry["note"] = raw["note"]
            try:
                if action == "sleep":
                    time.sleep(float(step_params.get("seconds") or 1.0))
                    entry["ok"] = True
                    results.append(entry)
                    continue
                handler = self.methods().get(action)
                if handler is None:
                    raise LiveError(f"unknown action '{action}'")
                outcome = handler(step_params)
                if action == "wait":
                    entry["ok"] = bool(outcome.get("matched", False))
                elif action == "assert":
                    entry["ok"] = bool(outcome.get("passed", False))
                else:
                    entry["ok"] = True
                if action in ("screen", "screenshot"):
                    entry.update(
                        {
                            key: value
                            for key, value in outcome.items()
                            if key in ("text", "screenshot", "cursor")
                        }
                    )
                elif action in ("wait", "assert"):
                    entry.update(
                        {
                            key: value
                            for key, value in outcome.items()
                            if key in ("matched", "found", "passed", "elapsed_ms", "line", "reason", "present", "pattern")
                        }
                    )
            except LiveError as exc:
                entry["ok"] = False
                entry["error"] = str(exc)
            if not entry.get("ok", True):
                passed = False
                results.append(entry)
                if not keep_going:
                    break
                continue
            results.append(entry)

        session = self.get(session_id)
        report: dict[str, Any] = {
            "session_id": session_id,
            "passed": passed,
            "steps": results,
            "screen": session.screen_text(),
        }
        if params.get("screenshot", True):
            report["screenshot"] = self.screenshot(
                {"session_id": session_id, "caption": params.get("caption")}
            )["screenshot"]
        return report

    def methods(self) -> dict[str, Callable[[dict[str, Any]], dict[str, Any]]]:
        return {
            "start": self.start,
            "send": self.send,
            "type": lambda p: self.send({**p, "enter": False}),
            "keys": self.keys,
            "idle": self.idle,
            "wait": self.wait,
            "assert": self.assert_,
            "screen": self.screen,
            "screenshot": self.screenshot,
            "resize": self.resize,
            "list": lambda _p: {"sessions": self.list()},
            "close": self.close,
            "run": self.run,
        }

    def dispatch(self, method: str, params: dict[str, Any]) -> Any:
        if method == "ping":
            return {"pong": True, "pid": os.getpid(), "dir": live_dir()}
        if method == "shutdown":
            self.stopping.set()
            return {"shutting_down": True}
        handler = self.methods().get(method)
        if handler is None:
            raise LiveError(f"unknown method '{method}'")
        return handler(params or {})


# ---------------------------------------------------------------- daemon


def serve(path: Optional[str] = None, *, ready: Optional[Callable[[str], None]] = None) -> None:
    """Run the daemon in the foreground until it is asked to shut down."""
    path = path or socket_path()
    _ensure_dir(os.path.dirname(path))
    registry = Registry()

    if os.path.exists(path):
        # A socket file with nothing behind it is a crashed daemon, not a
        # reason to refuse to start; a live one is.
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        probe.settimeout(0.4)
        try:
            probe.connect(path)
            listening = True
        except OSError:
            listening = False
        finally:
            probe.close()
        if listening:
            raise LiveError(f"a daemon is already listening on {path}")
        try:
            os.unlink(path)
        except OSError:
            pass

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(path)
    os.chmod(path, 0o600)
    server.listen(16)
    server.settimeout(0.5)
    if ready:
        ready(path)

    try:
        while not registry.stopping.is_set():
            try:
                conn, _ = server.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(
                target=_serve_connection, args=(conn, registry), daemon=True
            ).start()
    finally:
        registry.close_all()
        server.close()
        try:
            os.unlink(path)
        except OSError:
            pass


def _serve_connection(conn: socket.socket, registry: Registry) -> None:
    with conn:
        stream = conn.makefile("rwb")
        try:
            for raw in stream:
                line = raw.decode("utf-8", "replace").strip()
                if not line:
                    continue
                try:
                    request = json.loads(line)
                except json.JSONDecodeError as exc:
                    response = {"id": None, "ok": False, "error": f"bad JSON: {exc}"}
                else:
                    response = _handle_request(request, registry)
                try:
                    stream.write(
                        (json.dumps(response, default=str) + "\n").encode("utf-8")
                    )
                    stream.flush()
                except (BrokenPipeError, ValueError):
                    break
        except (ConnectionResetError, OSError):
            pass


def _handle_request(request: dict[str, Any], registry: Registry) -> dict[str, Any]:
    request_id = request.get("id")
    method = str(request.get("method") or "")
    params = request.get("params") or {}
    if not isinstance(params, dict):
        return {"id": request_id, "ok": False, "error": "params must be an object"}
    try:
        result = registry.dispatch(method, params)
        return {"id": request_id, "ok": True, "result": result}
    except LiveError as exc:
        return {"id": request_id, "ok": False, "error": str(exc)}
    except FileNotFoundError as exc:
        return {"id": request_id, "ok": False, "error": str(exc)}
    except Exception as exc:  # noqa: BLE001 - a daemon must answer, not die
        return {
            "id": request_id,
            "ok": False,
            "error": f"{type(exc).__name__}: {exc}",
        }


# ---------------------------------------------------------------- client


def _can_connect(path: str) -> bool:
    if not os.path.exists(path):
        return False
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    probe.settimeout(1.0)
    try:
        probe.connect(path)
        return True
    except OSError:
        return False
    finally:
        probe.close()


def ensure_daemon(path: Optional[str] = None, *, timeout: float = DAEMON_STARTUP_TIMEOUT) -> str:
    """Return the socket path, starting the daemon if nothing is listening."""
    path = path or socket_path()
    if _can_connect(path):
        return path
    _ensure_dir(os.path.dirname(path))

    entry = os.path.join(os.path.dirname(os.path.abspath(__file__)), "terminal_pty.py")
    log = open(log_path(), "ab")
    try:
        subprocess.Popen(
            [sys.executable, entry, "serve", "--socket", path],
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=log,
            start_new_session=True,
            close_fds=True,
        )
    except OSError as exc:
        log.close()
        raise LiveError(f"could not start the terminal-pty daemon: {exc}") from exc
    finally:
        log.close()

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if _can_connect(path):
            return path
        time.sleep(0.05)
    raise LiveError(
        f"terminal-pty daemon did not come up on {path} within {timeout:g}s; "
        f"see {log_path()}"
    )


class LiveClient:
    """One connection to the daemon. Not thread-safe; make one per thread."""

    def __init__(self, path: Optional[str] = None, *, autostart: bool = True) -> None:
        self.path = path or socket_path()
        if autostart:
            ensure_daemon(self.path)
        self._sock: Optional[socket.socket] = None
        self._stream: Any = None
        self._id = 0
        self._lock = threading.Lock()
        self._connect()

    def _connect(self) -> None:
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.settimeout(CLIENT_TIMEOUT)
        self._sock.connect(self.path)
        self._stream = self._sock.makefile("rwb")

    def call(
        self, method: str, params: Optional[dict[str, Any]] = None, *, timeout: Optional[float] = None
    ) -> Any:
        with self._lock:
            self._id += 1
            request_id = self._id
            payload = json.dumps(
                {"id": request_id, "method": method, "params": params or {}}
            )
            if self._sock is None:
                self._connect()
            assert self._sock is not None
            previous = self._sock.gettimeout()
            if timeout is not None:
                self._sock.settimeout(timeout)
            try:
                self._sock.sendall(payload.encode("utf-8") + b"\n")
                line = self._stream.readline()
            except socket.timeout as exc:
                raise LiveError(
                    f"daemon did not answer '{method}' in time"
                ) from exc
            finally:
                if timeout is not None and self._sock is not None:
                    self._sock.settimeout(previous)
            if not line:
                raise LiveError("daemon closed the connection")
            response = json.loads(line.decode("utf-8", "replace"))
            if not response.get("ok"):
                raise LiveError(str(response.get("error") or "daemon error"))
            return response.get("result")

    def close(self) -> None:
        try:
            if self._stream is not None:
                self._stream.close()
        except OSError:
            pass
        try:
            if self._sock is not None:
                self._sock.close()
        except OSError:
            pass
        self._sock = None

    def __enter__(self) -> "LiveClient":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()


def call(method: str, params: Optional[dict[str, Any]] = None, **kwargs: Any) -> Any:
    """One-shot convenience: connect, call, disconnect."""
    with LiveClient(kwargs.pop("path", None)) as client:
        return client.call(method, params, **kwargs)


def new_session_id() -> str:
    return uuid.uuid4().hex[:8]
