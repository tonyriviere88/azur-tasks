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
use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Painter, Pos2, Rect, Response, Sense, Stroke, Ui,
    Vec2,
};

pub const HEADER_H: f32 = 30.0;

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
            row_h: 24.0,
            dragging_edge: None,
            scroll_to_row: None,
        }
    }

    pub fn visible_cols(&self) -> impl Iterator<Item = (usize, &Column)> {
        self.cols.iter().enumerate().filter(|(_, c)| c.visible)
    }

    /// Slot of the visible column that absorbs leftover width, if any.
    fn stretch_slot(&self) -> Option<usize> {
        self.visible_cols().position(|(_, c)| c.stretch)
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
        let full = ui.available_rect_before_wrap();
        let rect = Rect::from_min_size(full.min, Vec2::new(full.width(), HEADER_H));
        ui.allocate_rect(rect, Sense::hover());

        let painter = ui.painter().clone();
        // The strip spans the full width so it reaches the edge, but the columns
        // are laid out over the body's narrower content span.
        painter.rect_filled(rect, CornerRadius::ZERO, t.surface);
        // Baseline under the header, so the body reads as a separate surface.
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, t.border),
        );

        let content = Rect::from_min_max(
            rect.min,
            Pos2::new(rect.right() - Self::bar_width(ui), rect.bottom()),
        );
        let mut cells = Vec::with_capacity(self.cols.len());
        self.layout(content, &mut cells);

        let mut sort_changed = false;
        let mut cols_changed = false;
        let mut pending_width: Option<(usize, f32)> = None;

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
                painter.rect_filled(cell, CornerRadius::ZERO, t.surface_hi);
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
            let menu = resp.context_menu(|ui| {
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
                    t.accent,
                );
            }

            let color = if is_sorted { t.accent } else { t.text_dim };
            paint_cell_text(
                &painter,
                text_rect,
                col.title,
                &t.fonts.small,
                color,
                col.align,
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
                        Stroke::new(1.0, t.accent),
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
                    Stroke::new(1.0, t.border_soft),
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

        let mut hovered_row = None;
        let mut clicked_row = None;
        let mut double_clicked_row = None;
        let mut context_row = None;

        // Scoped so the scrollbar restyling below cannot leak into whatever the
        // caller draws next.
        let styled = ui.scope(|ui| {
        // egui paints the handle with `fg_stroke.color` when
        // `ScrollStyle::foreground_color` is set, and this theme's `fg_stroke`
        // is the body text colour -- which put a near-white bar down the side of
        // every table. Switch to the fill colours and give it a dark groove.
        ui.spacing_mut().scroll.foreground_color = false;
        {
            let w = &mut ui.visuals_mut().widgets;
            w.inactive.bg_fill = t.scroll_handle;
            w.hovered.bg_fill = t.scroll_handle_hover;
            w.active.bg_fill = t.accent;
        }
        ui.visuals_mut().extreme_bg_color = t.scroll_track;

        let scroll = egui::ScrollArea::vertical()
            .id_salt(id_base.with("scroll"))
            // Always reserved. The header subtracts exactly this width to line
            // its columns up with the body's, and a bar that came and went as
            // the row count crossed the overflow point would shift every column
            // by 14px underneath a stationary heading.
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .auto_shrink([false, false]);

        let out = scroll.show_viewport(ui, |ui, viewport| {
            let width = ui.available_width();
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(width, (n_rows as f32 * row_h).max(1.0)),
                Sense::click(),
            );

            if let Some(row) = bring_into_view {
                if row < n_rows {
                    // Computed rather than captured while painting: the target
                    // row may currently be scrolled out of view and therefore
                    // never painted at all.
                    let y = rect.top() + row as f32 * row_h;
                    ui.scroll_to_rect(
                        Rect::from_min_size(
                            Pos2::new(rect.left(), y),
                            Vec2::new(1.0, row_h),
                        ),
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
            let painter = ui.painter().clone();

            for row in first..last {
                let y = rect.top() + row as f32 * row_h;
                let row_rect =
                    Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(width, row_h));

                let is_selected = selected == Some(row);
                let is_hovered = hovered_row == Some(row);

                // Row background: selection wins, then hover, then the zebra.
                if is_selected {
                    painter.rect_filled(row_rect, CornerRadius::ZERO, t.row_selected);
                } else if is_hovered {
                    painter.rect_filled(row_rect, CornerRadius::ZERO, t.row_hover);
                } else if row % 2 == 1 {
                    painter.rect_filled(row_rect, CornerRadius::ZERO, t.row_alt);
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
                };
                paint_row(&mut rp);
            }

            response
        });

        out.inner
        });

        TableOutput {
            clicked_row,
            double_clicked_row,
            context_row,
            response: styled.inner,
        }
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

    /// Draw a cell's text, clipped to the column and aligned.
    pub fn text(&self, slot: usize, s: &str, color: Color32, align: Align) {
        if s.is_empty() {
            return;
        }
        let cell = self.cell(slot);
        paint_cell_text(
            &self.painter.with_clip_rect(cell),
            cell.shrink2(Vec2::new(8.0, 0.0)),
            s,
            &self.theme.fonts.body,
            color,
            align,
        );
    }

    pub fn text_font(&self, slot: usize, s: &str, color: Color32, align: Align, font: &FontId) {
        if s.is_empty() {
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
        );
    }

    /// Text indented from the left, for tree rows.
    pub fn text_indent(&self, slot: usize, indent: f32, s: &str, color: Color32) {
        if s.is_empty() {
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
        );
    }

    /// Task Manager's warm load tint behind a numeric cell.
    pub fn heat(&self, slot: usize, t01: f32) {
        // Selection already colours the row; tinting on top muddies both.
        if self.selected {
            return;
        }
        let c = self.theme.heat(t01);
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
    pub fn pill(&self, slot: usize, s: &str, fg: Color32, bg: Color32) {
        let cell = self.cell(slot);
        let font = &self.theme.fonts.small;
        let galley = self
            .painter
            .layout_no_wrap(s.to_owned(), font.clone(), fg);
        let pad = Vec2::new(7.0, 2.0);
        let size = galley.size() + pad * 2.0;
        let r = Rect::from_min_size(
            Pos2::new(cell.left() + 8.0, cell.center().y - size.y * 0.5),
            size,
        );
        let p = self.painter.with_clip_rect(cell);
        p.rect_filled(r, CornerRadius::same(3), bg);
        p.galley(r.min + pad, galley, fg);
    }
}

/// Checkbox list of every column. Returns true when one was toggled.
///
/// Takes the column slice rather than `&mut Table` so it can be called from
/// inside `header`, which already holds other parts of `self`.
fn column_picker(ui: &mut Ui, cols: &mut [Column]) -> bool {
    let mut changed = false;
    egui::ScrollArea::vertical()
        .max_height(440.0)
        .show(ui, |ui| {
            // Refuse to hide the last visible column: the picker hangs off the
            // header cells, so an empty grid would have no way back.
            let shown = cols.iter().filter(|c| c.visible).count();
            for c in cols.iter_mut() {
                let mut on = c.visible;
                let can_toggle = !c.visible || shown > 1;
                let resp = ui.add_enabled(can_toggle, egui::Checkbox::new(&mut on, c.title));
                if resp.changed() {
                    c.visible = on;
                    changed = true;
                }
            }
        });
    changed
}

/// Paint text inside `rect`, aligned, without wrapping.
fn paint_cell_text(
    painter: &Painter,
    rect: Rect,
    s: &str,
    font: &FontId,
    color: Color32,
    align: Align,
) {
    if rect.width() <= 1.0 {
        return;
    }
    let (pos, anchor) = match align {
        Align::Left => (rect.left_center(), Align2::LEFT_CENTER),
        Align::Right => (rect.right_center(), Align2::RIGHT_CENTER),
    };
    painter.text(pos, anchor, s, font.clone(), color);
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
