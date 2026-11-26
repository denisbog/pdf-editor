//! The iced window: PdfCraft's chrome (tab strip, mode bar, rails, pasteboard, palette) around
//! `typst_iced_editor`, with the page view painting the very pages the document compiles to.
//!
//! Everything the two front-ends share lives in `pdf-editor-core`: the compile thread, the
//! raster pool, the click-to-source index, the vim state machine and the settings. This file is
//! the Elm shell — `Message`/`update`/`view` — and the geometry the page view needs.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::event::{self, Event};
use iced::keyboard;
use iced::mouse;
use iced::widget::{
    button, center_y, column, container, image, mouse_area, pick_list, row, scrollable, stack,
    text, text_input, Space,
};
use iced::widget::image::Handle as ImageHandle;
use iced::{Alignment, Border, Element, Length, Padding, Subscription, Task, Theme};
use typst_layout::PagedDocument;
use typst_iced_editor::{code_editor, Action, Content, Diagnostic as EditorDiagnostic, KeyPress};

use pdf_editor_core::document::{self, RightPanel, SessionDoc, Settings, ThemePreference, View, WELCOME};
use pdf_editor_core::engine::{self, Compiler, Diagnostic, Event as EngineEvent, PageMeta, RasterPool, Waker};
use pdf_editor_core::vim::Request as VimRequest;

use crate::icons;
use crate::pages::{self, Layout, Placement};
use crate::theme::{self, Tokens, EDITOR_LINE_HEIGHT, EDITOR_SIZE, GRIP};
use crate::vim::{Press, VimEditor};
use crate::widgets;

// The chrome metrics live in `theme` so the widget vocabulary and the page geometry agree.
const RAIL: f32 = theme::RAIL_WIDTH;
const TAB_STRIP: f32 = theme::TAB_STRIP_HEIGHT;
const MODE_BAR: f32 = theme::MODE_BAR_HEIGHT;
const BAR_PAD: f32 = theme::BAR_PAD;
const ICON_BOX: f32 = theme::ICON_BOX;
const TAB_CONTROL: f32 = theme::TAB_HEIGHT;
const SOURCE_MIN: f32 = 240.0;
const SIDE_MIN: f32 = 240.0;
const SIDE_MAX: f32 = 640.0;
const ZOOM_MIN: f32 = 0.1;
const ZOOM_MAX: f32 = 6.0;
const ZOOM_STEPS: [f32; 17] = [0.1, 0.25, 0.33, 0.5, 0.66, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 32.0];
/// The compile status is redrawn on every compile, so it gets a slot of its own: a new time or a
/// different diagnostic count must not shuffle the controls beside it. Wide enough for the widest
/// label the status can produce ("999 warnings — 9999 ms") at the mode bar's type size.
const STATUS_SLOT: f32 = 150.0;
/// A raster is thrown away when it is this far from the scale the view wants.
const SCALE_TOLERANCE: f32 = 0.01;
/// The lane the page view's scrollbars occupy (7 px bar + a 1 px margin a side). The content
/// width must leave it free, or a two-way scrollable shows a horizontal bar at fit-to-width.
const SCROLL_LANE: f32 = 9.0;

/// One page's raster, at `scale` device pixels per page point.
#[derive(Default)]
struct Slot {
    handle: Option<ImageHandle>,
    scale: f32,
    /// The compile generation the raster was rendered from: an older one stays on screen while
    /// the new generation is rendered, so typing never blanks a page.
    gen: u64,
    pending: Option<(u64, f32)>,
    error: Option<String>,
}

struct Doc {
    id: u64,
    path: PathBuf,
    content: Content,
    vim: VimEditor,
    /// The text as last written to disk; `None` when the document was never saved.
    saved: Option<String>,
    /// Incremented for every compile request; answers for older generations are dropped.
    gen: u64,
    compiling: bool,
    edited_at: Option<Instant>,
    millis: u64,
    metas: Vec<PageMeta>,
    document: Option<Arc<PagedDocument>>,
    diagnostics: Vec<Diagnostic>,
    pages: Vec<Slot>,
    /// A page to scroll to, set by a click on the page rail, `Home`/`End`, or a problem.
    goto: Option<usize>,
    page: usize,
    page_input: String,
    zoom_anchor: Option<(usize, f32, f32)>,
    shown_gen: u64,
    layout: Layout,
    scroll: f32,
    viewport: (f32, f32),
    hover: Option<(usize, (f32, f32))>,
}

impl Doc {
    fn new(id: u64, path: PathBuf, text: String, saved: Option<String>) -> Self {
        let mut content = Content::with_text(&text);
        // Vim typing wants the characters it was given: no pair-closing `(` -> `()` and no
        // indentation carried into a new line behind its back (the egui pane has neither).
        content.set_auto_pairs(false);
        content.set_auto_indent(false);
        let vim = VimEditor::new(&text);
        Self {
            id,
            path,
            content,
            vim,
            saved,
            gen: 0,
            compiling: true,
            edited_at: None,
            millis: 0,
            metas: Vec::new(),
            document: None,
            diagnostics: Vec::new(),
            pages: Vec::new(),
            goto: None,
            page: 0,
            page_input: "1".to_string(),
            zoom_anchor: None,
            shown_gen: 0,
            layout: Layout::default(),
            scroll: 0.0,
            viewport: (0.0, 0.0),
            hover: None,
        }
    }

    fn text(&self) -> String {
        self.content.text()
    }

    fn name(&self) -> String {
        self.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "untitled.typ".to_string())
    }

    fn dirty(&self) -> bool {
        self.saved.as_deref() != Some(self.text().as_str())
    }

    /// Turn to the next spread (two pages when the band shows a spread).
    fn step_page(&mut self, forward: bool) {
        let count = self.metas.len();
        if count == 0 {
            return;
        }
        let step = if self.layout.two_up { 2 } else { 1 };
        let row = self.page - self.page % step;
        let page = if forward { (row + step).min(count - 1) } else { row.saturating_sub(step) };
        self.go_to_page(page);
    }

    fn go_to_page(&mut self, page: usize) {
        let page = page.min(self.metas.len().saturating_sub(1));
        self.goto = Some(page);
        self.page = page;
        self.page_input = (page + 1).to_string();
    }
}

#[derive(Clone, Copy)]
enum Drag {
    /// The source/pages divider: the band's split ratio.
    Split,
    /// The right panel's edge: the panel width.
    Side,
    /// Dragging the pasteboard around (the Pan tool).
    Pan,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cmd {
    New,
    Open,
    Save,
    SaveAs,
    ExportPdf,
    Close,
    Compile,
    View(View),
    ZoomIn,
    ZoomOut,
    Zoom100,
    FitWidth,
    Panel(RightPanel),
    Theme(ThemePreference),
    ToggleVim,
    Shortcuts,
}

#[derive(Clone, Copy)]
pub struct Command {
    label: &'static str,
    detail: &'static str,
    icon: &'static str,
    cmd: Cmd,
}

/// The zoom menu's item type (`f32` is not `Eq`, and `pick_list` wants a displayable value).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ZoomStep(f32);

impl std::fmt::Display for ZoomStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.0}%", self.0 * 100.0)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Clone)]
pub enum Message {
    /// The 50 ms heartbeat: engine polling, the compile debounce, the toast timer.
    Tick,
    Edit(Action),
    Vim(Press),
    /// The page's text run under the pointer, for the hover highlight.
    PageHover(usize, (f32, f32)),
    PageLeave,
    PagePress(usize),
    PageRelease,
    Scrolled(scrollable::Viewport),
    Resized(iced::Size),
    Rescaled(f32),
    PointerMoved(iced::Point),
    Modifiers(keyboard::Modifiers),
    Wheel(mouse::ScrollDelta),
    Key(keyboard::Event),
    ClosePanel,
    OpenPath(PathBuf),
    TogglePan,
    About,
    GoHome,
    Persist,
    PlaceCaret(u64, usize, usize),
    GripPress(bool),
    SelectTab(u64),
    CloseTab(u64),
    OpenDialog,
    Run(Cmd),
    ViewChanged(View),
    TogglePanel(RightPanel),
    ZoomStep(ZoomStep),
    TabNext(bool),
    Palette(bool),
    PaletteQuery(String),
    PaletteSubmit,
    Shortcuts(bool),
    Filter(String),
    PageInput(String),
    PageSubmit,
    Dismiss,
    ConfirmClose(u64, CloseChoice),
    StepPage(bool),
}

pub struct App {
    docs: Vec<Doc>,
    active: Option<u64>,
    next_id: u64,
    /// Compile generations are unique across documents, so a raster can be traced to the one
    /// document it belongs to even when two tabs have the same page number in flight.
    next_gen: u64,
    settings: Settings,
    kind: document::ThemeKind,
    toks: Tokens,
    compiler: Compiler,
    rasters: RasterPool,
    right: Option<RightPanel>,
    pan: bool,
    filter: String,
    palette: bool,
    palette_query: String,
    shortcuts: bool,
    confirm: Option<u64>,
    toast: Option<(String, Instant)>,
    error: Option<String>,
    pending_zoom: Option<f32>,
    window: iced::Size,
    scale_factor: f32,
    pointer: iced::Point,
    modifiers: keyboard::Modifiers,
    drag: Option<(Drag, f32, f32)>,
    fonts_pending: bool,
    /// Whether the modal layer currently owns the keyboard.
    ///
    /// The editor widget's key binding is a closure made in `view`; this flag is what it reads
    /// to decide between handing a press to the modal layer and letting the widget type.
    vim_captures: Arc<AtomicBool>,
}

impl App {
    pub fn boot(settings: Settings) -> Self {
        // iced delivers the desktop's theme as an event, not a synchronous query: a `System`
        // preference starts light and the palette's two explicit choices switch it.
        let kind = settings.theme.resolve(None, document::ThemeKind::Light);
        let waker = Waker::none();
        let mut app = Self {
            docs: Vec::new(),
            active: None,
            next_id: 1,
            next_gen: 1,
            settings,
            kind,
            toks: Tokens::for_kind(kind),
            compiler: Compiler::spawn(waker.clone()),
            rasters: RasterPool::new(waker, render_threads()),
            right: None,
            pan: false,
            filter: String::new(),
            palette: false,
            palette_query: String::new(),
            shortcuts: false,
            confirm: None,
            toast: None,
            error: None,
            pending_zoom: None,
            window: iced::Size::new(1440.0, 920.0),
            scale_factor: 1.0,
            pointer: iced::Point::ORIGIN,
            modifiers: keyboard::Modifiers::default(),
            drag: None,
            fonts_pending: !engine::fonts_ready(),
            vim_captures: Arc::new(AtomicBool::new(true)),
        };
        if app.fonts_pending {
            engine::warm_fonts(Some(Waker::none()));
        }
        let named = document::named_documents();
        if !named.is_empty() {
            for path in named {
                app.open(path, None);
            }
        } else {
            let session = std::mem::take(&mut app.settings.session);
            let active = app.settings.session_active;
            for entry in session.into_iter().filter(|entry| entry.path.is_file()) {
                app.open(entry.path, Some(entry.page));
            }
            app.active = app.docs.get(active).map(|doc| doc.id).or_else(|| app.docs.first().map(|doc| doc.id));
        }
        app.relayout();
        app.refresh_rasters();
        app
    }

    pub fn theme(&self) -> Theme {
        self.toks.to_theme()
    }

    pub fn title(&self) -> String {
        match self.doc() {
            Some(doc) => format!("{} — PDF Editor", doc.name()),
            None => "PDF Editor".to_string(),
        }
    }

    // ------------------------------------------------------------------ documents

    fn doc(&self) -> Option<&Doc> {
        let active = self.active?;
        self.docs.iter().find(|doc| doc.id == active)
    }

    fn doc_mut(&mut self) -> Option<&mut Doc> {
        let active = self.active?;
        self.docs.iter_mut().find(|doc| doc.id == active)
    }

    fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    fn open(&mut self, path: PathBuf, page: Option<usize>) {
        self.error = None;
        if let Some(index) = self.docs.iter().position(|doc| doc.path == path) {
            if let Some(page) = page {
                self.docs[index].goto = Some(page);
            }
            self.active = Some(self.docs[index].id);
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let id = self.next_id;
                self.next_id += 1;
                let mut doc = Doc::new(id, path.clone(), text.clone(), Some(text));
                let gen = self.next_gen;
                self.next_gen += 1;
                doc.gen = gen;
                doc.goto = page;
                doc.page = page.unwrap_or(0);
                doc.page_input = (doc.page + 1).to_string();
                let name = doc.name();
                self.compiler.compile(id, gen, path, doc.text());
                self.docs.push(doc);
                self.active = Some(id);
                self.settings.fit_width = true;
                self.pending_zoom = None;
                self.toast(format!("{name} — compiling"));
            }
            Err(error) => self.error = Some(format!("cannot read {}: {error}", path.display())),
        }
    }

    /// A new document in the current folder, with a starter template.
    fn new_document(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let folder = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let taken: Vec<PathBuf> = self.docs.iter().map(|doc| doc.path.clone()).collect();
        let path = document::unique_path(&folder, "untitled.typ", &taken);
        let mut doc = Doc::new(id, path.clone(), WELCOME.to_string(), None);
        let gen = self.next_gen;
        self.next_gen += 1;
        doc.gen = gen;
        self.compiler.compile(id, gen, path, doc.text());
        self.docs.push(doc);
        self.active = Some(id);
        self.settings.view = View::Split;
        self.settings.fit_width = true;
        self.pending_zoom = None;
    }

    fn close_doc(&mut self, id: u64) {
        self.docs.retain(|doc| doc.id != id);
        if self.active == Some(id) {
            self.active = self.docs.last().map(|doc| doc.id);
        }
    }

    fn request_close(&mut self, id: u64) {
        let dirty = self.docs.iter().find(|doc| doc.id == id).is_some_and(|doc| doc.dirty());
        if dirty {
            self.confirm = Some(id);
        } else {
            self.close_doc(id);
        }
    }

    fn recompile(&mut self, id: u64) {
        let gen = self.next_gen;
        self.next_gen += 1;
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { return };
        doc.gen = gen;
        doc.compiling = true;
        doc.edited_at = None;
        self.compiler.compile(id, gen, doc.path.clone(), doc.text());
    }

    fn save(&mut self, id: u64, save_as: bool) {
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { return };
        if save_as || doc.saved.is_none() {
            let Some(path) = document::target_path(&doc.path, "PDF_EDITOR_SAVE_DIR", "typ") else { return };
            doc.path = path;
        }
        let path = doc.path.clone();
        match std::fs::write(&path, doc.text().as_bytes()) {
            Ok(()) => {
                doc.saved = Some(doc.text());
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                self.toast(format!("Saved {name}"));
                // A renamed document needs a fresh compile: its folder (and so its includes) may
                // have changed.
                self.recompile(id);
            }
            Err(error) => self.error = Some(format!("cannot write {}: {error}", path.display())),
        }
    }

    fn export(&mut self, id: u64) {
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { return };
        let Some(document) = doc.document.clone() else {
            self.toast("There is nothing to export yet");
            return;
        };
        let Some(path) = document::target_path(&doc.path, "PDF_EDITOR_EXPORT_DIR", "pdf") else { return };
        self.compiler.export(path.clone(), document);
        self.toast(format!("Exporting to {}…", path.display()));
    }

    /// Run a document command raised from the chrome or the command line.
    fn run_command(&mut self, cmd: Cmd) {
        let active = self.active;
        match cmd {
            Cmd::New => self.new_document(),
            Cmd::Open => self.open_dialog(),
            Cmd::Save => {
                if let Some(id) = active {
                    self.save(id, false);
                }
            }
            Cmd::SaveAs => {
                if let Some(id) = active {
                    self.save(id, true);
                }
            }
            Cmd::ExportPdf => {
                if let Some(id) = active {
                    self.export(id);
                }
            }
            Cmd::Close => {
                if let Some(id) = active {
                    self.request_close(id);
                }
            }
            Cmd::Compile => {
                if let Some(id) = active {
                    self.recompile(id);
                }
            }
            Cmd::View(view) => self.settings.view = view,
            Cmd::ZoomIn => self.zoom_step(true),
            Cmd::ZoomOut => self.zoom_step(false),
            Cmd::Zoom100 => self.pending_zoom = Some(1.0),
            Cmd::FitWidth => self.settings.fit_width = true,
            Cmd::Panel(panel) => self.right = if self.right == Some(panel) { None } else { Some(panel) },
            Cmd::Theme(preference) => self.settings.theme = preference,
            Cmd::ToggleVim => {
                self.settings.vim = !self.settings.vim;
                self.vim_captures.store(self.settings.vim, Ordering::Relaxed);
                let state = if self.settings.vim { "on" } else { "off" };
                self.toast(format!("Vim bindings {state}"));
            }
            Cmd::Shortcuts => self.shortcuts = true,
        }
        self.follow_theme();
        self.persist();
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Typst source", &["typ"]).pick_file() {
            self.open(path, None);
        }
    }

    // ------------------------------------------------------------------ engine

    fn poll_engine(&mut self) {
        for event in self.compiler.poll() {
            match event {
                EngineEvent::Compiled { id, gen, outcome } => {
                    let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { continue };
                    if gen != doc.gen {
                        continue;
                    }
                    doc.compiling = false;
                    doc.millis = outcome.millis;
                    doc.diagnostics = outcome.diagnostics;
                    // The marks are byte ranges into the text that was compiled; while newer
                    // keystrokes are waiting for their own compile they no longer line up.
                    if doc.edited_at.is_none() {
                        let text = doc.text();
                        let diagnostics: Vec<EditorDiagnostic> = document::diagnostic_ranges(&text, &doc.diagnostics)
                            .into_iter()
                            .map(|(range, error, message)| {
                                if error {
                                    EditorDiagnostic::error(range, message)
                                } else {
                                    EditorDiagnostic::warning(range, message)
                                }
                            })
                            .collect();
                        doc.content.set_diagnostics(diagnostics);
                    } else {
                        doc.content.clear_diagnostics();
                    }
                    if let Some(document) = outcome.document {
                        doc.metas = outcome.pages;
                        // Keep the old rasters: they stay on screen (stretched to the new page
                        // size if it changed) until the new generation's rasters arrive.
                        doc.pages.resize_with(doc.metas.len(), Slot::default);
                        for slot in &mut doc.pages {
                            slot.pending = None;
                            slot.error = None;
                        }
                        doc.document = Some(document.clone());
                        doc.shown_gen = gen;
                        self.rasters.set_document(gen, document);
                    }
                }
                EngineEvent::Exported { path, result } => match result {
                    Ok(len) => self.toast(format!("Exported {} — {} KB", path.display(), len / 1024)),
                    Err(error) => self.error = Some(error),
                },
            }
        }
        // Upload finished rasters.
        for raster in self.rasters.poll() {
            let Some(doc) = self.docs.iter_mut().find(|doc| doc.shown_gen == raster.gen) else { continue };
            let Some(slot) = doc.pages.get_mut(raster.page) else { continue };
            if slot.pending.map(|(tag, _)| tag) != Some(raster.tag) {
                continue;
            }
            slot.pending = None;
            if let Some(error) = raster.error {
                slot.error = Some(error);
                continue;
            }
            if raster.width == 0 || raster.height == 0 {
                continue;
            }
            slot.handle = Some(ImageHandle::from_rgba(raster.width as u32, raster.height as u32, raster.rgba));
            slot.scale = raster.scale;
            slot.gen = raster.gen;
            slot.error = None;
        }
        // Compile what was edited, once the typing has settled.
        let now = Instant::now();
        let mut compiled = Vec::new();
        for doc in &self.docs {
            if let Some(at) = doc.edited_at {
                if now.duration_since(at).as_secs_f64() > document::DEBOUNCE {
                    compiled.push(doc.id);
                }
            }
        }
        for id in compiled {
            self.recompile(id);
        }
        self.fonts_pending = !engine::fonts_ready();
    }

    /// Ask for rasters for the pages the view can see, at the scale it wants.
    ///
    /// The list sent to the pool is *declarative*: every visible page that is not satisfied is
    /// in it, including the ones already in flight (with their existing tag). Sending only the
    /// new ones and replacing the pool's queue would drop the requests that were still being
    /// rendered, leaving those slots `pending` for ever — which is what made the page view stop
    /// updating after an edit.
    fn refresh_rasters(&mut self) {
        let Some(active) = self.active else { return };
        let scale = self.settings.zoom * self.scale_factor;
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == active) else { return };
        if doc.shown_gen == 0 {
            return;
        }
        // `viewport` is only known after the first scroll event; assume a screenful until then.
        let viewport_h = if doc.viewport.1 > 1.0 { doc.viewport.1 } else { 800.0 };
        let visible = (doc.scroll - viewport_h, doc.scroll + viewport_h * 2.0);
        let mut tasks = Vec::new();
        for (page, slot) in doc.pages.iter_mut().enumerate() {
            let place = doc.layout.get(page);
            let in_sight = place.is_some_and(|place| place.bottom() > visible.0 && place.top < visible.1);
            if !in_sight {
                // A page far off screen gives its raster back: the handle is dropped and the
                // page shows its placeholder until it is scrolled to again. Pages just outside
                // the visible band keep theirs, so a small scroll does not flash.
                let far = place.is_none_or(|place| place.bottom() < doc.scroll - viewport_h * 3.0 || place.top > doc.scroll + viewport_h * 4.0);
                if far && page != doc.page {
                    slot.handle = None;
                    slot.scale = 0.0;
                    slot.pending = None;
                }
                continue;
            }
            let satisfied = slot.handle.is_some() && (slot.scale - scale).abs() < SCALE_TOLERANCE && slot.gen == doc.shown_gen;
            if satisfied {
                slot.pending = None;
                continue;
            }
            let tag = match slot.pending {
                Some((tag, pending_scale)) if (pending_scale - scale).abs() < SCALE_TOLERANCE => tag,
                _ => {
                    let tag = self.rasters.tag();
                    slot.pending = Some((tag, scale));
                    tag
                }
            };
            tasks.push(engine::Task { page, scale, tag });
        }
        // The page under the pointer first, so the page being read appears soonest.
        let current = doc.page;
        tasks.sort_by_key(|task| task.page != current);
        let gen = doc.shown_gen;
        self.rasters.set_queue(gen, tasks);
    }

    // ------------------------------------------------------------------ zoom and scroll

    /// The band's width: everything between the rails, minus the side panel and its grips.
    fn band_width(&self) -> f32 {
        let band = (self.window.width - RAIL * 2.0).max(64.0);
        band - if self.right.is_some() { self.settings.side_width.clamp(SIDE_MIN, SIDE_MAX) + GRIP * 2.0 } else { 0.0 }
    }

    /// The two panes' widths, from one ratio. The view and the page geometry must both use
    /// this: two `Fill` children in a row split 50/50 whatever `settings.split` says, which is
    /// what made the divider look fixed.
    fn split_widths(&self) -> (f32, f32) {
        let band = self.band_width();
        let usable = (band - GRIP * 2.0).max(64.0);
        match self.settings.view {
            View::Source => (band.max(64.0), 0.0),
            View::Preview => (0.0, band.max(64.0)),
            View::Split => {
                let source = (usable * self.settings.split).clamp(SOURCE_MIN.min(usable / 2.0), (usable - SOURCE_MIN).max(SOURCE_MIN.min(usable)));
                (source, (usable - source).max(1.0))
            }
        }
    }

    fn pages_width(&self) -> f32 {
        self.split_widths().1
    }

    /// The width the page rows are laid out in: the viewport (minus the scrollbar lane),
    /// unless the pages at this zoom are wider than that — then the content grows and the view
    /// scrolls horizontally instead of squeezing the pages (iced clamps fixed widths to the
    /// parent's max width).
    fn pages_band(&self) -> f32 {
        let usable = (self.pages_width() - SCROLL_LANE).max(64.0);
        let widest = self.doc().map(|doc| doc.metas.iter().map(|page| page.width).fold(0.0_f32, f32::max) * self.settings.zoom).unwrap_or(0.0);
        usable.max(widest + 2.0 * pages::SCROLL_MARGIN)
    }

    /// The source pane's width, mirroring `pages_width`; the header adapts to it.
    fn source_width(&self) -> f32 {
        self.split_widths().0
    }

    fn relayout(&mut self) {
        let width = self.pages_width();
        let zoom_before = self.settings.zoom;
        let usable = (width - SCROLL_LANE).max(64.0);
        let margins = 2.0 * pages::SCROLL_MARGIN;
        if width > 0.0 && self.settings.fit_width {
            // Fit to width waits for the first compile: there is no page size to fit yet.
            if let Some(widest) = self.doc().map(|doc| doc.metas.iter().map(|page| page.width).fold(1.0_f32, f32::max)).filter(|_| self.doc().is_some_and(|doc| !doc.metas.is_empty())) {
                let spread = (usable - margins - pages::SPREAD_GAP) / (2.0 * widest);
                let fit = (if spread >= pages::FIT_SPREAD_MIN { spread } else { (usable - margins) / widest }).clamp(ZOOM_MIN, ZOOM_MAX);
                if (fit - self.settings.zoom).abs() > 0.001 {
                    self.settings.zoom = fit;
                }
            }
        }
        // A zoom change of any origin (fit-to-width on a resize, the palette, a drag) keeps
        // the point at the centre of the view; without an anchor the old scroll offset would
        // be left pointing past the end of the new content.
        if (self.settings.zoom - zoom_before).abs() > 0.001 && self.doc().is_some_and(|doc| doc.zoom_anchor.is_none()) {
            let centre = iced::Point::new(self.window.width / 2.0, self.window.height / 2.0);
            if let Some(anchor) = self.doc().and_then(|doc| self.anchor_under(doc, centre)) {
                if let Some(doc) = self.doc_mut() {
                    doc.zoom_anchor = Some(anchor);
                }
            }
        }
        let zoom = self.settings.zoom;
        let band = self.pages_band();
        let Some(doc) = self.doc_mut() else { return };
        if width > 0.0 {
            doc.layout = pages::layout(&doc.metas, band, zoom);
        }
        // Zoom, panel and split changes all move through here; the raster pool has to hear
        // about the new scale, or the page stays at the old one stretched to the new box.
        self.refresh_rasters();
    }

    fn zoom_step(&mut self, up: bool) {
        let zoom = self.settings.zoom;
        let next = if up { ZOOM_STEPS.iter().copied().find(|step| *step > zoom * 1.01) } else { ZOOM_STEPS.iter().rev().copied().find(|step| *step < zoom * 0.99) };
        if let Some(next) = next {
            self.pending_zoom = Some(next.clamp(ZOOM_MIN, ZOOM_MAX));
        }
    }

    fn zoom_to(&mut self, zoom: f32, anchor: Option<(usize, f32, f32)>) {
        if let Some(doc) = self.doc_mut() {
            if let Some(anchor) = anchor {
                doc.zoom_anchor = Some(anchor);
            }
        }
        self.settings.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.settings.fit_width = false;
    }

    /// The page and fraction under a window point, with the point's offset from the viewport
    /// top, so a zoom can keep it under the pointer.
    fn anchor_under(&self, doc: &Doc, point: iced::Point) -> Option<(usize, f32, f32)> {
        // Window coordinates are measured from the window's top edge; the scroll view starts
        // below the tab strip and the mode bar.
        let y = (point.y - (TAB_STRIP + MODE_BAR)).max(0.0);
        let content_y = doc.scroll + y;
        let place = doc.layout.pages.iter().min_by(|a, b| {
            let distance = |place: &Placement| {
                let top = place.top;
                let bottom = place.bottom();
                if content_y < top { top - content_y } else if content_y > bottom { content_y - bottom } else { 0.0 }
            };
            distance(a).total_cmp(&distance(b))
        })?;
        let fraction = ((content_y - place.top) / place.height.max(1.0)).clamp(0.0, 1.0);
        Some((place.page, fraction, y))
    }

    /// Apply a geometry change and let the view settle: relayout, re-request rasters, and move
    /// the scroll view to the pending page or zoom anchor.
    fn settle(&mut self) -> Task<Message> {
        self.relayout();
        self.refresh_rasters();
        self.follow_pages()
    }

    /// Scroll the page view to the document's pending page (or to a zoom anchor).
    fn follow_pages(&mut self) -> Task<Message> {
        let zoom = self.settings.zoom;
        let width = self.pages_width();
        let band = self.pages_band();
        let Some(doc) = self.doc_mut() else { return Task::none() };
        doc.layout = pages::layout(&doc.metas, band, zoom);
        let x = Some(((band - width) / 2.0).max(0.0));
        let id = iced::widget::Id::from(format!("pages-{}", doc.id));
        if let Some(page) = doc.goto.take() {
            if let Some(place) = doc.layout.get(page) {
                let y = (place.top - pages::SCROLL_MARGIN).max(0.0);
                doc.scroll = y;
                return iced::widget::operation::scroll_to(id, scrollable::AbsoluteOffset { x, y: Some(y) });
            }
        } else if let Some((page, fraction, rel_y)) = doc.zoom_anchor.take() {
            if let Some(place) = doc.layout.get(page) {
                let y = (place.top + fraction * place.height - rel_y).max(0.0);
                doc.scroll = y;
                return iced::widget::operation::scroll_to(id, scrollable::AbsoluteOffset { x, y: Some(y) });
            }
        }
        Task::none()
    }

    // ------------------------------------------------------------------ update

    /// All keyboard handling in one place. Esc closes exactly one layer, highest first; while
    /// an overlay is open no app shortcut fires behind it.
    fn handle_key(&mut self, event: keyboard::Event) -> Task<Message> {
        let keyboard::Event::KeyPressed { key, modifiers, .. } = event else { return Task::none() };
        let named = |named: keyboard::key::Named| matches!(key, keyboard::Key::Named(value) if value == named);

        if named(keyboard::key::Named::Escape) {
            if self.palette {
                self.palette = false;
                self.palette_query.clear();
            } else if self.shortcuts {
                self.shortcuts = false;
            } else if self.confirm.is_some() {
                self.confirm = None;
            }
            return Task::none();
        }

        // An overlay owns the keyboard: its own field/submit handlers work, global shortcuts
        // do not reach the workspace behind it.
        if self.palette || self.shortcuts || self.confirm.is_some() {
            return Task::none();
        }
        if !(modifiers.control() || modifiers.command()) {
            return Task::none();
        }

        let command = match key {
            keyboard::Key::Named(keyboard::key::Named::Tab) => Message::TabNext(!modifiers.shift()),
            keyboard::Key::Named(keyboard::key::Named::Enter) => Message::Run(Cmd::Compile),
            keyboard::Key::Character(value) => match value.chars().next().map(|c| c.to_ascii_lowercase()) {
                Some(character) => match character {
                    'k' => Message::Palette(true),
                    'n' => Message::Run(Cmd::New),
                    'o' => Message::Run(Cmd::Open),
                    's' => Message::Run(if modifiers.shift() { Cmd::SaveAs } else { Cmd::Save }),
                    'e' => Message::Run(Cmd::ExportPdf),
                    'r' => Message::Run(Cmd::Compile),
                    'w' => Message::Run(Cmd::Close),
                    '0' => Message::Run(Cmd::Zoom100),
                    '2' => Message::Run(Cmd::FitWidth),
                    '+' | '=' => Message::Run(Cmd::ZoomIn),
                    '-' => Message::Run(Cmd::ZoomOut),
                    _ => return Task::none(),
                },
                None => return Task::none(),
            },
            _ => return Task::none(),
        };
        self.update(command)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        self.follow_theme();
        match message {
            Message::Tick => {
                self.poll_engine();
                self.relayout();
                self.refresh_rasters();
                if self.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_millis(3500)) {
                    self.toast = None;
                }
                Task::none()
            }
            Message::Edit(action) => {
                let vim = self.settings.vim;
                let is_edit = action.is_edit();
                let Some(active) = self.active else { return Task::none() };
                let captures = self.vim_captures.clone();
                let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == active) else { return Task::none() };
                doc.content.perform(action);
                if vim {
                    doc.vim.after_editor(&doc.content);
                }
                captures.store(vim && doc.vim.captures(), Ordering::Relaxed);
                if is_edit {
                    doc.edited_at = Some(Instant::now());
                    doc.content.clear_diagnostics();
                }
                Task::none()
            }
            Message::Vim(press) => {
                let Some(active) = self.active else { return Task::none() };
                let captures = self.vim_captures.clone();
                let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == active) else { return Task::none() };
                let actions = doc.vim.press(&press);
                let mut edited = false;
                for action in actions {
                    if action.is_edit() {
                        edited = true;
                    }
                    doc.content.perform(action);
                }
                if edited {
                    doc.edited_at = Some(Instant::now());
                    doc.content.clear_diagnostics();
                }
                captures.store(doc.vim.captures(), Ordering::Relaxed);
                let request = doc.vim.take_request();
                match request {
                    Some(request) => self.handle_vim_request(request),
                    None => Task::none(),
                }
            }
            Message::PageHover(page, local) => {
                if let Some(doc) = self.doc_mut() {
                    doc.hover = Some((page, local));
                }
                Task::none()
            }
            Message::PageLeave => {
                if let Some(doc) = self.doc_mut() {
                    doc.hover = None;
                }
                Task::none()
            }
            Message::PagePress(page) => {
                self.pan_press(page);
                Task::none()
            }
            Message::PageRelease => {
                let was_drag = self.drag.is_some();
                self.drag = None;
                // A finished resize is a settings change like any other: keep it.
                if was_drag {
                    self.persist();
                }
                Task::none()
            }
            Message::Scrolled(viewport) => {
                let Some(doc) = self.doc_mut() else { return Task::none() };
                let offset = viewport.absolute_offset();
                doc.scroll = offset.y;
                let bounds = viewport.bounds();
                doc.viewport = (bounds.width, bounds.height);
                if let Some(current) = pages::current_page(&doc.layout, doc.scroll, bounds.height) {
                    if current != doc.page {
                        doc.page = current;
                        doc.page_input = (current + 1).to_string();
                    }
                }
                self.refresh_rasters();
                Task::none()
            }
            Message::Resized(size) => {
                self.window = size;
                self.settle()
            }
            Message::Rescaled(scale) => {
                self.scale_factor = scale;
                self.refresh_rasters();
                Task::none()
            }
            Message::PointerMoved(point) => {
                self.pointer = point;
                self.drag_move()
            }
            Message::Modifiers(modifiers) => {
                self.modifiers = modifiers;
                Task::none()
            }
            Message::Wheel(delta) => {
                if !self.modifiers.control() && !self.modifiers.command() {
                    return Task::none();
                }
                let step = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => y * 0.08,
                    mouse::ScrollDelta::Pixels { y, .. } => y * 0.004,
                };
                let zoom = self.settings.zoom * (1.0 + step);
                let anchor = self.doc().and_then(|doc| self.anchor_under(doc, self.pointer));
                self.zoom_to(zoom, anchor);
                self.settle()
            }
            Message::Key(event) => self.handle_key(event),
            Message::OpenPath(path) => {
                self.open(path, None);
                self.settle()
            }
            Message::ClosePanel => {
                self.right = None;
                self.settle()
            }
            Message::GripPress(split) => {
                let value = if split { self.source_width() } else { self.settings.side_width };
                self.drag = Some((if split { Drag::Split } else { Drag::Side }, self.pointer.x, value));
                Task::none()
            }
            Message::SelectTab(id) => {
                self.active = Some(id);
                self.settle()
            }
            Message::CloseTab(id) => {
                self.request_close(id);
                Task::none()
            }
            Message::OpenDialog => {
                self.open_dialog();
                self.settle()
            }
            Message::Run(cmd) => {
                self.run_command(cmd);
                if self.pending_zoom.is_some() {
                    return self.take_pending_zoom();
                }
                self.settle()
            }
            Message::ViewChanged(view) => {
                self.settings.view = view;
                self.persist();
                self.settle()
            }
            Message::TogglePanel(panel) => {
                self.right = if self.right == Some(panel) { None } else { Some(panel) };
                self.settle()
            }
            Message::ZoomStep(step) => {
                self.pending_zoom = Some(step.0);
                self.take_pending_zoom()
            }
            Message::TabNext(forward) => {
                if !self.docs.is_empty() {
                    let index = self.active.and_then(|id| self.docs.iter().position(|doc| doc.id == id));
                    let next = match index {
                        Some(index) if forward => (index + 1) % self.docs.len(),
                        Some(index) => (index + self.docs.len() - 1) % self.docs.len(),
                        None => 0,
                    };
                    self.active = Some(self.docs[next].id);
                    return self.settle();
                }
                Task::none()
            }
            Message::Palette(open) => {
                self.palette = open;
                if !open {
                    self.palette_query.clear();
                }
                if open {
                    iced::widget::operation::focus(iced::widget::Id::new("palette-input"))
                } else {
                    Task::none()
                }
            }
            Message::PaletteQuery(query) => {
                self.palette_query = query;
                Task::none()
            }
            Message::PaletteSubmit => {
                let query = self.palette_query.trim().to_lowercase();
                let chosen = self.commands().into_iter().filter(|command| score(command.label, &query).is_some()).min_by_key(|command| score(command.label, &query).unwrap_or(0));
                self.palette = false;
                self.palette_query.clear();
                if let Some(command) = chosen {
                    self.run_command(command.cmd);
                }
                Task::none()
            }
            Message::Shortcuts(open) => {
                self.shortcuts = open;
                Task::none()
            }
            Message::Filter(filter) => {
                self.filter = filter;
                Task::none()
            }
            Message::PageInput(value) => {
                if value.chars().all(|c| c.is_ascii_digit()) {
                    if let Some(doc) = self.doc_mut() {
                        doc.page_input = value;
                    }
                }
                Task::none()
            }
            Message::PageSubmit => {
                let typed = self.doc().map(|doc| doc.page_input.clone()).unwrap_or_default();
                if let (Some(doc), Ok(number)) = (self.doc_mut(), typed.trim().parse::<usize>()) {
                    if (1..=doc.metas.len()).contains(&number) {
                        doc.go_to_page(number - 1);
                    } else {
                        doc.page_input = (doc.page + 1).to_string();
                    }
                }
                self.follow_pages()
            }
            Message::GoHome => {
                self.active = None;
                Task::none()
            }
            Message::Persist => {
                self.persist();
                Task::none()
            }
            Message::About => {
                self.toast("PDF Editor — a Typst document, typeset page by page");
                Task::none()
            }
            Message::TogglePan => {
                self.pan = !self.pan;
                Task::none()
            }
            Message::PlaceCaret(id, line, column) => {
                let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { return Task::none() };
                let text = doc.text();
                let byte = document::byte_at(&text, line, column);
                if self.settings.vim {
                    doc.vim.move_to(&mut doc.content, byte);
                } else {
                    doc.content.perform(Action::MoveTo(byte));
                }
                Task::none()
            }
            Message::Dismiss => {
                self.error = None;
                Task::none()
            }
            Message::ConfirmClose(id, choice) => {
                self.confirm = None;
                match choice {
                    CloseChoice::Save => {
                        self.save(id, false);
                        if self.docs.iter().any(|doc| doc.id == id && !doc.dirty()) {
                            self.close_doc(id);
                        }
                    }
                    CloseChoice::Discard => self.close_doc(id),
                    CloseChoice::Cancel => {}
                }
                Task::none()
            }
            Message::StepPage(forward) => {
                if let Some(doc) = self.doc_mut() {
                    doc.step_page(forward);
                }
                self.follow_pages()
            }
        }
    }

    /// Keep the tokens in step with the saved theme choice. iced reports the desktop's theme
    /// through a system event, not a synchronous call, so `System` falls back to light until
    /// the user picks a side (the egui build resolves it against `egui::Context::system_theme`).
    /// Write the settings now rather than only on exit: the window may be killed, and the
    /// choice (theme, view, split) is worth keeping either way.
    fn persist(&self) {
        document::save_settings(&self.settings);
    }

    fn follow_theme(&mut self) {
        let kind = self.settings.theme.resolve(None, document::ThemeKind::Light);
        if kind != self.kind {
            self.kind = kind;
            self.toks = Tokens::for_kind(kind);
        }
    }

    fn take_pending_zoom(&mut self) -> Task<Message> {
        let Some(zoom) = self.pending_zoom.take() else { return Task::none() };
        let anchor = self.doc().and_then(|doc| {
            let centre = iced::Point::new(self.window.width / 2.0, self.window.height / 2.0);
            self.anchor_under(doc, centre)
        });
        if let Some(doc) = self.doc_mut() {
            doc.zoom_anchor = anchor;
        }
        self.settings.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        // An explicit step wins over fit-to-width, which would otherwise recompute it away.
        self.settings.fit_width = false;
        self.relayout();
        self.follow_pages()
    }

    fn handle_vim_request(&mut self, request: VimRequest) -> Task<Message> {
        match request {
            VimRequest::Save => {
                if let Some(id) = self.active {
                    self.save(id, false);
                }
            }
            VimRequest::Close => {
                if let Some(id) = self.active {
                    self.request_close(id);
                }
            }
            VimRequest::Open(path) => self.open(PathBuf::from(path), None),
            VimRequest::Compile => {
                if let Some(id) = self.active {
                    self.recompile(id);
                }
            }
        }
        Task::none()
    }

    fn drag_move(&mut self) -> Task<Message> {
        let Some((kind, start_a, start_b)) = self.drag.clone() else { return Task::none() };
        match kind {
            Drag::Split => {
                let delta = self.pointer.x - start_a;
                let usable = (self.band_width() - GRIP * 2.0).max(64.0);
                let low = SOURCE_MIN.min(usable / 2.0);
                let high = (usable - SOURCE_MIN).max(low);
                let want = (start_b + delta).clamp(low, high);
                self.settings.split = (want / usable.max(1.0)).clamp(0.05, 0.95);
                self.settle()
            }
            Drag::Side => {
                let delta = self.pointer.x - start_a;
                self.settings.side_width = (start_b - delta).clamp(SIDE_MIN, SIDE_MAX);
                self.settle()
            }
            Drag::Pan => {
                // The content follows the hand: dragging down moves the pages down, which is a
                // negative scroll offset.
                let dy = start_a - self.pointer.y;
                self.drag = Some((Drag::Pan, self.pointer.y, 0.0));
                let Some(active) = self.active else { return Task::none() };
                let id = iced::widget::Id::from(format!("pages-{active}"));
                iced::widget::operation::scroll_by(id, scrollable::AbsoluteOffset { x: 0.0, y: dy })
            }
        }
    }

    /// A press on a page: put the caret on the text under the pointer, or start panning.
    fn pan_press(&mut self, page: usize) {
        if self.pan {
            self.drag = Some((Drag::Pan, self.pointer.x, self.pointer.y));
            return;
        }
        let Some(active) = self.active else { return };
        let vim = self.settings.vim;
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == active) else { return };
        // The mouse area reports page-local points; the last hover is the press position.
        let Some((hover_page, local)) = doc.hover else { return };
        if hover_page != page {
            return;
        }
        let text = doc.text();
        if let Some((line, column)) = pages::position_at(&doc.metas, page, local, &text) {
            let byte = document::byte_at(&text, line, column);
            if vim {
                doc.vim.move_to(&mut doc.content, byte);
            } else {
                doc.content.perform(Action::MoveTo(byte));
            }
            doc.goto = Some(page);
            if self.settings.view == View::Preview {
                self.settings.view = View::Split;
                self.settings.fit_width = true;
            }
        }
    }

    // ------------------------------------------------------------------ command palette

    fn commands(&self) -> Vec<Command> {
        let mut commands = vec![
            Command { label: "New document", detail: "Ctrl+N", icon: "file-plus", cmd: Cmd::New },
            Command { label: "Open a document", detail: "Ctrl+O", icon: "plus", cmd: Cmd::Open },
            Command { label: "Save", detail: "Ctrl+S", icon: "save", cmd: Cmd::Save },
            Command { label: "Save as…", detail: "Ctrl+Shift+S", icon: "save", cmd: Cmd::SaveAs },
            Command { label: "Export PDF", detail: "Ctrl+E", icon: "file-output", cmd: Cmd::ExportPdf },
            Command { label: "Compile now", detail: "Ctrl+R", icon: "refresh-cw", cmd: Cmd::Compile },
            Command { label: "Close the document", detail: "Ctrl+W", icon: "x", cmd: Cmd::Close },
            Command { label: "Source only", detail: "View", icon: "code", cmd: Cmd::View(View::Source) },
            Command { label: "Split view", detail: "View", icon: "columns-2", cmd: Cmd::View(View::Split) },
            Command { label: "Pages only", detail: "View", icon: "eye", cmd: Cmd::View(View::Preview) },
            Command { label: "Zoom in", detail: "Ctrl++", icon: "zoom-in", cmd: Cmd::ZoomIn },
            Command { label: "Zoom out", detail: "Ctrl+−", icon: "zoom-out", cmd: Cmd::ZoomOut },
            Command { label: "Actual size (100%)", detail: "Ctrl+0", icon: "gauge", cmd: Cmd::Zoom100 },
            Command { label: "Fit to width", detail: "Ctrl+2", icon: "maximize", cmd: Cmd::FitWidth },
            Command { label: "Problems", detail: "Panel", icon: "triangle-alert", cmd: Cmd::Panel(RightPanel::Problems) },
            Command { label: "Document", detail: "Panel", icon: "file-text", cmd: Cmd::Panel(RightPanel::Document) },
            Command { label: "Log", detail: "Panel", icon: "square-terminal", cmd: Cmd::Panel(RightPanel::Log) },
            Command { label: "Light gray theme", detail: "Theme", icon: "sun", cmd: Cmd::Theme(ThemePreference::Light) },
            Command { label: "Dark gray theme", detail: "Theme", icon: "moon", cmd: Cmd::Theme(ThemePreference::Dark) },
            Command { label: "Use system theme", detail: "Theme", icon: "settings", cmd: Cmd::Theme(ThemePreference::System) },
            Command { label: if self.settings.vim { "Vim bindings: on" } else { "Vim bindings: off" }, detail: "Editing", icon: "code", cmd: Cmd::ToggleVim },
            Command { label: "Keyboard shortcuts", detail: "Help", icon: "circle-help", cmd: Cmd::Shortcuts },
        ];
        if self.active.is_none() {
            commands.retain(|command| matches!(command.cmd, Cmd::New | Cmd::Open | Cmd::Theme(_) | Cmd::ToggleVim | Cmd::Shortcuts));
        }
        commands
    }

    // ------------------------------------------------------------------ keyboard

    pub fn subscription(&self) -> Subscription<Message> {
        // The heartbeat only runs while there is work: a compile in flight, typing waiting for
        // its debounce, a raster being rendered, a font scan, a toast fading. An idle window
        // asks for no frames at all, so it does not burn a core re-drawing a still page.
        let busy = self.fonts_pending
            || self.toast.is_some()
            || self.docs.iter().any(|doc| doc.compiling || doc.edited_at.is_some() || doc.pages.iter().any(|slot| slot.pending.is_some()));
        let tick = if busy { iced::time::every(Duration::from_millis(50)).map(|_| Message::Tick) } else { Subscription::none() };
        Subscription::batch([tick, event::listen_with(listen)])
    }

    /// The modal layer's gate on the editor widget: while it owns the keyboard every press that
    /// it names goes to it (so the widget inserts nothing); Insert mode keeps the widget's own
    /// bindings. Escape is always diverted, so leaving Insert does not also drop focus.
    fn binding(&self) -> impl Fn(KeyPress) -> Option<typst_iced_editor::Binding<Message>> + 'static {
        let captures = self.vim_captures.clone();
        let vim = self.settings.vim;
        move |press: KeyPress| {
            let escape = matches!(press.modified_key, keyboard::Key::Named(keyboard::key::Named::Escape));
            if vim && (escape || captures.load(Ordering::Relaxed)) {
                if let Some(press) = Press::new(&press) {
                    return Some(typst_iced_editor::Binding::Custom(Message::Vim(press)));
                }
            }
            typst_iced_editor::Binding::from_key_press(press)
        }
    }
}

/// Runtime events the window chrome needs: sizes, the pointer (for drags and zoom anchors),
/// modifiers (Ctrl + wheel) and every keyboard event (dispatched by `handle_key`).
fn listen(event: Event, _status: event::Status, _window: iced::window::Id) -> Option<Message> {
    match event {
        Event::Window(iced::window::Event::Resized(size)) => Some(Message::Resized(size)),
        Event::Window(iced::window::Event::CloseRequested) => Some(Message::Persist),
        Event::Window(iced::window::Event::Rescaled(scale)) => Some(Message::Rescaled(scale)),
        Event::Mouse(mouse::Event::CursorMoved { position }) => Some(Message::PointerMoved(position)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => Some(Message::PageRelease),
        Event::Mouse(mouse::Event::WheelScrolled { delta }) => Some(Message::Wheel(delta)),
        Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => Some(Message::Modifiers(modifiers)),
        Event::Keyboard(event @ keyboard::Event::KeyPressed { .. }) => Some(Message::Key(event)),
        _ => None,
    }
}

fn score(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay = hay.to_lowercase();
    if let Some(position) = hay.find(needle) {
        return Some(position);
    }
    let mut chars = hay.chars();
    needle.chars().all(|c| chars.any(|x| x == c)).then_some(100)
}

fn render_threads() -> usize {
    std::thread::available_parallelism().map(|count| count.get()).unwrap_or(4).clamp(1, 6)
}

/// The compile status shown in the mode bar and the source header.
fn status_of(doc: &Doc, fonts_pending: bool, t: Tokens) -> (String, iced::Color) {
    if doc.compiling {
        let label = if fonts_pending { "loading fonts…".to_string() } else { "typesetting…".to_string() };
        return (label, t.text_muted);
    }
    let errors = doc.diagnostics.iter().filter(|diagnostic| diagnostic.is_error()).count();
    let warnings = doc.diagnostics.len() - errors;
    if errors > 0 {
        (format!("{errors} error{} — {} ms", if errors == 1 { "" } else { "s" }, doc.millis), t.danger)
    } else if warnings > 0 {
        (format!("{warnings} warning{} — {} ms", if warnings == 1 { "" } else { "s" }, doc.millis), t.warning)
    } else if doc.millis > 0 {
        (format!("{} ms", doc.millis), t.success)
    } else {
        ("—".to_string(), t.text_muted)
    }
}

/// The palette's input id, shared by the view and the focus task.
const PALETTE_INPUT: &str = "palette-input";
const PAGE_INPUT: &str = "page-input";
const PROBLEM_FILTER: &str = "problem-filter";

impl App {
    // ------------------------------------------------------------------ view

    pub fn view(&self) -> Element<'_, Message> {
        let t = self.toks;
        let body: Element<'_, Message> = match self.doc() {
            Some(doc) => self.body(doc),
            None => self.welcome(),
        };
        // The root carries its own background so the first frame never shows the renderer's
        // clear colour; overlays are siblings in the stack so they never reflow the app.
        let root = container(column![self.tab_strip(), self.mode_bar(), body].height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(t.filled(t.panel));

        let mut layers = stack(vec![root.into()]);
        if self.palette {
            layers = layers.push(self.palette_overlay());
        }
        if self.shortcuts {
            layers = layers.push(self.shortcuts_overlay());
        }
        if self.confirm.is_some() {
            layers = layers.push(self.confirm_overlay());
        }
        if self.error.is_some() {
            layers = layers.push(self.error_banner());
        }
        if let Some((message, _)) = &self.toast {
            layers = layers.push(widgets::toast(message, t));
        }
        layers.into()
    }

    // ------------------------------------------------------------------ chrome

    fn tab_strip(&self) -> Element<'_, Message> {
        let t = self.toks;
        let mut tabs = row![].spacing(2).align_y(Alignment::Center);
        for doc in &self.docs {
            let active = Some(doc.id) == self.active;
            let name = widgets::tab_name(document::shorten(&doc.name(), 26), "file-text", active, doc.dirty(), t, Message::SelectTab(doc.id));
            tabs = tabs.push(row![name, widgets::tab_close(active, t, Message::CloseTab(doc.id))].spacing(0).align_y(Alignment::Center));
        }

        // The tab lane is sized from the tabs themselves (name width estimated from its
        // character count, plus the icon and the close box), so "+ New" sits right after the
        // last tab; it stays bounded, so a long list scrolls instead of pushing the strip apart.
        let reserved = BAR_PAD * 2.0 + ICON_BOX * 4.0 + 4.0 * 4.0 + 96.0;
        let wanted: f32 = self
            .docs
            .iter()
            .map(|doc| 10.0 + 15.0 + 6.0 + doc.name().chars().count() as f32 * 7.2 + if doc.dirty() { 12.0 } else { 0.0 } + 29.0)
            .sum::<f32>()
            + self.docs.len().saturating_sub(1) as f32 * 2.0;
        let lane = wanted.max(2.0 * TAB_CONTROL).min((self.window.width - reserved).max(120.0));

        let theme_icon = self.settings.theme.icon();
        let theme_message = Message::Run(Cmd::Theme(match self.kind {
            document::ThemeKind::Light => ThemePreference::Dark,
            document::ThemeKind::Dark => ThemePreference::Light,
        }));
        let mut bar = row![widgets::icon_button("house", ICON_BOX, self.active.is_none(), true, "Home", t, Message::GoHome)]
            .spacing(4)
            .align_y(Alignment::Center);
        // With no document open there is no tab lane at all, so "New" sits next to Home.
        if !self.docs.is_empty() {
            bar = bar
                .push(widgets::lane(tabs, TAB_CONTROL).width(Length::Fixed(lane)))
                .push(widgets::ghost_button("plus", "New", t, Message::Run(Cmd::New)));
        } else {
            bar = bar.push(widgets::ghost_button("plus", "New", t, Message::Run(Cmd::New)));
        }
        let bar = bar
            .push(Space::new().width(Length::Fill))
            .push(widgets::icon_button(theme_icon, ICON_BOX, false, true, format!("Theme: {}", self.settings.theme.label()), t, theme_message))
            .push(widgets::icon_button("circle-help", ICON_BOX, false, true, "Keyboard shortcuts", t, Message::Shortcuts(true)))
            .push(widgets::icon_button("info", ICON_BOX, false, true, "About", t, Message::About))
            .spacing(4)
            .align_y(Alignment::Center);

        column![
            container(bar)
             .height(Length::Fixed(TAB_STRIP))
                .width(Length::Fill)
                .padding(Padding { top: 0.0, bottom: 0.0, left: BAR_PAD, right: BAR_PAD })
                .style(t.filled(t.titlebar)),
            widgets::h_divider(t),
        ]
        .into()
    }

    fn mode_bar(&self) -> Element<'_, Message> {
        let t = self.toks;
        // With no document open the view tabs, zoom and compile status would be dead controls.
        let mut left = row![widgets::ghost_button("panel-left", "Menu", t, Message::Palette(true))].spacing(8).height(Length::Fill).align_y(Alignment::Center);
        
        // The compile status sits in front of the command box, in a slot of fixed width so a new
        // compile time or a different diagnostic count cannot move the controls around it.
        let mut right = row![].spacing(8).align_y(Alignment::Center);
        let doc = self.doc();
        if let Some(doc) = doc {
            let mut views = row![].align_y(Alignment::Center);
            for view in [View::Source, View::Split, View::Preview] {
                views = views.push(widgets::mode_tab(view.label(), self.settings.view == view, t, Message::ViewChanged(view)));
            }
            let divider = container(Space::new().width(Length::Fixed(1.0))).width(Length::Fixed(1.0)).height(Length::Fixed(22.0)).style(t.filled(t.divider));
            left = left.push(divider).push(views);
            let (label, color) = status_of(doc, self.fonts_pending, t);
            right = right.push(
                container(widgets::text_button(document::shorten(&label, 22), color, t, Message::TogglePanel(RightPanel::Problems)))
                    .width(Length::Fixed(STATUS_SLOT))
                    .align_x(Alignment::End),
            );
        }
        right = right.push(widgets::search_box("Find a command", 236.0, t, Message::Palette(true)));
        if doc.is_some() {
            let choices: Vec<ZoomStep> = ZOOM_STEPS.iter().map(|step| ZoomStep(*step)).collect();
            let zoom = pick_list(choices, Some(ZoomStep(self.settings.zoom)), |step: ZoomStep| Message::ZoomStep(step))
                .text_size(12.5)
                .padding(Padding { top: 7.0, bottom: 7.0, left: 10.0, right: 8.0 })
                .width(Length::Fixed(88.0))
                .style(t.pick_list());
            right = right.push(zoom);
        }
        let bar = row![left, Space::new().width(Length::Fill), right].align_y(Alignment::Center);

        column![
            container(bar)
                .height(Length::Fixed(MODE_BAR - 1.0))
                .width(Length::Fill)
                .padding(Padding { top: 0.0, bottom: 0.0, left: BAR_PAD, right: BAR_PAD })
                .style(t.filled(t.chrome)),
            widgets::h_divider(t),
        ]
        .into()
    }

    // ------------------------------------------------------------------ workspace

    fn body<'a>(&'a self, doc: &'a Doc) -> Element<'a, Message> {
        let t = self.toks;
        let split = self.settings.view == View::Split;
        let show_source = self.settings.view != View::Preview;
        let show_pages = self.settings.view != View::Source;

        let mut middle = row![].height(Length::Fill);
        if show_source {
            let source = self.source_pane(doc);
            middle = middle.push(if split { container(source).width(Length::Fixed(self.source_width())).height(Length::Fill) } else { container(source).width(Length::Fill).height(Length::Fill) });
        }
        if split {
            middle = middle.push(self.grip(true, t));
        }
        if show_pages {
            // The pages take whatever is left, so rounding never overflows the row.
            middle = middle.push(container(self.pages_pane(doc)).width(Length::Fill).height(Length::Fill));
        }

        let mut workspace = row![self.left_rail(doc), middle].height(Length::Fill);
        if let Some(panel) = self.right {
            workspace = workspace.push(self.grip(false, t)).push(self.side_panel(doc, panel));
        }
        workspace = workspace.push(self.right_rail());
        workspace.width(Length::Fill).into()
    }

    /// The 6 px grip gap between two panes: it keeps each side's scroll area off the divider,
    /// which is what makes the drag reach the handle.
    fn grip<'a>(&self, split: bool, t: Tokens) -> Element<'a, Message> {
        mouse_area(container(Space::new().width(Length::Fixed(GRIP * 2.0))).height(Length::Fill).style(t.filled(t.divider)))
            .on_press(Message::GripPress(split))
            .interaction(mouse::Interaction::ResizingHorizontally)
            .into()
    }

    fn source_pane<'a>(&'a self, doc: &'a Doc) -> Element<'a, Message> {
        let t = self.toks;
        let mut editor = code_editor(&doc.content)
            .on_action(Message::Edit)
            .font(theme::mono())
            .size(EDITOR_SIZE)
            .line_height(iced::Pixels(EDITOR_LINE_HEIGHT))
            .padding(Padding { top: 8.0, bottom: 8.0, left: 6.0, right: 6.0 })
            .style(t.editor_style())
            .width(Length::Fill)
            .height(Length::Fill);
        if self.settings.vim {
            editor = editor.key_binding(self.binding());
        }

        let mut pane = column![self.source_header(doc), widgets::h_divider(t), editor].height(Length::Fill);
        if self.settings.vim {
            if let Some((sigil, line)) = doc.vim.prompt() {
                pane = pane.push(
                    container(text(format!("{sigil}{line}▎")).font(theme::mono()).size(12.5).color(t.text))
                        .height(Length::Fixed(26.0))
                        .width(Length::Fill)
                        .padding(Padding { top: 4.0, bottom: 2.0, left: 10.0, right: 10.0 })
                        .style(t.bordered(t.field, t.divider)),
                );
            }
        }
        container(pane).width(Length::Fill).height(Length::Fill).style(t.filled(t.panel)).into()
    }

    fn source_header<'a>(&'a self, doc: &'a Doc) -> Element<'a, Message> {
        let t = self.toks;
        // A narrow pane drops the optional parts instead of overlapping them.
        let width = self.source_width();
        let show_mode = width > 360.0;
        let show_message = width > 520.0;
        let mut head = row![].spacing(8).align_y(Alignment::Center);
        head = head.push(text(document::shorten(&doc.name(), if show_mode { 32 } else { 16 })).font(theme::medium()).size(12.5).color(t.text));
        if doc.dirty() {
            head = head.push(text("●").size(9.0).color(t.accent));
        }
        if self.settings.vim && show_mode {
            let mode = doc.vim.status();
            let color = if mode.starts_with("INSERT") {
                t.success
            } else if mode.starts_with("VISUAL") {
                t.warning
            } else if mode.starts_with("COMMAND") {
                t.accent_text
            } else {
                t.text_muted
            };
            head = head.push(
                container(text(mode).font(theme::semibold()).size(10.5).color(color))
                    .padding(Padding { top: 2.0, bottom: 2.0, left: 6.0, right: 6.0 })
                    .style(move |_: &Theme| container::Style {
                        background: Some(iced::Color { a: 0.12, ..color }.into()),
                        border: Border { radius: 4.0.into(), ..Border::default() },
                        ..container::Style::default()
                    }),
            );
            if show_message && !doc.vim.message().is_empty() {
                head = head.push(text(document::shorten(doc.vim.message(), 48)).size(11.0).color(t.text_muted).wrapping(iced::widget::text::Wrapping::None));
            }
        }
        head = head.push(Space::new().width(Length::Fill));
        let cursor = doc.content.cursor();
        head = head.push(text(format!("{}:{} / {}", cursor.line + 1, cursor.column + 1, doc.content.line_count())).font(theme::mono()).size(11.0).color(t.text_faint));
        head = head.push(
            button(center_y(text("Compile").font(theme::medium()).size(11.5).color(t.text)))
                .on_press(Message::Run(Cmd::Compile))
                .height(Length::Fixed(24.0))
                .padding(Padding { top: 0.0, bottom: 0.0, left: 8.0, right: 8.0 })
                .style(move |_theme, status| button::Style {
                    background: matches!(status, button::Status::Hovered).then_some(t.hover.into()),
                    text_color: t.text,
                    border: Border { radius: 6.0.into(), ..Border::default() },
                    ..button::Style::default()
                }),
        );

        // The same header height and a separate 1 px rule, exactly like the side panels, so
        // both rules land on the same row.
        container(head)
            .height(Length::Fixed(theme::PANEL_HEADER_HEIGHT))
            .width(Length::Fill)
            .padding(Padding { top: 0.0, bottom: 0.0, left: 12.0, right: 10.0 })
            .align_y(Alignment::Center)
            .style(t.filled(t.panel))
            .into()
    }

    fn pages_pane<'a>(&'a self, doc: &'a Doc) -> Element<'a, Message> {
        let t = self.toks;
        let errors = doc.diagnostics.iter().filter(|diagnostic| diagnostic.is_error()).count();
        if doc.metas.is_empty() {
            let message = if doc.compiling {
                if self.fonts_pending { "Loading fonts…" } else { "Typesetting…" }
            } else if errors > 0 {
                "No pages — the source does not compile. Open Problems for the first error."
            } else {
                "No pages yet."
            };
            return container(widgets::empty_state("eye", message, t))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(t.filled(t.pasteboard))
                .into();
        }

        let mut content = column![].spacing(pages::PAGE_GAP).align_x(Alignment::Center);
        content = content.push(widgets::vspace(pages::SCROLL_MARGIN));
        let mut index = 0;
        while index < doc.layout.pages.len() {
            let row_start = doc.layout.pages[index].top;
            let mut cells = row![].spacing(pages::SPREAD_GAP).align_y(Alignment::Center);
            while index < doc.layout.pages.len() && (doc.layout.pages[index].top - row_start).abs() < 0.5 {
                let place = doc.layout.pages[index];
                cells = cells.push(self.page_widget(doc, place));
                index += 1;
            }
            content = content.push(cells);
        }
        content = content.push(widgets::vspace(pages::SCROLL_MARGIN));

        let view = widgets::scrolled_both(container(content).width(Length::Fixed(self.pages_band())).align_x(Alignment::Center), t)
            .id(iced::widget::Id::from(format!("pages-{}", doc.id)))
            .on_scroll(Message::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill);

        let mut pane = column![].height(Length::Fill).width(Length::Fill);
        if errors > 0 && !doc.metas.is_empty() {
            pane = pane.push(self.stale_banner(errors, t));
        }
        pane = pane.push(view);
        container(pane).width(Length::Fill).height(Length::Fill).style(t.filled(t.pasteboard)).into()
    }

    fn stale_banner<'a>(&self, errors: usize, t: Tokens) -> Element<'a, Message> {
        container(
            row![
                icons::image::<Message>("triangle-alert", 14.0, t.danger),
                text(document::shorten(&format!("The pages show the last good compile — the source has {errors} error(s)."), 72))
                    .size(12.5)
                    .color(t.danger)
                    .wrapping(iced::widget::text::Wrapping::None),
                Space::new().width(Length::Fill),
                button(center_y(text("Open Problems").font(theme::medium()).size(12.0).color(t.danger)))
                    .on_press(Message::TogglePanel(RightPanel::Problems))
                    .padding(Padding { top: 0.0, bottom: 0.0, left: 8.0, right: 8.0 })
                    .height(Length::Fixed(22.0))
                    .style(move |_theme, status| button::Style {
                        background: matches!(status, button::Status::Hovered).then_some(iced::Color { a: 0.12, ..t.danger }.into()),
                        text_color: t.danger,
                        border: Border { radius: 5.0.into(), ..Border::default() },
                        ..button::Style::default()
                    }),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fixed(30.0))
        .padding(Padding { top: 0.0, bottom: 0.0, left: 14.0, right: 10.0 })
        .style(move |_: &Theme| container::Style {
            background: Some(iced::Color { a: 0.1, ..t.danger }.into()),
            ..container::Style::default()
        })
        .into()
    }

    fn page_widget<'a>(&'a self, doc: &'a Doc, place: Placement) -> Element<'a, Message> {
        let t = self.toks;
        let size = (place.width.max(1.0), place.height.max(1.0));
        let body: Element<'_, Message> = match doc.pages.get(place.page) {
            Some(slot) => match (&slot.handle, slot.error.as_deref()) {
                (Some(handle), _) => image(handle.clone()).width(Length::Fixed(size.0)).height(Length::Fixed(size.1)).into(),
                (None, Some(error)) => text(error).size(13.0).color(t.danger).into(),
                (None, None) => text(format!("page {} …", place.page + 1)).size(13.0).color(t.text_faint).into(),
            },
            None => text(format!("page {} …", place.page + 1)).size(13.0).color(t.text_faint).into(),
        };
        let page = container(container(body).width(Length::Fixed(size.0)).height(Length::Fixed(size.1))).style(t.page());

        let mut element: Element<'_, Message> = page.into();
        if let Some((hover_page, local)) = doc.hover {
            if hover_page == place.page {
                if let Some(meta) = doc.metas.get(place.page) {
                    let zoom = self.settings.zoom;
                    if let Some(hit) = pages::nearest_hit(meta, local.0, local.1) {
                        let width = ((hit.x1 - hit.x0) * zoom).max(1.0);
                        let height = ((hit.y1 - hit.y0) * zoom).max(1.0);
                        let outline = container(Space::new().width(Length::Fixed(width)).height(Length::Fixed(height))).style(move |_: &Theme| container::Style {
                            background: Some(iced::Color { a: 0.16, ..t.accent }.into()),
                            border: Border { radius: 2.0.into(), ..Border::default() },
                            ..container::Style::default()
                        });
                        element = stack([
                            element,
                            container(container(outline).padding(Padding { top: (hit.y0 * zoom).max(0.0), bottom: 0.0, left: (hit.x0 * zoom).max(0.0), right: 0.0 }))
                                .width(Length::Fixed(size.0))
                                .height(Length::Fixed(size.1))
                                .into(),
                        ])
                        .into();
                    }
                }
            }
        }

        let page_id = place.page;
        let zoom = self.settings.zoom;
        mouse_area(element)
            .on_move(move |point: iced::Point| Message::PageHover(page_id, (point.x / zoom, point.y / zoom)))
            .on_press(Message::PagePress(page_id))
            .on_exit(Message::PageLeave)
            .interaction(if self.pan { mouse::Interaction::Grabbing } else { mouse::Interaction::Pointer })
            .into()
    }

    // ------------------------------------------------------------------ rails

    fn left_rail<'a>(&'a self, doc: &'a Doc) -> Element<'a, Message> {
        let t = self.toks;
        let has_pages = !doc.metas.is_empty();
        let rail = column![
            widgets::icon_button("save", 34.0, false, true, "Save (Ctrl+S)", t, Message::Run(Cmd::Save)),
            widgets::icon_button("file-output", 34.0, false, has_pages, "Export PDF (Ctrl+E)", t, Message::Run(Cmd::ExportPdf)),
            widgets::icon_button("refresh-cw", 34.0, false, true, "Compile now (Ctrl+R)", t, Message::Run(Cmd::Compile)),
            widgets::vspace(4.0),
            widgets::icon_button("hand", 34.0, self.pan, true, if self.pan { "Pan: on" } else { "Pan (drag the pages)" }, t, Message::TogglePan),
            widgets::icon_button("zoom-out", 34.0, false, true, "Zoom out (Ctrl+−)", t, Message::Run(Cmd::ZoomOut)),
            widgets::icon_button("zoom-in", 34.0, false, true, "Zoom in (Ctrl++)", t, Message::Run(Cmd::ZoomIn)),
            widgets::icon_button("maximize", 34.0, self.settings.fit_width, true, "Fit to width (Ctrl+2)", t, Message::Run(Cmd::FitWidth)),
            widgets::icon_button("gauge", 34.0, false, true, "Actual size (Ctrl+0)", t, Message::Run(Cmd::Zoom100)),
            Space::new().height(Length::Fill),
            widgets::icon_button("chevron-down", 32.0, false, has_pages, "Next page", t, Message::StepPage(true)),
            widgets::icon_button("chevron-up", 32.0, false, has_pages, "Previous page", t, Message::StepPage(false)),
            text(format!("{}", doc.metas.len())).font(theme::mono()).size(10.5).color(t.text_faint),
            // The page box matches the 32 px page buttons above it and centres its number,
            // so the whole bottom cluster shares one axis.
            widgets::tip(
                container(
                    text_input("", &doc.page_input)
                        .id(PAGE_INPUT)
                        .on_input(Message::PageInput)
                        .on_submit(Message::PageSubmit)
                        .font(theme::mono())
                        .size(12.0)
                        .padding(Padding { top: 4.0, bottom: 4.0, left: 2.0, right: 2.0 })
                        .align_x(Alignment::Center)
                        .width(Length::Fixed(32.0))
                        .style(widgets::field_style(t, 5.0)),
                )
                .width(Length::Fixed(32.0))
                .center_x(Length::Fixed(32.0)),
                "Page — type a number and press Enter",
                t,
            ),
        ]
        .spacing(4)
        .align_x(Alignment::Center)
        .padding(Padding { top: 8.0, bottom: 8.0, left: 6.0, right: 6.0 });
        container(rail).width(Length::Fixed(RAIL)).height(Length::Fill).style(t.bordered(t.chrome, t.divider)).into()
    }

    fn right_rail(&self) -> Element<'_, Message> {
        let t = self.toks;
        let log_lines = self.compiler.log().len();
        let mut rail = column![].spacing(4).align_x(Alignment::Center).padding(Padding { top: 8.0, bottom: 8.0, left: 6.0, right: 6.0 });
        for panel in RightPanel::ALL {
            let present = self.doc().is_some_and(|doc| panel.present(doc.diagnostics.len(), !doc.metas.is_empty(), log_lines));
            let selected = self.right == Some(panel);
            let button = widgets::icon_button(panel.icon(), 34.0, selected, true, panel.label(), t, Message::TogglePanel(panel));
            if present && !selected {
                // A dot says "this closed panel has something in it".
                let dot = container(container(Space::new().width(Length::Fixed(6.0)).height(Length::Fixed(6.0))).style(move |_: &Theme| container::Style {
                    background: Some(t.accent.into()),
                    border: Border { radius: 3.0.into(), ..Border::default() },
                    ..container::Style::default()
                }))
                .width(Length::Fixed(34.0))
                .height(Length::Fixed(34.0))
                .padding(Padding { top: 5.0, right: 4.0, bottom: 0.0, left: 0.0 })
                .align_x(Alignment::End)
                .align_y(Alignment::Start);
                rail = rail.push(stack([button, dot.into()]).width(Length::Fixed(34.0)).height(Length::Fixed(34.0)));
            } else {
                rail = rail.push(button);
            }
        }
        rail = rail
            .push(Space::new().height(Length::Fill))
            .push(widgets::icon_button("circle-help", 34.0, self.shortcuts, true, "Keyboard shortcuts", t, Message::Shortcuts(true)))
            .push(widgets::icon_button("info", 34.0, false, true, "About", t, Message::About));
        container(rail).width(Length::Fixed(RAIL)).height(Length::Fill).style(t.bordered(t.chrome, t.divider)).into()
    }

    // ------------------------------------------------------------------ side panels

    fn side_panel<'a>(&'a self, doc: &'a Doc, panel: RightPanel) -> Element<'a, Message> {
        let body: Element<'a, Message> = match panel {
            RightPanel::Problems => self.problems_panel(doc),
            RightPanel::Document => self.document_panel(doc),
            RightPanel::Log => self.log_panel(),
        };
        let width = self.settings.side_width.clamp(SIDE_MIN, SIDE_MAX);
        widgets::panel(panel.label(), body, width, self.toks, Message::ClosePanel)
    }

    fn problems_panel<'a>(&'a self, doc: &'a Doc) -> Element<'a, Message> {
        let t = self.toks;
        let query = self.filter.trim().to_lowercase();
        let matching: Vec<&Diagnostic> = doc.diagnostics.iter().filter(|diagnostic| query.is_empty() || diagnostic.message.to_lowercase().contains(&query)).collect();
        let errors = doc.diagnostics.iter().filter(|diagnostic| diagnostic.is_error()).count();
        let warnings = doc.diagnostics.len() - errors;

        let mut body = column![
            text_input("Filter…", &self.filter)
                .id(PROBLEM_FILTER)
                .on_input(Message::Filter)
                .font(theme::regular())
                .size(12.5)
                .padding(7)
                .style(widgets::field_style(t, 7.0)),
            widgets::vspace(8.0),
            row![
                widgets::badge(&format!("{errors} error{}", if errors == 1 { "" } else { "s" }), if errors > 0 { iced::Color { a: 0.12, ..t.danger } } else { t.hover }, if errors > 0 { t.danger } else { t.text_faint }),
                widgets::badge(&format!("{warnings} warning{}", if warnings == 1 { "" } else { "s" }), if warnings > 0 { iced::Color { a: 0.14, ..t.warning } } else { t.hover }, if warnings > 0 { t.warning } else { t.text_faint }),
            ]
            .spacing(6),
            widgets::vspace(8.0),
        ]
        .spacing(0);

        if matching.is_empty() {
            let message = if doc.diagnostics.is_empty() { "Nothing to report — the document typesets." } else { "No problem matches the filter." };
            body = body.push(widgets::empty_state(if doc.diagnostics.is_empty() { "check" } else { "search" }, message, t));
        } else {
            let mut list = column![].spacing(2);
            for diagnostic in matching {
                let location = match &diagnostic.file {
                    Some(file) => format!("{file}:{}:{}", diagnostic.line + 1, diagnostic.column + 1),
                    None => format!("{}:{}", diagnostic.line + 1, diagnostic.column + 1),
                };
                let jump = if diagnostic.file.is_none() { Some(Message::PlaceCaret(doc.id, diagnostic.line, diagnostic.column)) } else { None };
                list = list.push(widgets::problem_row(diagnostic.message.clone(), location, &diagnostic.hints, diagnostic.is_error(), jump, t));
            }
            body = body.push(list);
        }
        body.into()
    }

    fn document_panel<'a>(&'a self, doc: &'a Doc) -> Element<'a, Message> {
        let t = self.toks;
        let text_value = doc.text();
        let words = text_value.split_whitespace().count();
        let rows = [
            ("Pages".to_string(), doc.metas.len().to_string()),
            ("Words".to_string(), words.to_string()),
            ("Characters".to_string(), text_value.chars().count().to_string()),
            ("Lines".to_string(), doc.content.line_count().to_string()),
            ("Last compile".to_string(), if doc.metas.is_empty() { "—".to_string() } else { format!("{} ms", doc.millis) }),
            ("State".to_string(), if doc.dirty() { "unsaved changes".to_string() } else { "saved".to_string() }),
        ];
        let mut stats = column![].spacing(4);
        for (label, value) in rows {
            stats = stats.push(
                row![
                    text(label).font(theme::regular()).size(12.0).color(t.text_muted),
                    Space::new().width(Length::Fill),
                    text(value).font(theme::medium()).size(12.0).color(t.text),
                ]
                .align_y(Alignment::Center),
            );
        }
        let mut sizes = column![].spacing(2);
        for (index, meta) in doc.metas.iter().take(8).enumerate() {
            sizes = sizes.push(
                row![
                    text(format!("{}", index + 1)).font(theme::mono()).size(11.5).color(t.text_muted),
                    Space::new().width(Length::Fill),
                    text(format!("{:.0} × {:.0} pt", meta.width, meta.height)).font(theme::regular()).size(11.5).color(t.text_muted),
                ],
            );
        }
        let mut body = column![
            text(doc.name()).font(theme::semibold()).size(14.0).color(t.text),
            text(doc.path.display().to_string()).font(theme::regular()).size(10.5).color(t.text_faint),
            widgets::vspace(10.0),
            stats,
            widgets::vspace(14.0),
            widgets::section_title("Page sizes", t),
            widgets::vspace(4.0),
            sizes,
        ]
        .spacing(0);
        if let Some(error) = &self.error {
            body = body.push(widgets::vspace(12.0)).push(text(error.clone()).font(theme::regular()).size(12.0).color(t.danger));
        }
        body.into()
    }

    fn log_panel(&self) -> Element<'_, Message> {
        let t = self.toks;
        let lines = self.compiler.log();
        if lines.is_empty() {
            return widgets::empty_state("square-terminal", "Nothing logged yet. Compiles and exports are recorded here.", t);
        }
        let mut list = column![].spacing(3);
        for line in lines.iter().rev() {
            list = list.push(text(line.clone()).font(theme::mono()).size(11.0).color(t.text_muted));
        }
        list.into()
    }

    // ------------------------------------------------------------------ welcome

    fn welcome(&self) -> Element<'_, Message> {
        let t = self.toks;
        let mut content = column![
            text("PDF Editor").font(theme::semibold()).size(24.0).color(t.text),
            widgets::vspace(6.0),
            text("Write in Typst. The pages beside the source are the document.").font(theme::regular()).size(13.0).color(t.text_muted),
            widgets::vspace(18.0),
            row![
                widgets::icon_pill("file-plus", "New document", true, t, Message::Run(Cmd::New)),
                widgets::icon_pill("plus", "Open a .typ file…", false, t, Message::OpenDialog),
            ]
            .spacing(8),
            widgets::vspace(12.0),
            text("Typst source in, PDF out — nothing leaves this machine.").font(theme::regular()).size(11.5).color(t.text_faint),
        ]
        .spacing(0)
        .align_x(Alignment::Center);

        let recents: Vec<_> = self.settings.session.iter().filter(|entry| entry.path.is_file()).take(5).collect();
        if !recents.is_empty() {
            let mut list = column![].spacing(2);
            for entry in recents {
                let name = entry.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                let parent = entry.path.parent().map(|path| path.display().to_string()).unwrap_or_default();
                list = list.push(widgets::row_button("file-text", name, parent, true, t, Message::OpenPath(entry.path.clone())));
            }
            content = content.push(widgets::vspace(28.0)).push(widgets::section_title("Recent documents", t)).push(widgets::vspace(4.0));
            content = content.push(container(list).width(Length::Fixed(420.0)));
        }
        if let Some(error) = &self.error {
            content = content.push(widgets::vspace(12.0)).push(text(error.clone()).font(theme::regular()).size(12.5).color(t.danger));
        }
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(t.filled(t.pasteboard))
            .into()
    }

    // ------------------------------------------------------------------ overlays

    fn palette_overlay(&self) -> Element<'_, Message> {
        let t = self.toks;
        let query = self.palette_query.trim().to_lowercase();
        let mut hits: Vec<(usize, Command)> = self.commands().into_iter().filter_map(|command| score(command.label, &query).map(|rank| (rank, command))).collect();
        hits.sort_by_key(|(rank, _)| *rank);
        hits.truncate(10);

        let mut results = column![].spacing(2);
        for (_, command) in hits {
            results = results.push(widgets::row_button(command.icon, command.label, command.detail, true, t, Message::Run(command.cmd)));
        }

        let field = text_input("Search tools and commands…", &self.palette_query)
            .id(PALETTE_INPUT)
            .on_input(Message::PaletteQuery)
            .on_submit(Message::PaletteSubmit)
            .font(theme::regular())
            .size(14.0)
            .padding(6)
            .width(Length::Fill)
            .style(widgets::field_style(t, 8.0));
        let card = container(
            column![
                row![icons::image::<Message>("search", 18.0, t.text_muted), field, widgets::badge("Esc", t.hover, t.text_faint)]
                    .spacing(10)
                    .align_y(Alignment::Center),
                widgets::h_divider(t),
                widgets::vspace(4.0),
                widgets::scrolled(results, t).height(Length::Fixed(360.0)),
                widgets::vspace(4.0),
            ]
            .spacing(8),
        )
        .width(Length::Fixed(560.0))
        .padding(10)
        .style(widgets::floating_card(t, 0.22, 10.0, 30.0));

        let positioned = container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding { top: 96.0, right: 0.0, bottom: 0.0, left: 0.0 })
            .align_x(Alignment::Center)
            .align_y(Alignment::Start);
        stack([widgets::scrim(0.08, t, Message::Palette(false)), positioned.into()]).into()
    }

    fn shortcuts_overlay(&self) -> Element<'_, Message> {
        let t = self.toks;
        let rows = [
            ("Ctrl+N", "New document"),
            ("Ctrl+O", "Open a document"),
            ("Ctrl+S", "Save"),
            ("Ctrl+Shift+S", "Save as…"),
            ("Ctrl+E", "Export PDF"),
            ("Ctrl+R / Ctrl+Enter", "Compile now"),
            ("Ctrl+W", "Close the tab"),
            ("Ctrl+Tab", "Next tab"),
            ("Ctrl+K", "Command palette"),
            ("Ctrl++ / Ctrl+−", "Zoom in / out"),
            ("Ctrl + wheel", "Zoom around the pointer"),
            ("Ctrl+0", "Actual size (100%)"),
            ("Ctrl+2", "Fit to width"),
            ("Click a page", "Go to the line and column that set the text"),
            ("Page Down / Page Up", "Next / previous page"),
            ("Home / End", "First / last page"),
            ("Esc", "Close this window"),
        ];
        let mut list = column![].spacing(6);
        for (keys, what) in rows {
            list = list.push(
                row![
                    text(keys).font(theme::mono()).size(12.0).color(t.text),
                    Space::new().width(Length::Fill),
                    text(what).font(theme::regular()).size(12.5).color(t.text_muted),
                ]
                .spacing(20)
                .align_y(Alignment::Center),
            );
        }
        let card = container(
            column![
                row![
                    text("Keyboard shortcuts").font(theme::semibold()).size(15.0).color(t.text),
                    Space::new().width(Length::Fill),
                    widgets::icon_button("x", 24.0, false, true, "Close", t, Message::Shortcuts(false)),
                ]
                .align_y(Alignment::Center),
                widgets::h_divider(t),
                widgets::scrolled(container(list).padding(Padding { top: 10.0, right: 0.0, bottom: 4.0, left: 0.0 }), t).height(Length::Fixed(380.0)),
            ]
            .spacing(10),
        )
        .width(Length::Fixed(520.0))
        .padding(14)
        .style(widgets::floating_card(t, 0.24, 12.0, 34.0));

        let positioned = container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center);
        stack([widgets::scrim(0.24, t, Message::Shortcuts(false)), positioned.into()]).into()
    }

    fn confirm_overlay(&self) -> Element<'_, Message> {
        let t = self.toks;
        let Some(id) = self.confirm else { return Space::new().height(0).into() };
        let name = self.docs.iter().find(|doc| doc.id == id).map(|doc| doc.name()).unwrap_or_default();
        let card = container(
            column![
                row![
                    text("Unsaved changes").font(theme::semibold()).size(15.0).color(t.text),
                    Space::new().width(Length::Fill),
                    widgets::icon_button("x", 24.0, false, true, "Cancel", t, Message::ConfirmClose(id, CloseChoice::Cancel)),
                ]
                .align_y(Alignment::Center),
                widgets::h_divider(t),
                text(format!("{name} has changes that are not in the file.")).font(theme::regular()).size(12.5).color(t.text_muted),
                widgets::vspace(14.0),
                row![
                    Space::new().width(Length::Fill),
                    widgets::pill_button("Cancel", false, t, Message::ConfirmClose(id, CloseChoice::Cancel)),
                    widgets::pill_button("Close without saving", false, t, Message::ConfirmClose(id, CloseChoice::Discard)),
                    widgets::pill_button("Save and close", true, t, Message::ConfirmClose(id, CloseChoice::Save)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(10)
            .width(Length::Fixed(480.0)),
        )
        .padding(16)
        .style(widgets::floating_card(t, 0.24, 12.0, 34.0));

        let positioned = container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center);
        stack([widgets::scrim(0.24, t, Message::ConfirmClose(id, CloseChoice::Cancel)), positioned.into()]).into()
    }

    fn error_banner(&self) -> Element<'_, Message> {
        let t = self.toks;
        let message = self.error.clone().unwrap_or_default();
        let card = container(
            row![
                icons::image::<Message>("triangle-alert", 15.0, t.danger),
                text(message).font(theme::regular()).size(12.5).color(t.danger).width(Length::Fill),
                widgets::icon_button("x", 22.0, false, true, "Dismiss", t, Message::Dismiss),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(Padding { top: 8.0, right: 8.0, bottom: 8.0, left: 12.0 })
        .style(widgets::floating_card(t, 0.18, 6.0, 20.0));

        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding { top: 56.0, right: 0.0, bottom: 0.0, left: 0.0 })
            .align_x(Alignment::Center)
            .align_y(Alignment::Start)
            .into()
    }
}

impl Drop for App {
    /// Remember what was open and how the window was laid out.
    fn drop(&mut self) {
        self.settings.session = self.docs.iter().filter(|doc| doc.saved.is_some()).map(|doc| SessionDoc { path: doc.path.clone(), page: doc.page }).collect();
        self.settings.session_active = self.docs.iter().position(|doc| Some(doc.id) == self.active).unwrap_or(0);
        document::save_settings(&self.settings);
    }
}
