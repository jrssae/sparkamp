use super::*;

/// What the playlist window needs from the main window, which is built
/// first: the two `ApplicationWindow`s it lives beside, the box it fills,
/// and the handful of callbacks and channels born above it in `build`.
///
/// A struct rather than ten positional parameters, for the same reason
/// `MlHost` is one — the list is long, the entries are same-typed, and a
/// transposed pair of `ApplicationWindow`s would compile.
pub(super) struct Deps {
    pub(super) state: Rc<RefCell<AppState>>,
    pub(super) window: ApplicationWindow,
    pub(super) playlist_win: ApplicationWindow,
    pub(super) pl_root: GtkBox,
    pub(super) logo_img: Image,
    pub(super) set_track: Rc<dyn Fn(&str)>,
    pub(super) last_np_key: Rc<RefCell<Option<String>>>,
    pub(super) current_track_meta_tx:
        std::sync::mpsc::Sender<(PathBuf, String, String, String, String)>,
    pub(super) provider_for_settings: Rc<gtk4::CssProvider>,
    pub(super) initial_vars: SkinVars,
}

/// What the playlist window hands back. Everything here is read by the
/// rest of `player::build`, by [`PlayerCtx`], or by both.
///
/// `build` destructures this straight into locals of the same names, so
/// every line below the call site reads exactly as it did when all of this
/// was one function — the bundle is a transport, not a new API to learn.
pub(super) struct PlaylistWin {
    pub(super) pl_view: TreeView,
    pub(super) pl_scroll: ScrolledWindow,
    pub(super) pl_status_label: Label,
    pub(super) pl_selected_idx: Rc<Cell<usize>>,
    pub(super) accent_rgba: Rc<RefCell<Option<gdk::RGBA>>>,
    pub(super) btn_save_active: Button,
    pub(super) btn_add_files: Button,
    pub(super) btn_add_dir: Button,
    pub(super) btn_remove: Button,
    pub(super) btn_clear_all: Button,
    pub(super) btn_cancel: Button,
    pub(super) rebuild_playlist: Rc<dyn Fn()>,
    pub(super) patch_pl_row: Rc<dyn Fn(usize)>,
    pub(super) scroll_to_row_if_needed: Rc<dyn Fn(usize)>,
    pub(super) play_and_update: Rc<dyn Fn()>,
    pub(super) refresh_now_playing: Rc<dyn Fn()>,
    pub(super) refresh_pl_status: Rc<dyn Fn()>,
    pub(super) remove_selected: Rc<dyn Fn()>,
    pub(super) queue_toggle_selection: Rc<dyn Fn()>,
    pub(super) invert_selection: Rc<dyn Fn()>,
    pub(super) open_settings: Rc<dyn Fn()>,
}

/// Build the playlist window: its header, button bar, `TreeView` and store,
/// the row-render and selection callbacks, the status bar, and the
/// Winamp-style menu bar (Add ▸ / Select ▸ / Sort ▸ / List ▸).
///
/// Split out of `player::build` (breakup step 9). It is a window of its own
/// with its own widget tree, which is what made it a natural unit even
/// though it hands more back than the other two cuts did.
/// The text of a playlist row's position column: its 1-based number, plus the
/// lock marker when the file cannot be written.
///
/// Shared by the full rebuild and the single-row patch. They used to compose
/// this separately and had drifted: only the rebuild appended the lock, so a
/// row whose read-only status was discovered by the background pass — which
/// repaints through the patch — never showed it. The file was locked, the ID3
/// editor said so, and the playlist did not.
fn row_position_text(index: usize, read_only: bool) -> String {
    format!("{}.{}", index + 1, if read_only { " 🔒" } else { "" })
}

/// The text of a playlist row's name column: queue badge, state marker, and
/// the track's display name.
///
/// Shared by the rebuild and the patch for the same reason as
/// [`row_position_text`] — two copies of this is how the lock marker went
/// missing in the first place.
fn row_display_text(
    track: &sparkamp::model::Track,
    queue: &sparkamp::queue::Queue,
    is_active: bool,
) -> String {
    let badge = queue.badge(track.id);
    let name = track.display_name();
    if track.broken {
        format!("{badge}⚠ {name}")
    } else if is_active {
        format!("{badge}▶ {name}")
    } else {
        format!("{badge}{name}")
    }
}

/// Playlist menu bar button labels with mnemonics, in order: Add, Select, Sort,
/// List. Access keys are deconflicted within the menu bar: A, S, O, L. Sort uses
/// O instead of S to avoid collision with Select.
pub(super) const PLAYLIST_MENU_LABELS: [&str; 4] = [
    "_Add ▾",
    "_Select ▾",
    "S_ort ▾",
    "_List ▾",
];

/// Build a Winamp-style menu button: a labelled MenuButton whose popover is
/// a vertical list of action buttons. `items` are (label, Some(callback))
/// for an action row or (label, None) for a separator. Each action button
/// closes the popover after running its callback, so it behaves like a
/// real menu instead of a panel that stays open.
pub(super) fn menu_button(label: &str, items: Vec<(&str, Option<Rc<dyn Fn()>>)>) -> gtk4::MenuButton {
    let vbox = GtkBox::new(Orientation::Vertical, 2);
    let popover = gtk4::Popover::new();
    for (text, cb) in items {
        match cb {
            None => {
                vbox.append(&Separator::new(Orientation::Horizontal));
            }
            Some(cb) => {
                let b = Button::with_label(text);
                b.add_css_class("flat");
                b.set_halign(Align::Fill);
                let pop = popover.clone();
                b.connect_clicked(move |_| {
                    cb();
                    pop.popdown();
                });
                vbox.append(&b);
            }
        }
    }
    popover.set_child(Some(&vbox));
    let mb = gtk4::MenuButton::new();
    // use_underline makes `_A` an Alt+A access key. GTK shows the
    // underline only while Alt is held, so the menu bar looks unchanged.
    mb.set_use_underline(true);
    mb.set_label(label);
    mb.set_popover(Some(&popover));
    mb
}

pub(super) fn build(d: Deps) -> PlaylistWin {
    // Aliased under their original names so the moved body is unchanged.
    let state = d.state.clone();
    let window = d.window.clone();
    let playlist_win = d.playlist_win.clone();
    let pl_root = d.pl_root.clone();
    let logo_img = d.logo_img.clone();
    let set_track = d.set_track.clone();
    let last_np_key = d.last_np_key.clone();
    let current_track_meta_tx = d.current_track_meta_tx.clone();
    let provider_for_settings = d.provider_for_settings.clone();
    let initial_vars = d.initial_vars.clone();

    // ── Playlist window header: track count ───────────────────────────────────
    let pl_count_label = Label::builder()
        .label("Playlist — 0 tracks")
        .halign(Align::Start)
        .css_classes(["pl-count-label"])
        .margin_top(1)
        .build();
    pl_root.append(&pl_count_label);

    pl_root.append(&Separator::new(Orientation::Horizontal));

    // ── Playlist button bar: Add / Remove ─────────────────────────────────────
    let pl_btn_row = GtkBox::new(Orientation::Horizontal, 4);
    pl_btn_row.set_margin_start(8);
    pl_btn_row.set_margin_end(8);
    pl_btn_row.set_margin_top(4);
    pl_btn_row.set_margin_bottom(4);

    // "+ Files" opens a multi-select dialog — selecting one file also works,
    // making a separate single-file button redundant.
    let btn_add_files = Button::with_label("+ Files"); // one or more audio files
    let btn_add_dir = Button::with_label("+ Folder"); // directory (recursive scan)
    // Save the entire active playlist to an M3U8 file via the native
    // Save dialog.  Mirrors the macOS frontend's Save button.
    let btn_save_active = Button::with_label("⤓ Save");
    btn_save_active.add_css_class("pl-btn");
    btn_save_active.set_tooltip_text(Some("Save active playlist to an M3U8 file"));
    let btn_remove = Button::with_label("✕ Remove"); // remove selected row(s)
    let btn_clear_all = Button::with_label("✕ All"); // clear entire playlist
    let btn_cancel = Button::with_label("✕ Cancel Scan");
    btn_cancel.add_css_class("pl-btn");
    btn_cancel.add_css_class("destructive");
    btn_cancel.set_visible(false);

    for btn in [&btn_add_files, &btn_add_dir] {
        btn.add_css_class("pl-btn");
    }
    for btn in [&btn_remove, &btn_clear_all] {
        btn.add_css_class("pl-btn");
        btn.add_css_class("destructive");
    }

    // The flat buttons above stay constructed (their connect_clicked handlers
    // are wired further down, unchanged) but are no longer appended directly
    // to the row — they're invoked from the Winamp-style menus built below
    // instead. `btn_cancel` is the exception: it's appended on its own once
    // the menus are in place, since it toggles visibility during scans.

    // ── Playlist TreeView + ListStore ─────────────────────────────────────────
    // GtkTreeView uses virtual scrolling — only visible rows create cell renderers,
    // so 30k+ tracks render instantly without memory pressure.
    // Four-column ListStore: position | display name | duration | font weight.
    // Col 3 (i32): Pango weight — 700 for the active track, 400 for all others.
    // Col 4 (RGBA): Foreground color — accent for active, white for selected, grey for default.
    // Using attribute binding instead of cell_data_func for reliable color updates.
    #[allow(deprecated)]
    let pl_store = ListStore::new(&[
        String::static_type(),    // col 0: position ("1.", "2.", …)
        String::static_type(),    // col 1: display name ("Artist - Title" or filename)
        String::static_type(),    // col 2: duration ("-:--" or "3:45")
        i32::static_type(),       // col 3: Pango font weight (700 = active, 400 = normal)
        gdk::RGBA::static_type(), // col 4: foreground color
    ]);

    // Shared accent RGBA populated after main window realization by reading the
    // computed color of the hidden .np-title probe label.
    let accent_rgba: Rc<RefCell<Option<gdk::RGBA>>> = Rc::new(RefCell::new(None));

    // Playlist TreeView overrides cell foreground per-row via col 4; CSS alone
    // won't reach deprecated cell renderers. Keep an Rc-shared RGBA derived
    // from the active skin's text_color, updated whenever the skin changes.
    let text_rgba: Rc<RefCell<gdk::RGBA>> = Rc::new(RefCell::new(gdk::RGBA::new(
        initial_vars.text_color.r as f32 / 255.0,
        initial_vars.text_color.g as f32 / 255.0,
        initial_vars.text_color.b as f32 / 255.0,
        1.0,
    )));

    // Deferred rebuild_playlist handle — populated later when the closure is
    // defined. Lets the logo-click and other early-bound callbacks dispatch
    // to it even though construction happens further down.
    let rebuild_pl_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));

    // Shared "open settings window" action — used by the logo click and the
    // Ctrl+, keyboard shortcut (phase 6; GNOME-standard Settings binding, was
    // Ctrl+. before Task 4) so both go through one path.
    let open_settings: Rc<dyn Fn()> = {
        let state_rc = state.clone();
        let win_wk = window.downgrade();
        let provider_for_lclick = provider_for_settings.clone();
        let text_rgba_for_lclick = text_rgba.clone();
        let accent_rgba_for_lclick = accent_rgba.clone();
        let rebuild_pl_holder_lclick = rebuild_pl_holder.clone();
        Rc::new(move || {
            let parent_win = win_wk.upgrade();
            // Fall back to a no-op if rebuild_playlist hasn't been assigned
            // yet (should never happen post-init).
            let rebuild_pl: Rc<dyn Fn()> = rebuild_pl_holder_lclick
                .borrow()
                .clone()
                .unwrap_or_else(|| Rc::new(|| {}));
            open_settings_window(
                parent_win.as_ref().map(|w| w.upcast_ref()),
                state_rc.clone(),
                None,
                provider_for_lclick.clone(),
                text_rgba_for_lclick.clone(),
                accent_rgba_for_lclick.clone(),
                rebuild_pl,
            );
        })
    };

    // ── Left-click on the logo → open settings window ────────────────────────
    {
        let open_settings = open_settings.clone();
        let lclick = GestureClick::new();
        lclick.set_button(1); // primary button only
        lclick.connect_released(move |_, _, _, _| open_settings());
        logo_img.add_controller(lclick);
    }

    // Track the single-clicked row index (separate from the playing row).
    // usize::MAX means no row is selected.
    let pl_selected_idx: Rc<Cell<usize>> = Rc::new(Cell::new(usize::MAX));

    // Track the currently-playing row index (active row styling).
    // usize::MAX means no row is playing.
    let pl_active_idx: Rc<Cell<usize>> = Rc::new(Cell::new(usize::MAX));

    #[allow(deprecated)]
    let pl_view = TreeView::builder()
        .model(&pl_store)
        .headers_visible(false)
        .hexpand(true)
        .vexpand(true)
        .build();
    pl_view.add_css_class("playlist");
    // GtkTreeView (deprecated since 4.10) has no cell-level accessible
    // plumbing, so this widget-level name is the ceiling here. Full row
    // semantics need the ColumnView migration tracked as audit item 11 —
    // held back deliberately because the playlist's multi-select
    // drag-reorder is hard-won and deserves its own branch and its own tests.
    pl_view.update_property(&[gtk4::accessible::Property::Label("Playlist")]);
    #[allow(deprecated)]
    pl_view.selection().set_mode(gtk4::SelectionMode::Multiple);

    // Invert the playlist's multi-selection (Ctrl+I, phase 6). TreeSelection
    // has no "invert", so walk every row (bounded by the playlist length) and
    // flip each row's selected state.
    let invert_selection: Rc<dyn Fn()> = {
        let pl_view = pl_view.clone();
        let state = state.clone();
        Rc::new(move || {
            let sel = pl_view.selection();
            let n = state.borrow().playlist.tracks.len();
            for i in 0..n {
                let path = gtk4::TreePath::from_indices(&[i as i32]);
                if sel.path_is_selected(&path) {
                    sel.unselect_path(&path);
                } else {
                    sel.select_path(&path);
                }
            }
        })
    };

    // Position column — narrow, right-aligned, monospace.
    #[allow(deprecated)]
    let pos_col = TreeViewColumn::new();
    #[allow(deprecated)]
    let pos_cell = CellRendererText::new();
    pos_cell.set_xalign(1.0);
    #[allow(deprecated)]
    pos_col.pack_start(&pos_cell, false);
    #[allow(deprecated)]
    pos_col.add_attribute(&pos_cell, "text", 0);
    #[allow(deprecated)]
    pl_view.append_column(&pos_col);

    // Name column — expands to fill remaining width, ellipsizes long strings.
    // Using add_attribute for all properties (text, weight, foreground-rgba).
    // Foreground color is stored in column 4 and updated by patch_pl_row.
    #[allow(deprecated)]
    let name_col = TreeViewColumn::new();
    name_col.set_expand(true);
    #[allow(deprecated)]
    let name_cell = CellRendererText::new();
    name_cell.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    #[allow(deprecated)]
    name_col.pack_start(&name_cell, true);
    #[allow(deprecated)]
    name_col.add_attribute(&name_cell, "text", 1);
    #[allow(deprecated)]
    name_col.add_attribute(&name_cell, "weight", 3);
    #[allow(deprecated)]
    name_col.add_attribute(&name_cell, "foreground-rgba", 4);
    #[allow(deprecated)]
    pl_view.append_column(&name_col);

    // Duration column — fixed width, right-aligned, monospace.
    #[allow(deprecated)]
    let dur_col = TreeViewColumn::new();
    #[allow(deprecated)]
    let dur_cell = CellRendererText::new();
    dur_cell.set_xalign(1.0);
    #[allow(deprecated)]
    dur_col.pack_start(&dur_cell, false);
    #[allow(deprecated)]
    dur_col.add_attribute(&dur_cell, "text", 2);
    #[allow(deprecated)]
    pl_view.append_column(&dur_col);

    let pl_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .min_content_height(350)
        .child(&pl_view)
        .build();

    // This is the first thing a new user sees, so the empty page doubles as
    // onboarding rather than just saying what's missing.
    let pl_empty = super::util::empty_state(
        "view-list-symbolic",
        "No tracks in the playlist",
        Some("Press n to add files, or drag music here"),
    );
    let pl_stack = super::util::stack_with_empty_state(&pl_scroll, &pl_empty);
    pl_root.append(&pl_stack);

    // ── Playlist window status bar ────────────────────────────────────────────
    let pl_status_label = Label::builder()
        .label("")
        .halign(Align::Start)
        .css_classes(["status-label"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    pl_status_label.set_margin_start(8);
    pl_status_label.set_margin_end(8);
    pl_status_label.set_margin_top(1);
    pl_status_label.set_margin_bottom(5);
    pl_root.append(&pl_status_label);

    // Refresh the playlist status line: count · total · (selected when ≥1 row).
    let refresh_pl_status: Rc<dyn Fn()> = {
        let state = state.clone();
        let pl_status_label = pl_status_label.clone();
        let pl_view = pl_view.clone();
        Rc::new(move || {
            let (count, total) = {
                let s = state.borrow();
                let total: u64 = s.playlist.tracks.iter()
                    .map(|t| t.duration.map(|d| d.as_secs()).unwrap_or(0))
                    .sum();
                (s.playlist.tracks.len(), total)
            };
            // Selected duration — sum durations of selected TreeView rows.
            #[allow(deprecated)]
            let (sel_paths, _) = pl_view.selection().selected_rows();
            let selected = if sel_paths.is_empty() {
                None
            } else {
                let s = state.borrow();
                let sum: u64 = sel_paths.iter()
                    .filter_map(|p| p.indices().first().copied())
                    .filter_map(|i| s.playlist.tracks.get(i as usize))
                    .map(|t| t.duration.map(|d| d.as_secs()).unwrap_or(0))
                    .sum();
                Some((sel_paths.len(), sum))
            };
            pl_status_label.set_text(&sparkamp::playlist_status::playlist_status_line(count, total, selected));
        })
    };

    // ── Playlist button bar: Add / Remove (pinned to the bottom) ─────────────
    // Mirrors the layout of classic Winamp where the playlist action buttons
    // sit below the track list rather than above it.
    pl_root.append(&Separator::new(Orientation::Horizontal));
    pl_root.append(&pl_btn_row);

    // Every toast in this window lands here. Wrapping the root once means
    // call sites only need the window, not a threaded-through overlay.
    let toaster = adw::ToastOverlay::new();
    toaster.set_child(Some(&pl_root));
    playlist_win.set_child(Some(&toaster));

    // Closing the playlist window hides it (not destroys) so the next toggle
    // brings it back without rebuilding.  Save its size to both the in-memory
    // config (in state) and to disk so the main close handler and the next
    // launch both see the correct dimensions.
    playlist_win.connect_close_request({
        let state = state.clone();
        move |pw| {
            let (w, h) = (pw.width(), pw.height());
            // Update in-memory config so the main-window close handler reads
            // the correct size even after the playlist window is hidden
            // (a hidden GTK window reports width/height of 0).
            {
                let mut s = state.borrow_mut();
                s.config.window.playlist_width = w;
                s.config.window.playlist_height = h;
            }
            let _ = state.borrow().config.save();
            pw.set_visible(false);
            glib::Propagation::Stop
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Shared closures
    // ══════════════════════════════════════════════════════════════════════════

    // scan_viewport — ask the background pass about the rows now on screen.
    //
    // This is the whole of the file-reading policy. Winamp's classic playlist
    // editor keeps a `cached` bit per entry and a 100 ms timer that reads the
    // first unresolved *visible* row and stops (`Src/Winamp/Pledit.cpp`); rows
    // nobody scrolls to are never opened. Adding 36k tracks then costs one
    // screenful of I/O rather than the ~17 minutes a full walk would spend on
    // rows that will never be looked at.
    //
    // `visible_range` is asked rather than computed, so real rendered row
    // heights drive the answer — the same reason `scroll_to_row_if_needed`
    // below uses it.
    let scan_viewport: Rc<dyn Fn()> = {
        let state = state.clone();
        let pl_view = pl_view.clone();
        Rc::new(move || {
            // Detached mid-rebuild; the rebuild scans again once it reattaches.
            #[allow(deprecated)]
            if pl_view.model().is_none() {
                return;
            }
            #[allow(deprecated)]
            let Some((first, last)) = pl_view.visible_range() else {
                return;
            };
            let (Some(first), Some(last)) = (
                first.indices().first().copied(),
                last.indices().first().copied(),
            ) else {
                return;
            };
            let (first, last) = (first.max(0) as usize, last.max(0) as usize);
            // One page of margin either side, so an unhurried scroll meets rows
            // that are already finished instead of a wave of blanks at the edge.
            let page = last.saturating_sub(first) + 1;
            playlist_add::request_range(&state, first.saturating_sub(page), last + page);
        })
    };

    // Rescan whenever the set of visible rows can have changed, debounced.
    // Dragging the scrollbar emits a torrent of value-changed, and every scan
    // takes a `borrow_mut` of the playlist; at one scan per event a drag
    // through a 36k list would fight the UI for the state it is trying to draw.
    {
        let queued = Rc::new(Cell::new(false));
        let book_scan: Rc<dyn Fn()> = {
            let scan_viewport = scan_viewport.clone();
            let queued = queued.clone();
            Rc::new(move || {
                if queued.replace(true) {
                    return;
                }
                let scan = scan_viewport.clone();
                let queued = queued.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
                    queued.set(false);
                    scan();
                });
            })
        };
        let adj = pl_scroll.vadjustment();
        // Scrolling.
        adj.connect_value_changed({
            let book_scan = book_scan.clone();
            move |_| book_scan()
        });
        // Resizing the window taller uncovers rows without moving the scroll
        // position, so value-changed never fires for it; `changed` carries the
        // page-size and upper updates that do.
        adj.connect_changed(move |_| book_scan());
    }

    // rebuild_playlist — repopulate the ListStore from the current playlist model.
    //
    // The TreeView is temporarily disconnected from the model while the store is
    // cleared and repopulated.  This prevents the TreeView from processing one
    // row-deleted / row-inserted signal per track (which would block the UI for
    // several seconds on a 30k-track playlist).  Reconnecting the model triggers
    // a single bulk re-read; only visible rows are painted, so it remains O(1).
    let rebuild_playlist: Rc<dyn Fn()> = {
        let state = state.clone();
        let pl_store = pl_store.clone();
        let pl_view = pl_view.clone();
        let pl_count_label = pl_count_label.clone();
        let pl_active_idx = pl_active_idx.clone();
        let accent_rgba = accent_rgba.clone();
        let text_rgba = text_rgba.clone();
        let refresh_pl_status = refresh_pl_status.clone();
        let scan_viewport = scan_viewport.clone();
        let pl_stack = pl_stack.clone();
        Rc::new(move || {
            // Stamp any unstamped entries so queue badges have stable ids to
            // look up (idempotent; a no-op once every entry is stamped). Needs
            // its own mut borrow before the read-only borrow below.
            state.borrow_mut().playlist.ensure_ids();
            let s = state.borrow();
            let current = s.playlist.current_index;
            let is_playing = matches!(
                *s.player.state(),
                PlayerState::Playing | PlayerState::Paused
            );
            let n = s.playlist.tracks.len();
            // Update pl_active_idx to match current playing track.
            if is_playing {
                pl_active_idx.set(current);
            } else {
                pl_active_idx.set(usize::MAX);
            }
            // Remember the current scroll offset so a rebuild (e.g. enqueueing
            // files) repaints in place instead of jumping back to the top.
            let saved_scroll = pl_view.vadjustment().map(|a| a.value()).unwrap_or(0.0);
            // Detach TreeView so bulk model changes don't trigger per-row signals.
            #[allow(deprecated)]
            pl_view.set_model(None::<&ListStore>);
            #[allow(deprecated)]
            pl_store.clear();
            for (i, t) in s.playlist.tracks.iter().enumerate() {
                let is_active = is_playing && i == current;
                let pos = row_position_text(i, t.read_only);
                let display = row_display_text(t, &s.queue, is_active);
                let weight: i32 = if is_active { 700 } else { 400 };
                // Compute foreground color.  Active (playing) rows get the
                // skin's highlight/accent; everything else (including the
                // GTK-selected row) uses the skin's text color.
                let fg_rgba = if is_active {
                    accent_rgba
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| text_rgba.borrow().clone())
                } else {
                    text_rgba.borrow().clone()
                };
                #[allow(deprecated)]
                pl_store.insert_with_values(
                    None,
                    &[
                        (0, &gtk_safe(&pos) as &dyn ToValue),
                        (1, &gtk_safe(&display) as &dyn ToValue),
                        (2, &gtk_safe(&fmt_duration(t.duration)) as &dyn ToValue),
                        (3, &weight as &dyn ToValue),
                        (4, &fg_rgba as &dyn ToValue),
                    ],
                );
            }
            drop(s);
            // Reconnect — TreeView does one bulk re-read, only paints visible rows.
            #[allow(deprecated)]
            pl_view.set_model(Some(&pl_store));
            // Restore the scroll offset after layout settles (the adjustment's
            // upper bound only updates once the new rows are measured).
            if saved_scroll > 0.0 {
                if let Some(adj) = pl_view.vadjustment() {
                    glib::idle_add_local_once(move || {
                        let target = saved_scroll.min(adj.upper() - adj.page_size());
                        adj.set_value(target.max(0.0));
                    });
                }
            }
            pl_count_label.set_label(&format!(
                "Playlist — {} track{}",
                n,
                if n == 1 { "" } else { "s" },
            ));
            pl_stack.set_visible_child_name(if n == 0 { "empty" } else { "content" });
            refresh_pl_status();
            // On idle, not now: `visible_range` has nothing to report until GTK
            // has laid the reattached model out. The scroll restore above emits
            // value-changed, which books its own scan, but a rebuild that does
            // not move the viewport would otherwise never ask for anything.
            glib::idle_add_local_once({
                let scan_viewport = scan_viewport.clone();
                move || scan_viewport()
            });
        })
    };
    *rebuild_pl_holder.borrow_mut() = Some(rebuild_playlist.clone());

    // scroll_to_row_if_needed — scroll the playlist to make a row visible.
    //
    // Uses TreeView::visible_range + scroll_to_cell so that GTK's actual
    // rendered row heights drive the math rather than a hardcoded estimate.
    // A hardcoded estimate drifts after many skips and the row stops scrolling
    // into view.
    let scroll_to_row_if_needed: Rc<dyn Fn(usize)> = {
        let pl_scroll = pl_scroll.clone();
        let state    = state.clone();
        Rc::new(move |target_idx: usize| {
            let adj       = pl_scroll.vadjustment();
            let page_size = adj.page_size();
            let upper     = adj.upper();
            let current   = adj.value();
            let n         = state.borrow().playlist.len();

            if n == 0 || upper <= 0.0 || page_size <= 0.0 {
                return;
            }

            let row_h       = upper / n as f64;
            let row_top     = target_idx as f64 * row_h;
            let row_bottom  = row_top + row_h;
            let visible_end = current + page_size;

            if row_top < current || row_bottom > visible_end {
                let target = (row_top - page_size / 2.0 + row_h / 2.0)
                    .clamp(0.0, (upper - page_size).max(0.0));
                adj.set_value(target);
            }
        })
    };

    // patch_pl_row — update a single store row's text without a full rebuild.
    //
    // Called by the probe drain so name and duration updates appear row by row
    // as background probes complete.  O(1): finds the iter by position and
    // calls set() on just that row.
    let patch_pl_row: Rc<dyn Fn(usize)> = {
        let state = state.clone();
        let pl_store = pl_store.clone();
        let pl_active_idx = pl_active_idx.clone();
        let accent_rgba = accent_rgba.clone();
        let text_rgba = text_rgba.clone();
        Rc::new(move |idx: usize| {
            let (display, duration_str, weight, is_active, pos) = {
                let s = state.borrow();
                let Some(t) = s.playlist.tracks.get(idx) else {
                    return;
                };
                let is_playing = matches!(
                    *s.player.state(),
                    PlayerState::Playing | PlayerState::Paused
                );
                let is_active = is_playing && idx == s.playlist.current_index;
                let display = row_display_text(t, &s.queue, is_active);
                let weight: i32 = if is_active { 700 } else { 400 };
                // The position column carries the lock marker, so a patch has
                // to rewrite it too — the background status pass repaints
                // through here, and that is where read-only is discovered.
                let pos = row_position_text(idx, t.read_only);
                (display, fmt_duration(t.duration), weight, is_active, pos)
            };
            #[allow(deprecated)]
            let Some(iter) = pl_store.iter_nth_child(None, idx as i32) else {
                return;
            };
            // Update pl_active_idx state.
            let current_active = pl_active_idx.get();
            if is_active && current_active != idx {
                pl_active_idx.set(idx);
            } else if !is_active && current_active == idx {
                pl_active_idx.set(usize::MAX);
            }
            // Compute foreground color: active row → accent, all others → skin text.
            let fg_rgba = {
                let active_idx = pl_active_idx.get();
                let is_row_active = active_idx != usize::MAX && active_idx == idx;
                if is_row_active {
                    accent_rgba
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| text_rgba.borrow().clone())
                } else {
                    text_rgba.borrow().clone()
                }
            };
            // Update position (with its lock marker), name, duration, weight,
            // and foreground color columns.
            #[allow(deprecated)]
            pl_store.set(
                &iter,
                &[
                    (0, &gtk_safe(&pos) as &dyn ToValue),
                    (1, &gtk_safe(&display) as &dyn ToValue),
                    (2, &gtk_safe(&duration_str) as &dyn ToValue),
                    (3, &weight as &dyn ToValue),
                    (4, &fg_rgba as &dyn ToValue),
                ],
            );
        })
    };

    // Handle single-click row selection changes for highlighting.
    // Updates pl_selected_idx and repaints old/new selected rows.
    {
        let pl_selected_idx = pl_selected_idx.clone();
        let patch_pl_row = patch_pl_row.clone();
        let pl_view = pl_view.clone();
        #[allow(deprecated)]
        pl_view.selection().connect_changed(move |selection| {
            // Guard against model being detached (e.g., during rebuild_playlist).
            #[allow(deprecated)]
            if pl_view.model().is_none() {
                return;
            }
            // Guard against initial model setup (count is 0 when model is initializing).
            #[allow(deprecated)]
            if selection.count_selected_rows() == 0 && pl_selected_idx.get() == usize::MAX {
                return;
            }
            let old_idx = pl_selected_idx.get();
            #[allow(deprecated)]
            let (paths, _model): (Vec<_>, _) = selection.selected_rows();
            let new_idx = paths
                .into_iter()
                .next()
                .and_then(|p| p.indices().first().copied())
                .map(|i| i as usize)
                .unwrap_or(usize::MAX);
            if old_idx != new_idx {
                pl_selected_idx.set(new_idx);
                // Repaint old and new selected rows.
                if old_idx != usize::MAX {
                    patch_pl_row(old_idx);
                }
                if new_idx != usize::MAX {
                    patch_pl_row(new_idx);
                }
            }
        });
    }

    // scan_current_track_metadata — if the current track has no metadata (empty
    // artist AND album_artist), spawn a background thread to read the ID3 tags
    // and send the result via current_track_meta_tx so the marquee can be updated.

    // rescan_library_row_on_play — refresh the playing file's LIBRARY row.
    //
    // Playing is the moment the user is most likely to look at a track's
    // data (ID3 window, ML columns), so an unscanned library row is scanned
    // and an already-scanned one re-read — files edited outside Sparkamp
    // stay current without waiting for a folder rescan. Non-library files
    // are skipped (rescan_track errors on unknown paths; the ID3 window
    // probes those directly instead).
    //
    // Runs on its own thread with its own DB connection (SQLite is not
    // Send). The main loop swaps in a fresh handle when done; the full ML
    // rebuild only fires when a previously-UNSCANNED row gained data —
    // rebuilding 36k rows on every ordinary track advance would reset the
    // user's scroll position for no visible change.
    fn rescan_library_row_on_play(state: &Rc<RefCell<AppState>>) {
        let path = match state.borrow().playlist.current() {
            Some(t) => t.path.to_string_lossy().into_owned(),
            None => return,
        };
        let (result_tx, result_rx) = std::sync::mpsc::channel::<bool>();
        std::thread::spawn(move || {
            let db_path = sparkamp::media_library::MediaLibrary::db_path_pub();
            let Ok(lib) = sparkamp::media_library::MediaLibrary::open_at(&db_path) else {
                return;
            };
            let was_unscanned = match lib.track_by_path(&path) {
                Ok(row) => row.last_scanned.is_none(),
                Err(_) => return, // not a library file — nothing to refresh
            };
            if lib.rescan_track(&path).is_ok() {
                let _ = result_tx.send(was_unscanned);
            }
        });
        let result_rx = std::cell::RefCell::new(result_rx);
        let state_for_timer = state.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
            match result_rx.borrow().try_recv() {
                Ok(was_unscanned) => {
                    let mut s = state_for_timer.borrow_mut();
                    s.media_lib = sparkamp::media_library::MediaLibrary::open().ok();
                    let rebuild = if was_unscanned {
                        s.rebuild_ml_callback.clone()
                    } else {
                        None
                    };
                    drop(s);
                    if let Some(cb) = rebuild {
                        cb();
                    }
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
            }
        });
    }

    // refresh_now_playing — rebuild the now-playing info for whatever track is
    // CURRENT and fan it out to every subscriber (A1 panel, A6 art window,
    // future MPRIS). Not called explicitly from play_and_update / Next / Prev
    // / z / b any more: the tick loop's now-playing choke point (see
    // `last_np_key` below) calls this on every tick where the current track's
    // path changed, which covers those paths AND the ~17 Media Library /
    // device play_current() call sites that never fanned this out at all —
    // one choke point instead of one call site per play path, current and
    // future. ≤1 tick (~33 ms) of latency on art vs. the old immediate call
    // is not perceptible and matches how the marquee itself is driven.
    let refresh_now_playing: Rc<dyn Fn()> = {
        let state = state.clone();
        let last_np_key = last_np_key.clone();
        Rc::new(move || {
            // Build the info + clone the subscriber list under one short borrow,
            // drop it, THEN invoke callbacks (subscribers may re-borrow state).
            let (info, subs, key) = {
                let mut s = state.borrow_mut();
                let path_str = s
                    .playlist
                    .current()
                    .map(|t| t.path.to_string_lossy().into_owned());
                match path_str {
                    Some(p) => {
                        let snap = s
                            .media_lib
                            .as_ref()
                            .map(|ml| ml.play_snapshot(&p))
                            .unwrap_or_default();
                        let lib_row =
                            s.media_lib.as_ref().and_then(|ml| ml.track_by_path(&p).ok());
                        let info = sparkamp::now_playing::build_now_playing_info(
                            std::path::Path::new(&p),
                            lib_row.as_ref(),
                            snap,
                        );
                        s.current_now_playing = Some(info.clone());
                        let subs = s.now_playing_subscribers.clone();
                        (Some(info), subs, Some(p))
                    }
                    None => (None, Vec::new(), None),
                }
            };
            // Mark the track we just refreshed so the tick-loop choke point
            // doesn't fire a redundant second time for the same track. Explicit
            // callers (play/next/prev/z/b) thus get an immediate refresh AND a
            // fresh pre-play snapshot even when they REPLAY the same path
            // (Repeat-Song loop, Prev-restart ≥5s) where the path is unchanged.
            *last_np_key.borrow_mut() = key;
            if let Some(info) = info {
                for cb in &subs {
                    cb(&info);
                }
            }
        })
    };

    // play_and_update — play the current track and refresh the UI labels.
    //
    // All "start playing" paths (buttons, keyboard, auto-advance) funnel
    // through here so the marquee and playlist stay in sync.  Label text is
    // NOT set directly here; the tick loop renders the marquee window each
    // frame so the scrolling starts immediately after track change.
    //
    // The A1 panel / A6 art-window fan-out fires explicitly here (and from the
    // other button/key play paths) for an immediate, snapshot-fresh refresh —
    // including REPLAYS of the same track (Repeat-Song, play-from-stopped),
    // where the tick loop's path-keyed choke point alone would not re-fire.
    // The tick loop remains the catch-all for the ~17 Media-Library / device
    // paths that call `play_current()` directly and have no explicit call.
    let play_and_update: Rc<dyn Fn()> = {
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
        let refresh_now_playing = refresh_now_playing.clone();
        Rc::new(move || {
            // Record which row was playing before so we can un-bold it.
            let old_idx = state.borrow().playlist.current_index;
            let result = { state.borrow_mut().play_current() };
            if let Some(display) = result {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                // Scan metadata for the current track if it hasn't been scanned yet.
                // This updates the marquee with "Artist - Title" once the scan completes.
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                // Refresh the library row for the playing file (scan-on-play).
                rescan_library_row_on_play(&state);
                // Scroll to make the new current track visible
                scroll_to_row_if_needed(new_idx);
                // Patch the new current track to ensure active styling is applied.
                // Also patch old track if it was different.
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
                // Refresh ReplayGain album-mode for this track (Automatic
                // source tracks the shuffle state; a live property, no rebuild).
                state.borrow_mut().apply_rg_album_mode();
                // Follow the song on the art panel / window (immediate + covers
                // same-path replay, which the tick-loop choke point would skip).
                refresh_now_playing();
            }
            // Re-confirm the row's own ⚠ / 🔒 markers, which are otherwise
            // settled once per row per session.
            //
            // Here rather than only at the tick's now-playing choke point:
            // that one is keyed on the path, so it skips a REPLAY of the
            // current track — which is exactly when someone who has just
            // changed a file's permissions presses play to see the marker
            // catch up. `rescan_library_row_on_play` above is the same idea
            // for the library's own row, and is why the Media Library view
            // kept up while the playlist did not.
            //
            // Outside the success branch deliberately: a play that FAILED is
            // the strongest hint a file has gone, and that is when ⚠ matters
            // most. The index is re-read rather than reusing `old_idx`, since
            // a failed load can leave the current row somewhere else.
            let idx_now = state.borrow().playlist.current_index;
            playlist_add::request_row(&state, idx_now);
        })
    };

    // Store play/rebuild callbacks in AppState so secondary windows (dedupe,
    // etc.) can trigger playlist updates without needing direct closure refs.
    {
        let mut s = state.borrow_mut();
        s.rebuild_pl_callback = Some(rebuild_playlist.clone());
        s.play_and_update_callback = Some(play_and_update.clone());
        s.set_track_callback = Some(set_track.clone());
    }

    // remove_selected — remove every currently selected playlist row.
    //
    // Indices are sorted highest-first before removal so that earlier removes
    // do not shift the positions of later ones.  Does not delete files from
    // disk; only removes the entries from the in-memory playlist.
    let remove_selected: Rc<dyn Fn()> = {
        let state = state.clone();
        let pl_view = pl_view.clone();
        let pl_scroll = pl_scroll.clone();
        let rebuild_rm = rebuild_playlist.clone();
        let set_track_rm = set_track.clone();
        Rc::new(move || {
            #[allow(deprecated)]
            let (paths, _) = pl_view.selection().selected_rows();
            let mut indices: Vec<usize> = paths
                .iter()
                .filter_map(|p| p.indices().first().copied())
                .map(|i| i as usize)
                .collect();
            if indices.is_empty() {
                return;
            }
            // Highest first so earlier removes don't invalidate later indices.
            indices.sort_unstable_by(|a, b| b.cmp(a));
            let mut last_nowplaying: Option<String> = None;
            for idx in indices {
                if let Some(display) = { state.borrow_mut().remove_track(idx) } {
                    last_nowplaying = Some(display);
                }
            }
            if let Some(display) = last_nowplaying {
                set_track_rm(&display);
            }
            // Drop any now-removed entries from the play queue.
            state.borrow_mut().sync_queue_to_playlist();
            // Save and restore the scroll position around the rebuild so the
            // visible region doesn't jump after a removal.
            let adj = pl_scroll.vadjustment();
            let saved_scroll = adj.value();
            rebuild_rm();
            // The model re-attach resets the scroll; restore on next idle tick
            // after GTK has committed the new layout.
            glib::idle_add_local_once(move || {
                adj.set_value(saved_scroll);
            });
        })
    };

    // Toggle the play-queue membership of every selected playlist row, then
    // repaint the badges. Shared by the context-menu action and Ctrl+Q on the
    // playlist window. Uses in-place row patches (pl_store.set) rather than the
    // model-swap rebuild: swapping the model from the playlist window's own key
    // handler doesn't repaint until a later frame, which made the badge lag.
    // A toggle renumbers every queued row, so patch the whole list.
    let queue_toggle_selection: Rc<dyn Fn()> = {
        let state = state.clone();
        let pl_view = pl_view.clone();
        let patch_row = patch_pl_row.clone();
        Rc::new(move || {
            #[allow(deprecated)]
            let (sel_paths, _) = pl_view.selection().selected_rows();
            let indices: Vec<usize> = sel_paths
                .iter()
                .filter_map(|p| p.indices().first().copied())
                .map(|i| i as usize)
                .collect();
            if indices.is_empty() {
                return;
            }
            let n = {
                let mut s = state.borrow_mut();
                s.playlist.ensure_ids();
                let ids: Vec<u64> = indices
                    .iter()
                    .filter_map(|i| s.playlist.tracks.get(*i).map(|t| t.id))
                    .collect();
                for id in ids {
                    s.queue.toggle(id);
                }
                s.playlist.tracks.len()
            };
            for i in 0..n {
                patch_row(i);
            }
            refresh_queue_manager();
        })
    };

    // ── Winamp-style playlist menu bar (Add▸ / Select▸ / Sort▸ / List▸) ──────
    //
    // Run a reorder op, then rebuild the playlist view (which also repaints
    // queue badges — see rebuild_playlist above). The op mutates AppState
    // (which resets shuffle history); the playing track stays current by id,
    // so playback continues and its highlight follows.
    let apply_reorder: Rc<dyn Fn(&dyn Fn(&mut AppState))> = {
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        Rc::new(move |op: &dyn Fn(&mut AppState)| {
            {
                let mut s = state.borrow_mut();
                op(&mut s);
            }
            rebuild_playlist();
        })
    };

    // Add▸
    let add_menu = menu_button(
        PLAYLIST_MENU_LABELS[0],
        vec![
            (
                "Add Files…",
                Some({
                    let b = btn_add_files.clone();
                    Rc::new(move || b.emit_clicked()) as Rc<dyn Fn()>
                }),
            ),
            (
                "Add Folder…",
                Some({
                    let b = btn_add_dir.clone();
                    Rc::new(move || b.emit_clicked()) as Rc<dyn Fn()>
                }),
            ),
        ],
    );
    // Select▸
    let select_menu = menu_button(
        PLAYLIST_MENU_LABELS[1],
        vec![
            (
                "Select All",
                Some({
                    let pv = pl_view.clone();
                    let cb: Rc<dyn Fn()> = Rc::new(move || {
                        #[allow(deprecated)]
                        pv.selection().select_all();
                    });
                    cb
                }),
            ),
            (
                "Select None",
                Some({
                    let pv = pl_view.clone();
                    let cb: Rc<dyn Fn()> = Rc::new(move || {
                        #[allow(deprecated)]
                        pv.selection().unselect_all();
                    });
                    cb
                }),
            ),
            (
                "Invert Selection",
                Some({
                    let inv = invert_selection.clone();
                    Rc::new(move || inv()) as Rc<dyn Fn()>
                }),
            ),
        ],
    );
    // Sort▸
    let sort_item = |label: &'static str, key: sparkamp::model::SortKey| {
        let ar = apply_reorder.clone();
        (
            label,
            Some(Rc::new(move || ar(&move |s: &mut AppState| s.sort_playlist(key))) as Rc<dyn Fn()>),
        )
    };
    let sort_menu = menu_button(
        PLAYLIST_MENU_LABELS[2],
        vec![
            sort_item("Title", sparkamp::model::SortKey::Title),
            sort_item("Artist", sparkamp::model::SortKey::Artist),
            sort_item("Album", sparkamp::model::SortKey::Album),
            sort_item("Filename", sparkamp::model::SortKey::Filename),
            sort_item("Path", sparkamp::model::SortKey::Path),
            ("", None),
            (
                "Randomize",
                Some({
                    let ar = apply_reorder.clone();
                    Rc::new(move || ar(&|s: &mut AppState| s.randomize_playlist())) as Rc<dyn Fn()>
                }),
            ),
            (
                "Reverse",
                Some({
                    let ar = apply_reorder.clone();
                    Rc::new(move || ar(&|s: &mut AppState| s.reverse_playlist())) as Rc<dyn Fn()>
                }),
            ),
        ],
    );
    // List▸
    let list_menu = menu_button(
        PLAYLIST_MENU_LABELS[3],
        vec![
            (
                "Save Playlist…",
                Some({
                    let b = btn_save_active.clone();
                    Rc::new(move || b.emit_clicked()) as Rc<dyn Fn()>
                }),
            ),
            ("", None),
            (
                "Remove Selected",
                Some({
                    let rs = remove_selected.clone();
                    Rc::new(move || rs()) as Rc<dyn Fn()>
                }),
            ),
            (
                "Remove All",
                Some({
                    let b = btn_clear_all.clone();
                    Rc::new(move || b.emit_clicked()) as Rc<dyn Fn()>
                }),
            ),
        ],
    );

    pl_btn_row.append(&add_menu);
    pl_btn_row.append(&select_menu);
    pl_btn_row.append(&sort_menu);
    let pl_menu_spacer = GtkBox::new(Orientation::Horizontal, 0);
    pl_menu_spacer.set_hexpand(true);
    pl_btn_row.append(&pl_menu_spacer);
    pl_btn_row.append(&list_menu);
    pl_btn_row.append(&btn_cancel); // scan-cancel button, hidden unless a scan is running

    PlaylistWin {
        pl_view,
        pl_scroll,
        pl_status_label,
        pl_selected_idx,
        accent_rgba,
        btn_save_active,
        btn_add_files,
        btn_add_dir,
        btn_remove,
        btn_clear_all,
        btn_cancel,
        rebuild_playlist,
        patch_pl_row,
        scroll_to_row_if_needed,
        play_and_update,
        refresh_now_playing,
        refresh_pl_status,
        remove_selected,
        queue_toggle_selection,
        invert_selection,
        open_settings,
    }
}

#[cfg(test)]
mod row_text_tests {
    use super::*;
    use sparkamp::model::Track;

    fn track(id: u64, title: &str) -> Track {
        Track {
            path: std::path::PathBuf::from(format!("/m/{title}.mp3")),
            title: title.to_string(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only: false,
            id,
        }
    }

    /// The lock marker rides in the position column. The full rebuild appended
    /// it and the single-row patch did not, so a file whose read-only status
    /// was discovered by the background pass — which repaints through the
    /// patch — never showed it. One composer now serves both.
    #[test]
    fn a_read_only_row_carries_the_lock_marker() {
        assert_eq!(row_position_text(0, false), "1.");
        assert_eq!(row_position_text(0, true), "1. 🔒");
        assert_eq!(row_position_text(41, true), "42. 🔒");
    }

    /// A missing file gets the warning marker, matching the media library.
    #[test]
    fn a_broken_row_carries_the_warning_marker() {
        let mut t = track(1, "Gone");
        t.broken = true;
        let q = sparkamp::queue::Queue::new();
        assert_eq!(row_display_text(&t, &q, false), "⚠ Gone");
    }

    /// The playing row gets the play marker — unless it is also broken, where
    /// the warning wins, because a file that cannot be found is the more
    /// urgent fact.
    #[test]
    fn the_playing_row_carries_the_play_marker() {
        let t = track(1, "Now");
        let q = sparkamp::queue::Queue::new();
        assert_eq!(row_display_text(&t, &q, true), "▶ Now");

        let mut broken = track(2, "Gone");
        broken.broken = true;
        assert_eq!(row_display_text(&broken, &q, true), "⚠ Gone");
    }

    /// A queued row is prefixed with its 1-based queue position, and that
    /// badge survives alongside the state marker.
    #[test]
    fn a_queued_row_is_prefixed_with_its_queue_position() {
        let t = track(7, "Queued");
        let mut q = sparkamp::queue::Queue::new();
        q.enqueue(7);
        assert_eq!(row_display_text(&t, &q, false), "[1] Queued");
        assert_eq!(row_display_text(&t, &q, true), "[1] ▶ Queued");
    }
}

#[cfg(test)]
mod menu_button_mnemonic_tests {
    use super::*;

    /// Verify that the four playlist menu buttons are properly configured
    /// with use_underline and the correct labels. This test calls the actual
    /// menu_button() function and verifies the returned MenuButton widget.
    #[gtk4::test]
    fn menu_buttons_have_mnemonics_configured() {
        // Create all four menu buttons with their real labels from the const.
        let buttons: Vec<_> = PLAYLIST_MENU_LABELS
            .iter()
            .map(|label| menu_button(label, vec![]))
            .collect();

        // Verify each button has use_underline set and the correct label.
        for (idx, button) in buttons.iter().enumerate() {
            assert!(
                button.uses_underline(),
                "Menu button {} should have use_underline set",
                idx
            );
            assert_eq!(
                button.label().as_deref(),
                Some(PLAYLIST_MENU_LABELS[idx]),
                "Menu button {} should have the correct label",
                idx
            );
        }
    }

    /// Verify that PLAYLIST_MENU_LABELS defines all four mnemonics correctly.
    /// All labels must contain exactly one underscore, and the mnemonic
    /// characters (the ones following the underscores) must be distinct when
    /// lowercased to avoid collisions in the menu bar.
    #[test]
    fn playlist_menu_labels_are_deconflicted() {
        assert_eq!(
            PLAYLIST_MENU_LABELS.len(),
            4,
            "PLAYLIST_MENU_LABELS must have exactly 4 entries"
        );

        let mut mnemonic_chars = Vec::new();

        for (idx, label) in PLAYLIST_MENU_LABELS.iter().enumerate() {
            let underscores: Vec<usize> = label
                .chars()
                .enumerate()
                .filter(|(_, c)| *c == '_')
                .map(|(i, _)| i)
                .collect();

            assert_eq!(
                underscores.len(),
                1,
                "PLAYLIST_MENU_LABELS[{}] = {:?} must have exactly one underscore",
                idx,
                label
            );

            // The character immediately after the underscore is the mnemonic key.
            let underscore_idx = underscores[0];
            let mnemonic_char = label
                .chars()
                .nth(underscore_idx + 1)
                .expect("underscore must not be the last character");
            let mnemonic_lowercase = mnemonic_char.to_lowercase().to_string();

            // Check for duplicates.
            assert!(
                !mnemonic_chars.contains(&mnemonic_lowercase),
                "PLAYLIST_MENU_LABELS[{}] = {:?} has mnemonic '{}' which \
                 collides with an earlier menu (GTK lowercases all mnemonics)",
                idx,
                label,
                mnemonic_lowercase
            );
            mnemonic_chars.push(mnemonic_lowercase);
        }
    }
}
