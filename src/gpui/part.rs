//! A column of a page (Browse's results, the Library's mod table) as a
//! view of its own, drawn from the page's state and redrawn whenever the
//! page changes.
//!
//! GPUI redraws everything inside a view that redraws, and a redraw of a
//! child (a GIF moving on in a description, a logo's next frame) redraws
//! every view around it. So a page whose child animates is left uncached,
//! and the columns that don't animate are cached parts: a redraw that
//! leaves the page alone reuses them instead of building every row again.

use gpui_kit::component::h_flex;
use gpui_kit::*;

/// Draws a part from its page `V`; the id is the part's own view, under
/// which what it draws is laid out (see `logos::Slots::clear`).
pub type Draw<V> = fn(&mut V, EntityId, &mut Window, &mut Context<V>) -> AnyElement;

pub struct Part<V: 'static> {
    page: WeakEntity<V>,
    draw: Draw<V>,
}

impl<V: 'static> Part<V> {
    /// The part, redrawn whenever `page` changes.
    pub fn new(page: &Entity<V>, draw: Draw<V>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(page, |_, _, cx| cx.notify()).detach();
            Part { page: page.downgrade(), draw }
        })
    }
}

impl<V: 'static> Render for Part<V> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (draw, me) = (self.draw, cx.entity_id());
        let part = self
            .page
            .update(cx, |page, cx| draw(page, me, window, cx))
            .unwrap_or_else(|_| div().into_any_element());
        // A cached view is laid out on its own: this row gives the column
        // the flex parent it sizes itself by. The cache style carries the
        // column's own size (`AnyView::cached`).
        h_flex().size_full().child(part)
    }
}
