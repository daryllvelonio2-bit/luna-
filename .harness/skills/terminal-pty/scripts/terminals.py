"""PTY-backed terminal session for driving a live program.

The session owns a real pseudo-terminal, a child process attached to it, and a
`pyte` screen model of everything the child has drawn. What the model holds is
what a user would see: the viewport, the scrollback above it, and the per-cell
colours — which is what makes it possible to verify a workflow the way a person
experiences it rather than by reading the child's raw byte stream.

Nothing here decides policy. It spawns, writes, reads, and reports state; the
daemon and the MCP server above it decide what an agent may ask for.
"""

from __future__ import annotations

import errno
import fcntl
import os
import pty
import re
import signal
import struct
import subprocess
import termios
import threading
import time
from collections import deque
from typing import Any, Iterable, Optional, Sequence

import pyte

DEFAULT_COLS = 120
DEFAULT_ROWS = 34
DEFAULT_SCROLLBACK = 4000
READ_CHUNK = 65536
# Bounded so a chatty child cannot grow the daemon without limit. Only the tail
# is ever searched, and the tail is where a just-printed line lives.
RAW_MAX_BYTES = 2_000_000

ANSI_RE = re.compile(
    r"\x1b\[[0-9;?]*[ -/]*[@-~]"  # CSI
    r"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"  # OSC
    r"|\x1b[PX^_][^\x1b]*\x1b\\"  # DCS/SOS/PM/APC
    r"|\x1b[@-Z\\-_]"  # two-byte escapes
)

# ---------------------------------------------------------------- key names

_SS3 = "\x1bO"
_CSI = "\x1b["

NAMED_KEYS: dict[str, str] = {
    "enter": "\r",
    "return": "\r",
    "cr": "\r",
    "tab": "\t",
    "backtab": _CSI + "Z",
    "shift+tab": _CSI + "Z",
    "esc": "\x1b",
    "escape": "\x1b",
    "space": " ",
    "spc": " ",
    "backspace": "\x7f",
    "bspace": "\x7f",
    "delete": _CSI + "3~",
    "del": _CSI + "3~",
    "insert": _CSI + "2~",
    "ins": _CSI + "2~",
    "up": _CSI + "A",
    "down": _CSI + "B",
    "right": _CSI + "C",
    "left": _CSI + "D",
    "home": _CSI + "H",
    "end": _CSI + "F",
    "pageup": _CSI + "5~",
    "pgup": _CSI + "5~",
    "pagedown": _CSI + "6~",
    "pgdn": _CSI + "6~",
    "shift+up": _CSI + "1;2A",
    "shift+down": _CSI + "1;2B",
    "ctrl+up": _CSI + "1;5A",
    "ctrl+down": _CSI + "1;5B",
    "ctrl+right": _CSI + "1;5C",
    "ctrl+left": _CSI + "1;5D",
    "alt+left": _CSI + "1;3D",
    "alt+right": _CSI + "1;3C",
    "f1": _SS3 + "P",
    "f2": _SS3 + "Q",
    "f3": _SS3 + "R",
    "f4": _SS3 + "S",
    "f5": _CSI + "15~",
    "f6": _CSI + "17~",
    "f7": _CSI + "18~",
    "f8": _CSI + "19~",
    "f9": _CSI + "20~",
    "f10": _CSI + "21~",
    "f11": _CSI + "23~",
    "f12": _CSI + "24~",
}

_CTRL_RE = re.compile(r"ctrl\+(.)", re.IGNORECASE)
_ALT_RE = re.compile(r"alt\+(.+)", re.IGNORECASE)


def _resolve_one(token: str) -> str:
    low = token.lower()
    if low in NAMED_KEYS:
        return NAMED_KEYS[low]
    if len(token) == 1:
        return token
    match = _CTRL_RE.fullmatch(low)
    if match:
        # Ctrl+<letter> is the letter with bits 5 and 6 cleared.
        return chr(ord(match.group(1).upper()) & 0x1F)
    match = _ALT_RE.fullmatch(low)
    if match:
        return "\x1b" + _resolve_one(match.group(1))
    raise ValueError(f"unknown key name: '{token}'")


def resolve_keys(spec: str) -> str:
    """Turn a key spec such as ``Ctrl+C,Up,Enter`` into the bytes to write.

    Accepts comma- or space-separated names, ``char:<literal>`` for a literal
    run, ``Ctrl+<letter>``, ``Alt+<key>``, ``F1``..``F12``, and single
    characters. Unknown names raise rather than silently typing the name out.
    """
    out: list[str] = []
    for token in re.split(r"[,\s]+", spec.strip()):
        if not token:
            continue
        if token.lower().startswith("char:"):
            out.append(token[5:])
            continue
        if token.lower() == "wait":
            continue
        out.append(_resolve_one(token))
    return "".join(out)


# ---------------------------------------------------------------- styling


def cell_sgr(cell: pyte.screens.Char) -> str:
    """Render one cell's attributes as an SGR sequence, or ``""`` for default."""
    codes: list[str] = []
    fg, bg = cell.fg, cell.bg
    if cell.reverse:
        fg, bg = bg, fg
    if cell.bold:
        codes.append("1")
    if cell.italics:
        codes.append("3")
    if cell.underscore:
        codes.append("4")
    if cell.blink:
        codes.append("5")
    if cell.strikethrough:
        codes.append("9")
    if fg != "default":
        codes.append(_sgr_color(fg, background=False))
    if bg != "default":
        codes.append(_sgr_color(bg, background=True))
    return f"\x1b[{';'.join(codes)}m" if codes else ""


def _sgr_color(value: Any, *, background: bool) -> str:
    base = 48 if background else 38
    text = str(value)
    if text.isdigit():
        return f"{base};5;{text}"
    if re.fullmatch(r"[0-9a-fA-F]{6}", text):
        red, green, blue = (int(text[i : i + 2], 16) for i in (0, 2, 4))
        return f"{base};2;{red};{green};{blue}"
    names = ["black", "red", "green", "brown", "blue", "magenta", "cyan", "white"]
    low = text.lower().replace("bright", "")
    if low in names:
        index = names.index(low) + (8 if text.lower().startswith("bright") else 0)
        return f"{base};5;{index}"
    return "39" if not background else "49"


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


# ---------------------------------------------------------------- screen model

# Sequences pyte does not model faithfully, and what to do instead of letting
# them reach it. Both were found by diffing a real TUI's output against the
# screen model: the kitty keyboard protocol's pop/set forms paint literal text
# (`\x1b[<u` drew a stray "u"), and the alternate screen never restores what was
# underneath it.
_INTERESTING = re.compile(
    r"\x1b\[(?P<kitty>[<=][0-9;:]*u)"
    r"|\x1b\[\?(?P<mode>1049|1047|47)(?P<set>h|l)"
)

# A suffix that is still growing into one of the above. A sequence can straddle
# two reads, so the tail is held back rather than fed early — feeding `\x1b[`
# and then `<u` separately would let the "u" through.
_PARTIAL = re.compile(r"\x1b(?:\[(?:\?[0-9]*|[<=]?[0-9;:]*)?)?")
MAX_HOLD = 24


class _ScreenBuffer:
    """One pyte screen and the stream that feeds it."""

    def __init__(self, cols: int, rows: int, scrollback: int) -> None:
        self.screen = pyte.HistoryScreen(cols, rows, history=scrollback)
        self.stream = pyte.Stream(self.screen)

    def resize(self, cols: int, rows: int) -> None:
        self.screen.resize(rows, cols)


class ScreenModel:
    """The terminal as the user sees it, including the alternate screen.

    TUIs put dialogs (approvals, confirmations, pickers) on the alternate
    screen, so a model with only one buffer cannot show a dialog and cannot
    restore the transcript when the dialog closes. Both buffers are kept and
    swapped on the DECSET/DECRST that a real terminal honors.
    """

    def __init__(self, cols: int, rows: int, scrollback: int) -> None:
        self.cols = cols
        self.rows = rows
        self._main = _ScreenBuffer(cols, rows, scrollback)
        self._alt = _ScreenBuffer(cols, rows, scrollback)
        self._in_alt = False
        self._pending = ""

    @property
    def in_alt(self) -> bool:
        return self._in_alt

    @property
    def screen(self) -> pyte.HistoryScreen:
        return self._alt.screen if self._in_alt else self._main.screen

    def feed(self, text: str) -> None:
        for kind, value in self._events(text):
            if kind == "text":
                current = self._alt if self._in_alt else self._main
                current.stream.feed(value)
            elif kind == "alt":
                self._in_alt = bool(value)
                if value:
                    # A fresh alternate screen starts blank, as it does in a
                    # terminal; keeping the last dialog's pixels would show a
                    # frame the user never saw.
                    self._alt = _ScreenBuffer(self.cols, self.rows, self._scrollback)
            else:  # pragma: no cover - defensive
                continue

    @property
    def _scrollback(self) -> int:
        return self._main.screen.history.size or DEFAULT_SCROLLBACK

    def _events(self, text: str) -> list[tuple[str, Any]]:
        data = self._pending + text
        self._pending = ""
        held = self._hold_back(data)
        if held:
            data, self._pending = data[: len(data) - len(held)], held

        events: list[tuple[str, Any]] = []
        cursor = 0
        for match in _INTERESTING.finditer(data):
            if match.start() > cursor:
                events.append(("text", data[cursor : match.start()]))
            if match.group("kitty"):
                pass  # dropped: pyte would paint it as text
            else:
                events.append(("alt", match.group("set") == "h"))
            cursor = match.end()
        if cursor < len(data):
            events.append(("text", data[cursor:]))
        return events

    @staticmethod
    def _hold_back(data: str) -> str:
        """The trailing suffix that may still grow into a modelled sequence."""
        limit = min(len(data), MAX_HOLD)
        for size in range(limit, 0, -1):
            tail = data[len(data) - size :]
            if tail.startswith("\x1b") and _PARTIAL.fullmatch(tail):
                return tail
        return ""

    def resize(self, cols: int, rows: int) -> None:
        self.cols, self.rows = cols, rows
        self._main.resize(cols, rows)
        self._alt.resize(cols, rows)


# ---------------------------------------------------------------- session


class PtySession:
    """One child process on its own pty, with a screen model of its output."""

    def __init__(
        self,
        session_id: str,
        argv: Sequence[str],
        *,
        cwd: Optional[str] = None,
        env: Optional[dict[str, str]] = None,
        cols: int = DEFAULT_COLS,
        rows: int = DEFAULT_ROWS,
        scrollback: int = DEFAULT_SCROLLBACK,
        label: Optional[str] = None,
    ) -> None:
        if not argv:
            raise ValueError("argv must not be empty")
        self.id = session_id
        self.argv = [str(a) for a in argv]
        self.cwd = cwd
        self.label = label or " ".join(self.argv)
        self.cols = int(cols)
        self.rows = int(rows)
        self.created_at = time.time()

        self._env = dict(env or {})
        self._lock = threading.RLock()
        self._model = ScreenModel(self.cols, self.rows, scrollback)
        self._raw: deque[bytes] = deque()
        self._raw_len = 0
        self._last_output_at = time.time()
        self._exited = False
        self._exit_code: Optional[int] = None
        self._closing = False
        self._proc: Optional[subprocess.Popen[bytes]] = None
        self._master: Optional[int] = None
        self._thread: Optional[threading.Thread] = None
        self._started_at: Optional[float] = None

    # -- lifecycle ---------------------------------------------------

    def start(self) -> None:
        master, slave = pty.openpty()
        _set_winsize(slave, self.cols, self.rows)

        env = dict(os.environ)
        env.update(self._env)
        env.setdefault("TERM", "xterm-256color")
        env.setdefault("COLORTERM", "truecolor")
        # A Luna verification run must never stop at its onboarding prompt;
        # other programs ignore the variable.
        if os.path.basename(self.argv[0]) == "luna":
            env.setdefault("LUNA_FIRST_RUN", "false")

        slave_fd = slave

        def _child_setup() -> None:  # pragma: no cover - runs in the forked child
            # setsid makes the child a session leader with no controlling tty;
            # TIOCSCTTY then hands it this pty, which is what makes the child's
            # stdio a real terminal and lets `luna` take its attended path.
            os.setsid()
            try:
                fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)
            except OSError:
                pass

        try:
            self._proc = subprocess.Popen(
                self.argv,
                stdin=slave,
                stdout=slave,
                stderr=slave,
                cwd=self.cwd,
                env=env,
                preexec_fn=_child_setup,
                close_fds=True,
            )
        except FileNotFoundError as exc:
            os.close(master)
            os.close(slave)
            raise FileNotFoundError(f"cannot execute '{self.argv[0]}': {exc}") from exc
        finally:
            try:
                os.close(slave)
            except OSError:
                pass

        self._master = master
        self._started_at = time.time()
        self._thread = threading.Thread(
            target=self._reader, name=f"terminal-pty-{self.id}", daemon=True
        )
        self._thread.start()

    def _reader(self) -> None:
        assert self._master is not None
        fd = self._master
        while True:
            try:
                chunk = os.read(fd, READ_CHUNK)
            except OSError as exc:
                # EIO is what Linux reports once the child side is gone; it is
                # the normal end of a pty read, not a failure.
                if exc.errno in (errno.EIO, errno.EBADF):
                    break
                break
            if not chunk:
                break
            with self._lock:
                self._model.feed(chunk.decode("utf-8", "replace"))
                self._raw.append(chunk)
                self._raw_len += len(chunk)
                while self._raw_len > RAW_MAX_BYTES and len(self._raw) > 1:
                    self._raw_len -= len(self._raw.popleft())
                self._last_output_at = time.time()
        with self._lock:
            self._exited = True

    def close(self, *, grace_secs: float = 3.0) -> dict[str, Any]:
        self._closing = True
        if self._proc is not None and self._proc.poll() is None:
            _signal_group(self._proc.pid, signal.SIGTERM)
            deadline = time.monotonic() + grace_secs
            while time.monotonic() < deadline and self._proc.poll() is None:
                time.sleep(0.05)
            if self._proc.poll() is None:
                _signal_group(self._proc.pid, signal.SIGKILL)
                try:
                    self._proc.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    pass
        if self._thread is not None:
            self._thread.join(timeout=2)
        if self._master is not None:
            try:
                os.close(self._master)
            except OSError:
                pass
            self._master = None
        with self._lock:
            self._exited = True
        return self.info()

    # -- input -------------------------------------------------------

    def write(self, data: str) -> int:
        if self._master is None:
            raise RuntimeError("session has no pty")
        payload = data.encode("utf-8")
        written = os.write(self._master, payload)
        return written

    def send_text(self, text: str, *, enter: bool = False) -> None:
        self.write(text)
        if enter:
            self.write("\r")

    def send_keys(self, spec: str) -> None:
        self.write(resolve_keys(spec))

    def resize(self, cols: int, rows: int) -> None:
        self.cols, self.rows = int(cols), int(rows)
        self._model.resize(self.cols, self.rows)
        if self._master is not None:
            _set_winsize(self._master, self.cols, self.rows)
        if self._proc is not None and self._proc.poll() is None:
            _signal_group(self._proc.pid, signal.SIGWINCH, only_group=True)

    # -- observation -------------------------------------------------

    def viewport(self) -> list[str]:
        with self._lock:
            return [line.rstrip() for line in self._model.screen.display]

    def scrollback(self) -> list[str]:
        with self._lock:
            top = list(self._model.screen.history.top)
        return [_line_text(line, self.cols) for line in top]

    def screen_text(self, *, scrollback: bool = False) -> str:
        view = self.viewport()
        if not scrollback:
            return "\n".join(view).rstrip("\n")
        history = self.scrollback()
        return "\n".join(history + view).strip("\n")

    def screen_ansi(self, *, scrollback: bool = False) -> str:
        with self._lock:
            screen = self._model.screen
            rows = [
                _row_ansi(screen, y, screen.columns) for y in range(screen.lines)
            ]
            if scrollback:
                rows = [
                    _line_ansi(line, screen.columns)
                    for line in list(screen.history.top)
                ] + rows
        return "\n".join(rows).rstrip("\n")

    def cursor(self) -> dict[str, Any]:
        with self._lock:
            screen = self._model.screen
            return {
                "x": screen.cursor.x,
                "y": screen.cursor.y,
                "hidden": bool(screen.cursor.hidden),
                "alternate_screen": self._model.in_alt,
            }

    def snapshot_cells(self) -> list[list[pyte.screens.Char]]:
        """Immutable copy of the viewport, safe to render off-lock."""
        with self._lock:
            screen = self._model.screen
            return [
                [screen.buffer[y][x] for x in range(screen.columns)]
                for y in range(screen.lines)
            ]

    def raw_text(self) -> str:
        with self._lock:
            blob = b"".join(self._raw)
        return strip_ansi(blob.decode("utf-8", "replace"))

    def output_text(
        self, *, scrollback: bool = False, scope: str = "screen"
    ) -> str:
        if scope == "raw":
            return self.raw_text()
        return self.screen_text(scrollback=scrollback)

    def wait_for(
        self,
        pattern: str,
        *,
        timeout: float = 15.0,
        regex: bool = True,
        scope: str = "screen",
        scrollback: bool = True,
        interval: float = 0.05,
    ) -> dict[str, Any]:
        """Poll the rendered screen until ``pattern`` appears.

        Returns a report either way; ``matched`` is the verdict. Matching the
        screen (not the raw byte stream) is deliberate — a user cannot see bytes
        that the terminal swallowed, so a workflow is verified against what was
        actually drawn.
        """
        started = time.monotonic()
        if regex:
            compiled = re.compile(pattern)
            needle: Optional[str] = None
        else:
            compiled = None
            needle = pattern
        deadline = started + timeout
        text = ""
        while True:
            text = self.output_text(scrollback=scrollback, scope=scope)
            if compiled is not None:
                found = compiled.search(text)
                if found is not None:
                    return {
                        "matched": True,
                        "pattern": pattern,
                        "elapsed_ms": int((time.monotonic() - started) * 1000),
                        "line": found.group(0),
                    }
            elif needle is not None and needle in text:
                line = next(
                    (ln for ln in text.splitlines() if needle in ln), needle
                )
                return {
                    "matched": True,
                    "pattern": pattern,
                    "elapsed_ms": int((time.monotonic() - started) * 1000),
                    "line": line,
                }
            if time.monotonic() >= deadline:
                return {
                    "matched": False,
                    "pattern": pattern,
                    "elapsed_ms": int((time.monotonic() - started) * 1000),
                    "tail": _tail(text),
                    "exited": self.exited(),
                    "exit_code": self.exit_code(),
                }
            time.sleep(interval)

    def wait_idle(
        self, *, quiet_ms: int = 400, timeout: float = 20.0
    ) -> dict[str, Any]:
        """Block until the child has produced nothing for ``quiet_ms``.

        The reliable way to wait out a redraw without guessing a sleep: a TUI
        paints in bursts, and a fixed delay either wastes time or photographs a
        half-drawn frame.
        """
        started = time.monotonic()
        deadline = started + timeout
        quiet = quiet_ms / 1000.0
        while True:
            with self._lock:
                idle_for = time.time() - self._last_output_at
                exited = self._exited
            if idle_for >= quiet or exited:
                return {
                    "idle": True,
                    "quiet_ms": int(idle_for * 1000),
                    "elapsed_ms": int((time.monotonic() - started) * 1000),
                    "exited": self.exited(),
                }
            if time.monotonic() >= deadline:
                return {
                    "idle": False,
                    "quiet_ms": int(idle_for * 1000),
                    "elapsed_ms": int((time.monotonic() - started) * 1000),
                    "exited": self.exited(),
                }
            time.sleep(0.02)

    def settle(self, ms: int = 150) -> None:
        time.sleep(max(0, ms) / 1000.0)

    # -- state -------------------------------------------------------

    def exited(self) -> bool:
        if self._proc is not None and self._proc.poll() is not None:
            with self._lock:
                self._exited = True
            return True
        with self._lock:
            return self._exited

    def exit_code(self) -> Optional[int]:
        if self._proc is None:
            return self._exit_code
        code = self._proc.poll()
        if code is not None:
            self._exit_code = code
        return self._exit_code

    def info(self) -> dict[str, Any]:
        return {
            "session_id": self.id,
            "label": self.label,
            "argv": self.argv,
            "cwd": self.cwd,
            "pid": self._proc.pid if self._proc is not None else None,
            "cols": self.cols,
            "rows": self.rows,
            "running": not self.exited(),
            "exit_code": self.exit_code(),
            "age_secs": round(time.time() - self.created_at, 2),
        }


# ---------------------------------------------------------------- helpers


def _set_winsize(fd: int, cols: int, rows: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def _signal_group(pid: int, sig: int, *, only_group: bool = False) -> None:
    try:
        os.killpg(os.getpgid(pid), sig)
    except (ProcessLookupError, PermissionError, OSError):
        if not only_group:
            try:
                os.kill(pid, sig)
            except OSError:
                pass


def _line_text(line: Any, cols: int) -> str:
    return "".join(line[x].data for x in range(cols)).rstrip()


def _row_ansi(screen: pyte.HistoryScreen, y: int, cols: int) -> str:
    return _cells_ansi([screen.buffer[y][x] for x in range(cols)])


def _line_ansi(line: Any, cols: int) -> str:
    return _cells_ansi([line[x] for x in range(cols)])


def _cells_ansi(cells: Iterable[pyte.screens.Char]) -> str:
    parts: list[str] = []
    current = ""
    for cell in cells:
        style = cell_sgr(cell)
        if style != current:
            parts.append("\x1b[0m" if not style else style)
            current = style
        parts.append(cell.data)
    parts.append("\x1b[0m")
    return "".join(parts).rstrip()


def _tail(text: str, lines: int = 25) -> str:
    return "\n".join(text.splitlines()[-lines:])
