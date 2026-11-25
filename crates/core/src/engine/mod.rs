//! The Typst side of the editor: compiling the buffer (in the background) and rasterising the
//! compiled pages for the screen (also in the background).
//!
//! `world` builds the environment a compilation runs in, `compile` owns the compile/export
//! thread and the text index used for click-to-source, and this module adds the raster pool
//! that turns a `PagedDocument` into page textures at whatever zoom the view asks for.

mod compile;
mod wake;
mod world;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Condvar, Mutex};

pub use compile::{compile, line_at, position_at, Compiler, Diagnostic, Event, Glyph, Hit, Outcome, PageMeta};
pub use wake::Waker;
pub use world::{fonts, fonts_ready, warm_fonts};

use typst_layout::PagedDocument;
use typst::utils::Scalar;

/// A raster request: one page, at `scale` device pixels per point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Task {
    pub page: usize,
    pub scale: f32,
    pub tag: u64,
}

/// A finished raster.
pub struct RasterDone {
    pub gen: u64,
    pub page: usize,
    pub scale: f32,
    pub tag: u64,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub error: Option<String>,
}

struct State {
    document: Option<Arc<PagedDocument>>,
    gen: u64,
    /// What the view still needs, most urgent first.
    queue: Vec<Task>,
    running: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// Renders the pages the view asks for on a few threads, so a page scrolled to appears without
/// waiting for the ones that were already in flight.
pub struct RasterPool {
    shared: Arc<Shared>,
    done: Receiver<RasterDone>,
    next_tag: Arc<AtomicU64>,
}

/// The highest raster scale is capped: a page at 600% would otherwise be a hundred megabytes
/// of texture, and the page on screen is stretched from the nearest raster anyway.
const MAX_SCALE: f32 = 3.0;
/// Above this many device pixels the scale is reduced until the page fits.
const MAX_PIXELS: f32 = 24_000_000.0;

impl RasterPool {
    pub fn new(waker: Waker, threads: usize) -> Self {
        let shared = Arc::new(Shared { state: Mutex::new(State { document: None, gen: 0, queue: Vec::new(), running: true }), wake: Condvar::new() });
        let (tx, done) = channel::<RasterDone>();
        for index in 0..threads.max(1) {
            let shared = shared.clone();
            let tx: Sender<RasterDone> = tx.clone();
            let waker = waker.clone();
            std::thread::Builder::new()
                .name(format!("typst-raster-{index}"))
                .spawn(move || raster_thread(shared, tx, waker))
                .expect("a raster thread starts");
        }
        Self { shared, done, next_tag: Arc::new(AtomicU64::new(1)) }
    }

    pub fn tag(&self) -> u64 {
        self.next_tag.fetch_add(1, Ordering::Relaxed)
    }

    /// Hand the pool the document to render from.
    pub fn set_document(&self, gen: u64, document: Arc<PagedDocument>) {
        let mut state = self.shared.state.lock().unwrap();
        state.document = Some(document);
        state.gen = gen;
        state.queue.clear();
        self.shared.wake.notify_all();
    }

    /// Replace what still needs rendering for `gen` with `tasks`.
    pub fn set_queue(&self, gen: u64, tasks: Vec<Task>) {
        let mut state = self.shared.state.lock().unwrap();
        if state.gen != gen {
            return;
        }
        state.queue = tasks;
        self.shared.wake.notify_all();
    }

    pub fn poll(&self) -> Vec<RasterDone> {
        let mut done = Vec::new();
        loop {
            match self.done.try_recv() {
                Ok(raster) => done.push(raster),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        done
    }
}

fn raster_thread(shared: Arc<Shared>, done: Sender<RasterDone>, waker: Waker) {
    loop {
        let (gen, document, task) = {
            let mut state = shared.state.lock().unwrap();
            while state.queue.is_empty() {
                if !state.running {
                    return;
                }
                state = shared.wake.wait(state).unwrap();
            }
            let task = state.queue.remove(0);
            let Some(document) = state.document.clone() else { continue };
            (state.gen, document, task)
        };
        let raster = render_page(&document, gen, task);
        let _ = done.send(raster);
        waker.wake();
    }
}

fn render_page(document: &PagedDocument, gen: u64, task: Task) -> RasterDone {
    let Some(page) = document.pages().get(task.page) else {
        return RasterDone { gen, page: task.page, scale: task.scale, tag: task.tag, width: 0, height: 0, rgba: Vec::new(), error: Some("the page no longer exists".to_string()) };
    };
    let size = page.frame.size();
    let (width_pt, height_pt) = (size.x.to_pt() as f32, size.y.to_pt() as f32);
    let mut scale = task.scale.clamp(0.05, MAX_SCALE);
    let pixels = width_pt * height_pt * scale * scale;
    if pixels > MAX_PIXELS {
        scale *= (MAX_PIXELS / pixels).sqrt();
    }
    let options = typst_render::RenderOptions { pixel_per_pt: Scalar::new(scale as f64), render_bleed: false };
    let pixmap = typst_render::render(page, &options);
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    RasterDone { gen, page: task.page, scale: task.scale, tag: task.tag, width, height, rgba: pixmap.data().to_vec(), error: None }
}
