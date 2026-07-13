/// Parse a hex color string (`"#RRGGBB"`) to RGB components in [0, 1].
fn parse_hex_color(hex: &str) -> (f64, f64, f64) {
    let hex = hex.trim_start_matches('#');
    if hex.len() >= 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f64 / 255.0;
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f64 / 255.0;
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f64 / 255.0;
        (r, g, b)
    } else {
        (0.0, 0.4, 0.0) // fallback dark green
    }
}

/// Draw a single zone-coloured frequency bar.
/// For singular mode: bar extends from bottom to `amp × height`.
/// For mirrored mode: bar extends `amp × height / 2` above and below centre.
fn draw_zoned_bar(
    cr: &gtk4::cairo::Context,
    x: f64,
    bar_w: f64,
    height: f64,
    amp: f64,
    mirror: bool,
    num_zones: usize,
    zone_colors: &[String],
) {
    let num_zones = num_zones.max(1);
    let half_gap = 0.75;
    let bar_w = bar_w - half_gap;

    // Parse the hex colors once per draw — the closure runs per zone per
    // bar at 30 fps.
    let parsed: Vec<(f64, f64, f64)> = zone_colors.iter().map(|c| parse_hex_color(c)).collect();
    let get_color = |zone: usize| -> (f64, f64, f64) {
        parsed[zone.min(parsed.len().saturating_sub(1))]
    };

    if mirror {
        let center = height / 2.0;
        let max_extent = amp * center;

        for zone in 0..num_zones {
            let zone_inner = zone as f64 * (center / num_zones as f64);
            let zone_outer = (zone + 1) as f64 * (center / num_zones as f64);

            if zone_outer <= max_extent {
                let (r, g, b) = get_color(zone);
                cr.set_source_rgb(r, g, b);
                let y = center + zone_inner;
                let h = zone_outer - zone_inner;
                cr.rectangle(x + 0.5, y, bar_w, h);
                cr.fill().ok();
                let y = center - zone_outer;
                cr.rectangle(x + 0.5, y, bar_w, h);
                cr.fill().ok();
            } else if zone_inner < max_extent {
                let (r, g, b) = get_color(zone);
                cr.set_source_rgb(r, g, b);
                let y = center + zone_inner;
                let h = max_extent - zone_inner;
                cr.rectangle(x + 0.5, y, bar_w, h);
                cr.fill().ok();
                let y = center - max_extent;
                cr.rectangle(x + 0.5, y, bar_w, h);
                cr.fill().ok();
            }
        }
    } else {
        let bar_height = amp * height;
        let bar_top = height - bar_height;
        let zone_h = height / num_zones as f64;

        for zone in 0..num_zones {
            let zone_bottom = height - (zone + 1) as f64 * zone_h;
            let zone_top = height - zone as f64 * zone_h;

            if zone_top > bar_top {
                let (r, g, b) = get_color(zone);
                cr.set_source_rgb(r, g, b);
                let draw_bottom = zone_bottom.max(bar_top);
                let draw_top = zone_top.min(height);
                let h = (draw_top - draw_bottom).max(1.0);
                cr.rectangle(x + 0.5, draw_bottom, bar_w, h);
                cr.fill().ok();
            }
        }
    }
}

/// Draw the real-audio waveform visualizer using Cairo.
///
/// `samples` are bipolar PCM in `[-1, 1]` (0 = centre / silence).
/// Zones are horizontal bands; zone 0 (index 0 in `zone_colors`) is the
/// bottom of the widget and zone N-1 is the top.
///
/// - **Lines** — draws the stroke only; each segment coloured by zone.
/// - **Filled** — fills the area between the waveform and the centre
///   baseline, coloured per zone.
fn draw_waveform(
    cr: &gtk4::cairo::Context,
    width: f64,
    height: f64,
    samples: &[f64],
    num_zones: usize,
    zone_colors: &[String],
    style: &WaveformStyle,
) {
    let num_zones = num_zones.max(1);
    let center_y = height / 2.0;
    let n = samples.len();
    if n == 0 {
        return;
    }

    // Dim centre baseline.
    cr.set_source_rgb(0.0, 0.2, 0.08);
    cr.set_line_width(0.5);
    cr.move_to(0.0, center_y);
    cr.line_to(width, center_y);
    cr.stroke().ok();

    // Zone index for a Cairo y-coordinate. Zone 0 = bottom, zone N-1 = top.
    let zone_for_y = |y: f64| -> usize {
        let frac = (height - y) / height;
        ((frac * num_zones as f64) as usize).min(num_zones - 1)
    };

    // Parsed once per draw — the closure runs per waveform segment at 30 fps.
    let parsed: Vec<(f64, f64, f64)> = zone_colors.iter().map(|c| parse_hex_color(c)).collect();
    let get_color = |zone: usize| -> (f64, f64, f64) {
        parsed[zone.min(parsed.len().saturating_sub(1))]
    };

    // sample ∈ [-1, 1] → y = center - sample × (center × 0.9)
    let ys: Vec<f64> = samples
        .iter()
        .map(|&s| (center_y - s * center_y * 0.9).clamp(0.0, height))
        .collect();

    match style {
        WaveformStyle::Lines => {
            cr.set_line_width(1.5);
            for i in 0..n.saturating_sub(1) {
                let x0 = i as f64 * width / n as f64;
                let x1 = (i + 1) as f64 * width / n as f64;
                let y0 = ys[i];
                let y1 = ys[i + 1];
                let zone = zone_for_y((y0 + y1) / 2.0);
                let (r, g, b) = get_color(zone);
                cr.set_source_rgb(r, g, b);
                cr.move_to(x0, y0);
                cr.line_to(x1, y1);
                cr.stroke().ok();
            }
        }
        WaveformStyle::Filled => {
            for i in 0..n {
                let x = i as f64 * width / n as f64;
                let col_w = (width / n as f64).max(1.0);
                let y = ys[i];
                let (y_top, y_bot) = if y < center_y { (y, center_y) } else { (center_y, y) };
                for zone in 0..num_zones {
                    let zone_top_y = height - (zone + 1) as f64 * height / num_zones as f64;
                    let zone_bot_y = height - zone as f64 * height / num_zones as f64;
                    let draw_top = y_top.max(zone_top_y);
                    let draw_bot = y_bot.min(zone_bot_y);
                    if draw_top < draw_bot {
                        let (r, g, b) = get_color(zone);
                        cr.set_source_rgb(r, g, b);
                        cr.rectangle(x, draw_top, col_w, draw_bot - draw_top);
                        cr.fill().ok();
                    }
                }
            }
        }
    }
}

// Visualizer fullscreen (Waveform or Granite)
// ---------------------------------------------------------------------------

/// Open the active visualizer (Waveform or Granite) in fullscreen mode.
///
/// The window covers all other windows on the desktop.  While open:
/// - `z x c v b r s` are passed to the shared `handle_key` handler.
/// - `i` opens the information/shortcuts window.
/// - `j` opens the jump-to-track window.
/// - Status changes appear as a 3-second translucent toast at the bottom.
/// - `Esc` closes the fullscreen window.
///
/// Double-clicking the mini visualiser or pressing `f` when the active mode is
/// Waveform or Granite triggers this function. Bars is excluded.
fn open_waveform_fullscreen(
    state: Rc<RefCell<AppState>>,
    handle_key: Rc<dyn Fn(gdk::Key) -> glib::Propagation>,
    jump_win: gtk4::Window,
    jump_entry: gtk4::SearchEntry,
    rebuild_jump: Rc<dyn Fn()>,
    btn_info: gtk4::Button,
    // Single-driver rule: set while this window is open so the mini-viz
    // tick yields the shared Granite renderer (see the tick loop in build).
    fs_viz_open: Rc<Cell<bool>>,
) {
    fs_viz_open.set(true);
    let fs_win = gtk4::Window::new();
    fs_win.set_decorated(false);

    // ── Canvas (Stack: cairo + granite) + toast overlay ───────────────────
    let overlay = gtk4::Overlay::new();

    let canvas = DrawingArea::new();
    canvas.set_hexpand(true);
    canvas.set_vexpand(true);

    let granite_canvas = Picture::new();
    granite_canvas.set_hexpand(true);
    granite_canvas.set_vexpand(true);
    granite_canvas.set_content_fit(ContentFit::Fill);

    let canvas_stack = Stack::new();
    canvas_stack.set_hexpand(true);
    canvas_stack.set_vexpand(true);
    canvas_stack.add_named(&canvas, Some("cairo"));
    canvas_stack.add_named(&granite_canvas, Some("granite"));
    canvas_stack.set_visible_child_name(
        match state.borrow().config.visualizer.mode {
            VisualizerMode::Granite => "granite",
            _ => "cairo",
        },
    );
    overlay.set_child(Some(&canvas_stack));

    // Translucent status toast label at the bottom of the screen.
    let toast = gtk4::Label::new(None);
    toast.add_css_class("wf-fs-toast");
    toast.set_halign(Align::Center);
    toast.set_valign(Align::End);
    toast.set_margin_bottom(48);
    toast.set_visible(false);
    overlay.add_overlay(&toast);

    // FPS counter, top-right; toggled with the `g` key.
    let fps_label = gtk4::Label::new(Some("FPS: --"));
    fps_label.add_css_class("wf-fs-toast"); // share the toast pill style
    fps_label.set_halign(Align::End);
    fps_label.set_valign(Align::Start);
    fps_label.set_margin_top(16);
    fps_label.set_margin_end(20);
    fps_label.set_visible(false);
    overlay.add_overlay(&fps_label);

    fs_win.set_child(Some(&overlay));

    // ── Draw function ──────────────────────────────────────────────────────
    let state_draw = state.clone();
    canvas.set_draw_func(move |_da, cr, width, height| {
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.paint().ok();

        let s = state_draw.borrow();
        let is_playing = *s.player.state() == PlayerState::Playing;
        let wf_zones = s.config.visualizer.waveform_color_zones as usize;
        let wf_zone_colors = s.config.visualizer.waveform_zone_colors.clone();
        let wf_style = s.config.visualizer.waveform_style.clone();
        // Use 2× width for sharper fullscreen detail.
        let sample_count = (width * 2).max(512) as usize;
        let waveform_samples = s.player.get_waveform_samples(sample_count);
        drop(s);

        if !is_playing {
            // Flat dim centre line when idle.
            cr.set_source_rgb(0.0, 0.15, 0.05);
            cr.set_line_width(1.0);
            cr.move_to(0.0, height as f64 / 2.0);
            cr.line_to(width as f64, height as f64 / 2.0);
            cr.stroke().ok();
            return;
        }

        draw_waveform(
            cr,
            width as f64,
            height as f64,
            &waveform_samples,
            wf_zones,
            &wf_zone_colors,
            &wf_style,
        );
    });

    // ── Redraw timer (~30 fps) ─────────────────────────────────────────────
    // Cairo canvas redraws via queue_draw; Granite renders into the Picture
    // via the same MemoryTexture path the mini-viz uses.
    let canvas_weak = canvas.downgrade();
    let granite_canvas_weak = granite_canvas.downgrade();
    let stack_weak = canvas_stack.downgrade();
    let fps_label_weak = fps_label.downgrade();
    let state_tick = state.clone();
    let granite_buf: std::rc::Rc<std::cell::RefCell<Vec<u8>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    // Shut-down flag flipped from the fullscreen window's close_request so
    // the timer breaks before gsk gets a chance to paint a dead surface.
    let fs_shutting_down: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let fs_shut_for_tick = fs_shutting_down.clone();
    // FPS smoothing state. EMA of inter-frame interval; updated every tick,
    // displayed every ~10 frames so the number doesn't flicker.
    let last_instant: Rc<Cell<Option<std::time::Instant>>> = Rc::new(Cell::new(None));
    let ema_dt_ms: Rc<Cell<f32>> = Rc::new(Cell::new(33.3));
    let fps_update_countdown: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    // Vsync-locked render loop: the frame clock fires at the display's real
    // refresh rate (60 fps on most monitors, more on fast ones) and supplies
    // the timestamps the dt-aware sim needs, so fullscreen Granite runs at
    // full refresh while moving at exactly the same speed as the 30 fps
    // windowed view. The windowed mini stays on its 33 ms timer.
    let last_frame_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
    fs_win.add_tick_callback(move |_, frame_clock| {
        if fs_shut_for_tick.get() {
            return glib::ControlFlow::Break;
        }
        let Some(c) = canvas_weak.upgrade() else { return glib::ControlFlow::Break; };
        let Some(pic) = granite_canvas_weak.upgrade() else { return glib::ControlFlow::Break; };
        let Some(stack) = stack_weak.upgrade() else { return glib::ControlFlow::Break; };
        if pic.root().is_none() {
            return glib::ControlFlow::Break;
        }
        // Frame-clock timestamps (µs) → dt in 30 fps frame units.
        let now_us = frame_clock.frame_time();
        let prev_us = last_frame_us.replace(now_us);
        let dt_frames = if prev_us == 0 {
            1.0
        } else {
            ((now_us - prev_us) as f32 / 1_000_000.0) * 30.0
        };
        let mode = state_tick.borrow().config.visualizer.mode.clone();
        if mode == VisualizerMode::Granite {
            if stack.visible_child_name().as_deref() != Some("granite") {
                stack.set_visible_child_name("granite");
            }
            let viewport_w = pic.width().max(1) as f64;
            let viewport_h = pic.height().max(1) as f64;
            let aspect = (viewport_w / viewport_h).max(0.5).min(4.0);
            let h: u32 = crate::granite::GRANITE_INTERNAL_HEIGHT;
            let w: u32 = (h as f64 * aspect).round() as u32;
            let mut buf = granite_buf.borrow_mut();
            let need = (w as usize) * (h as usize) * 4;
            if buf.len() != need {
                buf.resize(need, 0);
            }
            let cfg = state_tick.borrow().config.visualizer.granite;
            state_tick
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
        } else {
            if stack.visible_child_name().as_deref() != Some("cairo") {
                stack.set_visible_child_name("cairo");
            }
            c.queue_draw();
        }

        // FPS tracking. EMA on inter-frame ms; display rounded each ~10 ticks.
        if let Some(label) = fps_label_weak.upgrade() {
            let now = std::time::Instant::now();
            if let Some(prev) = last_instant.get() {
                let dt_ms = now.duration_since(prev).as_secs_f32() * 1000.0;
                let cur = ema_dt_ms.get();
                ema_dt_ms.set(cur * 0.9 + dt_ms * 0.1);
            }
            last_instant.set(Some(now));

            if label.is_visible() {
                let n = fps_update_countdown.get();
                if n == 0 {
                    let fps = if ema_dt_ms.get() > 0.0 { 1000.0 / ema_dt_ms.get() } else { 0.0 };
                    // BPM from the Granite beat detector; "--" until it locks.
                    // Same format as the macOS overlay.
                    let (bpm, meter) = {
                        let s = state_tick.borrow();
                        (s.player.granite_bpm(), s.player.granite_meter())
                    };
                    let bpm_str = if bpm > 0.0 {
                        format!("{bpm:.0}")
                    } else {
                        "--".to_string()
                    };
                    let meter_str = if meter > 0 {
                        format!(" ({meter}/4)")
                    } else {
                        String::new()
                    };
                    label.set_text(&format!("FPS: {fps:.0}   BPM: {bpm_str}{meter_str}"));
                    fps_update_countdown.set(10);
                } else {
                    fps_update_countdown.set(n - 1);
                }
            }
        }

        glib::ControlFlow::Continue
    });

    // ── Toast helpers ──────────────────────────────────────────────────────
    let toast_label = toast.clone();
    let toast_source: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));

    let show_toast = {
        let tl = toast_label.clone();
        let ts = toast_source.clone();
        Rc::new(move |msg: String| {
            tl.set_text(&msg);
            tl.set_visible(true);
            if let Some(id) = ts.take() {
                id.remove();
            }
            let tl2 = tl.clone();
            let ts2 = ts.clone();
            let id = glib::timeout_add_local(std::time::Duration::from_secs(3), move || {
                tl2.set_visible(false);
                ts2.set(None);
                glib::ControlFlow::Break
            });
            ts.set(Some(id));
        })
    };

    // ── Key bindings ───────────────────────────────────────────────────────
    let key_ctrl = EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);

    let fs_win_weak = fs_win.downgrade();
    let state_keys = state.clone();
    let show_toast_key = show_toast.clone();
    let fps_label_keys = fps_label.clone();

    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        match key {
            gdk::Key::Escape => {
                if let Some(w) = fs_win_weak.upgrade() {
                    w.close();
                }
                glib::Propagation::Stop
            }
            // FPS overlay toggle
            gdk::Key::g | gdk::Key::G => {
                fps_label_keys.set_visible(!fps_label_keys.is_visible());
                glib::Propagation::Stop
            }
            // Jump window
            gdk::Key::j | gdk::Key::J => {
                gtk4::prelude::EditableExt::set_text(&jump_entry, "");
                rebuild_jump();
                jump_win.present();
                jump_entry.grab_focus();
                glib::Propagation::Stop
            }
            // Info / shortcuts window
            gdk::Key::i | gdk::Key::I => {
                btn_info.activate();
                glib::Propagation::Stop
            }
            // Random Granite effect — forward to the main-window handler.
            // The fullscreen window has its own key controller, so keys not
            // matched here never reach the main window.
            gdk::Key::e | gdk::Key::E => handle_key(key),
            // Transport + mode keys — pass through, then show toast
            gdk::Key::z
            | gdk::Key::x
            | gdk::Key::c
            | gdk::Key::v
            | gdk::Key::b
            | gdk::Key::r
            | gdk::Key::R
            | gdk::Key::s
            | gdk::Key::S => {
                let result = handle_key(key);
                let msg = {
                    let s = state_keys.borrow();
                    if let Some(track) = s.playlist.current() {
                        let ps = s.player.state().clone();
                        let verb = match ps {
                            PlayerState::Playing => "Playing",
                            PlayerState::Paused => "Paused",
                            PlayerState::Stopped => "Stopped",
                        };
                        format!("{}: {}", verb, track.display_name())
                    } else {
                        String::new()
                    }
                };
                if !msg.is_empty() {
                    show_toast_key(msg);
                }
                result
            }
            _ => glib::Propagation::Proceed,
        }
    });
    fs_win.add_controller(key_ctrl);

    // Keep the display awake while fullscreen, when configured. The
    // session manager auto-releases the inhibit if the app dies, so only
    // the orderly close path needs the explicit uninhibit.
    let inhibit_cookie: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    if state.borrow().config.visualizer.keep_screen_awake {
        if let Some(app) = gtk4::gio::Application::default()
            .and_downcast::<gtk4::Application>()
        {
            let cookie = app.inhibit(
                Some(&fs_win),
                gtk4::ApplicationInhibitFlags::IDLE,
                Some("Fullscreen visualizer"),
            );
            inhibit_cookie.set(cookie);
        }
    }

    // Stop the 33 ms tick before the fullscreen window's surface is freed,
    // and let the display sleep again.
    let cookie_close = inhibit_cookie.clone();
    fs_win.connect_close_request(move |_| {
        fs_shutting_down.set(true);
        fs_viz_open.set(false);
        if cookie_close.get() != 0 {
            if let Some(app) = gtk4::gio::Application::default()
                .and_downcast::<gtk4::Application>()
            {
                app.uninhibit(cookie_close.get());
            }
            cookie_close.set(0);
        }
        glib::Propagation::Proceed
    });

    // ── Show fullscreen ────────────────────────────────────────────────────
    fs_win.present();
    fs_win.fullscreen();
}

// Image viewer popup
// ---------------------------------------------------------------------------

/// Open a resizable window displaying the image at `path`.
fn open_image_viewer(path: &str) {
    use gtk4::ContentFit;

    let exists = std::path::Path::new(path).exists();

    let win = gtk4::Window::new();
    win.set_title(Some("Artwork — Sparkamp"));
    win.set_default_size(400, 400);
    win.set_resizable(true);

    if !exists {
        // File missing — show an inline message instead of a blank window.
        let lbl = gtk4::Label::builder()
            .label(format!("Artwork file not found:\n{path}"))
            .halign(Align::Center)
            .valign(Align::Center)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .wrap(true)
            .build();
        win.set_child(Some(&lbl));
        win.present();
        return;
    }

    // Load via Gdk Texture so we can surface decode failures explicitly
    // instead of silently rendering a blank Picture.
    match gtk4::gdk::Texture::from_filename(path) {
        Ok(tex) => {
            let picture = gtk4::Picture::for_paintable(&tex);
            picture.set_can_shrink(true);
            picture.set_content_fit(ContentFit::Contain);
            picture.set_hexpand(true);
            picture.set_vexpand(true);
            win.set_child(Some(&picture));
        }
        Err(e) => {
            let lbl = gtk4::Label::builder()
                .label(format!("Could not decode artwork:\n{e}"))
                .halign(Align::Center)
                .valign(Align::Center)
                .margin_top(24)
                .margin_bottom(24)
                .margin_start(24)
                .margin_end(24)
                .wrap(true)
                .build();
            win.set_child(Some(&lbl));
        }
    }
    win.present();
}

