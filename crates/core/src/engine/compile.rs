//! Compiling the buffer with Typst, in the background.
//!
//! One worker thread owns the work: it compiles the editor's buffer into a paged document
//! (`PagedDocument`), collects the diagnostics, and indexes the placed text so a click on a
//! rendered page can be traced back to the line of Typst source that produced it. When the
//! buffer changes again before the previous compile finished, the older request is dropped —
//! only the newest source matters. PDF export runs on the same thread.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::Arc;
use std::time::Instant;

use typst::diag::{Severity, SourceDiagnostic, Warned};
use typst::layout::Point;
use typst::layout::{Frame, FrameItem, Transform};
use typst::{World, WorldExt};

use typst_layout::{Page, PagedDocument};

use super::wake::Waker;
use super::world::{self, TypstWorld};

/// A diagnostic, in the form the problems panel shows.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub hints: Vec<String>,
    /// 0-based line and column in the file the diagnostic points into.
    pub line: usize,
    pub column: usize,
    /// The file's name when it is not the main one (an included file, a package).
    pub file: Option<String>,
    /// The byte range the diagnostic covers in the main source, when it points there. The editor
    /// underlines it, so the message has a place and not only a line number.
    pub range: Option<Range<usize>>,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        matches!(self.severity, Severity::Error)
    }
}

/// A run of glyphs on a page that all came from the same source line: the area a click on the
/// page maps back to. In page points, `<` in Typst's coordinate system with y pointing down.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// 0-based line of the source the glyphs were set from.
    pub line: usize,
}

/// One glyph (or glyph cluster) of the page, with the source byte it was set from.
///
/// The joined [`Hit`]s are what a hover highlight and a coarse click want; these are the
/// finer grain a caret placement needs, so a click inside a long line lands on the word (and
/// column) under the pointer rather than at the line's first character.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// 0-based line of the source the glyph came from.
    pub line: usize,
    /// Byte offset of the source span the glyph came from.
    pub byte: usize,
}

/// One page of the compiled document.
#[derive(Clone, Debug, Default)]
pub struct PageMeta {
    /// Size in points (1/72 inch), without bleed.
    pub width: f32,
    pub height: f32,
    pub hits: Vec<Hit>,
    pub glyphs: Vec<Glyph>,
}

/// What one compile produced: either pages, or the errors that stopped it.
pub struct Outcome {
    pub document: Option<Arc<PagedDocument>>,
    pub pages: Vec<PageMeta>,
    pub diagnostics: Vec<Diagnostic>,
    pub millis: u64,
}

impl Outcome {
    pub fn errors(&self) -> usize {
        self.diagnostics.iter().filter(|diagnostic| diagnostic.is_error()).count()
    }

    pub fn warnings(&self) -> usize {
        self.diagnostics.iter().filter(|diagnostic| !diagnostic.is_error()).count()
    }
}

/// A request for the compile thread.
enum Request {
    Compile { id: u64, gen: u64, main: PathBuf, text: String },
    Export { path: PathBuf, document: Arc<PagedDocument> },
}

/// An answer from the compile thread.
pub enum Event {
    Compiled { id: u64, gen: u64, outcome: Outcome },
    Exported { path: PathBuf, result: Result<usize, String> },
}

/// The UI's handle on the compile thread.
pub struct Compiler {
    requests: Sender<Request>,
    events: Receiver<Event>,
    log: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Compiler {
    pub fn spawn(waker: Waker) -> Self {
        let (requests, request_rx) = channel::<Request>();
        let (event_tx, events) = channel::<Event>();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = log.clone();
        std::thread::Builder::new()
            .name("typst-compile".to_string())
            .spawn(move || run(request_rx, event_tx, writer, waker))
            .expect("the compile thread starts");
        Self { requests, events, log }
    }

    /// Queue a compile of `text` as `main`, as generation `gen` of document `id`.
    pub fn compile(&self, id: u64, gen: u64, main: PathBuf, text: String) {
        let _ = self.requests.send(Request::Compile { id, gen, main, text });
    }

    pub fn export(&self, path: PathBuf, document: Arc<PagedDocument>) {
        let _ = self.requests.send(Request::Export { path, document });
    }

    pub fn poll(&self) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        events
    }

    pub fn log(&self) -> Vec<String> {
        self.log.lock().map(|log| log.clone()).unwrap_or_default()
    }
}

fn run(requests: Receiver<Request>, events: Sender<Event>, log: Arc<std::sync::Mutex<Vec<String>>>, waker: Waker) {
    // The fonts are loaded here, on this thread, so the window is already up while the system
    // font directories are scanned.
    let fonts = world::fonts();
    {
        world::mark_fonts_ready();
        if let Ok(mut log) = log.lock() {
            log.push(format!("{} font families available", fonts.book().families().count()));
        }
        waker.wake();
    }
    while let Ok(request) = requests.recv() {
        match request {
            Request::Compile { id, gen, main, text } => {
                // Only the newest buffer matters: take the last compile request that is
                // already waiting and forget the ones in between.
                let mut next = None;
                loop {
                    match requests.try_recv() {
                        Ok(Request::Compile { id, gen, main, text }) => next = Some((id, gen, main, text)),
                        Ok(other) => {
                            // An export that got queued behind the typing is handled now.
                            handle_export(other, &events, &log);
                        }
                        Err(_) => break,
                    }
                }
                let (id, gen, main, text) = next.unwrap_or((id, gen, main, text));
                let outcome = compile(&main, &text, fonts.clone());
                if let Ok(mut log) = log.lock() {
                    let summary = match outcome.errors() {
                        0 => format!("compiled in {} ms", outcome.millis),
                        n => format!("{n} error{} — compiled in {} ms", if n == 1 { "" } else { "s" }, outcome.millis),
                    };
                    log.push(summary);
                    for diagnostic in &outcome.diagnostics {
                        let at = match (&diagnostic.file, diagnostic.severity) {
                            (Some(file), _) => format!("{file}:{}:{}", diagnostic.line + 1, diagnostic.column + 1),
                            (None, _) => format!("{}:{}", diagnostic.line + 1, diagnostic.column + 1),
                        };
                        log.push(format!("{at} {}", diagnostic.message));
                    }
                }
                let _ = events.send(Event::Compiled { id, gen, outcome });
                waker.wake();
            }
            other => handle_export(other, &events, &log),
        }
    }
}

fn handle_export(request: Request, events: &Sender<Event>, log: &Arc<std::sync::Mutex<Vec<String>>>) {
    let Request::Export { path, document } = request else { return };
    let started = Instant::now();
    let options = typst_pdf::PdfOptions::default();
    let result = typst_pdf::pdf(&document, &options)
        .map_err(|errors| {
            let first = errors.first().map(|error| error.message.to_string()).unwrap_or_else(|| "export failed".to_string());
            first
        })
        .and_then(|bytes| {
            let len = bytes.len();
            std::fs::write(&path, bytes).map(|()| len).map_err(|error| format!("cannot write {}: {error}", path.display()))
        });
    if let Ok(mut log) = log.lock() {
        match &result {
            Ok(len) => log.push(format!("exported {len} bytes to {} in {} ms", path.display(), started.elapsed().as_millis())),
            Err(error) => log.push(format!("export failed: {error}")),
        }
    }
    let _ = events.send(Event::Exported { path, result });
}

/// Compile `text` as the document at `main`.
pub fn compile(main: &Path, text: &str, fonts: Arc<typst_kit::fonts::FontStore>) -> Outcome {
    let started = Instant::now();
    let world = TypstWorld::new(main, text.to_owned(), fonts);
    let Warned { output, warnings } = typst::compile::<PagedDocument>(&world);
    let mut diagnostics: Vec<Diagnostic> = warnings.iter().map(|warning| diagnostic(&world, warning)).collect();
    match output {
        Ok(document) => {
            let document = Arc::new(document);
            let pages = document.pages().iter().map(|page| page_meta(&world, page)).collect();
            Outcome { document: Some(document), pages, diagnostics, millis: (started.elapsed().as_millis() as u64).max(1) }
        }
        Err(errors) => {
            diagnostics.extend(errors.iter().map(|error| diagnostic(&world, error)));
            diagnostics.sort_by_key(|diagnostic| (diagnostic.line, diagnostic.column));
            Outcome { document: None, pages: Vec::new(), diagnostics, millis: (started.elapsed().as_millis() as u64).max(1) }
        }
    }
}

/// Turn a Typst diagnostic into line and column in the file it points into.
fn diagnostic(world: &TypstWorld, source_diagnostic: &SourceDiagnostic) -> Diagnostic {
    let span = source_diagnostic.span;
    let mut line = 0;
    let mut column = 0;
    let mut file = None;
    let mut covered = None;
    if let Some(range) = world.range(span) {
        if let Ok(source) = world.source(span.id().unwrap_or(world.main())) {
            if let Some((l, c)) = source.lines().byte_to_line_column(range.start) {
                line = l;
                column = c;
            }
            if span.id() == Some(world.main()) {
                covered = Some(range);
            } else {
                file = Some(source.id().vpath().get_without_slash().to_string());
            }
        }
    }
    Diagnostic {
        severity: source_diagnostic.severity,
        message: source_diagnostic.message.to_string(),
        hints: source_diagnostic.hints.iter().map(|hint| hint.v.to_string()).collect(),
        line,
        column,
        file,
        range: covered,
    }
}

fn page_meta(world: &TypstWorld, page: &Page) -> PageMeta {
    let size = page.frame.size();
    let mut hits = Vec::new();
    let mut glyphs = Vec::new();
    walk(world, &page.frame, Transform::identity(), &mut hits, &mut glyphs);
    hits.sort_by_key(|hit| (hit.line, float_key(hit.y0), float_key(hit.x0)));
    glyphs.sort_by_key(|glyph| (glyph.line, float_key(glyph.y0), float_key(glyph.x0)));
    PageMeta { width: size.x.to_pt() as f32, height: size.y.to_pt() as f32, hits: join(hits), glyphs }
}

fn float_key(value: f32) -> i64 {
    (value * 4.0) as i64
}

/// Walk the page's frames and index the placed text by source line.
fn walk(world: &TypstWorld, frame: &Frame, transform: Transform, hits: &mut Vec<Hit>, glyphs: &mut Vec<Glyph>) {
    for (position, item) in frame.items() {
        let at = apply(transform, *position);
        match item {
            FrameItem::Group(group) => {
                // As the renderer does: the group's frame is drawn at the item's position and
                // then transformed.
                let at = transform.pre_concat(Transform::translate(position.x, position.y));
                walk(world, &group.frame, at.pre_concat(group.transform), hits, glyphs);
            }
            FrameItem::Text(text) => {
                let scale = transform.sx.get() as f32;
                let size = text.size.to_pt() as f32 * scale;
                let mut x = at.x.to_pt() as f32;
                for glyph in &text.glyphs {
                    let advance = glyph.x_advance.at(text.size).to_pt() as f32 * scale;
                    let (x0, x1) = (x, x + advance);
                    x += advance;
                    if advance <= 0.0 {
                        continue;
                    }
                    let Some(line) = line_of(world, glyph.span.0) else { continue };
                    let baseline = at.y.to_pt() as f32;
                    let y0 = baseline - size * 0.82;
                    let y1 = baseline + size * 0.22;
                    hits.push(Hit { x0, y0, x1, y1, line });
                    glyphs.push(Glyph { x0, y0, x1, y1, line, byte: byte_of(world, glyph.span.0).unwrap_or(0) });
                }
            }
            _ => {}
        }
    }
}

fn apply(transform: Transform, point: Point) -> Point {
    Point::new(
        transform.sx.of(point.x) + transform.kx.of(point.y) + transform.tx,
        transform.ky.of(point.x) + transform.sy.of(point.y) + transform.ty,
    )
}

/// The byte offset in the main source that a span covers.
fn byte_of(world: &TypstWorld, span: typst::syntax::Span) -> Option<usize> {
    if span.id() != Some(world.main()) {
        return None;
    }
    world.range(span).map(|range| range.start)
}

/// The 0-based line of the main source that a span came from.
fn line_of(world: &TypstWorld, span: typst::syntax::Span) -> Option<usize> {
    if span.id() != Some(world.main()) {
        return None;
    }
    let range = world.range(span)?;
    let source = world.source(world.main()).ok()?;
    source.lines().byte_to_line(range.start)
}

/// Merge glyphs of the same line that sit on the same row and touch or nearly touch, so a word
/// (or a whole line) is one rectangular target rather than one per glyph.
fn join(hits: Vec<Hit>) -> Vec<Hit> {
    let mut joined: Vec<Hit> = Vec::with_capacity(hits.len());
    for hit in hits {
        if let Some(last) = joined.last_mut() {
            let same_row = (hit.y0 - last.y0).abs() < (hit.y1 - hit.y0).max(1.0) * 0.5;
            let touching = hit.x0 <= last.x1 + (hit.y1 - hit.y0).max(1.0) * 1.2 && hit.x0 >= last.x0;
            if last.line == hit.line && same_row && touching {
                last.x1 = last.x1.max(hit.x1);
                last.y1 = last.y1.max(hit.y1);
                last.y0 = last.y0.min(hit.y0);
                continue;
            }
        }
        joined.push(hit);
    }
    joined
}

/// The source line and column a click at `(x, y)` (page points) lands on.
///
/// The line comes from the glyph under the pointer, so it is exact even in a run that spans
/// several source lines; the column is counted in `text`, which must be the text that was
/// compiled.
pub fn position_at(pages: &[PageMeta], page: usize, x: f32, y: f32, text: &str) -> Option<(usize, usize)> {
    let meta = pages.get(page)?;
    let mut best: Option<(f32, Glyph)> = None;
    for glyph in &meta.glyphs {
        let dx = (glyph.x0 - x).max(x - glyph.x1).max(0.0);
        let dy = (glyph.y0 - y).max(y - glyph.y1).max(0.0);
        if dx > 6.0 || dy > 6.0 {
            continue;
        }
        let distance = dx * dx + dy * dy;
        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, *glyph));
        }
    }
    let (_, glyph) = best?;
    Some((glyph.line, crate::document::column_at_byte(text, glyph.byte)))
}

/// The page a click at `(x, y)` (page points) lands in, as a source line.
pub fn line_at(pages: &[PageMeta], page: usize, x: f32, y: f32) -> Option<usize> {
    let meta = pages.get(page)?;
    let mut best: Option<(f32, usize)> = None;
    for hit in &meta.hits {
        let dx = (hit.x0 - x).max(x - hit.x1).max(0.0);
        let dy = (hit.y0 - y).max(y - hit.y1).max(0.0);
        if dx > 6.0 || dy > 6.0 {
            continue;
        }
        let distance = dx * dx + dy * dy;
        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, hit.line));
        }
    }
    best.map(|(_, line)| line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(x0: f32, x1: f32, y0: f32, y1: f32, line: usize) -> Hit {
        Hit { x0, y0, x1, y1, line }
    }

    #[test]
    fn glyphs_of_one_row_merge_into_a_run() {
        let joined = join(vec![hit(0.0, 5.0, 10.0, 20.0, 3), hit(5.0, 9.0, 10.0, 20.0, 3), hit(9.0, 14.0, 10.0, 20.0, 3)]);
        assert_eq!(joined.len(), 1);
        assert!((joined[0].x1 - 14.0).abs() < 0.01);
    }

    #[test]
    fn other_lines_and_rows_stay_separate() {
        let joined = join(vec![hit(0.0, 5.0, 10.0, 20.0, 3), hit(0.0, 5.0, 30.0, 40.0, 4), hit(20.0, 25.0, 10.0, 20.0, 5)]);
        assert_eq!(joined.len(), 3);
    }

    #[test]
    fn a_click_picks_the_glyph_under_it_for_a_line_and_column() {
        let text = "= Title\n\nSome words on one line\n";
        let start = text.find("Some").unwrap();
        let mut glyphs = Vec::new();
        for (index, word) in ["Some", "words", "on"].iter().enumerate() {
            let x = 40.0 + index as f32 * 50.0;
            glyphs.push(Glyph { x0: x, y0: 60.0, x1: x + 40.0, y1: 72.0, line: 2, byte: start + text[start..].find(word).unwrap() });
        }
        let pages = vec![PageMeta { width: 400.0, height: 600.0, hits: Vec::new(), glyphs }];
        assert_eq!(position_at(&pages, 0, 45.0, 66.0, text), Some((2, 0)));
        assert_eq!(position_at(&pages, 0, 95.0, 66.0, text), Some((2, 5)));
        assert_eq!(position_at(&pages, 0, 160.0, 66.0, text), Some((2, 11)));
        assert_eq!(position_at(&pages, 0, 45.0, 400.0, text), None);
    }

    #[test]
    fn a_click_maps_to_the_line_of_the_run_under_it() {
        let pages = vec![PageMeta { width: 400.0, height: 600.0, hits: vec![hit(56.0, 300.0, 60.0, 72.0, 7)], glyphs: Vec::new() }];
        assert_eq!(line_at(&pages, 0, 100.0, 66.0), Some(7));
        // Within a few points of the run still counts (the pointer is not a pixel).
        assert_eq!(line_at(&pages, 0, 100.0, 76.0), Some(7));
        // Far away, and on a page that does not exist, nothing is returned.
        assert_eq!(line_at(&pages, 0, 100.0, 200.0), None);
        assert_eq!(line_at(&pages, 3, 100.0, 66.0), None);
    }
}
