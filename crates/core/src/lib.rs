//! The toolkit-neutral heart of PDF Editor.
//!
//! Everything here is independent of the window layer: the Typst engine (compile thread,
//! raster pool, click-to-source index), the modal editing state machine, the document model
//! (settings, session, diagnostics) and the `Source` abstraction both front-ends implement.
//!
//! The egui binary (`pdf-editor`) and the iced binary (`pdf-editor-iced`) are both thin
//! shells over this crate. `vim.rs` is shared byte-for-byte: it speaks byte offsets into a
//! plain `String`, which either front-end can mirror from its own editor widget.

pub mod document;
pub mod engine;
pub mod source;
pub mod vim;
