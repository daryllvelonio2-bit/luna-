use colored::Colorize;

/// The composer prompt. The `User >` label is gone, so Luna's crescent marks the input line
/// instead. U+263E LAST QUARTER MOON is a plain text glyph — terminals keep it
/// black-and-white, unlike the colour crescent U+1F319 which renders as an emoji.
pub const PROMPT: &str = "\u{263E} ";

/// The moon drawn above the composer when a session has no transcript yet.
///
/// The crescent is deliberately made of block glyphs rather than ASCII punctuation: a fresh
/// session is the one screen with nothing else on it, and the denser shape reads as a mark
/// instead of stray characters. Quadrant blocks give the concave edge sub-cell steps, so the
/// curve stays smooth instead of turning into the square notch a half-block crescent leaves.
/// It is deliberately compact — thirteen cells wide and ten rows tall — so the mark hugs the
/// left margin of a wide terminal instead of filling it.
pub(crate) const LANDING_MOON: [&str; 10] = [
    "   ▗▄▟█████▀▀",
    " ▗▟█████▀▘",
    "▗█████▛▘",
    "██████",
    "█████▌",
    "█████▙",
    "▜█████▖",
    " ▜█████▙",
    "  ▝▜█████▙▄",
    "     ▝▀▀▀▀▀▀▀",
];

/// The wordmark that sits beside the moon on a wide terminal.
pub(crate) const LANDING_TITLE: &str = "L U N A";

/// Column the title starts at, measured from the left edge of the moon block.
pub(crate) const LANDING_TEXT_COL: usize = 17;

/// Cells kept empty between the left edge of the terminal and the mark.
pub(crate) const LANDING_LEFT_MARGIN: usize = 2;

/// Narrowest terminal that can hold the moon and its wordmark side by side.
///
/// One column wider than the lockup itself on purpose: a row that fills the terminal exactly sits
/// in the pending-wrap state, and the next newline moves twice. The animation moves the cursor by
/// the block's height, so a wrapped row would desynchronise every frame after it.
pub(crate) const LANDING_FULL_MIN_WIDTH: usize =
    LANDING_LEFT_MARGIN + LANDING_TEXT_COL + LANDING_TITLE.len() + 1;

/// Narrowest terminal that can hold the stacked moon and still leave the last column clear.
pub(crate) const LANDING_MOON_MIN_WIDTH: usize = LANDING_TEXT_COL + 1;

/// Narrower than this, even the compact wordmark does not fit with a margin.
pub(crate) const LANDING_COMPACT_MIN_WIDTH: usize = 12;

/// Return the rows of the fresh-session mark, or an empty vector when the terminal is too
/// small to say anything.
///
/// A brand-new session has no transcript to anchor the screen, so the first frame is the only
/// chance to show what the user has opened. This is printed once above the composer (not kept
/// as chrome), so it scrolls away with the first turn rather than being redrawn on every paint.
/// Two layouts are available so a narrow or short terminal still gets a recognisable mark:
/// the full moon-and-wordmark lockup and a stacked moon. Every layout is left-aligned, so the
/// mark grows out of the same margin Luna's replies use instead of floating in the far corner of
/// a wide terminal.
pub fn landing_rows(width: usize, available_height: usize) -> Vec<String> {
    if available_height == 0 {
        return Vec::new();
    }
    if width >= LANDING_FULL_MIN_WIDTH && available_height >= LANDING_MOON.len() {
        return left_align_rows(full_landing_rows(), width);
    }
    if width >= LANDING_MOON_MIN_WIDTH && available_height > LANDING_MOON.len() {
        return left_align_rows(stacked_landing_rows(), width);
    }
    if width >= LANDING_COMPACT_MIN_WIDTH {
        return left_align_rows(
            vec![format!("{}  {}", "☾".cyan().bold(), LANDING_TITLE.bold())],
            width,
        );
    }
    Vec::new()
}

/// The wide lockup: the moon on the left, the wordmark on its right.
pub(crate) fn full_landing_rows() -> Vec<String> {
    LANDING_MOON
        .iter()
        .enumerate()
        .map(|(index, art)| {
            let text = match index {
                1 => Some(LANDING_TITLE.cyan().bold().to_string()),
                _ => None,
            };
            landing_row(art, text.as_deref())
        })
        .collect()
}

/// The narrow lockup: the moon with its name underneath, stacked as one block.
pub(crate) fn stacked_landing_rows() -> Vec<String> {
    let mut rows: Vec<String> = LANDING_MOON
        .iter()
        .map(|art| art.white().bold().to_string())
        .collect();
    rows.push(LANDING_TITLE.cyan().bold().to_string());
    rows
}

/// One row of the wide lockup: the moon in place, then the text at its own column.
pub(crate) fn landing_row(art: &str, text: Option<&str>) -> String {
    let art = art.trim_end();
    let mut row = art.white().bold().to_string();
    if let Some(text) = text {
        let pad = LANDING_TEXT_COL.saturating_sub(dialoguer::console::measure_text_width(art));
        row.push_str(&" ".repeat(pad));
        row.push_str(text);
    }
    row
}

/// Visible width of a row, ignoring its colour codes.
pub(crate) fn visible_width(row: &str) -> usize {
    dialoguer::console::measure_text_width(&dialoguer::console::strip_ansi_codes(row))
}

/// Left-align every row of a block on the terminal margin, using the block's widest row so the
/// pieces keep their relative alignment.
///
/// The margin shrinks on a terminal too narrow to hold the block and the full margin, so the
/// narrow layouts still fit instead of spilling a column off the edge. Where there is room it
/// also keeps one cell clear on the right: a row that fills the terminal sits in the pending-wrap
/// state, and the animation walks the cursor back up by row count, so a wrapped row would
/// desynchronise every frame after it.
pub(crate) fn left_align_rows(rows: Vec<String>, width: usize) -> Vec<String> {
    let block_width = rows.iter().map(|row| visible_width(row)).max().unwrap_or(0);
    let spare = width.saturating_sub(block_width);
    let margin = LANDING_LEFT_MARGIN.min(spare.saturating_sub(1));
    rows.into_iter()
        .map(|row| format!("{}{}", " ".repeat(margin), row))
        .collect()
}

/// The glyph a falling star draws: U+2726 BLACK FOUR-POINTED STAR.
///
/// A plain text star, not an emoji, for the same reason the moon is block glyphs: the mark should
/// keep the terminal's own font all the way through rather than pulling in a colour emoji.
pub const LANDING_STAR: &str = "\u{2726}";

/// The small dot a meteor leaves behind it. U+00B7 is one cell in every terminal font that draws
/// the rest of the mark, and it reads as distance rather than as another falling star.
pub const LANDING_TRAIL: &str = "\u{00b7}";

/// Falling stars in one frame. Nine read as a shower — enough to feel alive, few enough that the
/// moon still reads as the subject rather than vanishing under a starfield.
pub const LANDING_STARS: usize = 9;

/// Cells of trail behind each falling star. Two is the shortest tail that reads as motion at the
/// animation tick instead of a second star travelling in formation.
pub const LANDING_TRAIL_CELLS: usize = 2;

/// Frames in one full loop of the shower. At the animation tick this is a little under three
/// seconds, and the shower keeps looping for as long as the fresh session stays on screen.
pub const LANDING_FALL_FRAMES: usize = 72;

/// How much faster (or slower) each successive star crosses the block. Different speeds pull the
/// shower apart over time, so the stars do not stay locked in one rigid diagonal.
pub(crate) const LANDING_STAR_SPEEDS: [f64; 5] = [0.65, 0.95, 0.75, 1.1, 0.85];

/// The mark with the shower overlaid at one point of its fall.
///
/// The stars enter at the top-right of the block and drop diagonally toward the lower-left, so
/// the eye follows a meteor across the moon instead of a cursor blinking inside it. Each star has
/// its own phase, speed and lane, which is what turns the group into a shower rather than one
/// head with a tail.
///
/// A meteor passes behind the wordmark instead of through it: a cell is only replaced when it is
/// blank or part of the moon, so the title keeps every letter. The eye still reads the path
/// because the star reappears below the text.
///
/// Every frame replaces characters rather than inserting them, which keeps every row exactly as
/// wide as the static mark: the caller rewrites the block in place, and a row that changed width
/// would leave the previous frame's tail behind it.
///
/// ANSI sequences in `rows` are copied through untouched, so each star takes on the colour of
/// whatever it is passing over — a white cut-out on the moon, cyan on the wordmark — and the rest
/// of the row keeps its own styling.
pub fn landing_star_rows(rows: &[String], frame: usize) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }

    // Every row is padded to the block's width first: then a star always replaces a cell instead
    // of being appended, and every frame is the same width as the last one.
    let block_width = rows.iter().map(|row| visible_width(row)).max().unwrap_or(0);
    if block_width == 0 {
        return rows.to_vec();
    }

    // `&str` glyphs keep the heads and trails in one table so a meteor and a trail that land on
    // the same cell are deduplicated instead of overwriting one another.
    let mut slots: Vec<Vec<(usize, &'static str)>> = vec![Vec::new(); rows.len()];
    let stars = landing_star_positions(rows, frame);
    // Every head goes in before any trail. The layers are deduplicated by insertion order, and a
    // trail from an earlier star must not claim the cell a later star's head is about to occupy.
    for &(row, col) in &stars {
        let Some(row_text) = rows.get(row) else {
            continue;
        };
        // The star only lands on sky or on the moon. A letter is never replaced: the meteor
        // passes behind the wordmark and comes back out underneath it.
        if can_host_star(row_text, col) {
            slots[row].push((col, LANDING_STAR));
        }
    }
    for &(row, col) in &stars {
        let Some(row_text) = rows.get(row) else {
            continue;
        };
        if !can_host_star(row_text, col) {
            continue;
        }
        for step in 1..=LANDING_TRAIL_CELLS {
            let Some(trail_row) = row.checked_sub(step) else {
                break;
            };
            let trail_col = col.saturating_add(step);
            let Some(trail_text) = rows.get(trail_row) else {
                break;
            };
            if trail_col < block_width && can_host_star(trail_text, trail_col) {
                slots[trail_row].push((trail_col, LANDING_TRAIL));
            }
        }
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let mut row = row.clone();
            let width = visible_width(&row);
            row.push_str(&" ".repeat(block_width.saturating_sub(width)));

            let cells = &mut slots[index];
            // Stable sort: a meteor head is pushed before any trail, so it wins a cell two
            // stars both picked.
            cells.sort_by_key(|(col, _)| *col);
            cells.dedup_by_key(|(col, _)| *col);
            if cells.is_empty() {
                row
            } else {
                overlay_glyphs(&row, cells)
            }
        })
        .collect()
}

/// Where each falling star sits on `frame`, as `(row, visible column)` pairs.
///
/// Each star is a point on its own diagonal: `phase` spreads them along the fall, `speed` pulls
/// them apart over time, and `lane` shifts the whole track left so they do not all ride the same
/// line. `fract` sends a star that reaches the bottom back to the top-right, so the shower keeps
/// going for the whole loop instead of emptying out halfway through.
///
/// Exposed within the module so the tests can reason about individual tracks instead of scraping
/// glyphs back out of the rendered rows.
pub fn landing_star_positions(rows: &[String], frame: usize) -> Vec<(usize, usize)> {
    if rows.is_empty() {
        return Vec::new();
    }
    let last_row = rows.len() - 1;
    let block_width = rows.iter().map(|row| visible_width(row)).max().unwrap_or(0);
    if block_width == 0 {
        return Vec::new();
    }

    let cycle = LANDING_FALL_FRAMES.max(1);
    let progress = (frame % cycle) as f64 / cycle as f64;
    let base_start = block_width.saturating_sub(2);

    (0..LANDING_STARS)
        .map(|star| {
            let phase = star as f64 / LANDING_STARS as f64;
            let speed = LANDING_STAR_SPEEDS[star % LANDING_STAR_SPEEDS.len()];
            let t = (progress * speed + phase).fract();

            // Four lanes, two cells apart, each ending a little earlier than the one before it.
            let lane = (star % 4) * 2;
            let start_col = base_start.saturating_sub(lane);
            let end_col = (block_width / 4).max(1).saturating_sub(lane / 2).max(1);
            let span = start_col.saturating_sub(end_col) as f64;

            let row = (t * last_row as f64).round() as usize;
            let col = start_col.saturating_sub((span * t) as usize);
            (row.min(last_row), col)
        })
        .collect()
}

/// Whether the glyph at visible column `col` is the moon's body, which a star may cut into.
pub(crate) fn is_moon_block(ch: char) -> bool {
    ('\u{2580}'..='\u{259f}').contains(&ch)
}

/// Whether a star may replace the cell at `col` without destroying a letter.
///
/// Blank cells are sky, and block glyphs are the moon: a meteor cut-out there reads as the star
/// passing in front of the body. Everything else — the wordmark, punctuation — is left alone, so
/// the star passes behind the text and reappears below it. A column past the end of the row is
/// padded sky.
pub(crate) fn can_host_star(row: &str, col: usize) -> bool {
    let mut visible = 0usize;
    let mut chars = row.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }

        let width = dialoguer::console::measure_text_width(&ch.to_string()).max(1);
        if visible == col {
            if is_moon_block(ch) {
                return true;
            }
            if ch != ' ' {
                return false;
            }
            // A single space inside the wordmark is not open sky: a star there reads as
            // punctuation between two letters. The wide gaps before and after the text are.
            let blanks = blank_columns(row);
            let row_width = visible_width(row);
            let left_filled = col > 0 && !blanks.contains(&(col - 1));
            let right_blank = col + 1 < row_width && blanks.contains(&(col + 1));
            return !(left_filled && !right_blank);
        }
        if col < visible.saturating_add(width) {
            // The column lands inside a double-width glyph, so the whole cell is taken.
            return false;
        }
        visible = visible.saturating_add(width);
    }
    true
}

/// The visible columns of `row` that hold a space, ignoring ANSI sequences.
pub(crate) fn blank_columns(row: &str) -> Vec<usize> {
    let mut columns = Vec::new();
    let mut column = 0usize;
    let mut chars = row.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        let width = dialoguer::console::measure_text_width(&ch.to_string()).max(1);
        if ch == ' ' {
            columns.push(column);
        }
        column += width;
    }
    columns
}

/// Replace the cells at the visible columns in `cells` with their glyphs, leaving ANSI sequences
/// in place so each glyph takes on the styling of the text around it.
///
/// On a block glyph the cell is drawn in reverse video: filling the cell with the moon's own
/// colour turns the star into a cut-out instead of a black square punched through the mark. On
/// text and empty cells the plain glyph is invisible enough against the terminal background.
///
/// Widths are counted in terminal cells rather than bytes: the moon is block glyphs, which are
/// several bytes each, and slicing by byte would land in the middle of one. `cells` is sorted and
/// deduplicated by the caller; an out-of-range column appends rather than panics so a caller that
/// miscalculates still draws something.
pub(crate) fn overlay_glyphs(row: &str, cells: &[(usize, &str)]) -> String {
    let mut out = String::with_capacity(row.len() + 2 * cells.len());
    let mut visible = 0usize;
    let mut next = 0usize;
    let mut chars = row.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            out.push(ch);
            // Only CSI sequences reach these rows; copy one whole so its digits are never
            // mistaken for visible characters.
            if chars.peek() == Some(&'[') {
                out.push(chars.next().unwrap_or('['));
                for c in chars.by_ref() {
                    out.push(c);
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }

        let width = dialoguer::console::measure_text_width(&ch.to_string()).max(1);
        if let Some((_, glyph)) = cells.get(next).filter(|(col, _)| *col == visible) {
            // A block is part of the moon's body, so keep its cell filled and let the star read
            // as a cut-out. Anything else sits on the terminal background already.
            if is_moon_block(ch) {
                out.push_str("\x1b[7m");
                out.push_str(glyph);
                out.push_str("\x1b[27m");
            } else {
                out.push_str(glyph);
            }
            // A double-width glyph we replaced still occupies two cells: pad the second one so
            // the row keeps its visible width.
            if width > 1 {
                out.push_str(&" ".repeat(width - 1));
            }
            next += 1;
            visible += width;
            // The character under the star is dropped: this is a replacement, not an insertion.
            continue;
        }

        out.push(ch);
        visible += width;
    }

    for (_, glyph) in &cells[next..] {
        out.push_str(glyph);
    }
    out
}

/// Replace the cells at `cols` with [`LANDING_STAR`]. Kept as the narrow entry point for tests
/// that only need the falling layer.
#[cfg(test)]
pub(crate) fn overlay_stars(row: &str, cols: &[usize]) -> String {
    let cells: Vec<(usize, &str)> = cols.iter().map(|&col| (col, LANDING_STAR)).collect();
    overlay_glyphs(row, &cells)
}
