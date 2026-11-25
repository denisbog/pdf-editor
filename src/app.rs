//! The editor window, in PdfCraft's chrome: tab strip, mode bar, tool rails, a pasteboard of
//! typeset pages, and the Typst source beside them.
//!
//! The pages are compiled from the buffer on a background thread and rasterised on demand, so
//! typing stays responsive; a click on a page puts the caret on the line that set the text
//! under the pointer, which is what makes the page view feel editable rather than a preview.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, vec2};
use typst_layout::PagedDocument;

use crate::editor::{self, Editor};
use crate::engine::{self, Compiler, Diagnostic, Event, PageMeta, RasterPool, Task};
use crate::icons;
use crate::theme::{self, ThemeKind, ThemePreference, Tokens};
use crate::vim;
use crate::widgets;

const PAGE_GAP: f32 = 20.0;
const PAGE_SHADOW: f32 = 18.0;
const SPREAD_GAP: f32 = 14.0;
const FIT_SPREAD_MIN: f32 = 0.8;
const ZOOM_MIN: f32 = 0.1;
const ZOOM_MAX: f32 = 6.0;
const ZOOM_STEPS: [f32; 17] = [0.1, 0.25, 0.33, 0.5, 0.66, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 32.0];
/// Space above the first page and below the last, for the drop shadow.
const SCROLL_MARGIN: f32 = 22.0;
/// Width of a right-hand panel (Problems, Document, Log).
const SIDE_WIDTH: f32 = 312.0;

const WELCOME: &str = r#"#set page(paper: "a4", margin: (x: 2.4cm, y: 2.5cm))
#set text(font: "Libertinus Serif", size: 11pt)
#set par(justify: true, leading: 0.68em)
#set heading(numbering: "1.1")

#align(center)[
  #text(18pt, weight: "bold")[Untitled]
  #v(6pt)
  #text(10pt, fill: luma(120))[A document set with Typst]
]

= Start writing

The text on the left is the whole document. Every edit is typeset again into the pages on
the right, and *Export PDF* writes exactly what you see — no separate compile step.

== What this editor does

- The page view is the typeset document, not a separate preview.
- Click a word on a page and the caret goes to the line that set it.
- Mistakes are listed in the *Problems* panel, with the line they belong to.

#table(
  columns: 2,
  [*Feature*], [*Where*],
  [Compile], [Ctrl+R],
  [Export PDF], [Ctrl+E],
  [Save], [Ctrl+S],
)
"#;

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum View {
    Source,
    #[default]
    Split,
    Preview,
}

impl View {
    fn label(self) -> &'static str {
        match self {
            View::Source => "Source",
            View::Split => "Split",
            View::Preview => "Preview",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RightPanel {
    Problems,
    Document,
    Log,
}

/// A document to reopen on the next launch, and the page the editor was on.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionDoc {
    pub path: PathBuf,
    pub page: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub zoom: f32,
    pub theme: ThemePreference,
    pub view: View,
    /// Modal (Vim) editing in the source pane.
    pub vim: bool,
    pub session: Vec<SessionDoc>,
    pub session_active: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self { zoom: 1.0, theme: ThemePreference::Light, view: View::Split, vim: true, session: Vec::new(), session_active: 0 }
    }
}

/// A document command raised inside a pass, when `self.docs` is out of the window.
enum PendingAction {
    Save(u64),
    Export(u64),
    Compile(u64),
    Close(u64),
    Open(PathBuf),
}

/// One page's raster as a GPU texture, at `scale` device pixels per point.
#[derive(Default)]
pub struct Slot {
    pub texture: Option<egui::TextureHandle>,
    pub scale: f32,
    pending: Option<(u64, f32)>,
    pub error: Option<String>,
}

pub struct Doc {
    pub id: u64,
    pub path: PathBuf,
    pub editor: Editor,
    /// The text as last written to disk; `None` when the document was never saved.
    pub saved: Option<String>,
    /// Incremented for every compile request; answers for older generations are dropped.
    pub gen: u64,
    pub compiling: bool,
    pub edited_at: Option<f64>,
    pub millis: u64,
    pub metas: Vec<PageMeta>,
    pub document: Option<Arc<PagedDocument>>,
    pub diagnostics: Vec<Diagnostic>,
    pub pages: Vec<Slot>,
    pub goto: Option<usize>,
    pub page: usize,
    pub page_input: String,
    pub zoom_anchor: Option<(usize, f32, f32)>,
    pub last_pages: Vec<(usize, Rect)>,
    pub spread: bool,
    /// The document the page view was last asked to show (the generation it belongs to).
    pub shown_gen: u64,
}

impl Doc {
    fn name(&self) -> String {
        self.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "untitled.typ".to_string())
    }

    fn dirty(&self) -> bool {
        self.saved.as_deref() != Some(self.editor.text.as_str())
    }

    fn step_page(&mut self, forward: bool) {
        let pages = self.metas.len();
        if pages == 0 {
            return;
        }
        let step = if self.spread { 2 } else { 1 };
        let row = self.page - self.page % step;
        self.goto = Some(if forward { (row + step).min(pages - 1) } else { row.saturating_sub(step) });
    }

    fn go_to_end(&mut self, home: bool) {
        let pages = self.metas.len();
        if pages == 0 {
            return;
        }
        let page = if home { 0 } else { pages - 1 };
        self.goto = Some(page);
        self.page = page;
        self.page_input = (page + 1).to_string();
    }

    fn go_to_typed(&mut self, typed: &str) {
        let typed = typed.trim();
        match typed.parse::<usize>().ok().filter(|number| (1..=self.metas.len()).contains(number)) {
            Some(number) => {
                let page = number - 1;
                self.goto = Some(page);
                self.page = page;
                self.page_input = (page + 1).to_string();
            }
            None => self.page_input = (self.page + 1).to_string(),
        }
    }
}

struct Placement {
    page: usize,
    row: Rect,
    rect: Rect,
}

struct PageLayout {
    pages: Vec<Placement>,
    content_h: f32,
    two_up: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Cmd {
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
struct Command {
    label: &'static str,
    detail: &'static str,
    icon: &'static str,
    cmd: Cmd,
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

pub struct App {
    docs: Vec<Doc>,
    active: Option<u64>,
    next_id: u64,
    /// Compile generations are unique across documents, so a raster can be traced to the one
    /// document it belongs to even when two tabs have the same page number in flight.
    next_gen: u64,
    settings: Settings,
    kind: ThemeKind,
    compiler: Compiler,
    rasters: RasterPool,
    pending_open: Option<PathBuf>,
    pending_close: Option<u64>,
    toast: Option<(String, f64)>,
    error: Option<String>,
    view: View,
    right: Option<RightPanel>,
    pan: bool,
    filter: String,
    palette: bool,
    palette_query: String,
    palette_focus: bool,
    shortcuts: bool,
    fit_width: bool,
    pending_zoom: Option<f32>,
    /// The rasters this frame's page view asks for.
    wanted: Vec<Task>,
    /// Fonts still scanning: the first compile waits for them.
    fonts_pending: bool,
    /// Document commands raised while the document list was borrowed for a pass (the trail
    /// buttons, the Compile button, the Vim command line), applied once it is back.
    pending_actions: Vec<PendingAction>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        cc.egui_ctx.options_mut(|options| options.zoom_with_keyboard = false);
        theme::install_fonts(&cc.egui_ctx);
        let settings = cc.storage.and_then(|storage| eframe::get_value::<Settings>(storage, eframe::APP_KEY)).unwrap_or_default();
        let kind = settings.theme.resolve(cc.egui_ctx.system_theme(), ThemeKind::Light);
        theme::apply(&cc.egui_ctx, kind);
        let rasters = RasterPool::new(cc.egui_ctx.clone(), render_threads());
        let mut app = Self {
            docs: Vec::new(),
            active: None,
            next_id: 1,
            next_gen: 1,
            settings,
            kind,
            compiler: Compiler::spawn(cc.egui_ctx.clone()),
            rasters,
            pending_open: None,
            pending_close: None,
            toast: None,
            error: None,
            view: Default::default(),
            right: None,
            pan: false,
            filter: String::new(),
            palette: false,
            palette_query: String::new(),
            palette_focus: false,
            shortcuts: false,
            fit_width: true,
            pending_zoom: None,
            wanted: Vec::new(),
            fonts_pending: !engine::fonts_ready(),
            pending_actions: Vec::new(),
        };
        app.view = app.settings.view;
        if app.fonts_pending {
            engine::warm_fonts(Some(cc.egui_ctx.clone()));
        }
        // Files named on the command line win; otherwise reopen the last session's documents.
        let named: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).filter(|path| path.is_file()).collect();
        if !named.is_empty() {
            for path in named {
                app.open(path, &cc.egui_ctx, None);
            }
        } else {
            let session = std::mem::take(&mut app.settings.session);
            let active = app.settings.session_active;
            for entry in session.into_iter().filter(|entry| entry.path.is_file()) {
                app.open(entry.path, &cc.egui_ctx, Some(entry.page));
            }
            app.active = app.docs.get(active).map(|doc| doc.id).or_else(|| app.docs.first().map(|doc| doc.id));
        }
        app
    }

    fn doc(&self) -> Option<&Doc> {
        let active = self.active?;
        self.docs.iter().find(|doc| doc.id == active)
    }

    fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), 0.0));
    }

    // ------------------------------------------------------------------ documents

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Typst source", &["typ"]).pick_file() {
            self.pending_open = Some(path);
        }
    }

    fn open(&mut self, path: PathBuf, ctx: &egui::Context, page: Option<usize>) {
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
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                let page = page.unwrap_or(0);
                let gen = self.next_gen;
                self.next_gen += 1;
                let doc = Doc {
                    id,
                    path: path.clone(),
                    editor: Editor::new(text.clone()),
                    saved: Some(text),
                    gen,
                    compiling: true,
                    edited_at: None,
                    millis: 0,
                    metas: Vec::new(),
                    document: None,
                    diagnostics: Vec::new(),
                    pages: Vec::new(),
                    goto: page.into(),
                    page,
                    page_input: (page + 1).to_string(),
                    zoom_anchor: None,
                    last_pages: Vec::new(),
                    spread: false,
                    shown_gen: 0,
                };
                self.compiler.compile(id, doc.gen, path.clone(), doc.editor.text.clone());
                self.docs.push(doc);
                self.active = Some(id);
                self.fit_width = true;
                self.pending_zoom = None;
                self.toast(format!("{name} — compiling"));
                ctx.request_repaint();
            }
            Err(error) => self.error = Some(format!("cannot read {}: {error}", path.display())),
        }
    }

    /// A new document in the current folder, with a starter template.
    fn new_document(&mut self, ctx: &egui::Context) {
        let id = self.next_id;
        self.next_id += 1;
        let folder = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let taken: Vec<PathBuf> = self.docs.iter().map(|doc| doc.path.clone()).collect();
        let path = unique_path(&folder, "untitled.typ", &taken);
        let gen = self.next_gen;
        self.next_gen += 1;
        let doc = Doc {
            id,
            path: path.clone(),
            editor: Editor::new(WELCOME.to_string()),
            saved: None,
            gen,
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
            last_pages: Vec::new(),
            spread: false,
            shown_gen: 0,
        };
        self.compiler.compile(id, doc.gen, path, doc.editor.text.clone());
        self.docs.push(doc);
        self.active = Some(id);
        self.view = View::Split;
        self.fit_width = true;
        ctx.request_repaint();
    }

    fn close_doc(&mut self, id: u64) {
        self.docs.retain(|doc| doc.id != id);
        if self.active == Some(id) {
            self.active = self.docs.last().map(|doc| doc.id);
        }
    }

    /// Ask before closing a document with unwritten changes.
    fn request_close(&mut self, id: u64) {
        let dirty = self.docs.iter().find(|doc| doc.id == id).is_some_and(|doc| doc.dirty());
        if dirty {
            self.pending_close = Some(id);
        } else {
            self.close_doc(id);
        }
    }

    fn recompile(&mut self, id: u64, ctx: &egui::Context) {
        let gen = self.next_gen;
        self.next_gen += 1;
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { return };
        doc.gen = gen;
        doc.compiling = true;
        doc.edited_at = None;
        self.compiler.compile(id, doc.gen, doc.path.clone(), doc.editor.text.clone());
        ctx.request_repaint();
    }

    /// Write the buffer to disk (`save_as` picks a path first).
    fn save(&mut self, id: u64, save_as: bool, ctx: &egui::Context) {
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { return };
        if save_as || doc.saved.is_none() {
            let Some(path) = target_path(&doc.path, "PDF_EDITOR_SAVE_DIR", "typ") else { return };
            doc.path = path;
        }
        let path = doc.path.clone();
        match std::fs::write(&path, doc.editor.text.as_bytes()) {
            Ok(()) => {
                doc.saved = Some(doc.editor.text.clone());
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                self.toast(format!("Saved {name}"));
                // A renamed document needs a fresh compile: its folder (and so its includes) may
                // have changed.
                self.recompile(id, ctx);
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
        let Some(path) = target_path(&doc.path, "PDF_EDITOR_EXPORT_DIR", "pdf") else { return };
        self.compiler.export(path.clone(), document);
        self.toast(format!("Exporting to {}…", path.display()));
    }

    // ------------------------------------------------------------------ engine

    fn poll_engine(&mut self, ctx: &egui::Context) {
        for event in self.compiler.poll() {
            match event {
                Event::Compiled { id, gen, outcome } => {
                    let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { continue };
                    if gen != doc.gen {
                        continue;
                    }
                    doc.compiling = false;
                    doc.millis = outcome.millis;
                    doc.diagnostics = outcome.diagnostics;
                    if let Some(document) = outcome.document {
                        doc.metas = outcome.pages;
                        doc.pages.clear();
                        doc.pages.resize_with(doc.metas.len(), Slot::default);
                        doc.document = Some(document.clone());
                        doc.shown_gen = gen;
                        self.rasters.set_document(gen, document);
                        if let Some(goto) = doc.goto {
                            doc.goto = Some(goto.min(doc.metas.len().saturating_sub(1)));
                        }
                    }
                }
                Event::Exported { path, result } => match result {
                    Ok(len) => self.toast(format!("Exported {} — {} KB", path.display(), len / 1024)),
                    Err(error) => self.error = Some(error),
                },
            }
        }
        // Upload finished rasters.
        for raster in self.rasters.poll() {
            let Some(doc) = self.docs.iter_mut().find(|doc| doc.shown_gen == raster.gen) else { continue };
            let doc_id = doc.id;
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
            let image = egui::ColorImage::from_rgba_premultiplied([raster.width, raster.height], &raster.rgba);
            let texture = ctx.load_texture(format!("page-{doc_id}-{}", raster.page), image, egui::TextureOptions::LINEAR);
            slot.texture = Some(texture);
            slot.scale = raster.scale;
            slot.error = None;
        }
        // Compile what was edited, once the typing has settled.
        let now = ctx.input(|input| input.time);
        let mut compile: Vec<u64> = Vec::new();
        for doc in &self.docs {
            if let Some(at) = doc.edited_at {
                if now - at > editor::DEBOUNCE {
                    compile.push(doc.id);
                }
            }
        }
        for id in compile {
            self.recompile(id, ctx);
        }
        // Keep frames coming while work is in flight.
        if self.docs.iter().any(|doc| doc.compiling) {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
        self.fonts_pending = !engine::fonts_ready();
    }

    // ------------------------------------------------------------------ zoom

    fn zoom_step(&mut self, up: bool) {
        let zoom = self.settings.zoom;
        let next = if up { ZOOM_STEPS.iter().copied().find(|step| *step > zoom * 1.01) } else { ZOOM_STEPS.iter().rev().copied().find(|step| *step < zoom * 0.99) };
        if let Some(next) = next {
            self.pending_zoom = Some(next.clamp(ZOOM_MIN, ZOOM_MAX));
        }
    }

    fn zoom_input(&mut self, ui: &egui::Ui, viewport: Rect, doc: &mut Doc) {
        let delta = ui.input(|input| input.zoom_delta());
        let pointer = ui.ctx().pointer_hover_pos();
        if (delta - 1.0).abs() > 0.001 {
            if let Some(pointer) = pointer.filter(|pointer| viewport.contains(*pointer)) {
                let zoom = self.settings.zoom * delta;
                self.zoom_to(doc, zoom, pointer, viewport);
            }
        }
        if let Some(zoom) = self.pending_zoom.take() {
            self.zoom_to(doc, zoom, viewport.center(), viewport);
        }
    }

    fn zoom_to(&mut self, doc: &mut Doc, zoom: f32, screen: Pos2, viewport: Rect) {
        if let Some(anchor) = anchor_under(doc, screen, viewport.top()) {
            doc.zoom_anchor = Some(anchor);
        }
        self.settings.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.fit_width = false;
    }

    // ------------------------------------------------------------------ commands

    fn run_command(&mut self, cmd: Cmd, ctx: &egui::Context) {
        let active = self.active;
        match cmd {
            Cmd::New => self.new_document(ctx),
            Cmd::Open => self.open_dialog(),
            Cmd::Save => {
                if let Some(id) = active {
                    self.save(id, false, ctx);
                }
            }
            Cmd::SaveAs => {
                if let Some(id) = active {
                    self.save(id, true, ctx);
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
                    self.recompile(id, ctx);
                }
            }
            Cmd::View(view) => self.view = view,
            Cmd::ZoomIn => self.zoom_step(true),
            Cmd::ZoomOut => self.zoom_step(false),
            Cmd::Zoom100 => self.pending_zoom = Some(1.0),
            Cmd::FitWidth => self.fit_width = true,
            Cmd::Panel(panel) => self.right = if self.right == Some(panel) { None } else { Some(panel) },
            Cmd::Theme(preference) => self.settings.theme = preference,
            Cmd::ToggleVim => {
                self.settings.vim = !self.settings.vim;
                let state = if self.settings.vim { "on" } else { "off" };
                self.toast(format!("Vim bindings {state}"));
            }
            Cmd::Shortcuts => self.shortcuts = true,
        }
    }

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
            commands.retain(|command| {
                matches!(command.cmd, Cmd::New | Cmd::Open | Cmd::Theme(_) | Cmd::ToggleVim | Cmd::Shortcuts)
            });
        }
        commands
    }

    // ------------------------------------------------------------------ chrome

    fn tab_strip(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let t = Tokens::get(ctx);
        egui::Panel::top("tab_strip")
            .exact_size(38.0)
            .frame(Frame::NONE.fill(t.titlebar).inner_margin(Margin { left: 8, right: 10, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                let full = ui.max_rect();
                let drag = ui.interact(full, ui.id().with("title_drag"), Sense::click_and_drag());
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if icons::button(ui, "house", 28.0, self.active.is_none(), "Home").clicked() {
                        self.active = None;
                    }
                    let tabs_width = (ui.available_width() - 260.0).max(120.0);
                    egui::ScrollArea::horizontal().id_salt("document_tabs").max_width(tabs_width).auto_shrink([false, true]).show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let mut show = None;
                            let mut close = None;
                            for doc in &self.docs {
                                let (clicked, closed) = document_tab(ui, &t, &doc.name(), Some(doc.id) == self.active, doc.dirty(), doc.id);
                                if clicked {
                                    show = Some(doc.id);
                                }
                                if closed {
                                    close = Some(doc.id);
                                }
                            }
                            if let Some(id) = show {
                                self.active = Some(id);
                            }
                            if let Some(id) = close {
                                self.request_close(id);
                            }
                        });
                    });
                    if widgets::ghost_button(ui, "plus", "New").on_hover_text("New document (Ctrl+N)").clicked() {
                        self.new_document(ctx);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let icon = self.settings.theme.icon();
                        let other = match self.kind {
                            ThemeKind::Light => "dark gray",
                            ThemeKind::Dark => "light gray",
                        };
                        let tip = format!("Display theme: {} — click for {other}", self.settings.theme.label());
                        if icons::button(ui, icon, 28.0, false, &tip).clicked() {
                            self.settings.theme = match self.kind {
                                ThemeKind::Light => ThemePreference::Dark,
                                ThemeKind::Dark => ThemePreference::Light,
                            };
                        }
                        if icons::button(ui, "circle-help", 28.0, false, "Keyboard shortcuts").clicked() {
                            self.shortcuts = true;
                        }
                        if icons::button(ui, "info", 28.0, false, "About").clicked() {
                            self.toast("PDF Editor — a Typst document, typeset page by page");
                        }
                    });
                });
            });
    }

    fn mode_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let t = Tokens::get(ctx);
        egui::Panel::top("mode_bar")
            .exact_size(48.0)
            .frame(Frame::NONE.fill(t.chrome).inner_margin(Margin::symmetric(10, 0)).stroke(Stroke::new(1.0, t.divider)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    self.main_menu(ui, ctx);
                    widgets::vline(ui, 24.0, 6.0);
                    for view in [View::Source, View::Split, View::Preview] {
                        if widgets::mode_tab(ui, view.label(), self.view == view).clicked() {
                            self.view = view;
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if widgets::search_box(ui, "Find a command", 180.0).clicked() {
                            self.palette = true;
                            self.palette_focus = true;
                        }
                        ui.add_space(4.0);
                        self.zoom_button(ui);
                        if let Some(doc) = self.doc() {
                            let (label, color) = status_of(doc, self.fonts_pending);
                            let response = ui.add(
                                egui::Button::new(RichText::new(label).font(theme::medium(12.0)).color(color))
                                    .fill(Color32::TRANSPARENT)
                                    .corner_radius(CornerRadius::same(6)),
                            );
                            if response.on_hover_text("Compile status — click for the Problems panel").clicked() {
                                self.right = Some(RightPanel::Problems);
                            }
                        }
                    });
                });
            });
    }

    fn main_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let response = widgets::ghost_button(ui, "panel-left", "Menu");
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(230.0);
            if ui.button("New document").clicked() {
                self.run_command(Cmd::New, ctx);
                ui.close();
            }
            if ui.button("Open…").clicked() {
                self.open_dialog();
                ui.close();
            }
            if ui.button("Save").clicked() {
                self.run_command(Cmd::Save, ctx);
                ui.close();
            }
            if ui.button("Save as…").clicked() {
                self.run_command(Cmd::SaveAs, ctx);
                ui.close();
            }
            if ui.button("Export PDF…").clicked() {
                self.run_command(Cmd::ExportPdf, ctx);
                ui.close();
            }
            if ui.button("Compile now").clicked() {
                self.run_command(Cmd::Compile, ctx);
                ui.close();
            }
            ui.separator();
            ui.label(RichText::new("View").small().color(Tokens::get(ui.ctx()).text_faint));
            for view in [View::Source, View::Split, View::Preview] {
                if ui.radio(self.view == view, view.label()).clicked() {
                    self.view = view;
                    ui.close();
                }
            }
            ui.separator();
            ui.label(RichText::new("Zoom").small().color(Tokens::get(ui.ctx()).text_faint));
            if ui.button("Zoom in").clicked() {
                self.zoom_step(true);
                ui.close();
            }
            if ui.button("Zoom out").clicked() {
                self.zoom_step(false);
                ui.close();
            }
            if ui.button("Actual size (100%)").clicked() {
                self.pending_zoom = Some(1.0);
                ui.close();
            }
            if ui.button("Fit to width").clicked() {
                self.fit_width = true;
                ui.close();
            }
            ui.separator();
            let vim = if self.settings.vim { "Vim bindings: on" } else { "Vim bindings: off" };
            if ui.button(vim).clicked() {
                self.run_command(Cmd::ToggleVim, ctx);
                ui.close();
            }
            if ui.button("Keyboard shortcuts").clicked() {
                self.shortcuts = true;
                ui.close();
            }
        });
    }

    fn zoom_button(&mut self, ui: &mut egui::Ui) {
        let t = Tokens::get(ui.ctx());
        let mut chosen = None;
        let mut fit = false;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(52.0, 32.0), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect, t.radius, t.hover);
        }
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("{:.0}%", self.settings.zoom * 100.0), theme::medium(12.5), t.text_muted);
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(190.0);
            ui.label(RichText::new("Zoom").small().color(t.text_faint));
            if ui.button("Actual size (100%)").clicked() {
                chosen = Some(1.0);
                ui.close();
            }
            if ui.button("Fit to width").clicked() {
                fit = true;
                ui.close();
            }
            ui.separator();
            for step in ZOOM_STEPS.into_iter().filter(|step| (ZOOM_MIN..=ZOOM_MAX).contains(step)) {
                if ui.radio((self.settings.zoom - step).abs() < 0.005, format!("{:.0}%", step * 100.0)).clicked() {
                    chosen = Some(step);
                    ui.close();
                }
            }
        });
        response.on_hover_text("Zoom — Ctrl + wheel zooms in and out");
        if fit {
            self.fit_width = true;
        }
        if let Some(step) = chosen {
            self.pending_zoom = Some(step);
        }
    }

    fn left_rail(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, doc: &mut Doc) {
        let t = Tokens::get(ctx);
        let dirty = doc.dirty();
        egui::Panel::left("left_rail")
            .exact_size(48.0)
            .frame(Frame::NONE.fill(t.chrome).inner_margin(Margin::symmetric(6, 8)).stroke(Stroke::new(1.0, t.divider)))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let save = icons::button(ui, "save", 36.0, false, if dirty { "Save (Ctrl+S) — unsaved changes" } else { "Save (Ctrl+S)" });
                if save.clicked() {
                    self.pending_actions.push(PendingAction::Save(doc.id));
                }
                if icons::button(ui, "file-output", 36.0, false, "Export PDF (Ctrl+E)").clicked() {
                    self.pending_actions.push(PendingAction::Export(doc.id));
                }
                if icons::button(ui, "refresh-cw", 36.0, false, "Compile now (Ctrl+R)").clicked() {
                    self.pending_actions.push(PendingAction::Compile(doc.id));
                }
                ui.add_space(2.0);
                if icons::button(ui, "hand", 36.0, self.pan, "Pan (drag the pages)").clicked() {
                    self.pan = !self.pan;
                }
                if icons::button(ui, "zoom-out", 36.0, false, "Zoom out (Ctrl+−)").clicked() {
                    self.zoom_step(false);
                }
                if icons::button(ui, "zoom-in", 36.0, false, "Zoom in (Ctrl++)").clicked() {
                    self.zoom_step(true);
                }
                if icons::button(ui, "maximize", 36.0, false, "Fit to width").clicked() {
                    self.fit_width = true;
                }
                if icons::button(ui, "gauge", 36.0, false, "Actual size (100%)").clicked() {
                    self.pending_zoom = Some(1.0);
                }
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    if icons::button(ui, "chevron-down", 32.0, false, "Next page").clicked() {
                        doc.step_page(true);
                    }
                    if icons::button(ui, "chevron-up", 32.0, false, "Previous page").clicked() {
                        doc.step_page(false);
                    }
                    ui.label(RichText::new(doc.metas.len().to_string()).font(theme::regular(11.0)).color(t.text_muted));
                    let edit = egui::TextEdit::singleline(&mut doc.page_input)
                        .id(Id::new(("page-input", doc.id)))
                        .desired_width(32.0)
                        .horizontal_align(Align::Center)
                        .font(theme::medium(12.0))
                        .margin(vec2(2.0, 4.0));
                    let response = Frame::NONE
                        .fill(t.field)
                        .stroke(Stroke::new(1.0, t.border))
                        .corner_radius(CornerRadius::same(5))
                        .show(ui, |ui| ui.add(edit))
                        .inner
                        .on_hover_text("Current page — type a page number and press Enter");
                    if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                        let typed = doc.page_input.clone();
                        doc.go_to_typed(&typed);
                    }
                });
            });
    }

    fn right_rail(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let t = Tokens::get(ctx);
        egui::Panel::right("rail")
            .exact_size(48.0)
            .frame(Frame::NONE.fill(t.chrome).inner_margin(Margin::symmetric(6, 8)).stroke(Stroke::new(1.0, t.divider)))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for (panel, icon, tip) in [
                    (RightPanel::Problems, "triangle-alert", "Problems"),
                    (RightPanel::Document, "file-text", "Document"),
                    (RightPanel::Log, "square-terminal", "Log"),
                ] {
                    if icons::button(ui, icon, 36.0, self.right == Some(panel), tip).clicked() {
                        self.right = if self.right == Some(panel) { None } else { Some(panel) };
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    if icons::button(ui, "circle-help", 36.0, self.shortcuts, "Keyboard shortcuts").clicked() {
                        self.shortcuts = true;
                    }
                    if icons::button(ui, "info", 36.0, false, "About").clicked() {
                        self.toast("PDF Editor — a Typst document, typeset page by page");
                    }
                });
            });
    }

    fn source_panel(&mut self, ui: &mut egui::Ui, doc: &mut Doc, ctx: &egui::Context) {
        let t = Tokens::get(ctx);
        let width = (ui.available_width() * 0.44).clamp(320.0, 820.0);
        egui::Panel::left("source_panel")
            .exact_size(if self.view == View::Source { ui.available_width() } else { width })
            .resizable(self.view == View::Split)
            .frame(Frame::NONE.fill(t.panel).inner_margin(Margin { left: 0, right: 0, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                self.source_header(ui, doc, ctx);
                ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, t.divider));
                ui.add_space(1.0);
                doc.editor.vim_bindings = self.settings.vim;
                egui::Frame::NONE.inner_margin(Margin { left: 6, right: 0, top: 0, bottom: 0 }).show(ui, |ui| {
                    let output = doc.editor.show(ui, &t);
                    let edge = ui.max_rect().right();
                    ui.painter().vline(edge - 0.5, ui.max_rect().y_range(), Stroke::new(1.0, t.divider));
                    if output.changed {
                        let now = ui.input(|input| input.time);
                        doc.edited_at = Some(now);
                    }
                    if let Some(request) = output.request {
                        // The document list is out of the window for this pass, so the command
                        // waits until the pass is over.
                        self.pending_actions.push(match request {
                            vim::Request::Save => PendingAction::Save(doc.id),
                            vim::Request::Close => PendingAction::Close(doc.id),
                            vim::Request::Open(path) => PendingAction::Open(PathBuf::from(path)),
                            vim::Request::Compile => PendingAction::Compile(doc.id),
                        });
                    }
                });
            });
    }

    fn source_header(&mut self, ui: &mut egui::Ui, doc: &mut Doc, ctx: &egui::Context) {
        let t = Tokens::get(ctx);
        let header_rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 38.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(header_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(12.0);
                let dirty = doc.dirty();
                ui.label(RichText::new(doc.name()).font(theme::medium(12.5)).color(t.text));
                if dirty {
                    ui.label(RichText::new("●").font(theme::regular(9.0)).color(t.accent));
                }
                if self.settings.vim {
                    let mode = doc.editor.mode_label();
                    let color = if mode.starts_with("INSERT") {
                        Color32::from_rgb(0x1E, 0x7D, 0x52)
                    } else if mode.starts_with("VISUAL") {
                        Color32::from_rgb(0xC0, 0x5A, 0x1B)
                    } else if mode.starts_with("COMMAND") {
                        t.accent_text
                    } else {
                        t.text_muted
                    };
                    let (rect, _) = ui.allocate_exact_size(vec2(15.0 + mode.chars().count() as f32 * 7.0, 20.0), Sense::hover());
                    ui.painter().rect_filled(rect, CornerRadius::same(4), color.gamma_multiply(0.12));
                    ui.painter().text(rect.center(), Align2::CENTER_CENTER, &mode, theme::semibold(10.5), color);
                }
                ui.label(RichText::new(format!("line {} of {}", doc.editor.cursor_line + 1, doc.editor.line_count())).font(theme::regular(11.0)).color(t.text_faint));
                if !doc.editor.vim.message.is_empty() {
                    ui.add(egui::Label::new(RichText::new(&doc.editor.vim.message).font(theme::regular(11.0)).color(t.text_muted)).truncate());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(10.0);
                    let (label, color) = status_of(doc, self.fonts_pending);
                    ui.label(RichText::new(label).font(theme::medium(11.5)).color(color));
                    if ui.add(egui::Button::new(RichText::new("Compile").font(theme::medium(11.5))).corner_radius(CornerRadius::same(6))).clicked() {
                        self.pending_actions.push(PendingAction::Compile(doc.id));
                    }
                });
            });
        });
    }

    // ------------------------------------------------------------------ pages

    fn viewer(&mut self, ui: &mut egui::Ui, doc: &mut Doc, ctx: &egui::Context) {
        let t = Tokens::get(ui.ctx());
        // A failed compile leaves the last good pages on screen; say so under the mode bar.
        let errors = doc.diagnostics.iter().filter(|diagnostic| diagnostic.is_error()).count();
        if errors > 0 && !doc.metas.is_empty() {
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
            let color = Color32::from_rgb(0xC0, 0x39, 0x2B);
            ui.painter().rect_filled(rect, 0.0, color.gamma_multiply(if t.dark() { 0.22 } else { 0.10 }));
            ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, color.gamma_multiply(0.35)));
            let message = format!("The pages show the last good compile — the source has {errors} error{}.", if errors == 1 { "" } else { "s" });
            ui.painter().text(rect.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, message, theme::regular(12.5), color);
            ui.painter().text(rect.right_center() - vec2(14.0, 0.0), Align2::RIGHT_CENTER, "Open Problems", theme::medium(12.0), color);
            if response.clicked() {
                self.right = Some(RightPanel::Problems);
            }
        }
        let viewport = ui.available_rect_before_wrap();
        if doc.metas.is_empty() {
            // Nothing has typeset yet: either the source does not compile, or the first compile
            // is still running.
            let message = if doc.compiling {
                if self.fonts_pending { "Loading fonts…" } else { "Typesetting…" }
            } else if errors > 0 {
                "No pages — the source does not compile. Open Problems for the first error."
            } else {
                "No pages yet."
            };
            ui.painter().text(viewport.center(), Align2::CENTER_CENTER, message, theme::regular(13.5), t.text_faint);
            return;
        }
        self.zoom_input(ui, viewport, doc);
        // Fit to width waits for the first compile: there is no page size to fit yet.
        if self.fit_width && !doc.metas.is_empty() {
            let band = viewport.width().max(64.0);
            let widest = doc.metas.iter().map(|page| page.width).fold(1.0_f32, f32::max);
            let spread = (band - 32.0 - SPREAD_GAP) / (2.0 * widest);
            let zoom = if spread >= FIT_SPREAD_MIN { spread } else { (band - 32.0) / widest };
            if let Some(anchor) = anchor_under(doc, viewport.center(), viewport.top()) {
                doc.zoom_anchor = Some(anchor);
            }
            self.settings.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
            self.fit_width = false;
        }
        let goto_page = doc.goto.take();
        let zoom = self.settings.zoom;
        let scale = zoom * ctx.pixels_per_point();
        let band = viewport.width().max(64.0);
        let layout = page_layout(&doc.metas, band, zoom);
        doc.spread = layout.two_up;
        let current = current_page(doc, viewport);
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("page-view", doc.id));
        if let Some(page) = goto_page {
            if let Some(place) = layout.pages.iter().find(|place| place.page == page) {
                scroll = scroll.vertical_scroll_offset((place.rect.top() - PAGE_SHADOW).max(0.0));
            }
        } else if let Some((page, fraction, rel_y)) = doc.zoom_anchor.take() {
            if let Some(place) = layout.pages.iter().find(|place| place.page == page) {
                scroll = scroll.vertical_scroll_offset((place.rect.top() + fraction * place.rect.height() - rel_y).max(0.0));
            }
        }

        self.wanted.clear();
        let mut tasks: Vec<Task> = Vec::new();
        let mut clicks: Vec<(usize, Pos2)> = Vec::new();
        let gen = doc.shown_gen;

        scroll.show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let (content_rect, _) = ui.allocate_exact_size(Vec2::new(band, layout.content_h), Sense::hover());
            let origin = content_rect.min.to_vec2();
            if self.pan && ui.input(|input| input.pointer.primary_down()) {
                let delta = ui.input(|input| input.pointer.delta());
                ui.scroll_with_delta(-delta);
            }
            let clip = ui.clip_rect();
            let pointer = ui.ctx().pointer_hover_pos().filter(|pointer| ui.rect_contains_pointer(Rect::from_min_size(*pointer, Vec2::splat(1.0))));

            for place in &layout.pages {
                let page = place.page;
                let band_rect = place.row.translate(origin);
                let page_rect = place.rect.translate(origin);
                let visible = clip.expand(clip.height()).intersects(band_rect);
                let painter = ui.painter_at(band_rect.expand(PAGE_SHADOW));
                painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 14, spread: 0, color: t.page_shadow }.as_shape(page_rect, CornerRadius::ZERO));
                painter.rect_filled(page_rect, CornerRadius::ZERO, Color32::WHITE);

                if doc.shown_gen == 0 {
                    painter.text(page_rect.center(), Align2::CENTER_CENTER, "typesetting…", theme::regular(13.0), t.text_faint);
                } else if let Some(slot) = doc.pages.get(page) {
                    match (&slot.texture, slot.error.as_deref()) {
                        (Some(texture), _) => {
                            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                            painter.image(texture.id(), page_rect, uv, Color32::WHITE);
                        }
                        (None, Some(error)) => {
                            painter.text(page_rect.center(), Align2::CENTER_CENTER, error, theme::regular(13.0), Color32::from_rgb(0xBE, 0x46, 0x3C));
                        }
                        (None, None) => {
                            painter.text(page_rect.center(), Align2::CENTER_CENTER, format!("page {} …", page + 1), theme::regular(13.0), t.text_faint);
                        }
                    }
                }
                painter.rect_stroke(page_rect, CornerRadius::ZERO, Stroke::new(0.5, t.border.gamma_multiply(0.8)), StrokeKind::Outside);

                if visible && doc.shown_gen != 0 {
                    if let Some(slot) = doc.pages.get(page) {
                        let satisfied = slot.texture.is_some() && (slot.scale - scale).abs() < 0.01;
                        let pending = slot.pending.is_some_and(|(_, pending_scale)| (pending_scale - scale).abs() < 0.01);
                        if !satisfied && !pending {
                            let tag = self.rasters.tag();
                            if let Some(slot) = doc.pages.get_mut(page) {
                                slot.pending = Some((tag, scale));
                            }
                            tasks.push(Task { page, scale, tag });
                        }
                    }
                } else if !clip.expand(clip.height() * 3.0).intersects(band_rect) {
                    if let Some(slot) = doc.pages.get_mut(page) {
                        slot.texture = None;
                        slot.scale = 0.0;
                    }
                }

                // The source line under the pointer, marked so a click is obviously a jump.
                if visible {
                    if let (Some(pointer), Some(meta)) = (pointer, doc.metas.get(page)) {
                        if page_rect.contains(pointer) {
                            let x = (pointer.x - page_rect.left()) as f32 / zoom;
                            let y = (pointer.y - page_rect.top()) as f32 / zoom;
                            if engine::line_at(&doc.metas, page, x, y).is_some() {
                                if let Some(hit) = nearest_hit(meta, x, y) {
                                    let rect = Rect::from_min_max(
                                        page_rect.min + vec2(hit.x0 * zoom, hit.y0 * zoom),
                                        page_rect.min + vec2(hit.x1 * zoom, hit.y1 * zoom),
                                    );
                                    painter.rect_filled(rect, 2.0, t.accent.gamma_multiply(0.16));
                                }
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                        }
                    }
                }
            }

            let clicked = ui.input(|input| input.pointer.primary_clicked());
            let click_pos = ui.input(|input| input.pointer.interact_pos()).filter(|position| ui.rect_contains_pointer(Rect::from_min_size(*position, Vec2::splat(1.0))));
            if clicked && !self.pan {
                if let Some(position) = click_pos {
                    for place in &layout.pages {
                        let page_rect = place.rect.translate(origin);
                        if page_rect.contains(position) {
                            clicks.push((place.page, position));
                        }
                    }
                }
            }

            doc.last_pages = layout.pages.iter().map(|place| (place.page, place.rect.translate(origin))).collect();
            if let Some(current) = current {
                if current != doc.page {
                    doc.page = current;
                    doc.page_input = (current + 1).to_string();
                }
            }
        });

        self.wanted = tasks;
        let mut tasks = std::mem::take(&mut self.wanted);
        tasks.sort_by_key(|task| Some(task.page) != current);
        if doc.shown_gen != 0 {
            self.rasters.set_queue(gen, tasks);
        }

        for (page, position) in clicks {
            let page_rect = doc.last_pages.iter().find(|(index, _)| *index == page).map(|(_, rect)| *rect);
            let Some(page_rect) = page_rect else { continue };
            let x = (position.x - page_rect.left()) / zoom;
            let y = (position.y - page_rect.top()) / zoom;
            if let Some(line) = engine::line_at(&doc.metas, page, x, y) {
                doc.editor.go_to_line(line);
                // Leaving "Pages only" narrows the page view; keep the clicked page in sight.
                doc.goto = Some(page);
                if self.view == View::Preview {
                    self.view = View::Split;
                }
            }
        }
    }

    // ------------------------------------------------------------------ overlays

    fn palette(&mut self, ctx: &egui::Context) {
        if !self.palette {
            return;
        }
        let t = Tokens::get(ctx);
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.palette = false;
            self.palette_query.clear();
            return;
        }
        let query = self.palette_query.trim().to_lowercase();
        let mut hits: Vec<(usize, Command)> = self
            .commands()
            .into_iter()
            .filter_map(|command| score(command.label, &query).map(|rank| (rank, command)))
            .collect();
        hits.sort_by_key(|(rank, _)| *rank);
        hits.truncate(10);

        let screen = ctx.content_rect();
        let mut chosen: Option<Cmd> = None;
        egui::Area::new(Id::new("palette"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(Pos2::new(screen.center().x, screen.top() + 96.0))
            .show(ctx, |ui| {
                Frame::popup(ui.style())
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(520.0);
                        ui.horizontal(|ui| {
                            ui.add(icons::image("search", 18.0, t.text_muted));
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut self.palette_query).hint_text("Search tools and commands…").frame(Frame::NONE).font(theme::regular(15.0)).desired_width(f32::INFINITY),
                            );
                            if self.palette_focus {
                                edit.request_focus();
                                self.palette_focus = false;
                            }
                            if (edit.has_focus() || edit.lost_focus()) && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                                if let Some((_, command)) = hits.first() {
                                    chosen = Some(command.cmd);
                                }
                            }
                            edit.request_focus();
                        });
                        ui.separator();
                        for (_, command) in &hits {
                            let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
                            if response.hovered() {
                                ui.painter().rect_filled(rect, CornerRadius::same(6), t.hover);
                            }
                            icons::paint(ui, Rect::from_min_size(rect.min + vec2(8.0, 9.0), vec2(18.0, 18.0)), command.icon, 17.0, t.icon);
                            ui.painter().text(rect.left_center() + vec2(36.0, 0.0), Align2::LEFT_CENTER, command.label, theme::regular(13.5), t.text);
                            ui.painter().text(rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, command.detail, theme::regular(12.0), t.text_faint);
                            if response.clicked() {
                                chosen = Some(command.cmd);
                            }
                        }
                        if hits.is_empty() {
                            ui.label(RichText::new("No matching commands").color(t.text_muted));
                        }
                        ui.add_space(2.0);
                    });
            });
        if let Some(command) = chosen {
            self.palette = false;
            self.palette_query.clear();
            self.run_command(command, ctx);
        }
    }

    fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.shortcuts {
            return;
        }
        let mut open = true;
        egui::Window::new("Keyboard shortcuts").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            for (keys, what) in [
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
                ("Click a page", "Go to the line that set the text"),
                ("Page Down / Page Up", "Next / previous page"),
                ("Home / End", "First / last page"),
                ("Esc", "Close the palette"),
            ] {
                ui.horizontal(|ui| {
                    ui.add_sized([160.0, 18.0], egui::Label::new(RichText::new(keys).monospace().strong()));
                    ui.label(what);
                });
            }
        });
        self.shortcuts = open;
    }

    fn toast_ui(&mut self, ctx: &egui::Context) {
        let Some((message, start)) = self.toast.clone() else { return };
        let now = ctx.input(|input| input.time);
        let start = if start == 0.0 { now } else { start };
        self.toast = Some((message.clone(), start));
        if now - start > 3.5 {
            self.toast = None;
            return;
        }
        // The toast fades on a timer: keep drawing until it is gone.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let t = Tokens::get(ctx);
        egui::Area::new(Id::new("toast"))
            .order(egui::Order::Tooltip)
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -28.0))
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(if t.dark() { Color32::from_rgb(0xEC, 0xEC, 0xEF) } else { Color32::from_rgb(0x2A, 0x2A, 0x2F) })
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        let color = if t.dark() { Color32::from_rgb(0x22, 0x22, 0x26) } else { Color32::WHITE };
                        ui.label(RichText::new(message).color(color));
                    });
            });
    }

    fn close_confirm(&mut self, ctx: &egui::Context) {
        let Some(id) = self.pending_close else { return };
        let Some(doc) = self.docs.iter().find(|doc| doc.id == id) else {
            self.pending_close = None;
            return;
        };
        let name = doc.name();
        let mut decision: Option<&str> = None;
        let mut open = true;
        egui::Window::new("Unsaved changes").open(&mut open).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.label(format!("{name} has changes that are not in the file."));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save and close").clicked() {
                    decision = Some("save");
                }
                if ui.button("Close without saving").clicked() {
                    decision = Some("close");
                }
                if ui.button("Cancel").clicked() {
                    decision = Some("cancel");
                }
            });
        });
        match decision {
            Some("save") => {
                self.pending_close = None;
                self.save(id, false, ctx);
                if self.docs.iter().any(|doc| doc.id == id && !doc.dirty()) {
                    self.close_doc(id);
                }
            }
            Some("close") => {
                self.pending_close = None;
                self.close_doc(id);
            }
            Some("cancel") => self.pending_close = None,
            Some(_) => {}
            None if !open => self.pending_close = None,
            None => {}
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let t = Tokens::get(ctx);
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.24);
            ui.label(RichText::new("PDF Editor").font(theme::semibold(32.0)));
            ui.add_space(6.0);
            ui.label(RichText::new("Write in Typst. The pages beside the source are the document.").color(t.text_muted));
            ui.add_space(18.0);
            if ui.add(egui::Button::new(RichText::new("New document").color(Color32::WHITE).font(theme::medium(13.5))).fill(t.accent).corner_radius(CornerRadius::same(14)).min_size(vec2(150.0, 32.0))).clicked() {
                self.new_document(ctx);
            }
            ui.add_space(8.0);
            if ui.add(egui::Button::new(RichText::new("Open a .typ file…").font(theme::medium(13.5))).corner_radius(CornerRadius::same(14)).min_size(vec2(170.0, 32.0))).clicked() {
                self.open_dialog();
            }
            if let Some(error) = &self.error {
                ui.add_space(12.0);
                ui.colored_label(Color32::from_rgb(0xBE, 0x46, 0x3C), error);
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(path) = self.pending_open.take() {
            self.open(path, &ctx, None);
        }
        let kind = self.settings.theme.resolve(ctx.system_theme(), ThemeKind::Light);
        if kind != self.kind {
            theme::apply(&ctx, kind);
            self.kind = kind;
        }
        self.poll_engine(&ctx);

        let command = ctx.input(|input| input.modifiers.command);
        let shift = ctx.input(|input| input.modifiers.shift);
        if command && ctx.input(|input| input.key_pressed(egui::Key::O)) {
            self.open_dialog();
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::N)) {
            self.new_document(&ctx);
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::S)) {
            if let Some(id) = self.active {
                self.save(id, shift, &ctx);
            }
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::E)) {
            if let Some(id) = self.active {
                self.export(id);
            }
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::R)) {
            if let Some(id) = self.active {
                self.recompile(id, &ctx);
            }
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::W)) {
            if let Some(id) = self.active {
                self.request_close(id);
            }
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::Tab)) {
            let forward = !shift;
            if !self.docs.is_empty() {
                let index = self.active.and_then(|id| self.docs.iter().position(|doc| doc.id == id));
                let next = match index {
                    Some(index) if forward => (index + 1) % self.docs.len(),
                    Some(index) => (index + self.docs.len() - 1) % self.docs.len(),
                    None => 0,
                };
                self.active = Some(self.docs[next].id);
            }
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::K)) {
            self.palette = true;
            self.palette_focus = true;
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals)) {
            self.zoom_step(true);
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::Minus)) {
            self.zoom_step(false);
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::Num0)) {
            self.pending_zoom = Some(1.0);
        }
        if command && ctx.input(|input| input.key_pressed(egui::Key::Num2)) {
            self.fit_width = true;
        }
        if ctx.text_edit_focused() && ctx.input(|input| input.key_pressed(egui::Key::Enter) && input.modifiers.command) {
            if let Some(id) = self.active {
                self.recompile(id, &ctx);
            }
        }
        if !ctx.text_edit_focused() {
            if let Some(doc) = self.active.and_then(|id| self.docs.iter_mut().find(|doc| doc.id == id)) {
                if ctx.input(|input| input.key_pressed(egui::Key::PageDown)) {
                    doc.step_page(true);
                }
                if ctx.input(|input| input.key_pressed(egui::Key::PageUp)) {
                    doc.step_page(false);
                }
                if ctx.input(|input| input.key_pressed(egui::Key::Home)) {
                    doc.go_to_end(true);
                }
                if ctx.input(|input| input.key_pressed(egui::Key::End)) {
                    doc.go_to_end(false);
                }
            }
        }

        self.tab_strip(ui, &ctx);
        self.mode_bar(ui, &ctx);
        let mut docs = std::mem::take(&mut self.docs);
        if let Some(doc) = docs.iter_mut().find(|doc| Some(doc.id) == self.active) {
            self.left_rail(ui, &ctx, doc);
            self.right_rail(ui, &ctx);
            if self.view != View::Preview {
                self.source_panel(ui, doc, &ctx);
            }
        }
        let t = Tokens::get(&ctx);
        let fill = t.pasteboard;
        match self.active.and_then(|id| docs.iter_mut().find(|doc| doc.id == id)) {
            Some(doc) => {
                egui::CentralPanel::default().frame(Frame::NONE.fill(fill)).show(ui, |ui| {
                    let band = ui.available_rect_before_wrap();
                    // The side panel is drawn inside the central area rather than as a panel of
                    // its own: its width is then exactly what the page view leaves over, and a
                    // narrow panel cannot be squeezed by the page view beside it.
                    let width = if self.right.is_some() { SIDE_WIDTH.min(band.width() * 0.55) } else { 0.0 };
                    let view = Rect::from_min_max(band.min, egui::pos2(band.max.x - width, band.max.y));
                    if let Some(panel) = self.right {
                        let panel_rect = Rect::from_min_max(egui::pos2(view.max.x, band.min.y), band.max);
                        ui.painter().rect_filled(panel_rect, 0.0, t.panel);
                        ui.painter().vline(panel_rect.left() + 0.5, panel_rect.y_range(), Stroke::new(1.0, t.divider));
                        let inner = Rect::from_min_max(panel_rect.min + vec2(12.0, 8.0), panel_rect.max - vec2(10.0, 10.0));
                        let mut panel_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
                        panel_ui.set_clip_rect(inner);
                        match panel {
                            RightPanel::Problems => problems_panel(&mut panel_ui, doc, &mut self.filter),
                            RightPanel::Document => document_panel(&mut panel_ui, doc, self.error.as_deref()),
                            RightPanel::Log => log_panel(&mut panel_ui, &self.compiler),
                        }
                    }
                    let mut view_ui = ui.new_child(egui::UiBuilder::new().max_rect(view).layout(Layout::top_down(Align::Min)));
                    view_ui.set_clip_rect(view);
                    self.viewer(&mut view_ui, doc, &ctx);
                });
            }
            None => {
                egui::CentralPanel::default().frame(Frame::NONE.fill(fill)).show(ui, |ui| self.welcome(ui, &ctx));
            }
        }
        docs.append(&mut self.docs);
        self.docs = docs;
        for action in std::mem::take(&mut self.pending_actions) {
            match action {
                PendingAction::Save(id) => self.save(id, false, &ctx),
                PendingAction::Export(id) => self.export(id),
                PendingAction::Compile(id) => self.recompile(id, &ctx),
                PendingAction::Close(id) => self.request_close(id),
                PendingAction::Open(path) => self.pending_open = Some(path),
            }
        }
        self.palette(&ctx);
        self.shortcuts_window(&ctx);
        self.close_confirm(&ctx);
        self.toast_ui(&ctx);
        if self.fonts_pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.settings.session = self.docs.iter().filter(|doc| doc.saved.is_some()).map(|doc| SessionDoc { path: doc.path.clone(), page: doc.page }).collect();
        self.settings.session_active = self.docs.iter().position(|doc| Some(doc.id) == self.active).unwrap_or(0);
        self.settings.view = self.view;
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(10)
    }
}

/// One open document as a tab in the title strip. Returns `(clicked, closed)`.
fn document_tab(ui: &mut egui::Ui, t: &Tokens, name: &str, active: bool, dirty: bool, id: u64) -> (bool, bool) {
    let font = theme::regular(13.0);
    let label: String = if name.chars().count() > 26 { format!("{}…", name.chars().take(25).collect::<String>()) } else { name.to_string() };
    let text_w = ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), t.text).size().x);
    let (rect, response) = ui.allocate_exact_size(vec2(text_w + 66.0, 30.0), Sense::click());
    let close_rect = Rect::from_center_size(rect.right_center() - vec2(16.0, 0.0), vec2(20.0, 20.0));
    let close = ui.interact(close_rect, ui.id().with(("tab-close", id)), Sense::click());
    let fill = if active {
        t.chrome
    } else if response.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, fill);
    icons::paint(ui, Rect::from_min_size(rect.min + vec2(6.0, 7.0), vec2(16.0, 16.0)), "file-text", 15.0, if active { t.accent } else { t.text_muted });
    ui.painter().text(rect.min + vec2(28.0, rect.height() / 2.0), Align2::LEFT_CENTER, label, font, if active { t.text } else { t.text_muted });
    if dirty {
        let dot = Rect::from_center_size(rect.right_center() - vec2(32.0, 0.0), vec2(6.0, 6.0));
        ui.painter().circle_filled(dot.center(), 2.6, t.accent);
    }
    if close.hovered() {
        ui.painter().rect_filled(close_rect, CornerRadius::same(4), t.pressed);
    }
    icons::paint(ui, close_rect, "x", 13.0, if active || close.hovered() { t.text_muted } else { t.text_faint });
    (response.clicked() && !close.clicked(), close.clicked())
}

fn problems_panel(ui: &mut egui::Ui, doc: &mut Doc, filter: &mut String) {
    let t = Tokens::get(ui.ctx());
    widgets::section_title(ui, "Problems");
    let width = ui.available_width();
    ui.add(egui::TextEdit::singleline(filter).hint_text("Filter…").desired_width(width));
    ui.add_space(8.0);
    let query = filter.trim().to_lowercase();
    let matching: Vec<&Diagnostic> = doc.diagnostics.iter().filter(|diagnostic| query.is_empty() || diagnostic.message.to_lowercase().contains(&query)).collect();
    let errors = doc.diagnostics.iter().filter(|diagnostic| diagnostic.is_error()).count();
    let warnings = doc.diagnostics.len() - errors;
    ui.label(RichText::new(format!("{errors} error(s), {warnings} warning(s)")).font(theme::regular(11.5)).color(t.text_faint));
    ui.add_space(6.0);
    if matching.is_empty() {
        let empty = if doc.diagnostics.is_empty() { "Nothing to report — the document typesets." } else { "No problem matches the filter." };
        ui.label(RichText::new(empty).weak());
        return;
    }
    let mut jump = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for diagnostic in matching {
            let color = if diagnostic.is_error() { Color32::from_rgb(0xC0, 0x39, 0x2B) } else { Color32::from_rgb(0xB0, 0x66, 0x00) };
            let where_ = match &diagnostic.file {
                Some(file) => format!("{file}:{}:{}", diagnostic.line + 1, diagnostic.column + 1),
                None => format!("{}:{}", diagnostic.line + 1, diagnostic.column + 1),
            };
            let response = Frame::NONE.corner_radius(CornerRadius::same(6)).inner_margin(Margin { left: 8, right: 8, top: 6, bottom: 7 }).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    icons::paint(ui, Rect::from_min_size(ui.cursor().min, vec2(14.0, 14.0)), if diagnostic.is_error() { "triangle-alert" } else { "info" }, 13.0, color);
                    ui.add_space(2.0);
                    ui.label(RichText::new(&diagnostic.message).font(theme::regular(12.5)).color(t.text));
                });
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.label(RichText::new(where_).font(theme::regular(10.5)).color(t.text_faint));
                });
                for hint in &diagnostic.hints {
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.label(RichText::new(hint).font(theme::regular(11.0)).color(t.text_muted));
                    });
                }
            });
            let response = ui.interact(response.response.rect, ui.id().with(("problem", diagnostic.line, diagnostic.column, &diagnostic.message)), Sense::click());
            if response.hovered() {
                ui.painter().rect_filled(response.rect, CornerRadius::same(6), t.hover);
            }
            if response.clicked() {
                jump = Some(diagnostic.line);
            }
        }
    });
    if let Some(line) = jump {
        doc.editor.go_to_line(line);
    }
}

fn document_panel(ui: &mut egui::Ui, doc: &mut Doc, error: Option<&str>) {
    let t = Tokens::get(ui.ctx());
    widgets::section_title(ui, "Document");
    ui.label(RichText::new(doc.name()).font(theme::medium(14.0)));
    ui.add(egui::Label::new(RichText::new(doc.path.display().to_string()).font(theme::regular(10.5)).color(t.text_faint)).wrap());
    ui.add_space(10.0);
    let words = doc.editor.text.split_whitespace().count();
    let rows = [
        ("Pages".to_string(), doc.metas.len().to_string()),
        ("Words".to_string(), words.to_string()),
        ("Characters".to_string(), doc.editor.text.chars().count().to_string()),
        ("Lines".to_string(), doc.editor.line_count().to_string()),
        ("Last compile".to_string(), if doc.metas.is_empty() { "—".to_string() } else { format!("{} ms", doc.millis) }),
        ("State".to_string(), if doc.dirty() { "unsaved changes".to_string() } else { "saved".to_string() }),
    ];
    for (label, value) in rows {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).font(theme::regular(12.0)).color(t.text_muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(value).font(theme::medium(12.0)).color(t.text));
            });
        });
    }
    ui.add_space(12.0);
    widgets::section_title(ui, "Page sizes");
    for (index, meta) in doc.metas.iter().take(8).enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}", index + 1)).font(theme::regular(11.5)).color(t.text_muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{:.0} × {:.0} pt", meta.width, meta.height)).font(theme::regular(11.5)).color(t.text_muted));
            });
        });
    }
    if let Some(error) = error {
        ui.add_space(12.0);
        ui.colored_label(Color32::from_rgb(0xBE, 0x46, 0x3C), error);
    }
}

fn log_panel(ui: &mut egui::Ui, compiler: &Compiler) {
    widgets::section_title(ui, "Log");
    let lines = compiler.log();
    if lines.is_empty() {
        ui.label(RichText::new("Nothing logged yet. Compiles and exports are recorded here.").weak());
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for line in lines.iter().rev() {
            ui.label(RichText::new(line).monospace().small());
        }
    });
}

/// The compile status in the mode bar and the source header.
fn status_of(doc: &Doc, fonts_pending: bool) -> (String, Color32) {
    let error = Color32::from_rgb(0xC0, 0x39, 0x2B);
    let warn = Color32::from_rgb(0xB0, 0x66, 0x00);
    let ok = Color32::from_rgb(0x1E, 0x7D, 0x52);
    if doc.compiling {
        let label = if fonts_pending { "loading fonts…".to_string() } else { "typesetting…".to_string() };
        return (label, Color32::from_rgb(0x5E, 0x5E, 0x66));
    }
    let errors = doc.diagnostics.iter().filter(|diagnostic| diagnostic.is_error()).count();
    let warnings = doc.diagnostics.len() - errors;
    if errors > 0 {
        (format!("{errors} error{} — {} ms", if errors == 1 { "" } else { "s" }, doc.millis), error)
    } else if warnings > 0 {
        (format!("{warnings} warning{} — {} ms", if warnings == 1 { "" } else { "s" }, doc.millis), warn)
    } else if doc.millis > 0 {
        (format!("{} ms", doc.millis), ok)
    } else {
        ("—".to_string(), Color32::from_rgb(0x5E, 0x5E, 0x66))
    }
}

fn render_threads() -> usize {
    std::thread::available_parallelism().map(|count| count.get()).unwrap_or(4).clamp(1, 6)
}

/// Where a save or export goes: the folder named by `variable` when it is set (a hook for
/// scripted runs, where a native file dialog would block), else a dialog.
fn target_path(document: &Path, variable: &str, extension: &str) -> Option<PathBuf> {
    if let Ok(folder) = std::env::var(variable) {
        let name = document.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "document".to_string());
        return Some(Path::new(&folder).join(format!("{name}.{extension}")));
    }
    let name = document.with_extension(extension).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| format!("document.{extension}"));
    let mut dialog = rfd::FileDialog::new().add_filter(extension.to_uppercase(), &[extension]).set_file_name(name);
    if let Some(folder) = document.parent() {
        dialog = dialog.set_directory(folder);
    }
    let Some(path) = dialog.save_file() else { return None };
    Some(if path.extension().is_some() { path } else { path.with_extension(extension) })
}

fn unique_path(folder: &Path, name: &str, taken: &[PathBuf]) -> PathBuf {
    let free = |path: &Path| !path.exists() && !taken.iter().any(|other| other == path);
    let base = folder.join(name);
    if free(&base) {
        return base;
    }
    let stem = Path::new(name).file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "untitled".to_string());
    for index in 2..1000 {
        let candidate = folder.join(format!("{stem}-{index}.typ"));
        if free(&candidate) {
            return candidate;
        }
    }
    base
}

fn anchor_under(doc: &Doc, screen: Pos2, viewport_top: f32) -> Option<(usize, f32, f32)> {
    let distance = |rect: &Rect| rect.distance_to_pos(screen);
    let (page, rect) = doc.last_pages.iter().min_by(|a, b| distance(&a.1).total_cmp(&distance(&b.1))).copied()?;
    let fraction = ((screen.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
    Some((page, fraction, screen.y - viewport_top))
}

fn current_page(doc: &Doc, viewport: Rect) -> Option<usize> {
    let middle = viewport.center().y;
    doc.last_pages.iter().min_by(|a, b| (a.1.center().y - middle).abs().total_cmp(&(b.1.center().y - middle).abs())).map(|(page, _)| *page)
}

fn page_layout(metas: &[PageMeta], band: f32, zoom: f32) -> PageLayout {
    let widest = metas.iter().map(|page| page.width).fold(1.0_f32, f32::max);
    let per_row = if 2.0 * widest * zoom + SPREAD_GAP <= band { 2 } else { 1 };
    let count = metas.len();
    let mut pages = Vec::with_capacity(count);
    let mut y = SCROLL_MARGIN;
    for start in (0..count).step_by(per_row) {
        let row: Vec<usize> = (start..(start + per_row).min(count)).collect();
        let sizes: Vec<Vec2> = row.iter().map(|page| Vec2::new((metas[*page].width * zoom).max(1.0), (metas[*page].height * zoom).max(1.0))).collect();
        let row_w: f32 = sizes.iter().map(|size| size.x).sum::<f32>() + SPREAD_GAP * (sizes.len() as f32 - 1.0);
        let row_h = sizes.iter().map(|size| size.y).fold(0.0_f32, f32::max);
        let row_top = y;
        let mut x = ((band - row_w) / 2.0).max(0.0);
        let row_rect = Rect::from_min_max(Pos2::new(0.0, row_top), Pos2::new(band, row_top + row_h));
        for (page, size) in row.into_iter().zip(sizes) {
            let rect = Rect::from_min_size(Pos2::new(x, row_top + (row_h - size.y) / 2.0), size);
            pages.push(Placement { page, row: row_rect, rect });
            x += size.x + SPREAD_GAP;
        }
        y += row_h + PAGE_GAP;
    }
    PageLayout { pages, content_h: (y - PAGE_GAP + SCROLL_MARGIN).max(0.0), two_up: per_row == 2 }
}

/// The text run nearest an unzoomed page point, for the hover mark.
fn nearest_hit(meta: &PageMeta, x: f32, y: f32) -> Option<engine::Hit> {
    let mut best: Option<(f32, engine::Hit)> = None;
    for hit in &meta.hits {
        let dx = (hit.x0 - x).max(x - hit.x1).max(0.0);
        let dy = (hit.y0 - y).max(y - hit.y1).max(0.0);
        let distance = dx * dx + dy * dy;
        if best.is_none_or(|(best, _)| distance < best) {
            best = Some((distance, *hit));
        }
    }
    best.filter(|(distance, _)| *distance < 900.0).map(|(_, hit)| hit)
}
