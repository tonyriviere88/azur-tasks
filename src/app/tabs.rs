//! Which tabs there are, and everything that turns on which one is active.
//!
//! `Tab` is an enum rather than a trait object on purpose: the compiler then makes
//! every `match` on it exhaustive, so adding a tab is a list of compile errors
//! naming the decisions it still has to make. That is the property worth keeping —
//! a `Vec<Box<dyn TabView>>` would collapse those matches into one loop and take
//! the check away with them.
//!
//! What *is* a property of the tab lives here as a method, though. "Does this tab
//! list processes?" was written out as `matches!(self.tab, Processes | Details)` in
//! five different places in the shell — the inspector's applicability, the New task
//! button, the Ctrl-freeze indicator, twice more — and each of those was a copy of
//! one fact that a third process-listing tab would have had to find.

use super::App;
use crate::sys::snapshot::{Proc, Snapshot};
use crate::ui::icons;
use crate::ui::{details, processes, services, startup};
use azur_egui_theme as azur;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Summary,
    Processes,
    Performance,
    Sensors,
    Details,
    Services,
    Startup,
}

impl Tab {
    pub(super) const ALL: [Tab; 7] = [
        Tab::Summary,
        Tab::Processes,
        Tab::Performance,
        Tab::Sensors,
        Tab::Details,
        Tab::Services,
        Tab::Startup,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Summary => "Summary",
            Self::Processes => "Processes",
            Self::Performance => "Performance",
            Self::Sensors => "Sensors",
            Self::Details => "Details",
            Self::Services => "Services",
            Self::Startup => "Startup apps",
        }
    }

    /// The rail glyph, as an icon a component can take.
    pub(super) fn icon(self) -> azur::icons::Icon<'static> {
        match self {
            Self::Summary => &icons::icon::summary,
            Self::Processes => &icons::icon::processes,
            Self::Performance => &icons::icon::performance,
            Self::Sensors => &icons::icon::gauge,
            Self::Details => &icons::icon::details,
            Self::Services => &icons::icon::services,
            Self::Startup => &icons::icon::startup,
        }
    }

    /// Whether this tab lists processes.
    ///
    /// Three things in the shell turn on it: whether the New task button is there,
    /// whether the inspector's latch is offered, and whether the inspector panel
    /// opens. All three used to spell out `matches!(self.tab, Processes | Details)`,
    /// which is one fact copied three times — a third process-listing tab would have
    /// had to find every copy, and the copies could disagree without anything
    /// complaining.
    ///
    /// Holding the row order used to be one of them and is now
    /// [`Self::holds_order_with_ctrl`]: the Summary tab holds an order without being
    /// a process list, so the two questions came apart.
    pub(super) fn is_process_list(self) -> bool {
        matches!(self, Self::Processes | Self::Details)
    }

    /// Whether holding Ctrl pins this tab's rows in the order they are already in.
    ///
    /// Every page whose rows are ordered by a live reading and can be clicked: the
    /// two table tabs, and the Summary tab's rankings. It is what says the row under
    /// the pointer will still be there when the button goes down, and it is what the
    /// status bar reports — so it has to be one answer rather than a copy per tab.
    ///
    /// The Services and Startup tabs are absent on purpose: their rows are ordered by
    /// name and by a state that changes when the reader changes it, so they never
    /// move underneath anybody.
    pub(super) fn holds_order_with_ctrl(self) -> bool {
        matches!(self, Self::Summary | Self::Processes | Self::Details)
    }

    /// Tabs whose content is filtered by the search box.
    pub(super) fn searchable(self) -> bool {
        !matches!(self, Self::Summary | Self::Performance | Self::Sensors)
    }

    /// Whether this tab's content is a table that runs to the window's edges.
    ///
    /// A table paints its own panel and is welded to the strips above and below it
    /// by seams, so the central panel gives it no margin at all. The three card
    /// tabs want the opposite: cards float on the canvas, and the canvas has to be
    /// visible around them.
    ///
    /// The same tabs as [`Self::searchable`] today, and a different question -- one
    /// is about what a filter would narrow, the other about what the content is
    /// shaped like. A tab could answer them differently.
    pub(super) fn tabular(self) -> bool {
        !matches!(self, Self::Summary | Self::Performance | Self::Sensors)
    }
}

/// Every table whose column layout is persisted, paired with the key it lives under.
///
/// Written once because there are two sides to it — restored before the window opens,
/// saved while it is closing — and a table named on one side but not the other is a
/// set of column widths that silently stops being remembered. Handed the four tabs
/// rather than `&mut self` so the caller can prove to the borrow checker that
/// `Settings` is not among them.
pub(super) fn persisted_tables<'a>(
    processes: &'a mut processes::ProcessesTab,
    details: &'a mut details::DetailsTab,
    services: &'a mut services::ServicesTab,
    startup: &'a mut startup::StartupTab,
) -> [(&'static str, &'a mut crate::ui::table::Table); 4] {
    [
        (processes.list.key(), &mut processes.list.table),
        (details.list.key(), &mut details.list.table),
        (services.list.key(), &mut services.list.table),
        (startup.list.key(), &mut startup.list.table),
    ]
}

impl App {
    /// Drop the selection on whichever tab has one.
    ///
    /// Performance's `selected` is not one of these: it names the resource whose graph
    /// is on screen, which is a view and not a row, and a tab showing nothing would be
    /// a tab with a hole in it.
    pub(super) fn clear_selection(&mut self) {
        match self.tab {
            Tab::Processes => self.processes.clear_selection(),
            Tab::Details => self.details.clear_selection(),
            Tab::Services => self.services.clear_selection(),
            Tab::Startup => self.startup.clear_selection(),
            // Nothing on any of the three holds a selection: two are read-outs, and
            // the summary's rankings are a ranking rather than a list to pick from.
            Tab::Summary | Tab::Performance | Tab::Sensors => {}
        }
    }

    /// The process the inspector is about: whatever the active list has selected.
    ///
    /// The whole row, not just its pid: the panel is docked *under* the list and has
    /// to say which row it belongs to -- with the keyboard moving the selection, the
    /// two can otherwise be read as unrelated -- and its first tab graphs the same
    /// CPU and memory figures the row itself is showing.
    ///
    /// Borrowed from the snapshot rather than copied out of it. The snapshot is held
    /// for the whole frame and outlives the panel that reads it, so nothing here has
    /// to be cloned to be looked at.
    pub(super) fn inspected<'s>(&self, snap: &'s Snapshot) -> Option<&'s Proc> {
        let key = match self.tab {
            Tab::Processes => self.processes.list.selected,
            Tab::Details => self.details.list.selected,
            _ => None,
        };
        key.and_then(|k| snap.find_by_key(k))
            .map(|i| &snap.procs[i])
    }
}
