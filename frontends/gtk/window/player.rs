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
        .title("SparkAmp")
        .default_width(384)
        .default_height(init_player_height)
        .resizable(false)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 0);

    // Deferred fullscreen opener — set after handle_key is built (chicken-and-egg).
    // Declared early so the visualiser click handler can reference it.
    let open_fullscreen_fn: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));

    // ── Marquee / scrolling-title state ───────────────────────────────────────
    // The full "Title — Artist" string is stored as a Vec<char> so we can slice
    // it by character index without UTF-8 boundary arithmetic.  Each 100 ms tick
    // the scroll offset advances by 1 column; marquee_tick throttles this to
    // one advance every 3 ticks (≈ 3 chars/second — matches classic Winamp).
    let marquee_chars: Rc<RefCell<Vec<char>>> = Rc::new(RefCell::new(Vec::new()));
    let marquee_offset = Rc::new(Cell::new(0usize));
    let marquee_tick = Rc::new(Cell::new(0u32));

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
    let left_col = GtkBox::new(Orientation::Vertical, 2);
    left_col.set_valign(Align::Center);

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
    time_row.append(&state_label);
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
    // to match the active visualizer mode.
    let viz = DrawingArea::new();
    viz.set_content_height(52);
    viz.set_valign(Align::Center);
    viz.set_hexpand(true);
    viz.add_css_class("mini-viz");

    let granite_pic = Picture::new();
    granite_pic.set_height_request(52);
    granite_pic.set_valign(Align::Center);
    granite_pic.set_hexpand(true);
    granite_pic.set_content_fit(ContentFit::Fill);
    granite_pic.add_css_class("mini-viz");

    let viz_stack = Stack::new();
    viz_stack.set_hexpand(true);
    viz_stack.set_valign(Align::Center);
    viz_stack.set_height_request(52);
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
    let marquee_frame = GtkBox::new(Orientation::Vertical, 0);
    marquee_frame.add_css_class("np-frame");
    marquee_frame.set_margin_top(4);
    marquee_frame.set_margin_start(4);
    marquee_frame.set_margin_end(4);

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

    marquee_frame.append(&title_label);
    np_info.append(&marquee_frame);

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

    // ── Vol row: [VOL] [vol_bar(half-width)] [spring] [ℹ] [ML] [EQ] [PL] ───
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

    window.set_child(Some(&root));


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
        .title("SparkAmp — Playlist")
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

    // ── Playlist window header: track count ───────────────────────────────────
    let pl_count_label = Label::builder()
        .label("Playlist — 0 tracks")
        .halign(Align::Start)
        .css_classes(["pl-count-label"])
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

    // Left-align the add buttons; right-align destructive buttons with a flexible spacer.
    pl_btn_row.append(&btn_add_files);
    pl_btn_row.append(&btn_add_dir);
    pl_btn_row.append(&btn_save_active);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    pl_btn_row.append(&spacer);
    pl_btn_row.append(&btn_remove);
    pl_btn_row.append(&btn_clear_all);
    pl_btn_row.append(&btn_cancel);

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

    // ── Left-click on the logo → open settings window ────────────────────────
    {
        let state_rc = state.clone();
        let win_wk = window.downgrade();
        let provider_for_lclick = provider_for_settings.clone();
        let text_rgba_for_lclick = text_rgba.clone();
        let accent_rgba_for_lclick = accent_rgba.clone();
        let rebuild_pl_holder_lclick = rebuild_pl_holder.clone();
        let lclick = GestureClick::new();
        lclick.set_button(1); // primary button only
        lclick.connect_released(move |_, _, _, _| {
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
        });
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
    #[allow(deprecated)]
    pl_view.selection().set_mode(gtk4::SelectionMode::Multiple);

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
    pl_root.append(&pl_scroll);

    // ── Playlist window status bar ────────────────────────────────────────────
    let pl_status_label = Label::builder()
        .label("")
        .halign(Align::Start)
        .css_classes(["status-label"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    pl_status_label.set_margin_start(8);
    pl_status_label.set_margin_end(8);
    pl_status_label.set_margin_bottom(4);
    pl_root.append(&pl_status_label);

    // ── Playlist button bar: Add / Remove (pinned to the bottom) ─────────────
    // Mirrors the layout of classic Winamp where the playlist action buttons
    // sit below the track list rather than above it.
    pl_root.append(&Separator::new(Orientation::Horizontal));
    pl_root.append(&pl_btn_row);

    playlist_win.set_child(Some(&pl_root));

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

    // rebuild_playlist — repopulate the ListStore from the current playlist model.
    //
    // The TreeView is temporarily disconnected from the model while the store is
    // cleared and repopulated.  This prevents the TreeView from processing one
    // row-deleted / row-inserted signal per track (which would block the UI for
    // several seconds on a 30k-track playlist).  Reconnecting the model triggers
    // a single bulk re-read; only visible rows are painted, so it remains O(1).
    let rebuild_playlist = {
        let state = state.clone();
        let pl_store = pl_store.clone();
        let pl_view = pl_view.clone();
        let pl_count_label = pl_count_label.clone();
        let pl_active_idx = pl_active_idx.clone();
        let accent_rgba = accent_rgba.clone();
        let text_rgba = text_rgba.clone();
        Rc::new(move || {
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
                let lock_suffix = if t.read_only { " 🔒" } else { "" };
                let pos = format!("{}.{}", i + 1, lock_suffix);
                let name = t.display_name();
                let is_active = is_playing && i == current;
                let display = if t.broken {
                    format!("⚠ {}", name)
                } else if is_active {
                    format!("▶ {}", name)
                } else {
                    name
                };
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
        })
    };
    *rebuild_pl_holder.borrow_mut() = Some(rebuild_playlist.clone());

    // scroll_to_row_if_needed — scroll the playlist to make a row visible.
    //
    // Uses TreeView::visible_range + scroll_to_cell so that GTK's actual
    // rendered row heights drive the math rather than a hardcoded estimate.
    // A hardcoded estimate drifts after many skips and the row stops scrolling
    // into view.
    let scroll_to_row_if_needed = {
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
    let patch_pl_row = {
        let state = state.clone();
        let pl_store = pl_store.clone();
        let pl_active_idx = pl_active_idx.clone();
        let accent_rgba = accent_rgba.clone();
        let text_rgba = text_rgba.clone();
        Rc::new(move |idx: usize| {
            let (display, duration_str, weight, is_active) = {
                let s = state.borrow();
                let Some(t) = s.playlist.tracks.get(idx) else {
                    return;
                };
                let name = t.display_name();
                let is_playing = matches!(
                    *s.player.state(),
                    PlayerState::Playing | PlayerState::Paused
                );
                let is_active = is_playing && idx == s.playlist.current_index;
                let display = if t.broken {
                    format!("⚠ {}", name)
                } else if is_active {
                    format!("▶ {}", name)
                } else {
                    name
                };
                let weight: i32 = if is_active { 700 } else { 400 };
                (display, fmt_duration(t.duration), weight, is_active)
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
            // Update name, duration, weight, and foreground color columns.
            #[allow(deprecated)]
            pl_store.set(
                &iter,
                &[
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
    fn scan_current_track_metadata(
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

    // play_and_update — play the current track and refresh the UI labels.
    //
    // All "start playing" paths (buttons, keyboard, auto-advance) funnel
    // through here so the marquee and playlist stay in sync.  Label text is
    // NOT set directly here; the 100 ms tick loop renders the marquee window
    // each frame so the scrolling starts immediately after track change.
    let play_and_update = {
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
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
                // Scroll to make the new current track visible
                scroll_to_row_if_needed(new_idx);
                // Patch the new current track to ensure active styling is applied.
                // Also patch old track if it was different.
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
            }
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
    let remove_selected = {
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

    // ── Initial state ─────────────────────────────────────────────────────────

    rebuild_playlist();
    {
        let s = state.borrow();
        if let Some(t) = s.playlist.current() {
            set_track(&t.display_name());
        }
    }

    // ── DragSource on the TreeView ──────────────────────────────────────────
    // Persistent multi-selection snapshot for the active playlist.
    // Updated by selection.connect_changed whenever count > 1.
    // Cleared only on count==0 AND when a drag isn't in progress —
    // GtkTreeView transiently drops to 0 selected rows during the drag
    // event chain, and clearing then would wipe the snapshot before
    // the drop target gets a chance to read it.
    let pl_drag_selection: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let pl_drag_active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    {
        let snap_obs = pl_drag_selection.clone();
        let active_obs = pl_drag_active.clone();
        #[allow(deprecated)]
        pl_view.selection().connect_changed(move |sel| {
            #[allow(deprecated)]
            let count = sel.count_selected_rows() as usize;
            if count > 1 {
                #[allow(deprecated)]
                let (paths, _model) = sel.selected_rows();
                let v: Vec<usize> = paths.iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                *snap_obs.borrow_mut() = v;
            } else if count == 0 && !active_obs.get() {
                snap_obs.borrow_mut().clear();
            } else {
            }
        });
    }

    // Press-time selection restorer — re-applies the multi-select
    // visually so the drag-icon shows every dragged row's highlight,
    // not just the row under the cursor.  GTK's default press handler
    // collapses selection to the clicked row; we schedule an idle
    // restore that runs after that collapse but before drag-icon
    // rendering settles.
    {
        let press = GestureClick::new();
        press.set_button(gtk4::gdk::BUTTON_PRIMARY);
        let pl_view_p = pl_view.clone();
        let snap = pl_drag_selection.clone();
        let active_press = pl_drag_active.clone();
        press.connect_pressed(move |_, _n, x, y| {
            #[allow(deprecated)]
            let row_under = pl_view_p.path_at_pos(x as i32, y as i32)
                .and_then(|(p, _, _, _)| p)
                .and_then(|p| p.indices().first().copied())
                .map(|i| i as usize);
            let snapshot = snap.borrow().clone();
            if snapshot.len() > 1 && row_under.map_or(false, |r| snapshot.contains(&r)) {
                let pv = pl_view_p.clone();
                let snap_c = snapshot.clone();
                glib::idle_add_local_once(move || {
                    #[allow(deprecated)]
                    let selection = pv.selection();
                    #[allow(deprecated)]
                    selection.unselect_all();
                    #[allow(deprecated)]
                    if let Some(model) = pv.model() {
                        for idx in &snap_c {
                            if let Some(iter) = model.iter_nth_child(None, *idx as i32) {
                                #[allow(deprecated)]
                                selection.select_iter(&iter);
                            }
                        }
                    }
                });
            } else if !active_press.get() {
                // Click landed outside the multi-selection (and no drag is in
                // progress) → forget the snapshot, so a later click on a row
                // that used to be selected doesn't resurrect the whole group.
                snap.borrow_mut().clear();
            }
        });
        pl_view.add_controller(press);
    }
    // Emits a FileList of every currently-selected row's path so the drag
    // is consumable by both the active-playlist internal reorder target
    // and external destinations like sidebar pl rows / GNOME Files.  Uses
    // the press-time snapshot when GTK has already collapsed selection to
    // a single row.
    {
        let drag_src = DragSource::new();
        drag_src.set_actions(gdk::DragAction::COPY);
        let pl_view_ds = pl_view.clone();
        let state_ds   = state.clone();
        let drag_sel_ds = pl_drag_selection.clone();
        // Flip drag_active on drag begin / end so the selection-changed
        // observer doesn't wipe the snapshot during the drag chain.
        {
            let active = pl_drag_active.clone();
            drag_src.connect_drag_begin(move |_, _| {
                active.set(true);
            });
        }
        {
            let active = pl_drag_active.clone();
            drag_src.connect_drag_end(move |_, _, _| {
                active.set(false);
            });
        }
        {
            let active = pl_drag_active.clone();
            drag_src.connect_drag_cancel(move |_, _, _| {
                active.set(false);
                false
            });
        }
        let drag_active_pp = pl_drag_active.clone();
        drag_src.connect_prepare(move |_, x, y| {
            // Flip drag_active up-front — selection-changed events
            // between prepare and drag-begin would otherwise wipe the
            // snapshot before drop reads it.
            drag_active_pp.set(true);
            #[allow(deprecated)]
            let row_under = match pl_view_ds.path_at_pos(x as i32, y as i32) {
                Some((Some(p), _, _, _)) => p.indices().first().copied().map(|i| i as usize),
                _ => None,
            };
            // Prefer the connect_changed snapshot (multi-select); fall back
            // to the live selection; final fallback is the row under cursor.
            let snapshot = drag_sel_ds.borrow().clone();
            let sel_indices: Vec<usize> = if snapshot.len() > 1
                && row_under.map_or(false, |r| snapshot.contains(&r))
            {
                snapshot
            } else {
                #[allow(deprecated)]
                let (selected_paths, _model) = pl_view_ds.selection().selected_rows();
                let live: Vec<usize> = selected_paths
                    .iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                if !live.is_empty() { live }
                else { row_under.into_iter().collect() }
            };
            // Stash final source indices so the drop target can do a
            // precise reorder without round-tripping through paths.
            *drag_sel_ds.borrow_mut() = sel_indices.clone();
            let s = state_ds.borrow();
            let paths: Vec<std::path::PathBuf> = sel_indices.iter()
                .filter_map(|i| s.playlist.tracks.get(*i))
                .map(|t| t.path.clone())
                .collect();
            if paths.is_empty() { return None }
            let files: Vec<gio::File> = paths.iter()
                .map(|p| gio::File::for_path(p))
                .collect();
            let fl = gdk::FileList::from_array(&files);
            Some(gdk::ContentProvider::for_value(&fl.to_value()))
        });
        pl_view.add_controller(drag_src);
    }

    // Active-playlist internal reorder via FileList drop on pl_view.
    // Source indices are recovered by looking up each dropped path in the
    // current playlist; any path not found is treated as a new track and
    // appended (so cross-window drops from ML/editor also work here).
    {
        let drop_tgt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let state_dnd = state.clone();
        let rebuild_dnd = rebuild_playlist.clone();
        let pl_view_dnd = pl_view.clone();
        let drag_sel_drop = pl_drag_selection.clone();

        drop_tgt.connect_drop(move |_, value, x, y| {
            let file_list = match value.get::<gdk::FileList>() {
                Ok(fl) => fl,
                Err(_) => {
                    return false;
                }
            };
            let dropped: Vec<std::path::PathBuf> = file_list.files().iter()
                .filter_map(|f| f.path())
                .collect();
            if dropped.is_empty() { return false }

            let n = state_dnd.borrow().playlist.len();
            // Use dest_row_at_pos so the drop position honors the
            // before/after halves of the target row — dropping on the
            // bottom half of row 13 inserts at 14, top half at 13.
            // Without this, dropping [10,11,12,13] onto row 14 always
            // computed insert_at=10 (no visible move).
            #[allow(deprecated)]
            let dst_pos = match pl_view_dnd.dest_row_at_pos(x as i32, y as i32) {
                Some((Some(p), drop_pos)) => {
                    let row = p.indices().first().copied().unwrap_or(0) as usize;
                    match drop_pos {
                        gtk4::TreeViewDropPosition::Before
                        | gtk4::TreeViewDropPosition::IntoOrBefore => row,
                        gtk4::TreeViewDropPosition::After
                        | gtk4::TreeViewDropPosition::IntoOrAfter => row + 1,
                        _ => row,
                    }
                }
                _ => n,
            };

            // Prefer the press-time drag selection snapshot — it's the
            // authoritative source-row list and avoids the path-comparison
            // mismatch that round-trips through gio::File.  Empty snapshot
            // means the drop came from another window, so fall back to
            // path matching to decide reorder-vs-add.
            let snapshot: Vec<usize> = drag_sel_drop.borrow().clone();
            let mut existing_src_indices: Vec<usize>;
            let mut new_paths: Vec<std::path::PathBuf>;
            if !snapshot.is_empty() {
                existing_src_indices = snapshot;
                new_paths = Vec::new();
            } else {
                existing_src_indices = Vec::new();
                new_paths = Vec::new();
                let s = state_dnd.borrow();
                for dp in &dropped {
                    let idx = s.playlist.tracks.iter().position(|t| &t.path == dp);
                    match idx {
                        Some(i) => existing_src_indices.push(i),
                        None    => new_paths.push(dp.clone()),
                    }
                }
            }

            let did_move = !existing_src_indices.is_empty();
            // Capture the post-move range so the idle rebuild below can
            // re-select the moved rows — without this, the drop appears
            // to clear selection and the user can't see what was moved.
            let mut moved_range: Option<(usize, usize)> = None;
            if did_move {
                let mut s = state_dnd.borrow_mut();
                // Remove highest-first so earlier removes don't invalidate later indices.
                let mut sorted = existing_src_indices.clone();
                sorted.sort_unstable_by(|a, b| b.cmp(a));
                let mut adjusted_dst = dst_pos;
                let mut removed: Vec<crate::model::Track> = Vec::new();
                for src in sorted.iter() {
                    if *src < s.playlist.tracks.len() {
                        let t = s.playlist.tracks.remove(*src);
                        if *src < adjusted_dst { adjusted_dst -= 1; }
                        removed.push(t);
                    }
                }
                // Reinsert in original drop order at adjusted_dst.
                removed.reverse();
                let cap = s.playlist.tracks.len();
                let insert_at = adjusted_dst.min(cap);
                let removed_n = removed.len();
                for (i, t) in removed.into_iter().enumerate() {
                    s.playlist.tracks.insert(insert_at + i, t);
                }
                moved_range = Some((insert_at, removed_n));
            }

            let mut did_add = false;
            for p in new_paths {
                if state_dnd.borrow_mut().add_path(&p).is_ok() { did_add = true; }
            }
            // Clear the press-time selection snapshot so a subsequent
            // single-row drag doesn't accidentally reorder the whole set.
            drag_sel_drop.borrow_mut().clear();
            if did_move || did_add {
                // Defer to next idle tick — splicing the model while GTK
                // is still unwinding the drop event can segfault.
                let rb = rebuild_dnd.clone();
                let pv = pl_view_dnd.clone();
                let snap_restore = drag_sel_drop.clone();
                glib::idle_add_local_once(move || {
                    rb();
                    // Restore selection to the moved range so the user
                    // sees what was just reordered.
                    if let Some((start, n)) = moved_range {
                        #[allow(deprecated)]
                        let selection = pv.selection();
                        #[allow(deprecated)]
                        selection.unselect_all();
                        let mut restored: Vec<usize> = Vec::new();
                        #[allow(deprecated)]
                        if let Some(model) = pv.model() {
                            for i in 0..n {
                                let row_idx = start + i;
                                if let Some(iter) = model.iter_nth_child(None, row_idx as i32) {
                                    #[allow(deprecated)]
                                    selection.select_iter(&iter);
                                    restored.push(row_idx);
                                }
                            }
                        }
                        // Re-seed the multi-select snapshot so a follow-up
                        // drag of the same rows works without redoing the
                        // shift-click sequence.
                        if restored.len() > 1 {
                            *snap_restore.borrow_mut() = restored;
                        }
                    }
                });
            }
            true
        });

        pl_view.add_controller(drop_tgt);
    }

    // ── Drop target: accept files dragged from an external file manager ───────
    // Handles gdk::FileList drops (the standard type produced by GNOME Files
    // and most GTK4-aware file managers).  Files are appended to the playlist;
    // directories are scanned recursively.  Attached to the ScrolledWindow so
    // the full visible playlist area is a valid drop zone.
    {
        let file_drop = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let state_fd = state.clone();
        let rebuild_fd = rebuild_playlist.clone();
        let status_fd = pl_status_label.clone();
        let probe_tx_fd = probe_tx.clone();
        let broken_tx_fd = broken_tx.clone();
        file_drop.connect_drop(move |_, value, _, _| {
            let file_list = match value.get::<gdk::FileList>() {
                Ok(fl) => fl,
                Err(_) => return false,
            };
            let before = state_fd.borrow().playlist.tracks.len();
            let mut added = 0usize;
            for file in file_list.files() {
                if let Some(path) = file.path() {
                    if state_fd.borrow_mut().add_path(&path).is_ok() {
                        added += 1;
                    }
                }
            }
            if added > 0 {
                status_fd.set_text(&format!(
                    "Dropped {} file{}",
                    added,
                    if added == 1 { "" } else { "s" }
                ));
                rebuild_fd();
                let paths = state_fd.borrow().uncached_paths_from(before);
                if !paths.is_empty() {
                    duration_probe::spawn_probes(paths, probe_tx_fd.clone(), broken_tx_fd.clone());
                }
            }
            added > 0
        });
        pl_scroll.add_controller(file_drop);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Transport button callbacks
    // ══════════════════════════════════════════════════════════════════════════

    // ▶ Play / resume.
    btn_play.connect_clicked({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        move |_| {
            let ps = state.borrow().player.state().clone();
            match ps {
                PlayerState::Stopped => play_and_update(),
                PlayerState::Paused => {
                    let _ = state.borrow_mut().player.toggle_pause();
                }
                PlayerState::Playing => {}
            }
        }
    });

    // ⏸ Pause / resume toggle.
    btn_pause.connect_clicked({
        let state = state.clone();
        move |_| {
            let _ = state.borrow_mut().player.toggle_pause();
        }
    });

    // ⏹ Stop.
    btn_stop.connect_clicked({
        let state = state.clone();
        let seek_bar = seek_bar.clone();
        let patch_pl_row = patch_pl_row.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            let _ = state.borrow_mut().player.stop();
            seek_bar.set_value(0.0);
            // Remove the bold/arrow from the now-stopped track.
            patch_pl_row(old_idx);
        }
    });

    // ⏭ Next track.
    btn_next.connect_clicked({
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            if let Some(display) = { state.borrow_mut().play_next() } {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                scroll_to_row_if_needed(new_idx);
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
            }
        }
    });

    // ⏮ Previous / restart (PRD back-button logic).
    btn_prev.connect_clicked({
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            if let Some(display) = { state.borrow_mut().play_prev() } {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                scroll_to_row_if_needed(new_idx);
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
            }
        }
    });

    // 🔁 Repeat — cycle Off → Song → Playlist → Off.
    // Updates the button label and tooltip immediately so the user can see
    // the current mode without opening the help window.
    btn_repeat.connect_clicked({
        let state = state.clone();
        let btn_repeat = btn_repeat.clone();
        let repeat_icon = repeat_icon.clone();
        let repeat_label = repeat_label.clone();
        move |_| {
            let new_mode = {
                let mut s = state.borrow_mut();
                let m = s.config.playback.repeat_mode.cycle();
                s.config.playback.repeat_mode = m;
                m
            };
            // Update both icon and text so the button matches macOS visuals.
            repeat_icon.set_icon_name(Some(repeat_btn_icon(new_mode)));
            repeat_label.set_text(repeat_btn_text(new_mode));
            // Highlight with accent class when not off.
            if new_mode == crate::shuffle::RepeatMode::Off {
                btn_repeat.remove_css_class("mode-btn-active");
            } else {
                btn_repeat.add_css_class("mode-btn-active");
            }
        }
    });

    // 🔀 Shuffle — toggle on/off; accent-highlighted when on.
    btn_shuffle.connect_clicked({
        let state = state.clone();
        let btn_shuffle = btn_shuffle.clone();
        move |_| {
            let enabled = {
                let mut s = state.borrow_mut();
                s.shuffle_state.toggle();
                // Reset the shuffle history so the new setting takes effect cleanly.
                s.shuffle_state.reset();
                let on = s.shuffle_state.enabled;
                // Mirror to config so the setting survives to the next session.
                s.config.playback.shuffle_enabled = on;
                on
            };
            if enabled {
                btn_shuffle.add_css_class("mode-btn-active");
            } else {
                btn_shuffle.remove_css_class("mode-btn-active");
            }
        }
    });

    // PL — toggle the playlist window.
    btn_pl.connect_clicked({
        let playlist_win = playlist_win.clone();
        let state = state.clone();
        move |_| {
            if playlist_win.is_visible() {
                let (w, h) = (playlist_win.width(), playlist_win.height());
                {
                    let mut s = state.borrow_mut();
                    s.config.window.playlist_width = w;
                    s.config.window.playlist_height = h;
                }
                let _ = state.borrow().config.save();
            }
            playlist_win.set_visible(!playlist_win.is_visible());
        }
    });

    // ℹ Info button — connected after handle_key is defined (see below).

    // ══════════════════════════════════════════════════════════════════════════
    // Playlist TreeView interactions
    // ══════════════════════════════════════════════════════════════════════════

    // Double-click / Enter on a row: jump to that track and start playback.
    #[allow(deprecated)]
    pl_view.connect_row_activated({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        move |_, path, _| {
            if let Some(&idx) = path.indices().first() {
                // Record the previously-playing track before changing current_index
                // so we can de-highlight it after the jump.
                let old_idx = state.borrow().playlist.current_index;
                state.borrow_mut().playlist.jump_to(idx as usize);
                play_and_update();
                if old_idx != idx as usize {
                    patch_pl_row(old_idx);
                }
            }
        }
    });

    // Right-click context menu on a row: Play / View+Edit ID3 / Remove.
    // NOTE: Attached to ScrolledWindow instead of TreeView to work around GTK4 bug
    // where PopoverMenu doesn't receive hover events when attached directly to TreeView.
    {
        let ctx_click = GestureClick::new();
        ctx_click.set_button(3); // right mouse button
        // Capture phase pre-empts GtkTreeView's default Bubble-phase right-
        // click handler, which otherwise clears the multi-selection before
        // our `path_is_selected` guard sees it.  Claimed state at the end
        // prevents the default handler from running afterward.
        ctx_click.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let pl_view_ctx = pl_view.clone();
        let pl_scroll_ctx = pl_scroll.clone();

        // Create action group and attach to the ScrolledWindow (not TreeView)
        let pl_action_group = gio::SimpleActionGroup::new();
        pl_scroll_ctx.insert_action_group("pl", Some(&pl_action_group));

        // Store the current row index for the action handlers
        let current_row: Rc<RefCell<Option<i64>>> = Rc::new(RefCell::new(None));

        // Register playlist menu actions in the action group
        let action_play = gio::SimpleAction::new("play", None); // Short name
        let state_play = state.clone();
        let play_callback = play_and_update.clone();
        let patch_callback = patch_pl_row.clone();
        let scroll_callback = scroll_to_row_if_needed.clone();
        let row_play = current_row.clone();
        action_play.connect_activate(move |_, _| {
            let row_idx = *row_play.borrow();
            if let Some(idx) = row_idx {
                let idx = idx as usize;
                let old_idx = state_play.borrow().playlist.current_index;
                state_play.borrow_mut().playlist.jump_to(idx);
                play_callback();
                scroll_callback(idx);
                if old_idx != idx {
                    patch_callback(old_idx);
                }
            }
        });
        pl_action_group.add_action(&action_play);

        let action_id3 = gio::SimpleAction::new("edit-id3", None); // Short name
        let state_id3 = state.clone();
        let win_id3 = window.downgrade();
        let rebuild_id3 = rebuild_playlist.clone();
        let row_id3 = current_row.clone();
        action_id3.connect_activate(move |_, _| {
            let row_idx = *row_id3.borrow();
            if let Some(idx) = row_idx {
                let path = state_id3
                    .borrow()
                    .playlist
                    .tracks
                    .get(idx as usize)
                    .map(|t| t.path.clone());
                if let Some(path) = path {
                    open_id3_editor_window(
                        win_id3.upgrade().as_ref(),
                        path,
                        state_id3.clone(),
                        rebuild_id3.clone(),
                        None,
                    );
                }
            }
        });
        pl_action_group.add_action(&action_id3);

        let action_remove = gio::SimpleAction::new("remove", None); // Short name
        let remove_callback = remove_selected.clone();
        action_remove.connect_activate(move |_, _| {
            remove_callback();
        });
        pl_action_group.add_action(&action_remove);

        // Seed a brand new saved playlist from the current selection.
        // Opens the standard playlist save dialog so the user picks the
        // filename + folder; the resulting M3U8 contains EXTINF metadata
        // for every selected active-playlist row.
        let action_add_to_new = gio::SimpleAction::new("add-to-new", None);
        {
            let state_atn  = state.clone();
            let pl_view_atn = pl_view.clone();
            let win_atn    = playlist_win.clone();
            action_add_to_new.connect_activate(move |_, _| {
                #[allow(deprecated)]
                let (sel_paths, _) = pl_view_atn.selection().selected_rows();
                let indices: Vec<usize> = sel_paths.iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                let paths: Vec<String> = {
                    let s = state_atn.borrow();
                    indices.iter()
                        .filter_map(|i| s.playlist.tracks.get(*i))
                        .map(|t| t.path.to_string_lossy().into_owned())
                        .collect()
                };
                if paths.is_empty() { return }
                let default_stem = glib::DateTime::now_local()
                    .ok()
                    .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Playlist".to_string());
                let state_cb = state_atn.clone();
                run_playlist_save_dialog(
                    state_atn.clone(),
                    win_atn.clone(),
                    &default_stem,
                    move |path, win_cb| {
                        if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                            if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths) {
                                eprintln!("save_playlist_tracks_to_path: {e}");
                                show_playlist_save_error(&win_cb, &path, &e);
                            }
                        }
                        notify_playlist_nav_refresh();
                    },
                );
            });
        }
        pl_action_group.add_action(&action_add_to_new);

        // Add selection to a saved playlist (parameterised by id).
        // Multi-select aware: pulls every selected row from the active
        // playlist and appends their paths to the chosen saved playlist.
        let state_add_pl = state.clone();
        let pl_view_add  = pl_view.clone();
        let action_add_to_saved = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action_add_to_saved.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            #[allow(deprecated)]
            let (paths_models, _model) = pl_view_add.selection().selected_rows();
            let indices: Vec<i64> = paths_models
                .iter()
                .filter_map(|p| p.indices().first().copied())
                .map(|i| i as i64)
                .collect();
            let paths: Vec<String> = {
                let s = state_add_pl.borrow();
                indices.iter()
                    .filter_map(|i| s.playlist.tracks.get(*i as usize))
                    .map(|t| t.path.to_string_lossy().into_owned())
                    .collect()
            };
            if paths.is_empty() { return }
            let mut ok = false;
            if let Some(lib) = state_add_pl.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_)  => ok = true,
                    Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                }
            }
            if ok { notify_playlist_changed(pid); }
        });
        pl_action_group.add_action(&action_add_to_saved);

        // Send to Disc Drive: probe-on-add, then queue onto THAT drive.
        // Same body as the Media Library's ml.send-drive (media_library.rs)
        // — shares burn_queues so the burn panel sees items queued here.
        {
            let state_burn = state.clone();
            let pl_view_drv = pl_view.clone();
            let burn_queues = burn_queues.clone();
            let burn_refresh_holder = burn_refresh_holder.clone();
            let current_drives = current_drives.clone();
            let status = pl_status_label.clone();
            let win_wk: glib::WeakRef<gtk4::Window> =
                playlist_win.clone().upcast::<gtk4::Window>().downgrade();
            let action = gio::SimpleAction::new(
                "send-drive",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(drive_id) =
                    target.and_then(|v| v.get::<String>()) else { return };
                let drive_label = current_drives
                    .borrow()
                    .iter()
                    .find(|d| d.id == drive_id)
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| drive_id.clone());
                #[allow(deprecated)]
                let (sel_paths, _) = pl_view_drv.selection().selected_rows();
                let indices: Vec<usize> = sel_paths
                    .iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                let paths: Vec<std::path::PathBuf> = {
                    let s = state_burn.borrow();
                    indices.iter()
                        .filter_map(|i| s.playlist.tracks.get(*i))
                        .map(|t| t.path.clone())
                        .collect()
                };
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
            pl_action_group.add_action(&action);
        }

        // Send to Removable Device: self-contained — copies straight onto the
        // device using the same core plan/copy helpers as the Media Library's
        // copy_files_run (device_plan_fs / device_recorded_relpath /
        // device_record_pair / devices::io::for_device), but without any of
        // that runner's ML-only widgets (sidebar row, progress bar, eject
        // button — copy_files_run captures those directly and they don't
        // exist until the ML window has been built). That coupling is why
        // this path previously went through copy_files_holder and silently
        // did nothing when the holder was still empty: the device already
        // shows up in the menu via the app-start device poll, independent of
        // the ML window. Progress goes to the playlist status label and the
        // result to an alert on the playlist window, mirroring pl.send-drive.
        {
            let current_devices = current_devices.clone();
            let pl_view_dev = pl_view.clone();
            let state_dev = state.clone();
            let status = pl_status_label.clone();
            let win_wk = playlist_win.downgrade();
            let action = gio::SimpleAction::new(
                "send-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) =
                    target.and_then(|v| v.get::<String>()) else { return };
                let Some(dev) = current_devices
                    .borrow()
                    .iter()
                    .find(|d| d.id == dev_id)
                    .cloned()
                else { return };
                #[allow(deprecated)]
                let (sel_paths, _) = pl_view_dev.selection().selected_rows();
                let indices: Vec<usize> = sel_paths
                    .iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                let paths: Vec<std::path::PathBuf> = {
                    let s = state_dev.borrow();
                    indices.iter()
                        .filter_map(|i| s.playlist.tracks.get(*i))
                        .map(|t| t.path.clone())
                        .collect()
                };
                let paths: Vec<std::path::PathBuf> =
                    paths.into_iter().filter(|p| p.exists()).collect();
                if paths.is_empty() {
                    return;
                }
                let win_for_alert = || win_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
                if dev.read_only {
                    let n = if dev.label.is_empty() { "This device" } else { &dev.label };
                    show_alert_parented(
                        win_for_alert().as_ref(),
                        &format!("{n} is read-only — can't copy files to it."),
                    );
                    return;
                }
                if device_fs_unsupported(&dev.fs_type) {
                    show_alert_parented(
                        win_for_alert().as_ref(),
                        &format!(
                            "{} is an unsupported filesystem — can't write to this device yet.",
                            dev.fs_type
                        ),
                    );
                    return;
                }
                let device_id = device_sync_id(&dev);
                let mount = dev.mount_path.clone();
                // Free-space guard — only when capacity is known (mirrors
                // copy_files_run: skips slow per-file device checks on
                // devices that can't report capacity, e.g. MTP).
                if dev.free_bytes > 0 {
                    let mut need = 0u64;
                    for p in &paths {
                        if !device_plan_one(&state_dev, &mount, &device_id, p).1 {
                            need += std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                        }
                    }
                    if need > dev.free_bytes {
                        show_alert_parented(
                            win_for_alert().as_ref(),
                            &format!(
                                "Not enough space on the device: need {:.1} GB, {:.1} GB free.",
                                need as f64 / 1e9,
                                dev.free_bytes as f64 / 1e9
                            ),
                        );
                        return;
                    }
                }
                let dname = if dev.label.is_empty() { "device".to_string() } else { dev.label.clone() };
                let total = paths.len();
                let dev_for_copy = dev.clone();
                let state2 = state_dev.clone();
                let status2 = status.clone();
                let win2 = win_wk.clone();
                status.set_text("Reading files…");
                // Same spawn_future_local + spawn_blocking pattern as
                // pl.send-drive above: only Send data (paths, dev, ids)
                // crosses into spawn_blocking; the Rcs stay in the local
                // future, and the state borrow inside each step is short-lived.
                glib::spawn_future_local(async move {
                    let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                    for (i, src) in paths.iter().enumerate() {
                        status2.set_text(&gtk_safe(&format!(
                            "Copying {}/{total} to {dname}…", i + 1
                        )));
                        let recorded = device_recorded_relpath(&state2, &device_id, src);
                        let s = src.clone();
                        let m = mount.clone();
                        let dc = dev_for_copy.clone();
                        let joined = gio::spawn_blocking(
                            move || -> Result<(std::path::PathBuf, bool), ()> {
                                let (rel, present) = device_plan_fs(&m, &s, recorded);
                                if present {
                                    return Ok((rel, false)); // already there
                                }
                                match crate::devices::io::for_device(&dc)
                                    .copy_to_device(&s, &rel)
                                {
                                    Ok(_) => Ok((rel, true)),
                                    Err(_) => Err(()),
                                }
                            },
                        )
                        .await;
                        match joined {
                            Ok(Ok((rel, copied_now))) => {
                                if copied_now {
                                    copied += 1;
                                } else {
                                    skipped += 1;
                                }
                                device_record_pair(&state2, &device_id, src, &rel);
                            }
                            _ => failed += 1,
                        }
                    }
                    status2.set_text(&gtk_safe(&format!(
                        "Copied {copied}, skipped {skipped}, failed {failed} to {dname}."
                    )));
                    show_alert_parented(
                        win2.upgrade().map(|w| w.upcast::<gtk4::Window>()).as_ref(),
                        &gtk_safe(&format!(
                            "Copied {copied}, skipped {skipped}, failed {failed} to {dname}."
                        )),
                    );
                });
            });
            pl_action_group.add_action(&action);
        }

        let state_menu_pl = state.clone();
        let current_drives_ctx = current_drives.clone();
        let current_devices_ctx = current_devices.clone();
        let pl_drag_sel_ctx = pl_drag_selection.clone();
        ctx_click.connect_pressed(move |gest, _, x, y| {
            #[allow(deprecated)]
            let row_idx = match pl_view_ctx.path_at_pos(x as i32, y as i32) {
                Some((Some(path), _, _, _)) => match path.indices().first().copied() {
                    Some(i) => i as i64,
                    None => return,
                },
                _ => return,
            };

            // Store the row index for the action handlers
            *current_row.borrow_mut() = Some(row_idx);

            // Restore the press-time multi-selection snapshot when the
            // clicked row was part of a prior multi-select.  GtkTreeView
            // (deprecated) collapses selection to the clicked row on
            // secondary-button press even when our Capture-phase
            // handler claims the event, so we explicitly re-select the
            // snapshot rows here.
            let snapshot = pl_drag_sel_ctx.borrow().clone();
            let row_idx_u = row_idx as usize;
            let should_restore = snapshot.len() > 1 && snapshot.contains(&row_idx_u);

            #[allow(deprecated)]
            let path = gtk4::TreePath::from_indices(&[row_idx as i32]);
            #[allow(deprecated)]
            let already_selected = pl_view_ctx.selection().path_is_selected(&path);
            if should_restore {
                #[allow(deprecated)]
                pl_view_ctx.selection().unselect_all();
                let model = pl_view_ctx.model();
                #[allow(deprecated)]
                if let Some(model) = model {
                    for idx in &snapshot {
                        if let Some(iter) = model.iter_nth_child(None, *idx as i32) {
                            #[allow(deprecated)]
                            pl_view_ctx.selection().select_iter(&iter);
                        }
                    }
                }
            } else if !already_selected {
                #[allow(deprecated)]
                pl_view_ctx.selection().unselect_all();
                #[allow(deprecated)]
                if let Some(iter) = pl_view_ctx
                    .model()
                    .and_then(|m| m.iter_nth_child(None, row_idx as i32))
                {
                    #[allow(deprecated)]
                    pl_view_ctx.selection().select_iter(&iter);
                }
            }

            // Number of currently-selected rows — drives single-only menu
            // items (Edit ID3 is hidden in multi-select since the editor
            // can only bind to one track at a time).
            #[allow(deprecated)]
            let sel_count = pl_view_ctx.selection().count_selected_rows() as usize;

            // Build menu model with prefixed action names
            let menu = gio::Menu::new();
            menu.append_item(&gio::MenuItem::new(Some("▶ Play"), Some("pl.play")));
            if sel_count <= 1 {
                menu.append_item(&gio::MenuItem::new(
                    Some("🎵 View / Edit ID3"),
                    Some("pl.edit-id3"),
                ));
            }
            menu.append_item(&gio::MenuItem::new(Some("✕ Remove"), Some("pl.remove")));
            let send = build_send_to_menu(
                &state_menu_pl,
                &SendToActions {
                    active: "", // tracks are already in the active playlist
                    new_playlist: "pl.add-to-new",
                    saved_playlist: "pl.add-to-saved",
                    drive: "pl.send-drive",
                    device: "pl.send-device",
                    drives: current_drives_ctx.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    devices: current_devices_ctx.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                },
            );
            menu.append_submenu(Some("Send to"), &send);

            // Create popover menu — NESTED keeps the Add-to-Playlist
            // submenu from being clipped to the parent menu's height.
            let popover = gtk4::PopoverMenu::from_model_full(
                &menu,
                gtk4::PopoverMenuFlags::NESTED,
            );
            popover.set_parent(&pl_scroll_ctx);
            let rect = gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
            gest.set_state(gtk4::EventSequenceState::Claimed);
        });
        #[allow(deprecated)]
        pl_view.add_controller(ctx_click);
    }

    // Selection change: show a status hint when a broken track is selected.
    #[allow(deprecated)]
    pl_view.selection().connect_changed({
        let state     = state.clone();
        let pl_status = pl_status_label.clone();
        let pl_view_sc = pl_view.clone();
        move |_| {
            // set_model(None) during a bulk rebuild fires this signal with a null
            // model; selected_rows() would then panic.  Bail early if no model.
            #[allow(deprecated)]
            if pl_view_sc.model().is_none() { return; }
            #[allow(deprecated)]
            let (paths, _) = pl_view_sc.selection().selected_rows();
            let Some(path) = paths.first() else {
                pl_status.set_text("");
                return;
            };
            let Some(&idx) = path.indices().first() else {
                pl_status.set_text("");
                return;
            };
            let idx = idx as usize;
            let is_broken = state.borrow().playlist.tracks
                .get(idx)
                .map(|t| t.broken)
                .unwrap_or(false);
            if is_broken {
                let path_hint = state.borrow().playlist.tracks
                    .get(idx)
                    .map(|t| t.path.display().to_string())
                    .unwrap_or_default();
                pl_status.set_text(&format!(
                    "⚠  This file can't be played — it may have been moved, renamed, or deleted.  ({})",
                    path_hint
                ));
            } else {
                pl_status.set_text("");
            }
        }
    });

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
                    );
                }
            }
        });
        title_label.add_controller(click);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Playlist window: Add-file buttons
    // ══════════════════════════════════════════════════════════════════════════

    // Helper: build a FileFilter matching all common audio formats.
    // Used by all three add dialogs to avoid re-creating the filter object.
    let make_audio_filter = || {
        let f = gtk4::FileFilter::new();
        f.set_name(Some("Audio files"));
        // MIME types cover most desktop environments and file managers.
        for mime in &[
            "audio/mpeg",
            "audio/flac",
            "audio/ogg",
            "audio/opus",
            "audio/wav",
            "audio/x-wav",
            "audio/aac",
            "audio/mp4",
            "audio/x-m4a",
            "audio/x-ms-wma",
        ] {
            f.add_mime_type(mime);
        }
        // Extension patterns as fallback for systems without full MIME support.
        for pat in &[
            "*.mp3", "*.flac", "*.ogg", "*.opus", "*.wav", "*.aac", "*.m4a", "*.wma", "*.ape",
            "*.aiff",
        ] {
            f.add_pattern(pat);
        }
        f
    };

    // Cancel button: stops any active playlist scan (Add Folder or Add Files).
    // Wired once here, before the add handlers, so it is always connected.
    btn_cancel.connect_clicked({
        let state = state.clone();
        let pl_status = pl_status_label.clone();
        let cancel_btn = btn_cancel.clone();
        move |_| {
            let s = state.borrow();
            if let Some(ref scan) = s.playlist_scan {
                scan.cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            drop(s);
            pl_status.set_text("Cancelling…");
            cancel_btn.set_visible(false);
        }
    });

    // [+ Files]: open the desktop file browser to pick one or more audio files.
    // For small selections this is near-instant; for large selections it uses the
    // same two-phase background scan as Add Folder to avoid blocking the UI.
    btn_add_files.connect_clicked({
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let pl_status = pl_status_label.clone();
        let window_wk = playlist_win.downgrade();
        let make_filt = make_audio_filter.clone();
        let probe_tx = probe_tx.clone();
        let broken_tx = broken_tx.clone();
        let cancel_btn = btn_cancel.clone();
        let patch_pl_row_af = patch_pl_row.clone();
        let set_track_af = set_track.clone();
        move |_| {
            let dialog = gtk4::FileDialog::builder().title("Add Audio Files").build();
            let filter_store = gio::ListStore::new::<gtk4::FileFilter>();
            filter_store.append(&make_filt());
            dialog.set_filters(Some(&filter_store));

            let state_cb = state.clone();
            let rebuild_cb = rebuild_playlist.clone();
            let status_cb = pl_status.clone();
            let probe_tx_cb = probe_tx.clone();
            let broken_tx_cb = broken_tx.clone();
            let cancel_ref = cancel_btn.clone();
            let patch_cb = patch_pl_row_af.clone();
            let set_track_cb = set_track_af.clone();
            let parent = window_wk.upgrade();
            dialog.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                let Ok(list) = result else { return };

                // Collect selected paths on the main thread before spawning.
                let files: Vec<PathBuf> = (0..list.n_items())
                    .filter_map(|i| list.item(i))
                    .filter_map(|obj| obj.downcast::<gio::File>().ok())
                    .filter_map(|f| f.path())
                    .collect();

                if files.is_empty() {
                    return;
                }

                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                {
                    let mut s = state_cb.borrow_mut();
                    s.playlist_scan = Some(ScanState {
                        scan_type: ScanType::AddFiles,
                        current: 0,
                        total: 0,
                        cancel: cancel.clone(),
                    });
                    s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
                }

                status_cb.set_text("Scanning…");
                cancel_ref.set_visible(true);

                // Capture where the new tracks will start before any are added.
                let scan_start = state_cb.borrow().playlist.len();

                let (fast_tx, fast_rx) = std::sync::mpsc::channel::<crate::model::Track>();
                let (meta_tx, meta_rx) =
                    std::sync::mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = std::sync::mpsc::channel::<usize>();

                crate::model::Playlist::scan_files_for_ui(
                    files,
                    cancel,
                    fast_tx,
                    meta_tx,
                    done_tx,
                    phase1_done_tx,
                );

                start_playlist_scan_poller(
                    state_cb.clone(),
                    status_cb.clone(),
                    rebuild_cb.clone(),
                    cancel_ref.clone(),
                    probe_tx_cb.clone(),
                    broken_tx_cb.clone(),
                    patch_cb.clone(),
                    set_track_cb.clone(),
                    fast_rx,
                    meta_rx,
                    done_rx,
                    phase1_done_rx,
                    scan_start,
                );
            });
        }
    });

    // [⤓ Save] active playlist: open the native Save dialog, write the
    // current queue's track paths to the chosen .m3u8 file via the core
    // helper (which emits #EXTINF lines and registers the playlist in
    // the library), then refresh the sidebar so the new entry appears.
    btn_save_active.connect_clicked({
        let state = state.clone();
        let window_wk = playlist_win.downgrade();
        move |_| {
            let Some(win) = window_wk.upgrade() else { return };
            let paths: Vec<String> = state.borrow().playlist.tracks
                .iter().map(|t| t.path.to_string_lossy().into_owned()).collect();
            if paths.is_empty() { return }
            // Timestamped default name (readable, sortable, no colons).
            // Uses glib's local time so we don't add a chrono dependency.
            let default_stem = glib::DateTime::now_local()
                .ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = state.clone();
            run_playlist_save_dialog(state.clone(), win, &default_stem, move |path, win_cb| {
                if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                    if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths) {
                        eprintln!("save_playlist_tracks_to_path: {e}");
                        show_playlist_save_error(&win_cb, &path, &e);
                    }
                }
                notify_playlist_nav_refresh();
            });
        }
    });

    // [+ Folder]: open the desktop folder browser; recursively add all audio files.
    // Uses the same two-phase scan as Add Files: fast tracks appear immediately,
    // metadata fills in as it is read in the background.
    btn_add_dir.connect_clicked({
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let pl_status = pl_status_label.clone();
        let window_wk = playlist_win.downgrade();
        let probe_tx = probe_tx.clone();
        let broken_tx = broken_tx.clone();
        let cancel_btn = btn_cancel.clone();
        let patch_pl_row_adir = patch_pl_row.clone();
        let set_track_adir = set_track.clone();
        move |_| {
            let dialog = gtk4::FileDialog::new();
            dialog.set_title("Add Folder to Playlist");

            let state_cb = state.clone();
            let rebuild_cb = rebuild_playlist.clone();
            let status_cb = pl_status.clone();
            let probe_tx_cb = probe_tx.clone();
            let broken_tx_cb = broken_tx.clone();
            let cancel_ref = cancel_btn.clone();
            let patch_cb = patch_pl_row_adir.clone();
            let set_track_cb = set_track_adir.clone();
            let parent = window_wk.upgrade();
            dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else { return };
                let Some(folder) = file.path() else { return };

                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                {
                    let mut s = state_cb.borrow_mut();
                    s.playlist_scan = Some(ScanState {
                        scan_type: ScanType::AddFolder,
                        current: 0,
                        total: 0,
                        cancel: cancel.clone(),
                    });
                    s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
                }

                status_cb.set_text("Scanning…");
                cancel_ref.set_visible(true);

                // Capture where the new tracks will start before any are added.
                let scan_start = state_cb.borrow().playlist.len();

                let (fast_tx, fast_rx) = std::sync::mpsc::channel::<crate::model::Track>();
                let (meta_tx, meta_rx) =
                    std::sync::mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = std::sync::mpsc::channel::<usize>();

                crate::model::Playlist::scan_folder_for_ui(
                    folder,
                    cancel,
                    fast_tx,
                    meta_tx,
                    done_tx,
                    phase1_done_tx,
                );

                start_playlist_scan_poller(
                    state_cb.clone(),
                    status_cb.clone(),
                    rebuild_cb.clone(),
                    cancel_ref.clone(),
                    probe_tx_cb.clone(),
                    broken_tx_cb.clone(),
                    patch_cb.clone(),
                    set_track_cb.clone(),
                    fast_rx,
                    meta_rx,
                    done_rx,
                    phase1_done_rx,
                    scan_start,
                );
            });
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
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
    {
        let state = state.clone();
        let time_disp_label = time_disp_label.clone();
        let viz_shutting_down = viz_shutting_down.clone();
        let title_label = title_label.clone();
        let seek_bar = seek_bar.clone();
        let play_update = play_and_update.clone();
        let viz = viz.clone();
        let marquee_chars = marquee_chars.clone();
        let marquee_offset = marquee_offset.clone();
        let marquee_tick = marquee_tick.clone();
        let show_remaining = show_remaining.clone();
        let state_label = state_label.clone();
        let btn_play = btn_play.clone();
        let patch_pl_row = patch_pl_row.clone();
        let current_track_meta_rx = std::cell::RefCell::new(current_track_meta_rx);
        let set_track = set_track.clone();
        let rebuild_playlist_tick = rebuild_playlist.clone();
        let play_update_tick = play_and_update.clone();
        let scroll_tick = scroll_to_row_if_needed.clone();
        // Granite-mode renderer state captured by the tick closure. Weak
        // refs so the timer doesn't keep widgets alive after the main window
        // closes — calling `set_paintable` on a destroyed widget triggers a
        // Gdk-CRITICAL and (on Wayland) a segfault during gsk paint.
        let viz_stack_tick = viz_stack.downgrade();
        let granite_pic_tick = granite_pic.downgrade();
        let granite_buf_tick: std::rc::Rc<std::cell::RefCell<Vec<u8>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        // Last mini-Granite render instant → measured dt (in 30 fps frame
        // units) for the dt-aware sim, so timer jitter never changes the
        // plasma's speed.
        let granite_last_tick: std::rc::Rc<std::cell::Cell<Option<std::time::Instant>>> =
            std::rc::Rc::new(std::cell::Cell::new(None));
        // Tick-side handle on the shutdown flag declared above.
        let viz_shut_for_tick = viz_shutting_down.clone();
        let fs_viz_open_tick = fs_viz_open.clone();
        // Counter for periodic cache saves: fires every 300 ticks = 30 seconds.
        let mut cache_save_countdown = 300u32;

        // 33 ms (~30 fps) so the visualizer (Bars / Waveform / Granite) animates
        // smoothly. Bars/Waveform queue_draw is cheap; Granite renders into a
        // ~640×360 buffer that gets GPU-upscaled by gsk.
        glib::timeout_add_local(Duration::from_millis(33), move || {
            // Shutdown short-circuit. Set in connect_close_request below.
            if viz_shut_for_tick.get() {
                return ControlFlow::Break;
            }
            // 0. Drain probe results from background threads.
            // patch_pl_row is O(1) per call (updates a single TreeView store row).
            // Cap to 50 per tick so we never block the main thread for long when
            // a large library delivers thousands of results at once.
            let is_scanning = state.borrow().playlist_scan.is_some();
            let probe_cap = if is_scanning { 50usize } else { 500usize };
            // Drain into a batch first, then apply in ONE playlist pass —
            // a per-result pass was O(rows × results) and stalled this tick
            // on large playlists.
            let mut probe_batch: std::collections::HashMap<std::path::PathBuf, Duration> =
                std::collections::HashMap::new();
            while probe_batch.len() < probe_cap {
                let Ok((path, dur)) = probe_rx.try_recv() else {
                    break;
                };
                probe_batch.insert(path, dur);
            }
            if !probe_batch.is_empty() {
                // Bind so the RefMut drops before patch_pl_row borrows again.
                let changed = state.borrow_mut().apply_probed_durations(&probe_batch);
                for idx in changed {
                    patch_pl_row(idx);
                }
            }
            // 0b. Drain missing-file notifications; mark those tracks broken.
            while let Ok(path) = broken_rx.try_recv() {
                let found_idx = {
                    let mut s = state.borrow_mut();
                    let mut found = None;
                    for (idx, track) in s.playlist.tracks.iter_mut().enumerate() {
                        if track.path == path {
                            track.broken = true;
                            found = Some(idx);
                            break;
                        }
                    }
                    found
                };
                if let Some(idx) = found_idx {
                    patch_pl_row(idx);
                }
            }

            // 0c. Drain current track metadata scan results.
            // This is separate from the playlist scan (meta_rx) — it handles metadata
            // reads triggered by play_and_update when a track starts without metadata.
            while let Ok((path, title, artist, album_artist, album)) =
                current_track_meta_rx.borrow().try_recv()
            {
                let (updated_idx, is_current) = {
                    let mut s = state.borrow_mut();
                    let mut updated_idx = None;
                    let mut is_current = false;
                    for (idx, track) in s.playlist.tracks.iter_mut().enumerate() {
                        if track.path == path {
                            track.title = title;
                            track.artist = artist;
                            track.album_artist = album_artist;
                            track.album = album;
                            updated_idx = Some(idx);
                            is_current = idx == s.playlist.current_index;
                            break;
                        }
                    }
                    (updated_idx, is_current)
                };
                // Update the marquee with the new "Artist - Title" display name.
                if is_current {
                    let display = state
                        .borrow()
                        .playlist
                        .current()
                        .map(|t| t.display_name())
                        .unwrap_or_default();
                    if !display.is_empty() {
                        set_track(&display);
                    }
                }
                // Patch the row to show the new title/artist.
                if let Some(idx) = updated_idx {
                    patch_pl_row(idx);
                }
            }

            // 0d. Handle files received from "Open with Sparkamp" in the file manager.
            // Each batch respects playlist_add_behavior (append/replace) and
            // autoplay_on_add from config.
            while let Ok(paths) = open_rx.try_recv() {
                if paths.is_empty() {
                    continue;
                }
                use crate::config::PlaylistAddBehavior;
                let behavior = state.borrow().config.behavior.playlist_add_behavior.clone();
                let autoplay = state.borrow().config.behavior.autoplay_on_add;

                if behavior == PlaylistAddBehavior::Replace {
                    let _ = state.borrow_mut().player.stop();
                    {
                        let mut s = state.borrow_mut();
                        s.playlist.tracks.clear();
                        s.playlist.current_index = 0;
                        s.last_duration = None;
                        s.pending_seek = None;
                        s.mute_pending = None;
                    }
                }

                let insert_start = state.borrow().playlist.len();
                for path in &paths {
                    if let Ok(track) = crate::model::Track::from_path_fast(path) {
                        state.borrow_mut().playlist.tracks.push(track);
                    }
                }
                let inserted = state.borrow().playlist.len() - insert_start;
                if inserted == 0 {
                    continue;
                }
                rebuild_playlist_tick();

                if autoplay
                    && (behavior == PlaylistAddBehavior::Replace || insert_start == 0)
                {
                    state.borrow_mut().playlist.jump_to(insert_start);
                    play_update_tick();
                    scroll_tick(insert_start);
                }
            }

            // 1. Check for end-of-stream or GStreamer error.
            let bus_event = state.borrow_mut().poll_bus();

            // 1b. Apply any pending seek once the pipeline is running.
            //     Covers two cases:
            //       1. Live scrubbing while Playing/Paused.
            //       2. Pressing Play while Stopped with a pending seek: play_current()
            //          mutes audio and starts playing; the seek is applied here on the
            //          first tick that duration becomes available, then volume is restored.
            {
                let should_seek = {
                    let s = state.borrow();
                    s.pending_seek.is_some()
                        && *s.player.state() != PlayerState::Stopped
                        && (s.player.duration().is_some() || s.last_duration.is_some())
                };
                if should_seek {
                    let restore_vol = {
                        let mut s = state.borrow_mut();
                        let rv = s.mute_pending.take();
                        if let Some(fraction) = s.pending_seek.take() {
                            s.seek_fraction(fraction);
                        }
                        rv
                    };
                    if let Some(vol) = restore_vol {
                        state.borrow_mut().player.set_volume(vol);
                    }
                }
            }
            if let Some(event) = bus_event {
                // Record which track just finished so we can de-highlight it
                // after the advance changes current_index.
                let pre_advance_idx = state.borrow().playlist.current_index;

                // On error, mark the current track broken so it shows a
                // warning indicator and is skipped in future auto-advances.
                if matches!(event, BusEvent::Error) {
                    let mut s = state.borrow_mut();
                    let idx = s.playlist.current_index;
                    if let Some(t) = s.playlist.tracks.get_mut(idx) {
                        t.broken = true;
                    }
                }
                // Advance to the next track via shuffle/repeat logic.
                // Skips over tracks already marked broken.
                let advanced = {
                    let mut s = state.borrow_mut();
                    let total = s.playlist.len();
                    let repeat = s.config.playback.repeat_mode;
                    let current = s.playlist.current_index;

                    // Ask the shuffle engine for the next index.
                    let mut found = false;
                    if let Some(mut next_idx) = s.shuffle_state.next_index(current, total, repeat) {
                        // Skip broken tracks (up to `total` attempts to avoid infinite loop).
                        for _ in 0..total {
                            if s.playlist
                                .tracks
                                .get(next_idx)
                                .map(|t| t.broken)
                                .unwrap_or(false)
                            {
                                s.shuffle_state.record_played(next_idx);
                                match s.shuffle_state.next_index(next_idx, total, repeat) {
                                    Some(i) => {
                                        next_idx = i;
                                    }
                                    None => break,
                                }
                            } else {
                                s.playlist.jump_to(next_idx);
                                found = true;
                                break;
                            }
                        }
                    }
                    found
                };
                if advanced {
                    // play_update (play_and_update) patches the new current track.
                    // We also patch pre_advance_idx because jump_to() already
                    // updated current_index before play_and_update runs, so
                    // play_and_update won't know the finished track is different.
                    play_update();
                    let new_idx = state.borrow().playlist.current_index;
                    if pre_advance_idx != new_idx {
                        patch_pl_row(pre_advance_idx);
                    }
                }
            }

            // 2. Update time display and seek bar position.
            let (pos, dur_opt) = {
                let s = state.borrow();
                (s.player.position(), s.player.duration())
            };
            // Cache duration while it is available so seek-bar drags while
            // stopped can still show the correct time (GStreamer reports None
            // from a Null-state pipeline).
            let gst_dur_written = if let Some(dur) = dur_opt {
                let mut s = state.borrow_mut();
                s.last_duration = Some(dur);
                // Write GStreamer-queried duration back to the current track so
                // the playlist can show it even after playback stops.
                let idx = s.playlist.current_index;
                if let Some(track) = s.playlist.tracks.get_mut(idx) {
                    if track.duration.is_none() {
                        let path = track.path.clone();
                        track.duration = Some(dur);
                        s.duration_cache.insert(&path, dur);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if gst_dur_written {
                // Only the current track's duration changed; patch just that row.
                let idx = state.borrow().playlist.current_index;
                patch_pl_row(idx);
            }

            // Record play in media library after 20 seconds of playback.
            // The rebuild_ml_callback borrows state immutably, so it must be
            // called AFTER the mutable borrow is released — extract the Rc
            // first, then drop the borrow, then invoke the callback.
            let ml_rebuild_needed: Option<Rc<dyn Fn()>> = {
                let mut s = state.borrow_mut();
                let pos = pos.unwrap_or(Duration::ZERO);
                let path_str = s
                    .playlist
                    .current()
                    .map(|t| t.path.to_string_lossy().into_owned());
                if pos >= Duration::from_secs(20) {
                    if let Some(ref p) = path_str {
                        if s.counted_play_path.as_ref() != Some(p) {
                            if let Some(ref ml) = s.media_lib {
                                let _ = ml.record_play(p);
                                s.counted_play_path = Some(p.clone());
                                s.rebuild_ml_callback.clone()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(rebuild_ml) = ml_rebuild_needed {
                rebuild_ml();
                // Editor mirrors the same DB rows; reload its currently
                // open playlist so the just-recorded play count / last-
                // played timestamp / unread glyph reflect immediately.
                notify_editor_refresh();
            }

            {
                let (player_state, pending) = {
                    let s = state.borrow();
                    (s.player.state().clone(), s.pending_seek)
                };
                let show_rem = show_remaining.get();

                if player_state == PlayerState::Stopped {
                    // When stopped with a pending seek, hold the bar at the
                    // pending position and show its time.  set_value() does
                    // not re-trigger connect_change_value (GTK only emits
                    // change-value for user-initiated changes), so there is
                    // no feedback loop here.
                    if let Some(fraction) = pending {
                        seek_bar.set_value(fraction);
                        // Update the label if duration is known; otherwise
                        // leave whatever connect_change_value last set.
                        if let Some(text) =
                            state.borrow().time_display_for_fraction(fraction, show_rem)
                        {
                            time_disp_label.set_text(&text);
                        }
                    } else {
                        // Truly stopped with no pending seek — reset to zero.
                        seek_bar.set_value(0.0);
                        time_disp_label.set_text(if show_rem { "--:--" } else { "0:00" });
                    }
                } else {
                    // Playing or Paused — show live GStreamer position.
                    let pos = pos.unwrap_or(Duration::ZERO);
                    if show_rem {
                        if let Some(dur) = dur_opt {
                            let rem = dur.saturating_sub(pos);
                            let rs = rem.as_secs();
                            time_disp_label.set_text(&format!("-{}:{:02}", rs / 60, rs % 60));
                        } else {
                            time_disp_label.set_text("--:--");
                        }
                    } else {
                        let ps = pos.as_secs();
                        time_disp_label.set_text(&format!("{}:{:02}", ps / 60, ps % 60));
                    }
                    if let Some(dur) = dur_opt {
                        if dur.as_nanos() > 0 {
                            seek_bar.set_value(pos.as_nanos() as f64 / dur.as_nanos() as f64);
                        }
                    }
                }
            }

            // 3. Marquee / scrolling title.
            // Display a sliding window into the full "Title — Artist" text.
            // The window width is estimated from the label's allocated pixel
            // width divided by 8 (conservative px-per-char for the 13 px font).
            {
                let chars = marquee_chars.borrow();
                // Fallback to 30 chars before the label is laid out (width = 0).
                let label_w = title_label.allocated_width();
                let display_cols = if label_w > 0 {
                    (label_w / 8).max(10) as usize
                } else {
                    30
                };

                if chars.len() <= display_cols {
                    // Short enough to fit without scrolling.
                    title_label.set_text(&chars.iter().collect::<String>());
                    marquee_offset.set(0);
                } else {
                    // Advance offset every 3 ticks (≈ 300 ms, ~3 chars/second).
                    let tick = marquee_tick.get() + 1;
                    marquee_tick.set(tick);
                    if tick % 3 == 0 {
                        // 5-space visual gap between repetitions.
                        let cycle = chars.len() + 5;
                        marquee_offset.set((marquee_offset.get() + 1) % cycle);
                    }

                    let offset = marquee_offset.get();
                    // Pad with spaces so wrap-around reads cleanly.
                    let gap: Vec<char> = "     ".chars().collect();
                    let looped: Vec<char> = chars.iter().chain(gap.iter()).cloned().collect();
                    let loop_len = looped.len();
                    let visible: String = (0..display_cols)
                        .map(|i| *looped.get((offset + i) % loop_len).unwrap_or(&' '))
                        .collect();
                    title_label.set_text(&visible);
                }
            }

            // 4. State icon (left of time display) + dynamic play-button accent.
            //    The play button gains the `.transport-play` skin accent while
            //    the engine is Playing or Paused, and loses it when Stopped.
            {
                let s = state.borrow();
                let icon = match s.player.state() {
                    PlayerState::Playing => "▶",
                    PlayerState::Paused => "⏸",
                    PlayerState::Stopped => "⏹",
                };
                state_label.set_text(icon);
                match s.player.state() {
                    PlayerState::Playing | PlayerState::Paused => {
                        if !btn_play.has_css_class("transport-play") {
                            btn_play.add_css_class("transport-play");
                        }
                    }
                    PlayerState::Stopped => {
                        btn_play.remove_css_class("transport-play");
                    }
                }
            }

            // 5. Trigger a Cairo repaint of the visualizer (Bars / Waveform).
            // Granite renders into a Picture instead — see step 5b below.
            viz.queue_draw();

            // 5b. Granite plasma path. Cheap when not the active mode (the
            // match is the only cost). When active, render into the persistent
            // RGBA buffer and hand it to the GTK renderer as a MemoryTexture
            // — gsk uploads to the GPU once per frame and bilinear-upscales
            // for free in the compositor.
            {
                // Upgrade weak refs first; if the main window has closed,
                // both widgets are gone — break the timer instead of touching
                // freed Gdk surfaces.
                let (Some(stack), Some(pic)) = (
                    viz_stack_tick.upgrade(),
                    granite_pic_tick.upgrade(),
                ) else {
                    return ControlFlow::Break;
                };

                // If the widget has no root (no GtkWindow ancestor), the
                // surface is being torn down. Skip set_paintable to avoid a
                // gsk paint on a freed Gdk surface.
                if pic.root().is_none() {
                    return ControlFlow::Break;
                }

                let mode = state.borrow().config.visualizer.mode.clone();
                if mode == VisualizerMode::Granite {
                    if stack.visible_child_name().as_deref() != Some("granite") {
                        stack.set_visible_child_name("granite");
                    }
                    // Single-driver rule: yield while the fullscreen window
                    // owns the renderer (the mini keeps its last texture).
                    if !fs_viz_open_tick.get() {
                        // Aspect-matched internal width: viewport-aspect × fixed
                        // 360 short axis. Fall back to 16:9 when the widget hasn't
                        // been allocated yet.
                        let viewport_w = pic.width().max(1) as f64;
                        let viewport_h = pic.height().max(1) as f64;
                        let aspect = (viewport_w / viewport_h).max(0.5).min(4.0);
                        let h: u32 = crate::granite::GRANITE_INTERNAL_HEIGHT;
                        let w: u32 = (h as f64 * aspect).round() as u32;
                        let mut buf = granite_buf_tick.borrow_mut();
                        let need = (w as usize) * (h as usize) * 4;
                        if buf.len() != need {
                            buf.resize(need, 0);
                        }
                        let cfg = state.borrow().config.visualizer.granite;
                        let now = std::time::Instant::now();
                        let dt_frames = granite_last_tick
                            .replace(Some(now))
                            .map(|prev| now.duration_since(prev).as_secs_f32() * 30.0)
                            .unwrap_or(1.0);
                        state
                            .borrow_mut()
                            .player
                            .render_granite(&mut buf, w, h, &cfg, dt_frames);
                        let bytes = glib::Bytes::from(&buf[..]);
                        let texture = gdk::MemoryTexture::new(
                            w as i32,
                            h as i32,
                            gdk::MemoryFormat::R8g8b8a8,
                            &bytes,
                            (w * 4) as usize,
                        );
                        pic.set_paintable(Some(&texture));
                    }
                } else if stack.visible_child_name().as_deref() != Some("cairo") {
                    stack.set_visible_child_name("cairo");
                }
            }

            // 6. Periodically flush the duration cache and config to disk (every 30 s).
            // Saving config here ensures settings survive force-kills.
            cache_save_countdown -= 1;
            if cache_save_countdown == 0 {
                cache_save_countdown = 300;
                state.borrow_mut().duration_cache.save_if_dirty();
                let _ = state.borrow().config.save();
            }

            ControlFlow::Continue
        });
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Visualizer draw function (mini box in the now-playing row)
    // ══════════════════════════════════════════════════════════════════════════
    // Note: parse_hex_color / draw_zoned_bar / draw_waveform are module-level
    // functions defined near the bottom of this file so they can also be called
    // from open_waveform_fullscreen.

    {
        let state = state.clone();
        viz.set_draw_func(move |_da, cr, width, height| {
            // ── Background ────────────────────────────────────────────────
            cr.set_source_rgb(0.05, 0.05, 0.05);
            cr.paint().ok();

            let s = state.borrow();
            let is_playing = *s.player.state() == PlayerState::Playing;
            let mode = s.config.visualizer.mode.clone();
            let display_bands_count = s.config.visualizer.display_bands;
            let bars_mirror = s.config.visualizer.bars_mirror;
            let color_zones = s.config.visualizer.color_zones as usize;
            let zone_colors = s.config.visualizer.zone_colors.clone();
            let wf_zones = s.config.visualizer.waveform_color_zones as usize;
            let wf_zone_colors = s.config.visualizer.waveform_zone_colors.clone();
            let wf_style = s.config.visualizer.waveform_style.clone();

            // Get spectrum and waveform data before dropping the borrow.
            let display_bands_data = s.player.get_spectrum_display_bands(display_bands_count);
            let waveform_samples = s.player.get_waveform_samples(width.max(64) as usize);
            drop(s);

            if !is_playing {
                // Idle: flat dim centre line.
                cr.set_source_rgb(0.0, 0.3, 0.1);
                cr.set_line_width(1.0);
                let mid = height as f64 / 2.0;
                cr.move_to(0.0, mid);
                cr.line_to(width as f64, mid);
                cr.stroke().ok();
                return;
            }

            match mode {
                VisualizerMode::Bars => {
                    let num_bars = display_bands_count.max(10) as usize;
                    let bar_w = width as f64 / num_bars as f64;

                    if !display_bands_data.iter().all(|&v| v == 0.0) {
                        for (i, &amp) in display_bands_data.iter().enumerate() {
                            let x = i as f64 * bar_w;
                            draw_zoned_bar(
                                &cr,
                                x,
                                bar_w,
                                height as f64,
                                amp,
                                bars_mirror,
                                color_zones,
                                &zone_colors,
                            );
                        }
                    } else {
                        cr.set_source_rgb(0.0, 0.3, 0.1);
                        cr.set_line_width(1.0);
                        let mid = height as f64 / 2.0;
                        cr.move_to(0.0, mid);
                        cr.line_to(width as f64, mid);
                        cr.stroke().ok();

                        cr.set_source_rgb(0.0, 0.5, 0.2);
                        let font_size = 10.0_f64.min(height as f64 * 0.4);
                        cr.set_font_size(font_size);
                        let text = "Retry";
                        if let Ok(extents) = cr.text_extents(text) {
                            let text_x =
                                (width as f64 - extents.width()) / 2.0 - extents.x_bearing();
                            let text_y =
                                (height as f64 - extents.height()) / 2.0 - extents.y_bearing();
                            cr.move_to(text_x, text_y);
                            cr.show_text(text).ok();
                        }
                    }
                }
                VisualizerMode::Waveform => {
                    draw_waveform(
                        &cr,
                        width as f64,
                        height as f64,
                        &waveform_samples,
                        wf_zones,
                        &wf_zone_colors,
                        &wf_style,
                    );
                }
                // Granite is rendered by a separate Picture widget swapped in
                // via a Stack (see step 4); the Cairo DrawingArea sits behind
                // the Picture and isn't visible while Granite is active. Draw
                // nothing here so we don't waste cycles on a hidden surface.
                VisualizerMode::Granite => {}
            }
        });
    }

    // ══════════════════════════════════════════════════════════════════════════
    // ══════════════════════════════════════════════════════════════════════════
    // ══════════════════════════════════════════════════════════════════════════
    // Jump window — dedicated search/jump interface (opened with 'j').
    // Lives in its own window separate from the playlist so the two don't
    // overlap.  Populated fresh every time it opens.
    // ══════════════════════════════════════════════════════════════════════════
    let jump_entry = gtk4::SearchEntry::new();
    jump_entry.set_placeholder_text(Some("Search… (↑↓ navigate, Enter play, Esc close)"));
    jump_entry.set_margin_top(8);
    jump_entry.set_margin_bottom(4);
    jump_entry.set_margin_start(8);
    jump_entry.set_hexpand(true);

    let jump_clear_btn = Button::with_label("✕");
    jump_clear_btn.add_css_class("pl-btn");
    jump_clear_btn.set_margin_top(8);
    jump_clear_btn.set_margin_bottom(4);
    jump_clear_btn.set_margin_end(8);

    let jump_search_row = GtkBox::new(Orientation::Horizontal, 4);
    jump_search_row.append(&jump_entry);
    jump_search_row.append(&jump_clear_btn);

    let jump_box = ListBox::new();
    jump_box.add_css_class("playlist");
    jump_box.set_selection_mode(gtk4::SelectionMode::Single);

    let jump_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .min_content_height(280)
        .child(&jump_box)
        .build();

    // Status line below the results box: shows match count or a hint.
    let jump_status = gtk4::Label::builder()
        .halign(Align::Start)
        .margin_start(8)
        .margin_end(8)
        .margin_top(2)
        .margin_bottom(4)
        .build();
    jump_status.add_css_class("status-label");

    let jump_root = gtk4::Box::new(Orientation::Vertical, 0);
    jump_root.append(&jump_search_row);
    jump_root.append(&jump_scroll);
    jump_root.append(&jump_status);

    let jump_win = gtk4::Window::builder()
        .title("Jump to Track")
        .default_width(380)
        .default_height(360)
        .modal(false)
        .build();
    jump_win.set_transient_for(Some(&window));
    jump_win.set_child(Some(&jump_root));
    // Hide instead of destroy when the user closes the window so it can be
    // reopened later.  Without this, the underlying GObject may be freed after
    // the first close, making subsequent `present()` calls a no-op.
    jump_win.set_hide_on_close(true);
    jump_win.connect_visible_notify({
        let btn = btn_jump_vol.clone();
        move |w| {
            if w.is_visible() {
                btn.add_css_class("mode-btn-active");
            } else {
                btn.remove_css_class("mode-btn-active");
            }
        }
    });

    // Maps each visible row in jump_box → the original track index in the playlist.
    let jump_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Maximum rows shown in the jump list.  Caps widget creation so the window
    // stays responsive on playlists with tens of thousands of tracks.
    const MAX_JUMP_RESULTS: usize = 500;

    // Closure: clear and repopulate jump_box based on the current query.
    let rebuild_jump: Rc<dyn Fn()> = {
        let state = state.clone();
        let jump_entry = jump_entry.clone();
        let jump_box = jump_box.clone();
        let jump_indices = jump_indices.clone();
        let jump_status = jump_status.clone();
        Rc::new(move || {
            // remove_all() is a single GTK call instead of O(n) individual removes.
            jump_box.remove_all();
            let mut indices = jump_indices.borrow_mut();
            indices.clear();

            let q = jump_entry.text();
            // Empty query: show a hint and leave the list empty.
            // Without this guard, an empty query would match every track and
            // create tens of thousands of widgets, freezing the UI.
            if q.trim().is_empty() {
                let total = state.borrow().playlist.len();
                jump_status.set_text(&format!("{total} tracks — type to search"));
                return;
            }

            let all_matches = {
                let s = state.borrow();
                s.playlist.search_indices(&q)
            };
            let total_matches = all_matches.len();
            let capped = total_matches > MAX_JUMP_RESULTS;
            let s = state.borrow();
            for &idx in all_matches.iter().take(MAX_JUMP_RESULTS) {
                let track = &s.playlist.tracks[idx];
                let label_text = if track.artist.is_empty() {
                    format!("{:2}. {}", idx + 1, track.title)
                } else {
                    format!("{:2}. {} — {}", idx + 1, track.artist, track.title)
                };
                let row_label = gtk4::Label::builder()
                    .label(&label_text)
                    .halign(Align::Start)
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .build();
                row_label.set_margin_start(6);
                row_label.set_margin_end(6);
                row_label.set_margin_top(3);
                row_label.set_margin_bottom(3);
                let row = gtk4::ListBoxRow::new();
                row.set_child(Some(&row_label));
                jump_box.append(&row);
                indices.push(idx);
            }
            drop(s);

            // Status line.
            if total_matches == 0 {
                jump_status.set_text("No matches");
            } else if capped {
                jump_status.set_text(&format!(
                    "Showing {} of {} matches — type more to narrow",
                    MAX_JUMP_RESULTS, total_matches
                ));
            } else {
                jump_status.set_text(&format!("{total_matches} match{}", if total_matches == 1 { "" } else { "es" }));
            }

            // Auto-select the first row so Enter immediately plays.
            if let Some(row) = jump_box.row_at_index(0) {
                jump_box.select_row(Some(&row));
            }
        })
    };

    // Wire up the jump-window clear button now that rebuild_jump is in scope.
    {
        let e = jump_entry.clone();
        let rj = rebuild_jump.clone();
        jump_clear_btn.connect_clicked(move |_| {
            gtk4::prelude::EditableExt::set_text(&e, "");
            rj();
        });
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Keyboard shortcuts — shared handler applied to player + playlist windows.
    // ══════════════════════════════════════════════════════════════════════════

    let handle_key: Rc<dyn Fn(gdk::Key) -> glib::Propagation> = {
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let status_label = status_label.clone();
        let pl_status = pl_status_label.clone();
        let kbd_set_track = set_track.clone();
        let kbd_rebuild = rebuild_playlist.clone();
        let kbd_vol_bar = vol_bar.clone();
        let kbd_seek_bar = seek_bar.clone();
        let playlist_win_wk = playlist_win.downgrade();
        // Strong reference: keeps the window alive even when hidden, so
        // repeated open/close cycles work without recreating the widget tree.
        let kbd_jump_win = jump_win.clone();
        let window_weak = window.downgrade();
        let remove_sel = remove_selected.clone();
        let kbd_probe_tx = probe_tx.clone();
        let kbd_broken_tx = broken_tx.clone();
        let kbd_rebuild_jump = rebuild_jump.clone();
        let kbd_jump_entry = jump_entry.clone();
        let kbd_btn_info = btn_info.clone();
        // Clones for r/s key handlers to update button visuals.
        let kbd_btn_repeat = btn_repeat.clone();
        let kbd_repeat_icon = repeat_icon.clone();
        let kbd_repeat_label = repeat_label.clone();
        let kbd_btn_shuffle = btn_shuffle.clone();
        // Clones for z/b (prev/next) handlers — use patch instead of rebuild
        // so the scroll position is preserved rather than reset to the top.
        let kbd_patch_row = patch_pl_row.clone();
        let kbd_scroll = scroll_to_row_if_needed.clone();
        let kbd_open_fs = open_fullscreen_fn.clone();

        Rc::new(move |key: gdk::Key| -> glib::Propagation {
            match key {
                // ── Winamp transport bindings ──────────────────────────────
                gdk::Key::z => {
                    let old_idx = state.borrow().playlist.current_index;
                    let result = { state.borrow_mut().play_prev() };
                    if let Some(d) = result {
                        kbd_set_track(&d);
                        let new_idx = state.borrow().playlist.current_index;
                        if old_idx != new_idx {
                            kbd_patch_row(old_idx);
                        }
                        kbd_patch_row(new_idx);
                        kbd_scroll(new_idx);
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::x => {
                    let ps = state.borrow().player.state().clone();
                    match ps {
                        PlayerState::Stopped | PlayerState::Paused => play_and_update(),
                        PlayerState::Playing => {}
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::c => {
                    let _ = state.borrow_mut().player.toggle_pause();
                    glib::Propagation::Stop
                }
                gdk::Key::v => {
                    let _ = state.borrow_mut().player.stop();
                    kbd_seek_bar.set_value(0.0);
                    glib::Propagation::Stop
                }
                gdk::Key::b => {
                    let old_idx = state.borrow().playlist.current_index;
                    let result = { state.borrow_mut().play_next() };
                    if let Some(d) = result {
                        kbd_set_track(&d);
                        let new_idx = state.borrow().playlist.current_index;
                        if old_idx != new_idx {
                            kbd_patch_row(old_idx);
                        }
                        kbd_patch_row(new_idx);
                        kbd_scroll(new_idx);
                    }
                    glib::Propagation::Stop
                }

                // ── Arrow keys: seek ±5 seconds ───────────────────────────
                // GTK fires key-repeat while the key is held, so holding Left
                // or Right continuously rewinds / fast-forwards the track.
                gdk::Key::Left => {
                    state.borrow_mut().seek_delta_secs(-5.0);
                    glib::Propagation::Stop
                }
                gdk::Key::Right => {
                    state.borrow_mut().seek_delta_secs(5.0);
                    glib::Propagation::Stop
                }

                // ── Volume: - decreases, = / + increases ──────────────────
                // GTK fires key-repeat while the key is held, so volume
                // continues to ramp as long as the key is held down.
                gdk::Key::minus => {
                    let new_vol = {
                        let s = state.borrow();
                        (s.config.playback.volume - 0.05).clamp(0.0, 1.0)
                    };
                    {
                        let mut s = state.borrow_mut();
                        s.config.playback.volume = new_vol;
                        s.player.set_volume(new_vol);
                    }
                    kbd_vol_bar.set_value(new_vol);
                    glib::Propagation::Stop
                }
                gdk::Key::equal | gdk::Key::plus => {
                    let new_vol = {
                        let s = state.borrow();
                        (s.config.playback.volume + 0.05).clamp(0.0, 1.0)
                    };
                    {
                        let mut s = state.borrow_mut();
                        s.config.playback.volume = new_vol;
                        s.player.set_volume(new_vol);
                    }
                    kbd_vol_bar.set_value(new_vol);
                    glib::Propagation::Stop
                }

                // ── Visualizer mode toggle ─────────────────────────────────
                gdk::Key::a | gdk::Key::A => {
                    state.borrow_mut().toggle_visualizer_mode();
                    glib::Propagation::Stop
                }

                // ── Random Granite effect (e — Granite mode) ───────────────
                gdk::Key::e | gdk::Key::E => {
                    let mut s = state.borrow_mut();
                    if matches!(s.config.visualizer.mode, VisualizerMode::Granite) {
                        if let Some(eff) = s.player.granite_random_effect() {
                            // Record in config so pinned mode (auto-switch
                            // off) follows along instead of snapping back.
                            s.config.visualizer.granite.effect = eff;
                        }
                    }
                    glib::Propagation::Stop
                }

                // ── Visualizer fullscreen (f — Waveform or Granite mode) ──
                gdk::Key::f | gdk::Key::F => {
                    let supports_fs = matches!(
                        state.borrow().config.visualizer.mode,
                        VisualizerMode::Waveform | VisualizerMode::Granite,
                    );
                    if supports_fs {
                        if let Some(ref opener) = *kbd_open_fs.borrow() {
                            opener();
                        }
                    }
                    glib::Propagation::Stop
                }

                // ── Jump window ────────────────────────────────────────────
                gdk::Key::j | gdk::Key::J => {
                    kbd_jump_entry.set_text("");
                    kbd_rebuild_jump();
                    kbd_jump_win.present();
                    kbd_jump_entry.grab_focus();
                    glib::Propagation::Stop
                }

                // ── Add file (single file via desktop file browser) ────────
                gdk::Key::n => {
                    // Build a reusable audio filter for all common formats.
                    let filter = gtk4::FileFilter::new();
                    filter.set_name(Some("Audio files"));
                    for mime in &[
                        "audio/mpeg",
                        "audio/flac",
                        "audio/ogg",
                        "audio/opus",
                        "audio/wav",
                        "audio/aac",
                        "audio/mp4",
                        "audio/x-m4a",
                    ] {
                        filter.add_mime_type(mime);
                    }
                    for pat in &[
                        "*.mp3", "*.flac", "*.ogg", "*.opus", "*.wav", "*.aac", "*.m4a",
                    ] {
                        filter.add_pattern(pat);
                    }
                    let filters = gio::ListStore::new::<gtk4::FileFilter>();
                    filters.append(&filter);

                    let dialog = gtk4::FileDialog::builder().title("Add Audio File").build();
                    dialog.set_filters(Some(&filters));

                    let state_cb = state.clone();
                    let rebuild_cb = rebuild_playlist.clone();
                    let status_cb = status_label.clone();
                    let pl_stat_cb = pl_status.clone();
                    let probe_tx_cb = kbd_probe_tx.clone();
                    let broken_tx_cb = kbd_broken_tx.clone();
                    let parent = window_weak.upgrade();
                    dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let before = state_cb.borrow().playlist.tracks.len();
                                let outcome = state_cb.borrow_mut().add_path(&path);
                                match outcome {
                                    Ok(msg) => {
                                        status_cb.set_text(&msg);
                                        pl_stat_cb.set_text(&msg);
                                        rebuild_cb();
                                        let paths = state_cb.borrow().uncached_paths_from(before);
                                        if !paths.is_empty() {
                                            duration_probe::spawn_probes(
                                                paths,
                                                probe_tx_cb.clone(),
                                                broken_tx_cb.clone(),
                                            );
                                        }
                                    }
                                    Err(msg) => {
                                        status_cb.set_text(&msg);
                                    }
                                }
                            }
                        }
                    });
                    glib::Propagation::Stop
                }

                // ── Playlist window toggle ─────────────────────────────────
                gdk::Key::p | gdk::Key::P => {
                    if let Some(pw) = playlist_win_wk.upgrade() {
                        pw.set_visible(!pw.is_visible());
                    }
                    glib::Propagation::Stop
                }

                // ── Delete: remove all selected playlist rows ──────────────
                gdk::Key::Delete => {
                    remove_sel();
                    glib::Propagation::Stop
                }

                // ── Repeat mode cycle (r) ─────────────────────────────────
                gdk::Key::r | gdk::Key::R => {
                    let new_mode = {
                        let mut s = state.borrow_mut();
                        let m = s.config.playback.repeat_mode.cycle();
                        s.config.playback.repeat_mode = m;
                        m
                    };
                    kbd_repeat_icon.set_icon_name(Some(repeat_btn_icon(new_mode)));
                    kbd_repeat_label.set_text(repeat_btn_text(new_mode));
                    if new_mode == crate::shuffle::RepeatMode::Off {
                        kbd_btn_repeat.remove_css_class("mode-btn-active");
                    } else {
                        kbd_btn_repeat.add_css_class("mode-btn-active");
                    }
                    glib::Propagation::Stop
                }

                // ── Shuffle toggle (s — hidden; only shown in help) ───────
                gdk::Key::s | gdk::Key::S => {
                    let enabled = {
                        let mut s = state.borrow_mut();
                        s.shuffle_state.toggle();
                        s.shuffle_state.reset();
                        let on = s.shuffle_state.enabled;
                        // Mirror to config so the setting survives to the next session.
                        s.config.playback.shuffle_enabled = on;
                        on
                    };
                    if enabled {
                        kbd_btn_shuffle.add_css_class("mode-btn-active");
                    } else {
                        kbd_btn_shuffle.remove_css_class("mode-btn-active");
                    }
                    glib::Propagation::Stop
                }

                // ── ID3 tag editor (d) — open for the currently playing track ─
                gdk::Key::d | gdk::Key::D => {
                    let path = state.borrow().playlist.current().map(|t| t.path.clone());
                    if let Some(path) = path {
                        if let Some(w) = window_weak.upgrade() {
                            open_id3_editor_window(
                                Some(&w),
                                path,
                                state.clone(),
                                kbd_rebuild.clone(),
                                None,
                            );
                        }
                    } else {
                        status_label.set_text("No track loaded");
                    }
                    glib::Propagation::Stop
                }

                // ── Info / keyboard shortcuts window ──────────────────────
                gdk::Key::i | gdk::Key::I => {
                    kbd_btn_info.activate();
                    glib::Propagation::Stop
                }

                // ── Quit ──────────────────────────────────────────────────
                gdk::Key::q | gdk::Key::Q => {
                    let _ = state.borrow().playlist.save_last();
                    if let Some(w) = window_weak.upgrade() {
                        // Closing the main window triggers connect_close_request
                        // which also saves the playlist — belt-and-suspenders.
                        w.close();
                    }
                    glib::Propagation::Stop
                }

                _ => glib::Propagation::Proceed,
            }
        })
    };

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

    // Attach the shared handler to the main player window.
    // Capture phase ensures keys reach the handler even when a child widget
    // (e.g. the visualizer DrawingArea) has keyboard focus.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let handler = handle_key.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, _| handler(key));
        window.add_controller(key_ctrl);
    }

    // Attach the same handler to the playlist window so all shortcuts work
    // even when the playlist window has keyboard focus.  Use Capture phase so
    // the ListBox cannot swallow keys (e.g. 'j') before they reach this handler.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let handler = handle_key.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, _| handler(key));
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

        let sections: &[(&str, &[(&str, &str)])] = &[
            ("Playback", &[
                ("z",          "Previous track / restart"),
                ("x",          "Play"),
                ("c",          "Pause / resume"),
                ("v",          "Stop"),
                ("b",          "Next track"),
                ("← →",        "Seek −5 s / +5 s"),
                ("r",          "Cycle repeat (off / song / playlist)"),
                ("s",          "Toggle shuffle on/off"),
            ]),
            ("Volume", &[
                ("-",          "Volume down 5 %"),
                ("=",          "Volume up 5 %"),
            ]),
            ("Playlist", &[
                ("n",          "Add file(s) or folder(s)"),
                ("j",          "Jump / search"),
                ("↑ k / ↓ l",  "Browse up / down"),
                ("Enter",      "Play selected track"),
                ("Del",        "Remove highlighted track"),
                ("p",          "Toggle playlist window"),
            ]),
            ("View & Tags", &[
                ("a",           "Cycle visualizer mode (Bars / Waveform / Granite)"),
                ("e",           "Random Granite effect (Granite mode)"),
                ("f",           "Fullscreen visualizer (Waveform or Granite mode; Esc to exit)"),
                ("g",           "Toggle FPS / BPM overlay (fullscreen only)"),
                ("d",           "View/Edit ID3 tags for current track"),
                ("u",           "Open EQ (TUI only — use EQ button in GUI)"),
                ("Click logo",  "Open settings"),
            ]),
            ("Other", &[
                ("i",          "Toggle this help"),
                ("q / Esc",    "Quit"),
            ]),
        ];

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

    // J button — toggle jump window.
    btn_jump_vol.connect_clicked({
        let jump_win_wk = jump_win.downgrade();
        let entry = jump_entry.clone();
        let rebuild = rebuild_jump.clone();
        move |_| {
            if let Some(w) = jump_win_wk.upgrade() {
                if w.is_visible() {
                    w.hide();
                } else {
                    entry.set_text("");
                    rebuild();
                    w.present();
                    entry.grab_focus();
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
            // First open: create the window.
            let parent = window_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
            let (w, h) = {
                let cfg = &state_rc.borrow().config.window;
                (cfg.ml_width, cfg.ml_height)
            };
            let ml_win = open_media_library_window(
                parent.as_ref(),
                state_rc.clone(),
                rebuild_pl.clone(),
                set_track_ml.clone(),
                current_drives.clone(),
                current_devices.clone(),
                burn_queues.clone(),
                copy_files_holder.clone(),
                burn_refresh_holder.clone(),
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
    // Jump window callbacks (wired after handle_key so the key controller can
    // delegate transport shortcuts to it).
    // ══════════════════════════════════════════════════════════════════════════

    // Typing in the jump entry: immediately refilter results.
    jump_entry.connect_changed({
        let rebuild_jump = rebuild_jump.clone();
        move |_| {
            rebuild_jump();
        }
    });

    // Enter: play the selected (or first) result and close the window.
    jump_entry.connect_activate({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        let jump_box = jump_box.clone();
        let jump_indices = jump_indices.clone();
        let jump_win_wk = jump_win.downgrade();
        move |_| {
            let sel_row_idx = jump_box.selected_row().map(|r| r.index() as usize);
            if let Some(list_pos) = sel_row_idx {
                if let Some(&track_idx) = jump_indices.borrow().get(list_pos) {
                    let old_idx = state.borrow().playlist.current_index;
                    state.borrow_mut().playlist.jump_to(track_idx);
                    play_and_update();
                    if old_idx != track_idx {
                        patch_pl_row(old_idx);
                    }
                }
            }
            if let Some(w) = jump_win_wk.upgrade() {
                w.close();
            }
        }
    });

    // SearchEntry emits stop-search (and consumes Escape) before window-level
    // key controllers see it.  Wire the signal directly so Escape always closes.
    jump_entry.connect_stop_search({
        let jw = jump_win.clone();
        move |_| {
            jw.close();
        }
    });

    // Key controller for the jump window: ↑↓ navigate rows; Escape as a
    // fallback in case focus is on the list box rather than the entry.
    // PropagationPhase::Capture ensures we intercept before child widgets.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let jb = jump_box.clone();
        let jw_wk = jump_win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Escape => {
                if let Some(w) = jw_wk.upgrade() {
                    w.close();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                let cur = jb.selected_row().map(|r| r.index()).unwrap_or(1);
                if let Some(row) = jb.row_at_index((cur - 1).max(0)) {
                    jb.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            gdk::Key::Down => {
                let cur = jb.selected_row().map(|r| r.index()).unwrap_or(-1);
                if let Some(row) = jb.row_at_index(cur + 1) {
                    jb.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        jump_win.add_controller(key_ctrl);
    }

    // Double-clicking a result plays it immediately.
    jump_box.connect_row_activated({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        let jump_indices = jump_indices.clone();
        let jump_win_wk = jump_win.downgrade();
        move |_, row| {
            let list_pos = row.index() as usize;
            if let Some(&track_idx) = jump_indices.borrow().get(list_pos) {
                let old_idx = state.borrow().playlist.current_index;
                state.borrow_mut().playlist.jump_to(track_idx);
                play_and_update();
                if old_idx != track_idx {
                    patch_pl_row(old_idx);
                }
            }
            if let Some(w) = jump_win_wk.upgrade() {
                w.close();
            }
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
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
            let rebuild_pl = rebuild_playlist.clone();
            let ml_win = open_media_library_window(
                Some(&window.upcast::<gtk4::Window>()),
                state_rc.clone(),
                rebuild_pl.clone(),
                set_track_init_ml.clone(),
                current_drives,
                current_devices,
                burn_queues,
                copy_files_holder,
                burn_refresh_holder,
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

