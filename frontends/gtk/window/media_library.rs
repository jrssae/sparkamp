use super::*;

/// Bottom status bar for a Media Library list view: `N tracks · MM:SS total ·
/// MM:SS selected`, matching the active playlist. Works over any MultiSelection
/// whose items are `BoxedAnyObject<T>`; `secs_of` pulls each row's duration out
/// of its `T` (e.g. `LibTrack::length_secs`, `disc::mount::DiscFile::
/// duration_secs`) since the Devices/Files/Playlists views box `LibTrack` rows
/// but the Discs data-file browser boxes `DiscFile` rows instead. Returns the
/// Label (append it to the view's page box) and a refresh closure (already
/// wired to selection + model changes; also call it once after the store is
/// first populated).
pub(super) fn ml_status_bar_for<T: 'static>(
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
pub(super) fn ml_status_bar(selection: &MultiSelection) -> (Label, std::rc::Rc<dyn Fn()>) {
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
pub(super) type RefreshHolder = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// Device-copy runner, shared with player.rs so the active playlist's
/// Send-to menu drives the same copy as the Media Library's device views.
pub(super) type CopyFilesHolder =
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
/// `pub(super)` throughout: this file became a real `mod` in plan step 8, and
/// every page module reads these fields through the window module above it.
pub(super) struct MlHost {
    pub(super) state: Rc<RefCell<AppState>>,
    pub(super) rebuild_playlist: Rc<dyn Fn()>,
    pub(super) set_track: Rc<dyn Fn(&str)>,
    pub(super) current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>>,
    pub(super) current_devices: Rc<RefCell<Vec<crate::devices::Device>>>,
    pub(super) burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>>,
    pub(super) copy_files_holder: CopyFilesHolder,
    /// Filled by the burn panel with a closure that re-renders the shown
    /// drive's queue; the Send-to ▸ Disc Drive actions call it so an external
    /// add updates the open panel live (2026-07-16).
    pub(super) burn_refresh_holder: RefreshHolder,
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
pub(super) struct MlCtx {
    pub(super) host: MlHost,
    /// The window itself — pages parent their dialogs and file choosers to it.
    pub(super) win: gtk4::Window,
    /// The page stack. Pages `add_named` themselves to it and switch to each
    /// other through it.
    pub(super) stack: Stack,
    /// The gallery drill-down: `Some((album, album_artist))` narrows the Files
    /// page to one album's tracks. Written by Albums, read by Files.
    pub(super) album_filter: Rc<RefCell<Option<(String, String)>>>,
    /// "◀ Albums" — lives in the Files search row but is shown and hidden by
    /// the drill-down, so both pages touch it.
    pub(super) btn_album_back: Button,
    /// The Files `ColumnView` and its columns, late-bound because both are
    /// built inside the Files page but the window's close-request has to read
    /// their order and widths back out to save them.
    pub(super) col_view_holder: Rc<RefCell<Option<ColumnView>>>,
    pub(super) all_cols_holder: Rc<RefCell<Vec<(String, ColumnViewColumn)>>>,
}

/// Widget name Ctrl+F looks for. Set on every stack page's search `Entry`
/// (Files, Albums, Discs, Devices, and the Playlists page's own Manage/Edit
/// sub-views) so [`find_visible_search_entry`] finds whichever one is
/// visible without needing to know each page's internals.
pub(super) const ML_SEARCH_ENTRY_NAME: &str = "ml-search-entry";

/// Walks down from `root`, following a [`Stack`] into its visible child only,
/// until it finds a descendant `Entry` marked [`ML_SEARCH_ENTRY_NAME`].
///
/// The window has one search entry per top-level stack page, and the
/// Playlists page nests a second `Stack` (Manage / Edit) with one search
/// entry each — this recurses through both without the caller (Ctrl+F, in
/// `open_media_library_window`) needing to know Playlists has that extra
/// layer. Skips invisible subtrees so the Discs/Devices overview — a plain
/// `Box` toggle, not a `Stack`, hiding their detail view's search box — never
/// matches a box the user cannot currently see.
fn find_visible_search_entry(root: &gtk4::Widget) -> Option<Entry> {
    if !root.is_visible() {
        return None;
    }
    if let Some(entry) = root.downcast_ref::<Entry>()
        && entry.widget_name() == ML_SEARCH_ENTRY_NAME
    {
        return Some(entry.clone());
    }
    if let Some(stack) = root.downcast_ref::<Stack>() {
        return stack.visible_child().and_then(|c| find_visible_search_entry(&c));
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        if let Some(found) = find_visible_search_entry(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

pub(super) fn open_media_library_window(
    parent: Option<&gtk4::Window>,
    host: MlHost,
    init_width: i32,
    init_height: i32,
) -> gtk4::Window {
    // Step 1 aliased all eight of MlHost's fields here so the body could keep
    // its original names while the pages were carved out one at a time; each
    // extraction dropped the alias it stopped using. With Playlists gone
    // (step 7) only `state` is left — this function no longer touches the
    // player state the pages share, it just builds the chrome and hands
    // `host` to `MlCtx`.
    let state = host.state.clone();

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
    // Same story as the MlHost aliases above: what the pages needed travelled
    // with them, and each takes `&Sidebar` directly. Only the two the window
    // itself uses are left — the list for the initial selection, and the
    // scroller it parents into the Paned.
    let sidebar = sb.list.clone();
    let sidebar_scroll = sb.scroll.clone();


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
    // `rebuild_gallery` exists (albums.rs's `show_gallery_overview`).
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
    files::build(&ctx, &sb);

    // Every field this needs now exists, so the page context can be built.
    // `host` is moved in — the eight aliases above were cloned off it at the
    // top, so nothing below depends on it by that name any more.
    // ── Page: Albums (Phase 11 A5 — gallery grid, Task 4) ──────────────────
    // Extracted to `window/albums.rs` (plan step 2). Adds itself to the stack
    // and registers its own sidebar routing, so it hands nothing back.
    albums::build(&ctx, &sb);

    // ── Page: Playlists ──────────────────────────────────────────────────
    // Extracted to `window/playlists.rs` (plan step 7). Builds both sub-pages
    // — the saved-playlist manager and the track editor — adds itself to the
    // stack, and registers its own sidebar routing. `sb` goes along for the
    // playlist sub-rows, the chevron state and the send-a-playlist holder.
    playlists::build(&ctx, &sb);


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

    // Ctrl+F → focus the search entry for whichever page is visible. The
    // window has no single search box — every stack page (and, inside
    // Playlists, its own Manage/Edit sub-stack) owns its own — so this walks
    // the widget tree from the stack's current child rather than assuming
    // the Files page. Capture phase so it fires even when a child widget
    // (e.g. a ColumnView row) holds keyboard focus.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let stack_kf = stack.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, modifier| {
            if modifier.contains(gdk::ModifierType::CONTROL_MASK)
                && matches!(key, gdk::Key::f | gdk::Key::F)
            {
                // The ML search box otherwise has to be clicked — Ctrl+F is
                // the reflex, and every view here has a search entry.
                if let Some(entry) =
                    stack_kf.visible_child().and_then(|c| find_visible_search_entry(&c))
                {
                    entry.grab_focus();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        win.add_controller(key_ctrl);
    }

    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    let init_sidebar_width = state.borrow().config.window.ml_sidebar_width;
    paned.set_start_child(Some(&sidebar_scroll));
    paned.set_end_child(Some(&stack));
    paned.set_position(init_sidebar_width);
    // Every toast in this window lands here. Wrapping the root once means
    // call sites only need the window, not a threaded-through overlay.
    let toaster = adw::ToastOverlay::new();
    toaster.set_child(Some(&paned));
    win.set_child(Some(&toaster));

    win.connect_close_request({
        let state = state.clone();
        // The window persists this, not the Playlists page — the page only
        // sets it while navigating.
        let playlists_expanded = sb.playlists_expanded.clone();
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
            }
            let _ = state.borrow().config.save();
            // The window is kept, not dropped. `set_hide_on_close(true)` in
            // player.rs means closing only hides it, so clearing
            // `state.ml_window` here did not free anything — it only threw
            // away the handle the toolbar button reuses. GTK still owned the
            // hidden toplevel, so every reopen built a second window on top
            // of a first that could never be reached again: ~126 MB and a
            // fresh pair of 2 s pollers per close/open cycle, measured
            // (2026-08-11).
            //
            // Holding the handle makes a reopen a `present()` instead of a
            // rebuild, so `rebuild_ml_callback` and the editor-refresh hooks
            // stay live too — they belong to a window that is still there and
            // will be shown again.
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
pub(super) fn analyze_job(
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

#[cfg(test)]
mod find_visible_search_entry_tests {
    use super::*;

    /// Builds a fresh `gtk4::Box` with an `Entry` child in each branch of a
    /// `Stack`, so a test can drive `find_visible_search_entry` without the
    /// full window.
    ///
    /// `#[gtk4::test]` (not plain `#[test]`) is required: GTK is not
    /// thread-safe, and `cargo test` runs each `#[test]` on its own OS
    /// thread. `#[gtk4::test]` (from `gtk4-macros`, re-exported by `gtk4`)
    /// routes every GTK test through one dedicated worker thread it owns
    /// instead (`gtk4::test_synced`), which is what makes constructing real
    /// widgets here safe alongside the rest of the (non-GTK) suite. A plain
    /// `#[test]` calling `gtk4::init()` was tried first and passed in
    /// isolation, but broke the moment it ran in the same binary as a
    /// `#[gtk4::test]` — the two raced for the one process-wide GTK main
    /// context, and whichever lost panicked with "Failed to acquire default
    /// main context" or "GTK may only be used from the main thread." See the
    /// fix report for the full transcript.
    fn tagged_entry() -> Entry {
        let e = Entry::new();
        e.set_widget_name(ML_SEARCH_ENTRY_NAME);
        e
    }

    #[gtk4::test]
    fn resolves_through_a_nested_stack_and_skips_hidden_subtrees() {
        // Outer page: an invisible decoy Entry ahead of the real content, to
        // prove the walk does not just grab the first Entry it sees.
        let root = GtkBox::new(Orientation::Vertical, 0);
        let decoy_holder = GtkBox::new(Orientation::Vertical, 0);
        let decoy = tagged_entry();
        decoy_holder.append(&decoy);
        decoy_holder.set_visible(false); // Discs/Devices-style hidden detail pane
        root.append(&decoy_holder);

        // A nested Stack (mirrors Playlists' Manage/Edit) with two tagged
        // entries — only the visible child's entry should be found.
        let inner_stack = Stack::new();
        let manage_page = GtkBox::new(Orientation::Vertical, 0);
        let manage_entry = tagged_entry();
        manage_entry.set_text("manage");
        manage_page.append(&manage_entry);
        inner_stack.add_named(&manage_page, Some("pl-manage"));

        let edit_page = GtkBox::new(Orientation::Vertical, 0);
        let edit_entry = tagged_entry();
        edit_entry.set_text("edit");
        edit_page.append(&edit_entry);
        inner_stack.add_named(&edit_page, Some("pl-edit"));

        inner_stack.set_visible_child_name("pl-edit");
        root.append(&inner_stack);

        let found = find_visible_search_entry(root.upcast_ref::<gtk4::Widget>())
            .expect("expected the edit-page entry to be found");
        assert_eq!(found.text(), "edit");

        // Switching the nested stack's visible child changes which entry the
        // walk resolves to, without touching anything else.
        inner_stack.set_visible_child_name("pl-manage");
        let found = find_visible_search_entry(root.upcast_ref::<gtk4::Widget>())
            .expect("expected the manage-page entry to be found");
        assert_eq!(found.text(), "manage");
    }

    #[gtk4::test]
    fn returns_none_when_nothing_visible_is_tagged() {
        let root = GtkBox::new(Orientation::Vertical, 0);
        let untagged = Entry::new(); // no widget name set
        root.append(&untagged);

        assert!(find_visible_search_entry(root.upcast_ref::<gtk4::Widget>()).is_none());
    }
}
