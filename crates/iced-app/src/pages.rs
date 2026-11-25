//! The page view's geometry: where each page sits in the scrolled band, and how much room the
//! whole document needs. The maths is the egui build's, unchanged — a two-page spread when the
//! band is wide enough for it, a page gap down the column, room for the drop shadow at both
//! ends — but it produces plain `f32`s instead of `egui::Rect`s.

use pdf_editor_core::engine::PageMeta;

pub const PAGE_GAP: f32 = 20.0;
pub const SPREAD_GAP: f32 = 14.0;
/// Space above the first page and below the last, for the drop shadow.
pub const SCROLL_MARGIN: f32 = 22.0;
/// A spread is only used when it fits at least this much of the requested zoom.
pub const FIT_SPREAD_MIN: f32 = 0.8;

/// One page's place in the band, in content coordinates (the scroll view's, not the window's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub page: usize,
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub height: f32,
}

impl Placement {
    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub pages: Vec<Placement>,
    /// Whether the band is wide enough to show pages side by side.
    pub two_up: bool,
}

impl Layout {
    pub fn get(&self, page: usize) -> Option<&Placement> {
        self.pages.iter().find(|place| place.page == page)
    }
}

/// Lay the pages out down a band `band` points wide, at `zoom` screen points per page point.
pub fn layout(metas: &[PageMeta], band: f32, zoom: f32) -> Layout {
    let widest = metas.iter().map(|page| page.width).fold(1.0_f32, f32::max);
    let per_row = if 2.0 * widest * zoom + SPREAD_GAP <= band { 2 } else { 1 };
    let mut pages = Vec::with_capacity(metas.len());
    let mut y = SCROLL_MARGIN;
    for start in (0..metas.len()).step_by(per_row) {
        let row: Vec<usize> = (start..(start + per_row).min(metas.len())).collect();
        let sizes: Vec<(f32, f32)> = row.iter().map(|page| ((metas[*page].width * zoom).max(1.0), (metas[*page].height * zoom).max(1.0))).collect();
        let row_w: f32 = sizes.iter().map(|(width, _)| *width).sum::<f32>() + SPREAD_GAP * (sizes.len() as f32 - 1.0);
        let row_h = sizes.iter().map(|(_, height)| *height).fold(0.0_f32, f32::max);
        let mut x = ((band - row_w) / 2.0).max(0.0);
        for (page, (width, height)) in row.into_iter().zip(sizes) {
            pages.push(Placement { page, top: y + (row_h - height) / 2.0, left: x, width, height });
            x += width + SPREAD_GAP;
        }
        y += row_h + PAGE_GAP;
    }
    Layout { two_up: per_row == 2, pages }
}

/// The page a click at page-local points lands on, as a source `(line, column)`.
///
/// The engine's glyph index is the authority: it maps the click to the byte the glyph was set
/// from, so a click on a word lands on the column of that word and not merely on its line.
pub fn position_at(metas: &[PageMeta], page: usize, local: (f32, f32), text: &str) -> Option<(usize, usize)> {
    pdf_editor_core::engine::position_at(metas, page, local.0, local.1, text)
}

/// The run nearest a point, for the hover highlight.
pub fn nearest_hit(meta: &PageMeta, x: f32, y: f32) -> Option<pdf_editor_core::engine::Hit> {
    let mut best: Option<(f32, pdf_editor_core::engine::Hit)> = None;
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

/// The page the user is looking at: the one whose middle is nearest the viewport's middle.
pub fn current_page(layout: &Layout, scroll: f32, viewport_h: f32) -> Option<usize> {
    let middle = scroll + viewport_h / 2.0;
    layout.pages.iter().min_by(|a, b| (a.top + a.height / 2.0 - middle).abs().total_cmp(&(b.top + b.height / 2.0 - middle).abs())).map(|place| place.page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(width: f32, height: f32) -> PageMeta {
        PageMeta { width, height, hits: Vec::new(), glyphs: Vec::new() }
    }

    #[test]
    fn a_narrow_band_stacks_pages_and_a_wide_one_spreads_them() {
        let metas = vec![page(595.0, 842.0), page(595.0, 842.0), page(595.0, 842.0)];
        let narrow = layout(&metas, 700.0, 1.0);
        assert!(!narrow.two_up);
        assert_eq!(narrow.pages.len(), 3);
        assert!(narrow.pages[1].top > narrow.pages[0].top + narrow.pages[0].height);

        let wide = layout(&metas, 1400.0, 1.0);
        assert!(wide.two_up);
        assert_eq!(wide.pages[0].top, wide.pages[1].top, "a spread shares a top edge");
        assert!(wide.pages[1].left > wide.pages[0].left + wide.pages[0].width);
        // The third page starts a new row.
        assert!(wide.pages[2].top > wide.pages[0].top + wide.pages[0].height);
    }

    #[test]
    fn the_first_page_starts_below_the_shadow_margin() {
        let metas = vec![page(595.0, 842.0)];
        let laid = layout(&metas, 700.0, 0.5);
        assert_eq!(laid.pages[0].top, SCROLL_MARGIN);
        assert!((laid.pages[0].height - 421.0).abs() < 0.01);
    }

    #[test]
    fn an_empty_document_lays_out_nothing() {
        let laid = layout(&[], 700.0, 1.0);
        assert!(laid.pages.is_empty());
    }

    #[test]
    fn the_current_page_is_the_one_nearest_the_middle_of_the_view() {
        let metas = vec![page(595.0, 800.0), page(595.0, 800.0)];
        let laid = layout(&metas, 700.0, 1.0);
        assert_eq!(current_page(&laid, 0.0, 400.0), Some(0));
        let second_top = laid.pages[1].top;
        assert_eq!(current_page(&laid, second_top, 400.0), Some(1));
    }
}
