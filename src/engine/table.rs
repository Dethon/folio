//! Finds tables on a page.
//!
//! A table is only recognised when it is *drawn*: either a lattice of
//! horizontal and vertical rules, or a set of horizontal rules of equal width
//! (the top, middle and bottom rules of a typeset table). The rules say where
//! the table is; the text inside says how many columns it has, by the white
//! rivers that run between cells from row to row.
//!
//! Text that merely lines up, with nothing drawn around it, is left alone. It
//! cannot be told apart from columns of prose or an aligned list with any
//! confidence, and a wrong table is worse than plain paragraphs.

use super::markdown::{Span, TextLine, Word};
use super::text::MONO;

/// A straight ruling line in view space (points, origin top-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    pub horizontal: bool,
    /// The fixed coordinate: `y` for a horizontal rule, `x` for a vertical.
    pub at: f32,
    pub from: f32,
    pub to: f32,
}

impl Rule {
    fn length(&self) -> f32 {
        self.to - self.from
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    /// Indices of the page's lines that belong to the table, ascending.
    pub lines: Vec<usize>,
    /// The first row is the header.
    pub rows: Vec<Vec<Vec<Span>>>,
}

/// Rules closer than this are the same rule drawn twice (adjoining cells
/// each draw their own border).
const SAME: f32 = 1.6;
/// Slack when deciding whether rules touch or text lies inside a region.
const TOUCH: f32 = 2.5;
const MAX_COLUMNS: usize = 24;

pub fn detect(lines: &[TextLine], rules: &[Rule], skip: &dyn Fn(usize) -> bool) -> Vec<Table> {
    if rules.len() < 2 {
        return Vec::new();
    }

    let horizontal = merge(rules.iter().filter(|rule| rule.horizontal).copied());
    let vertical = merge(rules.iter().filter(|rule| !rule.horizontal).copied());
    // Before merging, vertical rules are still one piece per row.
    let pieces: Vec<Rule> = rules
        .iter()
        .filter(|rule| !rule.horizontal)
        .copied()
        .collect();

    let mut tables: Vec<Table> = Vec::new();
    let mut used = vec![false; horizontal.len()];

    for region in lattices(&horizontal, &vertical, &pieces, &mut used) {
        tables.extend(extract(lines, skip, &region));
    }

    // What is left are rules with nothing vertical attached: group those of
    // equal extent and look for the largest run that encloses a table.
    let mut loose: Vec<Rule> = horizontal
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(rule, _)| *rule)
        .collect();

    loose.sort_by(|a, b| a.from.total_cmp(&b.from).then(a.at.total_cmp(&b.at)));

    while let Some(first) = loose.first().copied() {
        let same_extent = |rule: &Rule| {
            (rule.from - first.from).abs() <= TOUCH && (rule.to - first.to).abs() <= TOUCH
        };
        let mut group: Vec<Rule> = loose.iter().copied().filter(same_extent).collect();

        loose.retain(|rule| !same_extent(rule));
        group.sort_by(|a, b| a.at.total_cmp(&b.at));

        ruled(lines, skip, &group, &mut tables);
    }

    tables.retain(|table| !table.lines.is_empty());
    tables.sort_by_key(|table| table.lines[0]);

    // A line belongs to one table at most.
    let mut seen = std::collections::HashSet::new();
    tables.retain(|table| table.lines.iter().all(|line| seen.insert(*line)));

    tables
}

/// Collapses duplicates and joins collinear segments that touch.
fn merge(rules: impl Iterator<Item = Rule>) -> Vec<Rule> {
    let mut rules: Vec<Rule> = rules.collect();
    rules.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.from.total_cmp(&b.from)));

    let mut merged: Vec<Rule> = Vec::new();
    let mut level_start = 0;

    for rule in rules {
        // Rules within `SAME` of the first rule of a level share that level.
        if merged
            .get(level_start)
            .is_none_or(|first| rule.at - first.at > SAME)
        {
            level_start = merged.len();
            merged.push(rule);
            continue;
        }

        let at = merged[level_start].at;

        match merged[level_start..]
            .iter_mut()
            .find(|other| rule.from <= other.to + TOUCH && rule.to >= other.from - TOUCH)
        {
            Some(other) => {
                other.from = other.from.min(rule.from);
                other.to = other.to.max(rule.to);
            }
            None => merged.push(Rule { at, ..rule }),
        }
    }

    // Merging can make two segments of a level overlap; fold those too.
    let mut folded: Vec<Rule> = Vec::new();
    merged.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.from.total_cmp(&b.from)));

    for rule in merged {
        match folded.last_mut() {
            Some(last) if last.at == rule.at && rule.from <= last.to + TOUCH => {
                last.to = last.to.max(rule.to);
            }
            _ => folded.push(rule),
        }
    }

    folded
}

/// Where a table is and what its rules say about its structure.
#[derive(Debug)]
struct Region {
    x0: f32,
    x1: f32,
    y0: f32,
    y1: f32,
    /// Column boundaries given by vertical rules inside the region.
    dividers: Vec<f32>,
    /// Row bands given by the rules, top to bottom, when they say.
    bands: Option<Vec<(f32, f32)>>,
    /// The rule under the header, for tables ruled only horizontally.
    header_rule: Option<f32>,
    /// Horizontal rules inside the region. Between each pair there must be
    /// something tabular, or the region reaches past the table's end.
    inner_rules: Vec<f32>,
}

/// Groups of horizontal and vertical rules that touch one another.
fn lattices(
    horizontal: &[Rule],
    vertical: &[Rule],
    pieces: &[Rule],
    used: &mut [bool],
) -> Vec<Region> {
    let touches = |h: &Rule, v: &Rule| {
        v.at >= h.from - TOUCH
            && v.at <= h.to + TOUCH
            && h.at >= v.from - TOUCH
            && h.at <= v.to + TOUCH
    };

    let mut regions = Vec::new();
    let mut taken = vec![false; vertical.len()];

    for seed in 0..vertical.len() {
        if taken[seed] {
            continue;
        }

        // Flood outwards from one vertical rule.
        let mut columns = vec![seed];
        let mut rows: Vec<usize> = Vec::new();
        let mut frontier = vec![seed];

        taken[seed] = true;

        while let Some(v) = frontier.pop() {
            for (h, rule) in horizontal.iter().enumerate() {
                if used[h] || !touches(rule, &vertical[v]) {
                    continue;
                }

                used[h] = true;
                rows.push(h);

                for (other, candidate) in vertical.iter().enumerate() {
                    if !taken[other] && touches(rule, candidate) {
                        taken[other] = true;
                        columns.push(other);
                        frontier.push(other);
                    }
                }
            }
        }

        if rows.len() < 2 {
            for h in rows {
                used[h] = false;
            }
            continue;
        }

        let x0 = rows
            .iter()
            .map(|h| horizontal[*h].from)
            .fold(f32::INFINITY, f32::min);
        let x1 = rows
            .iter()
            .map(|h| horizontal[*h].to)
            .fold(f32::NEG_INFINITY, f32::max);
        let mut levels: Vec<f32> = rows.iter().map(|h| horizontal[*h].at).collect();

        levels.sort_by(f32::total_cmp);

        let (y0, y1) = (levels[0], levels[levels.len() - 1]);

        let mut dividers: Vec<f32> = columns
            .iter()
            .map(|v| vertical[*v].at)
            .filter(|x| *x > x0 + TOUCH * 2.0 && *x < x1 - TOUCH * 2.0)
            .collect();

        dividers.sort_by(f32::total_cmp);
        dividers.dedup_by(|a, b| (*a - *b).abs() <= SAME);

        // Vertical rules are drawn a row at a time, so their pieces mark the
        // rows even where no horizontal rule separates them. Failing that,
        // every horizontal rule that spans the table does.
        let mut bands: Vec<(f32, f32)> = pieces
            .iter()
            .filter(|piece| {
                piece.at >= x0 - TOUCH
                    && piece.at <= x1 + TOUCH
                    && piece.from >= y0 - TOUCH
                    && piece.to <= y1 + TOUCH
            })
            .map(|piece| (piece.from, piece.to))
            .collect();

        bands.sort_by(|a, b| a.0.total_cmp(&b.0));
        bands.dedup_by(|a, b| (a.0 - b.0).abs() <= TOUCH && (a.1 - b.1).abs() <= TOUCH);

        let tiled = bands.len() >= 2
            && bands
                .windows(2)
                .all(|pair| (pair[1].0 - pair[0].1).abs() <= TOUCH * 1.5);

        let bands = if tiled {
            Some(bands)
        } else {
            let mut spanning: Vec<f32> = rows
                .iter()
                .map(|h| horizontal[*h])
                .filter(|rule| rule.length() >= (x1 - x0) * 0.9)
                .map(|rule| rule.at)
                .collect();

            spanning.sort_by(f32::total_cmp);

            (spanning.len() >= 3)
                .then(|| spanning.windows(2).map(|pair| (pair[0], pair[1])).collect())
        };

        // The header ends at the first rule below the top that spans the
        // table: it may hold more than one row.
        let header_rule = rows
            .iter()
            .map(|h| horizontal[*h])
            .filter(|rule| rule.length() >= (x1 - x0) * 0.9 && rule.at > y0 + TOUCH)
            .map(|rule| rule.at)
            .fold(None, |first: Option<f32>, at| {
                Some(first.map_or(at, |first| first.min(at)))
            })
            .filter(|at| *at < y1 - TOUCH);

        regions.push(Region {
            x0,
            x1,
            y0,
            y1,
            dividers,
            bands,
            header_rule,
            inner_rules: Vec::new(),
        });
    }

    regions
}

/// Tries runs of equal-width horizontal rules, widest first, as the bounds
/// of a table, and recurses into whatever the accepted run leaves over.
fn ruled(
    lines: &[TextLine],
    skip: &dyn Fn(usize) -> bool,
    rules: &[Rule],
    tables: &mut Vec<Table>,
) {
    if rules.len() < 2 {
        return;
    }

    for span in (1..rules.len()).rev() {
        for start in 0..rules.len() - span {
            let (top, bottom) = (rules[start], rules[start + span]);

            let region = Region {
                x0: top.from.min(bottom.from),
                x1: top.to.max(bottom.to),
                y0: top.at,
                y1: bottom.at,
                dividers: Vec::new(),
                bands: None,
                header_rule: (span >= 2).then(|| rules[start + 1].at),
                inner_rules: rules[start + 1..start + span]
                    .iter()
                    .map(|rule| rule.at)
                    .collect(),
            };

            if let Some(table) = extract(lines, skip, &region) {
                tables.push(table);
                ruled(lines, skip, &rules[..=start], tables);
                ruled(lines, skip, &rules[start + span..], tables);
                return;
            }
        }
    }
}

/// One baseline's worth of text inside a region.
struct TextRow<'a> {
    center: f32,
    top: f32,
    bottom: f32,
    size: f32,
    words: Vec<&'a Word>,
}

/// Words separated by no more than ordinary word spacing: the text of one
/// cell, or of one line of it.
struct Run<'a> {
    from: f32,
    to: f32,
    words: Vec<&'a Word>,
}

impl<'a> TextRow<'a> {
    fn runs(&self) -> Vec<Run<'a>> {
        let mut runs: Vec<Run<'a>> = Vec::new();

        for word in &self.words {
            match runs.last_mut() {
                Some(last) if word.x0 - last.to <= self.size * 0.75 => {
                    last.to = last.to.max(word.x1);
                    last.words.push(word);
                }
                _ => runs.push(Run {
                    from: word.x0,
                    to: word.x1,
                    words: vec![word],
                }),
            }
        }

        runs
    }
}

/// Whether a run reaches across a column boundary.
fn crosses(run: &Run, boundaries: &[f32]) -> bool {
    boundaries
        .iter()
        .any(|x| *x > run.from + 1.0 && *x < run.to - 1.0)
}

fn extract(lines: &[TextLine], skip: &dyn Fn(usize) -> bool, region: &Region) -> Option<Table> {
    let mut members: Vec<usize> = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let center = line.rect.center_y();

        if skip(index) || center <= region.y0 || center >= region.y1 {
            continue;
        }

        let inside =
            line.rect.x0 >= region.x0 - TOUCH * 2.0 && line.rect.x1 <= region.x1 + TOUCH * 2.0;
        let outside = line.rect.x1 < region.x0 || line.rect.x0 > region.x1;

        if inside {
            members.push(index);
        } else if !outside {
            // Text that straddles the edge means this is not a table's box.
            return None;
        }
    }

    if members.len() < 2 {
        return None;
    }

    // A plain box or pair of rules around mostly fixed-pitch text is a code
    // listing, however neatly its columns line up. Only rules inside the
    // region (dividers, a header rule, row bands) make it a table.
    let bare = region.dividers.is_empty() && region.bands.is_none() && region.header_rule.is_none();

    if bare {
        let (mut mono, mut total) = (0, 0);

        for span in members.iter().flat_map(|index| &lines[*index].spans) {
            let chars = span.text.chars().filter(|ch| !ch.is_whitespace()).count();

            total += chars;
            mono += if span.style & MONO != 0 { chars } else { 0 };
        }

        if mono * 2 >= total {
            return None;
        }
    }

    // Text rows: lines sharing a baseline.
    let mut order = members.clone();
    order.sort_by(|a, b| {
        lines[*a]
            .rect
            .center_y()
            .total_cmp(&lines[*b].rect.center_y())
    });

    let mut rows: Vec<TextRow> = Vec::new();

    for index in order {
        let line = &lines[index];
        let center = line.rect.center_y();

        match rows.last_mut() {
            Some(row) if center - row.center <= row.size.max(line.size) * 0.5 => {
                row.words.extend(&line.words);
                row.top = row.top.min(line.rect.y0);
                row.bottom = row.bottom.max(line.rect.y1);
            }
            _ => rows.push(TextRow {
                center,
                top: line.rect.y0,
                bottom: line.rect.y1,
                size: line.size.max(1.0),
                words: line.words.iter().collect(),
            }),
        }
    }

    for row in &mut rows {
        row.words.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    }

    let runs: Vec<Vec<Run>> = rows.iter().map(TextRow::runs).collect();
    let boundaries = columns(&rows, &runs, region)?;
    let width = boundaries.len() + 1;
    let column_of = |x: f32| boundaries.partition_point(|boundary| *boundary <= x);
    let edges = |column: usize| {
        (
            if column == 0 {
                region.x0
            } else {
                boundaries[column - 1]
            },
            boundaries.get(column).copied().unwrap_or(region.x1),
        )
    };

    // Between each pair of rules there must be something tabular. A stretch
    // holding only lines that run straight across the columns is a caption
    // or a paragraph, which means the region runs on beyond the table.
    let mut stops = vec![region.y0];
    stops.extend(&region.inner_rules);
    stops.push(region.y1);

    for pair in stops.windows(2) {
        let mut between = rows
            .iter()
            .zip(&runs)
            .filter(|(row, _)| row.center > pair[0] && row.center <= pair[1])
            .peekable();

        if between.peek().is_some()
            && between.all(|(_, runs)| runs.len() == 1 && crosses(&runs[0], &boundaries))
        {
            return None;
        }
    }

    // Reject prose set in columns: there, line after line fills its column
    // from edge to edge, which the cells of a table do not.
    if region.dividers.is_empty() && rows.len() > 12 {
        let full = runs
            .iter()
            .filter(|runs| {
                runs.len() == width
                    && runs.iter().enumerate().all(|(column, run)| {
                        let (left, right) = edges(column);
                        run.to - run.from >= (right - left) * 0.8
                    })
            })
            .count();

        if full * 10 >= rows.len() * 6 {
            return None;
        }
    }

    // Table rows: the bands the rules give, or else one per text row. Each
    // keeps the text rows that make it up, for the header logic below.
    struct GridRow<'a> {
        top: f32,
        bottom: f32,
        lines: Vec<&'a [Run<'a>]>,
    }

    let band_of = |row: &TextRow| {
        region.bands.as_ref().and_then(|bands| {
            bands
                .iter()
                .position(|(top, bottom)| row.center > *top && row.center <= *bottom)
        })
    };

    let mut grid: Vec<GridRow> = Vec::new();
    let mut current_band: Option<usize> = None;

    for (row, runs) in rows.iter().zip(&runs) {
        let band = band_of(row);
        let same_row =
            matches!((band, current_band), (Some(band), Some(current)) if band == current);

        match grid.last_mut() {
            Some(last) if same_row => {
                last.bottom = row.bottom;
                last.lines.push(runs);
            }
            _ => grid.push(GridRow {
                top: row.top,
                bottom: row.bottom,
                lines: vec![runs],
            }),
        }

        current_band = band;
    }

    // Without bands, a line that only continues one cell of the row above
    // (a wrapped description) is folded back into it.
    if region.bands.is_none() {
        let mut folded: Vec<GridRow> = Vec::new();

        for row in grid {
            let filled = |row: &GridRow| -> Vec<usize> {
                let mut columns: Vec<usize> = row
                    .lines
                    .iter()
                    .flat_map(|runs| runs.iter().map(|run| column_of(run.from + 1.0)))
                    .collect();

                columns.sort_unstable();
                columns.dedup();
                columns
            };
            let own = filled(&row);

            let continues = folded.last().is_some_and(|above| {
                own.len() == 1
                    && own[0] != 0
                    && filled(above).contains(&own[0])
                    && row.top - above.bottom <= (row.bottom - row.top) * 0.45
            });

            match folded.last_mut() {
                Some(above) if continues => {
                    above.bottom = row.bottom;
                    above.lines.extend(row.lines);
                }
                _ => folded.push(row),
            }
        }

        grid = folded;
    }

    // The header: every row above the rule beneath it, when there is one.
    let header_rows = match region.header_rule {
        Some(rule) => grid
            .iter()
            .take_while(|row| (row.top + row.bottom) * 0.5 < rule)
            .count(),
        None => 1,
    }
    .clamp(1, grid.len());

    // A header that takes every row means the rule was not a header rule.
    if header_rows == grid.len() {
        return None;
    }

    let cells_of = |rows: &[GridRow], header: bool| -> Vec<Vec<Span>> {
        let mut cells = vec![Vec::new(); width];
        // In a header of several lines, a title centred over a group of
        // columns belongs to each of them.
        let shared = header && rows.iter().map(|row| row.lines.len()).sum::<usize>() > 1;

        for run in rows
            .iter()
            .flat_map(|row| &row.lines)
            .flat_map(|runs| runs.iter())
        {
            let first = column_of(run.from + 1.0).min(width - 1);
            let mut targets = vec![first];

            if shared && crosses(run, &boundaries) {
                targets = (0..width)
                    .filter(|column| {
                        let (left, right) = edges(*column);
                        let overlap = run.to.min(right) - run.from.max(left);

                        overlap >= (run.to - run.from) * 0.25
                    })
                    .collect();

                if targets.is_empty() {
                    targets.push(first);
                }
            }

            for column in targets {
                for word in &run.words {
                    append(&mut cells[column], word);
                }
            }
        }

        cells
    };

    let mut table_rows: Vec<Vec<Vec<Span>>> = Vec::with_capacity(grid.len());

    table_rows.push(cells_of(&grid[..header_rows], true));
    table_rows.extend(
        grid[header_rows..]
            .iter()
            .map(|row| cells_of(std::slice::from_ref(row), false)),
    );

    // Sanity checks. A table labels its columns, most of its rows have more
    // than one cell, and every column is used. Rules that spell out the
    // grid are evidence enough to accept a sparsely filled one.
    let count = |row: &Vec<Vec<Span>>| row.iter().filter(|cell| !cell.is_empty()).count();
    let cells = table_rows.len() * width;
    let filled: usize = table_rows.iter().map(count).sum();
    let multi = table_rows.iter().filter(|row| count(row) >= 2).count();
    let every_column_used =
        (0..width).all(|column| table_rows.iter().any(|row| !row[column].is_empty()));
    let needed = if region.bands.is_some() { 15 } else { 35 };

    if count(&table_rows[0]) < 2
        || multi * 10 < table_rows.len() * 6
        || filled * 100 < cells * needed
        || !every_column_used
    {
        return None;
    }

    members.sort_unstable();

    Some(Table {
        lines: members,
        rows: table_rows,
    })
}

/// Column boundaries, from where the text leaves gaps.
///
/// Laying every run of every row along the x axis, the stretches no run
/// covers separate the columns; that keeps a sparsely filled column distinct
/// from its neighbours. Cells that span several columns would bridge those
/// gaps, so they are set aside first: a run is a spanning cell when other
/// rows fit two or more cells of their own beneath it. Vertical rules, where
/// drawn, are boundaries regardless.
fn columns(rows: &[TextRow], runs: &[Vec<Run>], region: &Region) -> Option<Vec<f32>> {
    let overlaps = |a: &Run, b: &Run| a.from.max(b.from) + 1.0 < a.to.min(b.to);
    // One other row is thin evidence, but in a short table it is all there is.
    let witnesses = if rows.len() >= 4 { 2 } else { 1 };

    let mut spans: Vec<(f32, f32)> = Vec::new();
    let mut spanning_rows = 0;

    for (index, row) in runs.iter().enumerate() {
        let mut spanned = false;

        for run in row {
            let split_by = runs
                .iter()
                .enumerate()
                .filter(|(other, cells)| {
                    *other != index && cells.iter().filter(|cell| overlaps(run, cell)).count() >= 2
                })
                .count();

            if split_by >= witnesses {
                spanned = true;
            } else {
                spans.push((run.from, run.to));
            }
        }

        spanning_rows += spanned as usize;
    }

    // If most rows "span", there are no columns to speak of.
    if spanning_rows * 2 > rows.len() {
        return None;
    }

    spans.sort_by(|a, b| a.0.total_cmp(&b.0));

    let smallest = rows
        .iter()
        .map(|row| row.size)
        .fold(f32::INFINITY, f32::min);
    let narrowest = (smallest * 0.2).max(1.5);

    // The gaps in the union of what is left.
    let mut gaps: Vec<(f32, f32)> = Vec::new();
    let mut reach = f32::NEG_INFINITY;

    for (from, to) in spans {
        if reach.is_finite() && from - reach >= narrowest {
            gaps.push((reach, from));
        }

        reach = reach.max(to);
    }

    // A divider drawn within a gap says exactly where that boundary is.
    let mut boundaries: Vec<f32> = gaps
        .iter()
        .filter(|(from, to)| {
            !region
                .dividers
                .iter()
                .any(|x| *x >= from - TOUCH && *x <= to + TOUCH)
        })
        .map(|(from, to)| (from + to) * 0.5)
        .collect();

    boundaries.extend(&region.dividers);
    boundaries.sort_by(f32::total_cmp);

    (!boundaries.is_empty() && boundaries.len() < MAX_COLUMNS).then_some(boundaries)
}

fn append(cell: &mut Vec<Span>, word: &Word) {
    for (index, span) in word.spans.iter().enumerate() {
        push(cell, span, index == 0);
    }
}

/// Adds a span to a cell, with a space before it when it starts a new word.
fn push(cell: &mut Vec<Span>, span: &Span, new_word: bool) {
    let space = if new_word && !cell.is_empty() {
        " "
    } else {
        ""
    };

    match cell.last_mut() {
        Some(last) if last.style == span.style => {
            last.text.push_str(space);
            last.text.push_str(&span.text);
        }
        Some(last) => {
            last.text.push_str(space);
            cell.push(span.clone());
        }
        None => cell.push(span.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::text::Rect;

    fn h(y: f32, from: f32, to: f32) -> Rule {
        Rule {
            horizontal: true,
            at: y,
            from,
            to,
        }
    }

    fn v(x: f32, from: f32, to: f32) -> Rule {
        Rule {
            horizontal: false,
            at: x,
            from,
            to,
        }
    }

    /// A line of 10pt text whose words sit at the given x positions; each
    /// character is 5pt wide.
    fn line(y: f32, words: &[(f32, &str)]) -> TextLine {
        let words: Vec<Word> = words
            .iter()
            .map(|(x, text)| Word {
                x0: *x,
                x1: *x + text.chars().count() as f32 * 5.0,
                spans: vec![Span {
                    text: (*text).into(),
                    style: 0,
                }],
            })
            .collect();

        TextLine {
            spans: words.iter().flat_map(|word| word.spans.clone()).collect(),
            rect: Rect {
                x0: words.first().unwrap().x0,
                y0: y,
                x1: words.last().unwrap().x1,
                y1: y + 10.0,
            },
            size: 10.0,
            words,
        }
    }

    fn cells(table: &Table) -> Vec<Vec<String>> {
        table
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.iter().map(|span| span.text.as_str()).collect())
                    .collect()
            })
            .collect()
    }

    fn none(_: usize) -> bool {
        false
    }

    #[test]
    fn reads_a_table_ruled_only_horizontally() {
        let lines = [
            line(84.0, &[(100.0, "Table"), (130.0, "1:"), (145.0, "Results")]),
            line(104.0, &[(100.0, "Model"), (220.0, "BLEU"), (300.0, "Cost")]),
            line(
                124.0,
                &[
                    (100.0, "Small"),
                    (130.0, "net"),
                    (225.0, "27.3"),
                    (300.0, "3.3"),
                ],
            ),
            line(138.0, &[(100.0, "Big"), (225.0, "28.4"), (300.0, "2.3")]),
            line(
                170.0,
                &[
                    (100.0, "Prose"),
                    (130.0, "after"),
                    (160.0, "the"),
                    (180.0, "table."),
                ],
            ),
        ];
        let rules = [
            h(100.0, 95.0, 330.0),
            h(118.0, 95.0, 330.0),
            h(152.0, 95.0, 330.0),
        ];

        let tables = detect(&lines, &rules, &none);

        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].lines, [1, 2, 3]);
        assert_eq!(
            cells(&tables[0]),
            [
                ["Model", "BLEU", "Cost"],
                ["Small net", "27.3", "3.3"],
                ["Big", "28.4", "2.3"],
            ]
        );
    }

    #[test]
    fn reads_a_grid_with_wrapped_cells() {
        // Each cell draws its own border, so every edge appears twice.
        let mut rules = Vec::new();

        for (top, bottom) in [(100.0, 120.0), (120.0, 152.0)] {
            for (left, right) in [(50.0, 150.0), (150.0, 400.0)] {
                rules.extend([h(top, left, right), h(bottom, left, right)]);
                rules.extend([v(left, top, bottom), v(right, top, bottom)]);
            }
        }

        let lines = [
            line(105.0, &[(55.0, "Option"), (155.0, "Description")]),
            line(
                124.0,
                &[
                    (55.0, "-p"),
                    (155.0, "Show"),
                    (180.0, "the"),
                    (200.0, "patch"),
                ],
            ),
            line(
                137.0,
                &[
                    (155.0, "introduced"),
                    (210.0, "with"),
                    (235.0, "each"),
                    (260.0, "commit."),
                ],
            ),
        ];

        let tables = detect(&lines, &rules, &none);

        assert_eq!(tables.len(), 1);
        assert_eq!(
            cells(&tables[0]),
            [
                ["Option", "Description"],
                ["-p", "Show the patch introduced with each commit."],
            ]
        );
    }

    #[test]
    fn rows_follow_the_pieces_of_vertical_rules() {
        // One horizontal band holding two rows, as typeset tables do: the
        // vertical rule is drawn once per row.
        let rules = [
            h(100.0, 50.0, 300.0),
            h(116.0, 50.0, 300.0),
            h(146.0, 50.0, 300.0),
            v(120.0, 100.0, 116.0),
            v(120.0, 116.0, 131.0),
            v(120.0, 131.0, 146.0),
        ];
        let lines = [
            line(103.0, &[(55.0, "Group"), (130.0, "N"), (200.0, "Score")]),
            line(119.0, &[(55.0, "(A)"), (130.0, "6"), (200.0, "25.8")]),
            line(134.0, &[(130.0, "8"), (200.0, "25.5")]),
        ];

        let tables = detect(&lines, &rules, &none);

        assert_eq!(tables.len(), 1);
        assert_eq!(
            cells(&tables[0]),
            [
                ["Group", "N", "Score"],
                ["(A)", "6", "25.8"],
                ["", "8", "25.5"]
            ]
        );
    }

    #[test]
    fn folds_a_wrapped_line_into_the_row_above() {
        let lines = [
            line(104.0, &[(100.0, "Name"), (200.0, "Meaning")]),
            line(
                124.0,
                &[
                    (100.0, "alpha"),
                    (200.0, "The"),
                    (220.0, "first"),
                    (250.0, "letter"),
                ],
            ),
            line(
                135.0,
                &[(200.0, "of"), (215.0, "the"), (235.0, "alphabet.")],
            ),
            line(
                150.0,
                &[(100.0, "beta"), (200.0, "The"), (220.0, "second.")],
            ),
        ];
        let rules = [
            h(100.0, 95.0, 330.0),
            h(118.0, 95.0, 330.0),
            h(164.0, 95.0, 330.0),
        ];

        let tables = detect(&lines, &rules, &none);

        assert_eq!(
            cells(&tables[0]),
            [
                ["Name", "Meaning"],
                ["alpha", "The first letter of the alphabet."],
                ["beta", "The second."],
            ]
        );
    }

    #[test]
    fn prose_between_rules_is_not_a_table() {
        let lines = [
            line(
                104.0,
                &[
                    (100.0, "A"),
                    (110.0, "note"),
                    (135.0, "set"),
                    (155.0, "between"),
                ],
            ),
            line(
                118.0,
                &[
                    (100.0, "two"),
                    (120.0, "rules"),
                    (150.0, "for"),
                    (170.0, "emphasis."),
                ],
            ),
        ];
        let rules = [h(100.0, 95.0, 330.0), h(132.0, 95.0, 330.0)];

        assert_eq!(detect(&lines, &rules, &none), []);
    }

    #[test]
    fn a_boxed_code_listing_is_not_a_table() {
        let mut listing = [
            line(104.0, &[(100.0, "total"), (200.0, "12")]),
            line(118.0, &[(100.0, "used"), (200.0, "7")]),
        ];

        for line in &mut listing {
            line.spans = vec![Span {
                text: "code".into(),
                style: MONO,
            }];
        }

        let rules = [
            h(100.0, 95.0, 330.0),
            h(132.0, 95.0, 330.0),
            v(95.0, 100.0, 132.0),
            v(330.0, 100.0, 132.0),
        ];

        assert_eq!(detect(&listing, &rules, &none), []);
    }

    #[test]
    fn aligned_text_without_rules_is_left_alone() {
        let lines = [
            line(104.0, &[(100.0, "Model"), (220.0, "BLEU")]),
            line(118.0, &[(100.0, "Small"), (220.0, "27.3")]),
        ];

        assert_eq!(detect(&lines, &[], &none), []);
        // A lone rule (a footnote separator, an underline) bounds nothing.
        assert_eq!(detect(&lines, &[h(100.0, 95.0, 330.0)], &none), []);
    }
}
