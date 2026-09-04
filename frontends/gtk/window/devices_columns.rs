//! The device track view's columns.
//!
//! Split from [`super::devices_page`] (plan step 6, fifth cut) — the same
//! split [`super::ml_columns`] is at window level, and it builds on it: the
//! shared `ALL_COLUMNS` table drives both this view and the Files view, so a
//! column the user adds, reorders or resizes in one shows up the same in the
//! other.
//!
//! Two columns are device-only and defined here rather than in that table:
//!
//! - **Playlist order**, at the front, shown only while a playlist chip is
//!   active and then made the default sort — like the editor's position
//!   column.
//! - **Synced from**, the library file each device file was copied from,
//!   read out of the page's pair map.
//!
//! Every cell also carries the right-click gesture that opens the row menu,
//! which is why the (still empty) `dev_row_menu_holder` is threaded in here:
//! the factories are built long before [`super::devices_menu`] fills it.

use gtk4::prelude::*;
use gtk4::{glib, Align, ColumnView, ColumnViewColumn, CustomSorter, Label,
    MultiSelection, SignalListItemFactory, SortListModel};
use std::cell::RefCell;
use std::rc::Rc;

use super::{
    attach_cell_context_menu, gtk_safe, ml_cell_text, ml_sort_key, AppState, ArtworkCells,
    MlColumnDef, ALL_COLUMNS,
};

/// The view and models the columns attach to.
pub(super) struct ColumnUi<'a> {
    pub dev_col_view: &'a ColumnView,
    pub dev_selection: &'a MultiSelection,
    pub dev_sort_model: &'a SortListModel,
    /// Device file path → the library file it was copied from.
    pub dev_pair_map: &'a Rc<RefCell<std::collections::HashMap<String, String>>>,
    /// Filled later by [`super::devices_menu`]; each cell's gesture calls it.
    pub dev_row_menu_holder: &'a Rc<RefCell<Option<Rc<dyn Fn(f64, f64)>>>>,
}

/// What the rest of the page needs back.
pub(super) struct Columns {
    /// The playlist-order column, shown only under an active playlist chip.
    pub dev_pos_col: ColumnViewColumn,
    /// Every column by id, for re-applying the shared column config.
    pub dev_named_cols: Rc<Vec<(String, ColumnViewColumn)>>,
}

/// Build and attach the device track view's columns.
pub(super) fn build(state: &Rc<RefCell<AppState>>, ui: ColumnUi<'_>) -> Columns {
    // Local names for what this takes from its caller, so the body below reads
    // as it did inside `devices_page::build`.
    let state = state.clone();
    let dev_col_view = ui.dev_col_view.clone();
    let dev_selection = ui.dev_selection.clone();
    let dev_sort_model = ui.dev_sort_model.clone();
    let dev_pair_map = ui.dev_pair_map.clone();
    let dev_row_menu_holder = ui.dev_row_menu_holder.clone();
    // The artwork column's cell, shared with the Files page and the playlist
    // editor so all three render the same thumbnail (see `ArtworkCells`).
    let artwork_cells = Rc::new(ArtworkCells::new());

    // Playlist-order column (front): shown only while a playlist filter is
    // active, then made the default sort — like the editor's position column.
    let dev_pos_col = {
        let sel_ctx = dev_selection.clone();
        let anchor_ctx = dev_col_view.clone();
        let holder_ctx = dev_row_menu_holder.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::End)
                .xalign(1.0)
                .margin_start(6)
                .margin_end(6)
                .css_classes(["pl-duration"])
                .build();
            li.set_child(Some(&lbl));
            // Right-click has to be handled per cell: ColumnView has no
            // `row_at_y`, so a ScrolledWindow-level gesture cannot tell which
            // row it hit and the menu did nothing until a left-click had
            // already selected something (2026-08-10).
            attach_cell_context_menu(
                li,
                lbl.upcast_ref(),
                &sel_ctx,
                anchor_ctx.upcast_ref(),
                {
                    let holder = holder_ctx.clone();
                    move |x, y| {
                        let f = holder.borrow().clone();
                        if let Some(f) = f {
                            f(x, y);
                        }
                    }
                },
            );
        });
        // The playlist view holds entries in order (no sort), so the row's
        // position in the model is its 1-based playlist position. Each duplicate
        // entry is its own row and gets its own number.
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                return;
            };
            lbl.set_text(&(li.position() + 1).to_string());
        });
        let col = ColumnViewColumn::new(Some("#"), Some(factory));
        col.set_fixed_width(48);
        col.set_visible(false);
        dev_col_view.append_column(&col);
        col
    };

    // "Synced from" column (device view only): the library file each device file
    // was copied from. Lets the user confirm at a glance which computer file a
    // sync keeps in step, instead of guessing among same-named files. Reads the
    // live per-device pair map keyed by on-device path.
    {
        let pair_map = dev_pair_map.clone();
        let sel_ctx = dev_selection.clone();
        let anchor_ctx = dev_col_view.clone();
        let holder_ctx = dev_row_menu_holder.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                // Fills the cell for the same reason the track columns do: a
                // controller only sees pointer events inside its widget's
                // allocation, and `halign(Start)` without `hexpand` allocates
                // the label its natural width — which `ellipsize` drives down
                // to about one character. The right-click gesture below hangs
                // off this label, so a shrunken one leaves most of the cell
                // dead to the row menu while still LOOKING correct, because
                // GTK draws the text either way.
                .hexpand(true)
                .halign(Align::Fill)
                .xalign(0.0)
                .margin_start(6)
                .margin_end(6)
                .ellipsize(gtk4::pango::EllipsizeMode::Middle)
                .css_classes(["status-label"])
                .build();
            li.set_child(Some(&lbl));
            // Right-click has to be handled per cell: ColumnView has no
            // `row_at_y`, so a ScrolledWindow-level gesture cannot tell which
            // row it hit and the menu did nothing until a left-click had
            // already selected something (2026-08-10).
            attach_cell_context_menu(
                li,
                lbl.upcast_ref(),
                &sel_ctx,
                anchor_ctx.upcast_ref(),
                {
                    let holder = holder_ctx.clone();
                    move |x, y| {
                        let f = holder.borrow().clone();
                        if let Some(f) = f {
                            f(x, y);
                        }
                    }
                },
            );
        });
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                return;
            };
            let Some(item) = li.item() else { return };
            let Some(boxed) = item.downcast_ref::<glib::BoxedAnyObject>() else {
                return;
            };
            let path = boxed.borrow::<sparkamp::media_library::LibTrack>().path.clone();
            match pair_map.borrow().get(&path) {
                Some(libp) => {
                    let base = std::path::Path::new(libp)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(libp);
                    lbl.set_text(&gtk_safe(base));
                    lbl.set_tooltip_text(Some(&gtk_safe(libp)));
                }
                None => {
                    lbl.set_text("—");
                    lbl.set_tooltip_text(Some("Not synced from this computer"));
                }
            }
        });
        let col = ColumnViewColumn::new(Some("Synced from"), Some(factory));
        col.set_fixed_width(220);
        col.set_resizable(true);
        dev_col_view.append_column(&col);
    }

    let mut dev_named_cols: Vec<(String, ColumnViewColumn)> = Vec::new();
    {
        // Columns that are library bookkeeping, not ID3 tags — irrelevant for a
        // device, so never shown here even if visible in the files view.
        const DEVICE_HIDDEN_COLS: &[&str] = &["play_count", "last_played", "last_scanned"];
        let visible_ids: Vec<String> =
            state.borrow().config.media_library.visible_columns.clone();
        let widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();
        let order = state.borrow().config.media_library.ml_file_col_order.clone();
        // Build columns in the saved order (unknown/leftover ids appended).
        let ordered: Vec<&MlColumnDef> = {
            let mut v: Vec<&MlColumnDef> = Vec::new();
            for id in &order {
                if let Some(c) = ALL_COLUMNS.iter().find(|c| &c.id == id) {
                    v.push(c);
                }
            }
            for c in ALL_COLUMNS.iter() {
                if !order.iter().any(|id| id == c.id) {
                    v.push(c);
                }
            }
            v
        };
        for c in ordered {
            if DEVICE_HIDDEN_COLS.contains(&c.id) {
                continue;
            }
            let id_str = c.id.to_string();
            let is_art = c.id == "artwork_path";
            let setup_cells = artwork_cells.clone();
            let bind_cells = artwork_cells.clone();
            let sel_ctx = dev_selection.clone();
            let anchor_ctx = dev_col_view.clone();
            let holder_ctx = dev_row_menu_holder.clone();
            let drag_sel_ctx = dev_selection.clone();
            let factory = SignalListItemFactory::new();
            factory.connect_setup(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() {
                    return;
                }
                // Artwork column gets the shared thumbnail cell, every other
                // column a plain label.
                let child: gtk4::Widget = if is_art {
                    setup_cells.setup().upcast::<gtk4::Widget>()
                } else {
                    Label::builder()
                        // Fills the cell, and that is load-bearing rather than
                        // cosmetic: a label with `halign(Start)` and no
                        // `hexpand` is allocated only its natural width, which
                        // `ellipsize(End)` collapses to almost nothing. The
                        // per-cell DragSource below hangs off this widget, so
                        // a sliver-sized label received no pointer events at
                        // all and dragging a device row did nothing — the
                        // prepare closure was never reached despite being
                        // attached to every cell. The Files table sizes its
                        // label exactly this way and its drag has always
                        // worked.
                        .hexpand(true)
                        .halign(Align::Fill)
                        .xalign(0.0)
                        .margin_start(6)
                        .margin_end(6)
                        .ellipsize(gtk4::pango::EllipsizeMode::End)
                        .css_classes(["ml-col-label"])
                        .build()
                        .upcast::<gtk4::Widget>()
                };
                // Per-cell DragSource — collects every currently-selected
                // device row (or just the row under the pointer, with
                // nothing selected) so it can be dropped on the active
                // playlist too. Mirrors the Files table's per-cell source
                // (files.rs:484).
                {
                    let ds_sel = drag_sel_ctx.clone();
                    let ds_li = li.clone();
                    super::ml_drag::attach_uri_drag(&child, move || {
                        let mut paths: Vec<String> = Vec::new();
                        for i in 0..ds_sel.n_items() {
                            if ds_sel.is_selected(i)
                                && let Some(obj) = ds_sel
                                    .item(i)
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            {
                                let t = obj.borrow::<sparkamp::media_library::LibTrack>();
                                paths.push(t.path.clone());
                            }
                        }
                        if paths.is_empty()
                            && let Some(obj) = ds_li
                                .item()
                                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let t = obj.borrow::<sparkamp::media_library::LibTrack>();
                            paths.push(t.path.clone());
                        }
                        paths
                    });
                }

                // Parented only now, AFTER its controllers are installed.
                //
                // The Files table (files.rs) has always done it in this order
                // and its per-cell drag works; this view set the child first
                // and its drag never started — the prepare closure was never
                // reached at all, though the source was demonstrably attached
                // to every cell. Setting the child first is the one structural
                // difference between the two, so the order is load-bearing.
                li.set_child(Some(&child));

                // Right-click has to be handled per cell: ColumnView has no
                // `row_at_y`, so a ScrolledWindow-level gesture cannot tell which
                // row it hit and the menu did nothing until a left-click had
                // already selected something (2026-08-10).
                attach_cell_context_menu(
                    li,
                    &child,
                    &sel_ctx,
                    anchor_ctx.upcast_ref(),
                    {
                        let holder = holder_ctx.clone();
                        move |x, y| {
                            let f = holder.borrow().clone();
                            if let Some(f) = f {
                                f(x, y);
                            }
                        }
                    },
                );
            });
            let bind_id = id_str.clone();
            let bind_state = state.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };
                let t = boxed.borrow::<sparkamp::media_library::LibTrack>();
                // F12.2: read live so a Settings toggle applies to already
                // -bound cells on the next rebind, not just at window
                // construction (the ML window is a singleton — see
                // rebuild_ml_callback in player.rs).
                let artist_as_album_artist =
                    bind_state.borrow().config.media_library.artist_as_album_artist;
                // Without this a screen reader reads every cell in the row
                // in sequence, empty ones included, so an untagged file
                // announced as "song, , ". One sentence per row is what the
                // HIG's list guidance asks for — but `Property::Label`
                // *replaces* a cell's own accessible name rather than adding
                // to it, and this closure runs once per visible column.
                // Setting it on every column made every cell repeat the same
                // sentence and cost non-title cells (Length, Size-style
                // columns) their own content, so exactly one column carries
                // it: "title", the row's identifying field — in
                // `default_ml_visible`/`default_id3_visible`, so this only
                // goes quiet if a user deliberately hides it.
                // `spoken_row_summary` lives in `files.rs`: the Files,
                // Playlist-editor and Device views all bind the same
                // `LibTrack` fields, so one function serves all three rather
                // than three copies of the same `match`.
                if bind_id == "title" {
                    let spoken = super::files::spoken_row_summary(
                        t.title.as_deref().unwrap_or(&t.filename),
                        t.artist.as_deref().unwrap_or(""),
                        t.album.as_deref().unwrap_or(""),
                    );
                    // `ListItem` itself carries no Accessible implementation
                    // (it isn't a widget) — the accessible tree is built
                    // from the actual cell widget `connect_setup` put in
                    // `li.child()`.
                    if let Some(cell) = li.child() {
                        cell.update_property(&[gtk4::accessible::Property::Label(&spoken)]);
                    }
                }
                if is_art {
                    bind_cells.bind(li, t.artwork_path.as_deref(), |li| {
                        li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .and_then(|b| {
                                b.borrow::<sparkamp::media_library::LibTrack>()
                                    .artwork_path
                                    .clone()
                            })
                    });
                    return;
                }
                let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                    return;
                };
                lbl.set_text(&gtk_safe(&ml_cell_text(&t, &bind_id, artist_as_album_artist)));
            });
            let col = ColumnViewColumn::new(Some(c.header), Some(factory));
            col.set_resizable(true);
            if c.expand {
                col.set_expand(true);
            }
            col.set_visible(visible_ids.contains(&id_str));
            if let Some(&w) = widths.get(&id_str) {
                if w > 0 {
                    col.set_fixed_width(w);
                }
            }
            let sort_id = id_str.clone();
            let sorter = CustomSorter::new(move |a, b| {
                let ka = a
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<sparkamp::media_library::LibTrack>(), &sort_id))
                    .unwrap_or_default();
                let kb = b
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<sparkamp::media_library::LibTrack>(), &sort_id))
                    .unwrap_or_default();
                ka.cmp(&kb).into()
            });
            col.set_sorter(Some(&sorter));
            dev_named_cols.push((id_str.clone(), col.clone()));
            dev_col_view.append_column(&col);
        }
        // Header clicks drive the sort model.
        dev_sort_model.set_sorter(dev_col_view.sorter().as_ref());
    }
    let dev_named_cols = Rc::new(dev_named_cols);

    Columns {
        dev_pos_col,
        dev_named_cols,
    }
}
