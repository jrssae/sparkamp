/// Bottom status bar for a Media Library list view: `N tracks · MM:SS total ·
/// MM:SS selected`, matching the active playlist. Works over any MultiSelection
/// whose items are `BoxedAnyObject<T>`; `secs_of` pulls each row's duration out
/// of its `T` (e.g. `LibTrack::length_secs`, `disc::mount::DiscFile::
/// duration_secs`) since the Devices/Files/Playlists views box `LibTrack` rows
/// but the Discs data-file browser boxes `DiscFile` rows instead. Returns the
/// Label (append it to the view's page box) and a refresh closure (already
/// wired to selection + model changes; also call it once after the store is
/// first populated).
fn ml_status_bar_for<T: 'static>(
    selection: &MultiSelection,
    secs_of: impl Fn(&T) -> Option<f64> + 'static,
) -> (Label, std::rc::Rc<dyn Fn()>) {
    let label = Label::builder()
        .halign(Align::Start)
        .css_classes(["status-label"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .margin_start(8)
        .margin_end(8)
        .margin_top(1)
        .margin_bottom(5)
        .build();
    let refresh: std::rc::Rc<dyn Fn()> = {
        let label = label.clone();
        let selection = selection.clone();
        std::rc::Rc::new(move || {
            let n = selection.n_items();
            let (mut count, mut total, mut sel_n, mut sel_secs) = (0usize, 0u64, 0usize, 0u64);
            for i in 0..n {
                let Some(obj) = selection.item(i) else { continue };
                let Ok(bx) = obj.downcast::<glib::BoxedAnyObject>() else { continue };
                let t = bx.borrow::<T>();
                let secs = secs_of(&t).unwrap_or(0.0).max(0.0) as u64;
                count += 1;
                total += secs;
                if selection.is_selected(i) {
                    sel_n += 1;
                    sel_secs += secs;
                }
            }
            let sel = if sel_n > 0 { Some((sel_n, sel_secs)) } else { None };
            label.set_text(&crate::playlist_status::playlist_status_line(count, total, sel));
        })
    };
    selection.connect_selection_changed({
        let r = refresh.clone();
        move |_, _, _| r()
    });
    selection.connect_items_changed({
        let r = refresh.clone();
        move |_, _, _, _| r()
    });
    refresh();
    (label, refresh)
}

/// LibTrack-boxed views (Files, Devices, Playlists) — the common case.
fn ml_status_bar(selection: &MultiSelection) -> (Label, std::rc::Rc<dyn Fn()>) {
    ml_status_bar_for::<crate::media_library::LibTrack>(selection, |t| t.length_secs)
}

/// A late-bound callback: declared empty, filled once the widget it drives
/// exists. This is the shape the file already used twenty-four times over
/// (`refresh_discs_holder`, `col_view_holder`, `files_status_holder`, …) to
/// break "closure A needs widget B, which is built by closure A" cycles.
/// Naming it makes it the explicit contract between pages once they live in
/// separate modules — see docs/gtk-breakup-plan.md §3.1.
///
/// A holder left at `None` is not an error: every call site silently does
/// nothing. That is the failure mode the smoke tests in §5 exist to catch.
type RefreshHolder = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// Device-copy runner, shared with player.rs so the active playlist's
/// Send-to menu drives the same copy as the Media Library's device views.
type CopyFilesHolder =
    Rc<RefCell<Option<Rc<dyn Fn(crate::devices::Device, Vec<std::path::PathBuf>)>>>>;

/// Everything the Media Library window receives from its host.
///
/// All of it is owned by player.rs's `build()` and shared rather than copied,
/// so the active playlist's Send-to menu sees the same drives, devices, burn
/// queues and copy runner as this window's Files, Editor and Device views
/// (Task 8).
///
/// This replaces eight positional parameters. Everything here crosses the
/// window boundary; everything a page needs from *inside* the window lives on
/// [`MlCtx`] instead, which wraps this.
///
/// No visibility modifier: `media_library.rs` is `include!`d into
/// `window/mod.rs`, so this is already the same module as its callers. Step 8
/// of the plan converts those includes to real `mod`s, and that is when
/// `pub(super)` becomes meaningful here.
struct MlHost {
    state: Rc<RefCell<AppState>>,
    rebuild_playlist: Rc<dyn Fn()>,
    set_track: Rc<dyn Fn(&str)>,
    current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>>,
    current_devices: Rc<RefCell<Vec<crate::devices::Device>>>,
    burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>>,
    copy_files_holder: CopyFilesHolder,
    /// Filled by the burn panel with a closure that re-renders the shown
    /// drive's queue; the Send-to ▸ Disc Drive actions call it so an external
    /// add updates the open panel live (2026-07-16).
    burn_refresh_holder: RefreshHolder,
}

/// What an extracted page builder is handed: the host bundle plus the shared
/// window chrome the page attaches itself to.
///
/// The split exists because the two halves are born in different places.
/// `MlHost` is built by player.rs before this window is opened; the chrome
/// below does not exist until `open_media_library_window` has built it, so it
/// cannot be a parameter. `MlCtx` is therefore assembled part-way down that
/// function, once its fields exist, and borrowed by each page from there on.
///
/// Fields join as a page actually needs them rather than up front — the
/// sidebar is still absent, because the pages extracted so far each want a
/// different slice of it and take it as a second `&Sidebar` argument instead.
/// The test for whether something belongs here is the plan's (§3.2): is it
/// touched by more than one stack page? Every field below is — Files and
/// Albums share the drill-down filter and its back button, and every page
/// parents dialogs to the window and adds itself to the stack. State touched
/// by one page only stays in that page's module.
///
/// Cross-page *behaviour* does not belong here either: the two runners the
/// device page owns and Files and the editor call (copy loose files, send a
/// whole playlist) reach their callers through the holders already on
/// [`MlHost`] and [`Sidebar`], not through a field. That indirection is what
/// lets the device page be built after the pages that call it.
struct MlCtx {
    host: MlHost,
    /// The window itself — pages parent their dialogs and file choosers to it.
    win: gtk4::Window,
    /// The page stack. Pages `add_named` themselves to it and switch to each
    /// other through it.
    stack: Stack,
    /// The gallery drill-down: `Some((album, album_artist))` narrows the Files
    /// page to one album's tracks. Written by Albums, read by Files.
    album_filter: Rc<RefCell<Option<(String, String)>>>,
    /// "◀ Albums" — lives in the Files search row but is shown and hidden by
    /// the drill-down, so both pages touch it.
    btn_album_back: Button,
    /// The Files `ColumnView` and its columns, late-bound because both are
    /// built inside the Files page but the window's close-request has to read
    /// their order and widths back out to save them.
    col_view_holder: Rc<RefCell<Option<ColumnView>>>,
    all_cols_holder: Rc<RefCell<Vec<(String, ColumnViewColumn)>>>,
}

fn open_media_library_window(
    parent: Option<&gtk4::Window>,
    host: MlHost,
    init_width: i32,
    init_height: i32,
) -> gtk4::Window {
    // The body below still refers to these by their original names. Rebinding
    // rather than rewriting ~1,450 capture sites keeps this a pure signature
    // change with nothing to review for behaviour; each later extraction drops
    // the alias it no longer needs and takes the field from `ctx` instead.
    // Cloning an `Rc` is an integer increment, and `host` stays whole so it can
    // be moved into the `MlCtx` built further down.
    let state = host.state.clone();
    let rebuild_playlist = host.rebuild_playlist.clone();
    let set_track = host.set_track.clone();
    let current_drives = host.current_drives.clone();
    let current_devices = host.current_devices.clone();
    let burn_queues = host.burn_queues.clone();
    let copy_files_holder = host.copy_files_holder.clone();
    let burn_refresh_holder = host.burn_refresh_holder.clone();

    let win = gtk4::Window::new();
    win.set_title(Some("Media Library — Sparkamp"));
    win.set_default_size(init_width, init_height);
    win.set_resizable(true);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    let paned = Paned::new(Orientation::Horizontal);
    paned.set_margin_top(8);
    paned.set_margin_bottom(8);
    paned.set_margin_start(8);
    paned.set_margin_end(8);

    // ── Left sidebar ──────────────────────────────────────────────────────
    // Built in `window/sidebar.rs` (plan step 3): the ListBox, its DropTarget,
    // the five static rows and the three expand/collapse chevrons.
    let sb = sidebar::build(&host);
    // Aliases so the ~80 references below keep their original names, the same
    // device step 1 used for MlHost's fields. Each later extraction takes what
    // it needs from `sb` and drops the alias it stops using.
    let sidebar = sb.list.clone();
    let sidebar_scroll = sb.scroll.clone();
    let send_playlist_holder = sb.send_playlist_holder.clone();
    let playlists_expanded = sb.playlists_expanded.clone();
    let pl_sub_rows = sb.pl_sub_rows.clone();


    let _vsep_unused = (); // replaced by Paned divider


    // ── Content stack ─────────────────────────────────────────────────────
    // Every page `add_named`s itself below, Devices included — it is the last
    // one added rather than the first, which is a page-order change only:
    // `Stack` shows a child by name, and the first row the sidebar selects is
    // Files.
    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(StackTransitionType::None);

    // Holders so close_request can save Files-tab state (col_view and all_cols are
    // defined inside the Files block scope below).
    let col_view_holder: Rc<RefCell<Option<ColumnView>>> = Rc::new(RefCell::new(None));
    let all_cols_holder: Rc<RefCell<Vec<(String, ColumnViewColumn)>>> =
        Rc::new(RefCell::new(Vec::new()));

    // Phase 11 A5: shared album→Files drill-down filter. `None` means the
    // normal search/all-tracks Files view; `Some((album, album_artist))`
    // means Files is showing just that album's tracks (set by the gallery's
    // on_album_activate below). Cleared by re-selecting the "Files" sidebar
    // row or by typing in the search box, so the user always has a way back
    // to the full library. Declared here (before `rebuild_files` and before
    // the sidebar wiring, both of which need it) rather than on AppState —
    // it's pure UI navigation state local to this window.
    let album_filter: Rc<RefCell<Option<(String, String)>>> = Rc::new(RefCell::new(None));

    // Back button that returns from an album's track list to the gallery
    // overview. Lives at the left of the Files search row; shown only while
    // `album_filter` is active (i.e. the user drilled in from the gallery),
    // hidden otherwise. Its click handler is connected further down, once
    // `rebuild_gallery` exists (the shared `show_gallery_overview` closure).
    let btn_album_back = Button::with_label("◀ Albums");
    btn_album_back.add_css_class("pl-btn");
    btn_album_back.set_visible(false);

    // Every field a page builder needs now exists, so the page context can be
    // built. `host` is moved in — the eight aliases at the top were cloned off
    // it, so nothing below depends on it by that name.
    let ctx = MlCtx {
        host,
        win: win.clone(),
        stack: stack.clone(),
        album_filter: album_filter.clone(),
        btn_album_back: btn_album_back.clone(),
        col_view_holder: col_view_holder.clone(),
        all_cols_holder: all_cols_holder.clone(),
    };

    // ── Page: Files ──────────────────────────────────────────────────────
    // Extracted to `window/files.rs` (plan step 4). Builds the table, search
    // row, status bar and row context menu, and adds itself to the stack.
    files::build(&ctx);

    // Every field this needs now exists, so the page context can be built.
    // `host` is moved in — the eight aliases above were cloned off it at the
    // top, so nothing below depends on it by that name any more.
    // ── Page: Albums (Phase 11 A5 — gallery grid, Task 4) ──────────────────
    // Extracted to `window/albums.rs` (plan step 2). Adds itself to the stack
    // and returns the shared "back to the gallery overview" closure, which the
    // sidebar wiring further down also calls.
    let show_gallery_overview = albums::build(&ctx);

    // ── Page: Playlists ──────────────────────────────────────────────────
    //
    // Two sub-pages within the "playlists" stack page:
    //   "pl-manage" – full-width list of saved playlists + New/Rename/Delete
    //   "pl-edit"   – track editor for the selected playlist
    //
    // pl_sub_stack is stored in an Rc so the sidebar wiring can switch pages.
    let pl_sub_stack: Rc<Stack> = Rc::new({
        let s = Stack::new();
        s.set_hexpand(true);
        s.set_vexpand(true);
        s.set_transition_type(StackTransitionType::None);
        s
    });

    // Shared: currently-editing playlist id and LibTrack list
    let editing_tracks: Rc<RefCell<Vec<crate::media_library::LibTrack>>> =
        Rc::new(RefCell::new(Vec::new()));
    let saved_track_ids: Rc<RefCell<Vec<i64>>> = Rc::new(RefCell::new(Vec::new()));
    // The DB row id of the playlist currently open in the editor (-1 = none)
    let editing_pl_id: Rc<Cell<i64>> = Rc::new(Cell::new(-1));

    // "Send to" and row-scoped actions for the editor's per-cell context
    // menu. Task 8 originally built this as a flat Popover of plain Buttons
    // because a PopoverMenu parented on the ColumnView cell tree lost
    // dispatch. The Files-view menu (~col_view, "ml" prefix) proves the
    // real fix: put the SimpleActionGroup on the SAME stable widget the
    // PopoverMenu is parented to (single widget, no ancestor walk) instead
    // of scattering it across track_list/win as the abandoned "ple" group
    // above did. Here that stable widget is the editor's ScrolledWindow
    // (`track_scroll`, exposed via `track_scroll_holder` since it doesn't
    // exist yet at this point in the function) — see its
    // `insert_action_group("ed", ...)` call right after it's built. The
    // group is *also* inserted directly on each popped-up PopoverMenu
    // instance (see the per-cell gesture) as defense in depth: the
    // `ple_action_group_holder` comment above documents a GTK4 version
    // where the NESTED PopoverMenu flag breaks the ancestor-chain walk
    // entirely, and installing the group straight on the popup sidesteps
    // that regardless of GTK version.
    // Canonical play-order indices (selection, or the single clicked row
    // as fallback) captured once per right-click so every "ed.*" action —
    // not just send-drive/send-device — can read row-scoped context
    // without needing per-item closures. Still used by the row-scoped
    // "Replace Current Playlist" / "Edit ID3" / "Remove" items, which are
    // right-click-only (never exposed on the "Send to ▾" button below).
    let ed_ctx_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    // Live "currently selected editor rows" reader for the actions that
    // ARE exposed on the "Send to ▾" button (send-active, add-to-new,
    // add-to-saved, send-drive, send-device): the button doesn't go
    // through the per-cell right-click gesture, so it must read the
    // editor's own MultiSelection directly at dispatch time instead of a
    // stash the gesture populates (same G1 fix as the files view's "Send
    // to ▾" button). `edit_multi_sel` doesn't exist yet at this point in
    // the function, so a holder defers the actual model until it's built
    // below (filled in right after `edit_multi_sel` is constructed).
    let edit_multi_sel_holder: Rc<RefCell<Option<gtk4::MultiSelection>>> =
        Rc::new(RefCell::new(None));
    let ed_selected_tracks: Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>> = {
        let sel_holder = edit_multi_sel_holder.clone();
        Rc::new(move || {
            let Some(sel) = sel_holder.borrow().clone() else { return Vec::new() };
            let mut out = Vec::new();
            for i in 0..sel.n_items() {
                if sel.is_selected(i) {
                    if let Some(obj) = sel
                        .item(i)
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    {
                        out.push(obj.borrow::<EditorEntry>().track.clone());
                    }
                }
            }
            out
        })
    };
    // Quiet status line (G3) — the editor's Send to Disc Drive reports
    // here instead of dropping the success message on the floor.
    let ed_status = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(6)
        .margin_end(6)
        .margin_bottom(2)
        .build();
    ed_status.add_css_class("status-label");
    let ed_action_group = gio::SimpleActionGroup::new();
    // Kept as named bindings (not scoped to a block) so later code in this
    // function (the "ed"-group insertion on track_scroll, and the
    // additional actions registered further down once
    // rebuild_track_list_holder/track_scroll_holder exist) can add more
    // actions to the same group.
    let ed_action_drive = gio::SimpleAction::new("send-drive", Some(glib::VariantTy::STRING));
    let ed_action_device = gio::SimpleAction::new("send-device", Some(glib::VariantTy::STRING));
    {
        let state_burn = state.clone();
        let burn_queues = burn_queues.clone();
        let burn_refresh_holder = burn_refresh_holder.clone();
        let current_drives = current_drives.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let win_wk = win.downgrade();
        let status = ed_status.clone();
        ed_action_drive.connect_activate(move |_, target| {
            let Some(drive_id) = target.and_then(|v| v.get::<String>()) else { return };
            let drive_label = current_drives
                .borrow()
                .iter()
                .find(|d| d.id == drive_id)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| drive_id.clone());
            // Live selection at dispatch (G1) — read straight from the
            // editor's MultiSelection, not a right-click stash, so the
            // "Send to ▾" button sees the actual current selection.
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter().map(|t| std::path::PathBuf::from(&t.path)).collect();
            // Metadata from the library NOW (SQLite is not Send).
            let metas: std::collections::HashMap<_, _> = {
                let s = state_burn.borrow();
                paths.iter().map(|path| {
                    let row = s.media_lib.as_ref().and_then(|l| {
                        l.track_by_path(&path.display().to_string()).ok()
                    });
                    let display = row.as_ref()
                        .map(|t| match (&t.artist, &t.title) {
                            (Some(a), Some(ti)) if !a.is_empty() =>
                                format!("{a} - {ti}"),
                            (_, Some(ti)) => ti.clone(),
                            _ => t.filename.clone(),
                        })
                        .unwrap_or_else(|| path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string()));
                    let secs = row.as_ref()
                        .and_then(|t| t.length_secs).map(|s| s as u32);
                    let bytes = std::fs::metadata(path)
                        .map(|m| m.len()).unwrap_or(0);
                    (path.clone(), (display, secs, bytes))
                }).collect()
            };
            let status = status.clone();
            queue_paths_to_drive(
                drive_id,
                drive_label,
                paths,
                metas,
                burn_queues.clone(),
                burn_refresh_holder.clone(),
                Rc::new(move |s: String| status.set_text(&gtk_safe(&s))),
                win_wk.clone(),
            );
        });
        ed_action_group.add_action(&ed_action_drive);
    }
    {
        // Send to Removable Device: hand off to the Files view's copy
        // runner via the shared holder (populated once the Device view's
        // widgets exist — see copy_files_holder's own doc comment).
        let current_devices = current_devices.clone();
        let copy_files_holder = copy_files_holder.clone();
        let sel_tracks = ed_selected_tracks.clone();
        ed_action_device.connect_activate(move |_, target| {
            let Some(dev_id) = target.and_then(|v| v.get::<String>()) else { return };
            let dev = current_devices
                .borrow()
                .iter()
                .find(|d| d.id == dev_id)
                .cloned();
            // Live selection at dispatch (G1).
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter().map(|t| std::path::PathBuf::from(&t.path)).collect();
            if let (Some(dev), false) = (dev, paths.is_empty()) {
                if let Some(run) = copy_files_holder.borrow().clone() {
                    run(dev, paths);
                }
            }
        });
        ed_action_group.add_action(&ed_action_device);
    }
    win.insert_action_group("ed", Some(&ed_action_group));

    // Widget handles for pl-manage playlist list (shared with sidebar)
    let pl_manage_list: Rc<ListBox> = Rc::new({
        let lb = ListBox::new();
        lb.add_css_class("playlist");
        lb.set_selection_mode(gtk4::SelectionMode::Single);
        lb.set_vexpand(true);
        lb
    });

    // Canonical play-order index of the row most recently right-clicked
    // in the editor; the ple.edit-id3 / ple.remove actions read this when
    // they need a single row to operate on.  Used instead of LibTrack.id
    // so duplicate entries (same track listed several times in the
    // playlist file) can be disambiguated by position.
    let ctx_canonical_idx: Rc<Cell<i64>> = Rc::new(Cell::new(-1));

    // Canonical play-order indices selected for an in-progress drag from
    // the editor.  Populated by the per-cell DragSource at prepare time
    // and consumed by the editor DropTarget when handling a reorder.
    // Cleared on every new drag prepare so a previous drag's selection
    // can't leak into a subsequent unrelated drop.
    let drag_selection: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Path → first canonical slot.  Used by the editor DropTarget when a
    // cross-window drop ships only paths (no canonical indices) and we
    // need to know whether every dropped path is already in the playlist.
    // For duplicates only the first slot is recorded; the drag_selection
    // path is preferred when the drag originated in the editor itself.
    let position_map: Rc<RefCell<std::collections::HashMap<String, usize>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // Wrapper put into the editor's ListStore.  Carrying `canonical_idx`
    // alongside the track lets every cell — even duplicates of the same
    // file in the playlist — bind to its own play-order slot, so the
    // position column reads the correct row instead of all duplicates
    // collapsing onto the last occurrence's index.  Cloned cheaply on
    // splice because `LibTrack` is `Clone` already.
    #[derive(Clone)]
    struct EditorEntry {
        track: crate::media_library::LibTrack,
        canonical_idx: usize,
    }

    // True when the editor's current display sort allows intra-list drag
    // reorder (only the canonical play-order ascending state preserves the
    // bijection between display index and play-order index).  Flipped by
    // a sorter-change handler installed once the ColumnView exists.
    let reorder_allowed: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    // Track editor: ListStore → SortListModel → MultiSelection → ColumnView.
    // Sort lives in the SortListModel so the user's column-header clicks
    // produce a display-only sort.  `editing_tracks` (the canonical play
    // order) is never reordered by sort — Save always writes that order.
    let edit_store: gio::ListStore = gio::ListStore::new::<glib::BoxedAnyObject>();
    // Per-view search over this playlist's rows: store → filter → sort →
    // selection. Rows keep their canonical_idx, so delete/context actions
    // stay correct under a filter; drag-reorder is refused while one is
    // active (display order no longer maps onto play order).
    let pl_edit_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let edit_filter = gtk4::CustomFilter::new({
        let q = pl_edit_query.clone();
        move |obj| {
            let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                return true;
            };
            lib_track_matches_query(&boxed.borrow::<EditorEntry>().track, &q.borrow())
        }
    });
    let edit_filter_model =
        gtk4::FilterListModel::new(Some(edit_store.clone()), Some(edit_filter.clone()));
    // Search filters just this playlist's rows (drag-reorder pauses while a
    // query is active — see the drop handler). Created here so
    // load_pl_by_id can clear it when a different playlist opens; packed
    // into the pl-edit page below.
    let (pl_search_row, pl_search_entry) =
        make_view_search_row("Search this playlist — artist, title, album…");
    // F12.1: restore this view's last search query if the feature is on.
    if state.borrow().config.media_library.remember_search {
        let last = state.borrow().config.media_library.last_search.get("playlists").cloned();
        if let Some(last) = last {
            pl_search_entry.set_text(&last);
        }
    }
    {
        // 150 ms debounce — same rationale as the device search: the filter
        // walks every row per change, heavy on multi-thousand-row playlists.
        let q = pl_edit_query.clone();
        let filter = edit_filter.clone();
        let state_rc = state.clone();
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        pl_search_entry.connect_changed(move |e| {
            let raw_text = e.text().to_string();
            let text = raw_text.to_lowercase();
            if let Some(src) = pending.borrow_mut().take() {
                src.remove();
            }
            let q = q.clone();
            let filter = filter.clone();
            let state_inner = state_rc.clone();
            let pending_inner = pending.clone();
            let src = glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                *q.borrow_mut() = text.clone();
                filter.changed(gtk4::FilterChange::Different);
                // F12.1: remember this view's query for next open.
                {
                    let mut s = state_inner.borrow_mut();
                    if s.config.media_library.remember_search {
                        s.config
                            .media_library
                            .last_search
                            .insert("playlists".to_string(), raw_text.clone());
                    }
                }
                pending_inner.borrow_mut().take();
                glib::ControlFlow::Break
            });
            *pending.borrow_mut() = Some(src);
        });
    }
    let edit_sort_model = gtk4::SortListModel::new(
        Some(edit_filter_model),
        None::<gtk4::Sorter>,
    );
    let edit_multi_sel: gtk4::MultiSelection =
        gtk4::MultiSelection::new(Some(edit_sort_model.clone()));
    // Fill the deferred holder now that the real model exists — see its
    // declaration above (`edit_multi_sel_holder`) for why this is deferred.
    *edit_multi_sel_holder.borrow_mut() = Some(edit_multi_sel.clone());
    let track_list: Rc<gtk4::ColumnView> = Rc::new({
        let cv = gtk4::ColumnView::new(Some(edit_multi_sel.clone()));
        cv.add_css_class("playlist");
        cv.set_vexpand(true);
        cv.set_show_row_separators(false);
        cv.set_show_column_separators(false);
        cv
    });

    // ── Editor columns: walk ALL_COLUMNS so files view + editor stay in
    //    lock-step on which columns exist and which order they default to.
    // Position column reference is captured here so the sorter-change
    // listener below can detect when the user has selected position-ASC
    // (the only sort that allows intra-list drag-reorder).
    let pos_col_holder: Rc<RefCell<Option<ColumnViewColumn>>> = Rc::new(RefCell::new(None));
    // Editor named columns (skipping the leading status + position pinned
    // columns) — captured so we can apply the files-view saved order so
    // the user only has to arrange columns in one place.
    let mut editor_named_cols: Vec<(String, ColumnViewColumn)> = Vec::new();
    // Holder for the rebuild closure — populated right after the closure
    // is defined.  Cell factories install per-cell drop targets that need
    // to refresh the editor after a successful reorder, but those factory
    // setups live above the rebuild definition in source order.
    type RebuildClosure = Rc<dyn Fn()>;
    let rebuild_track_list_holder: Rc<RefCell<Option<RebuildClosure>>> =
        Rc::new(RefCell::new(None));

    // Holder for the editor's "ple" action group.  Cell factories pop
    // PopoverMenus parented to track_list; the popover's action lookup
    // walks the GTK widget chain back to track_list where the group is
    // also attached, but some GTK4 versions break that walk with the
    // NESTED PopoverMenu flag.  Installing the group directly on each
    // popup makes dispatch reliable regardless of GTK version.
    let ple_action_group_holder: Rc<RefCell<Option<gio::SimpleActionGroup>>> =
        Rc::new(RefCell::new(None));
    // Holder for the editor's ScrolledWindow — populated right after it
    // is built so the cell right-click handler can use it as the popover
    // parent (cell-label parents render invisible on this GTK4 build), and
    // so the "ed" action group can be installed on it once (see below).
    let track_scroll_holder: Rc<RefCell<Option<gtk4::ScrolledWindow>>> =
        Rc::new(RefCell::new(None));

    // Row-scoped "ed" actions for the per-cell context menu's non-Send-to
    // items (Replace Current Playlist, Edit/View ID3, Remove from
    // Playlist) plus the Send-to family's Active Playlist / Saved
    // Playlist entries (send-drive/send-device were already registered
    // above). Registered here — after rebuild_track_list_holder and
    // track_scroll_holder exist — because `remove` needs the rebuild
    // closure holder. All read row context from `ed_ctx_indices`
    // (selection, falling back to the single clicked row), populated once
    // per right-click by the per-cell gesture below instead of being
    // recomputed per menu item.
    {
        // Send to Active Playlist — reachable from both the per-row
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_c = set_track.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let action = gio::SimpleAction::new("send-active", None);
        action.connect_activate(move |_, _| {
            let tracks = sel_tracks();
            if tracks.is_empty() { return }
            let was_empty = state_c.borrow().playlist.is_empty();
            let autoplay = state_c.borrow().config.behavior.autoplay_on_add;
            {
                let mut s = state_c.borrow_mut();
                for lt in &tracks { s.playlist.add(crate::model::Track::from(lt)); }
            }
            if autoplay && was_empty {
                if let Some(d) = state_c.borrow_mut().play_current() { set_track_c(&d); }
            }
            rebuild_pl();
        });
        ed_action_group.add_action(&action);
    }
    {
        // Replace Current Playlist — same body as the old flat button.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_c = set_track.clone();
        let idxs_src = ed_ctx_indices.clone();
        let action = gio::SimpleAction::new("replace", None);
        action.connect_activate(move |_, _| {
            let tracks: Vec<crate::media_library::LibTrack> = {
                let et_b = et.borrow();
                idxs_src.borrow().iter().filter_map(|&i| et_b.get(i).cloned()).collect()
            };
            if tracks.is_empty() { return }
            let autoplay = state_c.borrow().config.behavior.autoplay_on_add;
            {
                let mut s = state_c.borrow_mut();
                let _ = s.player.stop();
                s.playlist = crate::model::Playlist::new();
                for lt in &tracks { s.playlist.add(crate::model::Track::from(lt)); }
            }
            if autoplay {
                if let Some(d) = state_c.borrow_mut().play_current() { set_track_c(&d); }
            }
            rebuild_pl();
        });
        ed_action_group.add_action(&action);
    }
    {
        // Edit / View ID3 — single-row only, reads the clicked row (not
        // the selection) via ctx_canonical_idx, same as the old flat button.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let ctx_c = ctx_canonical_idx.clone();
        let action = gio::SimpleAction::new("edit-id3", None);
        action.connect_activate(move |_, _| {
            let c = ctx_c.get();
            if c < 0 { return }
            let path = et.borrow().get(c as usize).map(|t| t.path.clone());
            let Some(path) = path else { return };
            open_id3_editor_window(
                None::<&gtk4::Window>,
                path.into(),
                state_c.clone(),
                rebuild_pl.clone(),
                None,
                None,
            );
        });
        ed_action_group.add_action(&action);
    }
    {
        // View/Search Lyrics (F15) on saved-playlist editor rows.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let ctx_c = ctx_canonical_idx.clone();
        let action = gio::SimpleAction::new("lyrics", None);
        action.connect_activate(move |_, _| {
            let c = ctx_c.get();
            if c < 0 { return }
            let t = et.borrow().get(c as usize).map(|t| {
                (
                    std::path::PathBuf::from(&t.path),
                    t.artist.clone().unwrap_or_default(),
                    t.title.clone().unwrap_or_default(),
                    t.album_artist.clone().unwrap_or_default(),
                )
            });
            let Some((path, artist, title, album_artist)) = t else { return };
            view_or_search_lyrics(&state_c, &path, &artist, &title, &album_artist, rebuild_pl.clone(), LyricsMode::Specific);
        });
        ed_action_group.add_action(&action);
    }
    {
        // View Album Art for the right-clicked editor row.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let ctx_c = ctx_canonical_idx.clone();
        let action = gio::SimpleAction::new("view-art", None);
        action.connect_activate(move |_, _| {
            let c = ctx_c.get();
            if c < 0 { return }
            let path = et.borrow().get(c as usize).map(|t| std::path::PathBuf::from(&t.path));
            if let Some(path) = path { art_window::open_track_art(&state_c, &path); }
        });
        ed_action_group.add_action(&action);
    }
    {
        // Remove from Playlist — same body as the old flat button.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let ep_id = editing_pl_id.clone();
        let rb_holder = rebuild_track_list_holder.clone();
        let idxs_src = ed_ctx_indices.clone();
        let action = gio::SimpleAction::new("remove", None);
        action.connect_activate(move |_, _| {
            let mut idxs = idxs_src.borrow().clone();
            if idxs.is_empty() { return }
            idxs.sort_unstable_by(|a, b| b.cmp(a));
            {
                let mut e = et.borrow_mut();
                for i in idxs.iter() {
                    if *i < e.len() { e.remove(*i); }
                }
            }
            let pid = ep_id.get();
            if pid >= 0 {
                let s = state_c.borrow();
                if let Some(lib) = s.media_lib.as_ref() {
                    let paths: Vec<String> = et.borrow()
                        .iter().map(|t| t.path.clone()).collect();
                    if let Ok(pl) = lib.playlist_by_id(pid) {
                        let _ = lib.save_playlist_tracks_to_path(
                            std::path::Path::new(&pl.path),
                            &paths,
                        );
                    }
                }
            }
            if let Some(rb) = rb_holder.borrow().as_ref() { rb(); }
        });
        ed_action_group.add_action(&action);
    }
    {
        // Seed a brand new saved playlist — reachable from both the
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let win_c = win.clone();
        let action = gio::SimpleAction::new("add-to-new", None);
        action.connect_activate(move |_, _| {
            let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
            if paths.is_empty() { return }
            let default_stem = glib::DateTime::now_local().ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = state_c.clone();
            let paths_cb = paths.clone();
            run_playlist_save_dialog(
                state_c.clone(),
                win_c.clone(),
                &default_stem,
                move |path, win_cb| {
                    if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                        }
                    }
                },
            );
        });
        ed_action_group.add_action(&action);
    }
    {
        // Append to an existing saved playlist — reachable from both the
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let action = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
            if paths.is_empty() { return }
            let mut ok = false;
            if let Some(lib) = state_c.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_) => ok = true,
                    Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                }
            }
            if ok { notify_playlist_changed(pid); }
        });
        ed_action_group.add_action(&action);
    }
    {
        let visible_ids: Vec<String> =
            state.borrow().config.media_library.visible_columns.clone();
        let saved_widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();

        // Leading status-glyph column (⚠/🔒) — playlist-editor-only, mirrors
        // the unscanned-indicator column on the files side.
        {
            let factory = gtk4::SignalListItemFactory::new();
            factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                let lbl = Label::builder()
                    .halign(Align::Center)
                    .valign(Align::Center)
                    .build();
                li.set_child(Some(&lbl));
            });
            factory.connect_bind(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let t = &entry.track;
                let path = std::path::Path::new(&t.path);
                // Missing == the file is gone, mirroring the macOS/FFI
                // `file_missing` flag. `id == 0` only means "not catalogued";
                // an uncatalogued file that exists is a normal playable track.
                let missing  = !path.exists();
                let readonly = !missing && crate::media_library::is_read_only(path);
                let glyph = if missing { "⚠" } else if readonly { "🔒" } else { "" };
                if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                    lbl.set_label(glyph);
                }
            });
            let col = ColumnViewColumn::new(Some(""), Some(factory));
            col.set_fixed_width(24);
            track_list.append_column(&col);
        }

        // Position column (editor-only) — shows the 1-based playlist slot
        // resolved against the canonical play order in `editing_tracks`.
        // Pinned: fixed width, no resize/reorder.  Sorter is installed
        // below so clicking the header toggles position ASC/DESC.
        {
            let pos_factory = gtk4::SignalListItemFactory::new();
            pos_factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                let lbl = Label::builder()
                    .halign(Align::End)
                    .xalign(1.0)
                    .margin_start(6).margin_end(6)
                    .css_classes(["pl-duration"])
                    .build();
                li.set_child(Some(&lbl));
            });
            pos_factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let text = (entry.canonical_idx + 1).to_string();
                if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                    lbl.set_label(&text);
                }
            });
            let pos_col = ColumnViewColumn::new(Some("#"), Some(pos_factory));
            pos_col.set_fixed_width(48);
            pos_col.set_resizable(false);
            // Canonical-order sorter: compare each entry's slot directly.
            let sorter = CustomSorter::new(move |a, b| {
                let pa = a.downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .unwrap_or(usize::MAX);
                let pb = b.downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .unwrap_or(usize::MAX);
                pa.cmp(&pb).into()
            });
            pos_col.set_sorter(Some(&sorter));
            track_list.append_column(&pos_col);
            *pos_col_holder.borrow_mut() = Some(pos_col);
        }

        for c in ALL_COLUMNS.iter() {
            let id_str = c.id.to_string();
            let factory = gtk4::SignalListItemFactory::new();

            let setup_sel        = edit_multi_sel.clone();
            let setup_state      = state.clone();
            let setup_ctx_id     = ctx_canonical_idx.clone();
            let setup_et         = editing_tracks.clone();
            let setup_ep_id      = editing_pl_id.clone();
            let setup_drag_sel   = drag_selection.clone();
            let setup_ra         = reorder_allowed.clone();
            // rebuild_track_list isn't yet defined at this point of the
            // outer scope, so capture the Rc via a deferred holder filled
            // immediately after the rebuild closure is created.
            let setup_rebuild    = rebuild_track_list_holder.clone();
            let setup_scroll     = track_scroll_holder.clone();
            let setup_ed_ctx_idx    = ed_ctx_indices.clone();
            let setup_drives     = current_drives.clone();
            let setup_devices    = current_devices.clone();
            let setup_id         = id_str.clone();
            let is_artwork_col   = id_str == "artwork_path";
            // F12.2: separate clone for connect_bind — setup_state above is
            // moved into connect_setup.
            let bind_state       = state.clone();
            factory.connect_setup(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                // Artwork column gets a "View" Button instead of a Label —
                // matches the files view affordance.  Drag-source / drop-
                // target / right-click gesture attach to the Button just
                // like they would to a Label (both are Widget).
                let child: gtk4::Widget = if setup_id == "artwork_path" {
                    let btn = Button::with_label("View");
                    btn.add_css_class("link");
                    btn.set_margin_start(4);
                    btn.set_margin_end(4);
                    btn.set_halign(Align::Start);
                    btn.set_visible(false);
                    btn.upcast::<gtk4::Widget>()
                } else {
                    let lbl = Label::builder()
                        .margin_start(6).margin_end(6)
                        .margin_top(3).margin_bottom(3)
                        .hexpand(true).vexpand(true)
                        .halign(Align::Fill).valign(Align::Fill)
                        .xalign(0.0)
                        .ellipsize(gtk4::pango::EllipsizeMode::End)
                        .build();
                    lbl.upcast::<gtk4::Widget>()
                };
                let lbl = child.clone();
                let _ = is_artwork_col;

                // Per-cell DropTarget — handles intra-editor reorder.  When
                // the source drag originated in the editor (drag_selection
                // populated) and the current sort allows reorder, splice
                // those canonical rows to this cell's canonical slot.
                // Drops from other windows (drag_selection empty) fall
                // through to the outer track_scroll DropTarget which
                // appends the external paths.
                {
                    let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
                    let dt_li      = li.clone();
                    let dt_et      = setup_et.clone();
                    let dt_state   = setup_state.clone();
                    let dt_ep_id   = setup_ep_id.clone();
                    let dt_ra      = setup_ra.clone();
                    let dt_dragsel = setup_drag_sel.clone();
                    let dt_rebuild = setup_rebuild.clone();
                    dt.connect_drop(move |_, value, _, _| {
                        if !dt_ra.get() { return false }
                        // Reject the drop unless the drag originated in
                        // the editor itself — otherwise let the outer
                        // track_scroll DropTarget handle external add.
                        let src_indices: Vec<usize> = dt_dragsel.borrow().clone();
                        if src_indices.is_empty() { return false }
                        // Validate we still received the expected number
                        // of paths (sanity check; not used for indices).
                        if value.get::<gdk::FileList>().is_err() { return false }

                        // Resolve drop slot directly from this cell's
                        // EditorEntry so duplicate paths in the playlist
                        // collapse to the correct row, not the first one.
                        let Some(dst_canon) = dt_li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        else { return false };

                        // Splice in canonical order: remove src indices
                        // highest-first, then re-insert in original order
                        // at the adjusted destination.
                        let mut sorted = src_indices.clone();
                        sorted.sort_unstable_by(|a, b| b.cmp(a));
                        let mut adjusted_dst = dst_canon;
                        let mut removed: Vec<crate::media_library::LibTrack> = Vec::new();
                        {
                            let mut et = dt_et.borrow_mut();
                            for src in sorted.iter() {
                                if *src < et.len() {
                                    let t = et.remove(*src);
                                    if *src < adjusted_dst { adjusted_dst -= 1; }
                                    removed.push(t);
                                }
                            }
                            removed.reverse();
                            let cap = et.len();
                            let insert_at = adjusted_dst.min(cap);
                            for (i, t) in removed.into_iter().enumerate() {
                                et.insert(insert_at + i, t);
                            }
                        }

                        // Persist canonical order through the library so
                        // the on-disk M3U8 reflects the reorder immediately.
                        // Rewrites the existing playlist file in place;
                        // `add_playlist_file` upserts the row so registering
                        // the same path again is a no-op.
                        let pid = dt_ep_id.get();
                        if pid >= 0 {
                            let s = dt_state.borrow();
                            if let Some(lib) = s.media_lib.as_ref() {
                                let paths: Vec<String> = dt_et.borrow()
                                    .iter().map(|t| t.path.clone()).collect();
                                if let Ok(pl) = lib.playlist_by_id(pid) {
                                    if let Err(e) = lib.save_playlist_tracks_to_path(
                                        std::path::Path::new(&pl.path),
                                        &paths,
                                    ) {
                                        eprintln!("editor reorder persist {pid}: {e}");
                                    }
                                }
                            }
                        }

                        // Drag completed — clear selection so a stray
                        // subsequent drop (e.g. external) doesn't reorder.
                        dt_dragsel.borrow_mut().clear();

                        // Defer rebuild to next idle tick so we don't
                        // splice the backing ListStore while GTK is still
                        // unwinding the drop event chain — splicing mid-
                        // drop segfaults on some GTK4 versions.
                        if let Some(rb) = dt_rebuild.borrow().as_ref().cloned() {
                            glib::idle_add_local_once(move || rb());
                        }
                        true
                    });
                    lbl.add_controller(dt);
                }

                // Per-cell DragSource — ships every currently-selected editor
                // row as a FileList so the user can drag tracks out of the
                // playlist editor into the active playlist (pl_scroll accepts
                // FileList).  Single-row drag works too: if the row under
                // the pointer is not in the selection it still ships its
                // own path.
                {
                    let ds = gtk4::DragSource::new();
                    ds.set_actions(gtk4::gdk::DragAction::COPY);
                    let ds_sel       = setup_sel.clone();
                    let ds_li        = li.clone();
                    let ds_dragsel   = setup_drag_sel.clone();
                    ds.connect_prepare(move |_, _, _| {
                        // Clear any stale canonical indices from a prior
                        // drag, then record this drag's selection by
                        // canonical_idx so duplicates of the same path
                        // resolve to the correct rows on reorder.
                        ds_dragsel.borrow_mut().clear();
                        let mut paths: Vec<std::path::PathBuf> = Vec::new();
                        let mut indices: Vec<usize> = Vec::new();
                        let mut self_entry: Option<(std::path::PathBuf, usize)> = None;
                        if let Some(obj) = ds_li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let entry = obj.borrow::<EditorEntry>();
                            self_entry = Some((
                                std::path::PathBuf::from(&entry.track.path),
                                entry.canonical_idx,
                            ));
                        }
                        for i in 0..ds_sel.n_items() {
                            if ds_sel.is_selected(i) {
                                if let Some(obj) = ds_sel.item(i)
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                {
                                    let entry = obj.borrow::<EditorEntry>();
                                    paths.push(std::path::PathBuf::from(&entry.track.path));
                                    indices.push(entry.canonical_idx);
                                }
                            }
                        }
                        if paths.is_empty() {
                            if let Some((p, i)) = self_entry {
                                paths.push(p);
                                indices.push(i);
                            }
                        }
                        if paths.is_empty() { return None }
                        *ds_dragsel.borrow_mut() = indices;
                        let files: Vec<gio::File> = paths.iter()
                            .map(|p| gio::File::for_path(p))
                            .collect();
                        let fl = gdk::FileList::from_array(&files);
                        Some(gdk::ContentProvider::for_value(&fl.to_value()))
                    });
                    lbl.add_controller(ds);
                }

                // Per-cell right-click gesture. Builds a real gio::Menu +
                // PopoverMenu (NESTED), same as the Files view and the
                // device-tracks view — see the big comment above
                // `ed_ctx_indices`/`ed_selected_tracks` for why this now works
                // where Task 8's flat-button popover was a workaround: the
                // "ed" action group lives on `track_scroll` (installed once,
                // not per-cell) and the popover is parented on that same
                // widget, so action lookup never has to walk the ColumnView
                // cell tree at all.
                let gesture = gtk4::GestureClick::new();
                gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
                let g_sel        = setup_sel.clone();
                let g_state      = setup_state.clone();
                let g_ctx_id     = setup_ctx_id.clone();
                let g_li         = li.clone();
                let g_lbl        = lbl.clone();
                let g_scroll     = setup_scroll.clone();
                let g_ed_ctx_idx = setup_ed_ctx_idx.clone();
                let g_drives     = setup_drives.clone();
                let g_devices    = setup_devices.clone();
                gesture.connect_pressed(move |g, _n, x, y| {
                    let Some(scroll_widget) = g_scroll.borrow().clone() else {
                        return;
                    };
                    let Some(item) = g_li.item() else {
                        return;
                    };
                    let item_clone = item.clone();
                    let mut clicked_idx: Option<u32> = None;
                    for i in 0..g_sel.n_items() {
                        if g_sel.item(i).as_ref() == Some(&item_clone) {
                            clicked_idx = Some(i);
                            break;
                        }
                    }
                    if let Some(idx) = clicked_idx {
                        if !g_sel.is_selected(idx) {
                            g_sel.unselect_all();
                            g_sel.select_item(idx, true);
                        }
                    }
                    // Stash this row's canonical play-order slot so the
                    // single-row actions (edit-id3) operate on the exact
                    // row that was clicked even when the playlist lists
                    // duplicates of the same path.
                    let (cidx, is_lib_track) = item.downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            let e = o.borrow::<EditorEntry>();
                            (e.canonical_idx as i64, e.track.id > 0)
                        })
                        .unwrap_or((-1, false));
                    g_ctx_id.set(cidx);

                    let sel_count: usize = (0..g_sel.n_items())
                        .filter(|i| g_sel.is_selected(*i)).count();

                    // Gather canonical indices the row-scoped actions
                    // (Replace / Edit ID3 / Remove) operate on — selection
                    // first, falling back to the single clicked row when
                    // nothing is selected — and stash them once per
                    // right-click. send-active/add-to-new/add-to-saved/
                    // send-drive/send-device instead read the live
                    // selection straight off `edit_multi_sel` at dispatch
                    // (`ed_selected_tracks`), since they're also reachable
                    // from the "Send to ▾" button, which never fires this
                    // gesture.
                    let mut idxs: Vec<usize> = (0..g_sel.n_items())
                        .filter(|i| g_sel.is_selected(*i))
                        .filter_map(|i| g_sel.item(i))
                        .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        .collect();
                    if idxs.is_empty() {
                        let c = g_ctx_id.get();
                        if c >= 0 { idxs.push(c as usize); }
                    }
                    *g_ed_ctx_idx.borrow_mut() = idxs.clone();

                    // ── Build the real menu model ---------------------
                    // Order: Send to · Replace · ─ · ID3 · Album Art · Lyrics ·
                    // ─ · Remove from Playlist. Matches the macOS editor menu
                    // (MLPlaylistEditor.editorContextMenu).
                    let send = build_send_to_menu(
                        &g_state,
                        &SendToActions {
                            active: "ed.send-active",
                            new_playlist: "ed.add-to-new",
                            saved_playlist: "ed.add-to-saved",
                            drive: "ed.send-drive",
                            device: "ed.send-device",
                            drives: g_drives.borrow().iter()
                                .map(|d| (d.id.clone(), d.label.clone())).collect(),
                            devices: g_devices.borrow().iter()
                                .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        },
                    );
                    let menu = gio::Menu::new();
                    menu.append_submenu(Some("↪ Send to"), &send);
                    menu.append_item(&gio::MenuItem::new(
                        Some("♻ Replace Current Playlist"),
                        Some("ed.replace"),
                    ));
                    // ID3 / Album Art / Lyrics — single library row only.
                    if is_lib_track && sel_count <= 1 {
                        menu.append_item(&gio::MenuItem::new(
                            Some("🎵 View/Edit ID3"),
                            Some("ed.edit-id3"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("🖼 View Album Art"),
                            Some("ed.view-art"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("📝 View/Search Lyrics"),
                            Some("ed.lyrics"),
                        ));
                    }
                    menu.append_item(&gio::MenuItem::new(
                        Some("✕ Remove from Playlist"),
                        Some("ed.remove"),
                    ));

                    let popover = context_popover(&menu);
                    // EXACT mirror of the working Files-view context menu
                    // (~line 3630): parent the popover on the same widget the
                    // "ed" action group is installed on (track_scroll), and
                    // do NOT unparent on close. An earlier `connect_closed(||
                    // unparent)` severed the widget-tree link to the group as
                    // a nested item dispatched, so ed.send-drive never fired
                    // (2026-07-16). The Files menu leaves its popover parented
                    // too; matching that is what makes nested dispatch work.
                    let (px, py) = g_lbl
                        .translate_coordinates(&scroll_widget, x, y)
                        .unwrap_or((x, y));
                    let rect = gtk4::gdk::Rectangle::new(px as i32, py as i32, 1, 1);
                    popover.set_parent(&scroll_widget);
                    popover.set_pointing_to(Some(&rect));
                    popover.popup();
                    g.set_state(gtk4::EventSequenceState::Claimed);
                });
                lbl.add_controller(gesture);

                li.set_child(Some(&lbl));
            });

            let bind_id = id_str.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let t = &entry.track;
                // F12.2: read live so a Settings toggle applies to
                // already-bound cells on the next rebind, not just at
                // window construction (the ML window is a singleton — see
                // rebuild_ml_callback in player.rs).
                let artist_as_album_artist =
                    bind_state.borrow().config.media_library.artist_as_album_artist;
                // Stash this cell's canonical play-order index on whatever
                // child widget the cell currently holds so the editor-area
                // drop target can resolve a drop coordinate to a canonical
                // insert position via track_list.pick(x, y) → walk_up →
                // parse "pos:<N>".  Works for both Label and Button cells.
                if let Some(c) = li.child() {
                    c.set_widget_name(&format!("pos:{}", entry.canonical_idx));
                }
                // Artwork column gets the Button affordance, mirroring the
                // files view.  Click opens the cached cover-art image.
                if bind_id == "artwork_path" {
                    let Some(btn) = li.child().and_then(|c| c.downcast::<Button>().ok())
                    else { return };
                    if let Some(art_path) = t.artwork_path.clone() {
                        btn.set_visible(true);
                        btn.set_sensitive(true);
                        btn.set_label("View");
                        // Replace any prior click handler so the captured
                        // path always matches the row currently bound to
                        // this recycled cell.
                        let handler = btn.connect_clicked(move |_| {
                            open_image_viewer(&art_path);
                        });
                        // Disconnect previous handler if present to avoid
                        // accumulating across binds on the same widget.
                        unsafe {
                            if let Some(old) = btn.steal_data::<glib::SignalHandlerId>("art-handler") {
                                btn.disconnect(old);
                            }
                            btn.set_data("art-handler", handler);
                        }
                    } else {
                        btn.set_visible(false);
                    }
                    return;
                }
                let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok())
                else { return };
                let text = match bind_id.as_str() {
                    "num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
                    "title" => t.title.as_deref().unwrap_or(&t.filename).to_string(),
                    "artist" => t.artist.as_deref().unwrap_or("").to_string(),
                    "album" => t.album.as_deref().unwrap_or("").to_string(),
                    // F12.2: falls back to artist when the album-artist tag
                    // is blank and the toggle is on. A4 (phase 11 album
                    // gallery) MUST also use this helper.
                    "album_artist" => crate::play_stats::effective_album_artist(
                        t.artist.as_deref().unwrap_or(""),
                        t.album_artist.as_deref().unwrap_or(""),
                        artist_as_album_artist,
                    ),
                    "duration" => t.length_secs
                        .map(|s| { let ss = s as u64; format!("{}:{:02}", ss/60, ss%60) })
                        .unwrap_or_else(|| "-:--".to_string()),
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
                        if d == 0 { String::new() }
                        else if let Some(total) = t.disc_total {
                            if total > 0 { format!("{}/{}", d, total) } else { d.to_string() }
                        } else { d.to_string() }
                    }
                    "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
                    "composer" => t.composer.as_deref().unwrap_or("").to_string(),
                    "original_artist" => t.original_artist.as_deref().unwrap_or("").to_string(),
                    "copyright" => t.copyright.as_deref().unwrap_or("").to_string(),
                    "url" => t.url.as_deref().unwrap_or("").to_string(),
                    "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_string(),
                    "bpm" => t.bpm.as_deref().unwrap_or("").to_string(),
                    "lyric" => {
                        let ly = t.lyric.as_deref().unwrap_or("");
                        if ly.is_empty() { String::new() }
                        else if ly.len() > 30 { format!("{}…", &ly[..30]) }
                        else { ly.to_string() }
                    }
                    "comment" => t.comment.as_deref().unwrap_or("").to_string(),
                    "artwork_path" => if t.artwork_path.is_some() { "Yes".to_string() } else { String::new() },
                    _ => String::new(),
                };
                lbl.set_text(&gtk_safe(&text));
                // Unavailable file → broken color, mirroring the macOS
                // editor's red rows for missing files. Existence — not library
                // membership — decides this, so an uncatalogued but present
                // file shows normally.
                let missing = !std::path::Path::new(&t.path).exists();
                if missing {
                    lbl.add_css_class("broken");
                } else {
                    lbl.remove_css_class("broken");
                }
            });

            let col = ColumnViewColumn::new(Some(c.header), Some(factory));
            col.set_resizable(true);
            if c.expand { col.set_expand(true); }
            col.set_visible(visible_ids.contains(&id_str));
            if let Some(&w) = saved_widths.get(&id_str) {
                if w > 0 { col.set_fixed_width(w); }
            }

            // Display-only sorter — sort is applied via SortListModel so
            // `editing_tracks` (canonical play order) is never mutated.
            let sort_id = id_str.clone();
            let sorter = CustomSorter::new(move |a, b| {
                let a_val = a
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<EditorEntry>().track, &sort_id))
                    .unwrap_or_default();
                let b_val = b
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<EditorEntry>().track, &sort_id))
                    .unwrap_or_default();
                a_val.cmp(&b_val).into()
            });
            col.set_sorter(Some(&sorter));
            track_list.append_column(&col);
            editor_named_cols.push((id_str, col));
        }

        // Apply the files-view saved column order so the editor matches
        // it — the user only arranges columns once.  Columns not present
        // in saved_order keep their default position at the tail.
        let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
        if !saved_order.is_empty() {
            for (_, col) in editor_named_cols.iter() {
                track_list.remove_column(col);
            }
            // Position 0 = status glyph, 1 = position; named columns start at 2.
            let mut pos = 2u32;
            for col_id in &saved_order {
                if let Some((_, col)) = editor_named_cols.iter()
                    .find(|(id, _)| id == col_id)
                {
                    track_list.insert_column(pos, col);
                    pos += 1;
                }
            }
            for (id, col) in editor_named_cols.iter() {
                if !saved_order.contains(id) {
                    track_list.insert_column(pos, col);
                    pos += 1;
                }
            }
        }
    }
    // Allow drag-reorder of editor column headers — same affordance as
    // the files view.  Pinned columns (status + position) remain in
    // place because they aren't reorderable individually; GTK keeps them
    // in their declared positions.
    track_list.set_reorderable(true);

    // Shared closure that re-applies the files-view column state
    // (visibility, widths, order) to the editor's ColumnView.  Called
    // every time a saved playlist is loaded so the editor mirrors the
    // user's latest customization without needing a full ML reopen.
    let editor_cols_rc: Rc<Vec<(String, ColumnViewColumn)>> =
        Rc::new(editor_named_cols);
    let apply_editor_columns: Rc<dyn Fn()> = {
        let cols = editor_cols_rc.clone();
        let state_rc = state.clone();
        let tl = track_list.clone();
        // 2 pinned leading columns: status glyph + position.
        Rc::new(move || apply_ml_columns_to(&tl, cols.as_slice(), &state_rc, 2))
    };

    // Connect the sort model to the ColumnView's column-driven sorter so
    // header clicks produce a display sort.  Then listen for sorter changes
    // and update `reorder_allowed` — true when the active sort is "position
    // ASC" or no sort, false for any other column / order.
    {
        let sorter = track_list.sorter();
        edit_sort_model.set_sorter(sorter.as_ref());
        if let Some(s) = sorter {
            let pos_holder = pos_col_holder.clone();
            let ra = reorder_allowed.clone();
            let update = move |s: &gtk4::Sorter| {
                let pos_col = pos_holder.borrow().clone();
                let allowed = if let Some(cv_sorter) =
                    s.downcast_ref::<gtk4::ColumnViewSorter>()
                {
                    let primary = cv_sorter.primary_sort_column();
                    let order   = cv_sorter.primary_sort_order();
                    match (primary, pos_col) {
                        (None, _) => true, // default sort = canonical
                        (Some(pc), Some(target)) =>
                            pc == target && order == gtk4::SortType::Ascending,
                        _ => false,
                    }
                } else {
                    true
                };
                ra.set(allowed);
            };
            update(&s);
            s.connect_changed(move |s, _| update(s));
        }
    }

    // Rebuild track editor: splice the entire `editing_tracks` Vec into the
    // backing ListStore as `EditorEntry` items so each row carries its
    // canonical slot.  ColumnView recycles visible rows so this stays
    // cheap for big playlists.  Also rebuilds `position_map` for first-
    // occurrence path lookups by the cross-window drop target.
    let rebuild_track_list: Rc<dyn Fn()> = {
        let store    = edit_store.clone();
        let et       = editing_tracks.clone();
        let pos_map  = position_map.clone();
        Rc::new(move || {
            let mut map = pos_map.borrow_mut();
            map.clear();
            let items: Vec<glib::BoxedAnyObject> = et
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    map.entry(t.path.clone()).or_insert(i);
                    glib::BoxedAnyObject::new(EditorEntry {
                        track: t.clone(),
                        canonical_idx: i,
                    })
                })
                .collect();
            drop(map);
            store.splice(0, store.n_items(), &items);
        })
    };
    // Populate the holder so the column factories' per-cell drop targets
    // can refresh the editor after a successful reorder.
    *rebuild_track_list_holder.borrow_mut() = Some(rebuild_track_list.clone());

    // Error banner shown when a playlist's file can't be read (e.g. the
    // library was scanned in another sandbox and the stored path doesn't
    // resolve here).  Hidden while the playlist loads normally.  Hoisted
    // here so load_pl_by_id below can capture it; packed into the
    // pl-edit page further down.
    let edit_error_label: Label = Label::builder()
        .label("")
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .margin_start(8).margin_end(8)
        .margin_top(4).margin_bottom(4)
        .visible(false)
        .build();
    edit_error_label.add_css_class("broken");

    // ── Helper: load a playlist by DB id into editing state ───────────────
    let load_pl_by_id: Rc<dyn Fn(i64)> = {
        let state_rc   = state.clone();
        let et         = editing_tracks.clone();
        let saved      = saved_track_ids.clone();
        let rebuild    = rebuild_track_list.clone();
        let ep_id      = editing_pl_id.clone();
        let apply_cols = apply_editor_columns.clone();
        let err_lbl    = edit_error_label.clone();
        let search     = pl_search_entry.clone();
        Rc::new(move |id: i64| {
            ep_id.set(id);
            // A previous playlist's search query must not filter this one —
            // but F12.1: if remember_search is on, restore the "playlists"
            // view's saved query instead of clearing.
            if state_rc.borrow().config.media_library.remember_search {
                let last = state_rc
                    .borrow()
                    .config
                    .media_library
                    .last_search
                    .get("playlists")
                    .cloned();
                search.set_text(last.as_deref().unwrap_or(""));
            } else {
                search.set_text("");
            }
            // Re-apply files-view column state so customizations made
            // while the editor was elsewhere take effect immediately.
            apply_cols();
            let loaded = state_rc
                .borrow()
                .media_lib
                .as_ref()
                .map(|lib| {
                    lib.playlist_by_id(id)
                        .and_then(|pl| lib.load_playlist_tracks(&pl))
                });
            let tracks = match loaded {
                Some(Ok(tracks)) => {
                    err_lbl.set_visible(false);
                    tracks
                }
                Some(Err(e)) => {
                    // Playlist entries live only in the .m3u8 file, so an
                    // unreadable file means there is nothing to show — say
                    // why instead of presenting a silently empty playlist.
                    err_lbl.set_text(&gtk_safe(&format!(
                        "This playlist has not been scanned yet and its \
                         file is not accessible from here ({e:#})."
                    )));
                    err_lbl.set_visible(true);
                    Vec::new()
                }
                None => {
                    err_lbl.set_visible(false);
                    Vec::new()
                }
            };
            let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
            *et.borrow_mut() = tracks;
            *saved.borrow_mut() = ids;
            rebuild();
        })
    };

    // Register the editor-refresh hook so any cross-window add-to-saved
    // action that targets the currently-open playlist reloads the editor.
    {
        let load = load_pl_by_id.clone();
        let ep_id = editing_pl_id.clone();
        let hook: Rc<dyn Fn(i64)> = Rc::new(move |target_pid: i64| {
            if ep_id.get() == target_pid {
                load(target_pid);
            }
        });
        EDITOR_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }
    // Refresh-current hook: reloads whatever playlist is open in the
    // editor.  Fired after a track is recorded as played so the editor
    // mirrors the files view's updated metadata + unread state.
    {
        let load = load_pl_by_id.clone();
        let ep_id = editing_pl_id.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let id = ep_id.get();
            if id >= 0 { load(id); }
        });
        EDITOR_CURRENT_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }
    // Nav-refresh hook: re-sync the playlist sidebar sub-rows and the
    // manage list with the playlists table after a playlist is created
    // from another window (e.g. active-playlist "Add to new playlist").
    {
        let state_rc     = state.clone();
        let sidebar_ref  = sidebar.clone();
        let sub_rows_ref = pl_sub_rows.clone();
        let expanded_ref = playlists_expanded.clone();
        let manage_ref   = pl_manage_list.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let playlists = state_rc
                .borrow()
                .media_lib
                .as_ref()
                .and_then(|lib| lib.all_playlists().ok())
                .unwrap_or_default();

            // Remember the selected sidebar playlist (if any) so the
            // rebuild doesn't visually drop the user's place.
            let selected = sidebar_ref
                .selected_row()
                .map(|r| r.widget_name().to_string());

            // Clear both lists, then rebuild from the playlists table.
            // Sidebar sub-rows are tracked in `pl_sub_rows`, so drain that;
            // the manage list isn't tracked, so empty it by index.
            for row in sub_rows_ref.borrow_mut().drain(..) {
                sidebar_ref.remove(&row);
            }
            while let Some(row) = manage_ref.row_at_index(0) {
                manage_ref.remove(&row);
            }

            // Insert the rebuilt rows right after the Playlists header — not at
            // the sidebar end, which is below the Devices section.
            let mut insert_at = {
                let mut idx = 0i32;
                let mut after = 1i32;
                while let Some(r) = sidebar_ref.row_at_index(idx) {
                    if r.widget_name() == "playlists" {
                        after = idx + 1;
                        break;
                    }
                    idx += 1;
                }
                after
            };

            for pl in &playlists {
                let s_lbl = Label::builder()
                    .label(&pl.name)
                    .halign(Align::Start)
                    .xalign(0.0)
                    .margin_start(sidebar::SUB_ROW_INSET).margin_end(8)
                    .margin_top(4).margin_bottom(4)
                    .build();
                let s_row = ListBoxRow::new();
                s_row.set_widget_name(&format!("pl:{}", pl.id));
                s_row.set_child(Some(&s_lbl));
                s_row.set_visible(expanded_ref.get());
                attach_pl_row_drag(&s_row, pl.id);
                sidebar_ref.insert(&s_row, insert_at);
                insert_at += 1;
                if selected.as_deref() == Some(s_row.widget_name().as_str()) {
                    sidebar_ref.select_row(Some(&s_row));
                }
                sub_rows_ref.borrow_mut().push(s_row);

                let m_lbl = Label::builder()
                    .label(&pl.name)
                    .halign(Align::Start)
                    .margin_start(8).margin_end(8)
                    .margin_top(3).margin_bottom(3)
                    .build();
                let m_row = ListBoxRow::new();
                m_row.set_widget_name(&pl.id.to_string());
                m_row.set_child(Some(&m_lbl));
                attach_pl_row_drag(&m_row, pl.id);
                manage_ref.append(&m_row);
            }
        });
        PLAYLIST_NAV_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }

    // ── Helper: add a sub-row to both the sidebar and pl_manage_list ──────
    // Returns the sidebar row so the caller can select it.
    let _add_pl_sidebar_row = {
        let sidebar_ref  = sidebar.clone();
        let sub_rows_ref = pl_sub_rows.clone();
        let expanded_ref = playlists_expanded.clone();
        Rc::new(move |id: i64, name: &str| -> ListBoxRow {
            // Sidebar sub-row
            let s_lbl = Label::builder()
                .label(name)
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(sidebar::SUB_ROW_INSET).margin_end(8)
                .margin_top(4).margin_bottom(4)
                .build();
            let s_row = ListBoxRow::new();
            s_row.set_widget_name(&format!("pl:{}", id));
            s_row.set_child(Some(&s_lbl));
            s_row.set_visible(expanded_ref.get());
            attach_pl_row_drag(&s_row, id);
            sidebar_ref.append(&s_row);
            sub_rows_ref.borrow_mut().push(s_row.clone());
            s_row
        })
    };

    // ── Build "pl-manage" page ────────────────────────────────────────────
    {
        let manage_vbox = GtkBox::new(Orientation::Vertical, 0);

        // Populate the manage list from DB
        let playlists_initial = state
            .borrow()
            .media_lib
            .as_ref()
            .and_then(|lib| lib.all_playlists().ok())
            .unwrap_or_default();
        for pl in &playlists_initial {
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .margin_start(8).margin_end(8)
                .margin_top(3).margin_bottom(3)
                .build();
            let row = ListBoxRow::new();
            row.set_widget_name(&pl.id.to_string());
            row.set_child(Some(&lbl));
            attach_pl_row_drag(&row, pl.id);
            pl_manage_list.append(&row);
        }

        let manage_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .child(&*pl_manage_list)
            .build();
        manage_vbox.append(&manage_scroll);

        // Clicking a manage-list row → select its sidebar sub-row
        {
            let sidebar_ref   = sidebar.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            pl_manage_list.connect_row_selected(move |_, opt_row| {
                let row = match opt_row { Some(r) => r, None => return };
                let id_str = row.widget_name().to_string();
                // Find matching sidebar "pl:ID" row and select it
                let target = format!("pl:{}", id_str);
                let mut i = 0i32;
                loop {
                    match sidebar_ref.row_at_index(i) {
                        Some(sr) if sr.widget_name() == target => {
                            sidebar_ref.select_row(Some(&sr));
                            break;
                        }
                        Some(_) => { i += 1; }
                        None => break,
                    }
                }
                // Also switch sub-stack directly (sidebar handler may not fire
                // if the row is already selected)
                pl_sub_ref.set_visible_child_name("pl-edit");
            });
        }

        // Manage list bottom buttons: New / Rename / Delete
        let manage_btn_row = GtkBox::new(Orientation::Horizontal, 4);
        manage_btn_row.set_margin_start(4);
        manage_btn_row.set_margin_end(4);
        manage_btn_row.set_margin_top(4);
        manage_btn_row.set_margin_bottom(4);

        let btn_new_pl    = Button::with_label("+ New");
        btn_new_pl.add_css_class("pl-btn");
        let btn_rename_pl = Button::with_label("Rename");
        btn_rename_pl.add_css_class("pl-btn");
        btn_rename_pl.set_sensitive(false);
        let btn_delete_pl = Button::with_label("Delete");
        btn_delete_pl.add_css_class("pl-btn");
        btn_delete_pl.set_sensitive(false);

        manage_btn_row.append(&btn_new_pl);
        manage_btn_row.append(&btn_rename_pl);
        manage_btn_row.append(&btn_delete_pl);
        manage_vbox.append(&manage_btn_row);

        // Enable/disable rename+delete based on manage list selection
        {
            let btn_ren = btn_rename_pl.clone();
            let btn_del = btn_delete_pl.clone();
            pl_manage_list.connect_row_selected(move |_, opt| {
                let has = opt.is_some();
                btn_ren.set_sensitive(has);
                btn_del.set_sensitive(has);
            });
        }

        // ── New playlist ──────────────────────────────────────────────────
        {
            let state_rc      = state.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let sub_rows_ref  = pl_sub_rows.clone();
            let expanded_ref  = playlists_expanded.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            let load          = load_pl_by_id.clone();
            let win_wk        = win.downgrade();
            btn_new_pl.connect_clicked(move |_| {
                let Some(win) = win_wk.upgrade() else { return };
                let state2  = state_rc.clone();
                let pl_ref2 = pl_list_ref.clone();
                let sid2    = sidebar_ref.clone();
                let sub2    = sub_rows_ref.clone();
                let exp2    = expanded_ref.clone();
                let pls2    = pl_sub_ref.clone();
                let load2   = load.clone();
                // Save dialog replaces the previous name-only popup —
                // user picks BOTH the filename and the target folder so
                // the new playlist no longer lands silently in Sparkamp's
                // managed `~/.config/sparkamp/playlists/` directory (which
                // had the side effect of registering itself as a watched
                // folder via `add_playlist_file`).
                run_playlist_save_dialog(state_rc.clone(), win, "New Playlist", move |path, win_cb| {
                    let name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let save_result = state2.borrow().media_lib.as_ref()
                        .map(|lib| lib.save_playlist_tracks_to_path(&path, &[]));
                    let new_id = match save_result {
                        Some(Ok(id)) => id,
                        Some(Err(e)) => {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                            return;
                        }
                        None => return,
                    };

                    // Add to manage list
                    let row_lbl = Label::builder().label(&name)
                        .halign(Align::Start)
                        .margin_start(8).margin_end(8)
                        .margin_top(3).margin_bottom(3).build();
                    let manage_row = ListBoxRow::new();
                    manage_row.set_widget_name(&new_id.to_string());
                    manage_row.set_child(Some(&row_lbl));
                    attach_pl_row_drag(&manage_row, new_id);
                    pl_ref2.append(&manage_row);
                    pl_ref2.select_row(Some(&manage_row));

                    // Add sidebar sub-row and select it
                    let s_lbl = Label::builder().label(&name)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(sidebar::SUB_ROW_INSET).margin_end(8)
                        .margin_top(4).margin_bottom(4).build();
                    let s_row = ListBoxRow::new();
                    s_row.set_widget_name(&format!("pl:{}", new_id));
                    s_row.set_child(Some(&s_lbl));
                    s_row.set_visible(exp2.get());
                    attach_pl_row_drag(&s_row, new_id);
                    sid2.insert(&s_row, sidebar_pl_end_index(&sid2));
                    sub2.borrow_mut().push(s_row.clone());
                    sid2.select_row(Some(&s_row));

                    load2(new_id);
                    pls2.set_visible_child_name("pl-edit");
                });
            });
        }

        // ── Rename playlist ───────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let pl_list_ref = pl_manage_list.clone();
            let sidebar_ref = sidebar.clone();
            let win_wk      = win.downgrade();
            btn_rename_pl.connect_clicked(move |_| {
                let sel_row = match pl_list_ref.selected_row() { Some(r) => r, None => return };
                let id = match sel_row.widget_name().to_string().parse::<i64>() {
                    Ok(v) => v, Err(_) => return,
                };
                let current = sel_row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.text().to_string()).unwrap_or_default();

                let dialog = gtk4::Window::builder()
                    .title("Rename Playlist").modal(true).resizable(false).default_width(300)
                    .build();
                if let Some(w) = win_wk.upgrade() { dialog.set_transient_for(Some(&w)); }
                let vbox = GtkBox::new(Orientation::Vertical, 8);
                vbox.set_margin_top(12); vbox.set_margin_bottom(12);
                vbox.set_margin_start(12); vbox.set_margin_end(12);
                let lbl = Label::builder().label("New name:").halign(Align::Start).build();
                let name_entry = Entry::new();
                name_entry.set_text(&gtk_safe(&current));
                name_entry.set_hexpand(true);
                let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
                dialog_btns.set_halign(Align::End);
                let cancel_btn = Button::with_label("Cancel");
                let ok_btn     = Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                dialog_btns.append(&cancel_btn); dialog_btns.append(&ok_btn);
                vbox.append(&lbl); vbox.append(&name_entry); vbox.append(&dialog_btns);
                dialog.set_child(Some(&vbox));
                let d = dialog.clone();
                cancel_btn.connect_clicked(move |_| { d.close(); });
                let d        = dialog.clone();
                let e        = name_entry.clone();
                let state2   = state_rc.clone();
                let sel2     = sel_row.clone();
                let sid2     = sidebar_ref.clone();
                ok_btn.connect_clicked(move |_| {
                    let name = e.text().to_string();
                    if name.is_empty() { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.rename_playlist(id, &name);
                    }
                    // Update manage-list label
                    if let Some(c) = sel2.child() {
                        if let Ok(l) = c.downcast::<Label>() { l.set_text(&gtk_safe(&name)); }
                    }
                    // Update sidebar sub-row label
                    let target = format!("pl:{}", id);
                    let mut i = 0i32;
                    loop {
                        match sid2.row_at_index(i) {
                            Some(sr) if sr.widget_name() == target => {
                                if let Some(c) = sr.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(&name));
                                    }
                                }
                                break;
                            }
                            Some(_) => { i += 1; }
                            None => break,
                        }
                    }
                    d.close();
                });
                let ok2 = ok_btn.clone();
                name_entry.connect_activate(move |_| { ok2.activate(); });
                dialog.present();
            });
        }

        // ── Delete playlist ───────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let pl_list_ref = pl_manage_list.clone();
            let sidebar_ref = sidebar.clone();
            let sub_rows_ref = pl_sub_rows.clone();
            let pl_sub_ref  = pl_sub_stack.clone();
            let et          = editing_tracks.clone();
            let saved       = saved_track_ids.clone();
            let rebuild     = rebuild_track_list.clone();
            let win_wk      = win.downgrade();
            btn_delete_pl.connect_clicked(move |_| {
                let sel_row = match pl_list_ref.selected_row() { Some(r) => r, None => return };
                let id = match sel_row.widget_name().to_string().parse::<i64>() {
                    Ok(v) => v, Err(_) => return,
                };
                let pl_name = sel_row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.text().to_string()).unwrap_or_default();

                let dialog = gtk4::AlertDialog::builder()
                    .message(format!("Delete \"{}\"?", pl_name))
                    .detail("The playlist file on disk is not deleted.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0).default_button(1).modal(true).build();

                let state2    = state_rc.clone();
                let pl_ref2   = pl_list_ref.clone();
                let sid2      = sidebar_ref.clone();
                let sub2      = sub_rows_ref.clone();
                let pls2      = pl_sub_ref.clone();
                let sel2      = sel_row.clone();
                let et2       = et.clone();
                let saved2    = saved.clone();
                let rebuild2  = rebuild.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                    if result != Ok(1) { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.remove_playlist(id);
                    }
                    // Remove from manage list
                    pl_ref2.remove(&sel2);
                    // Remove sidebar sub-row
                    let target = format!("pl:{}", id);
                    let mut sub = sub2.borrow_mut();
                    sub.retain(|r| {
                        if r.widget_name() == target { sid2.remove(r); false } else { true }
                    });
                    // Go back to manage page
                    et2.borrow_mut().clear();
                    saved2.borrow_mut().clear();
                    rebuild2();
                    pls2.set_visible_child_name("pl-manage");
                });
            });
        }

        pl_sub_stack.add_named(&manage_vbox, Some("pl-manage"));
    }

    // Hoisted: title + rename button + path label (sidebar handler updates
    // the title text on selection change).
    let edit_header: Label = Label::builder()
        .label("Playlist Editor")
        .halign(Align::Start)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .margin_start(8).margin_top(4).margin_bottom(0)
        .build();
    edit_header.add_css_class("ml-section-header");

    let btn_rename_pl_inline: Button = {
        let b = Button::with_label("Rename");
        b.add_css_class("pl-btn");
        b.set_margin_end(8);
        b.set_margin_top(2);
        b
    };

    // File path bar — shows the .m3u path so the user can see if it is an
    // external playlist (not managed by Sparkamp).
    let edit_path_label: Label = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(8).margin_top(0).margin_bottom(4)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .selectable(true)
        .build();
    edit_path_label.add_css_class("status-label");

    // Save button (hoisted so the sidebar handler can toggle its sensitivity)
    let btn_save_pl_outer: Button = {
        let b = Button::with_label("Save");
        b.add_css_class("pl-btn");
        b
    };

    // ── Build "pl-edit" page ──────────────────────────────────────────────
    {
        let edit_vbox = GtkBox::new(Orientation::Vertical, 0);

        let header_row = GtkBox::new(Orientation::Horizontal, 4);
        header_row.append(&edit_header);
        header_row.append(&btn_rename_pl_inline);
        edit_vbox.append(&header_row);
        edit_vbox.append(&edit_path_label);
        edit_vbox.append(&edit_error_label);

        edit_vbox.append(&pl_search_row);

        let track_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Automatic)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&*track_list)
            .build();
        edit_vbox.append(&track_scroll);
        // Expose track_scroll so cell right-click popovers can parent
        // themselves to it (parented-to-leaf popovers don't render).
        *track_scroll_holder.borrow_mut() = Some(track_scroll.clone());
        // Install the "ed" action group ONCE on this stable ScrolledWindow —
        // mirrors dev_tracks_scroll.insert_action_group("dev", ...) in the
        // device-tracks view. The per-cell PopoverMenu is parented here too
        // (see the per-cell gesture), so action lookup never has to walk
        // more than zero ancestors.
        track_scroll.insert_action_group("ed", Some(&ed_action_group));

        // Delete key on the editor's ColumnView removes the selected
        // rows from the playlist (canonical play order) and rewrites
        // the on-disk M3U8 — same behavior as the Remove from Playlist
        // menu item.
        {
            let key = EventControllerKey::new();
            let sel    = edit_multi_sel.clone();
            let et     = editing_tracks.clone();
            let ep_id  = editing_pl_id.clone();
            let rb     = rebuild_track_list.clone();
            let st     = state.clone();
            key.connect_key_pressed(move |_, keyval, _keycode, _mods| {
                // `l` — View/Search Lyrics for the single selected editor row
                // in Specific mode. No-op (Proceed) on a multi-row or empty
                // selection, matching the row menu's single-selection rule.
                if matches!(keyval, gdk::Key::l | gdk::Key::L) {
                    let sel_tracks: Vec<crate::media_library::LibTrack> = (0..sel.n_items())
                        .filter(|i| sel.is_selected(*i))
                        .filter_map(|i| sel.item(i))
                        .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().track.clone())
                        .collect();
                    if let [t] = sel_tracks.as_slice() {
                        let path = std::path::PathBuf::from(&t.path);
                        let artist = t.artist.clone().unwrap_or_default();
                        let title = t.title.clone().unwrap_or_default();
                        let album_artist = t.album_artist.clone().unwrap_or_default();
                        view_or_search_lyrics(
                            &st, &path, &artist, &title, &album_artist,
                            rb.clone(), LyricsMode::Specific,
                        );
                        return glib::Propagation::Stop;
                    }
                    return glib::Propagation::Proceed;
                }
                if keyval != gdk::Key::Delete && keyval != gdk::Key::KP_Delete {
                    return glib::Propagation::Proceed;
                }
                let mut idxs: Vec<usize> = (0..sel.n_items())
                    .filter(|i| sel.is_selected(*i))
                    .filter_map(|i| sel.item(i))
                    .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .collect();
                if idxs.is_empty() { return glib::Propagation::Proceed }
                idxs.sort_unstable_by(|a, b| b.cmp(a));
                {
                    let mut e = et.borrow_mut();
                    for i in idxs.iter() {
                        if *i < e.len() { e.remove(*i); }
                    }
                }
                let pid = ep_id.get();
                if pid >= 0 {
                    let s = st.borrow();
                    if let Some(lib) = s.media_lib.as_ref() {
                        let paths: Vec<String> = et.borrow()
                            .iter().map(|t| t.path.clone()).collect();
                        if let Ok(pl) = lib.playlist_by_id(pid) {
                            let _ = lib.save_playlist_tracks_to_path(
                                std::path::Path::new(&pl.path),
                                &paths,
                            );
                        }
                    }
                }
                rb();
                glib::Propagation::Stop
            });
            track_list.add_controller(key);
        }

        // Editor DropTarget — handles two drop kinds:
        //
        //   1. Reorder (every dropped path already in `editing_tracks`):
        //      splice the rows to the canonical insert position resolved
        //      from the drop coordinate.  Gated by `reorder_allowed` so
        //      drops while a non-position sort is active no-op rather than
        //      adding duplicates at the bottom.
        //   2. External add (any dropped path not in `editing_tracks`):
        //      append the *new* paths to the on-disk M3U8 via
        //      `append_paths_to_playlist` and mirror them into the
        //      editor's in-memory state.
        {
            let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
            let state_drop  = state.clone();
            let et_drop     = editing_tracks.clone();
            let ep_drop     = editing_pl_id.clone();
            let rebuild_drop = rebuild_track_list.clone();
            let _posmap_drop = position_map.clone();
            let ra_drop     = reorder_allowed.clone();
            let query_drop  = pl_edit_query.clone();
            let tl_drop     = track_list.clone();
            let dragsel_drop = drag_selection.clone();
            dt.connect_drop(move |_, value, x, y| {
                let file_list = match value.get::<gdk::FileList>() {
                    Ok(fl) => fl,
                    Err(_) => return false,
                };
                let paths: Vec<String> = file_list.files().iter()
                    .filter_map(|f| f.path())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                if paths.is_empty() { return false }
                let pid = ep_drop.get();
                let lib_opt_has = state_drop.borrow().media_lib.is_some();
                if !lib_opt_has { return false }

                // Prefer drag_selection (canonical indices captured by
                // our DragSource) so duplicates in the playlist resolve
                // correctly.  If the drag came from another window the
                // selection is empty — treat the paths as external add.
                let drag_src_indices: Vec<usize> = dragsel_drop.borrow().clone();
                let is_internal_reorder = !drag_src_indices.is_empty();

                if is_internal_reorder {
                    // Pure reorder.  Refuse silently when the current sort
                    // doesn't make reorder semantically sensible — avoids
                    // appending duplicates at the bottom in that case.  A
                    // live search filter breaks the display↔play-order
                    // mapping the same way, so it refuses too.
                    if !ra_drop.get() || !query_drop.borrow().is_empty() {
                        dragsel_drop.borrow_mut().clear();
                        return true;
                    }

                    // Resolve the drop coordinate to a canonical insert
                    // position.  First try pick(x, y) + walk up — works
                    // when the cursor is over a cell.  Falls back to a
                    // scan of every visible cell when the cursor lands
                    // between rows (no cell directly under it), inserting
                    // before the first cell whose vertical midpoint is
                    // past the drop y.  Last-resort default is append.
                    let dst_canon: usize = (|| {
                        let mut w = tl_drop.pick(x, y, gtk4::PickFlags::DEFAULT)?;
                        loop {
                            let name = w.widget_name().to_string();
                            if let Some(rest) = name.strip_prefix("pos:") {
                                if let Ok(n) = rest.parse::<usize>() {
                                    return Some(n);
                                }
                            }
                            w = w.parent()?;
                        }
                    })()
                    .or_else(|| {
                        let root_widget: &gtk4::Widget = tl_drop.upcast_ref();
                        let mut cells = editor_cell_positions(root_widget);
                        cells.sort_by(|a, b| a.1.partial_cmp(&b.1)
                            .unwrap_or(std::cmp::Ordering::Equal));
                        let drop_y = y as f32;
                        cells.iter()
                            .find(|c| c.1 + c.2 / 2.0 > drop_y)
                            .map(|c| c.0)
                    })
                    .unwrap_or_else(|| et_drop.borrow().len());

                    let mut sorted = drag_src_indices.clone();
                    sorted.sort_unstable_by(|a, b| b.cmp(a));
                    let mut adjusted_dst = dst_canon;
                    let mut removed: Vec<crate::media_library::LibTrack> = Vec::new();
                    {
                        let mut et = et_drop.borrow_mut();
                        for src in sorted.iter() {
                            if *src < et.len() {
                                let t = et.remove(*src);
                                if *src < adjusted_dst { adjusted_dst -= 1; }
                                removed.push(t);
                            }
                        }
                        removed.reverse();
                        let cap = et.len();
                        let insert_at = adjusted_dst.min(cap);
                        for (i, t) in removed.into_iter().enumerate() {
                            et.insert(insert_at + i, t);
                        }
                    }

                    if pid >= 0 {
                        let s = state_drop.borrow();
                        if let Some(lib) = s.media_lib.as_ref() {
                            let paths_now: Vec<String> = et_drop.borrow()
                                .iter().map(|t| t.path.clone()).collect();
                            if let Ok(pl) = lib.playlist_by_id(pid) {
                                if let Err(e) = lib.save_playlist_tracks_to_path(
                                    std::path::Path::new(&pl.path),
                                    &paths_now,
                                ) {
                                    eprintln!("editor reorder persist {pid}: {e}");
                                }
                            }
                        }
                    }
                    dragsel_drop.borrow_mut().clear();
                    let rb = rebuild_drop.clone();
                    glib::idle_add_local_once(move || rb());
                    return true;
                }

                // External add: append every dropped path; the user's
                // playlist may already contain some of them but treating
                // a cross-window drop as add is the least-surprising
                // semantics (duplicates can be removed afterwards).
                let new_paths: Vec<String> = paths.clone();
                if new_paths.is_empty() { return true }
                // Persist to disk first; only mutate in-memory editor state
                // if the save succeeded so failures don't leave the editor
                // diverged from the file on disk.
                if pid >= 0 {
                    let s = state_drop.borrow();
                    let lib = s.media_lib.as_ref().unwrap();
                    if let Err(e) = lib.append_paths_to_playlist(pid, &new_paths) {
                        eprintln!("editor drop append_paths_to_playlist {pid}: {e}");
                        return false;
                    }
                }
                let paths = new_paths;
                // Mirror the new entries into editing_tracks so the visible
                // ColumnView reflects them without needing a full reload.
                let new_libtracks: Vec<crate::media_library::LibTrack> = {
                    let s = state_drop.borrow();
                    let lib = s.media_lib.as_ref().unwrap();
                    paths.iter()
                        .map(|p| {
                            if let Ok(t) = lib.track_by_path(p) { return t }
                            let filename = std::path::Path::new(p)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            crate::media_library::LibTrack {
                                id: 0,
                                path: p.clone(),
                                filename,
                                artist: None, title: None, album: None,
                                track_num: None, genre: None, year: None,
                                bpm: None, length_secs: None, bitrate: None,
                                channels: None, filetype: None,
                                play_count: 0, last_played: None,
                                comment: None, album_artist: None,
                                disc_num: None, disc_total: None,
                                composer: None, original_artist: None,
                                copyright: None, url: None, encoded_by: None,
                                lyric: None, artwork_path: None,
                                last_scanned: None,
                                sample_rate: None, file_size: None,
                                file_mtime: None, added_at: None,
                                bitrate_mode: None,
                                rg_track_gain: None,
                                rg_track_peak: None,
                                rg_album_gain: None,
                                rg_album_peak: None,
                                sort_keys: Default::default(),
                            }
                        })
                        .collect()
                };
                et_drop.borrow_mut().extend(new_libtracks);
                let rb = rebuild_drop.clone();
                glib::idle_add_local_once(move || rb());
                true
            });
            track_scroll.add_controller(dt);
        }

        // Track editor controls
        let edit_btn_row = GtkBox::new(Orientation::Horizontal, 4);
        edit_btn_row.set_margin_start(4); edit_btn_row.set_margin_end(4);
        edit_btn_row.set_margin_top(4);  edit_btn_row.set_margin_bottom(4);

        let btn_add_files_pl  = Button::with_label("+ Files");    btn_add_files_pl.add_css_class("pl-btn");
        let btn_add_folder_pl = Button::with_label("+ Folder");   btn_add_folder_pl.add_css_class("pl-btn");
        let btn_remove_tracks = Button::with_label("− Remove");   btn_remove_tracks.add_css_class("pl-btn");
        let btn_delete_pl     = Button::with_label("🗑 Delete Playlist"); btn_delete_pl.add_css_class("pl-btn");
        let spring_pl         = GtkBox::new(Orientation::Horizontal, 0); spring_pl.set_hexpand(true);
        let btn_revert_pl     = Button::with_label("↺ Revert");  btn_revert_pl.add_css_class("pl-btn");
        let btn_save_as_pl    = Button::with_label("Save As…");  btn_save_as_pl.add_css_class("pl-btn");
        let btn_save_pl       = btn_save_pl_outer.clone();
        let btn_enqueue_pl    = Button::with_label("Enqueue"); btn_enqueue_pl.add_css_class("pl-btn");
        let btn_send_to_ed    = gtk4::MenuButton::builder().label("Send to ▾").build();
        btn_send_to_ed.add_css_class("pl-btn");
        // Install "ed" directly on the button too — mirrors the files
        // view's btn_send_to: window-level alone enables the top-level
        // items but the NESTED submenu popovers (Saved Playlist ▸, Disc
        // Drive ▸, Entire playlist to device ▸) resolve actions against
        // the button's own popover chain, so their items don't dispatch
        // unless the group also sits on the button itself.
        btn_send_to_ed.insert_action_group("ed", Some(&ed_action_group));
        let btn_play_pl       = Button::with_label("▶ Play");  btn_play_pl.add_css_class("pl-btn");

        edit_btn_row.append(&btn_add_files_pl);
        edit_btn_row.append(&btn_add_folder_pl);
        edit_btn_row.append(&btn_remove_tracks);
        edit_btn_row.append(&btn_delete_pl);
        edit_btn_row.append(&spring_pl);
        edit_btn_row.append(&btn_revert_pl);
        edit_btn_row.append(&btn_save_as_pl);
        edit_btn_row.append(&btn_save_pl);
        edit_btn_row.append(&btn_enqueue_pl);
        edit_btn_row.append(&btn_send_to_ed);
        edit_btn_row.append(&btn_play_pl);
        edit_vbox.append(&ed_status);
        edit_vbox.append(&edit_btn_row);

        // ── Playlist editor status bar ──────────────────────────────────────
        // Rows are `BoxedAnyObject<EditorEntry>` (a LibTrack + its canonical
        // play-order index, not a bare LibTrack — see EditorEntry's doc
        // comment above), so this goes through `ml_status_bar_for` with an
        // extractor into `.track.length_secs`. `rebuild_track_list` (above)
        // always `edit_store.splice(...)`s the SAME store on load/reorder/
        // save-revert rather than swapping in a new one, and it's the store
        // `edit_multi_sel` wraps (via edit_filter_model/edit_sort_model), so
        // items_changed keeps this live without an explicit refresh call.
        let (pl_status_bar, _) = ml_status_bar_for::<EditorEntry>(&edit_multi_sel, |e| {
            e.track.length_secs
        });
        edit_vbox.append(&pl_status_bar);
        // Directly below the playlist track list (above the button row), matching
        // the active playlist window.
        edit_vbox.reorder_child_after(&pl_status_bar, Some(&track_scroll));

        // Whole playlist (files + .m3u8) to a device — the old flat
        // "Send to…" popover's only action, now a target-parameterised
        // action so it can live inside the standard Send-to ▾ menu as an
        // appended "Entire playlist to device" submenu (one item per
        // device). Body moved verbatim from the old per-device button.
        {
            let devices = current_devices.clone();
            let ep_id = editing_pl_id.clone();
            let state_rc = state.clone();
            let send_holder = send_playlist_holder.clone();
            let action = gio::SimpleAction::new(
                "send-playlist-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) = target.and_then(|v| v.get::<String>()) else { return };
                let Some(dev) = devices.borrow().iter().find(|d| d.id == dev_id).cloned()
                else {
                    return;
                };
                let id = ep_id.get();
                if id < 0 {
                    return;
                }
                let name = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|l| l.playlist_by_id(id).ok())
                    .map(|p| p.name)
                    .unwrap_or_default();
                if let Some(send) = send_holder.borrow().clone() {
                    send(dev, id, name);
                }
            });
            ed_action_group.add_action(&action);
        }

        // Rebuild the Send-to menu model fresh on every open — drives/
        // devices may have come or gone. `set_create_popup_func` is
        // invoked by GTK right before the popover is shown; mirrors the
        // files view's btn_send_to.
        {
            let state_menu = state.clone();
            let current_drives = current_drives.clone();
            let current_devices = current_devices.clone();
            btn_send_to_ed.set_create_popup_func(move |btn| {
                let menu = build_send_to_menu(
                    &state_menu,
                    &SendToActions {
                        active: "ed.send-active",
                        new_playlist: "ed.add-to-new",
                        saved_playlist: "ed.add-to-saved",
                        drive: "ed.send-drive",
                        device: "ed.send-device",
                        drives: current_drives.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        devices: current_devices.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    },
                );
                let devs = current_devices.borrow();
                if !devs.is_empty() {
                    let sub = gio::Menu::new();
                    for d in devs.iter() {
                        let label = if d.label.is_empty() {
                            "Untitled device".to_string()
                        } else {
                            d.label.clone()
                        };
                        let item = gio::MenuItem::new(Some(&gtk_safe(&label)), None);
                        item.set_action_and_target_value(
                            Some("ed.send-playlist-device"),
                            Some(&d.id.to_variant()),
                        );
                        sub.append_item(&item);
                    }
                    menu.append_submenu(Some("Entire playlist to device"), &sub);
                }
                btn.set_menu_model(Some(&menu));
            });
        }

        // ── Add Files ─────────────────────────────────────────────────────
        {
            let state_rc = state.clone();
            let et       = editing_tracks.clone();
            let rebuild  = rebuild_track_list.clone();
            let win_wk   = win.downgrade();
            btn_add_files_pl.connect_clicked(move |_| {
                let dialog = gtk4::FileDialog::builder().title("Add Audio Files").build();
                let filter = gtk4::FileFilter::new();
                filter.set_name(Some("Audio files"));
                // add_suffix (not add_mime_type) so files appear even when
                // the desktop has no MIME registration (.ape, .tta, …).
                for ext in crate::model::AUDIO_EXTENSIONS {
                    filter.add_suffix(ext);
                }
                let fs = gio::ListStore::new::<gtk4::FileFilter>();
                fs.append(&filter);
                dialog.set_filters(Some(&fs));
                let state2  = state_rc.clone();
                let et2     = et.clone();
                let rebuild2 = rebuild.clone();
                let parent  = win_wk.upgrade();
                dialog.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    let Ok(list) = result else { return };
                    let paths: Vec<PathBuf> = (0..list.n_items())
                        .filter_map(|i| list.item(i))
                        .filter_map(|o| o.downcast::<gio::File>().ok())
                        .filter_map(|f| f.path())
                        .collect();
                    if paths.is_empty() { return; }
                    let s = state2.borrow();
                    if let Some(ref lib) = s.media_lib {
                        let existing: std::collections::HashSet<String> =
                            et2.borrow().iter().map(|t| t.path.clone()).collect();
                        for p in &paths {
                            if let Some(p_str) = p.to_str() {
                                if !existing.contains(p_str) {
                                    // A file the library does not know cannot
                                    // be added, because the playlist is saved
                                    // by row id. Say so rather than dropping
                                    // it: silently ignoring the file the user
                                    // just picked reads as "Save did nothing".
                                    match lib.track_by_path(p_str) {
                                        Ok(t) => et2.borrow_mut().push(t),
                                        Err(e) => eprintln!(
                                            "add to playlist: skipping {p_str}: {e:#}"
                                        ),
                                    }
                                }
                            }
                        }
                    }
                    drop(s);
                    rebuild2();
                });
            });
        }

        // ── Add Folder ────────────────────────────────────────────────────
        {
            let state_rc = state.clone();
            let et       = editing_tracks.clone();
            let rebuild  = rebuild_track_list.clone();
            let win_wk   = win.downgrade();
            btn_add_folder_pl.connect_clicked(move |_| {
                let dialog = gtk4::FileDialog::builder().title("Add Folder").build();
                let state2   = state_rc.clone();
                let et2      = et.clone();
                let rebuild2 = rebuild.clone();
                let parent   = win_wk.upgrade();
                dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else { return };
                    let Some(folder) = file.path() else { return };
                    let Some(folder_str) = folder.to_str() else { return };
                    let s = state2.borrow();
                    if let Some(ref lib) = s.media_lib {
                        let existing: std::collections::HashSet<String> =
                            et2.borrow().iter().map(|t| t.path.clone()).collect();
                        let new_tracks: Vec<_> = lib.all_tracks().unwrap_or_default()
                            .into_iter()
                            .filter(|t| t.path.starts_with(folder_str) && !existing.contains(&t.path))
                            .collect();
                        et2.borrow_mut().extend(new_tracks);
                    }
                    drop(s);
                    rebuild2();
                });
            });
        }

        // ── Remove selected tracks ────────────────────────────────────────
        {
            let sel     = edit_multi_sel.clone();
            let et      = editing_tracks.clone();
            let rebuild = rebuild_track_list.clone();
            btn_remove_tracks.connect_clicked(move |_| {
                // Map display-index selection through EditorEntry so each
                // selected row resolves to its canonical play-order slot.
                // Otherwise duplicates / a non-default sort cause the wrong
                // rows to be removed from `editing_tracks`.
                let mut to_remove: Vec<usize> = (0..sel.n_items())
                    .filter(|i| sel.is_selected(*i))
                    .filter_map(|i| sel.item(i))
                    .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .collect();
                if to_remove.is_empty() { return }
                to_remove.sort_unstable_by(|a, b| b.cmp(a));
                let mut tracks = et.borrow_mut();
                for idx in to_remove.into_iter() {
                    if idx < tracks.len() { tracks.remove(idx); }
                }
                drop(tracks);
                rebuild();
            });
        }

        // ── Revert ────────────────────────────────────────────────────────
        {
            let load    = load_pl_by_id.clone();
            let sidebar_ref = sidebar.clone();
            btn_revert_pl.connect_clicked(move |_| {
                // Find currently-selected sidebar pl: row
                let mut i = 0i32;
                loop {
                    match sidebar_ref.row_at_index(i) {
                        Some(row) => {
                            let name = row.widget_name().to_string();
                            if row.is_selected() {
                                if let Some(id_str) = name.strip_prefix("pl:") {
                                    if let Ok(id) = id_str.parse::<i64>() { load(id); }
                                }
                                break;
                            }
                            i += 1;
                        }
                        None => break,
                    }
                }
            });
        }

        // ── Save As playlist ──────────────────────────────────────────────
        {
            let state_rc     = state.clone();
            let et           = editing_tracks.clone();
            let ep_id        = editing_pl_id.clone();
            let load         = load_pl_by_id.clone();
            let sidebar_ref  = sidebar.clone();
            let pl_ml_ref    = pl_manage_list.clone();
            let win_wk       = win.downgrade();
            btn_save_as_pl.connect_clicked(move |_| {
                let Some(win) = win_wk.upgrade() else { return };
                // Pre-fill the Save dialog with the current playlist's name
                // (or "New Playlist" when the editor has no playlist loaded).
                let initial_stem = if ep_id.get() >= 0 {
                    state_rc.borrow().media_lib.as_ref()
                        .and_then(|lib| lib.playlist_by_id(ep_id.get()).ok())
                        .map(|pl| pl.name)
                        .unwrap_or_else(|| "New Playlist".to_string())
                } else {
                    "New Playlist".to_string()
                };
                let paths: Vec<String> = et.borrow().iter().map(|t| t.path.clone()).collect();
                let state2   = state_rc.clone();
                let ep_id2   = ep_id.clone();
                let load2    = load.clone();
                let sidebar2 = sidebar_ref.clone();
                let pl_ml2   = pl_ml_ref.clone();
                // Native Save dialog replaces the previous name-only popup —
                // user chooses both filename and folder so the new .m3u8
                // doesn't silently land in the managed-playlists dir (which
                // `add_playlist_file` then registered as a watched folder).
                run_playlist_save_dialog(state_rc.clone(), win, &initial_stem, move |path, win_cb| {
                    let new_name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let save_result = state2.borrow().media_lib.as_ref()
                        .map(|lib| lib.save_playlist_tracks_to_path(&path, &paths));
                    let new_id = match save_result {
                        Some(Ok(id)) => id,
                        Some(Err(e)) => {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                            return;
                        }
                        None => return,
                    };

                    // Add row to manage list + sidebar
                    let lbl = Label::builder()
                        .label(&new_name)
                        .halign(Align::Start)
                        .margin_start(8).margin_end(8)
                        .margin_top(3).margin_bottom(3)
                        .build();
                    let manage_row = ListBoxRow::new();
                    manage_row.set_widget_name(&new_id.to_string());
                    manage_row.set_child(Some(&lbl));
                    attach_pl_row_drag(&manage_row, new_id);
                    pl_ml2.append(&manage_row);

                    let s_lbl = Label::builder()
                        .label(&new_name)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(sidebar::SUB_ROW_INSET).margin_end(8)
                        .margin_top(4).margin_bottom(4)
                        .build();
                    let s_row = ListBoxRow::new();
                    s_row.set_widget_name(&format!("pl:{}", new_id));
                    s_row.set_child(Some(&s_lbl));
                    attach_pl_row_drag(&s_row, new_id);
                    sidebar2.insert(&s_row, sidebar_pl_end_index(&sidebar2));
                    sidebar2.select_row(Some(&s_row));

                    ep_id2.set(new_id);
                    load2(new_id);
                });
            });
        }

        // ── Save playlist ─────────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let et          = editing_tracks.clone();
            let saved       = saved_track_ids.clone();
            let ep_id       = editing_pl_id.clone();
            btn_save_pl.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let track_ids: Vec<i64> = et.borrow().iter().map(|t| t.id).collect();
                if let Some(ref lib) = state_rc.borrow().media_lib {
                    // Report a failed write instead of discarding it. Save is
                    // the one button whose whole purpose is a side effect, so
                    // swallowing the Result made a failure indistinguishable
                    // from success — and left `saved` claiming state that
                    // never reached disk, so the dirty indicator cleared too.
                    if let Err(e) = lib.save_playlist_tracks(id, &track_ids) {
                        eprintln!("save_playlist_tracks {id}: {e:#}");
                        return;
                    }
                    *saved.borrow_mut() = track_ids;
                }
            });
        }

        // ── Play (replace active playlist; honour autoplay) ──────────────
        {
            let state_rc   = state.clone();
            let et         = editing_tracks.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track2 = set_track.clone();
            btn_play_pl.connect_clicked(move |_| {
                let tracks: Vec<crate::media_library::LibTrack> = et.borrow().clone();
                if tracks.is_empty() { return; }
                let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                {
                    let mut s = state_rc.borrow_mut();
                    let _ = s.player.stop();
                    s.playlist = crate::model::Playlist::new();
                    for lt in &tracks {
                        s.playlist.add(crate::model::Track::from(lt));
                    }
                }
                if autoplay {
                    if let Some(display) = state_rc.borrow_mut().play_current() {
                        set_track2(&display);
                    }
                }
                rebuild_pl();
            });
        }

        // ── Enqueue (append to active playlist) ──────────────────────────
        {
            let state_rc   = state.clone();
            let et         = editing_tracks.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track2 = set_track.clone();
            btn_enqueue_pl.connect_clicked(move |_| {
                let tracks: Vec<crate::media_library::LibTrack> = et.borrow().clone();
                if tracks.is_empty() { return; }
                let was_empty = state_rc.borrow().playlist.is_empty();
                let autoplay  = state_rc.borrow().config.behavior.autoplay_on_add;
                {
                    let mut s = state_rc.borrow_mut();
                    for lt in &tracks {
                        s.playlist.add(crate::model::Track::from(lt));
                    }
                }
                // Don't interrupt a track the user is already listening to.
                if autoplay && was_empty {
                    if let Some(display) = state_rc.borrow_mut().play_current() {
                        set_track2(&display);
                    }
                }
                rebuild_pl();
            });
        }

        // ── Delete this playlist ─────────────────────────────────────────
        {
            let state_rc      = state.clone();
            let ep_id         = editing_pl_id.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let sub_rows_ref  = pl_sub_rows.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            let et            = editing_tracks.clone();
            let saved         = saved_track_ids.clone();
            let rebuild       = rebuild_track_list.clone();
            let win_wk        = win.downgrade();
            btn_delete_pl.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let pl_name = state_rc.borrow().media_lib.as_ref()
                    .and_then(|lib| lib.playlist_by_id(id).ok())
                    .map(|pl| pl.name.clone())
                    .unwrap_or_default();

                let dialog = gtk4::AlertDialog::builder()
                    .message(format!("Delete \"{}\"?", pl_name))
                    .detail("The playlist file on disk is not deleted.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0).default_button(1).modal(true).build();

                let state2   = state_rc.clone();
                let ep_id2   = ep_id.clone();
                let pl_ref2  = pl_list_ref.clone();
                let sid2     = sidebar_ref.clone();
                let sub2     = sub_rows_ref.clone();
                let pls2     = pl_sub_ref.clone();
                let et2      = et.clone();
                let saved2   = saved.clone();
                let rebuild2 = rebuild.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                    if result != Ok(1) { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.remove_playlist(id);
                    }
                    // Drop the manage-list row whose widget_name == id.
                    let target = id.to_string();
                    let mut i = 0i32;
                    loop {
                        match pl_ref2.row_at_index(i) {
                            Some(r) if r.widget_name() == target => {
                                pl_ref2.remove(&r);
                                break;
                            }
                            Some(_) => i += 1,
                            None => break,
                        }
                    }
                    // Drop the matching sidebar sub-row.
                    let target_s = format!("pl:{}", id);
                    sub2.borrow_mut().retain(|r| {
                        if r.widget_name() == target_s {
                            sid2.remove(r);
                            false
                        } else { true }
                    });
                    // Clear editing state and bounce back to the manage page.
                    ep_id2.set(-1);
                    et2.borrow_mut().clear();
                    saved2.borrow_mut().clear();
                    rebuild2();
                    pls2.set_visible_child_name("pl-manage");
                });
            });
        }

        // ── Rename this playlist (header-row button) ─────────────────────
        {
            let state_rc      = state.clone();
            let ep_id         = editing_pl_id.clone();
            let header_ref    = edit_header.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let win_wk        = win.downgrade();
            btn_rename_pl_inline.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let current = state_rc.borrow().media_lib.as_ref()
                    .and_then(|lib| lib.playlist_by_id(id).ok())
                    .map(|pl| pl.name.clone())
                    .unwrap_or_default();

                let dialog = gtk4::Window::builder()
                    .title("Rename Playlist").modal(true).resizable(false).default_width(300)
                    .build();
                if let Some(w) = win_wk.upgrade() { dialog.set_transient_for(Some(&w)); }
                let vbox = GtkBox::new(Orientation::Vertical, 8);
                vbox.set_margin_top(12); vbox.set_margin_bottom(12);
                vbox.set_margin_start(12); vbox.set_margin_end(12);
                let lbl = Label::builder().label("New name:").halign(Align::Start).build();
                let name_entry = Entry::new();
                name_entry.set_text(&gtk_safe(&current));
                name_entry.set_hexpand(true);
                let btns_box = GtkBox::new(Orientation::Horizontal, 6);
                btns_box.set_halign(Align::End);
                let cancel_btn = Button::with_label("Cancel");
                let ok_btn     = Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                btns_box.append(&cancel_btn); btns_box.append(&ok_btn);
                vbox.append(&lbl); vbox.append(&name_entry); vbox.append(&btns_box);
                dialog.set_child(Some(&vbox));

                let d = dialog.clone();
                cancel_btn.connect_clicked(move |_| { d.close(); });

                let d        = dialog.clone();
                let e        = name_entry.clone();
                let state2   = state_rc.clone();
                let header2  = header_ref.clone();
                let pl_ref2  = pl_list_ref.clone();
                let sid2     = sidebar_ref.clone();
                ok_btn.connect_clicked(move |_| {
                    let name = e.text().to_string();
                    let name = name.trim();
                    if name.is_empty() { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.rename_playlist(id, name);
                    }
                    header2.set_text(&gtk_safe(name));
                    // Update manage-list row label.
                    let target = id.to_string();
                    let mut i = 0i32;
                    loop {
                        match pl_ref2.row_at_index(i) {
                            Some(r) if r.widget_name() == target => {
                                if let Some(c) = r.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(name));
                                    }
                                }
                                break;
                            }
                            Some(_) => i += 1,
                            None => break,
                        }
                    }
                    // Update sidebar sub-row label.
                    let target_s = format!("pl:{}", id);
                    let mut j = 0i32;
                    loop {
                        match sid2.row_at_index(j) {
                            Some(r) if r.widget_name() == target_s => {
                                if let Some(c) = r.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(name));
                                    }
                                }
                                break;
                            }
                            Some(_) => j += 1,
                            None => break,
                        }
                    }
                    d.close();
                });
                let ok2 = ok_btn.clone();
                name_entry.connect_activate(move |_| { ok2.activate(); });
                dialog.present();
            });
        }

        // ── Right-click context menu on track rows ───────────────────────
        // Add to / Replace active playlist, Edit ID3 (single only), Remove
        // from Library.  No album-art viewer in GTK so that entry is
        // omitted here.
        {
            // ctx_canonical_idx is now hoisted above the column builder so each
            // editor cell's right-click gesture can record into it.  Reuse
            // the outer binding so action handlers see the same Cell.
            let action_group = gio::SimpleActionGroup::new();

            // Helper: collect the canonical indices the action should
            // operate on — the current multi-selection, falling back to
            // the single right-clicked row when nothing is selected.
            let selected_canonical_indices = {
                let sel = edit_multi_sel.clone();
                let id_ref = ctx_canonical_idx.clone();
                Rc::new(move || -> Vec<usize> {
                    let mut idxs: Vec<usize> = (0..sel.n_items())
                        .filter(|i| sel.is_selected(*i))
                        .filter_map(|i| sel.item(i))
                        .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        .collect();
                    if idxs.is_empty() {
                        let c = id_ref.get();
                        if c >= 0 { idxs.push(c as usize); }
                    }
                    idxs
                })
            };

            // ─── Append (add to active playlist) ─────────────────────────
            {
                let state_rc   = state.clone();
                let et         = editing_tracks.clone();
                let rebuild_pl = rebuild_playlist.clone();
                let set_track2 = set_track.clone();
                let pick_idxs  = selected_canonical_indices.clone();
                let action     = gio::SimpleAction::new("append", None);
                action.connect_activate(move |_, _| {
                    let tracks: Vec<crate::media_library::LibTrack> = {
                        let et_b = et.borrow();
                        pick_idxs().into_iter()
                            .filter_map(|i| et_b.get(i).cloned())
                            .collect()
                    };
                    if tracks.is_empty() { return }
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay  = state_rc.borrow().config.behavior.autoplay_on_add;
                    {
                        let mut s = state_rc.borrow_mut();
                        for lt in &tracks {
                            s.playlist.add(crate::model::Track::from(lt));
                        }
                    }
                    if autoplay && was_empty {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track2(&display);
                        }
                    }
                    rebuild_pl();
                });
                action_group.add_action(&action);
            }

            // ─── Replace (active playlist becomes the selection) ─────────
            {
                let state_rc   = state.clone();
                let et         = editing_tracks.clone();
                let rebuild_pl = rebuild_playlist.clone();
                let set_track2 = set_track.clone();
                let pick_idxs  = selected_canonical_indices.clone();
                let action     = gio::SimpleAction::new("replace", None);
                action.connect_activate(move |_, _| {
                    let tracks: Vec<crate::media_library::LibTrack> = {
                        let et_b = et.borrow();
                        pick_idxs().into_iter()
                            .filter_map(|i| et_b.get(i).cloned())
                            .collect()
                    };
                    if tracks.is_empty() { return }
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    {
                        let mut s = state_rc.borrow_mut();
                        let _ = s.player.stop();
                        s.playlist = crate::model::Playlist::new();
                        for lt in &tracks {
                            s.playlist.add(crate::model::Track::from(lt));
                        }
                    }
                    if autoplay {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track2(&display);
                        }
                    }
                    rebuild_pl();
                });
                action_group.add_action(&action);
            }

            // ─── Edit ID3 (single only) ──────────────────────────────────
            {
                let state_rc      = state.clone();
                let id_ref        = ctx_canonical_idx.clone();
                let et            = editing_tracks.clone();
                let rebuild_pl    = rebuild_playlist.clone();
                let action        = gio::SimpleAction::new("edit-id3", None);
                action.connect_activate(move |_, _| {
                    let c = id_ref.get();
                    if c < 0 { return }
                    let path = et.borrow().get(c as usize)
                        .map(|t| t.path.clone());
                    let Some(path) = path else {
                        return;
                    };
                    open_id3_editor_window(
                        None::<&gtk4::Window>,
                        path.into(),
                        state_rc.clone(),
                        rebuild_pl.clone(),
                        None,
                        None,
                    );
                });
                action_group.add_action(&action);
            }

            // ─── View/Search Lyrics (F15, single only) ───────────────────
            {
                let state_rc      = state.clone();
                let id_ref        = ctx_canonical_idx.clone();
                let et            = editing_tracks.clone();
                let rebuild_pl    = rebuild_playlist.clone();
                let action        = gio::SimpleAction::new("lyrics", None);
                action.connect_activate(move |_, _| {
                    let c = id_ref.get();
                    if c < 0 { return }
                    let t = et.borrow().get(c as usize).map(|t| {
                        (
                            std::path::PathBuf::from(&t.path),
                            t.artist.clone().unwrap_or_default(),
                            t.title.clone().unwrap_or_default(),
                            t.album_artist.clone().unwrap_or_default(),
                        )
                    });
                    let Some((path, artist, title, album_artist)) = t else { return };
                    view_or_search_lyrics(&state_rc, &path, &artist, &title, &album_artist, rebuild_pl.clone(), LyricsMode::Specific);
                });
                action_group.add_action(&action);
            }

            // ─── Remove from Playlist (mutate editing_tracks + persist) ──
            // Removes selected rows from the canonical play order and
            // immediately rewrites the on-disk M3U8.  Does NOT delete the
            // track from the media library — the user's library DB is
            // untouched.
            {
                let state_rc = state.clone();
                let et       = editing_tracks.clone();
                let ep_id    = editing_pl_id.clone();
                let rebuild  = rebuild_track_list.clone();
                let pick_idxs = selected_canonical_indices.clone();
                let action   = gio::SimpleAction::new("remove", None);
                action.connect_activate(move |_, _| {
                    let mut idxs = pick_idxs();
                    if idxs.is_empty() { return }
                    idxs.sort_unstable_by(|a, b| b.cmp(a));
                    {
                        let mut e = et.borrow_mut();
                        for i in idxs.iter() {
                            if *i < e.len() { e.remove(*i); }
                        }
                    }
                    let pid = ep_id.get();
                    if pid >= 0 {
                        let s = state_rc.borrow();
                        if let Some(lib) = s.media_lib.as_ref() {
                            let paths: Vec<String> = et.borrow()
                                .iter().map(|t| t.path.clone()).collect();
                            if let Ok(pl) = lib.playlist_by_id(pid) {
                                if let Err(e) = lib.save_playlist_tracks_to_path(
                                    std::path::Path::new(&pl.path),
                                    &paths,
                                ) {
                                    eprintln!("ple.remove persist {pid}: {e}");
                                }
                            }
                        }
                    }
                    rebuild();
                });
                action_group.add_action(&action);
            }

            // ─── Seed a new saved playlist from the editor selection ─────
            {
                let state_rc = state.clone();
                let sel      = edit_multi_sel.clone();
                let et       = editing_tracks.clone();
                let win_atn  = win.clone();
                let action   = gio::SimpleAction::new("add-to-new", None);
                action.connect_activate(move |_, _| {
                    let paths: Vec<String> = {
                        let et_b = et.borrow();
                        // Selection indices are display positions in the
                        // sorted model — map each through EditorEntry to
                        // the canonical play-order slot so duplicates and
                        // non-default sorts both resolve correctly.
                        let mut p: Vec<String> = (0..sel.n_items())
                            .filter(|i| sel.is_selected(*i))
                            .filter_map(|i| sel.item(i))
                            .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                            .filter_map(|c| et_b.get(c))
                            .map(|t| t.path.clone())
                            .collect();
                        if p.is_empty() {
                            p = et_b.iter().map(|t| t.path.clone()).collect();
                        }
                        p
                    };
                    if paths.is_empty() { return }
                    let default_stem = glib::DateTime::now_local()
                        .ok()
                        .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Playlist".to_string());
                    let state_cb = state_rc.clone();
                    let paths_cb = paths.clone();
                    run_playlist_save_dialog(
                        state_rc.clone(),
                        win_atn.clone(),
                        &default_stem,
                        move |path, win_cb| {
                            if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                                if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                                    eprintln!("save_playlist_tracks_to_path: {e}");
                                    show_playlist_save_error(&win_cb, &path, &e);
                                }
                            }
                        },
                    );
                });
                action_group.add_action(&action);
            }

            // ─── Add selection to a saved playlist (parameterised by id) ─
            {
                let state_rc = state.clone();
                let sel      = edit_multi_sel.clone();
                let et       = editing_tracks.clone();
                let action   = gio::SimpleAction::new(
                    "add-to-saved",
                    Some(glib::VariantTy::INT64),
                );
                action.connect_activate(move |_, param| {
                    let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
                    let paths: Vec<String> = {
                        let et_borrow = et.borrow();
                        (0..sel.n_items())
                            .filter(|i| sel.is_selected(*i))
                            .filter_map(|i| sel.item(i))
                            .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                            .filter_map(|c| et_borrow.get(c))
                            .map(|t| t.path.clone())
                            .collect()
                    };
                    if paths.is_empty() { return }
                    let mut ok = false;
                    if let Some(lib) = state_rc.borrow().media_lib.as_ref() {
                        match lib.append_paths_to_playlist(pid, &paths) {
                            Ok(_)  => ok = true,
                            Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                        }
                    }
                    if ok { notify_playlist_changed(pid); }
                });
                action_group.add_action(&action);
            }

            track_list.insert_action_group("ple", Some(&action_group));
            if let Some(ref ts) = *track_scroll_holder.borrow() {
                ts.insert_action_group("ple", Some(&action_group));
            }
            win.insert_action_group("ple", Some(&action_group));
            // ALSO attach the actions to the GtkApplication (app-level)
            // under "app-ple-*" names — PopoverMenu dispatch via the
            // app prefix is the reliable code path in GTK4, even when
            // widget-tree action lookup fails for nested popovers.
            if let Some(app) = win.application() {
                let app_action_names = ["append", "replace", "edit-id3", "lyrics",
                                        "remove", "add-to-new", "add-to-saved"];
                for name in app_action_names {
                    if let Some(act) = action_group.lookup_action(name) {
                        let app_name = format!("ple-{name}");
                        let simple = act.downcast_ref::<gio::SimpleAction>();
                        if let Some(sa) = simple {
                            // Build a parallel app-level SimpleAction
                            // that forwards activate to the editor's
                            // group action.  Same parameter type.
                            let app_action = gio::SimpleAction::new(
                                &app_name,
                                sa.parameter_type().as_ref().map(|v| &**v),
                            );
                            let sa_clone = sa.clone();
                            app_action.connect_activate(move |_, param| {
                                eprintln!("[app.{app_name}] forwarding to ple.{name}");
                                sa_clone.activate(param);
                            });
                            app.add_action(&app_action);
                        }
                    }
                }
            }
            *ple_action_group_holder.borrow_mut() = Some(action_group.clone());
            // Per-cell right-click gesture lives inside each column's
            // factory.connect_setup — see the editor column builder at the
            // top of this scope.  Nothing to register here at the row level.

            // Double-click / Enter activates the row: append to the active
            // playlist (matches the ML files view affordance).  Respects
            // the user's playlist_add_behavior preference (Append vs Replace)
            // and autoplay_on_add config.
            {
                let state_rc     = state.clone();
                let et           = editing_tracks.clone();
                let rebuild_pl   = rebuild_playlist.clone();
                let set_track_pe = set_track.clone();
                let sel_act = edit_multi_sel.clone();
                track_list.connect_activate(move |_, pos| {
                    // `pos` is a display position; resolve through the
                    // sorted model to the canonical row in `editing_tracks`.
                    let canon = sel_act.item(pos)
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx);
                    let Some(canon) = canon else { return };
                    let lt = et.borrow().get(canon).cloned();
                    let Some(lt) = lt else { return };
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                        == crate::config::PlaylistAddBehavior::Replace;
                    if should_replace {
                        let _ = state_rc.borrow_mut().player.stop();
                        state_rc.borrow_mut().playlist.clear();
                    }
                    state_rc.borrow_mut().playlist.add(crate::model::Track::from(&lt));
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_pe(&display);
                        }
                    }
                    rebuild_pl();
                });
            }
        }

        pl_sub_stack.add_named(&edit_vbox, Some("pl-edit"));
    }

    {
        let pl_vbox = GtkBox::new(Orientation::Vertical, 0);
        pl_vbox.append(&*pl_sub_stack);
        stack.add_named(&pl_vbox, Some("playlists"));
    }

    // Wire sidebar to stack.
    {
        let stack_ref      = stack.clone();
        let pl_sub_ref     = pl_sub_stack.clone();
        let load           = load_pl_by_id.clone();
        let state_rc       = state.clone();
        let expanded_rc    = playlists_expanded.clone();
        let hdr_lbl        = edit_header.clone();
        let path_lbl       = edit_path_label.clone();
        let save_btn       = btn_save_pl_outer.clone();
        let album_filter_sb = album_filter.clone();
        let btn_album_back_sb = btn_album_back.clone();
        let show_gallery_overview_sb = show_gallery_overview.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let row = match opt_row { Some(r) => r, None => return };
            let name = row.widget_name().to_string();

            if name == "files" {
                // Explicitly returning to Files always means "show the full
                // library" — clear any album drill-down left over from the
                // gallery (Phase 11 A5) and rebuild through the same seam
                // background rebuilds use.
                {
                    *album_filter_sb.borrow_mut() = None;
                }
                btn_album_back_sb.set_visible(false);
                stack_ref.set_visible_child_name("files");
                let cb = state_rc.borrow().rebuild_ml_callback.clone();
                if let Some(cb) = cb {
                    cb();
                }
            } else if name == "albums" {
                // Always land on the gallery overview (clears any drill-down).
                show_gallery_overview_sb();
            } else if name == "playlists" {
                stack_ref.set_visible_child_name("playlists");
                pl_sub_ref.set_visible_child_name("pl-manage");
                // Expand sub-rows on navigation
                if !expanded_rc.get() {
                    expanded_rc.set(true);
                }
            } else if let Some(id_str) = name.strip_prefix("pl:") {
                if let Ok(id) = id_str.parse::<i64>() {
                    stack_ref.set_visible_child_name("playlists");
                    load(id);
                    pl_sub_ref.set_visible_child_name("pl-edit");
                    // Update editor header, path bar, and Save sensitivity.
                    if let Some(ref lib) = state_rc.borrow().media_lib {
                        if let Ok(pl) = lib.playlist_by_id(id) {
                            hdr_lbl.set_text(&gtk_safe(&pl.name));
                            path_lbl.set_text(&gtk_safe(&pl.path));
                            // Disable Save for external playlists; user should
                            // use Save As to get a Sparkamp-managed copy.
                            let is_managed = lib.playlist_is_managed(id);
                            save_btn.set_sensitive(is_managed);
                        }
                    }
                }
            }
        });
    }

    // Clicking the "Albums" sidebar row while it is ALREADY selected (i.e. the
    // user drilled into an album, so the row's highlight never left "Albums")
    // does not re-emit `row-selected`, so that path can't return to the
    // gallery. `row-activated` DOES fire on every click, so handle the
    // return-to-overview here too. Harmless when arriving from another row
    // (both signals fire; `show_gallery_overview` is idempotent).
    {
        let show_gallery_overview_ra = show_gallery_overview.clone();
        sidebar.connect_row_activated(move |_, row| {
            if row.widget_name() == "albums" {
                show_gallery_overview_ra();
            }
        });
    }

    // Persist sidebar expansion state on window close (handled in close_request below).


    // ── Page: Disc Drives ────────────────────────────────────────────────
    // Extracted to `window/disc_page.rs` (plan step 5). Builds the overview
    // cards, the drive detail view and the data-disc browser, adds itself to
    // the stack, and starts the 2 s drive poll that keeps the sidebar's
    // Disc Drives sub-rows live. `sb` goes along for those sub-rows, the
    // chevron state and the header spinner — see the module's `build` doc.
    disc_page::build(&ctx, &sb);

    // ── Page: Devices ────────────────────────────────────────────────────
    // Extracted to `window/devices_page.rs` (plan step 6). Builds the overview
    // cards and the device detail view, adds itself to the stack, and starts
    // the 2 s udisks2 poll that keeps the sidebar's Devices sub-rows live.
    // `sb` goes along for those sub-rows, the chevron state and the
    // send-a-playlist holder — see the module's `build` doc.
    devices_page::build(&ctx, &sb);


    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    let init_sidebar_width = state.borrow().config.window.ml_sidebar_width;
    paned.set_start_child(Some(&sidebar_scroll));
    paned.set_end_child(Some(&stack));
    paned.set_position(init_sidebar_width);
    win.set_child(Some(&paned));

    win.connect_close_request({
        let state = state.clone();
        let playlists_expanded = playlists_expanded.clone();
        let paned_ref = paned.clone();
        let col_view_holder = col_view_holder.clone();
        let all_cols_holder = all_cols_holder.clone();
        move |w| {
            let (w_size, h_size) = (w.width(), w.height());
            // Capture current column display order before borrowing state.
            let col_order: Vec<String> = col_view_holder
                .borrow()
                .as_ref()
                .map(|cv| {
                    let col_model = cv.columns();
                    let ac = all_cols_holder.borrow();
                    (0..col_model.n_items())
                        .filter_map(|i| col_model.item(i)?.downcast::<ColumnViewColumn>().ok())
                        .filter_map(|col| {
                            ac.iter().find(|(_, c)| c == &col).map(|(id, _)| id.clone())
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Capture current per-column widths.
            let col_widths: std::collections::HashMap<String, i32> = {
                let ac = all_cols_holder.borrow();
                ac.iter()
                    .filter_map(|(id, col)| {
                        let w = col.fixed_width();
                        if w > 0 { Some((id.clone(), w)) } else { None }
                    })
                    .collect()
            };
            {
                let mut s = state.borrow_mut();
                s.config.window.ml_width = w_size;
                s.config.window.ml_height = h_size;
                s.config.window.ml_playlists_expanded = playlists_expanded.get();
                s.config.window.ml_sidebar_width = paned_ref.position();
                s.config.media_library.ml_file_col_order = col_order;
                s.config.media_library.ml_file_col_widths = col_widths;
                s.rebuild_ml_callback = None;
            }
            let _ = state.borrow().config.save();
            state.borrow_mut().ml_window = None;
            // Drop the editor-refresh hooks so we don't pin closed-window
            // Rcs in thread-local storage across an ML reopen.
            EDITOR_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
            EDITOR_CURRENT_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
            PLAYLIST_NAV_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
            glib::Propagation::Proceed
        }
    });

    win.present();
    win
}

// ---------------------------------------------------------------------------
// ReplayGain analysis job — shared by the bulk "Analyze ReplayGain" button
// and the Files-view "Calculate ReplayGain" context action.
// ---------------------------------------------------------------------------

/// Spawn the single background ReplayGain analysis worker over `tracks`.
///
/// `force`:
/// - `true` (the per-selection "Calculate ReplayGain" context action):
///   analyze every track in `tracks` unconditionally.
/// - `false` (the bulk "Analyze ReplayGain" button): filter `tracks` down to
///   [`crate::replaygain::needs_analysis`] first — missing or stale only.
///
/// Refuses (and leaves `status_label` untouched by us, but sets a short
/// explanatory message on it) if `tracks` is empty, the media library isn't
/// open, or [`start_rg_job`] reports a scan/analysis already in flight.
///
/// The worker opens its OWN `MediaLibrary` via `MediaLibrary::open_at`
/// (SQLite isn't `Send` — the `AppState.media_lib` connection can't cross
/// the thread boundary). Progress crosses back over an mpsc channel drained
/// by a `glib::timeout_add_local` on the main loop, which is also the only
/// place `rg_job`/`AppState.media_lib` get touched again — never from the
/// worker thread.
fn analyze_job(
    state: &Rc<RefCell<AppState>>,
    tracks: Vec<crate::media_library::LibTrack>,
    force: bool,
    status_label: &Label,
    rebuild: Rc<dyn Fn()>,
) -> bool {
    if tracks.is_empty() {
        status_label.set_text("Nothing to analyze");
        return false;
    }
    let has_lib = state.borrow().media_lib.is_some();
    if !has_lib {
        status_label.set_text("Media library not available");
        return false;
    }
    let Some(cancel_flag) = start_rg_job(state, 0) else {
        status_label.set_text("A scan or analysis is already in progress");
        return false;
    };
    status_label.set_text("Analyzing ReplayGain…");

    let write_tags = state.borrow().config.playback.replaygain.write_tags;
    let db_path = crate::media_library::MediaLibrary::db_path_pub();
    let (progress_tx, progress_rx) =
        std::sync::mpsc::channel::<crate::replaygain::RgJobProgress>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<usize, String>>();
    let cancel_thread = cancel_flag.clone();
    std::thread::spawn(move || {
        let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
            Ok(l) => l,
            Err(e) => {
                let _ = result_tx.send(Err(format!("DB error: {e}")));
                return;
            }
        };
        let targets: Vec<crate::media_library::LibTrack> = if force {
            tracks
        } else {
            tracks
                .into_iter()
                .filter(crate::replaygain::needs_analysis)
                .collect()
        };
        let result = crate::replaygain::analyze_and_store(
            &lib,
            &targets,
            write_tags,
            &cancel_thread,
            |p| {
                let _ = progress_tx.send(p);
            },
        )
        .map_err(|e| e.to_string());
        let _ = result_tx.send(result);
    });

    let progress_rx = std::cell::RefCell::new(progress_rx);
    let result_rx = std::cell::RefCell::new(result_rx);
    let state2 = state.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
        while let Ok(p) = progress_rx.borrow().try_recv() {
            update_rg_job_progress(&state2, p.done, p.total);
        }
        if let Ok(result) = result_rx.borrow().try_recv() {
            {
                let mut s = state2.borrow_mut();
                s.media_lib = crate::media_library::MediaLibrary::open().ok();
            }
            // Hand the result to the shared UI state — each view's poller
            // (`sync_rg_ui`) renders the completion text and flips the
            // Cancel button back to Analyze. Don't write the status label
            // here: two writers (this + the poller) raced and left the Files
            // view stuck on "Analyzing N/M" after completion.
            let msg = match &result {
                Err(e) => format!("ReplayGain analysis error: {e}"),
                Ok(n) => format!("Analyzed {n} track(s)"),
            };
            if result.is_ok() {
                rebuild();
            }
            complete_rg_job(&state2, msg);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
    true
}

