//! The document model both front-ends share: settings and the session, the theme tokens'
//! meaning, diagnostic → editor-mark conversion, and the little helpers (paths, labels) that
//! have nothing to do with a window toolkit.
//!
//! Theme *colours* stay in the front-ends (egui needs `Color32`, iced needs `Color`), but the
//! palette itself — names and values — is here, so the two ports cannot drift apart.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::engine::Diagnostic;

/// How long after the last keystroke the document is typeset again. The page view is meant to
/// settle once you stop typing, not to follow every word.
pub const DEBOUNCE: f64 = 0.6;

/// The starter document a new tab gets.
pub const WELCOME: &str = r#"#set page(paper: "a4", margin: (x: 2.4cm, y: 2.5cm))
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

/// Which panes the band of the window shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
pub enum View {
    Source,
    #[default]
    Split,
    Preview,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Source => "Source",
            View::Split => "Split",
            View::Preview => "Preview",
        }
    }
}

/// Which side panel is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RightPanel {
    Problems,
    Document,
    Log,
}

impl RightPanel {
    pub const ALL: [RightPanel; 3] = [RightPanel::Problems, RightPanel::Document, RightPanel::Log];

    /// The panel header's title, and the rail button's tooltip.
    pub fn label(self) -> &'static str {
        match self {
            RightPanel::Problems => "Problems",
            RightPanel::Document => "Document",
            RightPanel::Log => "Log",
        }
    }

    /// The Lucide icon name the UI asks `icons` for.
    pub fn icon(self) -> &'static str {
        match self {
            RightPanel::Problems => "triangle-alert",
            RightPanel::Document => "file-text",
            RightPanel::Log => "square-terminal",
        }
    }

    /// Whether `doc` has something worth showing in this panel (drives the rail dot).
    pub fn present(self, diagnostics: usize, has_pages: bool, log_lines: usize) -> bool {
        match self {
            RightPanel::Problems => diagnostics > 0,
            RightPanel::Document => has_pages,
            RightPanel::Log => log_lines > 0,
        }
    }
}

/// The light/dark choice actually in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ThemeKind {
    #[default]
    Light,
    Dark,
}

impl ThemeKind {
    pub fn dark(self) -> bool {
        self == ThemeKind::Dark
    }
}

/// The saved choice, independent of the colours currently displayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ThemePreference {
    System,
    #[default]
    Light,
    Dark,
}

impl ThemePreference {
    /// Resolve the preference against what the desktop says (`Some(true)` for a dark system
    /// theme), falling back to `fallback` when the desktop has no opinion.
    pub fn resolve(self, system_dark: Option<bool>, fallback: ThemeKind) -> ThemeKind {
        match self {
            Self::Light => ThemeKind::Light,
            Self::Dark => ThemeKind::Dark,
            Self::System => match system_dark {
                Some(true) => ThemeKind::Dark,
                Some(false) => ThemeKind::Light,
                None => fallback,
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Use system setting",
            Self::Light => "Light gray",
            Self::Dark => "Dark gray",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::System => "settings",
            Self::Light => "sun",
            Self::Dark => "moon",
        }
    }
}

/// A document to reopen on the next launch, and the page the editor was on.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionDoc {
    pub path: PathBuf,
    pub page: usize,
}

/// Everything the window remembers between launches.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub zoom: f32,
    pub theme: ThemePreference,
    pub view: View,
    /// Modal (Vim) editing in the source pane.
    pub vim: bool,
    /// Keep the pages fitted to the band as it changes (until a zoom is chosen).
    #[serde(default = "default_fit")]
    pub fit_width: bool,
    /// The source pane's share of the band in Split view (0.2..=0.8).
    pub split: f32,
    /// The width of the Problems/Document/Log panel, in points.
    pub side_width: f32,
    pub session: Vec<SessionDoc>,
    pub session_active: usize,
}

fn default_fit() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self { zoom: 1.0, theme: ThemePreference::Light, view: View::Split, vim: true, fit_width: true, split: 0.44, side_width: 312.0, session: Vec::new(), session_active: 0 }
    }
}

/// Where the settings live between launches (the egui build used eframe's storage; the iced
/// one has no storage of its own, so both now use this file).
pub fn settings_path() -> PathBuf {
    if let Ok(path) = std::env::var("PDF_EDITOR_SETTINGS") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))).unwrap_or_else(|| PathBuf::from("."));
    base.join("pdf-editor").join("settings.json")
}

pub fn load_settings() -> Settings {
    std::fs::read_to_string(settings_path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn save_settings(settings: &Settings) {
    let path = settings_path();
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    if let Ok(text) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, text);
    }
}

/// A diagnostic's place in the buffer: a line, and the run of characters on it to underline.
///
/// This is the egui editor's `Mark`; the iced front-end feeds byte ranges straight into
/// `Content::set_diagnostics` instead, but the review and the tests are shared.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    /// 0-based line.
    pub line: usize,
    /// 0-based column in characters from the line's start.
    pub column: usize,
    /// How many characters the diagnostic covers (at least one).
    pub len: usize,
    pub error: bool,
}

/// Turn the compile's diagnostics into marks for the editor: which line, which column, how many
/// characters to underline. Ranges are byte offsets into `text`; anything that points into an
/// included file is left out (there is no line in this buffer for it).
pub fn marks_for(text: &str, diagnostics: &[Diagnostic]) -> Vec<Mark> {
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(index, _)| index + 1)).collect();
    let mut marks = Vec::new();
    for diagnostic in diagnostics {
        if diagnostic.file.is_some() {
            continue;
        }
        let (line, column, len) = match &diagnostic.range {
            Some(range) => {
                let start = range.start.min(text.len());
                let end = range.end.clamp(start, text.len());
                let line = starts.partition_point(|&offset| offset <= start).saturating_sub(1);
                let line_begin = starts.get(line).copied().unwrap_or(0);
                let line_end = text[line_begin..].find('\n').map_or(text.len(), |offset| line_begin + offset);
                let column = text[line_begin..start].chars().count();
                let covered = text[start..end.min(line_end)].chars().count().max(1);
                (line, column, covered)
            }
            // A diagnostic without a span (an included file's error, a detached one) still has a
            // line and column: mark the single character there.
            None => (diagnostic.line, diagnostic.column, 1),
        };
        marks.push(Mark { line, column, len, error: diagnostic.is_error() });
    }
    marks
}

/// The diagnostics that point into this buffer, as byte ranges (what an iced editor wants).
pub fn diagnostic_ranges(text: &str, diagnostics: &[Diagnostic]) -> Vec<(Range<usize>, bool, String)> {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.file.is_none())
        .map(|diagnostic| {
            let range = diagnostic.range.clone().unwrap_or_else(|| {
                let byte = byte_at(text, diagnostic.line, diagnostic.column);
                byte..byte
            });
            let start = range.start.min(text.len());
            let end = range.end.clamp(start, text.len());
            (start..end, diagnostic.is_error(), diagnostic.message.clone())
        })
        .collect()
}

/// Byte offset of a 0-based line/column pair.
pub fn byte_at(text: &str, line: usize, column: usize) -> usize {
    let start = line_start(text, line);
    let end = text[start..].find('\n').map_or(text.len(), |offset| start + offset);
    let mut byte = start;
    for _ in 0..column {
        if byte >= end {
            break;
        }
        byte += text[byte..].chars().next().map_or(0, char::len_utf8);
    }
    byte
}

/// Byte offset of the start of a 0-based line, clamped to the text's end.
pub fn line_start(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    text.match_indices('\n').nth(line - 1).map_or(text.len(), |(index, _)| index + 1)
}

/// Byte offset of the start of the line a byte sits on.
pub fn line_at_byte(text: &str, byte: usize) -> usize {
    line_start(text, text[..byte.min(text.len())].matches('\n').count())
}

/// The column (in characters) of a byte offset from its line's start.
pub fn column_at_byte(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text[line_at_byte(text, byte)..byte].chars().count()
}

/// Shorten a label to `max` characters, marking the cut with an ellipsis.
pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// Where a save or export goes: the folder named by `variable` when it is set (a hook for
/// scripted runs, where a native file dialog would block), else a dialog.
pub fn target_path(document: &Path, variable: &str, extension: &str) -> Option<PathBuf> {
    if let Ok(folder) = std::env::var(variable) {
        let name = document.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "document".to_string());
        return Some(Path::new(&folder).join(format!("{name}.{extension}")));
    }
    let name = document.with_extension(extension).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| format!("document.{extension}"));
    let mut dialog = rfd::FileDialog::new().add_filter(extension.to_uppercase(), &[extension]).set_file_name(name);
    if let Some(folder) = document.parent() {
        dialog = dialog.set_directory(folder);
    }
    let path = dialog.save_file()?;
    Some(if path.extension().is_some() { path } else { path.with_extension(extension) })
}

/// A name for a new document that no open tab and no file on disk is using yet.
pub fn unique_path(folder: &Path, name: &str, taken: &[PathBuf]) -> PathBuf {
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

/// The files named on the command line, in order.
pub fn named_documents() -> Vec<PathBuf> {
    std::env::args().skip(1).map(PathBuf::from).filter(|path| path.is_file()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use typst::diag::Severity;

    fn diagnostic(severity: Severity, range: Option<Range<usize>>, line: usize, column: usize) -> Diagnostic {
        Diagnostic { severity, message: String::new(), hints: Vec::new(), line, column, file: None, range }
    }

    #[test]
    fn shorten_marks_the_cut() {
        assert_eq!(shorten("demo.typ", 20), "demo.typ");
        assert_eq!(shorten("a-very-long-document-name.typ", 10), "a-very-lo…");
    }

    #[test]
    fn marks_carry_line_column_and_length() {
        let text = "= Title\n\nSome #bad() here\n";
        let bad = text.find("#bad").unwrap();
        let marks = marks_for(text, &[diagnostic(Severity::Error, Some(bad..bad + 4), 2, 5)]);
        assert_eq!(marks.len(), 1);
        assert_eq!((marks[0].line, marks[0].column, marks[0].len), (2, 5, 4));
        assert!(marks[0].error);
    }

    #[test]
    fn a_warning_is_marked_as_one_and_a_span_across_lines_is_clamped() {
        let text = "first line\nsecond\n";
        let start = text.find("line").unwrap();
        // Ran to the end of the buffer: the mark stops at this line's newline.
        let marks = marks_for(text, &[diagnostic(Severity::Warning, Some(start..text.len()), 0, 6)]);
        assert_eq!((marks[0].line, marks[0].column, marks[0].len), (0, 6, 4));
        assert!(!marks[0].error);
    }

    #[test]
    fn a_diagnostic_in_an_included_file_is_left_out() {
        let text = "= Title\n";
        let mut other = diagnostic(Severity::Error, Some(0..2), 0, 0);
        other.file = Some("chapter.typ".to_string());
        assert!(marks_for(text, &[other.clone()]).is_empty());
        assert!(diagnostic_ranges(text, &[other]).is_empty());
    }

    #[test]
    fn byte_ranges_stay_inside_this_buffer() {
        let text = "one\ntwo\n";
        let ranges = diagnostic_ranges(text, &[diagnostic(Severity::Error, Some(0..99), 0, 0)]);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].0, 0..text.len());
    }

    #[test]
    fn line_and_column_round_trip() {
        let text = "héllo\nwörld\n";
        let byte = text.find("wörld").unwrap() + "wö".len();
        assert_eq!(line_at_byte(text, byte), text.find("wörld").unwrap());
        assert_eq!(column_at_byte(text, byte), 2);
        assert_eq!(byte_at(text, 1, 2), byte);
    }

    #[test]
    fn the_system_preference_needs_the_desktop_to_agree() {
        assert_eq!(ThemePreference::System.resolve(Some(true), ThemeKind::Light), ThemeKind::Dark);
        assert_eq!(ThemePreference::System.resolve(None, ThemeKind::Light), ThemeKind::Light);
        assert_eq!(ThemePreference::Dark.resolve(Some(false), ThemeKind::Light), ThemeKind::Dark);
    }
}
