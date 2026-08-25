use super::*;

/// Keyboard-shortcut reference shown in the help window (`i`). This is the
/// single source of truth for GTK bindings (phase 6): the help builder renders
/// it and a test asserts every bound key appears here, so the dialog can never
/// silently drift from what the handlers actually do. Keep entries in sync with
/// the mac `KeyboardShortcutsView.swift` sections.
#[allow(clippy::type_complexity)]
pub(super) fn shortcut_sections() -> &'static [(&'static str, &'static [(&'static str, &'static str)])] {
    &[
        ("Playback", &[
            ("z",          "Previous track / restart"),
            ("x",          "Play"),
            ("c",          "Pause / resume"),
            ("v",          "Stop"),
            ("Shift+V",    "Stop with fadeout (length in Settings)"),
            ("b",          "Next track"),
            ("t",          "Stop after current track"),
            ("← →",        "Seek −5 s / +5 s"),
            ("r",          "Cycle repeat (off / song / playlist)"),
            ("s",          "Toggle shuffle on/off"),
        ]),
        ("Volume", &[
            ("-",          "Volume down 5 %"),
            ("=",          "Volume up 5 %"),
            ("↑ ↓",        "Volume up / down (main window)"),
        ]),
        ("Playlist", &[
            ("n",          "Add file(s)"),
            ("Shift+N",    "Add folder"),
            ("m",          "Toggle Media Library window"),
            ("j",          "Jump / search"),
            ("Ctrl+F",     "Jump / search"),
            ("q",          "Play queue (Jump/Queue window, Queue mode)"),
            ("Ctrl+Q",     "Enqueue / dequeue selection (playlist or jump)"),
            ("↑ ↓",        "Browse up / down (playlist window)"),
            ("Enter",      "Play selected track"),
            ("Ctrl+S",     "Save playlist"),
            ("Ctrl+I",     "Invert selection"),
            ("Del",        "Remove highlighted track"),
            ("p",          "Toggle playlist window"),
        ]),
        ("View & Tags", &[
            ("a",           "Cycle visualizer mode (Bars / Waveform / Granite)"),
            ("e",           "Random Granite effect (Granite mode)"),
            ("f",           "Fullscreen visualizer (Waveform or Granite mode; Esc to exit)"),
            ("g",           "Toggle FPS / BPM overlay (fullscreen only)"),
            ("d",           "View/Edit ID3 tags for current track"),
            ("l",           "View/Search Lyrics (selected track, else the current one)"),
            ("u",           "Toggle equalizer window"),
            ("w",           "Toggle now-playing panel (art, tags, links)"),
            ("k",           "Open album-art window"),
            ("Ctrl+,",      "Open settings"),
            ("Click logo",  "Open settings"),
        ]),
        ("Mouse", &[
            ("Click time",   "Switch elapsed / remaining"),
            ("Click viz",    "Cycle visualizer mode"),
            ("Dbl-click viz", "Fullscreen visualizer (Waveform or Granite mode)"),
        ]),
        ("Other", &[
            ("i",          "Toggle this help"),
            ("F1",         "Toggle this help"),
            ("Ctrl+?",     "Toggle this help"),
            ("Esc",        "Quit (main window) / close child window"),
        ]),
    ]
}

/// Everything the main window and the playlist window both hang off, bundled
/// once so the behaviour carved out of `build` can be handed the same widgets
/// and callbacks the rest of the function still uses.
///
/// The test for a field is the same one `MlCtx` uses: is it touched from more
/// than one place now that those places are separate modules? A widget only
/// its own section reads stays a local in `build`.
///
/// It is assembled part-way down `build` — after the playlist window's rows,
/// closures and buttons exist, and before the first extracted module needs
/// them. Anything born later than that (the Jump window's entry, the volume
/// stepper) is passed to its consumer as a separate argument rather than
/// forcing the bundle further down the function.
///
/// Every field is an `Rc`, a `Sender` or a GObject, so cloning the bundle
/// into a module costs a refcount bump apiece and nothing else.
pub(super) struct PlayerCtx {
    pub(super) state: Rc<RefCell<AppState>>,
    pub(super) window: ApplicationWindow,
    pub(super) playlist_win: ApplicationWindow,
    pub(super) pl_view: TreeView,
    pub(super) pl_scroll: ScrolledWindow,
    pub(super) pl_status_label: Label,
    pub(super) seek_bar: Scale,
    pub(super) status_label: Label,
    pub(super) repeat_icon: Image,
    pub(super) repeat_label: Label,
    pub(super) btn_prev: Button,
    pub(super) btn_play: Button,
    pub(super) btn_pause: Button,
    pub(super) btn_stop: Button,
    pub(super) btn_next: Button,
    pub(super) btn_repeat: Button,
    pub(super) btn_shuffle: Button,
    pub(super) btn_pl: Button,
    pub(super) btn_ml: Button,
    pub(super) btn_eq: Button,
    pub(super) btn_info: Button,
    pub(super) btn_add_files: Button,
    pub(super) btn_add_dir: Button,
    pub(super) set_track: Rc<dyn Fn(&str)>,
    pub(super) rebuild_playlist: Rc<dyn Fn()>,
    pub(super) patch_pl_row: Rc<dyn Fn(usize)>,
    pub(super) scroll_to_row_if_needed: Rc<dyn Fn(usize)>,
    pub(super) play_and_update: Rc<dyn Fn()>,
    pub(super) refresh_now_playing: Rc<dyn Fn()>,
    pub(super) refresh_pl_status: Rc<dyn Fn()>,
    pub(super) remove_selected: Rc<dyn Fn()>,
    pub(super) queue_toggle_selection: Rc<dyn Fn()>,
    pub(super) toggle_np_panel: Rc<dyn Fn()>,
    pub(super) open_fullscreen_fn: RefreshHolder,
    pub(super) art_open: RefreshHolder,
    pub(super) current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>>,
    pub(super) current_devices: Rc<RefCell<Vec<crate::devices::Device>>>,
    pub(super) burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>>,
    pub(super) copy_files_holder: CopyFilesHolder,
    pub(super) burn_refresh_holder: RefreshHolder,
    pub(super) probe_tx: std::sync::mpsc::Sender<(PathBuf, Duration)>,
    pub(super) broken_tx: std::sync::mpsc::Sender<PathBuf>,
    pub(super) current_track_meta_tx:
        std::sync::mpsc::Sender<(PathBuf, String, String, String, String)>,
}

pub(super) fn scan_current_track_metadata(
    state: &Rc<RefCell<AppState>>,
    meta_tx: std::sync::mpsc::Sender<(PathBuf, String, String, String, String)>,
) {
    let (path, has_metadata) = {
        let s = state.borrow();
        match s.playlist.current() {
            Some(t) => {
                let has_meta = !t.artist.is_empty() || !t.album_artist.is_empty();
                (t.path.clone(), has_meta)
            }
            None => return,
        }
    };
    if has_metadata {
        return;
    }
    let path_for_thread = path.clone();
    std::thread::spawn(move || {
        if let Ok(track) = crate::model::Track::from_path(&path_for_thread) {
            let _ = meta_tx.send((
                track.path,
                track.title,
                track.artist,
                track.album_artist,
                track.album,
            ));
        }
    });
}

/// Build and present the Sparkamp main window and companion playlist window.
///
/// ## Layout overview
///
/// **Main window** (always visible):
/// ```text
/// [mini viz | title / artist]   ← now-playing row
/// [seek bar                  ]
/// [⏮ ▶ ⏸ ⏹ ⏭  VOL  PL     ]   ← transport + PL toggle
/// [status bar                ]
/// ```
///
/// **Playlist window** (shown/hidden with `p` or the PL button):
/// ```text
/// [Playlist — N tracks              ]
/// [+ File] [+ Files] [+ Folder] [✕ Remove]
/// [scrollable playlist ListBox      ]
/// [status bar                       ]
/// ```
///
/// ## Playlist window positioning / snap
///
/// GTK4 on Wayland does **not** allow applications to control window
/// positions programmatically — the compositor exclusively manages
/// placement.  We use `set_transient_for` to hint to the window manager
/// that the playlist window belongs with the main window; most WMs will
/// group them in the taskbar and may place the playlist near the main
/// window on first display.
///
/// On X11 / XWayland, position control is possible via platform-specific
/// GDK APIs (`gdk_x11_surface_get_xid` + `XMoveWindow`), but doing so
/// requires `unsafe` FFI and is not implemented here to keep the code
/// portable.  The Winamp-style "snap within 10–20 px" behaviour would
/// require that platform path.
///
/// In practice, with `set_transient_for` and a modern WM the windows
/// behave as a logical unit: they share the taskbar and are typically
/// raised/lowered together.
pub fn build(
    app: &Application,
    playlist: Playlist,
    config: Config,
    // Receives batches of file paths from the `open` GApplication signal so that
    // "Open with Sparkamp" in the file manager reaches the running instance
    // rather than spawning a new one.
    open_rx: std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>,
) {
    // ── CSS theme ─────────────────────────────────────────────────────────────
    // Load the active skin from config. Fall back to Dark if the named
    // skin cannot be resolved.
    let initial_vars = skin::load_skin(&config.appearance.active_skin)
        .map(|s| s.vars)
        .unwrap_or_else(SkinVars::dark_defaults);
    let initial_css = render_gtk_css(&initial_vars);

    let provider = Rc::new(gtk4::CssProvider::new());
    provider.load_from_data(&initial_css);
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("No display"),
        &*provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    // Use the dark Adwaita variant for built-in widgets whenever the
    // skin's window background is dark.
    let initial_dark = initial_vars.background.luminance() < 0.5;
    if let Some(gtk_settings) = gtk4::Settings::default() {
        gtk_settings.set_gtk_application_prefer_dark_theme(initial_dark);
    }

    // Cloned Rc references used by the Appearance tab handlers.
    let provider_for_settings = provider.clone();

    // ── AppState ──────────────────────────────────────────────────────────────
    let state = match AppState::new(playlist, config) {
        Ok(s) => Rc::new(RefCell::new(s)),
        Err(e) => {
            eprintln!("Failed to initialise GStreamer player: {e}");
            return;
        }
    };

    // ── Live folder watcher (Phase 8 Task 10) ────────────────────────────────
    // Must come after AppState (and its `media_lib`) exists. `rebuild_watcher`
    // itself checks `config.media_library.watch_folders` / folder list /
    // media_lib availability and degrades gracefully — see watch.rs.
    watch::rebuild_watcher(&state);
    watch::start_drain_tick(&state);
    // Before the watcher's events can add to the damage: collapse any track
    // rows stored under a stale spelling of their folder (see watch.rs).
    watch::start_path_normalization(&state);
    if state.borrow().config.media_library.rescan_on_startup {
        watch::trigger_startup_rescan(&state);
    }

    // ── Drives / devices / burn queues — shared with the Media Library ──────────
    // Owned here (not inside open_media_library_window) so the active
    // playlist's Send-to menu (below) and the ML window's Files/Editor/Device
    // views all read and write the SAME lists and burn queue. Threaded into
    // open_media_library_window at each call site. current_drives is kept
    // fresh by the audio-CD watcher below, and current_devices by the device
    // poll further down (mirrors the ML window's own device poll) — both run
    // from app start, independent of whether the ML window has ever been
    // opened, so the active playlist's "Removable Device" Send-to entry is
    // never missing devices while some are actually present.
    // copy_files_holder is only populated once the ML window has been built
    // at least once (its copy runner lives there).
    let current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>> =
        Rc::new(RefCell::new(Vec::new()));
    let current_devices: Rc<RefCell<Vec<crate::devices::Device>>> =
        Rc::new(RefCell::new(Vec::new()));
    let burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>> =
        Rc::new(RefCell::new(Default::default()));
    let copy_files_holder: Rc<
        RefCell<Option<Rc<dyn Fn(crate::devices::Device, Vec<std::path::PathBuf>)>>>,
    > = Rc::new(RefCell::new(None));
    // Filled by the ML window's burn panel; the active playlist's
    // "Send to ▸ Disc Drive" calls it to live-refresh an open panel.
    let burn_refresh_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));

    // ── Duration probe channel ─────────────────────────────────────────────────
    // std::sync::mpsc::Sender is Clone+Send so it can be handed to Rayon
    // worker threads.  The Receiver is polled non-blocking from the tick loop
    // (try_recv), keeping the GTK main thread fully responsive.
    let (probe_tx, probe_rx) = std::sync::mpsc::channel::<(std::path::PathBuf, Duration)>();
    let (broken_tx, broken_rx) = std::sync::mpsc::channel::<std::path::PathBuf>();

    // ── Current track metadata scan channel ─────────────────────────────────────
    // When the player starts a track that has no metadata (empty artist/album_artist),
    // this channel receives the scanned metadata so we can update the marquee display.
    let (current_track_meta_tx, current_track_meta_rx) =
        std::sync::mpsc::channel::<(std::path::PathBuf, String, String, String, String)>();

    // ── Playlist row-finishing channel ────────────────────────────────────────
    // Answers from the background pass that finishes a newly added row: is the
    // file there, is it writable, and — only for files the media library has
    // never seen — its tags and duration. Published on AppState so all 27 add
    // sites reach it through `playlist_add` instead of each being handed a
    // sender, which is how three of them ended up without one.
    let (row_facts_tx, row_facts_rx) =
        std::sync::mpsc::channel::<crate::file_status::RowFacts>();
    // The other half: batches of rows to finish, produced by the playlist
    // window's viewport scan. One worker for the session — the producer is a
    // scroll handler, and a thread per batch would mean a thread per stop while
    // dragging the scrollbar.
    let (row_check_tx, row_check_rx) =
        std::sync::mpsc::channel::<Vec<crate::file_status::RowCheck>>();
    crate::file_status::spawn_row_worker(row_check_rx, row_facts_tx.clone());
    {
        let mut s = state.borrow_mut();
        s.row_facts_tx = Some(row_facts_tx);
        s.row_check_tx = Some(row_check_tx);
    }

    // Populate durations from the on-disk cache for the already-loaded
    // playlist, then probe any tracks that are still unknown.
    {
        state.borrow_mut().apply_cached_durations();
        let paths = state.borrow().uncached_paths_from(0);
        if !paths.is_empty() {
            duration_probe::spawn_probes(paths, probe_tx.clone(), broken_tx.clone());
        }
    }

    // ── Read window geometry from config ──────────────────────────────────────
    // All values are mutable so the display-bounds check below can clamp them.
    let init_playlist_visible = state.borrow().config.window.playlist_visible;
    let init_ml_visible = state.borrow().config.window.ml_visible;
    let init_player_expanded = state.borrow().config.window.player_expanded;
    let mut init_player_width = state.borrow().config.window.player_width;
    let mut init_player_height = state.borrow().config.window.player_height;
    let mut init_pl_width = state.borrow().config.window.playlist_width;
    let mut init_pl_height = state.borrow().config.window.playlist_height;
    let mut init_ml_width = state.borrow().config.window.ml_width;
    let mut init_ml_height = state.borrow().config.window.ml_height;

    // Defensive: if any stored dimension exceeds the largest available monitor,
    // reset that window's geometry to first-launch defaults so it is never
    // sized off-screen.
    {
        use crate::config::WindowConfig;
        if let Some(display) = gdk::Display::default() {
            let monitors = display.monitors();
            let (mut max_w, mut max_h) = (1920i32, 1080i32);
            for i in 0..monitors.n_items() {
                if let Some(obj) = monitors.item(i) {
                    if let Ok(mon) = obj.downcast::<gdk::Monitor>() {
                        let g = mon.geometry();
                        max_w = max_w.max(g.width());
                        max_h = max_h.max(g.height());
                    }
                }
            }
            if init_player_width > max_w || init_player_height > max_h {
                init_player_width = WindowConfig::default_player_width();
                init_player_height = WindowConfig::default_player_height();
            }
            if init_pl_width > max_w || init_pl_height > max_h {
                init_pl_width = WindowConfig::default_playlist_width();
                init_pl_height = WindowConfig::default_playlist_height();
            }
            if init_ml_width > max_w || init_ml_height > max_h {
                init_ml_width = WindowConfig::default_ml_width();
                init_ml_height = WindowConfig::default_ml_height();
            }
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Main window
    // ══════════════════════════════════════════════════════════════════════════

    // Player window — fixed 384 px wide. Non-resizable so the seek bar /
    // transport row / now-playing column proportions can never drift.
    let _ = init_player_width;
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Sparkamp")
        .default_width(384)
        .default_height(init_player_height)
        .resizable(false)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 0);

    // Deferred fullscreen opener — set after handle_key is built (chicken-and-egg).
    // Declared early so the visualiser click handler can reference it.
    let open_fullscreen_fn: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));

    // Deferred A6 art-window opener — same chicken-and-egg as above: the art
    // window's own key controller needs `handle_key` for delegation, but the
    // A1 panel (which needs an art-click callback) is built before
    // `handle_key` exists. Declared early so the A1 panel's art-click
    // handler and the `k` key arm can both reference it; filled in once
    // `handle_key` is defined (see the fullscreen-opener fill site below).
    let art_open: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    // ── Marquee / scrolling-title state ───────────────────────────────────────
    // The full "Title — Artist" string is stored as a Vec<char> so we can slice
    // it by character index without UTF-8 boundary arithmetic.  Each 100 ms tick
    // the scroll offset advances by 1 column; marquee_tick throttles this to
    // one advance every 3 ticks (≈ 3 chars/second — matches classic Winamp).
    let marquee_chars: Rc<RefCell<Vec<char>>> = Rc::new(RefCell::new(Vec::new()));
    let marquee_offset = Rc::new(Cell::new(0usize));
    let marquee_tick = Rc::new(Cell::new(0u32));

    // Last now-playing key (current track's path) seen by the tick loop.
    // Keyed by path rather than index so that replacing a playlist in place
    // (same index, different track) still triggers a refresh. Drives the A1
    // panel / A6 art window fan-out from a single choke point in the tick
    // loop, so EVERY path that changes the current track — Next/Prev/z/b and
    // the ~17 Media Library / device play_current() call sites that used to
    // bypass refresh_now_playing entirely — keeps art in sync, the same way
    // the marquee already does.
    let last_np_key: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Helper: called whenever the playing track changes.  Updates the marquee
    // state and resets the scroll position to the beginning.
    let set_track: Rc<dyn Fn(&str)> = {
        let chars_ref = marquee_chars.clone();
        let off_ref = marquee_offset.clone();
        let tick_ref = marquee_tick.clone();
        Rc::new(move |display: &str| {
            *chars_ref.borrow_mut() = display.chars().collect();
            off_ref.set(0);
            tick_ref.set(0);
        })
    };

    // ── Now-playing row: [time + viz (left)] [marquee title + index (right)] ──
    // Mirrors the classic Winamp 2.x layout: visualizer left, scrolling title
    // right.  The time display (elapsed or remaining) sits just above the viz
    // and toggles on click.
    let np_row = GtkBox::new(Orientation::Horizontal, 14);
    np_row.set_margin_top(6);
    np_row.set_margin_start(8);
    np_row.set_margin_end(8);
    np_row.set_margin_bottom(2);

    // ── Left column: [state icon | time display] ABOVE the mini visualizer ────
    // Collapsed: center the compact time+viz block in the row. Expanded: FILL
    // the taller row so the viz (which vexpands) has space to grow into and its
    // bottom pins to the vol row. Without Fill here left_col stays at its
    // natural height and the viz can never expand.
    let left_col = GtkBox::new(Orientation::Vertical, 2);
    left_col.set_valign(if init_player_expanded {
        Align::Fill
    } else {
        Align::Center
    });

    // Small play/pause/stop indicator — sits inside the same dark box as
    // the time display. Class-less label inherits styling from the parent.
    // Reserve 2 character widths so the emoji glyphs (⏹/▶/⏸), which can have
    // slightly different widths depending on font fallback, can swap without
    // changing the row's natural size.
    let state_label = Label::builder()
        .label("⏹")
        .halign(Align::Center)
        .valign(Align::Center)
        .width_chars(2)
        .max_width_chars(2)
        .xalign(0.5)
        .build();

    // Stop-after-current (phase 6, key `t`): a small stop-square badged on the
    // bottom-right corner of the play/pause/stop indicator (next to the time
    // index) while armed. An Overlay pins it to the state glyph.
    let state_overlay = gtk4::Overlay::new();
    state_overlay.set_child(Some(&state_label));
    let state_stop_badge = Label::new(Some("⏹"));
    state_stop_badge.add_css_class("stop-after-badge");
    state_stop_badge.set_halign(Align::End);
    state_stop_badge.set_valign(Align::End);
    state_stop_badge.set_visible(false);
    state_overlay.add_overlay(&state_stop_badge);

    // Time display label — single-line, monospace, centered.
    // Clicking toggles between elapsed and remaining time.
    // Reserve 7 character widths so "0:00", "12:34", and "-123:45" all
    // allocate the same horizontal slot — without this the time text grows
    // during playback and drags the whole left column wider, causing the
    // visualizer below to widen on play and shrink on stop.
    let show_remaining = Rc::new(Cell::new(false));
    let time_disp_label = Label::builder()
        .label("0:00")
        .halign(Align::Center)
        .width_chars(6)
        .max_width_chars(6)
        .xalign(0.5)
        .build();

    // Row containing [state_icon | time_display] — carries the `.time-disp`
    // dark background so both labels sit in a single box.
    let time_row = GtkBox::new(Orientation::Horizontal, 4);
    time_row.set_halign(Align::Fill);
    time_row.add_css_class("time-disp");
    time_row.append(&state_overlay);
    time_row.append(&time_disp_label);
    {
        let show_rem = show_remaining.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            show_rem.set(!show_rem.get());
        });
        time_row.add_controller(click);
    }

    // Mini visualizer — a Stack holding the Cairo DrawingArea (Bars / Waveform)
    // and a Picture (Granite plasma RGBA buffer). The visible child is swapped
    // to match the active visualizer mode. Its height is a compact fixed value
    // when collapsed; when the A1 panel is expanded it VEXPANDS to fill the
    // taller row (bottom pinned to the vol row) so the time counter above it
    // and the gap down to the seek bar stay put between the two modes.
    const VIZ_HEIGHT_COLLAPSED: i32 = 52;
    // Granite render-texture height. FIXED per mode — deliberately NOT the
    // Picture's live allocation: feeding the allocation back into the texture
    // size creates an intrinsic-size loop that grows the Picture unbounded
    // (full-size when collapsed, taller than Bars/Waveform when expanded).
    // content_fit=Fill upscales this into whatever height the vexpanded row
    // gives it, so a small fixed value stays crisp enough and never over-drives
    // the layout.
    const GRANITE_RENDER_EXPANDED: i32 = 100;
    let granite_render_h = Rc::new(std::cell::Cell::new(if init_player_expanded {
        GRANITE_RENDER_EXPANDED
    } else {
        VIZ_HEIGHT_COLLAPSED
    }));

    let viz = DrawingArea::new();
    viz.set_content_height(VIZ_HEIGHT_COLLAPSED);
    viz.set_valign(Align::Fill);
    viz.set_vexpand(init_player_expanded);
    viz.set_hexpand(true);
    viz.add_css_class("mini-viz");

    let granite_pic = Picture::new();
    granite_pic.set_height_request(VIZ_HEIGHT_COLLAPSED);
    granite_pic.set_valign(Align::Fill);
    granite_pic.set_vexpand(init_player_expanded);
    granite_pic.set_hexpand(true);
    granite_pic.set_content_fit(ContentFit::Fill);
    // The granite texture's intrinsic height is the render resolution. Without
    // can_shrink the Picture refuses to draw below it and pins the collapsed
    // player tall; let it shrink to whatever height the row gives it.
    granite_pic.set_can_shrink(true);
    granite_pic.add_css_class("mini-viz");

    let viz_stack = Stack::new();
    viz_stack.set_hexpand(true);
    viz_stack.set_valign(Align::Fill);
    viz_stack.set_vexpand(init_player_expanded);
    viz_stack.set_height_request(VIZ_HEIGHT_COLLAPSED);
    // Track the visible child's height rather than the tallest child's, so the
    // tall granite Picture never forces the row taller when Bars/Waveform is
    // showing (and vice-versa).
    viz_stack.set_vhomogeneous(false);
    viz_stack.add_named(&viz, Some("cairo"));
    viz_stack.add_named(&granite_pic, Some("granite"));
    viz_stack.set_visible_child_name(
        match state.borrow().config.visualizer.mode {
            VisualizerMode::Granite => "granite",
            _ => "cairo",
        },
    );

    {
        let state_vc = state.clone();
        let open_fs_vc = open_fullscreen_fn.clone();
        let click = GestureClick::new();
        // Single click: cycle mode (or retry spectrum).
        // Double click: open fullscreen when in Waveform or Granite mode.
        // GestureClick fires `released` once per click (n_press 1 then 2),
        // so the first release of a double-click has already cycled the mode
        // by the time the second arrives. Remember the pre-click state so
        // the double-click can undo the cycle and judge fullscreen support
        // on the mode the user actually double-clicked.
        let pre_click: Rc<RefCell<Option<VisualizerMode>>> =
            Rc::new(RefCell::new(None));
        click.connect_released(move |_, n_press, _, _| {
            if n_press == 2 {
                if let Some(mode) = pre_click.borrow_mut().take() {
                    let mut s = state_vc.borrow_mut();
                    s.config.visualizer.mode = mode;
                }
                let supports_fs = matches!(
                    state_vc.borrow().config.visualizer.mode,
                    VisualizerMode::Waveform | VisualizerMode::Granite,
                );
                if supports_fs {
                    if let Some(ref opener) = *open_fs_vc.borrow() {
                        opener();
                    }
                }
                return;
            }
            let needs_retry = {
                let s = state_vc.borrow();
                !s.player.has_spectrum_data() && s.config.visualizer.mode == VisualizerMode::Bars
            };
            if needs_retry {
                *pre_click.borrow_mut() = None;
                let _ = state_vc.borrow_mut().retry_spectrum();
            } else {
                let mut s = state_vc.borrow_mut();
                *pre_click.borrow_mut() = Some(s.config.visualizer.mode.clone());
                s.toggle_visualizer_mode();
            }
        });
        // Attach the click controller to the Stack rather than each child so
        // events fire whether the Cairo DrawingArea or the Granite Picture
        // is the visible child.
        viz_stack.add_controller(click);
    }

    left_col.append(&time_row);
    left_col.append(&viz_stack);
    // Pin the left column to a fixed width (70 px). Without this, the
    // time-display string ("0:00" vs "12:34 / 45:67") would drag the column
    // wider when it grows and snap it narrower when it shrinks, jiggling
    // the visualizer below it. A fixed-width column also means the marquee
    // on the right always has the same horizontal budget.
    left_col.set_size_request(70, -1);
    time_row.set_hexpand(true);

    // ── Right column: marquee frame (title only) + index + vol row ───────────
    // `np_info` fills the full height of `np_row` so the vol row at the bottom
    // aligns horizontally with the bottom of the 68 px visualizer on the left.
    let np_info = GtkBox::new(Orientation::Vertical, 0);
    np_info.set_hexpand(true);
    np_info.set_valign(Align::Fill);

    // The `.np-frame` border wraps ONLY the scrolling title, not the vol row.
    // margin_start/end(8) matches the vol row so the marquee box, the carousel
    // box, the vol slider, and the mode buttons all share the same left/right
    // borders and the same 8px inset from the window edge.
    let marquee_frame = GtkBox::new(Orientation::Vertical, 0);
    marquee_frame.add_css_class("np-frame");
    marquee_frame.set_margin_top(4);
    marquee_frame.set_margin_start(8);
    marquee_frame.set_margin_end(8);

    // Marquee label — no ellipsize; we manually slide the text window each tick.
    // single_line_mode ensures overflow is hidden at the label boundary rather
    // than wrapping to a second line.
    let title_label = Label::builder()
        .label("No track loaded")
        .halign(Align::Fill)
        .xalign(0.0) // text left-aligned within the full-width label
        .hexpand(true)
        .margin_start(8) // aligns with the VOL label start in the row below
        .single_line_mode(true)
        .css_classes(["np-title"])
        .build();

    // Inline show/hide toggle at the right end of the marquee — a borderless,
    // background-less arrow (down = reveal panel, up = hide) tinted with the
    // skin's button colour. Clicking it runs the same shared toggle as the `w`
    // key (wired once `toggle_np_panel` is built below), which flips its icon.
    let np_toggle = Button::from_icon_name(if init_player_expanded {
        "pan-up-symbolic"
    } else {
        "pan-down-symbolic"
    });
    np_toggle.add_css_class("flat");
    np_toggle.add_css_class("np-collapse-btn");
    np_toggle.set_valign(Align::Center);
    np_toggle.set_has_frame(false);
    np_toggle.set_tooltip_text(Some("Show/hide now-playing panel (w)"));

    // The title fills the row and pushes the toggle to the far right.
    let marquee_row = GtkBox::new(Orientation::Horizontal, 0);
    marquee_row.append(&title_label);
    marquee_row.append(&np_toggle);
    marquee_frame.append(&marquee_row);
    // The scrolling "artist - title" marquee is PERSISTENT — it sits above the
    // carousel and stays put on every track / carousel-page change, in both the
    // collapsed (classic) and expanded layouts. Only the data area below it is
    // swapped by the Stack.
    np_info.append(&marquee_frame);

    // A1 expandable now-playing panel (`w` key / mode button) — replaces the
    // marquee when expanded. Built once and swapped via a Stack (rather than
    // reparented on every toggle) so its scroll position and widget identity
    // survive repeated open/close. Seeded with whatever track is already
    // playing (AppState::current_now_playing) so toggling mid-playback isn't
    // empty — subscribe_now_playing's fan-out only fires on the *next* track
    // change, not for tracks already playing when the panel is built.
    let initial_np = state.borrow().current_now_playing();
    let (np_panel_widget, np_panel_update) = now_playing::build_panel(initial_np.as_ref(), {
        // Routed through the deferred `art_open` slot (declared above) since
        // the real opener can't be built until `handle_key` exists.
        let art_open = art_open.clone();
        Rc::new(move || {
            if let Some(f) = art_open.borrow().as_ref() {
                f();
            }
        })
    }, {
        // A1 "Lyrics" link — acts on whatever track is currently playing.
        // rebuild_playlist isn't built yet at this point, so the Edit-in-editor
        // path uses a no-op refresh; the panel self-refreshes on the next track
        // change via subscribe_now_playing.
        let state_lyr = state.clone();
        Rc::new(move || {
            let cur = state_lyr.borrow().playlist.current().map(|t| {
                (t.path.clone(), t.artist.clone(), t.title.clone(), t.album_artist.clone())
            });
            if let Some((path, artist, title, album_artist)) = cur {
                view_or_search_lyrics(
                    &state_lyr, &path, &artist, &title, &album_artist, Rc::new(|| {}),
                    LyricsMode::Current,
                );
            }
        })
    });
    state.borrow_mut().subscribe_now_playing(np_panel_update.clone());

    // Retarget an open Current-mode lyrics window on every track change
    // (F15 revision, point 4). Registered once; the lyrics window sets/clears
    // `lyrics_refresh` as it opens/closes, and the mode gate lives in the Cell.
    {
        let state_sub = state.clone();
        let cb: Rc<dyn Fn(&crate::now_playing::NowPlayingInfo)> = Rc::new(move |_info| {
            let (mode, refresh) = {
                let s = state_sub.borrow();
                (s.lyrics_mode.get(), s.lyrics_refresh.clone())
            };
            if mode == LyricsMode::Current {
                if let Some(r) = refresh {
                    r();
                }
            }
        });
        state.borrow_mut().subscribe_now_playing(cb);
    }

    // Collapsed shows nothing extra below the persistent marquee (classic
    // look); expanded shows the art + carousel panel.
    let np_collapsed = GtkBox::new(Orientation::Vertical, 0);

    let np_stack = Stack::new();
    np_stack.set_hexpand(true);
    // A GtkStack defaults to vhomogeneous = true, sizing itself to the TALLEST
    // child regardless of which is visible — that would pin the row to the
    // expanded panel height even when collapsed, and (on a resizable(false)
    // window) leave the natural height unchanged between states so the window
    // never grows/shrinks on toggle. Track the visible child's height instead,
    // so collapse is compact and expand actually enlarges the window.
    np_stack.set_vhomogeneous(false);
    // Same 8px left/right inset as the marquee + vol row (art's left edge lines
    // up with the VOL label), and a bottom gap so the carousel doesn't butt up
    // against the mode-button (vol) row. Collapsed child is empty, so these
    // only show in the expanded panel.
    np_stack.set_margin_start(8);
    np_stack.set_margin_end(8);
    np_stack.set_margin_bottom(8);
    np_stack.add_named(&np_collapsed, Some("collapsed"));
    np_stack.add_named(&np_panel_widget, Some("expanded"));
    np_stack.set_visible_child_name(if init_player_expanded {
        "expanded"
    } else {
        "collapsed"
    });
    np_info.append(&np_stack);

    // Expanding spring pushes the vol row to the bottom of the column so it
    // sits on the same horizontal line as the bottom of the visualizer.
    let info_spring = GtkBox::new(Orientation::Vertical, 0);
    info_spring.set_vexpand(true);
    np_info.append(&info_spring);

    np_row.append(&left_col);
    np_row.append(&np_info);
    root.append(&np_row);

    // ── Buttons created early so they can all live in the vol row ───────────
    // Mode buttons are icon-only to mirror the macOS layout's compact look.
    // The `.mode-btn-active` class is toggled by the corresponding window's
    // visible-notify handler so the icon lights up while the window is open.
    let init_repeat = state.borrow().config.playback.repeat_mode;
    // Repeat / shuffle are icon+text to match the macOS ModeButton layout.
    // Inner Image / Label refs are kept so the cycle handlers can swap both
    // when the repeat mode rotates.
    let repeat_icon = Image::from_icon_name(repeat_btn_icon(init_repeat));
    let repeat_label = Label::new(Some(repeat_btn_text(init_repeat)));
    // Reserve width for the widest mode text ("Repeat All") so the button
    // doesn't reflow when cycling between modes. xalign default 0.5 keeps
    // the icon+label visually centered inside the reserved width.
    repeat_label.set_width_chars(10);
    repeat_label.set_max_width_chars(10);
    repeat_label.set_xalign(0.5);
    let repeat_box = GtkBox::new(Orientation::Horizontal, 3);
    repeat_box.append(&repeat_icon);
    repeat_box.append(&repeat_label);
    let btn_repeat = Button::new();
    btn_repeat.set_child(Some(&repeat_box));
    btn_repeat.add_css_class("mode-btn");
    btn_repeat.set_tooltip_text(Some("Repeat: off / 1 (song) / all"));
    if init_repeat != crate::shuffle::RepeatMode::Off {
        btn_repeat.add_css_class("mode-btn-active");
    }
    let init_shuffle = state.borrow().shuffle_state.enabled;
    let shuffle_box = GtkBox::new(Orientation::Horizontal, 3);
    shuffle_box.append(&Image::from_icon_name("media-playlist-shuffle-symbolic"));
    shuffle_box.append(&Label::new(Some("Shuffle")));
    let btn_shuffle = Button::new();
    btn_shuffle.set_child(Some(&shuffle_box));
    btn_shuffle.add_css_class("mode-btn");
    btn_shuffle.set_tooltip_text(Some("Shuffle on/off"));
    if init_shuffle {
        btn_shuffle.add_css_class("mode-btn-active");
    }

    let btn_pl = Button::from_icon_name("view-list-symbolic");
    btn_pl.add_css_class("mode-btn");
    btn_pl.set_tooltip_text(Some("Playlist (p)"));
    let btn_eq = Button::from_icon_name("applications-multimedia-symbolic");
    btn_eq.add_css_class("mode-btn");
    btn_eq.set_tooltip_text(Some("10-band equalizer (u)"));
    // Size the "ⓘ" glyph to match the other mode-btn icons (which use SVG
    // icon-name buttons sized by GTK).  Pango markup avoids a global font
    // bump on every mode-btn label.
    let btn_info = {
        let lbl = Label::new(None);
        lbl.set_markup("<span size=\"x-large\">ⓘ</span>");
        let b = Button::new();
        b.set_child(Some(&lbl));
        b
    };
    btn_info.add_css_class("mode-btn");
    btn_info.set_tooltip_text(Some("Keyboard shortcuts (i)"));
    let btn_jump_vol = Button::from_icon_name("edit-find-symbolic");
    btn_jump_vol.add_css_class("mode-btn");
    btn_jump_vol.set_tooltip_text(Some("Jump to track (j)"));
    let btn_ml = Button::from_icon_name("folder-music-symbolic");
    btn_ml.add_css_class("mode-btn");
    btn_ml.set_tooltip_text(Some("Media library"));

    // Single source of truth for the now-playing panel toggle, shared by the
    // `w` key and the inline marquee arrow so both triggers run the identical
    // Stack-swap / viz-resize / arrow-flip / persist logic.
    let toggle_np_panel: Rc<dyn Fn()> = {
        let state = state.clone();
        let np_stack = np_stack.clone();
        let viz = viz.clone();
        let viz_stack = viz_stack.clone();
        let granite_pic = granite_pic.clone();
        let left_col = left_col.clone();
        let np_toggle = np_toggle.clone();
        let granite_render_h = granite_render_h.clone();
        let window_wk = window.downgrade();
        Rc::new(move || {
            let expanded = {
                let mut s = state.borrow_mut();
                let now = !s.config.window.player_expanded;
                s.config.window.player_expanded = now;
                now
            };
            let _ = state.borrow().config.save();

            np_stack.set_visible_child_name(if expanded { "expanded" } else { "collapsed" });
            // Flip the inline marquee arrow: down = reveal, up = hide.
            np_toggle.set_icon_name(if expanded {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            });

            // Expanded: left_col fills the taller row and the viz vexpands into
            // that space (bottom pinned to the vol row); collapsed: left_col
            // re-centers the compact block and the viz drops to fixed height.
            left_col.set_valign(if expanded { Align::Fill } else { Align::Center });
            viz.set_vexpand(expanded);
            viz_stack.set_vexpand(expanded);
            granite_pic.set_vexpand(expanded);
            granite_render_h.set(if expanded {
                GRANITE_RENDER_EXPANDED
            } else {
                VIZ_HEIGHT_COLLAPSED
            });
            // Drop the current (old-size) granite frame NOW so the window
            // measures the Picture at zero intrinsic and can shrink on collapse;
            // the tick loop refills it at the new size within a frame.
            granite_pic.set_paintable(gtk4::gdk::Paintable::NONE);

            // `resizable(false)` windows don't renegotiate height on their own;
            // re-kick a fixed 384px width / natural height so the window grows
            // to fit the expanded panel and shrinks back on collapse.
            if let Some(w) = window_wk.upgrade() {
                w.set_default_size(384, -1);
                w.queue_resize();
            }
        })
    };

    // The inline marquee arrow drives the shared now-playing panel toggle.
    np_toggle.connect_clicked({
        let toggle = toggle_np_panel.clone();
        move |_| toggle()
    });

    // ── Vol row: [VOL] [vol_bar(half-width)] [spring] [ℹ] [ML] [EQ] [PL] ─
    // Vol bar is fixed-width so it reads as secondary to the seek bar below.
    // PL is pushed to the far right with an expanding spacer.
    let vol_row = GtkBox::new(Orientation::Horizontal, 4);
    vol_row.set_margin_start(8);
    vol_row.set_margin_end(8);
    vol_row.set_margin_bottom(2);

    let vol_label = Label::builder()
        .label("VOL")
        .css_classes(["vol-label"])
        .build();

    let init_vol = state.borrow().config.playback.volume;
    let vol_adj = Adjustment::new(init_vol, 0.0, 1.0, 0.05, 0.1, 0.0);
    let vol_bar = Scale::new(Orientation::Horizontal, Some(&vol_adj));
    vol_bar.set_draw_value(false);
    vol_bar.set_hexpand(false);
    vol_bar.set_width_request(90);
    vol_bar.add_css_class("vol-scale");

    // Expanding spacer pushes PL to the right edge of np_info.
    let vol_spring = GtkBox::new(Orientation::Horizontal, 0);
    vol_spring.set_hexpand(true);

    vol_row.append(&vol_label);
    vol_row.append(&vol_bar);
    vol_row.append(&vol_spring);
    vol_row.append(&btn_info);
    vol_row.append(&btn_jump_vol);
    vol_row.append(&btn_ml);
    vol_row.append(&btn_eq);
    vol_row.append(&btn_pl);

    np_info.append(&vol_row);

    // ── Progress / seek row ───────────────────────────────────────────────────
    // Time labels have moved above the visualizer; the seek row now contains
    // only the bar itself so it reads as the dominant control in this area.
    let prog_row = GtkBox::new(Orientation::Horizontal, 4);
    prog_row.set_margin_start(8);
    prog_row.set_margin_end(8);
    prog_row.set_margin_bottom(0);

    let seek_adj = Adjustment::new(0.0, 0.0, 1.0, 0.01, 0.1, 0.0);
    let seek_bar = Scale::new(Orientation::Horizontal, Some(&seek_adj));
    seek_bar.set_draw_value(false);
    seek_bar.set_hexpand(true);
    seek_bar.add_css_class("seek-scale");

    prog_row.append(&seek_bar);
    root.append(&prog_row);

    // ── Transport buttons + GNOME logo ───────────────────────────────────────
    // Row spans the full width: buttons left-aligned, logo pinned to the right.
    let transport = GtkBox::new(Orientation::Horizontal, 8);
    transport.set_hexpand(true);
    transport.set_margin_start(8);
    transport.set_margin_end(8);
    transport.set_margin_top(8);
    transport.set_margin_bottom(8);

    let btn_prev = Button::from_icon_name("media-skip-backward-symbolic");
    let btn_play = Button::from_icon_name("media-playback-start-symbolic");
    let btn_pause = Button::from_icon_name("media-playback-pause-symbolic");
    let btn_stop = Button::from_icon_name("media-playback-stop-symbolic");
    let btn_next = Button::from_icon_name("media-skip-forward-symbolic");

    for btn in [&btn_prev, &btn_play, &btn_pause, &btn_stop, &btn_next] {
        btn.add_css_class("transport");
    }
    // `transport-play` accent is toggled dynamically by the tick loop based on
    // the engine's playback state — applied while Playing/Paused, removed when
    // Stopped — so initial Stopped state matches the visual.
    // Sparkamp skin-format CSS classes — used by skins to target individual
    // buttons with background-image overrides (.sparkamp-button-play { ... }).
    btn_prev.add_css_class("sparkamp-button-prev");
    btn_play.add_css_class("sparkamp-button-play");
    btn_pause.add_css_class("sparkamp-button-pause");
    btn_stop.add_css_class("sparkamp-button-stop");
    btn_next.add_css_class("sparkamp-button-next");

    // Load logo at ~42 px (50 % larger than the transport buttons).
    // If the PNG fails to load (e.g. asset missing), the image slot stays blank.
    const LOGO_PX: i32 = 42;
    let logo_pixbuf = load_logo_pixbuf(LOGO_PX);
    let logo_img = Image::new();
    logo_img.set_valign(Align::Center);
    logo_img.set_pixel_size(LOGO_PX);
    // Extra right-side padding so the logo's right edge aligns with the PL
    // button and progress bar end (both sit at 8px from the window edge; the
    // transport box itself already has margin_end(8)).
    logo_img.set_margin_end(8);
    if let Some(ref pb) = logo_pixbuf {
        logo_img.set_from_pixbuf(Some(pb));
    }

    // Two equal springs place repeat/shuffle equidistant between Next and logo.
    let transport_spring_l = GtkBox::new(Orientation::Horizontal, 0);
    transport_spring_l.set_hexpand(true);
    let transport_spring_r = GtkBox::new(Orientation::Horizontal, 0);
    transport_spring_r.set_hexpand(true);

    // Repeat/shuffle sit at natural (shorter) height rather than stretching
    // to fill the transport row.
    btn_repeat.set_valign(Align::Center);
    btn_shuffle.set_valign(Align::Center);

    transport.append(&btn_prev);
    transport.append(&btn_play);
    transport.append(&btn_pause);
    transport.append(&btn_stop);
    transport.append(&btn_next);
    transport.append(&transport_spring_l);
    transport.append(&btn_repeat);
    transport.append(&btn_shuffle);
    transport.append(&transport_spring_r);
    transport.append(&logo_img);
    root.append(&transport);

    // ── Status bar (main window) ──────────────────────────────────────────────
    let status_label = Label::builder()
        .label("")
        .halign(Align::Start)
        .css_classes(["status-label"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    status_label.set_margin_start(8);
    status_label.set_margin_end(8);
    status_label.set_margin_bottom(4);
    root.append(&status_label);
    // Hidden probe label carries .np-title CSS class.  Appended to the main
    // window root so it is realized — and its computed text color readable —
    // as soon as the main window opens, not only when the playlist opens.
    let np_probe = Label::builder()
        .css_classes(["np-title"])
        .visible(false)
        .build();
    root.append(&np_probe);

    // Every toast in this window lands here. Wrapping the root once means
    // call sites only need the window, not a threaded-through overlay.
    let toaster = adw::ToastOverlay::new();
    toaster.set_child(Some(&root));
    window.set_child(Some(&toaster));


    // ══════════════════════════════════════════════════════════════════════════
    // Playlist window (separate, transient to main window)
    // ══════════════════════════════════════════════════════════════════════════
    //
    // `set_transient_for` groups the playlist with the main window in the
    // taskbar and prompts the WM to raise/lower them together.  On Wayland the
    // compositor controls exact placement; on X11 it opens wherever the WM
    // decides (typically near the main window).  Both windows remember their
    // last size via the config and restore it on the next launch.

    let playlist_win = ApplicationWindow::builder()
        .application(app)
        .title("Sparkamp — Playlist")
        .default_width(init_pl_width)
        .default_height(init_pl_height)
        .transient_for(&window)
        .build();

    // Mirror playlist-window visibility onto the PL toggle button so it lights
    // up while the playlist is open and dims when it closes.
    playlist_win.connect_visible_notify({
        let btn = btn_pl.clone();
        move |w| {
            if w.is_visible() {
                btn.add_css_class("mode-btn-active");
            } else {
                btn.remove_css_class("mode-btn-active");
            }
        }
    });

    let pl_root = GtkBox::new(Orientation::Vertical, 0);

    // The playlist window is a window of its own; it is built whole in
    // `playlist_window.rs` and hands back the parts the rest of `build`,
    // and `PlayerCtx` below, still read.
    let playlist_window::PlaylistWin {
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
    } = playlist_window::build(playlist_window::Deps {
        state: state.clone(),
        window: window.clone(),
        playlist_win: playlist_win.clone(),
        pl_root: pl_root.clone(),
        logo_img: logo_img.clone(),
        set_track: set_track.clone(),
        last_np_key: last_np_key.clone(),
        current_track_meta_tx: current_track_meta_tx.clone(),
        provider_for_settings: provider_for_settings.clone(),
        initial_vars: initial_vars.clone(),
    });

    // ── Initial state ─────────────────────────────────────────────────────────

    rebuild_playlist();
    {
        let s = state.borrow();
        if let Some(t) = s.playlist.current() {
            set_track(&t.display_name());
        }
    }

    // Desktop notification on track change. MPRIS already publishes metadata,
    // so the Shell's media widget is covered; this is the transient banner.
    // Fires only when no Sparkamp window is focused — a banner over the
    // player you are already looking at is why people disable these. "No
    // Sparkamp window" is checked against every persistent top-level window
    // the app can have open (main player, playlist, Media Library, Settings,
    // ID3 editor, Lyrics, Album Art) — being focused on any of them means
    // you're already looking at Sparkamp, so the same reasoning applies.
    // Short-lived helper popups (Jump, Shortcuts, disc/device dialogs) are
    // not tracked as persistent state and are left out of this check.
    {
        let state_rc = state.clone();
        let app_rc = app.clone();
        let win_wk = window.downgrade();
        let pl_wk = playlist_win.downgrade();
        let cb: Rc<dyn Fn(&crate::now_playing::NowPlayingInfo)> = Rc::new(move |_info| {
            let s = state_rc.borrow();
            if !s.config.playback.notify_track_change {
                return;
            }
            let singleton_active =
                |w: &Option<gtk4::Window>| w.as_ref().map(|w| w.is_active()).unwrap_or(false);
            let focused = win_wk.upgrade().map(|w| w.is_active()).unwrap_or(false)
                || pl_wk.upgrade().map(|w| w.is_active()).unwrap_or(false)
                || singleton_active(&s.ml_window)
                || singleton_active(&s.settings_window)
                || singleton_active(&s.id3_editor_window)
                || singleton_active(&s.lyrics_window)
                || singleton_active(&s.art_window);
            if focused {
                return;
            }
            // NowPlayingInfo carries curated (label, value) tag pairs re-read
            // straight off disk for the panel/editor — it has no structured
            // title/artist fields or TPE1→TPE2 fallback of its own. The
            // playlist's Track already has both, via notification_lines(),
            // so reading it from there keeps one source of truth instead of
            // re-deriving artist precedence from the tag list.
            let (heading, body) = match s.playlist.current() {
                Some(t) => t.notification_lines(),
                None => return,
            };
            let n = gio::Notification::new(&gtk_safe(&heading));
            if let Some(b) = body {
                n.set_body(Some(&gtk_safe(&b)));
            }
            // The app icon rather than the cover: a notification icon is
            // rendered at ~48px, where album art is unreadable anyway, and
            // this keeps the banner identifiably Sparkamp's.
            n.set_icon(&gio::ThemedIcon::new("dev.sparkamp.Sparkamp"));
            // A stable id replaces the previous banner instead of stacking
            // one per track.
            app_rc.send_notification(Some("sparkamp-track"), &n);
        });
        state.borrow_mut().subscribe_now_playing(cb);
    }

    // Bundled here: everything below this point that was carved into its own
    // module reads the same widgets and callbacks the rest of `build` does.
    let ctx = PlayerCtx {
        state: state.clone(),
        window: window.clone(),
        playlist_win: playlist_win.clone(),
        pl_view: pl_view.clone(),
        pl_scroll: pl_scroll.clone(),
        pl_status_label: pl_status_label.clone(),
        seek_bar: seek_bar.clone(),
        status_label: status_label.clone(),
        repeat_icon: repeat_icon.clone(),
        repeat_label: repeat_label.clone(),
        btn_prev: btn_prev.clone(),
        btn_play: btn_play.clone(),
        btn_pause: btn_pause.clone(),
        btn_stop: btn_stop.clone(),
        btn_next: btn_next.clone(),
        btn_repeat: btn_repeat.clone(),
        btn_shuffle: btn_shuffle.clone(),
        btn_pl: btn_pl.clone(),
        btn_ml: btn_ml.clone(),
        btn_eq: btn_eq.clone(),
        btn_info: btn_info.clone(),
        btn_add_files: btn_add_files.clone(),
        btn_add_dir: btn_add_dir.clone(),
        set_track: set_track.clone(),
        rebuild_playlist: rebuild_playlist.clone(),
        patch_pl_row: patch_pl_row.clone(),
        scroll_to_row_if_needed: scroll_to_row_if_needed.clone(),
        play_and_update: play_and_update.clone(),
        refresh_now_playing: refresh_now_playing.clone(),
        refresh_pl_status: refresh_pl_status.clone(),
        remove_selected: remove_selected.clone(),
        queue_toggle_selection: queue_toggle_selection.clone(),
        toggle_np_panel: toggle_np_panel.clone(),
        open_fullscreen_fn: open_fullscreen_fn.clone(),
        art_open: art_open.clone(),
        current_drives: current_drives.clone(),
        current_devices: current_devices.clone(),
        burn_queues: burn_queues.clone(),
        copy_files_holder: copy_files_holder.clone(),
        burn_refresh_holder: burn_refresh_holder.clone(),
        probe_tx: probe_tx.clone(),
        broken_tx: broken_tx.clone(),
        current_track_meta_tx: current_track_meta_tx.clone(),
    };

    dnd::install(&ctx);

    // ── Playlist window "Remove" button ───────────────────────────────────────
    btn_remove.connect_clicked({
        let remove_selected = remove_selected.clone();
        move |_| remove_selected()
    });

    // ── Playlist window "✕ All" button — clear entire playlist ───────────────
    btn_clear_all.connect_clicked({
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let set_track = set_track.clone();
        move |_| {
            {
                let mut s = state.borrow_mut();
                let _ = s.player.stop();
                s.playlist.tracks.clear();
                s.queue.clear();
                s.playlist.current_index = 0;
                s.last_duration = None;
                s.pending_seek = None;
                s.mute_pending = None;
            }
            set_track("No track loaded");
            rebuild_playlist();
        }
    });

    // ── Left-click on the marquee title → open ID3 editor for current track ──
    // Adding the click controller to title_label so only the text area is
    // clickable, not the whole now-playing frame.
    {
        let state_mc = state.clone();
        let win_mc = window.downgrade();
        let rebuild_mc = rebuild_playlist.clone();
        let click = GestureClick::new();
        click.set_button(1); // primary button
        click.connect_released(move |_, _, _, _| {
            let path = state_mc.borrow().playlist.current().map(|t| t.path.clone());
            if let Some(path) = path {
                if let Some(w) = win_mc.upgrade() {
                    open_id3_editor_window(
                        Some(&w),
                        path,
                        state_mc.clone(),
                        rebuild_mc.clone(),
                        None,
                        None,
                    );
                }
            }
        });
        title_label.add_controller(click);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // The three add buttons and their two-phase scan live in add_files.rs.
    add_files::install(&ctx, &btn_save_active, &btn_cancel);

    // Volume slider
    // ══════════════════════════════════════════════════════════════════════════

    // connect_change_value fires only on user-driven changes, avoiding a loop.
    vol_bar.connect_change_value({
        let state = state.clone();
        move |_, _, value| {
            let mut s = state.borrow_mut();
            s.config.playback.volume = value;
            s.player.set_volume(value);
            glib::Propagation::Proceed
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Seek bar interaction
    // ══════════════════════════════════════════════════════════════════════════

    // connect_change_value fires for both a single trough click and thumb drag.
    // It does NOT fire when set_value() is called programmatically (GTK only
    // emits change-value for user-initiated changes), so there is no feedback
    // loop between the tick-loop's set_value calls and this handler.
    //
    // Note: GestureClick added directly to GtkScale does not reliably fire
    // its released signal because the Scale's internal GestureDrag claims the
    // pointer sequence after the press.  We therefore skip the is_seeking flag
    // and let the tick loop freely update the bar and label — set_value()
    // cannot re-trigger this handler so there is no oscillation risk.
    seek_bar.connect_change_value({
        let state = state.clone();
        let time_lbl = time_disp_label.clone();
        let show_rem = show_remaining.clone();
        move |_, _, value| {
            // Update the time display immediately so the user sees the correct
            // offset while scrubbing (stopped or paused), without waiting for
            // the next 100 ms tick.
            if let Some(text) = state
                .borrow()
                .time_display_for_fraction(value, show_rem.get())
            {
                time_lbl.set_text(&text);
            }
            state.borrow_mut().seek_fraction_or_pend(value);
            glib::Propagation::Proceed // allow the Scale to update its visual position
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Tick loop — fires every 100 ms
    // ══════════════════════════════════════════════════════════════════════════
    // Shutdown flag set by window.connect_close_request below; the
    // visualizer timer breaks on it before gsk paints a freed surface.
    let viz_shutting_down: std::rc::Rc<std::cell::Cell<bool>> =
        std::rc::Rc::new(std::cell::Cell::new(false));
    // Single-driver rule (mirrors GraniteView.swift on macOS): while the
    // fullscreen visualizer is open it owns the shared Granite renderer.
    // The mini tick must yield — its aspect-derived width differs from the
    // fullscreen one, and alternating sizes makes Granite::resize() wipe
    // the feedback buffer every frame (leaving just the raw waveform ink).
    let fs_viz_open: std::rc::Rc<std::cell::Cell<bool>> =
        std::rc::Rc::new(std::cell::Cell::new(false));
    tick::start(
        &ctx,
        tick::Deps {
            time_disp_label: time_disp_label.clone(),
            title_label: title_label.clone(),
            state_label: state_label.clone(),
            state_stop_badge: state_stop_badge.clone(),
            show_remaining: show_remaining.clone(),
            last_np_key: last_np_key.clone(),
            marquee_chars: marquee_chars.clone(),
            marquee_offset: marquee_offset.clone(),
            marquee_tick: marquee_tick.clone(),
            viz: viz.clone(),
            viz_stack: viz_stack.clone(),
            granite_pic: granite_pic.clone(),
            granite_render_h: granite_render_h.clone(),
            viz_shutting_down: viz_shutting_down.clone(),
            fs_viz_open: fs_viz_open.clone(),
            probe_rx,
            broken_rx,
            current_track_meta_rx,
            open_rx,
            row_facts_rx,
        },
    );

    viz::install_mini_draw(&state, &viz);

    // The Jump window is built whole in jump.rs; its handlers are wired
    // further down, once the key dispatcher they delegate to exists.
    let jump = jump::build(&ctx, &btn_jump_vol);
    let jump_win = jump.jump_win.clone();
    let jump_entry = jump.jump_entry.clone();
    let rebuild_jump = jump.rebuild_jump.clone();
    let open_jump_mode = jump.open_jump_mode.clone();

    // Keyboard shortcuts — shared handler applied to player + playlist windows.
    // ══════════════════════════════════════════════════════════════════════════

    // Shared volume step used by the -/= keys and the main-window ↑/↓ keys.
    let step_volume: Rc<dyn Fn(f64)> = {
        let state = state.clone();
        let vol_bar = vol_bar.clone();
        Rc::new(move |delta: f64| {
            let new_vol = {
                let s = state.borrow();
                (s.config.playback.volume + delta).clamp(0.0, 1.0)
            };
            {
                let mut s = state.borrow_mut();
                s.config.playback.volume = new_vol;
                s.player.set_volume(new_vol);
            }
            vol_bar.set_value(new_vol);
        })
    };

    let handle_key = keys::build(
        &ctx,
        &jump_entry,
        &open_jump_mode,
        &rebuild_jump,
        &step_volume,
    );

    // Publish the transport key handler so the lyrics window can forward the
    // Winamp keys (z/x/c/v/b/j/r/s) to it (F15 revision, point 5).
    state.borrow_mut().set_transport_key_handler(handle_key.clone());

    // Wire up the fullscreen opener now that handle_key is fully defined.
    {
        let hk = handle_key.clone();
        let state_fs = state.clone();
        let jump_win_fs = jump_win.clone();
        let jump_entry_fs = jump_entry.clone();
        let rebuild_jump_fs = rebuild_jump.clone();
        let btn_info_fs = btn_info.clone();
        let fs_viz_open_fs = fs_viz_open.clone();
        *open_fullscreen_fn.borrow_mut() = Some(Rc::new(move || {
            open_waveform_fullscreen(
                state_fs.clone(),
                hk.clone(),
                jump_win_fs.clone(),
                jump_entry_fs.clone(),
                rebuild_jump_fs.clone(),
                btn_info_fs.clone(),
                fs_viz_open_fs.clone(),
            );
        }));
    }

    // Wire up the A6 art-window opener now that handle_key is fully defined
    // (same chicken-and-egg as the fullscreen opener above).
    {
        let hk = handle_key.clone();
        let state_art = state.clone();
        let window_art = window.clone();
        *art_open.borrow_mut() = Some(Rc::new(move || {
            art_window::open_or_focus(
                state_art.clone(),
                hk.clone(),
                Some(window_art.upcast_ref::<gtk4::Window>()),
            );
        }));
    }

    // Attach the shared handler to the main player window.
    // Capture phase ensures keys reach the handler even when a child widget
    // (e.g. the visualizer DrawingArea) has keyboard focus. Ctrl+Q is swallowed
    // here (no playlist selection on the main window) so it doesn't fall into
    // the plain-`q` = open-queue arm — enqueue happens from the playlist/jump.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let handler = handle_key.clone();
        let wrap_step_volume = step_volume.clone();
        let wrap_open_settings = open_settings.clone();
        let wrap_save_active = btn_save_active.clone();
        let wrap_btn_info = btn_info.clone();
        let wrap_open_jump = open_jump_mode.clone();
        let wrap_jump_entry = jump_entry.clone();
        let wrap_rebuild_jump = rebuild_jump.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, modifier| {
            if modifier.contains(gdk::ModifierType::CONTROL_MASK) {
                match key {
                    gdk::Key::q | gdk::Key::Q => return glib::Propagation::Stop,
                    // Ctrl+, → settings. Replaces Ctrl+. (the GNOME standard
                    // is comma, and it is what the macOS frontend binds).
                    gdk::Key::comma => {
                        wrap_open_settings();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::s | gdk::Key::S => {
                        wrap_save_active.emit_clicked();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+? → keyboard shortcuts. Arrives as Ctrl+Shift+slash
                    // on most layouts, so both keyvals are accepted.
                    gdk::Key::question | gdk::Key::slash => {
                        wrap_btn_info.emit_clicked();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+F → jump / search, the reflex shortcut in any list UI.
                    // Mirrors the `j` arm in keys.rs exactly (clear the stale
                    // query, rebuild the result list, then open) — the jump
                    // window hides rather than closes, so without this a reopen
                    // via Ctrl+F would show a leftover query and stale results.
                    gdk::Key::f | gdk::Key::F => {
                        wrap_jump_entry.set_text("");
                        wrap_rebuild_jump();
                        wrap_open_jump(false);
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            // Main-window ↑/↓ = volume. The playlist window's own controller
            // does NOT do this, so its TreeView keeps native row browse.
            match key {
                gdk::Key::Up => {
                    wrap_step_volume(0.05);
                    return glib::Propagation::Stop;
                }
                gdk::Key::Down => {
                    wrap_step_volume(-0.05);
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
            handler(key)
        });
        window.add_controller(key_ctrl);
    }

    // Single Capture-phase controller on the playlist window (one controller,
    // so there is no ordering race between a Ctrl+Q override and the shared
    // handler). It intercepts the playlist-specific keys and delegates the rest
    // to `handle_key`:
    //   Ctrl+Q → queue/dequeue the selected rows (the enqueue hotkey; must be
    //            caught BEFORE the shared handler's plain-`q` = open queue).
    //   Esc    → hide the playlist window (a child window — not Quit).
    //   else   → shared handler (plain `q` opens the queue window, j, transport…).
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let toggle = queue_toggle_selection.clone();
        let plwin_wk = playlist_win.downgrade();
        let handler = handle_key.clone();
        let wrap_invert_selection = invert_selection.clone();
        let wrap_save_active = btn_save_active.clone();
        let wrap_open_settings = open_settings.clone();
        let wrap_btn_info = btn_info.clone();
        let wrap_open_jump = open_jump_mode.clone();
        let wrap_jump_entry = jump_entry.clone();
        let wrap_rebuild_jump = rebuild_jump.clone();
        let lyr_state = state.clone();
        let lyr_sel_idx = pl_selected_idx.clone();
        let lyr_rebuild = rebuild_playlist.clone();
        let lyr_pl_view = pl_view.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, modifier| {
            if modifier.contains(gdk::ModifierType::CONTROL_MASK) {
                match key {
                    gdk::Key::q | gdk::Key::Q => {
                        toggle();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+S → save active playlist; Ctrl+I → invert selection.
                    gdk::Key::s | gdk::Key::S => {
                        wrap_save_active.emit_clicked();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::i | gdk::Key::I => {
                        wrap_invert_selection();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+A → select every row, the standard gesture for
                    // "act on the whole list". Ctrl+I inverts, so this was the
                    // conspicuous gap.
                    gdk::Key::a | gdk::Key::A => {
                        lyr_pl_view.selection().select_all();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+, → settings. Replaces Ctrl+. (the GNOME standard
                    // is comma, and it is what the macOS frontend binds).
                    gdk::Key::comma => {
                        wrap_open_settings();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+? → keyboard shortcuts. Arrives as Ctrl+Shift+slash
                    // on most layouts, so both keyvals are accepted.
                    gdk::Key::question | gdk::Key::slash => {
                        wrap_btn_info.emit_clicked();
                        return glib::Propagation::Stop;
                    }
                    // Ctrl+F → jump / search, the reflex shortcut in any list UI.
                    // Mirrors the `j` arm in keys.rs exactly (clear the stale
                    // query, rebuild the result list, then open) — the jump
                    // window hides rather than closes, so without this a reopen
                    // via Ctrl+F would show a leftover query and stale results.
                    gdk::Key::f | gdk::Key::F => {
                        wrap_jump_entry.set_text("");
                        wrap_rebuild_jump();
                        wrap_open_jump(false);
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            if key == gdk::Key::Escape {
                if let Some(w) = plwin_wk.upgrade() {
                    w.set_visible(false);
                }
                return glib::Propagation::Stop;
            }
            // `l` on the playlist window opens lyrics for the single SELECTED
            // row in Specific mode (the window does not follow playback). A
            // multi-row selection does nothing — matching the Media Library and
            // the row menu, which only offer lyrics for exactly one row. With
            // no row selected it falls through to the shared handler, whose `l`
            // arm opens the currently-playing track in Current mode — "selected
            // track, else the current one".
            if matches!(key, gdk::Key::l | gdk::Key::L) {
                #[allow(deprecated)]
                let sel_count = lyr_pl_view.selection().count_selected_rows();
                if sel_count > 1 {
                    return glib::Propagation::Stop; // no action on multi-select
                }
                let idx = lyr_sel_idx.get();
                if sel_count == 1 && idx != usize::MAX {
                    let t = lyr_state.borrow().playlist.tracks.get(idx).map(|t| {
                        (
                            t.path.clone(),
                            t.artist.clone(),
                            t.title.clone(),
                            t.album_artist.clone(),
                        )
                    });
                    if let Some((path, artist, title, album_artist)) = t {
                        view_or_search_lyrics(
                            &lyr_state, &path, &artist, &title, &album_artist,
                            lyr_rebuild.clone(), LyricsMode::Specific,
                        );
                        return glib::Propagation::Stop;
                    }
                }
            }
            handler(key)
        });
        playlist_win.add_controller(key_ctrl);
    }

    // ── Persistent shortcuts window (created once; shown/hidden as a toggle) ──
    // Built here after handle_key is defined so the Esc/transport shortcuts
    // work inside it.
    let shortcuts_win = {
        let win = gtk4::Window::builder()
            .title("Keyboard Shortcuts")
            .modal(false)
            .default_width(420)
            .default_height(480)
            .build();
        win.set_transient_for(Some(window.upcast_ref::<gtk4::Window>()));

        let sections = shortcut_sections();

        let grid = gtk4::Grid::builder()
            .column_spacing(16)
            .row_spacing(4)
            .halign(gtk4::Align::Fill)
            .valign(gtk4::Align::Start)
            .build();

        // Title row.
        let title = gtk4::Label::builder()
            .label("Sparkamp — Keyboard Shortcuts")
            .halign(gtk4::Align::Start)
            .css_classes(["info-title"])
            .build();
        grid.attach(&title, 0, 0, 2, 1);

        let mut row: i32 = 1;
        // Spacer below title.
        let spacer = gtk4::Label::new(Some(""));
        grid.attach(&spacer, 0, row, 2, 1);
        row += 1;

        for (section, entries) in sections.iter() {
            let header = gtk4::Label::builder()
                .label(*section)
                .halign(gtk4::Align::Start)
                .css_classes(["info-section"])
                .build();
            grid.attach(&header, 0, row, 2, 1);
            row += 1;

            for (key, desc) in entries.iter() {
                let key_lbl = gtk4::Label::builder()
                    .label(*key)
                    .halign(gtk4::Align::Start)
                    .css_classes(["info-key"])
                    .build();
                let desc_lbl = gtk4::Label::builder()
                    .label(*desc)
                    .halign(gtk4::Align::Start)
                    .css_classes(["info-desc"])
                    .build();
                grid.attach(&key_lbl,  0, row, 1, 1);
                grid.attach(&desc_lbl, 1, row, 1, 1);
                row += 1;
            }

            // Section spacer.
            let spc = gtk4::Label::new(Some(""));
            grid.attach(&spc, 0, row, 2, 1);
            row += 1;
        }

        let body = GtkBox::new(Orientation::Vertical, 0);
        body.set_css_classes(&["info-text"]);
        body.append(&grid);

        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .margin_top(12).margin_bottom(12)
            .margin_start(12).margin_end(12)
            .child(&body)
            .build();
        let key_ctrl = gtk4::EventControllerKey::new();
        let handler = handle_key.clone();
        let win_wk = win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(w) = win_wk.upgrade() { w.hide(); }
                return glib::Propagation::Stop;
            }
            handler(key)
        });
        win.add_controller(key_ctrl);
        win.set_child(Some(&scroll));
        win.set_hide_on_close(true);
        win.connect_visible_notify({
            let btn = btn_info.clone();
            move |w| {
                if w.is_visible() {
                    btn.add_css_class("mode-btn-active");
                } else {
                    btn.remove_css_class("mode-btn-active");
                }
            }
        });
        win
    };

    // ℹ Info button — toggle keyboard shortcuts window.
    btn_info.connect_clicked({
        let sw = shortcuts_win.clone();
        move |_| {
            if sw.is_visible() { sw.hide(); } else { sw.present(); }
        }
    });

    // Find button — toggle the Jump/Queue window (opens in Jump mode).
    btn_jump_vol.connect_clicked({
        let jump_win_wk = jump_win.downgrade();
        let entry = jump_entry.clone();
        let rebuild = rebuild_jump.clone();
        let open_jump = open_jump_mode.clone();
        move |_| {
            if let Some(w) = jump_win_wk.upgrade() {
                if w.is_visible() {
                    w.hide();
                } else {
                    entry.set_text("");
                    rebuild();
                    open_jump(false);
                }
            }
        }
    });

    // ML button — toggle the media library browser window.
    btn_ml.connect_clicked({
        let window_wk = window.downgrade();
        let state_rc = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_ml = set_track.clone();
        let btn_ml_for_notify = btn_ml.clone();
        let current_drives = current_drives.clone();
        let current_devices = current_devices.clone();
        let burn_queues = burn_queues.clone();
        let copy_files_holder = copy_files_holder.clone();
        let burn_refresh_holder = burn_refresh_holder.clone();
        move |_| {
            // If already open (visible or hidden), toggle visibility.
            {
                let s = state_rc.borrow();
                if let Some(ref w) = s.ml_window {
                    if w.is_visible() { w.hide(); } else { w.present(); }
                    return;
                }
            }
            // First open: create the window. Lazy-open the ML database now if
            // `skip_db_load` left it unopened at startup (F12.3) — this is
            // the real first-demand site.
            ensure_media_lib_open(&state_rc);
            let parent = window_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
            let (w, h) = {
                let cfg = &state_rc.borrow().config.window;
                (cfg.ml_width, cfg.ml_height)
            };
            let ml_win = open_media_library_window(
                parent.as_ref(),
                MlHost {
                    state: state_rc.clone(),
                    rebuild_playlist: rebuild_pl.clone(),
                    set_track: set_track_ml.clone(),
                    current_drives: current_drives.clone(),
                    current_devices: current_devices.clone(),
                    burn_queues: burn_queues.clone(),
                    copy_files_holder: copy_files_holder.clone(),
                    burn_refresh_holder: burn_refresh_holder.clone(),
                },
                w,
                h,
            );
            ml_win.set_hide_on_close(true);
            ml_win.connect_visible_notify({
                let btn = btn_ml_for_notify.clone();
                move |w| {
                    if w.is_visible() {
                        btn.add_css_class("mode-btn-active");
                    } else {
                        btn.remove_css_class("mode-btn-active");
                    }
                }
            });
            // open_media_library_window already calls present() before
            // returning, so the visible-notify above has already fired and
            // skipped attaching — sync the button state to match.
            btn_ml_for_notify.add_css_class("mode-btn-active");
            state_rc.borrow_mut().ml_window = Some(ml_win);
        }
    });

    // ── Audio-CD insertion watcher (auto-open, from app start) ──────────────
    // Every 10 s, a NO-SPIN status check (list_drives_cached — kernel ioctl,
    // full probe only on change) looks for a drive transitioning to "audio CD
    // loaded". On that transition — including the first poll after launch
    // seeing an already-loaded CD, so an OS-handler launch navigates — the
    // Media Library opens (or comes forward) on that drive's detail view.
    // Runs regardless of the ML window; the setting gates the reaction.
    {
        let state_rc = state.clone();
        let btn_ml_watch = btn_ml.clone();
        let prev: Rc<RefCell<Vec<crate::disc::OpticalDrive>>> = Rc::new(RefCell::new(Vec::new()));
        // Keeps the Send-to menu's drive list fresh even before the ML
        // window has ever been opened (its own poll only starts then).
        let current_drives_watch = current_drives.clone();
        let in_flight = Rc::new(Cell::new(false));
        let tick: Rc<dyn Fn()> = Rc::new(move || {
            if in_flight.get() {
                return;
            }
            // NEVER touch the drive while it's being read: even the status
            // ioctls interleave SCSI commands with the streaming reads and
            // make flaky drives fault mid-read (kills playback/rips).
            {
                let s = state_rc.borrow();
                let playing_disc = !matches!(s.player.state(), PlayerState::Stopped)
                    && s
                        .playlist
                        .current()
                        .map(|t| t.path.to_string_lossy().starts_with("cdda://"))
                        .unwrap_or(false);
                if playing_disc || s.disc_reading.get() {
                    return;
                }
            }
            // No auto-show gate here: the poll also drives the playlist
            // invalidation for removed/swapped discs, which must run even
            // when the auto-open setting is off. The setting gates only the
            // open-the-library reaction below.
            in_flight.set(true);
            let state_rc = state_rc.clone();
            let btn_ml_watch = btn_ml_watch.clone();
            let prev = prev.clone();
            let current_drives_watch = current_drives_watch.clone();
            let in_flight = in_flight.clone();
            glib::spawn_future_local(async move {
                let drives = gio::spawn_blocking(crate::disc::detect::list_drives_shared)
                    .await
                    .unwrap_or_default();
                in_flight.set(false);
                let inserted: Option<String> = drives
                    .iter()
                    .find(|d| {
                        d.media.is_audio_cd
                            && !prev
                                .borrow()
                                .iter()
                                .any(|o| o.id == d.id && o.media.is_audio_cd)
                    })
                    .map(|d| d.id.clone());
                // Disc removed or swapped: every active-playlist row still
                // streaming from that drive is dead — mark it broken NOW
                // (event-driven) instead of waiting for a read error, stop
                // the player if the current row was one, and repaint.
                let invalidated: Vec<String> = prev
                    .borrow()
                    .iter()
                    .filter(|old| {
                        if !old.media.is_audio_cd {
                            return false;
                        }
                        let now = drives.iter().find(|d| d.id == old.id);
                        !now.map(|n| n.media.is_audio_cd && n.toc == old.toc)
                            .unwrap_or(false)
                    })
                    .map(|old| old.id.clone())
                    .collect();
                if !invalidated.is_empty() {
                    let rebuild_pl = {
                        let mut s = state_rc.borrow_mut();
                        let cur = s.playlist.current_index;
                        let mut touched = false;
                        let mut current_dead = false;
                        for (i, t) in s.playlist.tracks.iter_mut().enumerate() {
                            let path = t.path.to_string_lossy();
                            let on_gone_drive = crate::disc::parse_cdda_uri(&path)
                                .and_then(|(_, dev)| dev)
                                .map(|dev| invalidated.iter().any(|id| id == dev))
                                .unwrap_or(false);
                            if on_gone_drive && !t.broken {
                                t.broken = true;
                                touched = true;
                                if i == cur {
                                    current_dead = true;
                                }
                            }
                        }
                        if current_dead
                            && !matches!(*s.player.state(), PlayerState::Stopped)
                        {
                            let _ = s.player.stop();
                        }
                        if touched {
                            s.rebuild_pl_callback.clone()
                        } else {
                            None
                        }
                    };
                    if let Some(cb) = rebuild_pl {
                        cb();
                    }
                }
                *current_drives_watch.borrow_mut() = drives.clone();
                *prev.borrow_mut() = drives;
                let Some(id) = inserted else { return };
                if !state_rc.borrow().config.disc.auto_show_inserted_audio_cd {
                    return;
                }
                state_rc.borrow_mut().pending_disc_nav = Some(id);
                // Bring the Media Library up: present the existing window, or
                // create it through the toolbar button's own handler.
                let existing = state_rc.borrow().ml_window.clone();
                match existing {
                    Some(w) => {
                        if !w.is_visible() {
                            w.present();
                        }
                    }
                    None => {
                        // emit_clicked runs the ML button handler above, which
                        // creates + presents the window (and its initial disc
                        // refresh will consume the parked navigation).
                        btn_ml_watch.emit_clicked();
                    }
                }
                // Window already open: nudge its disc poll so the navigation
                // doesn't wait for the next 10 s cadence.
                let refresh = state_rc.borrow().disc_refresh_callback.clone();
                if let Some(f) = refresh {
                    f();
                }
            });
        });
        tick();
        // 2 s: each unchanged tick costs one status ioctl (~ms, no medium
        // access) — insertion reacts about as fast as the file manager.
        glib::timeout_add_local(Duration::from_secs(2), move || {
            tick();
            ControlFlow::Continue
        });
    }

    // ── Device poll: keeps the Send-to menu's device list fresh ─────────────
    // Shares `refresh_device_cache` (util.rs) with the ML window's device
    // poll (media_library.rs) — same listing/merge logic, same 2 s cadence —
    // so `current_devices` is populated from app start instead of staying
    // empty until the ML window has been opened once. Both pollers write the
    // same Rc when the ML window is also open; that's cheap and idempotent
    // (identical source data, no UI to fight over here), the same way
    // `current_drives` above is written by this watcher regardless of
    // whether the ML window's own disc poll is also running — so no extra
    // coordination is added.
    {
        let current_devices_watch = current_devices.clone();
        let in_flight = Rc::new(Cell::new(false));
        // udisks failing or the worker thread panicking just leaves the
        // Send-to entry showing no devices; the ML window (if opened)
        // surfaces the diagnostic banner for this, so there's nothing more
        // to do here on completion.
        let on_done: Rc<dyn Fn(DeviceRefreshOutcome)> = Rc::new(|_outcome| {});
        let tick: Rc<dyn Fn()> = Rc::new(move || {
            refresh_device_cache(current_devices_watch.clone(), in_flight.clone(), on_done.clone());
        });
        tick();
        // Same 2 s cadence as the ML window's device poll.
        glib::timeout_add_local(Duration::from_secs(2), move || {
            tick();
            ControlFlow::Continue
        });
    }

    // EQ button — toggle the 10-band equalizer window.
    let eq_win_ref: Rc<RefCell<Option<gtk4::Window>>> = Rc::new(RefCell::new(None));
    btn_eq.connect_clicked({
        let window_wk = window.downgrade();
        let state_rc = state.clone();
        let eq_ref = eq_win_ref.clone();
        let btn_eq_for_notify = btn_eq.clone();
        move |_| {
            // Toggle if already created.
            {
                let existing = eq_ref.borrow();
                if let Some(ref w) = *existing {
                    if w.is_visible() { w.hide(); } else { w.present(); }
                    return;
                }
            }
            // First open: create the window.
            let parent = window_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
            let win = open_eq_window(parent.as_ref(), state_rc.clone());
            win.connect_visible_notify({
                let btn = btn_eq_for_notify.clone();
                move |w| {
                    if w.is_visible() {
                        btn.add_css_class("mode-btn-active");
                    } else {
                        btn.remove_css_class("mode-btn-active");
                    }
                }
            });
            // open_eq_window calls present() before returning; sync the
            // button state since the notify handler attached above fires only
            // on subsequent visibility changes.
            btn_eq_for_notify.add_css_class("mode-btn-active");
            *eq_ref.borrow_mut() = Some(win);
        }
    });


    // ══════════════════════════════════════════════════════════════════════════
    jump::connect(&ctx, &jump);

    // Window close handlers
    // ══════════════════════════════════════════════════════════════════════════

    // Main window close: save both windows' geometry and playlist-visible state,
    // then destroy the playlist window so the app quits cleanly.
    // Using destroy() bypasses playlist_win's close_request handler (which only
    // hides it) so no ApplicationWindow is left alive keeping the process running.
    window.connect_close_request({
        let state = state.clone();
        let playlist_win = playlist_win.clone();
        let viz_shut = viz_shutting_down.clone();
        move |w| {
            // Stop the 33 ms visualizer timer before any gsk paint can run
            // against a freed surface.
            viz_shut.set(true);
            // Stop new blocking device FUSE work from starting during teardown,
            // so a slow MTP mount can't pin a worker thread and delay exit.
            DEVICE_IO_SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = state.borrow().playlist.save_last();

            let mut cfg = state.borrow().config.clone();
            cfg.window.player_width = w.width();
            cfg.window.player_height = w.height();
            cfg.window.playlist_visible = playlist_win.is_visible();
            // If the playlist window is currently visible, capture its live
            // size.  If it was already hidden, its size was already written to
            // cfg by playlist_win.connect_close_request, so we leave it alone.
            if playlist_win.is_visible() {
                cfg.window.playlist_width = playlist_win.width();
                cfg.window.playlist_height = playlist_win.height();
            }
            cfg.window.ml_visible = state.borrow().ml_window.is_some();
            // Record ML window size for next launch.
            if let Some(ref ml_win) = state.borrow().ml_window {
                cfg.window.ml_width = ml_win.width();
                cfg.window.ml_height = ml_win.height();
            }
            let _ = cfg.save();

            // Quit through the GApplication rather than destroying the other
            // ApplicationWindows (playlist_win / ml_win) by hand: a manual
            // `.destroy()` from inside this close-request handler re-enters GTK's
            // window teardown and segfaults (GtkApplication mutates its window
            // list mid signal-emission). `app.quit()` closes every window and
            // unwinds the main loop cleanly, and still guarantees the process
            // exits even though those windows use hide-on-close.
            if let Some(app) = w.application() {
                app.quit();
            }
            glib::Propagation::Proceed
        }
    });

    // After the main window is realized, read the computed text color of the
    // hidden .np-title probe label and cache it as gdk::RGBA.  The cell data
    // func reads this directly — no string parsing, no GTK color warnings.
    // Hooking the main window (not the playlist window) means the color is
    // available the moment the app starts.
    {
        let accent_rgba = accent_rgba.clone();
        let np_probe = np_probe.clone();
        let patch_pl_row = patch_pl_row.clone();
        let state = state.clone();
        window.connect_realize(move |_| {
            *accent_rgba.borrow_mut() = Some(np_probe.color());
            // Re-patch the current row so it immediately gets the accent color
            // if a track is already playing when the app starts.
            let idx = state.borrow().playlist.current_index;
            patch_pl_row(idx);
        });
    }

    // Stand up the MPRIS D-Bus service (media keys / GNOME widget / playerctl).
    // Degrades silently if there is no session bus or the name is already owned.
    mpris::init(app, &window, state.clone());

    window.present();
    if init_playlist_visible {
        // Delay the playlist window slightly so the Wayland compositor has
        // time to place and map the main window first.  Without this, the
        // playlist window often appears half off-screen because the compositor
        // hasn't resolved the transient-parent relationship yet.
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            playlist_win.present();
        });
    }
    if init_ml_visible {
        let set_track_init_ml = set_track.clone();
        let btn_ml_for_restore = btn_ml.clone();
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            let state_rc = state.clone();
            // The ML window is being restored visible from last session —
            // that is itself first demand, so lazy-open the DB now (F12.3).
            ensure_media_lib_open(&state_rc);
            let rebuild_pl = rebuild_playlist.clone();
            let ml_win = open_media_library_window(
                Some(&window.upcast::<gtk4::Window>()),
                MlHost {
                    state: state_rc.clone(),
                    rebuild_playlist: rebuild_pl.clone(),
                    set_track: set_track_init_ml.clone(),
                    // Moved, not cloned — this restore path is the last use
                    // of these bindings, as it was before.
                    current_drives,
                    current_devices,
                    burn_queues,
                    copy_files_holder,
                    burn_refresh_holder,
                },
                init_ml_width,
                init_ml_height,
            );
            // Mirror the click-handler path: hide-on-close keeps the window
            // alive across toggles, and visible-notify keeps the toolbar
            // button's active class in sync with whether the window is shown.
            ml_win.set_hide_on_close(true);
            ml_win.connect_visible_notify({
                let btn = btn_ml_for_restore.clone();
                move |w| {
                    if w.is_visible() {
                        btn.add_css_class("mode-btn-active");
                    } else {
                        btn.remove_css_class("mode-btn-active");
                    }
                }
            });
            // open_media_library_window calls present() before returning, so
            // the notify above missed the initial show — sync the class now.
            btn_ml_for_restore.add_css_class("mode-btn-active");
            state_rc.borrow_mut().ml_window = Some(ml_win);
        });
    }
}

// ---------------------------------------------------------------------------
// ID3 editor windows
// ---------------------------------------------------------------------------


#[cfg(test)]
mod shortcut_dialog_tests {
    use super::shortcut_sections;

    /// The help window is the single source of truth for GTK bindings — every
    /// key the phase-6 handlers bind must appear in it, so the dialog can never
    /// silently drift from reality. Update this list deliberately when adding keys.
    #[test]
    fn shortcut_dialog_lists_every_phase6_key() {
        let keys: Vec<&str> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|(k, _)| *k))
            .collect();
        for k in [
            "m", "t", "Shift+N", "Ctrl+S", "Ctrl+,", "Ctrl+I", "n", "Enter", "↑ ↓",
        ] {
            assert!(keys.contains(&k), "shortcuts dialog is missing `{k}`");
        }
    }

    /// The shortcuts window is the only place a user discovers these, so a
    /// binding that exists in code but not in the dialog is invisible.
    #[test]
    fn shortcut_dialog_lists_the_standard_aliases() {
        let keys: Vec<&str> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|(k, _)| *k))
            .collect();
        let joined = keys.join(" | ");
        for expected in ["Ctrl+F", "F1", "Ctrl+?", "Ctrl+,"] {
            assert!(
                joined.contains(expected),
                "{expected} missing from the shortcuts dialog: {joined}"
            );
        }
    }

    /// Ctrl+. was replaced by Ctrl+, — the GNOME standard, and what the
    /// macOS frontend already uses. Leaving it listed would document a
    /// binding that no longer fires.
    #[test]
    fn shortcut_dialog_no_longer_lists_ctrl_period() {
        let keys: Vec<&str> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|(k, _)| *k))
            .collect();
        assert!(
            !keys.iter().any(|k| *k == "Ctrl+."),
            "Ctrl+. is no longer bound and must not be listed"
        );
    }
}
