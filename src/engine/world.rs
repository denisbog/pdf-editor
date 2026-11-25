//! The environment a Typst compilation runs in: the editor's buffer as the main source, the
//! files it includes or imports read from the document's folder, and the fonts the machine
//! has (plus Typst's embedded ones).
//!
//! Only what is needed for editing a local document is implemented: there is no package
//! downloader, so `@preview/…` imports resolve from `~/.cache/typst/packages` (or fail with
//! the usual "package not found" diagnostic). Files are always resolved below the document's
//! folder, which is also how `typst compile` treats a project root.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::{Library, LibraryExt};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::World;

use typst_kit::fonts::FontStore;

/// The standard library is built once per process: it is immutable and expensive to set up.
fn library() -> &'static LazyHash<Library> {
    static LIBRARY: OnceLock<LazyHash<Library>> = OnceLock::new();
    LIBRARY.get_or_init(|| LazyHash::new(Library::builder().build()))
}

/// Loads the fonts once per process (embedded faces plus everything installed on the machine)
/// and shares them between compilations. The first call scans the system font directories.
pub fn fonts() -> Arc<FontStore> {
    static FONTS: OnceLock<Arc<FontStore>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut store = FontStore::new();
            store.extend(typst_kit::fonts::embedded());
            store.extend(typst_kit::fonts::system());
            Arc::new(store)
        })
        .clone()
}
/// Whether the font scan has happened already (the UI says "loading fonts…" until it has).
pub fn fonts_ready() -> bool {
    FONTS_READY.load(Ordering::Relaxed)
}

pub fn mark_fonts_ready() {
    FONTS_READY.store(true, Ordering::Relaxed);
}

static FONTS_READY: AtomicBool = AtomicBool::new(false);

/// Grab the fonts ahead of the first compile, off the UI thread, so the first render is not
/// delayed by the system font scan.
pub fn warm_fonts(ctx: Option<egui::Context>) {
    std::thread::spawn(move || {
        let _ = fonts();
        mark_fonts_ready();
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
}
/// The environment of one compilation.
pub struct TypstWorld {
    /// The document's folder: every virtual path is relative to it.
    root: PathBuf,
    /// The main source file (the editor's buffer, whatever is on disk).
    main: FileId,
    source: Source,
    fonts: Arc<FontStore>,
    /// Included and imported files, read once per compilation.
    files: Mutex<HashMap<FileId, FileResult<Bytes>>>,
}

impl TypstWorld {
    /// Build a world for `main` with the given (in-memory) source text.
    pub fn new(main: &Path, text: String, fonts: Arc<FontStore>) -> Self {
        let root = main.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        // The main file is named after the real file, but relative to the document's folder,
        // so an `#include "chapter.typ"` resolves the way `typst compile` resolves it.
        let name = main.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "main.typ".to_string());
        let vpath = VirtualPath::new(format!("/{name}")).expect("a file name is a valid virtual path");
        let id = FileId::new(RootedPath::new(VirtualRoot::Project, vpath));
        Self { root, main: id, source: Source::new(id, text), fonts, files: Mutex::new(HashMap::new()) }
    }

    /// Where a virtual path lives on disk.
    fn disk_path(&self, id: FileId) -> PathBuf {
        let relative = id.vpath().get_without_slash();
        self.root.join(relative)
    }

    fn load(&self, id: FileId) -> FileResult<Bytes> {
        let mut files = self.files.lock().unwrap();
        if let Some(cached) = files.get(&id) {
            return cached.clone();
        }
        let path = self.disk_path(id);
        let result = std::fs::read(&path).map(Bytes::new).map_err(|error| FileError::from_io(error, &path));
        files.insert(id, result.clone());
        result
    }
}

impl World for TypstWorld {
    fn library(&self) -> &LazyHash<Library> {
        library()
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main {
            return Ok(self.source.clone());
        }
        let bytes = self.load(id)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| FileError::InvalidUtf8)?;
        Ok(Source::new(id, text.to_owned()))
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main {
            return Ok(Bytes::new(self.source.text().to_owned()));
        }
        self.load(id)
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        // `None` asks for the local date, a duration for a fixed UTC offset. Typst only wants
        // calendar fields, so build the datetime from them.
        let offset_seconds = offset.map(|duration| duration.seconds() as i64).unwrap_or(0);
        let moment = if offset.is_some() {
            time::OffsetDateTime::now_utc() + time::Duration::seconds(offset_seconds)
        } else {
            time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        };
        Datetime::from_ymd_hms(moment.year(), moment.month() as u8, moment.day(), moment.hour(), moment.minute(), moment.second())
    }
}
