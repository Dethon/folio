//! The text layer of a page: glyph boxes grouped into lines.
//!
//! Everything here is plain data with no dependency on PDFium, so selection
//! and hit testing can run on the UI thread against a cached [`PageText`].

use std::ops::Range;

/// A rectangle in page space: points, origin at the top-left, y pointing down.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }

    pub fn center_y(&self) -> f32 {
        (self.y0 + self.y1) * 0.5
    }

    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 1 << 1;
pub const MONO: u8 = 1 << 2;

/// One character on the page. Characters synthesised by PDFium (word spaces,
/// line breaks) carry an empty `rect`.
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    pub ch: char,
    pub rect: Rect,
    /// Font size in points. Only filled in by styled extraction.
    pub size: f32,
    /// Combination of [`BOLD`], [`ITALIC`] and [`MONO`]. Only filled in by
    /// styled extraction.
    pub style: u8,
}

/// A run of glyphs sharing a baseline.
#[derive(Debug, Clone)]
pub struct Line {
    pub glyphs: Range<u32>,
    pub rect: Rect,
}

/// A position on a page, used to anchor selections independently of whether
/// the page's text has been loaded yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub page: u32,
    pub x: f32,
    pub y: f32,
}

impl Anchor {
    pub fn start_of(page: u32) -> Self {
        Self {
            page,
            x: f32::NEG_INFINITY,
            y: f32::NEG_INFINITY,
        }
    }

    pub fn end_of(page: u32) -> Self {
        Self {
            page,
            x: f32::INFINITY,
            y: f32::INFINITY,
        }
    }

    /// Document order.
    pub fn precedes(&self, other: &Anchor, text: Option<&PageText>) -> bool {
        if self.page != other.page {
            return self.page < other.page;
        }

        match text {
            Some(text) => text.caret_at(self.x, self.y) <= text.caret_at(other.x, other.y),
            None => (self.y, self.x) <= (other.y, other.x),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Granularity {
    #[default]
    Char,
    Word,
    Line,
}

#[derive(Debug, Default)]
pub struct PageText {
    pub glyphs: Vec<Glyph>,
    pub lines: Vec<Line>,
}

impl PageText {
    pub fn new(glyphs: Vec<Glyph>) -> Self {
        let lines = group_lines(&glyphs);

        Self { glyphs, lines }
    }

    pub fn len(&self) -> u32 {
        self.glyphs.len() as u32
    }

    /// Whether the point lies over selectable text.
    pub fn hit(&self, x: f32, y: f32) -> bool {
        self.lines.iter().any(|line| {
            let slack = line.rect.height() * 0.15;

            x >= line.rect.x0 - slack
                && x <= line.rect.x1 + slack
                && y >= line.rect.y0
                && y <= line.rect.y1
        })
    }

    fn nearest_line(&self, x: f32, y: f32) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }

        // Prefer a line containing the point vertically; among those (columns
        // side by side) take the horizontally closest. Otherwise fall back to
        // plain distance.
        let distance = |line: &Line| {
            let dx = if x < line.rect.x0 {
                line.rect.x0 - x
            } else if x > line.rect.x1 {
                x - line.rect.x1
            } else {
                0.0
            };
            let dy = if y < line.rect.y0 {
                line.rect.y0 - y
            } else if y > line.rect.y1 {
                y - line.rect.y1
            } else {
                0.0
            };

            // Vertical misses cost more than horizontal ones so that dragging
            // into a margin keeps tracking the line beside the cursor.
            dx + dy * 4.0
        };

        self.lines
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
            .map(|(index, _)| index)
    }

    /// The caret index (`0..=len`) nearest to the given point.
    pub fn caret_at(&self, x: f32, y: f32) -> u32 {
        if y == f32::NEG_INFINITY {
            return 0;
        }

        if y == f32::INFINITY {
            return self.len();
        }

        let Some(line) = self.nearest_line(x, y) else {
            return 0;
        };
        let line = &self.lines[line];

        if y < line.rect.y0 - line.rect.height() * 4.0 && line.glyphs.start == 0 {
            return 0;
        }

        for index in line.glyphs.clone() {
            let rect = self.glyphs[index as usize].rect;

            if rect.is_empty() {
                continue;
            }

            if x < (rect.x0 + rect.x1) * 0.5 {
                return index;
            }
        }

        self.visible_end(line)
    }

    /// End of the line excluding trailing synthesised whitespace.
    fn visible_end(&self, line: &Line) -> u32 {
        let mut end = line.glyphs.end;

        while end > line.glyphs.start && self.glyphs[end as usize - 1].rect.is_empty() {
            end -= 1;
        }

        end
    }

    fn glyph_at(&self, x: f32, y: f32) -> Option<(usize, u32)> {
        let line_index = self.nearest_line(x, y)?;
        let line = &self.lines[line_index];

        if !line.rect.contains(x, y) {
            return None;
        }

        line.glyphs
            .clone()
            .filter(|index| !self.glyphs[*index as usize].rect.is_empty())
            .min_by(|a, b| {
                let center = |index: &u32| {
                    let rect = self.glyphs[*index as usize].rect;
                    ((rect.x0 + rect.x1) * 0.5 - x).abs()
                };

                center(a).total_cmp(&center(b))
            })
            .map(|index| (line_index, index))
    }

    /// The range covered by the word or line under the point.
    pub fn unit_at(&self, x: f32, y: f32, granularity: Granularity) -> Option<Range<u32>> {
        let (line_index, index) = self.glyph_at(x, y)?;
        let line = &self.lines[line_index];

        match granularity {
            Granularity::Char => Some(index..index + 1),
            Granularity::Line => Some(line.glyphs.start..self.visible_end(line)),
            Granularity::Word => {
                let is_word = |index: u32| is_word_char(self.glyphs[index as usize].ch);

                if !is_word(index) {
                    return Some(index..index + 1);
                }

                let mut start = index;
                let mut end = index + 1;

                while start > line.glyphs.start && is_word(start - 1) {
                    start -= 1;
                }

                while end < line.glyphs.end && is_word(end) {
                    end += 1;
                }

                Some(start..end)
            }
        }
    }

    /// One highlight rectangle per line touched by `range`.
    pub fn rects(&self, range: Range<u32>) -> Vec<Rect> {
        let mut rects = Vec::new();

        if range.is_empty() {
            return rects;
        }

        let first = self
            .lines
            .partition_point(|line| line.glyphs.end <= range.start);

        for line in &self.lines[first..] {
            if line.glyphs.start >= range.end {
                break;
            }

            let start = line.glyphs.start.max(range.start);
            let end = line.glyphs.end.min(range.end);
            let mut x0 = f32::INFINITY;
            let mut x1 = f32::NEG_INFINITY;

            for glyph in &self.glyphs[start as usize..end as usize] {
                if !glyph.rect.is_empty() {
                    x0 = x0.min(glyph.rect.x0);
                    x1 = x1.max(glyph.rect.x1);
                }
            }

            if x0 < x1 {
                rects.push(Rect {
                    x0,
                    x1,
                    y0: line.rect.y0,
                    y1: line.rect.y1,
                });
            }
        }

        rects
    }

    pub fn text(&self, range: Range<u32>) -> String {
        let end = (range.end as usize).min(self.glyphs.len());
        let start = (range.start as usize).min(end);

        self.glyphs[start..end]
            .iter()
            .map(|glyph| glyph.ch)
            .collect()
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '\'' || ch == '’'
}

fn group_lines(glyphs: &[Glyph]) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Option<Line> = None;
    let mut last_x1 = 0.0f32;

    for (index, glyph) in glyphs.iter().enumerate() {
        let index = index as u32;

        if glyph.ch == '\n' {
            // The break belongs to the line it terminates.
            if let Some(mut line) = current.take() {
                line.glyphs.end = index + 1;
                lines.push(line);
            }
            continue;
        }

        if glyph.rect.is_empty() {
            if let Some(line) = current.as_mut() {
                line.glyphs.end = index + 1;
            }
            continue;
        }

        let breaks = current.as_ref().is_some_and(|line| {
            let height = line.rect.height().max(glyph.rect.height());
            let off_baseline = (glyph.rect.center_y() - line.rect.center_y()).abs() > height * 0.6;
            // A jump back to the left means a new column or a wrapped line
            // that PDFium did not mark with a break.
            let rewinds = glyph.rect.x1 < last_x1 - height * 2.0;

            off_baseline || rewinds
        });

        if breaks && let Some(line) = current.take() {
            lines.push(line);
        }

        match current.as_mut() {
            Some(line) => {
                line.glyphs.end = index + 1;
                line.rect = line.rect.union(&glyph.rect);
            }
            None => {
                current = Some(Line {
                    glyphs: index..index + 1,
                    rect: glyph.rect,
                });
            }
        }

        last_x1 = glyph.rect.x1;
    }

    lines.extend(current);
    lines
}

/// The glyph range on `page` covered by a selection running from `from` to
/// `to` (already in document order).
pub fn selected_range(
    text: &PageText,
    page: u32,
    from: &Anchor,
    to: &Anchor,
    granularity: Granularity,
) -> Range<u32> {
    let edge = |anchor: &Anchor, is_end: bool| -> u32 {
        if anchor.page < page {
            return 0;
        }

        if anchor.page > page {
            return text.len();
        }

        let caret = text.caret_at(anchor.x, anchor.y);

        if granularity == Granularity::Char {
            return caret;
        }

        match text.unit_at(anchor.x, anchor.y, granularity) {
            Some(unit) if is_end => unit.end.max(caret.min(unit.end)),
            Some(unit) => unit.start,
            None => caret,
        }
    };

    let start = edge(from, false);
    let end = edge(to, true);

    start.min(end)..end.max(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(ch: char, x: f32, y: f32) -> Glyph {
        Glyph {
            ch,
            rect: Rect {
                x0: x,
                y0: y,
                x1: x + 10.0,
                y1: y + 12.0,
            },
            size: 12.0,
            style: 0,
        }
    }

    fn generated(ch: char) -> Glyph {
        Glyph {
            ch,
            rect: Rect::default(),
            size: 12.0,
            style: 0,
        }
    }

    fn sample() -> PageText {
        // "ab cd" on one line, "ef" on the next.
        PageText::new(vec![
            glyph('a', 0.0, 0.0),
            glyph('b', 10.0, 0.0),
            generated(' '),
            glyph('c', 30.0, 0.0),
            glyph('d', 40.0, 0.0),
            generated('\n'),
            glyph('e', 0.0, 20.0),
            glyph('f', 10.0, 20.0),
        ])
    }

    #[test]
    fn groups_glyphs_into_lines() {
        let text = sample();

        assert_eq!(text.lines.len(), 2);
        assert_eq!(text.lines[0].glyphs, 0..6);
        assert_eq!(text.lines[1].glyphs, 6..8);
    }

    #[test]
    fn caret_snaps_to_nearest_glyph_edge() {
        let text = sample();

        assert_eq!(text.caret_at(2.0, 5.0), 0);
        assert_eq!(text.caret_at(8.0, 5.0), 1);
        assert_eq!(text.caret_at(500.0, 5.0), 5);
        assert_eq!(text.caret_at(500.0, 25.0), 8);
        assert_eq!(text.caret_at(f32::INFINITY, f32::INFINITY), 8);
    }

    #[test]
    fn selects_words_and_lines() {
        let text = sample();

        assert_eq!(text.unit_at(32.0, 5.0, Granularity::Word), Some(3..5));
        assert_eq!(text.unit_at(32.0, 5.0, Granularity::Line), Some(0..5));
        assert_eq!(text.text(3..5), "cd");
    }

    #[test]
    fn selection_spans_pages() {
        let text = sample();
        let from = Anchor {
            page: 0,
            x: 32.0,
            y: 5.0,
        };
        let to = Anchor::end_of(2);

        assert_eq!(
            selected_range(&text, 0, &from, &to, Granularity::Char),
            3..8
        );
        assert_eq!(
            selected_range(&text, 1, &from, &to, Granularity::Char),
            0..8
        );
        assert_eq!(text.rects(3..8).len(), 2);
    }
}
