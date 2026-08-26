//! The Media Library's "Files" page — the whole-library track table.
//!
//! Child module of [`super`] (window.rs), extracted from
//! `open_media_library_window` by plan step 4. It owns the `ColumnView` and
//! its columns, the search row, the status bar, and the row context menu
//! (including Send to ▸ Device / Disc Drive).
//!
//! It is also the Albums gallery's drill-down target: when `ctx.album_filter`
//! is `Some((album, album_artist))` the table shows that album's tracks
//! instead of the full library, and `ctx.btn_album_back` is the way back.
//!
//! The page reads its column order and widths back out through
//! `ctx.col_view_holder` / `ctx.all_cols_holder`, which the window's
//! close-request consults when saving config — the widgets are built here but
//! outlive this function only through those holders.

use gtk4::prelude::*;
use gtk4::{
    gdk, gio, glib, Align, Box as GtkBox, Button, ColumnView, ColumnViewColumn,
    CustomSorter, Entry, EventControllerKey, Label, MultiSelection, Orientation,
    PolicyType, ScrolledWindow, SignalListItemFactory, SortListModel,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// Sibling modules this page drives.
use super::watch;
// The sidebar this page registers its own row-selected routing on.
use super::sidebar::Sidebar;
// Everything else is private to the parent module, which a child may still
// use. The long list is what "the Files page" actually depends on: the shared
// column model (ALL_COLUMNS / ml_cell_text / ml_sort_key), the row actions
// that open other windows, and the scan/ReplayGain progress seams.
use super::{
    analyze_job, build_send_to_menu, cancel_ml_scan, cancel_rg_job, complete_ml_scan,
    context_popover, format_last_played, gtk_safe, ml_cell_text, ml_sort_key, ml_status_bar,
    open_customize_columns_dialog, start_ml_scan, sync_rg_ui, truncate_display,
    update_ml_scan_progress, view_or_search_lyrics, ArtworkCells, ColumnCustomizerMode, LyricsMode,
    MlCtx,
    ScanType, SendToActions, ALL_COLUMNS, ML_SEARCH_ENTRY_NAME,
};

/// What the Files view's leading status column shows for one row.
///
/// Split out of the bind handler because working it out costs two blocking
/// syscalls — a `stat` for the mtime compare and an `open(O_WRONLY)` for the
/// read-only probe — and a `SignalListItemFactory` bind runs for **every row
/// on every scroll**, on the GTK main thread. Measured at ~0.5 ms per row on a
/// mounted volume, roughly thirty visible rows dropped frames continuously
/// (2026-08-09). The probe now happens off-thread, once per path per session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FileStatus {
    /// The file is no longer on disk. Decided before every other check: a
    /// path that cannot be stat'd has nothing else worth asking about.
    Missing,
    /// No metadata was ever extracted — a pure DB answer, never probed.
    Unscanned,
    /// The file's mtime is newer than its last scan.
    Changed,
    /// The file cannot be opened for writing.
    ReadOnly,
    /// Scanned, unchanged, writable — nothing to show.
    Clean,
}

impl FileStatus {
    fn glyph(self) -> &'static str {
        match self {
            // Same marker the playlist uses for the same fact.
            FileStatus::Missing => "⚠",
            FileStatus::Unscanned => "❓",
            FileStatus::Changed => "🔄",
            FileStatus::ReadOnly => "🔒",
            FileStatus::Clean => "",
        }
    }

    fn tooltip(self) -> Option<&'static str> {
        match self {
            FileStatus::Missing => Some("File is missing — it has been moved, renamed, or deleted"),
            FileStatus::Unscanned => Some("Not scanned yet — metadata loads on the next scan"),
            FileStatus::Changed => {
                Some("File changed since last scan — rescan to refresh its metadata")
            }
            FileStatus::ReadOnly => Some("Read-only file"),
            FileStatus::Clean => None,
        }
    }

    fn apply(self, lbl: &Label) {
        lbl.set_label(self.glyph());
        lbl.set_tooltip_text(self.tooltip());
    }
}

/// The paths of every currently selected row, in no particular order.
///
/// Paths rather than positions: a rebuild re-sorts as well as re-fills, so a
/// position means nothing across it, and a path identifies the same track
/// wherever it lands.
fn selected_paths_of(sel: &MultiSelection) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let bitset = sel.selection();
    for i in 0..bitset.size() {
        let pos = bitset.nth(i as u32);
        if let Some(obj) = sel.item(pos) {
            if let Ok(b) = obj.downcast::<glib::BoxedAnyObject>() {
                out.insert(b.borrow::<crate::media_library::LibTrack>().path.clone());
            }
        }
    }
    out
}

/// Re-select the rows holding `paths`, wherever the rebuild put them.
///
/// Tracks that no longer exist are simply not re-selected, which is the right
/// answer: a file that vanished mid-selection cannot be acted on anyway.
fn restore_selection(sel: &MultiSelection, paths: &std::collections::HashSet<String>) {
    if paths.is_empty() {
        return;
    }
    let mask = gtk4::Bitset::new_empty();
    for pos in 0..sel.n_items() {
        let Some(obj) = sel.item(pos) else { continue };
        let Ok(b) = obj.downcast::<glib::BoxedAnyObject>() else {
            continue;
        };
        let hit = paths.contains(&b.borrow::<crate::media_library::LibTrack>().path);
        if hit {
            mask.add(pos);
        }
    }
    if mask.size() > 0 {
        // One call, so listeners see a single selection change rather than one
        // per row — the status bar recomputes on every notification.
        sel.set_selection(&mask, &gtk4::Bitset::new_range(0, sel.n_items()));
    }
}

/// Is this recycled row still bound to the track we started work for?
///
/// `ColumnView` reuses one widget for many rows, so anything asynchronous has
/// to re-check before it acts: by the time it resumes, the row it was launched
/// from may be showing a completely different track.
fn row_shows_path(li: &gtk4::ListItem, path: &str) -> bool {
    li.item()
        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
        .map(|b| b.borrow::<crate::media_library::LibTrack>().path == path)
        .unwrap_or(false)
}

/// The two filesystem probes behind [`FileStatus`]. Touches no GTK state, so
/// it is safe to run on a worker thread — which is the whole point.
fn probe_file_status(
    path: &str,
    unscanned: bool,
    last_scanned: Option<&str>,
    stored_mtime: Option<&str>,
) -> FileStatus {
    // Missing first. `needs_metadata_scan` answers `true` for a path it cannot
    // stat, because it cannot tell "modified since the scan" from "no longer
    // there" — so without this a deleted track reported Changed and the view
    // advised a rescan, while the playlist marked the same file ⚠.
    if !std::path::Path::new(path).exists() {
        return FileStatus::Missing;
    }
    // A row whose metadata was never read has nothing further to say: the ❓
    // stands until a scan fills it in. Only its disappearance, handled above,
    // is worth overriding that with.
    if unscanned {
        return FileStatus::Unscanned;
    }
    if crate::media_library::MediaLibrary::needs_metadata_scan(path, last_scanned, stored_mtime) {
        return FileStatus::Changed;
    }
    if crate::media_library::is_read_only(std::path::Path::new(path)) {
        return FileStatus::ReadOnly;
    }
    FileStatus::Clean
}

/// How long a resolved row status is trusted before it is probed again.
///
/// The cache exists to stop a scroll re-probing the same rows hundreds of
/// times, and a scroll storm lasts a second or two — so a short life absorbs
/// all of it. Caching for the whole session, which is what this used to do,
/// meant a permission change made outside Sparkamp stayed invisible until a
/// scan happened to run: the ID3 editor reported the file read-only while the
/// Files view went on showing it as clean.
const GLYPH_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Cache of resolved row statuses, keyed by track path, plus the set of paths
/// a probe is already running for so a scroll cannot queue the same work
/// hundreds of times. Entries expire after [`GLYPH_TTL`]; a finished scan
/// clears the whole map, since that is when many answers change at once.
type GlyphCache = Rc<RefCell<std::collections::HashMap<String, (FileStatus, std::time::Instant)>>>;
type GlyphInflight = Rc<RefCell<std::collections::HashSet<String>>>;

/// Build the Files page and attach it to `ctx.stack` under the name `"files"`.
pub(super) fn build(ctx: &MlCtx, sb: &Sidebar) {
    // Local names for what this page uses from its context, so the body below
    // reads as it did inside `open_media_library_window`. Same device step 1
    // used for MlHost's fields: cloning an `Rc` is an integer increment, and
    // rewriting several hundred capture sites would bury a move in an
    // unreviewable diff.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let set_track = ctx.host.set_track.clone();
    let current_drives = ctx.host.current_drives.clone();
    let current_devices = ctx.host.current_devices.clone();
    let win = ctx.win.clone();
    let stack = ctx.stack.clone();
    let album_filter = ctx.album_filter.clone();
    let btn_album_back = ctx.btn_album_back.clone();
    let col_view_holder = ctx.col_view_holder.clone();
    let all_cols_holder = ctx.all_cols_holder.clone();

        let files_vbox = GtkBox::new(Orientation::Vertical, 4);

        let search_entry = Entry::new();
        search_entry.set_placeholder_text(Some("Search artist, title, album…"));
        search_entry.set_hexpand(true);
        // Marks the entry Ctrl+F should focus when this page is the visible
        // one — see the widget-name walk in media_library.rs.
        search_entry.set_widget_name(ML_SEARCH_ENTRY_NAME);
        // F12.1: restore this view's last search query if the feature is on.
        // rebuild_files() (below) reads search_entry.text() for its initial
        // fill, so this must happen before that call.
        if state.borrow().config.media_library.remember_search {
            let last =
                state.borrow().config.media_library.last_search.get("files").cloned();
            if let Some(last) = last {
                search_entry.set_text(&last);
            }
        }

        let search_clear_btn = Button::with_label("✕");
        search_clear_btn.add_css_class("pl-btn");
        {
            let e = search_entry.clone();
            search_clear_btn.connect_clicked(move |_| {
                e.set_text("");
            });
        }

        let search_row = GtkBox::new(Orientation::Horizontal, 4);
        search_row.set_margin_top(4);
        search_row.set_margin_start(4);
        search_row.set_margin_end(4);
        // Back-to-gallery button sits at the far left of the search row.
        search_row.append(&btn_album_back);
        search_row.append(&search_entry);
        search_row.append(&search_clear_btn);
        files_vbox.append(&search_row);

        let track_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sort_model = SortListModel::new(Some(track_store.clone()), None::<gtk4::Sorter>);
        let multi_sel = MultiSelection::new(Some(sort_model.clone()));
        let col_view = ColumnView::new(Some(multi_sel.clone()));
        col_view.add_css_class("ml-col-view");
        col_view.set_show_row_separators(false);
        col_view.set_show_column_separators(false);
        col_view.set_hexpand(true);
        col_view.set_vexpand(true);
        col_view.set_reorderable(true);

        // Row context menu + Send-to actions, built in `window/files_menu.rs`
        // (plan step 4). Returns the action group, already attached to the
        // ColumnView and the window, plus the three cells the rest of this
        // page shares with the actions.
        let menu = super::files_menu::install(ctx, &col_view, &multi_sel, &track_store);
        let ml_action_group = menu.group;
        let files_status_holder = menu.files_status_holder;
        let ml_selected_tracks = menu.selected_tracks;
        let ml_live_selected_paths = menu.live_selected_paths;

        let col_defs: &[(&str, &str, i32, bool)] = ALL_COLUMNS
            .iter()
            .map(|c| (c.id, c.header, 80, c.expand))
            .collect::<Vec<_>>()
            .leak();

        let visible_ids: Vec<String> = state.borrow().config.media_library.visible_columns.clone();
        let saved_widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();

        // The artwork column's cell. Shared with the playlist editor and the
        // device view via `ArtworkCells` so all three render identically —
        // they had drifted into three different cells (see its doc comment).
        // It also owns the two caches this used to keep here: the per-button
        // click handlers and the in-flight thumbnail decodes.
        let artwork_cells = Rc::new(ArtworkCells::new());

        // Capture store_ref before factory so it's available for the factory's right-click handler
        let store_for_ctx = track_store.clone();

        // ── Unscanned indicator column (always first, always visible) ──────────
        // The status is resolved off-thread and memoized per path: see
        // `FileStatus`. A bind must never touch the filesystem, because it runs
        // for every row on every scroll.
        let glyph_cache: GlyphCache = Rc::new(RefCell::new(std::collections::HashMap::new()));
        let glyph_inflight: GlyphInflight =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        {
            let unscanned_factory = SignalListItemFactory::new();
            let cache = glyph_cache.clone();
            let inflight = glyph_inflight.clone();

            unscanned_factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() {
                    return;
                }
                let lbl = Label::builder()
                    .halign(Align::Center)
                    .valign(Align::Center)
                    .css_classes(["ml-col-label"])
                    .build();
                li.set_child(Some(&lbl));
            });

            unscanned_factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let boxed = li
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok());
                let Some(boxed) = boxed else {
                    return;
                };
                let t = boxed.borrow::<crate::media_library::LibTrack>();
                let lbl = li.child().and_then(|c| c.downcast::<Label>().ok());
                let Some(lbl) = lbl else {
                    return;
                };
                // A row can carry a `last_scanned` timestamp yet have no real
                // metadata: `update_last_scanned` runs after every scan pass
                // even when extraction produced nothing (e.g. the duration
                // probe failed). So "scanned" for the status glyph means
                // metadata was actually extracted — duration is the reliable
                // tell — not merely that a timestamp exists.
                //   ❓ never (properly) scanned — no metadata
                //   🔄 scanned, but the file changed since (rescan to refresh)
                //   🔒 read-only
                //
                // ❓ is answered entirely from the row's own DB fields, so it
                // stays inline. The other two need the filesystem and must not
                // run here — see `FileStatus`.
                let unscanned = t.length_secs.is_none() || t.last_scanned.is_none();
                let path = t.path.clone();
                let fresh = cache
                    .borrow()
                    .get(&path)
                    .filter(|(_, at)| at.elapsed() < GLYPH_TTL)
                    .map(|(status, _)| *status);
                if let Some(known) = fresh {
                    known.apply(&lbl);
                    return;
                }
                // Not resolved yet. Paint the answer the row's own DB fields
                // already give — ❓ for never-scanned, nothing otherwise —
                // rather than spending this frame on syscalls. The probe
                // refines it in a moment, and for an unscanned row the only
                // refinement it can make is ⚠ when the file has gone.
                if unscanned {
                    FileStatus::Unscanned.apply(&lbl);
                } else {
                    lbl.set_label("");
                    lbl.set_tooltip_text(None);
                }
                // Scrolling rebinds the same rows constantly, so one probe per
                // path at a time — otherwise a fast scroll queues hundreds of
                // duplicate reads of the same file.
                if !inflight.borrow_mut().insert(path.clone()) {
                    return;
                }
                let last_scanned = t.last_scanned.clone();
                let stored_mtime = t.file_mtime.clone();
                let li_row = li.clone();
                let cache_done = cache.clone();
                let inflight_done = inflight.clone();
                glib::spawn_future_local(async move {
                    // Let the view settle before touching the disk.
                    //
                    // A row that scrolls past is bound and rebound in far less
                    // than this, so waiting first means only the rows the user
                    // actually lands on are ever probed. Without it, dragging
                    // the scrollbar across the library queued one probe per
                    // distinct row it swept over — each a `stat` plus an
                    // `open(O_WRONLY)` — and the app kept working through that
                    // backlog at ~24% of a core for 20 s after the scrolling
                    // stopped, painting glyphs onto rows nobody was looking at
                    // any more (measured 2026-08-11).
                    glib::timeout_future(std::time::Duration::from_millis(150)).await;
                    if !row_shows_path(&li_row, &path) {
                        // Scrolled away. Drop the reservation so a later bind
                        // of this same path can still probe it.
                        inflight_done.borrow_mut().remove(&path);
                        return;
                    }
                    let probe_path = path.clone();
                    let status = gio::spawn_blocking(move || {
                        probe_file_status(
                            &probe_path,
                            unscanned,
                            last_scanned.as_deref(),
                            stored_mtime.as_deref(),
                        )
                    })
                    .await;
                    inflight_done.borrow_mut().remove(&path);
                    let Ok(status) = status else { return };
                    cache_done
                        .borrow_mut()
                        .insert(path.clone(), (status, std::time::Instant::now()));
                    // Recycled while the probe ran — the answer is still worth
                    // caching, but paint it only if this row still shows it.
                    if row_shows_path(&li_row, &path) {
                        if let Some(l) = li_row.child().and_then(|c| c.downcast::<Label>().ok()) {
                            status.apply(&l);
                        }
                    }
                });
            });

            let unscanned_col = ColumnViewColumn::new(Some(""), Some(unscanned_factory));
            unscanned_col.set_fixed_width(24);
            col_view.append_column(&unscanned_col);
        }

        let all_cols: Vec<(String, ColumnViewColumn)> = col_defs
            .iter()
            .map(|(id, header, _min_w, expand)| {
                let factory = SignalListItemFactory::new();
                let id_str = id.to_string();
                let is_artwork = id_str == "artwork_path";
                let setup_cells = artwork_cells.clone();
                let bind_cells = artwork_cells.clone();
                let ctx_multi_sel = multi_sel.clone();
                let ctx_col_view = col_view.clone();
                let _ctx_store = store_for_ctx.clone();
                let ml_tracks_gest = ml_selected_tracks.clone();
                let state_for_ctx = state.clone();
                let ctx_drives = current_drives.clone();
                let ctx_devices = current_devices.clone();
                // F12.2: separate clone for connect_bind — state_for_ctx
                // above is moved into connect_setup's right-click handler.
                let bind_state = state.clone();

                factory.connect_setup(move |_, obj| {
                    let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();

                    // Skip if child already exists (row is being recycled)
                    if li.child().is_some() {
                        return;
                    }

                    let child: gtk4::Widget;

                    if is_artwork {
                        child = setup_cells.setup().upcast::<gtk4::Widget>();
                    } else {
                        let lbl = Label::builder()
                            .margin_start(6)
                            .margin_end(6)
                            .margin_top(3)
                            .margin_bottom(3)
                            .hexpand(true)
                            .vexpand(true)
                            .halign(Align::Fill)
                            .valign(Align::Fill)
                            .xalign(0.0)
                            .ellipsize(gtk4::pango::EllipsizeMode::End)
                            .css_classes(["ml-col-label"])
                            .build();
                        child = lbl.upcast::<gtk4::Widget>();
                    }

                    // Per-cell DragSource — collects every currently-selected
                    // ML row as a FileList content provider so the user can
                    // drag library tracks out to the active playlist's
                    // pl_scroll drop target (which accepts FileList).  Plain
                    // single-track drag works too: if the row under the
                    // pointer is not in the selection it still ships its
                    // own path.
                    {
                        let ds = gtk4::DragSource::new();
                        ds.set_actions(gtk4::gdk::DragAction::COPY);
                        let ds_sel = ctx_multi_sel.clone();
                        let ds_li  = li.clone();
                        ds.connect_prepare(move |_, _, _| {
                            let mut paths: Vec<std::path::PathBuf> = Vec::new();
                            let mut self_path: Option<std::path::PathBuf> = None;
                            if let Some(obj) = ds_li.item()
                                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            {
                                let t = obj.borrow::<crate::media_library::LibTrack>();
                                self_path = Some(std::path::PathBuf::from(&t.path));
                            }
                            for i in 0..ds_sel.n_items() {
                                if ds_sel.is_selected(i) {
                                    if let Some(obj) = ds_sel.item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        paths.push(std::path::PathBuf::from(&t.path));
                                    }
                                }
                            }
                            if paths.is_empty() {
                                if let Some(p) = self_path { paths.push(p); }
                            }
                            if paths.is_empty() { return None }
                            let files: Vec<gio::File> = paths.iter()
                                .map(|p| gio::File::for_path(p))
                                .collect();
                            let fl = gdk::FileList::from_array(&files);
                            Some(gdk::ContentProvider::for_value(&fl.to_value()))
                        });
                        child.add_controller(ds);
                    }

                    // Add right-click gesture to each row.  Capture phase
                    // pre-empts ColumnView's default secondary-button
                    // handler so multi-selection survives long enough for
                    // our is_selected guard to inspect it.
                    let gesture = gtk4::GestureClick::new();
                    gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
                    gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
                    let sel_gest = ctx_multi_sel.clone();
                    let col_popup = ctx_col_view.clone();
                    let li_gest = li.clone();
                    let ml_tracks_for_gest = ml_tracks_gest.clone();
                    let state_for_gest = state_for_ctx.clone();
                    let drives_for_gest = ctx_drives.clone();
                    let devices_for_gest = ctx_devices.clone();
                    gesture.connect_pressed(move |gest, n_press, x, y| {
                        if n_press != 1 {
                            return;
                        }
                        // Get the item directly from the ListItem - no coordinate math needed!
                        let Some(item) = li_gest.item() else {
                            return;
                        };
                        let item_clone = item.clone();

                        // Find the index of the clicked item by checking each item
                        let mut clicked_index: Option<u32> = None;
                        for i in 0..sel_gest.n_items() {
                            if let Some(model_item) = sel_gest.item(i) {
                                if model_item == item_clone {
                                    clicked_index = Some(i);
                                    break;
                                }
                            }
                        }

                        // Only change selection if clicked on non-selected item
                        // This preserves multi-selection when right-clicking on selected items
                        if let Some(idx) = clicked_index {
                            if !sel_gest.is_selected(idx) {
                                sel_gest.unselect_all();
                                sel_gest.select_item(idx, true);
                            }
                        }

                        // Collect selected tracks into shared state for action handlers
                        let mut paths: Vec<std::path::PathBuf> = Vec::new();
                        let mut selected_count = 0usize;
                        for i in 0..sel_gest.n_items() {
                            if sel_gest.is_selected(i) {
                                if let Some(obj) = sel_gest
                                    .item(i)
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                {
                                    let t = obj.borrow::<crate::media_library::LibTrack>();
                                    paths.push(std::path::PathBuf::from(&t.path));
                                    selected_count += 1;
                                }
                            }
                        }
                        *ml_tracks_for_gest.borrow_mut() = paths;

                        // Convert coordinates from gesture widget to ColumnView
                        // The gesture gives coords in the child widget's space
                        let child = li_gest.child();
                        let (popup_x, popup_y) = if let Some(child_widget) = child {
                            if let Some((rel_x, rel_y)) =
                                child_widget.translate_coordinates(&col_popup, x, y)
                            {
                                (rel_x, rel_y)
                            } else {
                                (x, y)
                            }
                        } else {
                            (x, y)
                        };

                        // Order: Send to · Replace · ─ · ID3 · Album Art ·
                        // Lyrics · Rescan · Calc RG · ─ · Remove. Matches the
                        // macOS Files row menu (MLFilesTable). "Append to
                        // Playlist" is gone — Send to ▸ Active Playlist is the
                        // append path, the same as macOS.
                        let send = build_send_to_menu(
                            &state_for_gest,
                            &SendToActions {
                                active: "ml.send-active",
                                new_playlist: "ml.add-to-new",
                                saved_playlist: "ml.add-to-saved",
                                drive: "ml.send-drive",
                                device: "ml.send-device",
                                drives: drives_for_gest.borrow().iter()
                                    .map(|d| (d.id.clone(), d.label.clone()))
                                    .collect(),
                                devices: devices_for_gest.borrow().iter()
                                    .map(|d| (d.id.clone(), d.label.clone()))
                                    .collect(),
                            },
                        );
                        let menu = gio::Menu::new();
                        menu.append_submenu(Some("↪ Send to"), &send);
                        menu.append_item(&gio::MenuItem::new(
                            Some("♻ Replace Current Playlist"),
                            Some("ml.replace"),
                        ));
                        // Flat (no sections): single-only view items, then the
                        // maintenance actions, then Remove last.
                        if selected_count == 1 {
                            menu.append_item(&gio::MenuItem::new(
                                Some("🎵 View/Edit ID3"),
                                Some("ml.edit-id3"),
                            ));
                            menu.append_item(&gio::MenuItem::new(
                                Some("🖼 View Album Art"),
                                Some("ml.view-art"),
                            ));
                            menu.append_item(&gio::MenuItem::new(
                                Some("📝 View/Search Lyrics"),
                                Some("ml.lyrics"),
                            ));
                        }
                        menu.append_item(&gio::MenuItem::new(
                            Some("🔄 Rescan Metadata"),
                            Some("ml.rescan"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("📊 Calculate ReplayGain"),
                            Some("ml.calc-rg"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("✕ Remove from Library"),
                            Some("ml.remove"),
                        ));

                        // NESTED (pop-out submenus) via the shared helper, which
                        // also forces the popover to grow to its full height so
                        // sectioned menus don't sprout scroll arrows. set_parent
                        // BEFORE set_pointing_to so the anchor rect is in the
                        // parent's coordinate space.
                        let popover = context_popover(&menu);
                        popover.set_parent(&col_popup);
                        popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(
                            popup_x as i32,
                            popup_y as i32,
                            1,
                            1,
                        )));
                        popover.popup();
                        gest.set_state(gtk4::EventSequenceState::Claimed);
                    });
                    child.add_controller(gesture);
                    if li.child().is_none() {
                        li.set_child(Some(&child));
                    }
                });
                factory.connect_bind(move |_, obj| {
                    let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                    let boxed = li
                        .item()
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok());
                    let Some(boxed) = boxed else {
                        return;
                    };
                    let t = boxed.borrow::<crate::media_library::LibTrack>();
                    // F12.2: read live so a Settings toggle applies to
                    // already-bound cells on the next rebind, not just at
                    // window construction (the ML window is a singleton —
                    // see rebuild_ml_callback in player.rs).
                    let artist_as_album_artist =
                        bind_state.borrow().config.media_library.artist_as_album_artist;

                    if is_artwork {
                        bind_cells.bind(li, t.artwork_path.as_deref(), |li| {
                            li.item()
                                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                .and_then(|b| {
                                    b.borrow::<crate::media_library::LibTrack>()
                                        .artwork_path
                                        .clone()
                                })
                        });
                        return;
                    }

                    let lbl = li.child().and_then(|c| c.downcast::<Label>().ok());
                    let Some(lbl) = lbl else {
                        return;
                    };
                    let text = match id_str.as_str() {
                        "num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
                        "title" => t.title.as_deref().unwrap_or(&t.filename).to_string(),
                        "artist" => t.artist.as_deref().unwrap_or("").to_string(),
                        "album" => t.album.as_deref().unwrap_or("").to_string(),
                        // F12.2: falls back to artist when the album-artist
                        // tag is blank and the toggle is on. A4 (phase 11
                        // album gallery) MUST also use this helper.
                        "album_artist" => crate::play_stats::effective_album_artist(
                            t.artist.as_deref().unwrap_or(""),
                            t.album_artist.as_deref().unwrap_or(""),
                            artist_as_album_artist,
                        ),
                        "duration" => crate::model::fmt_secs(t.length_secs),
                        "filename" => t.filename.clone(),
                        "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
                        "genre" => t.genre.as_deref().unwrap_or("").to_string(),
                        "bitrate" => t.bitrate.map(|b| format!("{b}k")).unwrap_or_default(),
                        "channels" => match t.channels.unwrap_or(0) {
                            1 => "mono".to_string(),
                            2 => "stereo".to_string(),
                            n => format!("{}ch", n),
                        },
                        "path" => t.path.clone(),
                        "play_count" => t.play_count.to_string(),
                        "last_played" => format_last_played(t.last_played.as_deref().unwrap_or("")),
                        "last_scanned" => t.last_scanned.as_deref().unwrap_or("").to_string(),
                        "disc_num" => {
                            let d = t.disc_num.unwrap_or(0);
                            if d == 0 {
                                String::new()
                            } else if let Some(total) = t.disc_total {
                                if total > 0 {
                                    format!("{}/{}", d, total)
                                } else {
                                    d.to_string()
                                }
                            } else {
                                d.to_string()
                            }
                        }
                        "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
                        "composer" => t.composer.as_deref().unwrap_or("").to_string(),
                        "original_artist" => t.original_artist.as_deref().unwrap_or("").to_string(),
                        "copyright" => t.copyright.as_deref().unwrap_or("").to_string(),
                        "url" => t.url.as_deref().unwrap_or("").to_string(),
                        "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_string(),
                        "bpm" => t.bpm.as_deref().unwrap_or("").to_string(),
                        "lyric" => truncate_display(t.lyric.as_deref().unwrap_or(""), 30),
                        "comment" => t.comment.as_deref().unwrap_or("").to_string(),
                        "artwork_path" => {
                            if t.artwork_path.is_some() {
                                "Yes".to_string()
                            } else {
                                String::new()
                            }
                        }
                        // Every column this match doesn't special-case falls
                        // through to the shared renderer — the phase-1 columns
                        // (filetype, sample rate, size, date added, mtime,
                        // mode) silently rendered blank here while the DB had
                        // the data, because `_ => String::new()` swallowed
                        // them (found in the phase-1 user pass).
                        other => ml_cell_text(&t, other, artist_as_album_artist),
                    };
                    lbl.set_text(&gtk_safe(&text));
                });

                let col = ColumnViewColumn::new(Some(header), Some(factory));
                col.set_resizable(true);
                if *expand {
                    col.set_expand(true);
                }
                col.set_visible(visible_ids.contains(&id.to_string()));
                if let Some(&w) = saved_widths.get(&id.to_string()) {
                    if w > 0 {
                        col.set_fixed_width(w);
                    }
                }

                let sort_id = id.to_string();
                let sorter = CustomSorter::new(move |a, b| {
                    let a_val = a
                        .downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id)
                        })
                        .unwrap_or_default();
                    let b_val = b
                        .downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id)
                        })
                        .unwrap_or_default();
                    a_val.cmp(&b_val).into()
                });
                col.set_sorter(Some(&sorter));

                col_view.append_column(&col);
                (id.to_string(), col)
            })
            .collect();
        let all_cols = Rc::new(all_cols);

        // Expose col_view and all_cols for close_request (outside this block scope).
        *col_view_holder.borrow_mut() = Some(col_view.clone());
        *all_cols_holder.borrow_mut() = all_cols.iter().cloned().collect();

        // Restore column order from config (empty list means use default order).
        // The unscanned indicator column is always first (position 0); named
        // columns start at position 1.
        {
            let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
            if !saved_order.is_empty() {
                // Remove all named columns from their current positions.
                for (_, col) in all_cols.iter() {
                    col_view.remove_column(col);
                }
                // Re-insert in saved order starting after the unscanned column.
                let mut pos = 1u32;
                for col_id in &saved_order {
                    if let Some((_, col)) = all_cols.iter().find(|(id, _)| id == col_id) {
                        col_view.insert_column(pos, col);
                        pos += 1;
                    }
                }
                // Append columns not present in saved_order (e.g. newly added columns).
                for (id, col) in all_cols.iter() {
                    if !saved_order.contains(id) {
                        col_view.insert_column(pos, col);
                        pos += 1;
                    }
                }
            }
        }

        // Phase 11 A5: "Play Album" / "Enqueue Album" — only meaningful while
        // `album_filter` is active, i.e. the Files view is showing a single
        // album drilled into from the gallery. Declared here (before
        // `rebuild_files`) so their visibility can be kept in sync from inside
        // that closure on every rebuild.
        //
        // Hidden rather than merely insensitive: the plain Files view is a
        // list of tracks, not of an album, so a greyed "Play Album" there is
        // an action the view never offers rather than one temporarily
        // unavailable. They appear on entering a drill-down and go again when
        // it clears.
        let btn_play_album = Button::with_label("▶ Play Album");
        btn_play_album.add_css_class("pl-btn");
        btn_play_album.set_visible(false);
        let btn_enqueue_album = Button::with_label("+ Enqueue Album");
        btn_enqueue_album.add_css_class("pl-btn");
        btn_enqueue_album.set_visible(false);

        // Whether the table currently *shows* a drill-down, which is not the
        // same question as whether `album_filter` is currently set: pressing
        // "◀ Albums" clears the filter and refreshes the gallery, leaving this
        // table still holding one album's tracks. Keying the Files re-select
        // rebuild off the filter alone therefore skipped a rebuild the table
        // needed, and Files came back showing the album (2026-08-11).
        //
        // Written by `rebuild_files` itself, so it reports what was actually
        // rendered rather than what someone remembered to record.
        let files_filtered = Rc::new(Cell::new(false));

        // The track table's vertical adjustment, so a rebuild can put the view
        // back where it was. Late-bound: the `ScrolledWindow` that owns it is
        // built below, after this closure. Empty until then, which is correct
        // — the first rebuild has no position worth keeping.
        let vadj_holder: Rc<RefCell<Option<gtk4::Adjustment>>> = Rc::new(RefCell::new(None));

        let rebuild_files: Rc<dyn Fn() -> usize> = {
            let state_rc = state.clone();
            let store_ref = track_store.clone();
            let search_ref = search_entry.clone();
            let album_filter_rc = album_filter.clone();
            let btn_play_album_rc = btn_play_album.clone();
            let btn_enqueue_album_rc = btn_enqueue_album.clone();
            let files_filtered_rc = files_filtered.clone();
            let vadj_rc = vadj_holder.clone();
            let sel_ref = multi_sel.clone();
            Rc::new(move || {
                // Album drill-down (Phase 11 A5): when a gallery cell was
                // activated, populate from that one album instead of the
                // search/all-tracks path below, and ignore whatever's in the
                // search box until the filter is cleared (Files re-select or
                // typing in the search box both clear it).
                let active_filter = { album_filter_rc.borrow().clone() };
                btn_play_album_rc.set_visible(active_filter.is_some());
                btn_enqueue_album_rc.set_visible(active_filter.is_some());
                files_filtered_rc.set(active_filter.is_some());
                let tracks: Vec<crate::media_library::LibTrack> =
                    if let Some((album, album_artist)) = active_filter {
                        search_ref.set_placeholder_text(Some(&format!(
                            "Album: {} — {}",
                            gtk_safe(&album),
                            gtk_safe(&album_artist)
                        )));
                        let artist_as_album =
                            state_rc.borrow().config.media_library.artist_as_album_artist;
                        state_rc
                            .borrow()
                            .media_lib
                            .as_ref()
                            .and_then(|lib| {
                                lib.album_tracks(&album, &album_artist, artist_as_album).ok()
                            })
                            .unwrap_or_default()
                    } else {
                        search_ref
                            .set_placeholder_text(Some("Search artist, title, album…"));
                        // Respect any active search filter so that background rebuilds
                        // (rescan, folder add, ID3 save) don't discard the current query.
                        let query = search_ref.text().to_lowercase();
                        state_rc
                            .borrow()
                            .media_lib
                            .as_ref()
                            .and_then(|lib| {
                                if query.is_empty() {
                                    lib.all_tracks().ok()
                                } else {
                                    lib.search_tracks(&query).ok()
                                }
                            })
                            .unwrap_or_default()
                    };
                let count = tracks.len();
                let boxed: Vec<glib::BoxedAnyObject> =
                    tracks.into_iter().map(glib::BoxedAnyObject::new).collect();

                // `splice` empties the store before refilling it, so the
                // adjustment's `upper` momentarily collapses to zero and GTK
                // clamps `value` to 0 with it. Anyone mid-drag on the
                // scrollbar was then tracking against a dead range and the
                // view snapped to an extreme (2026-08-11). Put the offset
                // back afterwards.
                // A rebuild replaces every row, which drops the selection on
                // the floor. That is only tolerable if it is invisible: a
                // background rescan must not throw away a multi-select the
                // user is part-way through building a playlist from. Remember
                // the chosen tracks by path and put them back afterwards.
                let selected_paths = selected_paths_of(&sel_ref);
                let saved = vadj_rc.borrow().as_ref().map(|a| a.value());
                store_ref.splice(0, store_ref.n_items(), &boxed);
                restore_selection(&sel_ref, &selected_paths);
                if let (Some(adj), Some(v)) = (vadj_rc.borrow().clone(), saved) {
                    adj.set_value(v);
                    // The ColumnView re-measures its content height after the
                    // model settles, which clamps the value we just set
                    // against a stale `upper`. Set it once more when that has
                    // happened — unless something moved the view in between,
                    // which would make this a yank rather than a restore.
                    let clamped = adj.value();
                    let adj_idle = adj.clone();
                    glib::idle_add_local_once(move || {
                        if (adj_idle.value() - clamped).abs() < 1.0 {
                            adj_idle.set_value(v);
                        }
                    });
                }
                count
            })
        };

        rebuild_files();
        sort_model.set_sorter(col_view.sorter().as_ref());

        let track_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Automatic)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .min_content_height(300)
            .child(&col_view)
            .build();
        *vadj_holder.borrow_mut() = Some(track_scroll.vadjustment());

        // Two empty states share this view: nothing indexed at all, and a
        // search that matched nothing. The second is only reachable once the
        // first is not, so one stack with a swapped-out page covers both.
        let files_empty = super::util::empty_state(
            "folder-music-symbolic",
            "No music folders",
            Some("Add a folder to start building your library"),
        );
        let files_stack = super::util::stack_with_empty_state(&track_scroll, &files_empty);
        files_vbox.append(&files_stack);
        // Both branches below go through the same `empty_state_for` decision
        // (util.rs) rather than each re-deriving "is the query non-empty" —
        // an initial-sync block here once checked only `n_items() > 0` and
        // never looked at the query, so a remembered search that matched
        // nothing showed "No music folders" on cold load instead of "No
        // results" (2026-08-24 review). Routing every call site through one
        // function makes that class of bug structurally impossible: there is
        // only one place left to get the decision wrong.
        let apply_files_empty_state: Rc<dyn Fn()> = {
            let stack = files_stack.clone();
            let empty = files_empty.clone();
            let store = track_store.clone();
            let entry = search_entry.clone();
            Rc::new(move || {
                match super::util::empty_state_for(
                    store.n_items() > 0,
                    &entry.text(),
                    (
                        "folder-music-symbolic",
                        "No music folders",
                        "Add a folder to start building your library",
                    ),
                ) {
                    super::util::EmptyState::Content => stack.set_visible_child_name("content"),
                    super::util::EmptyState::Show { icon, title, description } => {
                        empty.set_icon_name(Some(icon));
                        empty.set_title(title);
                        empty.set_description(Some(&gtk_safe(&description)));
                        stack.set_visible_child_name("empty");
                    }
                }
            })
        };
        {
            let apply = apply_files_empty_state.clone();
            track_store.connect_items_changed(move |_, _, _, _| apply());
        }
        // `rebuild_files()` above already populated the store once, before
        // this handler existed to see it — sync the initial state by hand,
        // through the same function the handler uses.
        apply_files_empty_state();

        // Live search with 300ms debounce to avoid rebuilding on every keystroke.
        {
            let state_rc = state.clone();
            let store_ref = track_store.clone();
            let album_filter_search = album_filter.clone();
            let btn_play_album_search = btn_play_album.clone();
            let btn_enqueue_album_search = btn_enqueue_album.clone();
            let btn_album_back_search = btn_album_back.clone();
            let files_filtered_search = files_filtered.clone();
            let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
            search_entry.connect_changed(move |entry| {
                // Typing escapes any active album drill-down back to the full
                // library — done synchronously (not behind the debounce)
                // since it's cheap and shouldn't wait on the timer.
                {
                    *album_filter_search.borrow_mut() = None;
                }
                // The debounced splice below repopulates from the query, not
                // through `rebuild_files`, so record here that the table is no
                // longer a drill-down.
                files_filtered_search.set(false);
                entry.set_placeholder_text(Some("Search artist, title, album…"));
                btn_play_album_search.set_visible(false);
                btn_enqueue_album_search.set_visible(false);
                btn_album_back_search.set_visible(false);

                let raw_query = entry.text().to_string();
                let query = raw_query.to_lowercase();
                // Cancel any pending search.
                if let Some(src) = pending.borrow_mut().take() {
                    src.remove();
                }
                // Schedule a new search after 300ms of inactivity.
                let state_inner = state_rc.clone();
                let store_inner = store_ref.clone();
                let pending_inner = pending.clone();
                let src =
                    glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                        let tracks: Vec<crate::media_library::LibTrack> = state_inner
                            .borrow()
                            .media_lib
                            .as_ref()
                            .and_then(|lib| {
                                if query.is_empty() {
                                    lib.all_tracks().ok()
                                } else {
                                    lib.search_tracks(&query).ok()
                                }
                            })
                            .unwrap_or_default();
                        let boxed: Vec<glib::BoxedAnyObject> =
                            tracks.into_iter().map(glib::BoxedAnyObject::new).collect();
                        store_inner.splice(0, store_inner.n_items(), &boxed);
                        // F12.1: remember this view's query for next open (only
                        // when the feature is on; the value is unused otherwise).
                        {
                            let mut s = state_inner.borrow_mut();
                            if s.config.media_library.remember_search {
                                s.config
                                    .media_library
                                    .last_search
                                    .insert("files".to_string(), raw_query.clone());
                            }
                        }
                        pending_inner.borrow_mut().take();
                        glib::ControlFlow::Break
                    });
                *pending.borrow_mut() = Some(src);
            });
        }

        let files_status = Label::builder()
            .label("")
            .halign(Align::Start)
            .margin_start(6)
            .margin_end(6)
            .margin_bottom(2)
            .build();
        files_status.add_css_class("status-label");
        files_vbox.append(&files_status);
        *files_status_holder.borrow_mut() = Some(files_status.clone());

        // Button row.
        let btn_row = GtkBox::new(Orientation::Horizontal, 4);
        btn_row.set_margin_start(4);
        btn_row.set_margin_end(4);
        btn_row.set_margin_bottom(4);

        let btn_send_to = gtk4::MenuButton::builder()
            .label("Send to ▾")
            .build();
        btn_send_to.add_css_class("pl-btn");
        // Install "ml" directly on the button. Window-level alone enabled the
        // top-level items but the NESTED submenu popovers (Saved Playlist ▸,
        // Disc Drive ▸) resolve actions against the button's own popover
        // chain, so their items didn't dispatch until the group sits on the
        // button itself — the closest ancestor of every nested popover
        // (2026-07-16).
        btn_send_to.insert_action_group("ml", Some(&ml_action_group));
        let btn_customize = Button::with_label("⚙ Columns");
        btn_customize.add_css_class("pl-btn");
        let btn_add_folder = Button::with_label("+ Add Folder");
        btn_add_folder.add_css_class("pl-btn");
        let btn_rescan = Button::with_label("⟳ Rescan");
        btn_rescan.add_css_class("pl-btn");
        let btn_cancel = Button::with_label("✕ Cancel Scan");
        btn_cancel.add_css_class("pl-btn");
        btn_cancel.add_css_class("destructive");
        btn_cancel.set_visible(false);
        let btn_rm_from_ml = Button::with_label("✕ Remove");
        btn_rm_from_ml.add_css_class("pl-btn");
        btn_rm_from_ml.add_css_class("destructive");

        // Bulk ReplayGain analysis (missing-or-stale set only — the forced,
        // "analyze exactly this selection" variant lives in the row context
        // menu as "Calculate ReplayGain", ml.calc-rg). Disabled with a
        // tooltip when the `rganalysis` GStreamer element isn't installed —
        // silently-unavailable rather than an error dialog (house rule).
        let btn_analyze_rg = Button::with_label("Analyze ReplayGain");
        btn_analyze_rg.add_css_class("pl-btn");
        let rg_available = crate::replaygain::rg_analysis_available();
        if !rg_available {
            btn_analyze_rg.set_sensitive(false);
            btn_analyze_rg.set_tooltip_text(Some("rganalysis plugin not installed"));
        }
        let btn_cancel_rg = Button::with_label("✕ Cancel Analysis");
        btn_cancel_rg.add_css_class("pl-btn");
        btn_cancel_rg.add_css_class("destructive");
        btn_cancel_rg.set_visible(false);

        // Button row: library management on the left, actions on what is
        // selected on the right.
        //
        // This is the Disc page's layout (`disc_page.rs`, where identify /
        // rip / edit / eject sit left of the spring and enqueue / play sit
        // right of it), and the Device page's. Files and the album
        // drill-down had the two groups the other way round, so the button
        // in a given corner changed meaning as you moved between views.
        //
        // Play/Enqueue Album (Phase 11 A5) are hidden unless an album
        // drill-down filter is active — see `rebuild_files` above.
        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        btn_row.append(&btn_customize);
        btn_row.append(&btn_add_folder);
        btn_row.append(&btn_rescan);
        btn_row.append(&btn_cancel);
        btn_row.append(&btn_analyze_rg);
        btn_row.append(&btn_cancel_rg);
        btn_row.append(&spring);
        btn_row.append(&btn_play_album);
        btn_row.append(&btn_enqueue_album);
        btn_row.append(&btn_send_to);
        btn_row.append(&btn_rm_from_ml);
        files_vbox.append(&btn_row);

        // Play Album: replace the active playlist with the drilled-into
        // album's tracks and play from the first one. Same seam as the
        // device-view Play button above (~line 2215) — fresh borrow per
        // line, never one held across `play_current()`.
        {
            let state_pa = state.clone();
            let album_filter_pa = album_filter.clone();
            let rebuild_pl = rebuild_playlist.clone();
            btn_play_album.connect_clicked(move |_| {
                let filt = { album_filter_pa.borrow().clone() };
                let Some((album, album_artist)) = filt else { return };
                let artist_as_album =
                    state_pa.borrow().config.media_library.artist_as_album_artist;
                let tracks: Vec<crate::media_library::LibTrack> = state_pa
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| {
                        lib.album_tracks(&album, &album_artist, artist_as_album).ok()
                    })
                    .unwrap_or_default();
                if tracks.is_empty() {
                    return;
                }
                let _ = state_pa.borrow_mut().player.stop();
                state_pa.borrow_mut().playlist.clear();
                for lt in &tracks {
                    super::playlist_add::add_track(&state_pa, crate::model::Track::from(lt), false);
                }
                if !state_pa.borrow().playlist.is_empty() {
                    state_pa.borrow_mut().play_current();
                }
                rebuild_pl();
            });
        }

        // Enqueue Album: append the drilled-into album's tracks to the
        // active playlist. Same seam as the device-view Enqueue button above
        // (~line 2238).
        {
            let state_ea = state.clone();
            let album_filter_ea = album_filter.clone();
            let rebuild_pl = rebuild_playlist.clone();
            btn_enqueue_album.connect_clicked(move |_| {
                let filt = { album_filter_ea.borrow().clone() };
                let Some((album, album_artist)) = filt else { return };
                let artist_as_album =
                    state_ea.borrow().config.media_library.artist_as_album_artist;
                let tracks: Vec<crate::media_library::LibTrack> = state_ea
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| {
                        lib.album_tracks(&album, &album_artist, artist_as_album).ok()
                    })
                    .unwrap_or_default();
                if tracks.is_empty() {
                    return;
                }
                let was_empty = state_ea.borrow().playlist.is_empty();
                for lt in &tracks {
                    super::playlist_add::add_track(&state_ea, crate::model::Track::from(lt), false);
                }
                if state_ea.borrow().config.behavior.autoplay_on_add && was_empty {
                    state_ea.borrow_mut().play_current();
                }
                rebuild_pl();
            });
        }

        // ── Files view status bar ───────────────────────────────────────────
        // `rebuild_files()` (above) already populated `track_store` once, and
        // every later mutation (rescan, add-folder, search debounce, ID3
        // save, remove) goes through the same `track_store.splice(...)`, so
        // the helper's `items_changed` wiring keeps this live without extra
        // refresh calls at each call site.
        let (files_status_bar, _) = ml_status_bar(&multi_sel);
        files_vbox.append(&files_status_bar);
        // Sit directly below the file list (above the button row), matching the
        // active playlist window.
        files_vbox.reorder_child_after(&files_status_bar, Some(&files_stack));

        // Add selected tracks to playlist.
        let add_selected: Rc<dyn Fn()> = {
            let state_rc = state.clone();
            let sel_ref = multi_sel.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track_add = set_track.clone();
            Rc::new(move || {
                let was_empty = state_rc.borrow().playlist.is_empty();
                let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                let should_replace = crate::playlist_add::should_replace(
                    &state_rc.borrow().config.behavior.playlist_add_behavior,
                    crate::playlist_add::AddMode::Behavior,
                );
                if should_replace {
                    let _ = state_rc.borrow_mut().player.stop();
                    state_rc.borrow_mut().playlist.clear();
                }
                let mut added = 0usize;
                for i in 0..sel_ref.n_items() {
                    if sel_ref.is_selected(i) {
                        if let Some(obj) = sel_ref
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let t = obj.borrow::<crate::media_library::LibTrack>();
                            let track = crate::model::Track::from(&*t);
                            super::playlist_add::add_track(&state_rc, track, false);
                            added += 1;
                        }
                    }
                }
                if added > 0 {
                    // Autoplay when replacing (always start fresh) or when the
                    // playlist was empty and a track just arrived.
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_add(&display);
                        }
                    }
                    rebuild_pl();
                }
            })
        };

        // "Active Playlist" in the Send-to menu reuses this same logic.
        {
            let add = add_selected.clone();
            let action_send_active = gio::SimpleAction::new("send-active", None);
            action_send_active.connect_activate(move |_, _| {
                add();
            });
            ml_action_group.add_action(&action_send_active);
        }

        // Rebuild the Send-to menu model fresh on every open — drives/devices
        // may have come or gone. `set_create_popup_func` is invoked by GTK
        // right before the popover is shown; `connect_activate` does NOT fire
        // on a plain click, so the button appeared dead (2026-07-16).
        {
            let state_menu = state.clone();
            let current_drives = current_drives.clone();
            let current_devices = current_devices.clone();
            btn_send_to.set_create_popup_func(move |btn| {
                let menu = build_send_to_menu(
                    &state_menu,
                    &SendToActions {
                        active: "ml.send-active",
                        new_playlist: "ml.add-to-new",
                        saved_playlist: "ml.add-to-saved",
                        drive: "ml.send-drive",
                        device: "ml.send-device",
                        drives: current_drives.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        devices: current_devices.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    },
                );
                btn.set_menu_model(Some(&menu));
            });
        }

        // Double-click / Enter to add a single track.
        {
            let state_rc = state.clone();
            let sel_ref = multi_sel.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track_ml = set_track.clone();
            col_view.connect_activate(move |_, pos| {
                if let Some(obj) = sel_ref
                    .item(pos)
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                {
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    let should_replace = crate::playlist_add::should_replace(
                        &state_rc.borrow().config.behavior.playlist_add_behavior,
                        crate::playlist_add::AddMode::Behavior,
                    );
                    let t = obj.borrow::<crate::media_library::LibTrack>();
                    let track = crate::model::Track::from(&*t);
                    drop(t);
                    if should_replace {
                        // Stop before clearing so the current track doesn't
                        // keep playing after the playlist is replaced.
                        let _ = state_rc.borrow_mut().player.stop();
                        state_rc.borrow_mut().playlist.clear();
                    }
                    super::playlist_add::add_track(&state_rc, track, false);
                    // Autoplay when: the playlist was empty (append mode), or
                    // when replacing (the new track should always start playing).
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_ml(&display);
                        }
                    }
                    rebuild_pl();
                }
            });
        }

        // `l` — View/Search Lyrics for the single selected library row, in
        // Specific mode (the Media Library never follows playback). No-op on a
        // multi-row selection or an empty one, matching the row menu's
        // single-selection rule.
        {
            let key = EventControllerKey::new();
            let state_l = state.clone();
            let live_sel = ml_live_selected_paths.clone();
            let rebuild_l = rebuild_playlist.clone();
            key.connect_key_pressed(move |_, keyval, _, _| {
                if !matches!(keyval, gdk::Key::l | gdk::Key::L) {
                    return glib::Propagation::Proceed;
                }
                let paths = live_sel();
                if paths.len() != 1 {
                    return glib::Propagation::Proceed;
                }
                let path = paths[0].clone();
                let path_str = path.to_string_lossy().into_owned();
                let (artist, title, album_artist) = {
                    let s = state_l.borrow();
                    let lt = s
                        .media_lib
                        .as_ref()
                        .and_then(|ml| ml.track_by_path(&path_str).ok());
                    match lt {
                        Some(t) => (
                            t.artist.clone().unwrap_or_default(),
                            t.title.clone().unwrap_or_default(),
                            t.album_artist.clone().unwrap_or_default(),
                        ),
                        None => (
                            String::new(),
                            path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string(),
                            String::new(),
                        ),
                    }
                };
                view_or_search_lyrics(
                    &state_l, &path, &artist, &title, &album_artist,
                    rebuild_l.clone(), LyricsMode::Specific,
                );
                glib::Propagation::Stop
            });
            col_view.add_controller(key);
        }

        // Customize columns dialog.
        {
            let state_rc = state.clone();
            let all_cols_rc = all_cols.clone();
            let cv_holder = col_view_holder.clone();
            let ac_holder = all_cols_holder.clone();
            let state_reorder = state.clone();
            let win_wk = win.downgrade();
            btn_customize.connect_clicked(move |_| {
                let cols_for_callback = all_cols_rc.clone();
                let cv_h = cv_holder.clone();
                let ac_h = ac_holder.clone();
                let st_r = state_reorder.clone();
                open_customize_columns_dialog(
                    win_wk.upgrade().as_ref(),
                    state_rc.clone(),
                    "Customize Columns",
                    ColumnCustomizerMode::MediaLibrary,
                    Some(Rc::new(move |id: String, visible: bool| {
                        if let Some((_, col)) =
                            cols_for_callback.iter().find(|(col_id, _)| col_id == &id)
                        {
                            col.set_visible(visible);
                        }
                    }) as Rc<dyn Fn(String, bool)>),
                    Some(Rc::new(move || {
                        let saved_order =
                            st_r.borrow().config.media_library.ml_file_col_order.clone();
                        if saved_order.is_empty() {
                            return;
                        }
                        let cv_opt = cv_h.borrow();
                        let all_cols = ac_h.borrow();
                        if let Some(col_view) = &*cv_opt {
                            for (_, col) in all_cols.iter() {
                                col_view.remove_column(col);
                            }
                            let mut pos = 1u32;
                            for col_id in &saved_order {
                                if let Some((_, col)) =
                                    all_cols.iter().find(|(id, _)| id == col_id)
                                {
                                    col_view.insert_column(pos, col);
                                    pos += 1;
                                }
                            }
                            for (id, col) in all_cols.iter() {
                                if !saved_order.contains(id) {
                                    col_view.insert_column(pos, col);
                                    pos += 1;
                                }
                            }
                        }
                    }) as Rc<dyn Fn()>),
                );
            });
        }

        // Add Folder handler.
        {
            // A scan is the one thing that can change what the status column
            // should say, so it is also the only thing that invalidates the
            // memoized glyphs.
            let glyph_cache_scan = glyph_cache.clone();
            let state_rc = state.clone();
            let win_wk = win.downgrade();
            let rebuild_ref = rebuild_files.clone();
            let status_ref = files_status.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            btn_add_folder.connect_clicked(move |_| {
                let chooser = gtk4::FileDialog::new();
                chooser.set_title("Add Folder to Media Library");
                let state_inner = state_rc.clone();
                let rebuild_inner = rebuild_ref.clone();
                let status_inner = status_ref.clone();
                let cancel_btn = cancel_ref.clone();
                let rescan_btn = rescan_ref.clone();
                let glyph_cache_scan = glyph_cache_scan.clone();
                if let Some(w) = win_wk.upgrade() {
                    chooser.select_folder(Some(&w), None::<&gio::Cancellable>, move |result| {
                        let Ok(file) = result else {
                            return;
                        };
                        let Some(folder) = file.path() else {
                            return;
                        };
                        let path_str = folder.to_string_lossy().to_string();

                        let db_path = {
                            let s = state_inner.borrow();
                            s.media_lib
                                .as_ref()
                                .map(|_| crate::media_library::MediaLibrary::db_path_pub())
                        };
                        let Some(db_path) = db_path else {
                            status_inner.set_text("Media library not available");
                            return;
                        };
                        // Refuse to start a second concurrent scan.
                        if state_inner.borrow().ml_scan.is_some() {
                            status_inner.set_text("Scan already in progress — please wait");
                            return;
                        }

                        // Set up scan state: shows cancel button and disables rescan.
                        let cancel_flag = start_ml_scan(&state_inner, ScanType::AddFolder, 0);
                        status_inner.set_text("Reading tags…");
                        cancel_btn.set_visible(true);
                        rescan_btn.set_sensitive(false);

                        // Three channels: fast done, metadata progress, final result.
                        let (fast_tx, fast_rx) =
                            std::sync::mpsc::channel::<Result<usize, String>>();
                        let (progress_tx, progress_rx) =
                            std::sync::mpsc::channel::<(usize, usize)>();
                        let (result_tx, result_rx) =
                            std::sync::mpsc::channel::<Result<usize, String>>();

                        let cancel_thread = cancel_flag.clone();
                        // Read the config bool on the GTK thread before handing off —
                        // AppState (holds Player/GStreamer state) isn't Send.
                        let remove_missing =
                            state_inner.borrow().config.media_library.remove_missing_on_rescan;
                        std::thread::spawn(move || {
                            let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                                Ok(l) => l,
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                    return;
                                }
                            };
                            let folder_id = match lib.add_folder(&path_str) {
                                Err(e) => {
                                    let _ = fast_tx
                                        .send(Err(format!("Could not add '{}': {e}", path_str)));
                                    return;
                                }
                                Ok(r) => r.id(),
                            };
                            // Phase 1: insert file paths into DB (fast).
                            if let Err(e) =
                                lib.rescan_folder_fast(folder_id, &path_str, remove_missing)
                            {
                                let _ = fast_tx
                                    .send(Err(format!("Scan error for '{}': {e}", path_str)));
                                return;
                            }
                            let _ = fast_tx.send(Ok(folder_id as usize));
                            // Phase 2: read metadata. Reset tracks with no metadata
                            // first so any missed by a prior scan are re-processed.
                            let _ = lib.reset_unscanned_metadata();
                            let count = lib
                                .scan_folder(folder_id, &cancel_thread, |c, t| {
                                    let _ = progress_tx.send((c, t));
                                })
                                .map(|(scanned, _, _)| scanned)
                                .unwrap_or(0);
                            let _ = result_tx.send(Ok(count));
                        });

                        let fast_rx = std::cell::RefCell::new(fast_rx);
                        let progress_rx = std::cell::RefCell::new(progress_rx);
                        let result_rx = std::cell::RefCell::new(result_rx);
                        let fast_handled = std::cell::Cell::new(false);
                        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                            // Handle fast scan completion — rebuild immediately so
                            // tracks appear in the library while metadata loads.
                            if !fast_handled.get() {
                                if let Ok(fast_result) = fast_rx.borrow().try_recv() {
                                    fast_handled.set(true);
                                    {
                                        let mut s = state_inner.borrow_mut();
                                        s.media_lib =
                                            crate::media_library::MediaLibrary::open().ok();
                                    }
                                    if let Err(e) = fast_result {
                                        status_inner.set_text(&e);
                                        complete_ml_scan(&state_inner);
                                        cancel_btn.set_visible(false);
                                        rescan_btn.set_sensitive(true);
                                        return glib::ControlFlow::Break;
                                    }
                                    rebuild_inner();
                                    status_inner.set_text("Reading tags…");
                                    // New folder registered — restart the
                                    // live watcher so it's covered.
                                    watch::rebuild_watcher(&state_inner);
                                }
                            }

                            // Drain metadata progress updates.
                            while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                                update_ml_scan_progress(&state_inner, current, total);
                                status_inner
                                    .set_text(&format!("Reading tags {}/{}…", current, total));
                            }

                            // Check for final completion.
                            if let Ok(result) = result_rx.borrow().try_recv() {
                                {
                                    let mut s = state_inner.borrow_mut();
                                    s.media_lib = crate::media_library::MediaLibrary::open().ok();
                                }
                                complete_ml_scan(&state_inner);
                                glyph_cache_scan.borrow_mut().clear();
                                match result {
                                    Err(e) => status_inner.set_text(&e),
                                    Ok(_) => {
                                        let count = rebuild_inner();
                                        status_inner
                                            .set_text(&format!("{count} tracks in library"));
                                    }
                                }
                                cancel_btn.set_visible(false);
                                rescan_btn.set_sensitive(true);
                                return glib::ControlFlow::Break;
                            }

                            glib::ControlFlow::Continue
                        });
                    });
                }
            });
        }

        // Rescan handler — runs in a background thread to avoid blocking the UI.
        {
            let glyph_cache_rescan = glyph_cache.clone();
            let state_rc = state.clone();
            let rebuild_ref = rebuild_files.clone();
            let status_ref = files_status.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            btn_rescan.connect_clicked(move |_| {
                let glyph_cache_rescan = glyph_cache_rescan.clone();
                let db_path = {
                    let s = state_rc.borrow();
                    match s.media_lib.as_ref() {
                        None => {
                            status_ref.set_text("Media library not available");
                            return;
                        }
                        Some(_) => crate::media_library::MediaLibrary::db_path_pub(),
                    }
                };

                let cancel_flag = start_ml_scan(&state_rc, ScanType::Rescan, 0);
                status_ref.set_text("Reading tags…");
                cancel_ref.set_visible(true);
                rescan_ref.set_sensitive(false);

                let (progress_tx, progress_rx) = std::sync::mpsc::channel();
                let (result_tx, result_rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = result_tx.send(Err(format!("DB error: {e}")));
                            return;
                        }
                    };
                    let _ = lib.reset_unscanned_metadata();
                    let result = lib
                        .scan_all_folders(&cancel_flag, |current, total| {
                            let _ = progress_tx.send((current, total));
                        })
                        .map_err(|e| e.to_string());
                    let _ = result_tx.send(result);
                });
                let progress_rx = std::cell::RefCell::new(progress_rx);
                let result_rx = std::cell::RefCell::new(result_rx);
                let state_rc2 = state_rc.clone();
                let rebuild_ref2 = rebuild_ref.clone();
                let status_ref2 = status_ref.clone();
                let cancel_ref2 = cancel_ref.clone();
                let rescan_ref2 = rescan_ref.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                    // Check for progress updates
                    while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                        update_ml_scan_progress(&state_rc2, current, total);
                        status_ref2.set_text(&format!("Reading tags {}/{}…", current, total));
                    }

                    // Check for completion
                    if let Ok(result) = result_rx.borrow().try_recv() {
                        {
                            let mut s = state_rc2.borrow_mut();
                            s.media_lib = crate::media_library::MediaLibrary::open().ok();
                        }
                        complete_ml_scan(&state_rc2);
                        glyph_cache_rescan.borrow_mut().clear();
                        // Compact after a successful FULL rescan only, gated
                        // on the setting — VACUUM is too heavy to run after
                        // every fast folder-add, which is why this lives
                        // here and not in the shared complete_ml_scan.
                        if result.is_ok() {
                            let compact = state_rc2.borrow().config.media_library.compact_on_rescan;
                            if compact {
                                if let Some(ref lib) = state_rc2.borrow().media_lib {
                                    if let Err(e) = lib.compact() {
                                        eprintln!("compact_on_rescan: VACUUM failed: {e}");
                                    }
                                }
                            }
                        }
                        match result {
                            Err(e) => status_ref2.set_text(&format!("Rescan error: {}", e)),
                            Ok(_) => {
                                let count = rebuild_ref2();
                                status_ref2.set_text(&format!("{count} tracks in library"));
                            }
                        }
                        cancel_ref2.set_visible(false);
                        rescan_ref2.set_sensitive(true);
                        return glib::ControlFlow::Break;
                    }

                    glib::ControlFlow::Continue
                });
            });
        }

        // Bulk "Analyze ReplayGain" handler — analyzes the missing-or-stale
        // set across the whole library (not just the current selection/
        // search filter). Shares `analyze_job` with the context action.
        {
            let state_rc = state.clone();
            let rebuild_ref = rebuild_files.clone();
            let status_ref = files_status.clone();
            btn_analyze_rg.connect_clicked(move |_| {
                if !crate::replaygain::rg_analysis_available() {
                    return; // button is disabled in this case; defensive only
                }
                let tracks: Vec<crate::media_library::LibTrack> = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.all_tracks().ok())
                    .unwrap_or_default();
                // `rebuild_files` returns the new row count (for the "N
                // tracks" search-result label); `analyze_job` just wants a
                // refresh signal, so discard it here.
                let rebuild_ref2 = rebuild_ref.clone();
                let rebuild: Rc<dyn Fn()> = Rc::new(move || {
                    rebuild_ref2();
                });
                analyze_job(&state_rc, tracks, false, &status_ref, rebuild);
            });
        }

        // Cancel scan handler
        {
            let state_rc = state.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            let status_ref = files_status.clone();
            btn_cancel.connect_clicked(move |_| {
                cancel_ml_scan(&state_rc);
                status_ref.set_text("Cancelling…");
                cancel_ref.set_visible(false);
                rescan_ref.set_sensitive(true);
            });
        }

        // Cancel ReplayGain analysis handler.
        {
            let state_rc = state.clone();
            let status_ref = files_status.clone();
            btn_cancel_rg.connect_clicked(move |_| {
                cancel_rg_job(&state_rc);
                status_ref.set_text("Cancelling…");
            });
        }

        // Polling timer to sync scan/analysis state with UI. Single timer
        // owns all these buttons + the shared status label so a metadata
        // scan (`ml_scan`) and an RG analysis job (`rg_job`) — which are
        // mutually exclusive, see `start_rg_job` — can't fight over the same
        // widgets from two independent tickers.
        {
            let state_rc = state.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            let add_folder_ref = btn_add_folder.clone();
            let analyze_ref = btn_analyze_rg.clone();
            let cancel_rg_ref = btn_cancel_rg.clone();
            let status_ref = files_status.clone();
            let rg_was_running = std::cell::Cell::new(false);
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                let scan_state = state_rc.borrow().ml_scan.clone();
                let scan_busy = scan_state.is_some();
                // RG buttons + status: shared with the Settings window via
                // `sync_rg_ui`. A running metadata scan owns the status label
                // this tick, so render_status = !scan_busy.
                let rg_running = sync_rg_ui(
                    &state_rc,
                    &analyze_ref,
                    &cancel_rg_ref,
                    &status_ref,
                    rg_available,
                    scan_busy,
                    !scan_busy,
                    rg_was_running.get(),
                );
                rg_was_running.set(rg_running);
                let busy = scan_busy || rg_running;
                rescan_ref.set_sensitive(!busy);
                add_folder_ref.set_sensitive(!busy);
                if let Some(scan) = scan_state {
                    cancel_ref.set_visible(true);
                    if scan.total > 0 {
                        status_ref
                            .set_text(&format!("Reading tags {}/{}…", scan.current, scan.total));
                    } else {
                        status_ref.set_text("Reading tags…");
                    }
                } else {
                    cancel_ref.set_visible(false);
                }
                glib::ControlFlow::Continue
            });
        }

        // Remove selected tracks from library.
        {
            let sel_ref = multi_sel.clone();
            let store_ref = track_store.clone();
            let status_ref = files_status.clone();
            btn_rm_from_ml.connect_clicked(move |_| {
                // Collect IDs of every selected item in one pass.
                let mut ids_vec: Vec<i64> = Vec::new();
                for i in 0..sel_ref.n_items() {
                    if sel_ref.is_selected(i) {
                        if let Some(obj) = sel_ref
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            ids_vec.push(obj.borrow::<crate::media_library::LibTrack>().id);
                        }
                    }
                }
                if ids_vec.is_empty() {
                    return;
                }
                let ids_set: std::collections::HashSet<i64> =
                    ids_vec.iter().copied().collect();
                let n_items = store_ref.n_items();

                // Build the kept list and splice in one shot — a single
                // items-changed signal instead of one per removed row.
                // This is the same pattern used by rebuild_files/search and
                // avoids blocking the main thread on large selections.
                let kept: Vec<glib::Object> = (0..n_items)
                    .filter_map(|i| store_ref.item(i))
                    .filter(|obj| {
                        obj.downcast_ref::<glib::BoxedAnyObject>()
                            .map(|b| !ids_set.contains(
                                &b.borrow::<crate::media_library::LibTrack>().id,
                            ))
                            .unwrap_or(true)
                    })
                    .collect();
                let removed = n_items as usize - kept.len();
                store_ref.splice(0, n_items, &kept);

                status_ref.set_text(&format!(
                    "Removed {removed} track{}. {} tracks in library",
                    if removed == 1 { "" } else { "s" },
                    kept.len(),
                ));

                // Soft-delete in background, then purge — same pattern as
                // folder removal.  Opens its own DB connection because
                // rusqlite::Connection is not Send.
                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                std::thread::spawn(move || {
                    if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                        let _ = lib.soft_delete_tracks(&ids_vec);
                        let _ = lib.purge_deleted_tracks();
                    }
                });
            });
        }

        stack.add_named(&files_vbox, Some("files"));
        let rf = rebuild_files.clone();
        state.borrow_mut().rebuild_ml_callback = Some(Rc::new(move || {
            rf();
        }));

        // ── Sidebar routing ──────────────────────────────────────────────
        // This page's own row-selected handler. Until 2026-08-10 the Files,
        // Albums and Playlists branches shared one handler that lived with
        // the Playlists page, purely because that is where the code happened
        // to sit; `sidebar.rs`'s doc has always said routing belongs to the
        // page it routes to. Splitting it is what let the Playlists page be
        // extracted without dragging the other two pages' navigation along.
        //
        // Every handler on this signal keys off a disjoint `widget_name`
        // prefix with no catch-all branch, so all of them run on every
        // selection and only one acts. That is why registration order carries
        // no meaning here — see the same note in disc_page and devices_page.
        {
            let stack_ref = stack.clone();
            let state_rc = state.clone();
            let album_filter_sb = ctx.album_filter.clone();
            let btn_album_back_sb = ctx.btn_album_back.clone();
            let files_filtered_sb = files_filtered.clone();
            sb.list.connect_row_selected(move |_, opt_row| {
                let Some(row) = opt_row else { return };
                if row.widget_name() != "files" {
                    return;
                }
                // Explicitly returning to Files always means "show the full
                // library" — clear any album drill-down left over from the
                // gallery (Phase 11 A5) and rebuild through the same seam
                // background rebuilds use.
                //
                // Only when the table is actually showing one, though. When it
                // already lists every track, rebuilding is a full pass over
                // the library — 474 ms at 37k tracks — and the window paid
                // that on every open, because the initial `select_row(0)`
                // fires this handler against a table `files::build` has only
                // just filled (2026-08-11).
                //
                // The test is `files_filtered`, not whether `album_filter` is
                // set: "◀ Albums" clears the filter without touching this
                // table, so the filter is already None by the time we get here
                // and the table is still showing one album (2026-08-11).
                let stale = files_filtered_sb.get();
                album_filter_sb.borrow_mut().take();
                btn_album_back_sb.set_visible(false);
                stack_ref.set_visible_child_name("files");
                if stale {
                    let cb = state_rc.borrow().rebuild_ml_callback.clone();
                    if let Some(cb) = cb {
                        cb();
                    }
                }
            });
        }
}

#[cfg(test)]
mod file_status_tests {
    use super::*;

    /// A file that is gone must read as missing, not as "changed".
    ///
    /// `needs_metadata_scan` answers `true` for a path it cannot stat — it
    /// cannot tell "modified since the scan" from "no longer there" — so a
    /// deleted track used to show 🔄 with the tooltip "rescan to refresh its
    /// metadata". The playlist marks the same file ⚠. Two views, one file,
    /// opposite stories.
    #[test]
    fn a_deleted_file_reads_as_missing_not_changed() {
        let status = probe_file_status("/no/such/file.mp3", false, Some("2026-01-01T00:00:00Z"), None);
        assert_eq!(status, FileStatus::Missing);
        assert_eq!(status.glyph(), "⚠");
    }

    /// Missing is checked before anything that touches the file, so a path
    /// that cannot be stat'd never reaches the read-only or mtime probes.
    #[test]
    fn missing_is_decided_before_the_other_probes() {
        // No stored mtime and no scan timestamp would normally mean "changed".
        assert_eq!(probe_file_status("/no/such/file.mp3", false, None, None), FileStatus::Missing);
    }

    /// A writable file that has not changed shows nothing at all.
    #[test]
    fn an_unchanged_writable_file_is_clean() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().into_owned();
        let mtime = crate::timeutil::format_system_time(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
        );
        let status = probe_file_status(&path, false, Some("2026-01-01T00:00:00Z"), Some(&mtime));
        assert_eq!(status, FileStatus::Clean);
        assert_eq!(status.glyph(), "");
    }

    /// A read-only file reports read-only. Changing permissions does not touch
    /// mtime, so the file still counts as unchanged and the lock is what the
    /// user needs to see.
    #[test]
    fn a_read_only_file_reports_read_only() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().into_owned();
        let mtime = crate::timeutil::format_system_time(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
        );
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        let status = probe_file_status(&path, false, Some("2026-01-01T00:00:00Z"), Some(&mtime));
        // Running as root defeats the permission bits entirely; skip rather
        // than assert something the kernel will not honour.
        if !crate::media_library::is_read_only(std::path::Path::new(&path)) {
            eprintln!("write access survives chmod (running as root?) — skipping");
            return;
        }
        assert_eq!(status, FileStatus::ReadOnly);
        assert_eq!(status.glyph(), "🔒");
    }

    /// A file that vanishes before it was ever scanned still reads as missing.
    /// The ❓ says "metadata loads on the next scan", which is not going to
    /// happen for a file that is gone.
    #[test]
    fn an_unscanned_file_that_has_gone_reads_as_missing() {
        assert_eq!(
            probe_file_status("/no/such/file.mp3", true, None, None),
            FileStatus::Missing
        );
    }

    /// But an unscanned file that is still there keeps its ❓ — that indicator
    /// matters, and nothing about a never-read row justifies replacing it.
    #[test]
    fn an_unscanned_file_that_is_present_stays_unscanned() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().into_owned();
        let status = probe_file_status(&path, true, None, None);
        assert_eq!(status, FileStatus::Unscanned);
        assert_eq!(status.glyph(), "❓");
    }
}
