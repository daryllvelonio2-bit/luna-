"""Render a `pyte` screen model to a PNG that looks like the user's terminal.

A screenshot of a terminal is not a screenshot of a window: there is no
compositor to ask, and the child writes escape sequences rather than pixels.
So the pixels are drawn here from the screen model — the same characters, the
same true-colour foregrounds and backgrounds, the same box-drawing and Nerd
Font glyphs a real terminal would show.

The font is a monospaced Nerd Font when present, because `luna` draws its
panels with box-drawing characters and its status line with powerline glyphs:
a plain proportional font would render those frames as a wall of tofu.
"""

from __future__ import annotations

import hashlib
import os
import re
from typing import Any, Iterable, Optional, Sequence

from PIL import Image, ImageDraw, ImageFont

DEFAULT_FG = (208, 208, 208)
DEFAULT_BG = (24, 24, 24)
CAPTION_FG = (150, 150, 150)
CAPTION_BG = (38, 38, 38)

# Tango-ish defaults: close to what a stock xterm-256color palette gives, so
# the image reads the way the terminal the user runs `luna` in reads.
PALETTE: dict[str, tuple[int, int, int]] = {
    "black": (0, 0, 0),
    "red": (204, 0, 0),
    "green": (78, 154, 6),
    "brown": (196, 160, 0),
    "yellow": (196, 160, 0),
    "blue": (52, 101, 164),
    "magenta": (117, 80, 123),
    "cyan": (6, 152, 154),
    "white": (211, 215, 207),
    "grey": (136, 136, 136),
    "gray": (136, 136, 136),
    "brightblack": (85, 87, 83),
    "brightred": (239, 41, 41),
    "brightgreen": (138, 226, 52),
    "brightbrown": (252, 233, 79),
    "brightyellow": (252, 233, 79),
    "brightblue": (114, 159, 207),
    "brightmagenta": (173, 127, 168),
    "brightcyan": (52, 226, 226),
    "brightwhite": (238, 238, 236),
}

_BASIC_16: tuple[tuple[int, int, int], ...] = (
    PALETTE["black"],
    PALETTE["red"],
    PALETTE["green"],
    PALETTE["brown"],
    PALETTE["blue"],
    PALETTE["magenta"],
    PALETTE["cyan"],
    PALETTE["white"],
    PALETTE["brightblack"],
    PALETTE["brightred"],
    PALETTE["brightgreen"],
    PALETTE["brightbrown"],
    PALETTE["brightblue"],
    PALETTE["brightmagenta"],
    PALETTE["brightcyan"],
    PALETTE["brightwhite"],
)

# JetBrains Mono Nerd Font first: it carries both the box-drawing set and the
# powerline separators `luna` uses in its status bar.
FONT_CANDIDATES: tuple[str, ...] = (
    "/usr/share/fonts/TTF/JetBrainsMonoNerdFontMono-Regular.ttf",
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMonoNL-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/Adwaita/AdwaitaMono-Regular.ttf",
    "/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf",
)

# Consulted in order for glyphs the primary font lacks: Adwaita Mono carries
# the media/verdict marks (⏵ ⏸ ⏩ ✔ ✖ ⏺) at a monospaced advance, and
# Noto Sans Symbols 2 covers what neither mono face has (📷 and friends).
FALLBACK_CHAIN: tuple[tuple[str, Optional[str]], ...] = (
    (
        "/usr/share/fonts/Adwaita/AdwaitaMono-Regular.ttf",
        "/usr/share/fonts/Adwaita/AdwaitaMono-Bold.ttf",
    ),
    (
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        None,
    ),
    (
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
    ),
    (
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    ),
)


class FontUnavailable(RuntimeError):
    """No monospaced font could be found to draw the frame with."""


def find_font_path(*, bold: bool = False) -> Optional[str]:
    candidates: list[str] = []
    for path in FONT_CANDIDATES:
        candidates.append(path)
        if bold:
            candidates.append(path.replace("-Regular", "-Bold"))
    for path in candidates:
        if os.path.exists(path):
            return path
    return None


def _load_font(size: int, *, bold: bool = False) -> ImageFont.FreeTypeFont:
    path = find_font_path(bold=bold)
    if path is None:
        raise FontUnavailable(
            "no monospaced TrueType font found; install fonts-dejavu or a "
            "Nerd Font, or pass font_path"
        )
    return ImageFont.truetype(path, size)


def _xterm256(index: int) -> tuple[int, int, int]:
    if index < 0:
        return DEFAULT_FG
    if index < 16:
        return _BASIC_16[index]
    if index < 232:
        index -= 16
        scale = lambda value: 0 if value == 0 else 55 + value * 40  # noqa: E731
        return (
            scale(index // 36),
            scale((index % 36) // 6),
            scale(index % 6),
        )
    level = 8 + (index - 232) * 10
    return (level, level, level)


def resolve_color(value: Any, default: tuple[int, int, int]) -> tuple[int, int, int]:
    """Map a pyte colour (name, palette index, or ``rrggbb``) to RGB."""
    if value is None:
        return default
    text = str(value).strip()
    if not text or text == "default":
        return default
    low = text.lower()
    if low in PALETTE:
        return PALETTE[low]
    if re.fullmatch(r"[0-9a-f]{6}", low):
        return tuple(int(low[i : i + 2], 16) for i in (0, 2, 4))  # type: ignore[return-value]
    if low.isdigit():
        return _xterm256(int(low))
    # A few terminals spell variants with separators ('bright_red', 'bright-red').
    squashed = low.replace("_", "").replace("-", "").replace(" ", "")
    if squashed in PALETTE:
        return PALETTE[squashed]
    if squashed.startswith("bright") and squashed[6:] in PALETTE:
        return PALETTE["bright" + squashed[6:]]
    return default


def _style_of(cell: Any) -> tuple[tuple[int, int, int], tuple[int, int, int], bool]:
    fg_raw, bg_raw = getattr(cell, "fg", "default"), getattr(cell, "bg", "default")
    if getattr(cell, "reverse", False):
        fg_raw, bg_raw = bg_raw, fg_raw
    foreground = resolve_color(fg_raw, DEFAULT_FG)
    background = resolve_color(bg_raw, DEFAULT_BG)
    # Bold is drawn as a brighter foreground: without a real bright palette the
    # emphasis would otherwise be lost entirely.
    if getattr(cell, "bold", False) and foreground == DEFAULT_FG:
        foreground = (255, 255, 255)
    return foreground, background, bool(getattr(cell, "bold", False))


class FontSet:
    """A font chain that resolves each glyph to the first font that has it.

    `luna` draws its frames with a mixed glyph vocabulary — box drawing and
    powerline from the Nerd Font, media glyphs (⏵ ⏸ ⏩) and verdict marks
    (✔ ✖) that the Nerd Font simply does not carry. A real terminal falls back
    to another font for those; a renderer that does not prints tofu. So
    coverage is probed per glyph (a missing glyph renders as the same .notdef
    box as any other missing glyph) and the first covering font wins.
    """

    def __init__(
        self,
        size: int,
        *,
        font_path: Optional[str] = None,
        bold: bool = False,
    ) -> None:
        paths: list[str] = []
        if font_path:
            paths.append(font_path)
        else:
            primary = find_font_path(bold=bold) or find_font_path(bold=False)
            if primary:
                paths.append(primary)
            for regular, bold_variant in FALLBACK_CHAIN:
                candidate = (bold_variant if bold else None) or regular
                if os.path.exists(candidate) and candidate not in paths:
                    paths.append(candidate)
        if not paths:
            raise FontUnavailable("no usable TrueType font found")
        self.paths = paths
        self._fonts = [ImageFont.truetype(p, size) for p in paths]
        self._notdef = [self._signature(i, "\ue123") for i in range(len(paths))]
        self._coverage: dict[tuple[int, str], bool] = {}

    def _signature(self, index: int, char: str) -> bytes:
        image = Image.new("L", (24, 28), 0)
        ImageDraw.Draw(image).text((0, 0), char, font=self._fonts[index], fill=255)
        return image.tobytes()

    def has(self, index: int, char: str) -> bool:
        key = (index, char)
        if key not in self._coverage:
            self._coverage[key] = (
                self._signature(index, char) != self._notdef[index]
            )
        return self._coverage[key]

    def index_for(self, char: str) -> int:
        for index in range(len(self._fonts)):
            if self.has(index, char):
                return index
        return 0

    def font(self, index: int) -> ImageFont.FreeTypeFont:
        return self._fonts[index]

    @property
    def primary(self) -> ImageFont.FreeTypeFont:
        return self._fonts[0]


def render_png(
    cells: Sequence[Sequence[Any]],
    path: str,
    *,
    font_size: int = 15,
    padding: int = 12,
    cursor: Optional[dict[str, Any]] = None,
    caption: Optional[str] = None,
    font_path: Optional[str] = None,
) -> dict[str, Any]:
    """Draw ``cells`` (rows of pyte ``Char``) to a PNG at ``path``."""
    rows = len(cells)
    cols = max((len(row) for row in cells), default=0)
    if rows == 0 or cols == 0:
        raise ValueError("nothing to render: empty screen")

    fonts = FontSet(font_size, font_path=font_path, bold=False)
    bold_fonts = fonts if font_path else FontSet(font_size, font_path=None, bold=True)
    regular = fonts.primary

    ascent, descent = regular.getmetrics()
    line_gap = max(1, font_size // 6)
    cell_h = ascent + descent + line_gap
    cell_w = max(1, int(round(regular.getlength("M"))))
    text_offset_y = (cell_h - (ascent + descent)) // 2

    def glyph_font(char: str, is_bold: bool) -> ImageFont.FreeTypeFont:
        chain = bold_fonts if is_bold else fonts
        return chain.font(chain.index_for(char))

    caption_h = (font_size + 8) if caption else 0
    width = padding * 2 + cols * cell_w
    height = padding * 2 + caption_h + rows * cell_h

    image = Image.new("RGB", (width, height), DEFAULT_BG)
    draw = ImageDraw.Draw(image)

    if caption:
        draw.rectangle([(0, 0), (width, caption_h)], fill=CAPTION_BG)
        draw.text(
            (padding, max(2, (caption_h - font_size) // 2)),
            caption,
            font=regular,
            fill=CAPTION_FG,
        )

    cursor_x = cursor_y = -1
    if cursor and not cursor.get("hidden", False):
        cursor_x, cursor_y = int(cursor.get("x", -1)), int(cursor.get("y", -1))

    top = padding + caption_h
    for y, row in enumerate(cells):
        baseline_y = top + y * cell_h
        x = 0
        while x < cols:
            cell = row[x] if x < len(row) else None
            if cell is None:
                x += 1
                continue
            fg, bg, is_bold = _style_of(cell)
            char = getattr(cell, "data", " ") or " "

            is_cursor = y == cursor_y and x == cursor_x
            if is_cursor:
                # An inverted block is what a real terminal shows for the caret.
                fg, bg = bg, fg
                char = char if char.strip() else " "
            font = glyph_font(char, is_bold)

            # Group the run of cells that share this exact style and font. The
            # font is part of the key: a run drawn in two faces at one advance
            # each would still land on the wrong columns.
            run_chars = [char]
            x_end = x + 1
            while x_end < cols:
                nxt = row[x_end] if x_end < len(row) else None
                if nxt is None:
                    break
                nfg, nbg, nbold = _style_of(nxt)
                if (nfg, nbg, nbold) != (fg, bg, is_bold):
                    break
                if x_end == cursor_x and y == cursor_y:
                    break
                nchar = getattr(nxt, "data", " ") or " "
                if glyph_font(nchar, nbold) is not font:
                    break
                run_chars.append(nchar)
                x_end += 1

            run_text = "".join(run_chars)
            run_x = padding + x * cell_w
            run_w = (x_end - x) * cell_w
            if bg != DEFAULT_BG or is_cursor:
                draw.rectangle(
                    [(run_x, baseline_y), (run_x + run_w - 1, baseline_y + cell_h - 1)],
                    fill=bg,
                )
            if run_text.strip():
                draw.text(
                    (run_x, baseline_y + text_offset_y),
                    run_text,
                    font=font,
                    fill=fg,
                )
            x = x_end

    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    image.save(path, "PNG", optimize=True)
    with open(path, "rb") as handle:
        digest = hashlib.sha256(handle.read()).hexdigest()

    return {
        "path": os.path.abspath(path),
        "width": width,
        "height": height,
        "cols": cols,
        "rows": rows,
        "cell_w": cell_w,
        "cell_h": cell_h,
        "font": font_path or (fonts.paths[0] if fonts.paths else "default"),
        "font_chain": list(fonts.paths),
        "bytes": os.path.getsize(path),
        "sha256": digest,
    }


def render_lines_png(
    lines: Iterable[str], path: str, **kwargs: Any
) -> dict[str, Any]:
    """Convenience wrapper that draws plain text (no styling)."""
    from collections import namedtuple

    Plain = namedtuple("Plain", "data fg bg bold reverse italics underscore blink strikethrough")
    rows = [
        [Plain(ch, "default", "default", False, False, False, False, False, False) for ch in line]
        for line in lines
    ]
    return render_png(rows, path, **kwargs)
