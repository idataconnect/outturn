use std::cell::RefCell;
use std::collections::HashMap;

use krilla::Document;
use krilla::color::rgb;
use krilla::geom::{PathBuilder, Point, Rect};
use krilla::metadata::Metadata;
use krilla::page::PageSettings;
use krilla::paint::Fill;
use krilla::text::{Font, TextDirection};
use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};

static REGULAR: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");
static BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold.ttf");
static ITALIC: &[u8] = include_bytes!("../fonts/DejaVuSans-Oblique.ttf");
static MONO: &[u8] = include_bytes!("../fonts/DejaVuSansMono.ttf");
/// Color emoji, as PNG bitmaps. Drawn for whatever the text faces lack and it
/// has, so an emoji in a report is an emoji rather than an empty box.
static EMOJI: &[u8] = include_bytes!("../fonts/Twemoji.ttf");

// A4, in points. Letter is an inch shorter and a little wider; A4 is what
// most of the world prints on, and either prints acceptably on the other.
const PAGE_W: f32 = 595.0;
const PAGE_H: f32 = 842.0;
const MARGIN: f32 = 56.0;
const BOTTOM: f32 = PAGE_H - MARGIN;
const MEASURE: f32 = PAGE_W - 2.0 * MARGIN;

const BODY: f32 = 10.5;
const CODE: f32 = 9.0;
const LEADING: f32 = 1.4;
const INDENT: f32 = 18.0;

type Color = (u8, u8, u8);
const INK: Color = (0x22, 0x22, 0x22);
const MUTED: Color = (0x66, 0x66, 0x66);
const LINK: Color = (0x1a, 0x55, 0xb0);
const RULE: Color = (0xcc, 0xcc, 0xcc);
const SHADE: Color = (0xf2, 0xf2, 0xf2);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum FaceId {
    Regular,
    Bold,
    Italic,
    Mono,
    Emoji,
}

/// A font and the advance of each character in it, in ems.
///
/// Measured from the font rather than from shaping: shaping is what draws,
/// but breaking a line only needs to know whether a word fits, and kerning
/// only ever makes a word narrower than its advances say.
struct Face {
    font: Font,
    data: &'static [u8],
    emoji: bool,
    advances: RefCell<HashMap<char, f32>>,
    covers: RefCell<HashMap<char, bool>>,
}

impl Face {
    fn new(data: &'static [u8], emoji: bool) -> Self {
        // From the static bytes, not a copy of them: the fonts are most of
        // what this component holds, and holding them twice is most of its
        // memory.
        let font = Font::new(data.into(), 0).expect("a bundled font did not parse");
        Face {
            font,
            data,
            emoji,
            advances: RefCell::default(),
            covers: RefCell::default(),
        }
    }

    /// An emoji font draws a sequence -- a family joined by joiners, a flag
    /// made of two letters -- as one glyph, so adding up its characters
    /// would count a family as seven. Every emoji in it is the same width, so
    /// a stretch is that width times the emoji it shows.
    fn emoji_width(&self, text: &str, size: f32) -> f32 {
        let one = self.char_width('😀');
        let mut count = 0;
        let mut after_joiner = false;
        let mut pending_flag = false;
        for c in text.chars() {
            let regional = ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
            if regional {
                // Two regional indicators are one flag.
                if !pending_flag {
                    count += 1;
                }
                pending_flag = !pending_flag;
            } else if !joins(c) && !after_joiner {
                count += 1;
                pending_flag = false;
            }
            after_joiner = c == '\u{200D}';
        }
        count as f32 * one * size
    }

    /// One character's advance, in ems.
    fn char_width(&self, c: char) -> f32 {
        *self.advances.borrow_mut().entry(c).or_insert_with(|| {
            let font = skrifa::FontRef::new(self.data).expect("bundled font");
            let upem = font
                .metrics(Size::unscaled(), LocationRef::default())
                .units_per_em as f32;
            let glyph = font.charmap().map(c).unwrap_or_default();
            font.glyph_metrics(Size::unscaled(), LocationRef::default())
                .advance_width(glyph)
                .unwrap_or(0.0)
                / upem
        })
    }

    fn has(&self, c: char) -> bool {
        *self.covers.borrow_mut().entry(c).or_insert_with(|| {
            skrifa::FontRef::new(self.data)
                .map(|f| f.charmap().map(c).is_some())
                .unwrap_or(false)
        })
    }

    fn width(&self, text: &str, size: f32) -> f32 {
        if self.emoji {
            return self.emoji_width(text, size);
        }
        text.chars().map(|c| self.char_width(c)).sum::<f32>() * size
    }
}

struct Faces([Face; 5]);

impl Faces {
    fn new() -> Self {
        Faces([
            Face::new(REGULAR, false),
            Face::new(BOLD, false),
            Face::new(ITALIC, false),
            Face::new(MONO, false),
            Face::new(EMOJI, true),
        ])
    }

    fn get(&self, id: FaceId) -> &Face {
        &self.0[id as usize]
    }

    /// A space beside text in `face`. The emoji font has none of its own, so
    /// a space next to an emoji is the body text's.
    fn space(&self, face: FaceId, size: f32) -> f32 {
        let face = if face == FaceId::Emoji {
            FaceId::Regular
        } else {
            face
        };
        self.get(face).width(" ", size)
    }

    /// Splits text into stretches each drawn in one face: `primary` where it
    /// has the character, the emoji face where it does not and that does.
    ///
    /// What joins an emoji to the next -- a zero-width joiner, a variation
    /// selector, a skin tone, a keycap -- stays with the emoji before it, so
    /// a family or a flag is one stretch the emoji font can shape as one. A
    /// character followed by the emoji variation selector asks to be an
    /// emoji, and is one where the emoji face has it, even if the text face
    /// does too: "1️⃣", "❤️".
    fn segments<'t>(&self, text: &'t str, primary: FaceId) -> Vec<(FaceId, &'t str)> {
        let main = self.get(primary);
        let emoji = self.get(FaceId::Emoji);
        let mut out: Vec<(FaceId, &'t str)> = Vec::new();
        let mut start = 0;
        let mut current: Option<FaceId> = None;
        let mut chars = text.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let next = chars.peek().map(|(_, n)| *n);
            let modifies_emoji = joins(c) && current == Some(FaceId::Emoji);
            let asks_for_emoji = next == Some('\u{FE0F}') && emoji.has(c);
            let face = if modifies_emoji || asks_for_emoji {
                FaceId::Emoji
            } else if main.has(c) || !emoji.has(c) {
                primary
            } else {
                FaceId::Emoji
            };
            if current.is_some_and(|f| f != face) {
                out.push((current.unwrap(), &text[start..i]));
                start = i;
            }
            current = Some(face);
        }
        if let Some(f) = current {
            out.push((f, &text[start..]));
        }
        out
    }
}

/// Whether a character modifies the one before it rather than standing alone.
fn joins(c: char) -> bool {
    matches!(c,
        '\u{200D}'                      // zero-width joiner
        | '\u{FE00}'..='\u{FE0F}'       // variation selectors
        | '\u{1F3FB}'..='\u{1F3FF}'     // skin tones
        | '\u{20E3}'                    // combining keycap
        | '\u{E0020}'..='\u{E007F}'     // tag sequences, as in subdivision flags
    ) || ('\u{1F1E6}'..='\u{1F1FF}').contains(&c) // regional indicators, paired into flags
}

/// One thing drawn on a page. Laid out first and painted after, so that page
/// numbers can say how many pages there are.
enum Op {
    Text {
        x: f32,
        y: f32,
        face: FaceId,
        size: f32,
        color: Color,
        text: String,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Color,
    },
    Link {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        url: String,
    },
}

/// A stretch of inline text in one style.
#[derive(Clone, Debug)]
struct Run {
    text: String,
    face: FaceId,
    color: Color,
    link: Option<String>,
}

/// A word, or the part of one that is in a single style.
struct Piece<'a> {
    text: &'a str,
    run: &'a Run,
    /// Whether a line may break before this piece. False joins it to the
    /// piece before -- `**bold**ly` is one word in two styles.
    breakable: bool,
    /// Whether the source said to break here regardless.
    forced: bool,
}

fn pieces(runs: &[Run]) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut after_space = true;
    let mut forced = false;
    for run in runs {
        if run.text == "\n" {
            forced = true;
            after_space = true;
            continue;
        }
        let mut rest = run.text.as_str();
        while !rest.is_empty() {
            let trimmed = rest.trim_start();
            if trimmed.len() != rest.len() {
                after_space = true;
                rest = trimmed;
                continue;
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            out.push(Piece {
                text: &rest[..end],
                run,
                breakable: after_space,
                forced,
            });
            forced = false;
            after_space = false;
            rest = &rest[end..];
        }
    }
    out
}

/// A piece placed on a line, by its offset from the line's start.
struct Placed {
    dx: f32,
    text: String,
    face: FaceId,
    color: Color,
    link: Option<String>,
    width: f32,
}

/// Breaks runs into lines no wider than `measure`.
fn wrap(faces: &Faces, runs: &[Run], size: f32, measure: f32) -> Vec<Vec<Placed>> {
    let runs: Vec<Run> = runs
        .iter()
        .flat_map(|r| {
            faces
                .segments(&r.text, r.face)
                .into_iter()
                .map(|(face, text)| Run {
                    text: text.to_string(),
                    face,
                    color: r.color,
                    link: r.link.clone(),
                })
        })
        .collect();
    let pieces = pieces(&runs);

    // Words: a breakable piece and the unbreakable ones after it.
    let mut words: Vec<(bool, Vec<&Piece>)> = Vec::new();
    for p in &pieces {
        if p.breakable || words.is_empty() {
            words.push((p.forced, vec![p]));
        } else {
            words.last_mut().unwrap().1.push(p);
        }
    }

    let mut lines: Vec<Vec<Placed>> = vec![Vec::new()];
    let mut x = 0.0;

    for (forced, word) in words {
        let width: f32 = word
            .iter()
            .map(|p| faces.get(p.run.face).width(p.text, size))
            .sum();
        let line_empty = lines.last().unwrap().is_empty();
        // The space in the face of the word before it: the space that word's
        // run puts there itself when the two are drawn as one, and the
        // narrower choice before code, whose spaces are as wide as its letters.
        let before = lines
            .last()
            .unwrap()
            .last()
            .map_or(word[0].run.face, |p| p.face);
        let space = faces.space(before, size);
        let gap = if line_empty { 0.0 } else { space };

        if forced || (!line_empty && x + gap + width > measure) {
            lines.push(Vec::new());
            x = 0.0;
        } else {
            x += gap;
        }

        if width <= measure {
            for p in word {
                let w = faces.get(p.run.face).width(p.text, size);
                lines.last_mut().unwrap().push(Placed {
                    dx: x,
                    text: p.text.to_string(),
                    face: p.run.face,
                    color: p.run.color,
                    link: p.run.link.clone(),
                    width: w,
                });
                x += w;
            }
            continue;
        }

        // Wider than a whole line -- a URL, a hash, a path. Broken wherever
        // it has to be, because the alternative is text off the page.
        for p in word {
            let face = faces.get(p.run.face);
            let mut chunk = String::new();
            let mut chunk_w = 0.0;
            for c in p.text.chars() {
                let cw = face.width(c.encode_utf8(&mut [0; 4]), size);
                if x + chunk_w + cw > measure && (x > 0.0 || !chunk.is_empty()) {
                    if !chunk.is_empty() {
                        lines.last_mut().unwrap().push(Placed {
                            dx: x,
                            text: std::mem::take(&mut chunk),
                            face: p.run.face,
                            color: p.run.color,
                            link: p.run.link.clone(),
                            width: chunk_w,
                        });
                    }
                    lines.push(Vec::new());
                    x = 0.0;
                    chunk_w = 0.0;
                }
                chunk.push(c);
                chunk_w += cw;
            }
            if !chunk.is_empty() {
                lines.last_mut().unwrap().push(Placed {
                    dx: x,
                    text: chunk,
                    face: p.run.face,
                    color: p.run.color,
                    link: p.run.link.clone(),
                    width: chunk_w,
                });
                x += chunk_w;
            }
        }
    }

    if lines.len() > 1 && lines.last().unwrap().is_empty() {
        lines.pop();
    }
    lines
}

/// Joins the pieces of a line that share a style into one, spaces included.
///
/// Drawing is where a render's time goes, and each draw shapes its text
/// afresh: a paragraph drawn word by word costs ten times what it costs drawn
/// a run at a time. Layout placed each word so that this join lands it where
/// it was put -- the gap before a word is a space in that word's face.
fn merge(faces: &Faces, line: &[Placed], size: f32) -> Vec<Placed> {
    let mut out: Vec<Placed> = Vec::new();
    for p in line {
        if let Some(last) = out.last_mut()
            && last.face == p.face
            && last.color == p.color
            && last.link == p.link
        {
            let gap = p.dx - (last.dx + last.width);
            let space = faces.space(last.face, size);
            let joiner = if gap.abs() < 0.01 {
                Some("")
            } else if (gap - space).abs() < 0.01 {
                Some(" ")
            } else {
                None
            };
            if let Some(j) = joiner {
                last.text.push_str(j);
                last.text.push_str(&p.text);
                last.width = p.dx + p.width - last.dx;
                continue;
            }
        }
        out.push(Placed {
            dx: p.dx,
            text: p.text.clone(),
            face: p.face,
            color: p.color,
            link: p.link.clone(),
            width: p.width,
        });
    }
    out
}

/// Where a block's lines go, and what decorates them.
#[derive(Clone, Copy)]
struct Frame {
    x: f32,
    measure: f32,
    /// Block quotes enclosing this block, each drawn as a bar in the margin.
    quotes: usize,
}

/// The page being laid out, handed on whole as the next one starts.
///
/// One page is held at a time, never the document: what a render holds does
/// not grow with how long the document is, beyond what the PDF writer keeps.
struct Pages<'f, 's> {
    faces: &'f Faces,
    current: Vec<Op>,
    finished: &'s mut dyn FnMut(Vec<Op>),
    count: usize,
    y: f32,
}

impl<'f, 's> Pages<'f, 's> {
    fn new(faces: &'f Faces, finished: &'s mut dyn FnMut(Vec<Op>)) -> Self {
        Pages {
            faces,
            current: Vec::new(),
            finished,
            count: 0,
            y: MARGIN,
        }
    }

    fn ops(&mut self) -> &mut Vec<Op> {
        &mut self.current
    }

    fn end_page(&mut self) {
        (self.finished)(std::mem::take(&mut self.current));
        self.count += 1;
    }

    /// Moves to a new page unless `height` still fits on this one. A block
    /// that starts a page never moves, so nothing taller than a page loops.
    fn room_for(&mut self, height: f32) {
        if self.y + height > BOTTOM && self.y > MARGIN {
            self.end_page();
            self.y = MARGIN;
        }
    }

    fn gap(&mut self, height: f32) {
        // Space after the last block on a page is not carried to the next.
        if self.y > MARGIN {
            self.y += height;
        }
    }

    fn quote_bars(&mut self, frame: Frame, top: f32, height: f32) {
        for q in 0..frame.quotes {
            let x = MARGIN + q as f32 * INDENT + 2.0;
            self.ops().push(Op::Rect {
                x,
                y: top,
                w: 2.0,
                h: height,
                color: RULE,
            });
        }
    }

    /// Lays out a paragraph-like block, with an optional marker -- a bullet
    /// or a number -- hung in the indent before its first line.
    fn text_block(
        &mut self,
        runs: &[Run],
        size: f32,
        frame: Frame,
        marker: Option<String>,
        shade: bool,
    ) {
        let lines = wrap(self.faces, runs, size, frame.measure);
        let leading = size * LEADING;
        for (i, line) in lines.iter().enumerate() {
            self.room_for(leading);
            let top = self.y;
            let baseline = top + size * 1.1;
            if shade {
                self.ops().push(Op::Rect {
                    x: frame.x - 4.0,
                    y: top,
                    w: frame.measure + 8.0,
                    h: leading,
                    color: SHADE,
                });
            }
            self.quote_bars(frame, top, leading);
            if i == 0
                && let Some(m) = &marker
            {
                let w = self.faces.get(FaceId::Regular).width(m, size);
                self.ops().push(Op::Text {
                    x: frame.x - w - 6.0,
                    y: baseline,
                    face: FaceId::Regular,
                    size,
                    color: INK,
                    text: m.clone(),
                });
            }
            for p in merge(self.faces, line, size) {
                if let Some(url) = &p.link {
                    self.ops().push(Op::Link {
                        x: frame.x + p.dx,
                        y: top,
                        w: p.width,
                        h: leading,
                        url: url.clone(),
                    });
                }
                self.ops().push(Op::Text {
                    x: frame.x + p.dx,
                    y: baseline,
                    face: p.face,
                    size,
                    color: p.color,
                    text: p.text,
                });
            }
            self.y += leading;
        }
    }

    fn code_block(&mut self, text: &str, frame: Frame) {
        let size = CODE;
        let leading = size * LEADING;
        let face = self.faces.get(FaceId::Mono);
        // Code keeps its own line breaks, and a line too long for the page is
        // wrapped at the measure rather than lost off its edge.
        let per_line = (frame.measure / face.width("m", size)).floor().max(1.0) as usize;
        let mut lines: Vec<String> = Vec::new();
        for line in text.trim_end_matches('\n').split('\n') {
            let expanded = line.replace('\t', "    ");
            let chars: Vec<char> = expanded.chars().collect();
            if chars.is_empty() {
                lines.push(String::new());
            }
            for chunk in chars.chunks(per_line) {
                lines.push(chunk.iter().collect());
            }
        }
        for line in lines {
            self.room_for(leading);
            let top = self.y;
            self.ops().push(Op::Rect {
                x: frame.x - 4.0,
                y: top,
                w: frame.measure + 8.0,
                h: leading,
                color: SHADE,
            });
            self.quote_bars(frame, top, leading);
            let mut x = frame.x;
            for (face, text) in self.faces.segments(&line, FaceId::Mono) {
                let w = self.faces.get(face).width(text, size);
                self.ops().push(Op::Text {
                    x,
                    y: top + size * 1.1,
                    face,
                    size,
                    color: INK,
                    text: text.to_string(),
                });
                x += w;
            }
            self.y += leading;
        }
    }

    fn rule(&mut self, frame: Frame) {
        self.room_for(BODY);
        let y = self.y + BODY / 2.0;
        self.ops().push(Op::Rect {
            x: frame.x,
            y,
            w: frame.measure,
            h: 0.75,
            color: RULE,
        });
        self.y += BODY;
    }

    fn table(&mut self, rows: &[(bool, Vec<Vec<Run>>)], aligns: &[Alignment], frame: Frame) {
        let columns = rows.iter().map(|(_, r)| r.len()).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let size = BODY - 1.0;
        let leading = size * LEADING;
        let pad = 4.0;

        // Each column gets what its widest cell would take on one line, until
        // that is more than there is; then each keeps a fair floor and the
        // rest is shared by what they asked for.
        let mut natural = vec![0.0f32; columns];
        for (_, cells) in rows {
            for (i, cell) in cells.iter().enumerate() {
                let w: f32 = cell
                    .iter()
                    .map(|r| self.faces.get(r.face).width(&r.text, size))
                    .sum();
                natural[i] = natural[i].max(w + 2.0 * pad);
            }
        }
        let total: f32 = natural.iter().sum();
        let widths: Vec<f32> = if total <= frame.measure {
            natural
        } else {
            let floor = frame.measure / columns as f32 / 2.0;
            let spare = frame.measure - floor * columns as f32;
            natural.iter().map(|n| floor + spare * n / total).collect()
        };

        for (header, cells) in rows {
            let wrapped: Vec<Vec<Vec<Placed>>> = (0..columns)
                .map(|i| {
                    let cell = cells.get(i).map(Vec::as_slice).unwrap_or(&[]);
                    wrap(self.faces, cell, size, (widths[i] - 2.0 * pad).max(1.0))
                })
                .collect();
            let lines = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
            let height = lines as f32 * leading + pad;
            self.room_for(height);
            let top = self.y;
            if *header {
                self.ops().push(Op::Rect {
                    x: frame.x,
                    y: top,
                    w: widths.iter().sum(),
                    h: height,
                    color: SHADE,
                });
            }
            self.quote_bars(frame, top, height);
            let mut x = frame.x;
            for (i, cell) in wrapped.iter().enumerate() {
                for (n, line) in cell.iter().enumerate() {
                    let baseline = top + pad / 2.0 + n as f32 * leading + size * 1.1;
                    let used = line.last().map(|p| p.dx + p.width).unwrap_or(0.0);
                    let slack = (widths[i] - 2.0 * pad - used).max(0.0);
                    let shift = match aligns.get(i) {
                        Some(Alignment::Right) => slack,
                        Some(Alignment::Center) => slack / 2.0,
                        _ => 0.0,
                    };
                    for p in merge(self.faces, line, size) {
                        self.ops().push(Op::Text {
                            x: x + pad + shift + p.dx,
                            y: baseline,
                            face: p.face,
                            size,
                            color: p.color,
                            text: p.text,
                        });
                    }
                }
                x += widths[i];
            }
            self.y += height;
            let y = self.y - 0.5;
            self.ops().push(Op::Rect {
                x: frame.x,
                y,
                w: widths.iter().sum(),
                h: 0.5,
                color: RULE,
            });
        }
    }
}

/// What the parser is inside of, outermost first.
enum Container {
    List { next: Option<u64> },
    Item { marker: Option<String> },
    Quote,
}

/// Inline style, as nested spans set it.
#[derive(Default)]
struct Inline {
    strong: usize,
    emphasis: usize,
    links: Vec<String>,
}

impl Inline {
    fn run(&self, text: &str, code: bool) -> Run {
        let face = if code {
            FaceId::Mono
        } else if self.strong > 0 {
            FaceId::Bold
        } else if self.emphasis > 0 {
            FaceId::Italic
        } else {
            FaceId::Regular
        };
        let link = self.links.last().cloned();
        Run {
            text: text.to_string(),
            face,
            color: if link.is_some() { LINK } else { INK },
            link,
        }
    }
}

fn frame_for(stack: &[Container]) -> Frame {
    let mut x = MARGIN;
    let mut quotes = 0;
    for c in stack {
        match c {
            Container::List { .. } => x += INDENT,
            Container::Quote => {
                x += INDENT;
                quotes += 1;
            }
            Container::Item { .. } => {}
        }
    }
    Frame {
        x,
        measure: (MARGIN + MEASURE - x).max(INDENT),
        quotes,
    }
}

/// The marker waiting to be drawn beside the next block in the innermost
/// list item, taken so it is drawn once.
fn take_marker(stack: &mut [Container]) -> Option<String> {
    stack.iter_mut().rev().find_map(|c| match c {
        Container::Item { marker } => Some(marker.take()),
        _ => None,
    })?
}

/// Renders markdown as a PDF.
///
/// In two passes. The first only counts pages, so that each page can say how
/// many there are; the second lays out again and draws each page as it is
/// finished. Layout depends on nothing but the markdown and the fonts, so the
/// passes break pages in the same places, and laying out twice is cheap next
/// to drawing once.
pub fn render(markdown: &str) -> Result<Vec<u8>, String> {
    let faces = Faces::new();
    let (total, title) = lay_out(markdown, &faces, &mut |_| {});

    let mut doc = Document::new();
    let mut metadata = Metadata::new().producer("outturn".to_string());
    if let Some(t) = title {
        metadata = metadata.title(t);
    }
    doc.set_metadata(metadata);

    let mut n = 0;
    let mut failed: Option<String> = None;
    lay_out(markdown, &faces, &mut |ops| {
        n += 1;
        if failed.is_none()
            && let Err(e) = paint_page(&mut doc, &faces, ops, n, total)
        {
            failed = Some(e);
        }
    });
    if let Some(e) = failed {
        return Err(e);
    }
    doc.finish()
        .map_err(|e| format!("the PDF could not be written: {e:?}"))
}

/// Lays out a document, handing each page to `finished` as it is completed.
/// Says how many pages there were, and the document's title if it has one.
fn lay_out(
    markdown: &str,
    faces: &Faces,
    finished: &mut dyn FnMut(Vec<Op>),
) -> (usize, Option<String>) {
    let mut pages = Pages::new(faces, finished);

    let mut stack: Vec<Container> = Vec::new();
    let mut inline = Inline::default();
    let mut runs: Vec<Run> = Vec::new();
    let mut heading: Option<f32> = None;
    let mut code: Option<String> = None;
    let mut table: Option<Vec<(bool, Vec<Vec<Run>>)>> = None;
    let mut aligns: Vec<Alignment> = Vec::new();
    let mut in_head = false;
    let mut title: Option<String> = None;
    let mut image_alt = false;

    // Lays out whatever inline text has gathered as one block.
    let flush = |runs: &mut Vec<Run>,
                 stack: &mut Vec<Container>,
                 pages: &mut Pages,
                 heading: Option<f32>,
                 quoted_color: bool| {
        if runs.iter().all(|r| r.text.trim().is_empty()) {
            runs.clear();
            return;
        }
        let frame = frame_for(stack);
        let marker = take_marker(stack);
        if quoted_color {
            for r in runs.iter_mut() {
                if r.link.is_none() {
                    r.color = MUTED;
                }
            }
        }
        match heading {
            Some(size) => {
                pages.gap(size * 0.6);
                // A heading alone at the foot of a page belongs on the next.
                pages.room_for(size * LEADING + BODY * LEADING * 2.0);
                for r in runs.iter_mut() {
                    if r.face == FaceId::Regular || r.face == FaceId::Italic {
                        r.face = FaceId::Bold;
                    }
                }
                pages.text_block(runs, size, frame, marker, false);
                pages.gap(size * 0.25);
            }
            None => {
                pages.text_block(runs, BODY, frame, marker, false);
                pages.gap(BODY * 0.6);
            }
        }
        runs.clear();
    };

    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        let quoted = stack.iter().any(|c| matches!(c, Container::Quote));
        match event {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                if table.is_none() {
                    flush(&mut runs, &mut stack, &mut pages, None, quoted);
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut runs, &mut stack, &mut pages, None, quoted);
                heading = Some(match level {
                    HeadingLevel::H1 => 20.0,
                    HeadingLevel::H2 => 15.5,
                    HeadingLevel::H3 => 12.5,
                    _ => BODY,
                });
            }
            Event::End(TagEnd::Heading(level)) => {
                if level == HeadingLevel::H1 && title.is_none() {
                    title = Some(runs.iter().map(|r| r.text.as_str()).collect());
                }
                flush(&mut runs, &mut stack, &mut pages, heading.take(), quoted);
            }
            Event::Start(Tag::List(start)) => {
                // A nested list ends the item text before it.
                flush(&mut runs, &mut stack, &mut pages, None, quoted);
                stack.push(Container::List { next: start });
            }
            Event::End(TagEnd::List(_)) => {
                flush(&mut runs, &mut stack, &mut pages, None, quoted);
                stack.pop();
                if !stack.iter().any(|c| matches!(c, Container::List { .. })) {
                    pages.gap(BODY * 0.4);
                }
            }
            Event::Start(Tag::Item) => {
                let marker = match stack.last_mut() {
                    Some(Container::List { next: Some(n) }) => {
                        let m = format!("{n}.");
                        *n += 1;
                        m
                    }
                    _ => "•".to_string(),
                };
                stack.push(Container::Item {
                    marker: Some(marker),
                });
            }
            Event::End(TagEnd::Item) => {
                // A tight list's items hold text with no paragraph around it.
                if !runs.is_empty() {
                    flush(&mut runs, &mut stack, &mut pages, None, quoted);
                    // Tight items sit closer than paragraphs do.
                    pages.y -= BODY * 0.35;
                }
                stack.pop();
            }
            Event::TaskListMarker(done) => {
                if let Some(Container::Item { marker }) = stack.last_mut() {
                    *marker = Some(if done { "☑" } else { "☐" }.to_string());
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                flush(&mut runs, &mut stack, &mut pages, None, quoted);
                stack.push(Container::Quote);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut runs, &mut stack, &mut pages, None, quoted);
                stack.pop();
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut runs, &mut stack, &mut pages, None, quoted);
                code = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(text) = code.take() {
                    let frame = frame_for(&stack);
                    // A code block opening a list item still gets its marker.
                    if let Some(m) = take_marker(&mut stack) {
                        let marker_run = vec![Run {
                            text: String::new(),
                            face: FaceId::Regular,
                            color: INK,
                            link: None,
                        }];
                        pages.text_block(&marker_run, BODY, frame, Some(m), false);
                    }
                    pages.code_block(&text, frame);
                    pages.gap(BODY * 0.6);
                }
            }
            Event::Rule => {
                flush(&mut runs, &mut stack, &mut pages, None, quoted);
                pages.rule(frame_for(&stack));
            }
            Event::Start(Tag::Table(a)) => {
                flush(&mut runs, &mut stack, &mut pages, None, quoted);
                aligns = a;
                table = Some(Vec::new());
            }
            Event::End(TagEnd::Table) => {
                if let Some(rows) = table.take() {
                    pages.table(&rows, &aligns, frame_for(&stack));
                    pages.gap(BODY * 0.8);
                }
            }
            Event::Start(Tag::TableHead) => {
                in_head = true;
                if let Some(t) = &mut table {
                    t.push((true, Vec::new()));
                }
            }
            Event::End(TagEnd::TableHead) => in_head = false,
            Event::Start(Tag::TableRow) => {
                if let Some(t) = &mut table {
                    t.push((in_head, Vec::new()));
                }
            }
            Event::Start(Tag::TableCell) => runs.clear(),
            Event::End(TagEnd::TableCell) => {
                if let Some((header, cells)) = table.as_mut().and_then(|t| t.last_mut()) {
                    let mut cell = std::mem::take(&mut runs);
                    if *header {
                        for r in &mut cell {
                            if r.face == FaceId::Regular || r.face == FaceId::Italic {
                                r.face = FaceId::Bold;
                            }
                        }
                    }
                    cells.push(cell);
                }
            }
            Event::Start(Tag::Strong) => inline.strong += 1,
            Event::End(TagEnd::Strong) => inline.strong -= 1,
            Event::Start(Tag::Emphasis) => inline.emphasis += 1,
            Event::End(TagEnd::Emphasis) => inline.emphasis -= 1,
            Event::Start(Tag::Link { dest_url, .. }) => inline.links.push(dest_url.to_string()),
            Event::End(TagEnd::Link) => {
                inline.links.pop();
            }
            Event::Start(Tag::Image { .. }) => {
                // Images are not drawn yet; the alt text stands in, marked so
                // a reader knows something was there.
                image_alt = true;
                runs.push(Run {
                    text: "[image: ".into(),
                    face: FaceId::Italic,
                    color: MUTED,
                    link: None,
                });
            }
            Event::End(TagEnd::Image) => {
                image_alt = false;
                runs.push(Run {
                    text: "]".into(),
                    face: FaceId::Italic,
                    color: MUTED,
                    link: None,
                });
            }
            Event::Text(text) => {
                if let Some(c) = &mut code {
                    c.push_str(&text);
                } else if image_alt {
                    runs.push(Run {
                        text: text.to_string(),
                        face: FaceId::Italic,
                        color: MUTED,
                        link: None,
                    });
                } else {
                    runs.push(inline.run(&text, false));
                }
            }
            Event::Code(text) => runs.push(inline.run(&text, true)),
            Event::SoftBreak => runs.push(inline.run(" ", false)),
            Event::HardBreak => runs.push(inline.run("\n", false)),
            // Raw HTML has no meaning here, and showing its tags would be
            // worse than dropping them.
            _ => {}
        }
    }
    let quoted = stack.iter().any(|c| matches!(c, Container::Quote));
    flush(&mut runs, &mut stack, &mut pages, None, quoted);
    pages.end_page();

    (pages.count, title)
}

fn fill(color: Color) -> Fill {
    Fill {
        paint: rgb::Color::new(color.0, color.1, color.2).into(),
        ..Default::default()
    }
}

/// Draws one page. `n` counts from one.
fn paint_page(
    doc: &mut Document,
    faces: &Faces,
    ops: Vec<Op>,
    n: usize,
    count: usize,
) -> Result<(), String> {
    let settings = PageSettings::from_wh(PAGE_W, PAGE_H).ok_or("bad page size")?;
    let mut page = doc.start_page_with(settings);
    let mut links = Vec::new();
    {
        let mut surface = page.surface();
        for op in ops {
            match op {
                Op::Text {
                    x,
                    y,
                    face,
                    size,
                    color,
                    text,
                } => {
                    if text.is_empty() {
                        continue;
                    }
                    surface.set_fill(Some(fill(color)));
                    surface.draw_text(
                        Point::from_xy(x, y),
                        faces.get(face).font.clone(),
                        size,
                        &text,
                        false,
                        TextDirection::Auto,
                    );
                }
                Op::Rect { x, y, w, h, color } => {
                    let Some(rect) = Rect::from_xywh(x, y, w, h) else {
                        continue;
                    };
                    let mut path = PathBuilder::new();
                    path.push_rect(rect);
                    if let Some(path) = path.finish() {
                        surface.set_fill(Some(fill(color)));
                        surface.draw_path(&path);
                    }
                }
                Op::Link { x, y, w, h, url } => links.push((x, y, w, h, url)),
            }
        }
        if count > 1 {
            let label = format!("{n} / {count}");
            let w = faces.get(FaceId::Regular).width(&label, 8.0);
            surface.set_fill(Some(fill(MUTED)));
            surface.draw_text(
                Point::from_xy((PAGE_W - w) / 2.0, PAGE_H - MARGIN / 2.0),
                faces.get(FaceId::Regular).font.clone(),
                8.0,
                &label,
                false,
                TextDirection::Auto,
            );
        }
        surface.finish();
    }
    for (x, y, w, h, url) in links {
        use krilla::action::LinkAction;
        use krilla::annotation::{LinkAnnotation, Target};
        if let Some(rect) = Rect::from_xywh(x, y, w, h) {
            let target = Target::Action(LinkAction::new(url).into());
            page.add_annotation(LinkAnnotation::new(rect, target).into());
        }
    }
    page.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(text: &str) -> Vec<Run> {
        vec![Run {
            text: text.into(),
            face: FaceId::Regular,
            color: INK,
            link: None,
        }]
    }

    #[test]
    fn lines_never_exceed_the_measure() {
        let faces = Faces::new();
        let text = "the quick brown fox jumps over the lazy dog ".repeat(40);
        for line in wrap(&faces, &runs(&text), BODY, 200.0) {
            let end = line.last().map(|p| p.dx + p.width).unwrap_or(0.0);
            assert!(end <= 200.0 + 0.01, "a line ran to {end}");
        }
    }

    #[test]
    fn a_word_wider_than_the_line_is_broken_not_lost() {
        let faces = Faces::new();
        let url = "https://example.com/".to_string() + &"a".repeat(300);
        let lines = wrap(&faces, &runs(&url), BODY, 150.0);
        assert!(lines.len() > 1);
        let joined: String = lines.iter().flatten().map(|p| p.text.as_str()).collect();
        assert_eq!(joined, url);
    }

    #[test]
    fn a_word_in_two_styles_stays_together() {
        let faces = Faces::new();
        let mut r = runs("aaaa ");
        r.push(Run {
            text: "bold".into(),
            face: FaceId::Bold,
            color: INK,
            link: None,
        });
        r.extend(runs("ly"));
        // Just wide enough for "aaaa" and not for the rest.
        let measure = faces.get(FaceId::Regular).width("aaaa bo", BODY);
        let lines = wrap(&faces, &r, BODY, measure);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].len(), 2, "bold and ly were split across lines");
    }

    #[test]
    fn renders_every_block_kind() {
        let md = "# Title\n\nSome **bold**, *italic*, `code` and [a link](https://example.com).\n\n\
                  - one\n- two\n  1. nested\n  2. again\n\n> quoted\n\n```\nfn main() {}\n```\n\n\
                  | a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n- [x] done\n- [ ] not\n\n![alt](x.png)\n";
        let pdf = render(md).expect("rendered");
        assert!(pdf.starts_with(b"%PDF"));
        let (_, title) = lay_out(md, &Faces::new(), &mut |_| {});
        assert_eq!(title.as_deref(), Some("Title"));
    }

    #[test]
    fn empty_input_is_still_a_document() {
        assert!(render("").expect("rendered").starts_with(b"%PDF"));
    }

    #[test]
    fn long_input_paginates() {
        let md = "A paragraph of moderate length that wraps a line or two. ".repeat(20) + "\n\n";
        let (pages, _) = lay_out(&md.repeat(60), &Faces::new(), &mut |_| {});
        assert!(pages > 3, "only {pages} pages");
    }

    #[test]
    fn a_line_in_one_style_is_drawn_as_one_piece() {
        let faces = Faces::new();
        let lines = wrap(&faces, &runs("one two three four"), BODY, 400.0);
        let merged = merge(&faces, &lines[0], BODY);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "one two three four");
    }

    #[test]
    fn a_change_of_style_starts_a_new_piece() {
        let faces = Faces::new();
        let mut r = runs("plain ");
        r.push(Run {
            text: "bold".into(),
            face: FaceId::Bold,
            color: INK,
            link: None,
        });
        r.extend(runs(" plain again"));
        let lines = wrap(&faces, &r, BODY, 400.0);
        let merged = merge(&faces, &lines[0], BODY);
        let texts: Vec<&str> = merged.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["plain", "bold", "plain again"]);
    }

    #[test]
    fn both_passes_break_pages_alike() {
        let md = "Words enough to wrap across a line or two of the page. ".repeat(30) + "\n\n";
        let md = md.repeat(40);
        let faces = Faces::new();
        let mut first = Vec::new();
        lay_out(&md, &faces, &mut |ops| first.push(ops.len()));
        let mut second = Vec::new();
        lay_out(&md, &faces, &mut |ops| second.push(ops.len()));
        assert_eq!(first, second);
    }

    #[test]
    fn emoji_fall_back_to_the_emoji_face_and_text_does_not() {
        let faces = Faces::new();
        let segs = faces.segments("Fog ahead 🌫️ then sun ☀️!", FaceId::Regular);
        let emoji: Vec<&str> = segs
            .iter()
            .filter(|(f, _)| *f == FaceId::Emoji)
            .map(|(_, t)| *t)
            .collect();
        assert_eq!(emoji, ["🌫️", "☀️"]);
        assert!(
            segs.iter()
                .any(|(f, t)| *f == FaceId::Regular && t.contains("Fog ahead"))
        );
    }

    #[test]
    fn a_joined_emoji_stays_one_stretch() {
        let faces = Faces::new();
        // A family: four people and three joiners.
        let family = "👨\u{200D}👩\u{200D}👧\u{200D}👦";
        let segs = faces.segments(family, FaceId::Regular);
        assert_eq!(segs, [(FaceId::Emoji, family)]);
    }

    #[test]
    fn a_document_with_emoji_renders() {
        let pdf = render(
            "# Karl 🌫️\n\nBirds 🐦 judging you 👀\n\n| Mood | Icon |\n|---|---|\n| Grumpy | 😾 |\n",
        )
        .expect("rendered");
        assert!(pdf.starts_with(b"%PDF"));
    }
}
