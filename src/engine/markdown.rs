//! Converts extracted text into Markdown.
//!
//! A PDF carries no structure, only positioned glyphs, so everything here is
//! inferred: headings from font size relative to the body text (and from the
//! document outline when a line matches a bookmark), paragraphs from line
//! spacing, lists from their markers, emphasis from font style. Running
//! headers and footers are recognised by repetition across pages and dropped.
//! Tables are rebuilt where they are drawn with rules (see [`super::table`]);
//! figures are not extracted.

use std::collections::{HashMap, HashSet};

use super::table::{self, Rule, Table};
use super::text::{BOLD, ITALIC, MONO, PageText, Rect};
use super::{OutlineItem, PageSize};

/// Set on glyphs that do not run left to right, such as watermarks printed
/// up the margin. They are left out of the export.
pub const ROTATED: u8 = 1 << 7;

const STYLE_MASK: u8 = BOLD | ITALIC | MONO;

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: u8,
}

/// A word and where it sits on its line, which is what table detection
/// works from.
#[derive(Debug, Clone)]
pub struct Word {
    pub x0: f32,
    pub x1: f32,
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone)]
pub struct TextLine {
    pub spans: Vec<Span>,
    pub words: Vec<Word>,
    pub rect: Rect,
    pub size: f32,
}

impl TextLine {
    fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    fn chars(&self) -> usize {
        self.spans
            .iter()
            .map(|span| span.text.chars().count())
            .sum()
    }

    pub(crate) fn all(&self, style: u8) -> bool {
        self.spans
            .iter()
            .filter(|span| !span.text.trim().is_empty())
            .all(|span| span.style & style != 0)
    }
}

#[derive(Debug, Clone)]
pub struct Page {
    pub lines: Vec<TextLine>,
    /// Ruling lines drawn on the page.
    pub rules: Vec<Rule>,
    pub height: f32,
}

impl Page {
    /// Builds the export model from a page extracted with styling.
    pub fn new(text: &PageText, size: PageSize, rules: Vec<Rule>) -> Self {
        let mut lines = Vec::with_capacity(text.lines.len());

        for line in &text.lines {
            let mut spans: Vec<Span> = Vec::new();
            let mut words: Vec<Word> = Vec::new();
            let mut in_word = false;
            let mut sizes: HashMap<u32, usize> = HashMap::new();
            let mut style = 0;

            for glyph in &text.glyphs[line.glyphs.start as usize..line.glyphs.end as usize] {
                if glyph.ch == '\n' || glyph.style & ROTATED != 0 {
                    continue;
                }

                // Whitespace has no font of its own; it continues the run.
                if !glyph.rect.is_empty() {
                    style = glyph.style & STYLE_MASK;
                    *sizes.entry(size_key(glyph.size)).or_default() += 1;

                    // A jump with no space glyph in it still ends a word.
                    let jumps = words
                        .last()
                        .is_some_and(|word| glyph.rect.x0 - word.x1 > glyph.size.max(4.0));

                    match words.last_mut() {
                        Some(word) if in_word && !jumps => {
                            word.x0 = word.x0.min(glyph.rect.x0);
                            word.x1 = word.x1.max(glyph.rect.x1);

                            match word.spans.last_mut() {
                                Some(span) if span.style == style => span.text.push(glyph.ch),
                                _ => word.spans.push(Span {
                                    text: glyph.ch.to_string(),
                                    style,
                                }),
                            }
                        }
                        _ => words.push(Word {
                            x0: glyph.rect.x0,
                            x1: glyph.rect.x1,
                            spans: vec![Span {
                                text: glyph.ch.to_string(),
                                style,
                            }],
                        }),
                    }

                    in_word = true;
                } else {
                    in_word = false;
                }

                match spans.last_mut() {
                    Some(span) if span.style == style => span.text.push(glyph.ch),
                    _ => spans.push(Span {
                        text: glyph.ch.to_string(),
                        style,
                    }),
                }
            }

            let Some((size, _)) = sizes.into_iter().max_by_key(|(key, count)| (*count, *key))
            else {
                continue;
            };

            trim_spans(&mut spans);

            if !spans.is_empty() {
                lines.push(TextLine {
                    spans,
                    words,
                    rect: line.rect,
                    size: size as f32 / 2.0,
                });
            }
        }

        Self {
            lines,
            rules,
            height: size.height,
        }
    }
}

/// Font sizes are compared in half-point steps.
fn size_key(size: f32) -> u32 {
    (size * 2.0).round().max(0.0) as u32
}

fn trim_spans(spans: &mut Vec<Span>) {
    if let Some(first) = spans.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }

    if let Some(last) = spans.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }

    spans.retain(|span| !span.text.is_empty());
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Marker {
    Bullet,
    Ordered(u32),
}

#[derive(Debug)]
enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(Vec<Span>),
    Item {
        depth: u8,
        marker: Marker,
        spans: Vec<Span>,
    },
    Code(Vec<String>),
    /// Rows of cells; the first row is the header.
    Table(Vec<Vec<Vec<Span>>>),
}

pub fn render(pages: &[Page], outline: &[OutlineItem]) -> String {
    let furniture = find_furniture(pages);

    let repeated = repeated_rules(pages);

    let tables: Vec<Vec<Table>> = pages
        .iter()
        .enumerate()
        .map(|(page_index, page)| {
            let rules: Vec<Rule> = page
                .rules
                .iter()
                .filter(|rule| !repeated.contains(&rule_key(rule)))
                .copied()
                .collect();

            table::detect(&page.lines, &rules, &|line| {
                furniture.contains(&(page_index, line))
            })
        })
        .collect();

    // Lines that are not running text: they neither set heading sizes nor
    // flow into paragraphs.
    let mut set_aside = furniture.clone();

    for (page_index, tables) in tables.iter().enumerate() {
        for table in tables {
            set_aside.extend(table.lines.iter().map(|line| (page_index, *line)));
        }
    }

    let body = body_size(pages);
    let outline = OutlineIndex::new(outline);
    let levels = Levels::new(pages, body, &set_aside, &outline);

    let mut builder = Builder {
        minor_level: levels.minor(),
        ..Builder::default()
    };

    for (page_index, page) in pages.iter().enumerate() {
        builder.page_break();

        for (line_index, line) in page.lines.iter().enumerate() {
            // A table is emitted where its first line stood.
            if let Some(table) = tables[page_index]
                .iter()
                .find(|table| table.lines.first() == Some(&line_index))
            {
                builder.table(table.rows.clone());
            }

            if set_aside.contains(&(page_index, line_index)) {
                continue;
            }

            let text = line.text();
            let level = outline
                .level(page_index as u32, &text)
                .map(|level| levels.anchored(level))
                .or_else(|| levels.of(page_index, line, &text));
            let next = page.lines.get(line_index + 1);

            builder.push(line, text, level, next, body);
        }
    }

    builder.finish()
}

/// Maps font sizes to heading levels.
///
/// When lines match the document outline, the outline is the authority: its
/// depth gives the level, and the sizes of the matched lines anchor a scale
/// that other lines are placed on. Without an outline, sizes larger than the
/// body text are simply ranked.
struct Levels {
    /// `(size, level)` for sizes used by outline entries, largest first.
    anchors: Vec<(u32, u8)>,
    /// `(size, level)` by rank, used when there is no outline to go by.
    ranks: Vec<(u32, u8)>,
    /// Size of a title on the first page that is larger than every anchor.
    /// When present it takes level 1 and everything else moves down one.
    title: Option<u32>,
    /// How many heading-like lines use each size.
    counts: HashMap<u32, usize>,
    threshold: u32,
}

impl Levels {
    fn new(
        pages: &[Page],
        body: u32,
        furniture: &HashSet<(usize, usize)>,
        outline: &OutlineIndex,
    ) -> Self {
        let threshold = ((body as f32 * 1.12).ceil() as u32).max(body + 2);
        let mut anchors: HashMap<u32, u8> = HashMap::new();
        let mut counts: HashMap<u32, usize> = HashMap::new();
        let mut first_page: Vec<u32> = Vec::new();

        for (page_index, page) in pages.iter().enumerate() {
            for (line_index, line) in page.lines.iter().enumerate() {
                if furniture.contains(&(page_index, line_index)) {
                    continue;
                }

                let size = size_key(line.size);
                let text = line.text();

                if let Some(level) = outline.level(page_index as u32, &text) {
                    let anchor = anchors.entry(size).or_insert(level);
                    *anchor = (*anchor).min(level);
                } else if size >= threshold && heading_like(&text) {
                    *counts.entry(size).or_default() += 1;

                    if page_index == 0 {
                        first_page.push(size);
                    }
                }
            }
        }

        let mut anchors: Vec<(u32, u8)> = anchors.into_iter().collect();
        anchors.sort_unstable_by_key(|(size, _)| std::cmp::Reverse(*size));

        let mut ranks: Vec<u32> = counts.keys().copied().collect();
        ranks.sort_unstable_by(|a, b| b.cmp(a));

        let title = anchors
            .first()
            .and_then(|(largest, _)| first_page.into_iter().filter(|size| size > largest).max());

        Self {
            anchors,
            ranks: ranks
                .into_iter()
                .enumerate()
                .map(|(rank, size)| (size, (rank as u8 + 1).min(4)))
                .collect(),
            title,
            counts,
            threshold,
        }
    }

    fn shift(&self) -> u8 {
        self.title.is_some() as u8
    }

    /// The level for a line matching an outline entry of the given level.
    fn anchored(&self, outline_level: u8) -> u8 {
        (outline_level + self.shift()).min(6)
    }

    /// The level for headings recognised by weight rather than size.
    fn minor(&self) -> u8 {
        let deepest = if self.anchors.is_empty() {
            self.ranks.iter().map(|(_, level)| *level).max()
        } else {
            self.anchors
                .iter()
                .map(|(_, level)| self.anchored(*level))
                .max()
        };

        (deepest.unwrap_or(0) + 1).clamp(2, 6)
    }

    /// The level for a line that matches no outline entry, judged by size.
    fn of(&self, page: usize, line: &TextLine, text: &str) -> Option<u8> {
        let size = size_key(line.size);

        if size < self.threshold || !heading_like(text) {
            return None;
        }

        let Some((largest, _)) = self.anchors.first() else {
            return self
                .ranks
                .iter()
                .find(|(candidate, _)| *candidate == size)
                .map(|(_, level)| *level);
        };

        if size > *largest {
            // Bigger than every section heading: the title on the first
            // page, a figure label anywhere else.
            return (page == 0 && self.title == Some(size)).then_some(1);
        }

        if let Some((_, level)) = self.anchors.iter().find(|(anchor, _)| *anchor == size) {
            return Some(self.anchored(*level));
        }

        // A size the outline does not use: one step below the nearest larger
        // heading, provided it recurs.
        if self.counts.get(&size).copied().unwrap_or(0) < 2 {
            return None;
        }

        self.anchors
            .iter()
            .rev()
            .find(|(anchor, _)| *anchor > size)
            .map(|(_, level)| (self.anchored(*level) + 1).min(6))
    }
}

/// Whether a line could plausibly be a heading rather than a sentence or a
/// stray label.
fn heading_like(text: &str) -> bool {
    let length = text.chars().count();

    length <= 120
        && text.chars().filter(|ch| ch.is_alphabetic()).count() >= 2
        && !(length > 60 && text.trim_end().ends_with('.'))
}

/// The dominant font size by character count, in half points.
fn body_size(pages: &[Page]) -> u32 {
    let mut histogram: HashMap<u32, usize> = HashMap::new();

    for line in pages.iter().flat_map(|page| &page.lines) {
        *histogram.entry(size_key(line.size)).or_default() += line.chars();
    }

    histogram
        .into_iter()
        .max_by_key(|(size, count)| (*count, *size))
        .map(|(size, _)| size)
        .unwrap_or(24)
}

type RuleKey = (bool, i32, i32, i32);

fn rule_key(rule: &Rule) -> RuleKey {
    (
        rule.horizontal,
        rule.at.round() as i32,
        rule.from.round() as i32,
        rule.to.round() as i32,
    )
}

/// Rules drawn in the same place on page after page are page furniture (a
/// line under the running header, above the footer), not part of any table.
fn repeated_rules(pages: &[Page]) -> HashSet<RuleKey> {
    let mut counts: HashMap<RuleKey, usize> = HashMap::new();

    for page in pages {
        let keys: HashSet<RuleKey> = page.rules.iter().map(rule_key).collect();

        for key in keys {
            *counts.entry(key).or_default() += 1;
        }
    }

    let needed = (pages.len() / 3).max(3);

    counts
        .into_iter()
        .filter(|(_, count)| *count >= needed)
        .map(|(key, _)| key)
        .collect()
}

/// Finds running headers, footers and page numbers: lines in the top or
/// bottom margin whose text, ignoring digits, repeats across pages.
fn find_furniture(pages: &[Page]) -> HashSet<(usize, usize)> {
    let mut found = HashSet::new();

    if pages.len() < 3 {
        return found;
    }

    let key = |line: &TextLine| -> String {
        let mut key = String::new();
        let mut in_digits = false;

        for ch in line.text().chars() {
            if ch.is_ascii_digit() {
                if !in_digits {
                    key.push('#');
                }
                in_digits = true;
            } else if !ch.is_whitespace() {
                key.extend(ch.to_lowercase());
                in_digits = false;
            }
        }

        key
    };

    let in_margin = |page: &Page, line: &TextLine| {
        line.rect.y1 < page.height * 0.09 || line.rect.y0 > page.height * 0.91
    };

    let mut counts: HashMap<String, usize> = HashMap::new();

    for page in pages {
        let mut seen = HashSet::new();

        for line in page.lines.iter().filter(|line| in_margin(page, line)) {
            let key = key(line);

            if seen.insert(key.clone()) {
                *counts.entry(key).or_default() += 1;
            }
        }
    }

    let needed = (pages.len() / 3).max(3);

    for (page_index, page) in pages.iter().enumerate() {
        for (line_index, line) in page.lines.iter().enumerate() {
            if !in_margin(page, line) {
                continue;
            }

            let key = key(line);
            let lone_number = key == "#" || key == "-#-" || key == "page#" || key == "page#of#";

            if counts.get(&key).copied().unwrap_or(0) >= needed || (lone_number && pages.len() >= 3)
            {
                found.insert((page_index, line_index));
            }
        }
    }

    found
}

struct OutlineIndex {
    entries: HashMap<u32, Vec<(String, u8)>>,
}

impl OutlineIndex {
    fn new(outline: &[OutlineItem]) -> Self {
        let mut entries: HashMap<u32, Vec<(String, u8)>> = HashMap::new();

        for item in outline {
            let key = fold(&item.title);

            if let Some(page) = item.page
                && key.chars().count() >= 3
            {
                entries
                    .entry(page)
                    .or_default()
                    .push((key, (item.depth as u8 + 1).min(6)));
            }
        }

        Self { entries }
    }

    fn level(&self, page: u32, line: &str) -> Option<u8> {
        let entries = self.entries.get(&page)?;
        let line = fold(line);

        if line.is_empty() {
            return None;
        }

        entries
            .iter()
            .find(|(title, _)| {
                *title == line
                    // "3.1 Results" in the text against "Results" in the outline,
                    // or the reverse.
                    || (line.len() >= 6 && title.ends_with(&line) && line.len() * 2 >= title.len())
                    || (title.len() >= 6 && line.ends_with(title) && title.len() * 2 >= line.len())
            })
            .map(|(_, level)| *level)
    }
}

/// Lowercased alphanumerics only, for forgiving title comparison.
fn fold(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Splits a list marker off the front of a line.
fn list_marker(text: &str) -> Option<(Marker, usize)> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;

    if matches!(
        first,
        '•' | '◦'
            | '▪'
            | '■'
            | '●'
            | '○'
            | '‣'
            | '➢'
            | '►'
            | '▸'
            | '✓'
            | '∙'
            | '·'
            | '⁃'
    ) {
        let rest = &text[first.len_utf8()..];
        return Some((Marker::Bullet, text.len() - rest.trim_start().len()));
    }

    if matches!(first, '-' | '–' | '—' | '*') {
        let rest = &text[first.len_utf8()..];

        if rest.starts_with(' ') && !rest.trim().is_empty() {
            return Some((Marker::Bullet, text.len() - rest.trim_start().len()));
        }

        return None;
    }

    let digits = text.chars().take_while(char::is_ascii_digit).count();

    if (1..=3).contains(&digits) {
        let rest = &text[digits..];

        if let Some(after) = rest.strip_prefix(['.', ')'])
            && after.starts_with(' ')
            && !after.trim().is_empty()
        {
            let number = text[..digits].parse().ok()?;
            return Some((
                Marker::Ordered(number),
                text.len() - after.trim_start().len(),
            ));
        }
    }

    None
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end()
        .chars()
        .next_back()
        .is_some_and(|ch| matches!(ch, '.' | '!' | '?' | ':' | ';' | '"' | '”' | ')' | '。'))
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    x0: f32,
    y0: f32,
    y1: f32,
    size: f32,
}

#[derive(Debug)]
enum Open {
    Paragraph {
        spans: Vec<Span>,
    },
    Item {
        depth: u8,
        marker: Marker,
        spans: Vec<Span>,
        /// Left edge of the marker, to tell nesting apart.
        indent: f32,
    },
    Heading {
        level: u8,
        text: String,
    },
    Code {
        lines: Vec<String>,
    },
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    open: Option<Open>,
    last: Option<Geometry>,
    /// Marker indents of the enclosing list items.
    indents: Vec<f32>,
    crossed_page: bool,
    minor_level: u8,
}

impl Builder {
    fn page_break(&mut self) {
        self.crossed_page = true;
    }

    fn close(&mut self) {
        match self.open.take() {
            Some(Open::Paragraph { spans }) => self.blocks.push(Block::Paragraph(spans)),
            Some(Open::Item {
                depth,
                marker,
                spans,
                ..
            }) => self.blocks.push(Block::Item {
                depth,
                marker,
                spans,
            }),
            // A "heading" that turns out to be a wrapped sentence is body text.
            Some(Open::Heading { text, .. }) if !heading_like(&text) => {
                self.blocks
                    .push(Block::Paragraph(vec![Span { text, style: 0 }]));
            }
            Some(Open::Heading { level, text }) => self.blocks.push(Block::Heading { level, text }),
            Some(Open::Code { lines }) => self.blocks.push(Block::Code(lines)),
            None => {}
        }
    }

    fn table(&mut self, rows: Vec<Vec<Vec<Span>>>) {
        self.close();
        self.indents.clear();
        self.blocks.push(Block::Table(rows));
        // Whatever follows starts afresh rather than continuing a paragraph
        // from above the table.
        self.last = None;
    }

    fn push(
        &mut self,
        line: &TextLine,
        text: String,
        heading: Option<u8>,
        next: Option<&TextLine>,
        body: u32,
    ) {
        let geometry = Geometry {
            x0: line.rect.x0,
            y0: line.rect.y0,
            y1: line.rect.y1,
            size: line.size.max(1.0),
        };
        let previous = self.last.replace(geometry);
        let crossed_page = std::mem::take(&mut self.crossed_page);

        // How this line sits relative to the one before it.
        let (gap, same_size, moved_up) = match previous {
            Some(previous) if !crossed_page => (
                geometry.y0 - previous.y1,
                (geometry.size - previous.size).abs() <= 0.6,
                geometry.y0 < previous.y0 - previous.size,
            ),
            Some(previous) => (0.0, (geometry.size - previous.size).abs() <= 0.6, true),
            None => (0.0, false, false),
        };
        let spaced = gap > geometry.size * 0.9;
        let indented =
            previous.is_some_and(|previous| geometry.x0 > previous.x0 + geometry.size * 0.8);

        let heading = heading.or_else(|| {
            // A short bold line standing on its own reads as a minor heading.
            let stands_alone = (spaced || moved_up || previous.is_none())
                && next.is_none_or(|next| {
                    !next.all(BOLD) || next.rect.y0 - line.rect.y1 > line.size * 0.9
                });

            (line.all(BOLD)
                && size_key(line.size) >= body
                && stands_alone
                && text.chars().count() <= 90
                && !text.ends_with(['.', ',', ';'])
                && text.chars().any(char::is_alphabetic)
                && list_marker(&text).is_none())
            .then_some(self.minor_level.max(2))
        });

        if let Some(level) = heading {
            // A heading wrapped over several lines stays one heading.
            if let Some(Open::Heading {
                level: open_level,
                text: open_text,
            }) = &mut self.open
                && *open_level == level
                && !spaced
                && !moved_up
            {
                open_text.push(' ');
                open_text.push_str(text.trim());
                return;
            }

            self.close();
            self.indents.clear();
            self.open = Some(Open::Heading {
                level,
                text: text.trim().to_owned(),
            });
            return;
        }

        if line.all(MONO) {
            let code = text.clone();

            if let Some(Open::Code { lines }) = &mut self.open
                && !spaced
            {
                lines.push(code);
            } else {
                self.close();
                self.open = Some(Open::Code { lines: vec![code] });
            }
            return;
        }

        if let Some((marker, skip)) = list_marker(&text) {
            self.close();

            while self
                .indents
                .last()
                .is_some_and(|indent| geometry.x0 < indent - geometry.size * 0.5)
            {
                self.indents.pop();
            }

            if self
                .indents
                .last()
                .is_none_or(|indent| geometry.x0 > indent + geometry.size * 0.5)
            {
                self.indents.push(geometry.x0);
            }

            self.open = Some(Open::Item {
                depth: (self.indents.len().saturating_sub(1)).min(6) as u8,
                marker,
                spans: skip_prefix(&line.spans, skip),
                indent: geometry.x0,
            });
            return;
        }

        match &mut self.open {
            Some(Open::Item { spans, indent, .. })
                if !spaced && !moved_up && geometry.x0 > *indent + geometry.size * 0.3 =>
            {
                join(spans, &line.spans);
                return;
            }
            Some(Open::Paragraph { spans }) => {
                let previous_text: String = spans.iter().map(|span| span.text.as_str()).collect();
                let continues_sentence = !ends_sentence(&previous_text)
                    && text.chars().next().is_some_and(|ch| !ch.is_uppercase());

                let continues = if moved_up {
                    // New column or page: only carry on mid-sentence.
                    same_size && continues_sentence
                } else {
                    same_size && !spaced && !(indented && ends_sentence(&previous_text))
                };

                if continues {
                    join(spans, &line.spans);
                    return;
                }
            }
            _ => {}
        }

        self.close();

        if !matches!(self.blocks.last(), Some(Block::Item { .. })) || spaced {
            self.indents.clear();
        }

        self.open = Some(Open::Paragraph {
            spans: line.spans.clone(),
        });
    }

    fn finish(mut self) -> String {
        self.close();

        let mut out = String::new();
        let mut previous_was_item = false;

        for block in &self.blocks {
            let is_item = matches!(block, Block::Item { .. });

            if !out.is_empty() {
                out.push_str(if is_item && previous_was_item {
                    "\n"
                } else {
                    "\n\n"
                });
            }

            match block {
                Block::Heading { level, text } => {
                    out.push_str(&"#".repeat(*level as usize));
                    out.push(' ');
                    out.push_str(&escape(text, false));
                }
                Block::Paragraph(spans) => out.push_str(&inline(spans, true)),
                Block::Item {
                    depth,
                    marker,
                    spans,
                } => {
                    out.push_str(&"    ".repeat(*depth as usize));

                    match marker {
                        Marker::Bullet => out.push_str("- "),
                        Marker::Ordered(number) => out.push_str(&format!("{number}. ")),
                    }

                    out.push_str(&inline(spans, false));
                }
                Block::Code(lines) => {
                    out.push_str("```\n");
                    out.push_str(&lines.join("\n"));
                    out.push_str("\n```");
                }
                Block::Table(rows) => {
                    for (index, row) in rows.iter().enumerate() {
                        if index > 0 {
                            out.push('\n');
                        }

                        out.push('|');
                        for cell in row {
                            out.push(' ');
                            // A pipe inside a cell would end it.
                            out.push_str(&inline(cell, false).replace('|', "\\|"));
                            out.push_str(" |");
                        }

                        if index == 0 {
                            out.push_str("\n|");
                            out.push_str(&" --- |".repeat(row.len()));
                        }
                    }
                }
            }

            previous_was_item = is_item;
        }

        if !out.is_empty() {
            out.push('\n');
        }

        out
    }
}

/// Drops the first `bytes` bytes of text (a list marker) from a run of spans.
fn skip_prefix(spans: &[Span], mut bytes: usize) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());

    for span in spans {
        if bytes >= span.text.len() {
            bytes -= span.text.len();
            continue;
        }

        out.push(Span {
            text: span.text[bytes..].to_owned(),
            style: span.style,
        });
        bytes = 0;
    }

    out
}

/// Appends a line to a paragraph, undoing end-of-line hyphenation.
fn join(spans: &mut Vec<Span>, line: &[Span]) {
    let starts_lowercase = line
        .first()
        .and_then(|span| span.text.chars().next())
        .is_some_and(char::is_lowercase);

    let mut glue = " ";

    if let Some(last) = spans.last_mut() {
        let mut tail = last.text.chars().rev();
        let hyphenated = matches!(tail.next(), Some('-' | '\u{ad}'))
            && tail.next().is_some_and(char::is_alphabetic);

        // "English-" / "to-German" is a compound, not a broken word.
        let compound = line
            .first()
            .and_then(|span| span.text.split_whitespace().next())
            .is_some_and(|word| word.contains('-'));

        if hyphenated && starts_lowercase && !compound {
            last.text.pop();
            glue = "";
        }
    }

    for (index, span) in line.iter().enumerate() {
        let text = if index == 0 {
            format!("{glue}{}", span.text)
        } else {
            span.text.clone()
        };

        match spans.last_mut() {
            Some(last) if last.style == span.style => last.text.push_str(&text),
            _ => spans.push(Span {
                text,
                style: span.style,
            }),
        }
    }
}

fn inline(spans: &[Span], block_start: bool) -> String {
    let mut out = String::new();

    for (index, span) in spans.iter().enumerate() {
        let text = span.text.as_str();
        let core = text.trim();

        if core.is_empty() {
            out.push_str(text);
            continue;
        }

        // Emphasis markers must hug the text, so surrounding whitespace
        // moves outside them.
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];

        out.push_str(lead);

        if span.style & MONO != 0 {
            let fence = if core.contains('`') { "``" } else { "`" };
            out.push_str(fence);
            out.push_str(core);
            out.push_str(fence);
        } else {
            // Emphasis on a lone symbol (a footnote mark, an operator) is noise.
            let emphasised = core.chars().any(char::is_alphanumeric);

            let marker = match (
                emphasised && span.style & BOLD != 0,
                emphasised && span.style & ITALIC != 0,
            ) {
                (true, true) => "***",
                (true, false) => "**",
                (false, true) => "*",
                (false, false) => "",
            };

            out.push_str(marker);
            out.push_str(&escape(core, block_start && index == 0 && lead.is_empty()));
            out.push_str(marker);
        }

        out.push_str(trail);
    }

    out
}

/// Escapes characters Markdown would otherwise interpret.
fn escape(text: &str, block_start: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);

    for (index, ch) in chars.iter().copied().enumerate() {
        let previous = index.checked_sub(1).map(|index| chars[index]);
        let next = chars.get(index + 1).copied();

        let needs_escape = match ch {
            '\\' | '`' | '*' => true,
            // Intraword underscores are literal in CommonMark.
            '_' => {
                !(previous.is_some_and(char::is_alphanumeric)
                    && next.is_some_and(char::is_alphanumeric))
            }
            '<' => next.is_some_and(|next| next.is_ascii_alphabetic() || matches!(next, '/' | '!')),
            '#' | '>' | '+' | '-' => {
                block_start
                    && index == 0
                    && (ch == '>' || next.is_none_or(|next| next == ' ' || next == ch))
            }
            _ => false,
        };

        if needs_escape {
            out.push('\\');
        }

        out.push(ch);
    }

    // "12. " at the start of a paragraph would turn it into a list.
    if block_start {
        let digits = out.chars().take_while(char::is_ascii_digit).count();

        if digits > 0 && out[digits..].starts_with(['.', ')']) && out[digits + 1..].starts_with(' ')
        {
            out.insert(digits, '\\');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f32, y: f32, size: f32, style: u8) -> TextLine {
        TextLine {
            spans: vec![Span {
                text: text.into(),
                style,
            }],
            words: Vec::new(),
            rect: Rect {
                x0: x,
                y0: y,
                x1: x + 400.0,
                y1: y + size,
            },
            size,
        }
    }

    fn page(lines: Vec<TextLine>) -> Page {
        Page {
            lines,
            rules: Vec::new(),
            height: 800.0,
        }
    }

    #[test]
    fn infers_headings_from_font_size() {
        let pages = [page(vec![
            line("Title", 72.0, 100.0, 24.0, 0),
            line("Section", 72.0, 150.0, 16.0, 0),
            line(
                "Body text that is long enough to dominate the page.",
                72.0,
                180.0,
                10.0,
                0,
            ),
            line("It continues on a second line here.", 72.0, 192.0, 10.0, 0),
        ])];

        assert_eq!(
            render(&pages, &[]),
            "# Title\n\n## Section\n\nBody text that is long enough to dominate the page. \
             It continues on a second line here.\n"
        );
    }

    #[test]
    fn joins_hyphenated_lines() {
        let pages = [page(vec![
            line("A sentence with a hyphen-", 72.0, 100.0, 10.0, 0),
            line("ated word and a well-", 72.0, 112.0, 10.0, 0),
            line("Known name.", 72.0, 124.0, 10.0, 0),
        ])];

        assert_eq!(
            render(&pages, &[]),
            "A sentence with a hyphenated word and a well- Known name.\n"
        );
    }

    #[test]
    fn splits_paragraphs_on_vertical_space() {
        let pages = [page(vec![
            line("First paragraph.", 72.0, 100.0, 10.0, 0),
            line("Second paragraph.", 72.0, 130.0, 10.0, 0),
        ])];

        assert_eq!(
            render(&pages, &[]),
            "First paragraph.\n\nSecond paragraph.\n"
        );
    }

    #[test]
    fn renders_lists_with_nesting_and_continuations() {
        let pages = [page(vec![
            line(
                "Intro paragraph with enough text to set the body size.",
                72.0,
                100.0,
                10.0,
                0,
            ),
            line("• first item that wraps", 72.0, 130.0, 10.0, 0),
            line("onto another line", 84.0, 142.0, 10.0, 0),
            line("◦ nested item", 96.0, 154.0, 10.0, 0),
            line("2. numbered item", 72.0, 166.0, 10.0, 0),
        ])];

        assert_eq!(
            render(&pages, &[]),
            "Intro paragraph with enough text to set the body size.\n\n\
             - first item that wraps onto another line\n    - nested item\n2. numbered item\n"
        );
    }

    #[test]
    fn emphasises_styled_runs() {
        let pages = [page(vec![TextLine {
            spans: vec![
                Span {
                    text: "Plain ".into(),
                    style: 0,
                },
                Span {
                    text: "bold ".into(),
                    style: BOLD,
                },
                Span {
                    text: "code".into(),
                    style: MONO,
                },
                Span {
                    text: " and 2*3".into(),
                    style: 0,
                },
            ],
            words: Vec::new(),
            rect: Rect {
                x0: 72.0,
                y0: 100.0,
                x1: 400.0,
                y1: 110.0,
            },
            size: 10.0,
        }])];

        assert_eq!(render(&pages, &[]), "Plain **bold** `code` and 2\\*3\n");
    }

    #[test]
    fn drops_running_headers_and_page_numbers() {
        let pages: Vec<Page> = (1..=4)
            .map(|number| {
                page(vec![
                    line("My Book — Chapter", 72.0, 30.0, 9.0, 0),
                    line(&format!("Content of page {number}."), 72.0, 300.0, 10.0, 0),
                    line(&number.to_string(), 300.0, 770.0, 9.0, 0),
                ])
            })
            .collect();

        assert_eq!(
            render(&pages, &[]),
            "Content of page 1.\n\nContent of page 2.\n\nContent of page 3.\n\nContent of page 4.\n"
        );
    }

    #[test]
    fn uses_the_outline_for_heading_levels() {
        let outline = [OutlineItem {
            title: "Results".into(),
            depth: 1,
            page: Some(0),
        }];
        let pages = [page(vec![
            line("3.1 Results", 72.0, 100.0, 10.0, 0),
            line(
                "Body text follows the heading at the same size.",
                72.0,
                130.0,
                10.0,
                0,
            ),
        ])];

        assert_eq!(
            render(&pages, &outline),
            "## 3.1 Results\n\nBody text follows the heading at the same size.\n"
        );
    }

    #[test]
    fn escapes_markdown_syntax() {
        assert_eq!(escape("# not a heading", true), "\\# not a heading");
        assert_eq!(escape("1. not a list", true), "1\\. not a list");
        assert_eq!(
            escape("snake_case and _edge", false),
            "snake_case and \\_edge"
        );
        assert_eq!(escape("a <b> c < d", false), "a \\<b> c < d");
    }
}
