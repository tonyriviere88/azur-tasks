//! A virtualized, sortable, resizable table that paints straight to the draw
//! list.
//!
//! The design constraint is that a Details view can hold 600 processes across
//! 15 columns. Instantiating an egui widget per cell would mean 9000 `Id`
//! allocations, 9000 layout passes and 9000 interaction checks per frame. So
//! instead:
//!
//! * only the rows intersecting the viewport are touched at all (virtualization);
//! * cells are `painter.galley` calls, not widgets;
//! * the entire body is a *single* interactive rect, and the hovered/clicked row
//!   is derived arithmetically from the pointer's `y`.
//!
//! That turns per-frame cost from O(rows x columns) into O(visible rows), and
//! hit-testing from O(rows) into O(1).

use super::theme::Theme;
use azur_egui_theme::components::{
    galley_baseline, galley_on_baseline, ink_baseline, row_baseline, Checkbox, ContextMenu,
};
use azur_egui_theme::desktop;
use azur_egui_theme::tokens::{row, space, typography};
use egui::{
    Color32, CornerRadius, FontId, Id, Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2,
};

/// The heading strip: one dense row, the same height as the rows under it.
///
/// Azur's `th` is a body line with `space-3` either side, which is 36px -- the form
/// row, and the one [`row::DENSE`] is already the argument against. A heading is
/// not read by scanning, but it is *aligned to* by everything that is, and a strip
/// half again as tall as its rows reads as a second toolbar. What separates it from
/// the body is what Azur says separates them: `background-layer-alt` and the rule
/// under it, not height.
pub const HEADER_H: f32 = row::DENSE;

/// How wide a tooltip is allowed to get.
///
/// Shared with the styling in [`super::theme`] so every tooltip in the app wraps
/// at the same place, whether it is a sentence about a control or a row spelled
/// out field by field.
pub const TOOLTIP_W: f32 = 400.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// `Copy` so the header pass can snapshot a column and then mutate sort state
/// without holding a borrow of `self.cols` across the loop.
#[derive(Clone, Copy)]
pub struct Column {
    /// Stable identifier, used as the sort key and for persistence.
    pub id: u16,
    pub title: &'static str,
    pub width: f32,
    pub min_width: f32,
    pub align: Align,
    pub visible: bool,
    /// Takes any width left over once the other columns have had theirs, so the
    /// grid always reaches the right edge. At most one column should set it.
    pub stretch: bool,
    /// Shown on the header tooltip; several Task Manager columns need one.
    pub help: &'static str,
}

impl Column {
    pub fn new(id: u16, title: &'static str, width: f32, align: Align) -> Self {
        Self {
            id,
            title,
            width,
            min_width: 44.0,
            align,
            visible: true,
            stretch: false,
            help: "",
        }
    }

    pub fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
    }

    pub fn stretch(mut self) -> Self {
        self.stretch = true;
        self
    }

    pub fn min(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }
}

/// Persistent table state: column layout and sort order.
pub struct Table {
    pub cols: Vec<Column>,
    /// Column `id` currently sorted by.
    pub sort_by: u16,
    pub sort_desc: bool,
    pub row_h: f32,
    /// Index of the column whose right edge is being dragged.
    dragging_edge: Option<usize>,
    /// Set when a row should be scrolled into view on the next pass.
    pub scroll_to_row: Option<usize>,
    /// How far the grid is scrolled to the right, in points.
    ///
    /// Kept here rather than left to live inside the body's `ScrollArea`, because
    /// the header is drawn *before* the body and outside that area: it has to know
    /// where the columns are going to be. The body is handed this value and hands
    /// back what it became, so the two halves of the table can never be a frame
    /// apart.
    x_offset: f32,
}

/// What the caller needs to know after a pass.
pub struct TableOutput {
    /// Row left-clicked this frame.
    pub clicked_row: Option<usize>,
    pub double_clicked_row: Option<usize>,
    /// Row right-clicked this frame; the caller opens its own context menu.
    pub context_row: Option<usize>,
    /// The body response, for callers that need extra interaction.
    pub response: Response,
}

impl Table {
    pub fn new(cols: Vec<Column>, sort_by: u16, sort_desc: bool) -> Self {
        Self {
            cols,
            sort_by,
            sort_desc,
            // The listing row: 24px, and the rows touch. A process table is read by
            // scanning hundreds of lines, where every point of row height is a line
            // that did not fit -- which is the whole of rule 4 in `azur::desktop`.
            row_h: row::DENSE,
            dragging_edge: None,
            scroll_to_row: None,
            x_offset: 0.0,
        }
    }

    pub fn visible_cols(&self) -> impl Iterator<Item = (usize, &Column)> {
        self.cols.iter().enumerate().filter(|(_, c)| c.visible)
    }

    /// Slot of the visible column that absorbs leftover width, if any.
    fn stretch_slot(&self) -> Option<usize> {
        self.visible_cols().position(|(_, c)| c.stretch)
    }

    /// Narrowest width that still shows every visible column.
    ///
    /// Each column at the width it declares, except the one that absorbs slack,
    /// counted at *its* minimum -- because that is as far as [`Self::layout`]
    /// squeezes it before the grid has to overflow. Below this, the columns past
    /// the right edge can only be reached by scrolling.
    fn min_grid_width(&self) -> f32 {
        let total: f32 = self.cols.iter().filter(|c| c.visible).map(|c| c.width).sum();
        // The same column `layout` gives the slack to: the one marked `stretch`,
        // or the last one when none is.
        match self
            .visible_cols()
            .find(|(_, c)| c.stretch)
            .or_else(|| self.visible_cols().last())
        {
            Some((_, c)) => total - c.width + c.min_width,
            None => 0.0,
        }
    }

    /// The grid's span, given the width `view` the reader can see.
    ///
    /// At least as wide as the view -- the columns always reach the right edge --
    /// and starting left of it by however far the grid is scrolled. Both halves of
    /// the table go through here, so a column's rectangle is the same number in the
    /// header and in the body of the same frame, which is the whole of what keeps a
    /// heading over its column while the grid moves.
    fn grid_rect(&self, view: Rect) -> Rect {
        Rect::from_min_size(
            Pos2::new(view.left() - self.x_offset, view.top()),
            Vec2::new(self.min_grid_width().max(view.width()), view.height()),
        )
    }

    /// Compute each visible column's x-range for a given content width.
    ///
    /// The difference between the columns' declared widths and the space
    /// available goes to the column marked `stretch`, or to the last one. The
    /// difference is *signed*: when the columns declare more than there is, that
    /// column gives width back rather than pushing the rest off the right edge,
    /// which is what used to crop the final header.
    fn layout(&self, content: Rect, out: &mut Vec<Rect>) {
        out.clear();
        let total: f32 = self.cols.iter().filter(|c| c.visible).map(|c| c.width).sum();
        let slack = content.width() - total;
        let n = self.cols.iter().filter(|c| c.visible).count();
        let stretch = self.stretch_slot();

        let mut x = content.left();
        for (i, (_, c)) in self.visible_cols().enumerate() {
            let mut w = c.width;
            if Some(i) == stretch || (stretch.is_none() && i + 1 == n) {
                // Never below the column's own minimum: past that the grid has
                // to overflow, and clipping one column beats squeezing every
                // cell into illegibility.
                w = (w + slack).max(c.min_width);
            }
            out.push(Rect::from_min_size(
                Pos2::new(x, content.top()),
                Vec2::new(w, content.height()),
            ));
            x += w;
        }
    }

    /// Width the body's scroll area takes for its bar.
    ///
    /// The header sits outside the scroll area, so without subtracting this the
    /// two lay out over spans that differ by the bar's width and every column
    /// after the stretch column is offset against its own heading. The bar is
    /// always visible (see `body`) precisely so this is always the right answer.
    fn bar_width(ui: &Ui) -> f32 {
        ui.spacing().scroll.allocated_width()
    }

    /// Draw the header strip. Returns true when the sort changed.
    pub fn header(&mut self, ui: &mut Ui, t: &Theme, id_base: Id) -> bool {
        self.header_with(ui, t, id_base, &[])
    }

    /// Draw the header strip with a live reading beside some of the titles.
    ///
    /// `values` is looked up by column id, so a caller can label the CPU column
    /// with the machine's total utilization. The reading goes immediately left of
    /// its title, at the title's own size in a quieter weight: same line, same
    /// height, one row of headings.
    pub fn header_with(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        id_base: Id,
        values: &[(u16, &str)],
    ) -> bool {
        let full = ui.available_rect_before_wrap();
        let rect = Rect::from_min_size(full.min, Vec2::new(full.width(), HEADER_H));
        ui.allocate_rect(rect, Sense::hover());

        let painter = ui.painter().clone();
        // The pane itself: `background-layer` under everything the table draws, so
        // rows read as sitting on a panel rather than on the canvas. Square and
        // unringed, and no margin -- it is the window's structure, not a card on
        // it, which is what `desktop::panel` says for a panel egui frames.
        painter.rect_filled(full, desktop::structural_radius(), t.bg.layer);
        // The strip spans the full width so it reaches the edge, but the columns
        // are laid out over the body's narrower content span.
        //
        // `background-layer-alt` and a `stroke-default` rule under it: Azur's
        // `thead`, which is a step in from the `background-layer` body so the
        // header reads as attached to the table rather than floating over it.
        painter.rect_filled(rect, desktop::structural_radius(), t.bg.layer_alt);
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, t.stroke.default),
        );

        // What the reader can see of the strip: all of it but the body's scrollbar
        // gutter. The grid is laid out over a span that is at least this wide and
        // begins left of it whenever the table is scrolled, so from here on the
        // columns are painted through a painter clipped back to the view -- without
        // that, a column scrolled off the left would paint on top of the first one
        // still showing, and the last one would run into the scrollbar.
        let view = Rect::from_min_max(
            rect.min,
            Pos2::new(rect.right() - Self::bar_width(ui), rect.bottom()),
        );
        let painter = painter.with_clip_rect(view);
        let mut cells = Vec::with_capacity(self.cols.len());
        self.layout(self.grid_rect(view), &mut cells);
        // The ink baseline, not the line box's: the sort marker is a glyph centred in
        // this strip, and a heading centred by its box would sit a point and a half
        // below it. One baseline for the whole strip, from its principal font, so the
        // titles and the live readings beside them share it whatever size each is.
        let head_baseline = ink_baseline(&painter, &t.fonts.body_strong, rect.top(), HEADER_H);

        let mut sort_changed = false;
        let mut cols_changed = false;
        let mut pending_width: Option<(usize, f32)> = None;

        // Cloned out here because the column menu is opened from inside the loop,
        // where `self.cols` is already borrowed by the closure that draws it.
        let ctx = ui.ctx().clone();

        // Snapshot which columns are visible before the loop: the body mutates
        // `sort_by` / `dragging_edge`, which it cannot do while an iterator over
        // `self.cols` is alive.
        let slots: Vec<usize> = self
            .cols
            .iter()
            .enumerate()
            .filter(|(_, c)| c.visible)
            .map(|(i, _)| i)
            .collect();

        for (slot, col_idx) in slots.iter().copied().enumerate() {
            let col = self.cols[col_idx];
            let cell = cells[slot];
            // Scrolled out of sight. Skipped rather than merely clipped, because
            // this is also where the header's *interaction* is registered: an
            // off-screen resize handle would still answer the pointer, since egui
            // narrows a widget to the `Ui`'s clip rect -- the whole pane -- and not
            // to the view the grid is being shown through.
            if !cell.intersects(view) {
                continue;
            }
            let is_sorted = col.id == self.sort_by;

            // --- click to sort --------------------------------------------
            // Leave the last 6px to the resize handle so the two do not fight.
            let click_rect = Rect::from_min_max(
                cell.min,
                Pos2::new((cell.right() - 6.0).max(cell.left()), cell.bottom()),
            );
            let resp = ui.interact(
                click_rect,
                id_base.with("hdr").with(col.id),
                Sense::click(),
            );
            if resp.hovered() {
                // The window's one hover fill, read back from the token -- the same
                // grey a row, a rail entry and a toolbar button take.
                painter.rect_filled(cell, desktop::structural_radius(), desktop::hover_fill(t));
            }
            if resp.clicked() {
                if is_sorted {
                    self.sort_desc = !self.sort_desc;
                } else {
                    self.sort_by = col.id;
                    // Numbers are most useful largest-first; names A-Z.
                    self.sort_desc = col.align == Align::Right;
                }
                sort_changed = true;
            }
            // Right-click anywhere on the header picks columns. Hung off each
            // header cell rather than one strip-wide response, because the
            // per-column responses already own this area and would swallow the
            // secondary click before a strip-wide one saw it.
            //
            // Azur's `ContextMenu`, not `Response::context_menu`: every other menu
            // in the application is one, and egui's own differs in ways a reader
            // sees side by side -- 180pt minimum width against a menu sized to its
            // longest label, and `item_spacing` between rows where a menu wants its
            // entries stacked flush. `sticky` is what the design system asks for
            // when the entries are settings rather than commands: a picker that
            // dismissed itself on every tick would have to be reopened once per
            // column, which is the Sensors page's show/hide menu behaviour too.
            let menu = ContextMenu::new(&resp).sticky(true).show(&ctx, |ui| {
                if column_picker(ui, &mut self.cols) {
                    cols_changed = true;
                }
            });
            if menu.is_some() {
                // A tooltip on top of an open menu is just noise.
            } else if !col.help.is_empty() {
                resp.on_hover_text(col.help);
            }

            // --- sort marker ---------------------------------------------
            let mut text_rect = cell.shrink2(Vec2::new(8.0, 0.0));
            if is_sorted {
                let arrow_w = 12.0;
                let cx = match col.align {
                    Align::Right => cell.left() + 8.0,
                    _ => cell.right() - 14.0,
                };
                // Keep the label clear of the marker.
                match col.align {
                    Align::Right => {
                        text_rect =
                            Rect::from_min_max(Pos2::new(cx + arrow_w, cell.top()), text_rect.max)
                    }
                    _ => {
                        text_rect = Rect::from_min_max(
                            text_rect.min,
                            Pos2::new(cx - 2.0, cell.bottom()),
                        )
                    }
                }
                draw_sort_arrow(
                    &painter,
                    Pos2::new(cx + arrow_w * 0.5, cell.center().y),
                    self.sort_desc,
                    t.accent.default,
                );
            }

            let color = if is_sorted { t.accent.default } else { t.text.secondary };

            // --- live reading, left of the title -------------------------
            // Body size, like the title: this is a number a reader is meant to
            // read, not a footnote. `body` against the title's `body-strong`, in
            // `text-secondary`, is the whole distinction it needs -- the title
            // says what the column is, the reading only what it adds up to now.
            let mut title_rect = text_rect;
            if let Some((_, value)) = values.iter().find(|(id, _)| *id == col.id) {
                let clipped = painter.with_clip_rect(cell);
                let measure = |s: &str, font: &FontId| {
                    painter
                        .layout_no_wrap(s.to_owned(), font.clone(), Color32::PLACEHOLDER)
                        .size()
                        .x
                };
                let gap = space::S2;
                match col.align {
                    // The title stays anchored to the right edge; the reading
                    // ends a gap short of where the title begins.
                    Align::Right => {
                        let right =
                            text_rect.right() - measure(col.title, &t.fonts.body_strong) - gap;
                        paint_cell_text(
                            &clipped,
                            Rect::from_min_max(
                                text_rect.min,
                                Pos2::new(right, text_rect.bottom()),
                            ),
                            value,
                            &t.fonts.body,
                            t.text.secondary,
                            Align::Right,
                            head_baseline,
                        );
                    }
                    // Left-aligned: the reading takes the left edge and the title
                    // moves along to make room.
                    Align::Left => {
                        paint_cell_text(
                            &clipped,
                            text_rect,
                            value,
                            &t.fonts.body,
                            t.text.secondary,
                            Align::Left,
                            head_baseline,
                        );
                        let shift = measure(value, &t.fonts.body) + gap;
                        title_rect = Rect::from_min_max(
                            Pos2::new(text_rect.left() + shift, text_rect.top()),
                            text_rect.max,
                        );
                    }
                }
            }

            paint_cell_text(
                &painter,
                title_rect,
                col.title,
                // `th { font: body-strong }`.
                &t.fonts.body_strong,
                color,
                col.align,
                head_baseline,
            );

            // --- resize handle -------------------------------------------
            let is_last = slot + 1 == cells.len();
            if !is_last {
                let handle = Rect::from_min_max(
                    Pos2::new(cell.right() - 4.0, cell.top()),
                    Pos2::new(cell.right() + 4.0, cell.bottom()),
                );
                let h = ui.interact(
                    handle,
                    id_base.with("edge").with(col.id),
                    Sense::click_and_drag(),
                );
                if h.hovered() || h.dragged() {
                    ui.ctx()
                        .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    painter.vline(
                        cell.right() - 0.5,
                        cell.y_range(),
                        Stroke::new(1.0, t.accent.default),
                    );
                }
                if h.drag_started() {
                    self.dragging_edge = Some(col_idx);
                }
                if h.dragged() && self.dragging_edge == Some(col_idx) {
                    let dx = h.drag_delta().x;
                    if col.stretch {
                        // A stretch column's width is whatever is left over, so
                        // widening it would immediately be cancelled out by the
                        // slack shrinking and the divider would not move. Resize
                        // the neighbour instead, which is what the user is
                        // actually pointing at.
                        if let Some(&next) = slots.get(slot + 1) {
                            let nc = self.cols[next];
                            pending_width = Some((next, (nc.width - dx).max(nc.min_width)));
                        }
                    } else {
                        pending_width = Some((col_idx, (col.width + dx).max(col.min_width)));
                    }
                }
                if h.drag_stopped() {
                    self.dragging_edge = None;
                }
            }

            // Column separator.
            if !is_last {
                painter.vline(
                    cell.right() - 0.5,
                    egui::Rangef::new(cell.top() + 7.0, cell.bottom() - 7.0),
                    Stroke::new(1.0, t.stroke.subtle),
                );
            }
        }

        if let Some((idx, w)) = pending_width {
            self.cols[idx].width = w;
        }

        sort_changed || cols_changed
    }

    /// Draw the scrollable body.
    ///
    /// `paint_row` is called only for rows intersecting the viewport. It gets a
    /// [`RowPainter`] holding the row's cell rectangles, already clipped.
    pub fn body(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        id_base: Id,
        n_rows: usize,
        selected: Option<usize>,
        mut paint_row: impl FnMut(&mut RowPainter<'_>),
    ) -> TableOutput {
        let row_h = self.row_h;

        // Row to bring into view (keyboard navigation, "go to details").
        // Handled inside the scroll area with `scroll_to_rect` so the view moves
        // the minimum needed; forcing an absolute offset would yank the list on
        // every arrow keypress even when the row was already visible.
        let bring_into_view = self.scroll_to_row.take();

        // How wide the grid wants to be, against the width there is for it. Past
        // that the columns overflow the right edge, and a table whose right-hand
        // columns cannot be reached is a table with its data hidden -- so the body
        // scrolls sideways and the header, already drawn from `self.x_offset`,
        // travels with it.
        let view_w = ui.available_width() - Self::bar_width(ui);
        let grid_w = self.min_grid_width().max(view_w);
        // Half a point of slack: a grid that overshoots by a rounding error does
        // not need a scrollbar.
        let h_scroll = grid_w > view_w + 0.5;
        if !h_scroll {
            // An offset left over from a narrower window, or from a column since
            // hidden, would otherwise hold the grid to the left with no bar left to
            // bring it back.
            self.x_offset = 0.0;
        }

        let mut hovered_row = None;
        let mut clicked_row = None;
        let mut double_clicked_row = None;
        let mut context_row = None;

        // Scoped so the scrollbar restyling below cannot leak into whatever the
        // caller draws next.
        let styled = ui.scope(|ui| {
        azur_egui_theme::scrollbar(ui, t);

        // Horizontal only when the grid overflows. `AlwaysVisible` reserves a bar
        // for every *enabled* direction, so leaving the axis switched off is what
        // keeps a table that fits from carrying an inert bar along its bottom edge.
        let scroll = egui::ScrollArea::new([h_scroll, true])
            .id_salt(id_base.with("scroll"))
            // Pushed in and read back out below, rather than left to live inside the
            // scroll area: the header was painted before this runs, from the same
            // number, and could not otherwise know where the body ended up.
            .horizontal_scroll_offset(self.x_offset)
            // The vertical bar is always reserved. The header subtracts exactly this
            // width to line its columns up with the body's, and a bar that came and
            // went as the row count crossed the overflow point would shift every
            // column by 14px underneath a stationary heading.
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .auto_shrink([false, false]);

        let out = scroll.show_viewport(ui, |ui, viewport| {
            let width = grid_w;
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(width, (n_rows as f32 * row_h).max(1.0)),
                Sense::click(),
            );
            // The visible span, in screen coordinates: `viewport` is in content
            // coordinates and `rect` starts at the content's origin, so this is the
            // window onto the grid wherever it happens to be scrolled to.
            let visible_x =
                egui::Rangef::new(rect.left() + viewport.min.x, rect.left() + viewport.max.x);

            if let Some(row) = bring_into_view {
                if row < n_rows {
                    // Computed rather than captured while painting: the target
                    // row may currently be scrolled out of view and therefore
                    // never painted at all.
                    let y = rect.top() + row as f32 * row_h;
                    // Only the row's *vertical* position is being asked for, so the
                    // target spans exactly what is already visible across: a
                    // narrower rect would be scrolled into view horizontally too,
                    // and every arrow keypress would drag the grid back to column
                    // one.
                    ui.scroll_to_rect(
                        Rect::from_x_y_ranges(visible_x, y..=y + row_h),
                        None,
                    );
                }
            }

            // Rows are uniform height, so the visible window is arithmetic.
            let first = ((viewport.min.y / row_h).floor().max(0.0) as usize).min(n_rows);
            let last = (((viewport.max.y / row_h).ceil() as usize) + 1).min(n_rows);

            // One hit test for the whole table: the row is arithmetic from y.
            let pointer_row = if ui.rect_contains_pointer(rect) {
                ui.ctx().pointer_latest_pos().and_then(|p| {
                    let r = ((p.y - rect.top()) / row_h).floor();
                    (r >= 0.0 && (r as usize) < n_rows).then_some(r as usize)
                })
            } else {
                None
            };
            hovered_row = pointer_row;

            if let Some(r) = pointer_row {
                if response.clicked() {
                    clicked_row = Some(r);
                }
                if response.double_clicked() {
                    double_clicked_row = Some(r);
                }
            }
            // Right-click is reported through the secondary-click helper so it
            // also works when the press and release land on different rows.
            if response.secondary_clicked() {
                context_row = pointer_row;
            }

            let mut cells = Vec::with_capacity(self.cols.len());
            // Clipped to the visible span exactly. egui leaves `clip_rect_margin`
            // of slack around a scrolled viewport, which costs nothing on a page and
            // is three points of a row fill -- or of the glyph at the grid's right
            // edge -- showing inside the scrollbar's gutter here.
            let painter = ui
                .painter()
                .with_clip_rect(Rect::from_x_y_ranges(visible_x, ui.clip_rect().y_range()));
            // One baseline for every row, measured once: rows are uniform, so
            // this is the same number for all of them.
            //
            // The ink baseline, because these rows are not text alone -- an icon and
            // an expander in the name column, a pill in Status, a heat bar behind a
            // number -- and all of those are centred in the row. Text centred by its
            // line box hangs a point and a half below them, at every size, which
            // reads as the window being slightly out of true.
            let base_dy = ink_baseline(&painter, &t.fonts.body, 0.0, row_h);

            for row in first..last {
                let y = rect.top() + row as f32 * row_h;
                let row_rect =
                    Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(width, row_h));

                let is_selected = selected == Some(row);
                let is_hovered = hovered_row == Some(row);

                // Row background: selection wins, then hover, then the zebra.
                //
                // The fills come from `azur::desktop`, which on this side of the
                // palette puts a selected row on the accent's *surface* ramp:
                // `accent-subtle` is Azur's list vocabulary and, on a panel this
                // dark, a navy barely off it -- an eye misses it in a list this
                // long. The zebra is this window's own and stays underneath.
                match desktop::row_fill(t, is_selected, is_hovered) {
                    Some(fill) => {
                        painter.rect_filled(row_rect, desktop::structural_radius(), fill);
                    }
                    None if row % 2 == 1 => {
                        painter.rect_filled(row_rect, desktop::structural_radius(), t.row_alt);
                    }
                    None => {}
                }
                if is_selected {
                    // `accent-mark` -- the accent as ink. A bar from the same ramp
                    // as the fill would be a bar the colour of what it is drawn on.
                    desktop::selection_bar(&painter, row_rect, t);
                }

                self.layout(row_rect, &mut cells);
                let mut rp = RowPainter {
                    row,
                    rect: row_rect,
                    cells: &cells,
                    painter: &painter,
                    theme: t,
                    selected: is_selected,
                    hovered: is_hovered,
                    baseline: row_rect.top() + base_dy,
                    sink: None,
                };
                paint_row(&mut rp);
            }

            response
        });

        // Where the bar, the wheel and the clamp against the grid's right edge left
        // it. The header reads this on the next pass, and the pass after hands it
        // straight back in, so the two never drift.
        self.x_offset = out.state.offset.x;

        out.inner
        });

        // No `collection_outline` here any more. Azur rings a collection with
        // `stroke-subtle` at `radius-medium`, which is right for a scroll area
        // floating on a page and wrong for the pane that *is* this window: the
        // table is welded to the toolbar above it, the rail beside it and the
        // status bar below it, and each of those already draws the one line that
        // divides them. A ring inside those seams is a second boundary a point
        // away from the first, with a rounded corner on a square row.
        // The row under the pointer, spelled out in full. A cell is as wide as
        // its column and crops what does not fit; this is where the rest of it
        // goes.
        if let Some(row) = hovered_row {
            self.row_tooltip(ui, t, &styled.inner, row, &mut paint_row);
        }

        TableOutput {
            clicked_row,
            double_clicked_row,
            context_row,
            response: styled.inner,
        }
    }

    /// Every column of `row`, uncropped, in a tooltip pinned to the pointer.
    ///
    /// The row callback is run a second time against a [`RowPainter`] that
    /// records instead of painting, so the tooltip says exactly what the cells
    /// say -- there is no second copy of any tab's formatting to keep in step.
    fn row_tooltip(
        &self,
        ui: &Ui,
        t: &Theme,
        response: &Response,
        row: usize,
        paint_row: &mut impl FnMut(&mut RowPainter<'_>),
    ) {
        let rows = self.read_row(&ui.painter().with_clip_rect(Rect::NOTHING), t, row, paint_row);
        if rows.is_empty() {
            return;
        }

        // 400px, or less in a window too narrow for it. A tooltip is read as a
        // block, not scanned across: past about that width the eye has to travel
        // back to find the next line, and a long path is easier to read wrapped
        // than stretched. Narrower than the window either way, so there is always
        // some of the row visible beside it.
        let width = (ui.ctx().viewport_rect().width() - 48.0).min(TOOLTIP_W);
        let mut tip = egui::Tooltip::for_enabled(response)
            // Anchored to the pointer, not to the widget: the widget here is the
            // whole scrolled body, so a widget-anchored tooltip would appear at
            // the bottom of the table however far up the row is.
            .at_pointer()
            .width(width);
        tip.popup = tip
            .popup
            .frame(azur_egui_theme::components::popover_frame(t));
        tip.show(|ui| {
            // Claimed outright, because `width` above is only a *default* for the
            // popup's area: without this the contents lay out against whatever
            // width the first frame happened to have, which in a popup is none.
            ui.set_min_width(width);
            ui.spacing_mut().item_spacing.y = space::S1;
            // Painted rather than added as widgets, so a name and its value can share
            // a baseline. Two labels in a `horizontal_top` are aligned by their
            // *boxes*, and a caption box is shorter than a body box, so the name would
            // sit a couple of pixels above the value it names -- for every row, all
            // the way down the tooltip.
            let painter = ui.painter().clone();
            let names: Vec<_> = rows
                .iter()
                .map(|(label, _)| {
                    painter.layout_no_wrap(
                        (*label).to_owned(),
                        t.fonts.caption.clone(),
                        t.text.secondary,
                    )
                })
                .collect();
            // One column for the names, wide enough for the widest of them, so every
            // value starts at the same x and no label can run into one. `egui::Grid`
            // would do this for free except that it measures its columns on the frame
            // it first appears and remembers them, and in a popup that first frame has
            // no width -- which is how a tooltip ends up one word wide and stays that
            // way. Capped, because a value squeezed into the last 90px of a tooltip is
            // a worse outcome than a label that has to be read across a wide gap.
            let column = names
                .iter()
                .map(|g| g.size().x)
                .fold(0.0f32, f32::max)
                .min(width * 0.45)
                + space::S4;
            for ((label, value), name) in rows.iter().zip(names) {
                let _ = label;
                // Wrapped, not truncated: cropping here would defeat the point of
                // the tooltip.
                let text = painter.layout(
                    value.clone(),
                    t.fonts.body.clone(),
                    t.text.primary,
                    (width - column).max(24.0),
                );
                let (line, _) = ui.allocate_exact_size(
                    Vec2::new(width, text.size().y.max(name.size().y)),
                    Sense::hover(),
                );
                // The value is the principal text, so it sets the line; a value
                // that wrapped to three lines still has its *first* line on it.
                let baseline =
                    row_baseline(&painter, &t.fonts.body, line.top(), typography::LINE_BODY);
                galley_on_baseline(&painter, line.left(), baseline, name);
                galley_on_baseline(&painter, line.left() + column, baseline, text);
            }
        });
    }

    /// Run the row callback against a recording [`RowPainter`] and return what
    /// each column would have said.
    ///
    /// `painter` is never drawn into — pass one clipped to nothing. Nothing here
    /// lays out text either, which is what lets a test call it without fonts.
    ///
    /// This is what the row tooltip is built from, and it is public so a "copy
    /// everything about this row" command can be built from the same pass: two
    /// readings of one row that could disagree would be a bug waiting to happen.
    pub fn read_row(
        &self,
        painter: &Painter,
        t: &Theme,
        row: usize,
        paint_row: &mut impl FnMut(&mut RowPainter<'_>),
    ) -> Vec<(&'static str, String)> {
        let titles: Vec<&'static str> = self.visible_cols().map(|(_, c)| c.title).collect();
        // Cells wide enough that nothing declines to draw: the callbacks check a
        // cell's width before writing into it, and a degenerate rect would drop
        // the very text this pass exists to collect.
        const WIDE: f32 = 4000.0;
        let cells: Vec<Rect> = (0..titles.len())
            .map(|i| {
                Rect::from_min_size(
                    Pos2::new(i as f32 * WIDE, 0.0),
                    Vec2::new(WIDE, self.row_h),
                )
            })
            .collect();
        let mut rows: Vec<(&'static str, String)> = Vec::with_capacity(titles.len());
        let mut rp = RowPainter {
            row,
            rect: Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(WIDE * titles.len() as f32, self.row_h),
            ),
            cells: &cells,
            painter,
            theme: t,
            // Neither, in a read pass: a callback that records the hovered row's
            // geometry (the tree expander's hit rect) would otherwise record this
            // fabricated one.
            selected: false,
            hovered: false,
            baseline: 0.0,
            sink: Some(Sink {
                titles: &titles,
                out: &mut rows,
            }),
        };
        paint_row(&mut rp);
        rows
    }
}

/// Painting surface handed to the row callback.
pub struct RowPainter<'a> {
    pub row: usize,
    pub rect: Rect,
    pub cells: &'a [Rect],
    pub painter: &'a Painter,
    pub theme: &'a Theme,
    pub selected: bool,
    pub hovered: bool,
    /// Absolute y every cell in this row puts its text baseline on.
    baseline: f32,
    /// Set when the row is being *read* rather than drawn — see [`Table::body`].
    /// Every text call appends `(column title, text)` here and paints nothing.
    sink: Option<Sink<'a>>,
}

/// Where a read pass puts what the row would have said.
struct Sink<'a> {
    /// Slot -> column title, for the columns the row callback will address.
    titles: &'a [&'static str],
    out: &'a mut Vec<(&'static str, String)>,
}

impl<'a> RowPainter<'a> {
    /// Rect of the nth *visible* column.
    #[inline]
    pub fn cell(&self, slot: usize) -> Rect {
        self.cells
            .get(slot)
            .copied()
            .unwrap_or(Rect::from_min_size(self.rect.min, Vec2::ZERO))
    }

    /// Record a cell's text in a read pass. Returns true when there is nothing
    /// left to draw.
    fn record(&mut self, slot: usize, s: &str) -> bool {
        let Some(sink) = self.sink.as_mut() else {
            return false;
        };
        if let Some(title) = sink.titles.get(slot) {
            // A column addressed twice (a name and its badge, say) keeps both,
            // joined: dropping either would make the tooltip disagree with the
            // cell it is explaining.
            match sink.out.iter_mut().find(|(t, _)| t == title) {
                Some((_, existing)) => {
                    existing.push_str("  ");
                    existing.push_str(s);
                }
                None => sink.out.push((title, s.to_owned())),
            }
        }
        true
    }

    /// A line for the row tooltip that no column carries.
    ///
    /// Painting ignores it -- there is no cell for it to go in -- and the read pass
    /// [`Table::read_row`] records it, so it appears in the tooltip and in "copy this
    /// row" and nowhere else. For the fact that explains a row and is the wrong shape
    /// for a column: an image path, a command line. Both are long enough that a
    /// column showing them is a column showing the first third of them, which is why
    /// the tabs that have such a column keep it switched off.
    ///
    /// The value arrives as a closure because it is the read pass's business alone:
    /// painting runs for every visible row on every frame, and one of these costs a
    /// registry read. It is called once, for the one row under the pointer.
    ///
    /// Dropped when a visible column carries the same heading, so a reader who turns
    /// that column on does not get told twice.
    pub fn detail(&mut self, label: &'static str, value: impl FnOnce() -> String) {
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        if sink.titles.contains(&label) {
            return;
        }
        let value = value();
        // Nothing rather than an empty line: a path that could not be read is the
        // ordinary case for a protected process, and a heading with nothing after it
        // says less than no heading at all.
        if !value.is_empty() {
            sink.out.push((label, value));
        }
    }

    /// Draw a cell's text, clipped to the column and aligned.
    pub fn text(&mut self, slot: usize, s: &str, color: Color32, align: Align) {
        self.text_font(slot, s, color, align, &self.theme.fonts.body.clone());
    }

    pub fn text_font(&mut self, slot: usize, s: &str, color: Color32, align: Align, font: &FontId) {
        if s.is_empty() {
            return;
        }
        if self.record(slot, s) {
            return;
        }
        let cell = self.cell(slot);
        paint_cell_text(
            &self.painter.with_clip_rect(cell),
            cell.shrink2(Vec2::new(8.0, 0.0)),
            s,
            font,
            color,
            align,
            self.baseline,
        );
    }

    /// Text indented from the left, for tree rows.
    pub fn text_indent(&mut self, slot: usize, indent: f32, s: &str, color: Color32) {
        if s.is_empty() {
            return;
        }
        if self.record(slot, s) {
            return;
        }
        let cell = self.cell(slot);
        let inner = Rect::from_min_max(
            Pos2::new(cell.left() + indent, cell.top()),
            Pos2::new(cell.right() - 6.0, cell.bottom()),
        );
        if inner.width() <= 2.0 {
            return;
        }
        paint_cell_text(
            &self.painter.with_clip_rect(cell),
            inner,
            s,
            &self.theme.fonts.body,
            color,
            Align::Left,
            self.baseline,
        );
    }

    /// Magnitude tint behind a numeric cell, in that column's own series colour.
    pub fn heat(&self, slot: usize, t01: f32, hue: Color32) {
        // Selection already colours the row; tinting on top muddies both.
        if self.selected {
            return;
        }
        let c = self.theme.heat(t01, hue);
        if c != Color32::TRANSPARENT {
            self.painter
                .rect_filled(self.cell(slot), CornerRadius::ZERO, c);
        }
    }

    /// A 16x16 icon at the left of a cell, at `indent`.
    pub fn icon(&self, slot: usize, indent: f32, tex: egui::TextureId) {
        let cell = self.cell(slot);
        let size = 16.0;
        let pos = Pos2::new(
            cell.left() + indent,
            cell.center().y - size * 0.5,
        );
        let r = Rect::from_min_size(pos, Vec2::splat(size));
        if !cell.intersects(r) {
            return;
        }
        self.painter.with_clip_rect(cell).add(egui::Shape::image(
            tex,
            r,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        ));
    }

    /// Triangle disclosure control for a tree row. Returns its hit rect.
    pub fn expander(&self, slot: usize, indent: f32, expanded: bool, color: Color32) -> Rect {
        let cell = self.cell(slot);
        let box_size = 16.0;
        let r = Rect::from_min_size(
            Pos2::new(cell.left() + indent, cell.center().y - box_size * 0.5),
            Vec2::splat(box_size),
        );
        let c = r.center();
        let s = 3.6;
        let pts = if expanded {
            // Pointing down.
            vec![
                Pos2::new(c.x - s, c.y - s * 0.6),
                Pos2::new(c.x + s, c.y - s * 0.6),
                Pos2::new(c.x, c.y + s * 0.8),
            ]
        } else {
            // Pointing right.
            vec![
                Pos2::new(c.x - s * 0.6, c.y - s),
                Pos2::new(c.x - s * 0.6, c.y + s),
                Pos2::new(c.x + s * 0.8, c.y),
            ]
        };
        self.painter
            .with_clip_rect(cell)
            .add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        r
    }

    /// Small pill used for status labels ("Suspended", "Running").
    pub fn pill(&mut self, slot: usize, s: &str, fg: Color32, bg: Color32) {
        if self.record(slot, s) {
            return;
        }
        let cell = self.cell(slot);
        let font = &self.theme.fonts.caption;
        let galley = self.painter.layout_no_wrap(s.to_owned(), font.clone(), fg);
        let pad = Vec2::new(7.0, 2.0);
        // The label sits on the row's baseline like any other cell -- a pill is
        // still text, and "Running" reading a pixel above the name beside it is
        // exactly the wobble the shared baseline exists to remove. The box is
        // then hung around the label rather than centred in the cell itself.
        let pos = Pos2::new(
            cell.left() + 8.0 + pad.x,
            self.baseline - galley_baseline(&galley),
        );
        let r = Rect::from_min_size(pos - pad, galley.size() + pad * 2.0);
        let p = self.painter.with_clip_rect(cell);
        p.rect_filled(r, CornerRadius::same(3), bg);
        p.galley(pos, galley, fg);
    }
}

/// Checkbox list of every column. Returns true when one was toggled.
///
/// Takes the column slice rather than `&mut Table` so it can be called from
/// inside `header`, which already holds other parts of `self`.
fn column_picker(ui: &mut Ui, cols: &mut [Column]) -> bool {
    let mut changed = false;
    // Floating, the way `Menu` scrolls its own overflow: a non-floating bar
    // allocates its width whether or not it is showing, so the Details table's
    // twenty-odd entries would each lose 14pt off their right edge -- enough to
    // ellipsize the longest titles in a menu that has room for them.
    ui.style_mut().spacing.scroll.floating = true;
    egui::ScrollArea::vertical()
        .max_height(440.0)
        .show(ui, |ui| {
            // Refuse to hide the last visible column: the picker hangs off the
            // header cells, so an empty grid would have no way back.
            let shown = cols.iter().filter(|c| c.visible).count();
            for c in cols.iter_mut() {
                let mut on = c.visible;
                let can_toggle = !c.visible || shown > 1;
                // Azur's `Checkbox`, not `egui::Checkbox`: the stock one draws
                // egui's own box, tick and hover fill, which is a different shape,
                // a different weight and a different blue from every other tick in
                // the window — including the one in the Sensors page's own
                // show/hide menu, two clicks away.
                let resp = ui.add_enabled(can_toggle, Checkbox::new(&mut on, c.title));
                if resp.changed() {
                    c.visible = on;
                    changed = true;
                }
            }
        });
    changed
}

/// Paint text inside `rect`, horizontally aligned, sitting on `baseline`.
///
/// The baseline is passed in rather than derived from `rect` because a row mixes
/// fonts (see [`row_baseline`]) and every cell has to land on the same one.
fn paint_cell_text(
    painter: &Painter,
    rect: Rect,
    s: &str,
    font: &FontId,
    color: Color32,
    align: Align,
    baseline: f32,
) {
    if rect.width() <= 1.0 {
        return;
    }
    let galley = painter.layout_no_wrap(s.to_owned(), font.clone(), color);
    let x = match align {
        Align::Left => rect.left(),
        Align::Right => rect.right() - galley.size().x,
    };
    galley_on_baseline(painter, x, baseline, galley);
}


/// Where the arrow keys would move a flat list's selection, if anywhere.
///
/// The keyboard half of a table, for the tabs whose rows are a plain list: Up and
/// Down by one, Home and End to the ends, and a first press with nothing selected
/// starts at the end the key points away from. The Processes tab has its own
/// version because a tree also answers Left and Right.
///
/// Returns the target row even when it is the current one, so holding Down at the
/// bottom of a list keeps it scrolled into view rather than going quiet.
///
/// Nothing happens while a widget holds focus: the filter box is one keystroke
/// away from every one of these tables, and typing `End` into it would otherwise
/// also jump the selection to the last row.
pub fn list_nav(ui: &Ui, current: Option<usize>, n_rows: usize) -> Option<usize> {
    if n_rows == 0 || ui.ctx().memory(|m| m.focused().is_some()) {
        return None;
    }
    let last = n_rows - 1;
    ui.input(|i| {
        for (key, target) in [
            (
                egui::Key::ArrowDown,
                current.map_or(0, |r| (r + 1).min(last)),
            ),
            (egui::Key::ArrowUp, current.map_or(last, |r| r.saturating_sub(1))),
            (egui::Key::Home, 0),
            (egui::Key::End, last),
        ] {
            if i.key_pressed(key) {
                return Some(target);
            }
        }
        None
    })
}

fn draw_sort_arrow(painter: &Painter, center: Pos2, desc: bool, color: Color32) {
    let s = 3.2;
    let pts = if desc {
        vec![
            Pos2::new(center.x - s, center.y - s * 0.5),
            Pos2::new(center.x + s, center.y - s * 0.5),
            Pos2::new(center.x, center.y + s * 0.7),
        ]
    } else {
        vec![
            Pos2::new(center.x - s, center.y + s * 0.5),
            Pos2::new(center.x + s, center.y + s * 0.5),
            Pos2::new(center.x, center.y - s * 0.7),
        ]
    };
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Table {
        Table::new(
            vec![
                Column::new(0, "Name", 200.0, Align::Left),
                Column::new(1, "Status", 90.0, Align::Left),
                // Hidden: the tooltip explains the row as displayed, and a column
                // the reader turned off is not displayed.
                {
                    let mut c = Column::new(2, "Command", 400.0, Align::Left);
                    c.visible = false;
                    c
                },
                Column::new(3, "PID", 80.0, Align::Right),
            ],
            0,
            false,
        )
    }

    /// The read pass is what the hover tooltip is made of: it runs the tab's own
    /// row callback, so whatever a cell would have said is what it collects --
    /// uncropped, and without a second copy of the formatting to keep in step.
    ///
    /// Slots are indexes into the *visible* columns, here and when painting: with
    /// "Command" hidden, slot 2 is PID.
    #[test]
    fn reads_back_what_the_row_would_have_drawn() {
        let t = Theme::dark();
        let table = table();
        let mut collected = Vec::new();
        egui::__run_test_ui(|ui| {
            let painter = ui.painter().with_clip_rect(Rect::NOTHING);
            collected = table.read_row(&painter, &t, 7, &mut |rp| {
                assert_eq!(rp.row, 7, "the callback is told which row to read");
                rp.text_indent(0, 20.0, "a name far too long for its column", t.text.primary);
                rp.pill(1, "Running", t.text.primary, t.bg.card);
                rp.text(2, "4 812", t.text.secondary, Align::Right);
                // Past the last visible column: dropped, exactly as painting
                // drops it.
                rp.text(3, "nowhere to put this", t.text.secondary, Align::Right);
            });
        });

        assert_eq!(
            collected,
            vec![
                ("Name", "a name far too long for its column".to_owned()),
                ("Status", "Running".to_owned()),
                ("PID", "4 812".to_owned()),
            ]
        );
    }

    /// A detail is a tooltip line no *visible* column carries: the read pass
    /// collects it, the paint pass costs nothing for it, and a visible column with
    /// the same heading wins -- which is what stops the Details tab printing its
    /// command line twice when the reader turns that column on.
    ///
    /// A *hidden* column of the same name does not suppress it, and that is the
    /// point of the whole thing: the tooltip is then the only place the row can say
    /// where its program lives, which is why those columns are off by default.
    #[test]
    fn collects_the_details_no_visible_column_carries() {
        let t = Theme::dark();
        let table = table();
        let mut collected = Vec::new();
        let mut built_dropped = 0usize;
        egui::__run_test_ui(|ui| {
            let painter = ui.painter().with_clip_rect(Rect::NOTHING);
            collected = table.read_row(&painter, &t, 0, &mut |rp| {
                rp.text(0, "svchost.exe", t.text.primary, Align::Left);
                rp.detail("Image path", || {
                    r"C:\Windows\System32\svchost.exe".to_owned()
                });
                // "Status" is on show, so the row has a cell for this already.
                rp.detail("Status", || {
                    built_dropped += 1;
                    "Running".to_owned()
                });
                // "Command" is a column of this table too, but a hidden one, so the
                // tooltip is the only place the row can say it.
                rp.detail("Command", || "svchost.exe -k netsvcs".to_owned());
                // Nothing to say -- a protected process whose path could not be
                // read. A heading with nothing after it says less than no heading.
                rp.detail("User name", String::new);
            });
        });
        assert_eq!(
            collected,
            vec![
                ("Name", "svchost.exe".to_owned()),
                ("Image path", r"C:\Windows\System32\svchost.exe".to_owned()),
                ("Command", "svchost.exe -k netsvcs".to_owned()),
            ]
        );
        assert_eq!(
            built_dropped, 0,
            "a line a visible column carries is dropped without being built"
        );
    }

    /// And painting asks for none of them: the closure is the read pass's business,
    /// and one of these costs a registry read on every visible row of every frame.
    #[test]
    fn painting_never_asks_a_detail_for_its_value() {
        let t = Theme::dark();
        let table = table();
        let mut asked = 0usize;
        egui::__run_test_ui(|ui| {
            let mut cells = Vec::new();
            table.layout(ui.max_rect(), &mut cells);
            let mut rp = RowPainter {
                row: 0,
                rect: ui.max_rect(),
                cells: &cells,
                painter: &ui.painter().with_clip_rect(Rect::NOTHING),
                theme: &t,
                selected: false,
                hovered: false,
                baseline: 0.0,
                sink: None,
            };
            rp.detail("Image path", || {
                asked += 1;
                "never".to_owned()
            });
        });
        assert_eq!(asked, 0);
    }

    /// A cell drawn in two parts — Task Manager writes a name and then a
    /// "Suspended" badge into the same column — reads back as one entry holding
    /// both. Keeping only one of them would make the tooltip contradict the cell.
    #[test]
    fn joins_a_column_written_to_twice() {
        let t = Theme::dark();
        let table = table();
        let mut collected = Vec::new();
        egui::__run_test_ui(|ui| {
            let painter = ui.painter().with_clip_rect(Rect::NOTHING);
            collected = table.read_row(&painter, &t, 0, &mut |rp| {
                rp.text(0, "firefox.exe", t.text.primary, Align::Left);
                rp.pill(0, "Suspended", t.status.warning, t.bg.card);
            });
        });
        assert_eq!(
            collected,
            vec![("Name", "firefox.exe  Suspended".to_owned())]
        );
    }

    /// An empty cell says nothing rather than saying "".
    #[test]
    fn leaves_out_the_columns_a_row_has_nothing_for() {
        let t = Theme::dark();
        let table = table();
        let mut collected = Vec::new();
        egui::__run_test_ui(|ui| {
            let painter = ui.painter().with_clip_rect(Rect::NOTHING);
            collected = table.read_row(&painter, &t, 0, &mut |rp| {
                rp.text(0, "svchost.exe", t.text.primary, Align::Left);
                rp.text(1, "", t.text.primary, Align::Left);
            });
        });
        assert_eq!(collected, vec![("Name", "svchost.exe".to_owned())]);
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    /// Five 200pt columns, none of which will go below 100. Nothing is marked
    /// `stretch`, so the last one is the flexible one -- which is what `layout`
    /// does when no column claims the slack.
    fn wide() -> Table {
        Table::new(
            (0..5u16)
                .map(|i| Column::new(i, "Col", 200.0, Align::Left).min(100.0))
                .collect(),
            0,
            false,
        )
    }

    /// `frames` header-and-body passes over a pane `pane_w` wide, starting from an
    /// offset of `x`, with `events` delivered on the first of them. Returns the
    /// offset the table ends up with.
    ///
    /// One context across all the passes, because that is where the scroll area
    /// keeps its state -- and both of the things tested through here are spread over
    /// frames rather than done on the one that started them: a scroll-to-a-target is
    /// animated, and a wheel delta is smoothed. A test given one frame could not
    /// tell the difference.
    fn passes(
        table: &mut Table,
        pane_w: f32,
        x: f32,
        frames: usize,
        events: Vec<egui::Event>,
    ) -> f32 {
        let t = Theme::dark();
        let ctx = egui::Context::default();
        // Real fonts and the real density: a heading is laid out here, and the
        // scrollbar's width -- which the header subtracts to find its view -- is one
        // of the things the density sets.
        super::super::theme::install(&ctx);
        table.x_offset = x;
        let base = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(pane_w, 400.0))),
            ..Default::default()
        };
        for frame in 0..frames {
            let mut input = base.clone();
            if frame == 0 {
                input.events = events.clone();
            }
            let _ = ctx.run_ui(input, |ui| {
                let id = Id::new("table");
                table.header(ui, &t, id);
                table.body(ui, &t, id, 40, None, |rp| {
                    rp.text(0, "a value", t.text.primary, Align::Left);
                });
            });
        }
        table.x_offset
    }

    fn pass(table: &mut Table, pane_w: f32, x: f32, frames: usize) -> f32 {
        passes(table, pane_w, x, frames, Vec::new())
    }

    /// One flick of a horizontal wheel, `lines` of it, with the pointer over the
    /// rows -- and enough frames after it for egui to finish smoothing the delta out.
    ///
    /// A line delta on the x axis is exactly what a tilting wheel or a thumb rocker
    /// arrives as: Windows sends `WM_MOUSEHWHEEL`, winit turns it into
    /// `MouseScrollDelta::LineDelta(value, 0.0)` -- negating it on the way -- and
    /// egui-winit forwards that as this event.
    fn tilt(table: &mut Table, pane_w: f32, x: f32, lines: f32) -> f32 {
        passes(
            table,
            pane_w,
            x,
            60,
            vec![
                // The wheel only turns what it is pointing at.
                egui::Event::PointerMoved(Pos2::new(pane_w * 0.5, 200.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: Vec2::new(lines, 0.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: Default::default(),
                },
            ],
        )
    }

    /// A pane with room to spare is filled exactly: the flexible column takes the
    /// slack and the last column ends at the right edge. This is the case the table
    /// has always handled, and the one the horizontal bar must stay out of.
    #[test]
    fn a_grid_that_fits_takes_exactly_the_room_it_is_given() {
        let table = wide();
        let view = Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, row::DENSE));
        assert_eq!(table.grid_rect(view).width(), 1200.0);

        let mut cells = Vec::new();
        table.layout(table.grid_rect(view), &mut cells);
        assert_eq!(cells.last().unwrap().right(), 1200.0);
    }

    /// Below that, the grid stops shrinking and states the width it needs -- four
    /// columns at the 200 they declare plus the flexible one squeezed to the 100 it
    /// will not go below. That number is the scrollbar's range.
    #[test]
    fn a_grid_too_wide_for_its_pane_reports_what_it_needs() {
        let table = wide();
        assert_eq!(table.min_grid_width(), 900.0);
        let view = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, row::DENSE));
        assert_eq!(table.grid_rect(view).width(), 900.0);

        let mut cells = Vec::new();
        table.layout(table.grid_rect(view), &mut cells);
        assert_eq!(
            cells.iter().map(|c| c.width()).collect::<Vec<_>>(),
            vec![200.0, 200.0, 200.0, 200.0, 100.0]
        );
    }

    /// A column marked `stretch` is the flexible one wherever it sits, because that
    /// is the column `layout` hands the slack to.
    #[test]
    fn the_stretch_column_is_the_one_counted_at_its_minimum() {
        let mut cols: Vec<Column> = (0..3u16)
            .map(|i| Column::new(i, "Col", 200.0, Align::Left).min(50.0))
            .collect();
        cols[0] = cols[0].stretch();
        let table = Table::new(cols, 0, false);
        // 200 + 200 with the first column at 50, not the last.
        assert_eq!(table.min_grid_width(), 450.0);
    }

    /// Scrolling moves the whole grid, not the columns within it: every column keeps
    /// its width and shifts by the same amount, which is what keeps each heading over
    /// its own column. At the far end the last column lands flush against the right
    /// edge -- there is nothing past it left to reach.
    #[test]
    fn scrolling_moves_every_column_by_the_same_amount() {
        let mut table = wide();
        let view = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, row::DENSE));

        let mut home = Vec::new();
        table.layout(table.grid_rect(view), &mut home);

        table.x_offset = 120.0;
        let mut scrolled = Vec::new();
        table.layout(table.grid_rect(view), &mut scrolled);
        for (a, b) in home.iter().zip(&scrolled) {
            assert_eq!(b.left(), a.left() - 120.0);
            assert_eq!(b.width(), a.width());
        }

        table.x_offset = table.min_grid_width() - view.width();
        let mut end = Vec::new();
        table.layout(table.grid_rect(view), &mut end);
        assert_eq!(end.last().unwrap().right(), view.right());
    }

    /// The offset lives across a pass while the grid overflows, and is put back to
    /// the left when it stops: a table held to one side with no bar left to bring it
    /// back would simply have lost its first columns.
    #[test]
    fn the_offset_survives_a_pass_only_while_the_grid_overflows() {
        assert_eq!(pass(&mut wide(), 400.0, 300.0, 1), 300.0);
        assert_eq!(pass(&mut wide(), 1600.0, 300.0, 1), 0.0);
    }

    /// Bringing a row into view moves the list down and leaves the grid where the
    /// reader scrolled it across. The target handed to egui spans exactly what is
    /// already visible for that reason: a narrower rectangle would be scrolled into
    /// view horizontally too, and every arrow keypress would drag the table back to
    /// its first column.
    #[test]
    fn bringing_a_row_into_view_does_not_scroll_the_grid_back() {
        let mut table = wide();
        table.scroll_to_row = Some(30);
        // Long enough for the vertical scroll to animate all the way there, which is
        // also long enough for a horizontal one to have shown itself.
        assert_eq!(pass(&mut table, 400.0, 300.0, 40), 300.0);
    }

    /// A tilting wheel or a thumb rocker scrolls the grid sideways, and stops at
    /// each end of it.
    ///
    /// There is no code of ours in this path -- the axis being enabled at all is what
    /// opens it -- which is the reason to pin it: nothing in the table would fail if
    /// a wheel event stopped arriving, so nothing else would notice. The sign is
    /// egui's: it *subtracts* the delta from the offset, and winit has already
    /// negated the Windows message, so tilting right arrives as a negative delta and
    /// brings the columns further right into view.
    #[test]
    fn a_horizontal_wheel_scrolls_the_grid_and_stops_at_its_ends() {
        // Far more than the grid can travel, so both ends are reached and the
        // assertions do not depend on egui's lines-per-notch.
        let flick = 100.0;

        let right = tilt(&mut wide(), 400.0, 0.0, -flick);
        assert!(right > 0.0, "tilting right must scroll right, got {right}");
        assert_eq!(
            tilt(&mut wide(), 400.0, right, -flick),
            right,
            "the grid does not scroll past its last column"
        );

        assert_eq!(
            tilt(&mut wide(), 400.0, right, flick),
            0.0,
            "tilting left comes back, and stops at the first column"
        );
    }

    /// A column scrolled clean out of the view is not painted -- and, more to the
    /// point, not interacted with: egui narrows a widget to the pane's clip
    /// rectangle rather than to the span the grid is shown through, so an
    /// off-screen resize handle would otherwise still answer the pointer.
    ///
    /// At an offset of 260 the five 200pt columns sit at -260, -60, 140, 340 and
    /// 540. The first and the last are outside a pane about 390 wide, whatever
    /// exact width the scrollbar takes, leaving three titles painted.
    #[test]
    fn the_header_leaves_out_the_columns_scrolled_out_of_view() {
        let t = Theme::dark();
        let ctx = egui::Context::default();
        super::super::theme::install(&ctx);
        let mut table = wide();
        table.x_offset = 260.0;
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                ..Default::default()
            },
            |ui| {
                table.header(ui, &t, Id::new("table"));
            },
        );

        let titles = out
            .shapes
            .iter()
            .filter(|s| matches!(s.shape, egui::Shape::Text(_)))
            .count();
        assert_eq!(titles, 3);
    }
}

#[cfg(test)]
mod nav_tests {
    use super::list_nav;

    /// One egui pass with `key` pressed, asking where the selection would go.
    fn press(key: egui::Key, current: Option<usize>, n_rows: usize) -> Option<usize> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let mut out = None;
        let _ = ctx.run_ui(input, |ui| {
            out = list_nav(ui, current, n_rows);
        });
        out
    }

    /// Down and up by one, and the ends. This is the whole of what the Details,
    /// Services and Startup tabs were missing.
    #[test]
    fn walks_the_list_one_row_at_a_time() {
        assert_eq!(press(egui::Key::ArrowDown, Some(3), 10), Some(4));
        assert_eq!(press(egui::Key::ArrowUp, Some(3), 10), Some(2));
        assert_eq!(press(egui::Key::Home, Some(3), 10), Some(0));
        assert_eq!(press(egui::Key::End, Some(3), 10), Some(9));
    }

    /// The ends do not wrap, and neither of them panics -- `saturating_sub` on the
    /// top row and a clamp on the bottom, because a list is not a carousel.
    #[test]
    fn stops_at_both_ends() {
        assert_eq!(press(egui::Key::ArrowUp, Some(0), 10), Some(0));
        assert_eq!(press(egui::Key::ArrowDown, Some(9), 10), Some(9));
        // A one-row list is both ends at once.
        assert_eq!(press(egui::Key::ArrowDown, Some(0), 1), Some(0));
    }

    /// A first press with nothing selected starts at the end the key points away
    /// from, so Down enters at the top and Up at the bottom.
    #[test]
    fn enters_the_list_from_the_end_the_key_came_from() {
        assert_eq!(press(egui::Key::ArrowDown, None, 10), Some(0));
        assert_eq!(press(egui::Key::ArrowUp, None, 10), Some(9));
    }

    /// An empty list has nowhere to go, and asking must not underflow to
    /// `usize::MAX` -- which as a row index is a panic in the row callback.
    #[test]
    fn an_empty_list_goes_nowhere() {
        for key in [
            egui::Key::ArrowDown,
            egui::Key::ArrowUp,
            egui::Key::Home,
            egui::Key::End,
        ] {
            assert_eq!(press(key, None, 0), None, "{key:?}");
            assert_eq!(press(key, Some(0), 0), None, "{key:?}");
        }
    }

    /// Keys this list does not claim are left for whoever else wants them.
    #[test]
    fn ignores_everything_else() {
        for key in [egui::Key::ArrowLeft, egui::Key::Delete, egui::Key::F5] {
            assert_eq!(press(key, Some(3), 10), None, "{key:?}");
        }
    }
}
