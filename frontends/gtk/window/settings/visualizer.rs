//! The Visualizer tab: mode, bands and the Granite controls.
//!
//! Split out of `open_settings_window`, which was one 2,775-line function
//! holding every tab inline. The body is unchanged; what it used to close
//! over arrives as arguments.

use super::*;

pub(super) fn build(notebook: &Notebook, state: &Rc<RefCell<AppState>>) {
    let state = state.clone();
        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        // ── Mode selector ──────────────────────────────────────────────
        let lbl = Label::new(Some("Visualizer mode"));
        lbl.set_halign(Align::Start);
        grid.attach(&lbl, 0, 0, 1, 1);

        // DropDown: index 0 = Bars, 1 = Waveform, 2 = Granite.
        let dd_mode = DropDown::from_strings(&["Bars", "Waveform", "Granite"]);
        {
            let mode = state.borrow().config.visualizer.mode.clone();
            dd_mode.set_selected(match mode {
                VisualizerMode::Bars     => 0,
                VisualizerMode::Waveform => 1,
                VisualizerMode::Granite  => 2,
            });
        }
        {
            let state_rc = state.clone();
            dd_mode.connect_selected_notify(move |d| {
                let mut s = state_rc.borrow_mut();
                s.config.visualizer.mode = match d.selected() {
                    0 => VisualizerMode::Bars,
                    1 => VisualizerMode::Waveform,
                    _ => VisualizerMode::Granite,
                };
            });
        }
        grid.attach(&dd_mode, 1, 0, 1, 1);

        // ── Keep display awake during fullscreen visualizer ────────────
        // Mode-independent: applies to Waveform and Granite fullscreen.
        let lbl_awake = Label::new(Some("Keep display awake in fullscreen"));
        lbl_awake.set_halign(Align::Start);
        grid.attach(&lbl_awake, 0, 4, 1, 1);
        let chk_awake = CheckButton::new();
        chk_awake.set_active(state.borrow().config.visualizer.keep_screen_awake);
        {
            let state_rc = state.clone();
            chk_awake.connect_toggled(move |c| {
                state_rc.borrow_mut().config.visualizer.keep_screen_awake =
                    c.is_active();
            });
        }
        grid.attach(&chk_awake, 1, 4, 1, 1);

        // ── Bars Settings (visible only when Bars mode is selected) ───
        let bars_settings_box = Grid::new();
        bars_settings_box.set_row_spacing(12);
        bars_settings_box.set_column_spacing(16);
        bars_settings_box.set_margin_top(16);
        bars_settings_box.set_margin_start(16);
        bars_settings_box.attach(&Label::new(Some("Bars Settings")), 0, 0, 2, 1);

        // Mirror bars toggle
        let lbl_mirror = Label::new(Some("Mirror bars"));
        lbl_mirror.set_halign(Align::Start);
        bars_settings_box.attach(&lbl_mirror, 0, 1, 1, 1);

        let chk_mirror = CheckButton::new();
        {
            let bars_mirror = state.borrow().config.visualizer.bars_mirror;
            chk_mirror.set_active(bars_mirror);
        }
        {
            let state_rc = state.clone();
            chk_mirror.connect_toggled(move |c| {
                state_rc.borrow_mut().config.visualizer.bars_mirror = c.is_active();
            });
        }
        bars_settings_box.attach(&chk_mirror, 1, 1, 1, 1);

        // Color zones selector
        let lbl_zones = Label::new(Some("Color zones"));
        lbl_zones.set_halign(Align::Start);
        bars_settings_box.attach(&lbl_zones, 0, 2, 1, 1);

        let spin_zones = SpinButton::with_range(1.0, 6.0, 1.0);
        {
            let zones = state.borrow().config.visualizer.color_zones;
            spin_zones.set_value(zones as f64);
        }
        bars_settings_box.attach(&spin_zones, 1, 2, 1, 1);

        // Zone colors - create 6 color buttons (one per possible zone)
        let zone_color_buttons: Vec<(Label, ColorButton)> = (0..6)
            .map(|i| {
                let lbl = Label::new(Some(&format!("Zone {} color:", i + 1)));
                lbl.set_halign(Align::Start);

                let btn = ColorButton::new();
                let zone_colors = state.borrow().config.visualizer.zone_colors.clone();
                if let Some(hex) = zone_colors.get(i) {
                    if let Ok(rgba) = gdk::RGBA::parse(hex) {
                        btn.set_rgba(&rgba);
                    }
                }

                (lbl, btn)
            })
            .collect();

        // Add color buttons to grid (start at row 3)
        for (i, (lbl, btn)) in zone_color_buttons.iter().enumerate() {
            bars_settings_box.attach(lbl, 0, 3 + i as i32, 1, 1);
            bars_settings_box.attach(btn, 1, 3 + i as i32, 1, 1);
            // Start with all hidden; they'll be shown based on zone count
            lbl.set_visible(false);
            btn.set_visible(false);
        }

        // Helper to update zone button visibility
        let update_zone_visibility = {
            let zone_labels: Vec<_> = zone_color_buttons.iter().map(|(l, _)| l.clone()).collect();
            let zone_buttons: Vec<_> = zone_color_buttons.iter().map(|(_, b)| b.clone()).collect();
            move |num_zones: u8| {
                for i in 0..6 {
                    let visible = (i as u8) < num_zones;
                    zone_labels[i].set_visible(visible);
                    zone_buttons[i].set_visible(visible);
                }
            }
        };

        // Connect zone count changes
        {
            let state_rc = state.clone();
            let update_zone_visibility = update_zone_visibility.clone();
            spin_zones.connect_value_changed(move |s| {
                let num_zones = s.value() as u8;
                state_rc.borrow_mut().config.visualizer.color_zones = num_zones;
                update_zone_visibility(num_zones);
            });
        }

        // Connect color button signals
        for (i, (_, btn)) in zone_color_buttons.iter().enumerate() {
            let state_rc = state.clone();
            btn.connect_color_set(move |button| {
                let rgba = button.rgba();
                let hex = format!(
                    "#{:02x}{:02x}{:02x}",
                    (rgba.red() * 255.0) as u8,
                    (rgba.green() * 255.0) as u8,
                    (rgba.blue() * 255.0) as u8,
                );
                let mut s = state_rc.borrow_mut();
                let zone_colors = &mut s.config.visualizer.zone_colors;
                // Ensure we have at least i+1 entries
                while zone_colors.len() <= i {
                    zone_colors.push("#000000".to_string());
                }
                zone_colors[i] = hex;
            });
        }

        // Set initial visibility based on current zone count
        {
            let num_zones = state.borrow().config.visualizer.color_zones;
            update_zone_visibility(num_zones);
        }

        // Show/hide bars settings based on mode
        bars_settings_box.set_visible(false); // Start hidden
        {
            let bars_settings = bars_settings_box.clone();
            dd_mode.connect_selected_notify(move |d| {
                bars_settings.set_visible(d.selected() == 0);
            });
        }
        {
            let bars_settings = bars_settings_box.clone();
            bars_settings.set_visible(
                state.borrow().config.visualizer.mode == VisualizerMode::Bars,
            );
        }

        grid.attach(&bars_settings_box, 0, 1, 2, 1);

        // ── Waveform Settings (visible only when Waveform mode is selected) ─
        let wf_settings_box = Grid::new();
        wf_settings_box.set_row_spacing(12);
        wf_settings_box.set_column_spacing(16);
        wf_settings_box.set_margin_top(16);
        wf_settings_box.set_margin_start(16);
        wf_settings_box.attach(&Label::new(Some("Waveform Settings")), 0, 0, 2, 1);

        // Style selector (Lines / Filled)
        let lbl_wf_style = Label::new(Some("Style"));
        lbl_wf_style.set_halign(Align::Start);
        wf_settings_box.attach(&lbl_wf_style, 0, 1, 1, 1);

        let dd_wf_style = DropDown::from_strings(&["Lines", "Filled"]);
        {

            let cur = state.borrow().config.visualizer.waveform_style.clone();
            dd_wf_style.set_selected(match cur {
                WaveformStyle::Lines => 0,
                WaveformStyle::Filled => 1,
            });
        }
        {

            let state_rc = state.clone();
            dd_wf_style.connect_selected_notify(move |d| {
                state_rc.borrow_mut().config.visualizer.waveform_style = match d.selected() {
                    1 => WaveformStyle::Filled,
                    _ => WaveformStyle::Lines,
                };
            });
        }
        wf_settings_box.attach(&dd_wf_style, 1, 1, 1, 1);

        // Color zones count
        let lbl_wf_zones = Label::new(Some("Color zones"));
        lbl_wf_zones.set_halign(Align::Start);
        wf_settings_box.attach(&lbl_wf_zones, 0, 2, 1, 1);

        let spin_wf_zones = SpinButton::with_range(1.0, 6.0, 1.0);
        {
            let zones = state.borrow().config.visualizer.waveform_color_zones;
            spin_wf_zones.set_value(zones as f64);
        }
        wf_settings_box.attach(&spin_wf_zones, 1, 2, 1, 1);

        // 6 zone colour buttons
        let wf_zone_color_buttons: Vec<(Label, ColorButton)> = (0..6)
            .map(|i| {
                let lbl = Label::new(Some(&format!("Zone {} color:", i + 1)));
                lbl.set_halign(Align::Start);
                let btn = ColorButton::new();
                let colors = state.borrow().config.visualizer.waveform_zone_colors.clone();
                if let Some(hex) = colors.get(i) {
                    if let Ok(rgba) = gdk::RGBA::parse(hex) {
                        btn.set_rgba(&rgba);
                    }
                }
                (lbl, btn)
            })
            .collect();

        for (i, (lbl, btn)) in wf_zone_color_buttons.iter().enumerate() {
            wf_settings_box.attach(lbl, 0, 3 + i as i32, 1, 1);
            wf_settings_box.attach(btn, 1, 3 + i as i32, 1, 1);
            lbl.set_visible(false);
            btn.set_visible(false);
        }

        let update_wf_zone_visibility = {
            let lbls: Vec<_> = wf_zone_color_buttons.iter().map(|(l, _)| l.clone()).collect();
            let btns: Vec<_> = wf_zone_color_buttons.iter().map(|(_, b)| b.clone()).collect();
            move |num: u8| {
                for i in 0..6 {
                    let v = (i as u8) < num;
                    lbls[i].set_visible(v);
                    btns[i].set_visible(v);
                }
            }
        };

        {
            let state_rc = state.clone();
            let upd = update_wf_zone_visibility.clone();
            spin_wf_zones.connect_value_changed(move |s| {
                let n = s.value() as u8;
                state_rc.borrow_mut().config.visualizer.waveform_color_zones = n;
                upd(n);
            });
        }

        for (i, (_, btn)) in wf_zone_color_buttons.iter().enumerate() {
            let state_rc = state.clone();
            btn.connect_color_set(move |button| {
                let rgba = button.rgba();
                let hex = format!(
                    "#{:02x}{:02x}{:02x}",
                    (rgba.red() * 255.0) as u8,
                    (rgba.green() * 255.0) as u8,
                    (rgba.blue() * 255.0) as u8,
                );
                let mut s = state_rc.borrow_mut();
                let colors = &mut s.config.visualizer.waveform_zone_colors;
                while colors.len() <= i {
                    colors.push("#000000".to_string());
                }
                colors[i] = hex;
            });
        }

        {
            let n = state.borrow().config.visualizer.waveform_color_zones;
            update_wf_zone_visibility(n);
        }

        // Show/hide waveform settings based on mode
        wf_settings_box.set_visible(false);
        {
            let wf_settings = wf_settings_box.clone();
            dd_mode.connect_selected_notify(move |d| {
                wf_settings.set_visible(d.selected() == 1);
            });
        }
        {
            let wf_settings = wf_settings_box.clone();
            wf_settings.set_visible(
                state.borrow().config.visualizer.mode == VisualizerMode::Waveform,
            );
        }

        grid.attach(&wf_settings_box, 0, 2, 2, 1);

        // ── Granite Settings (visible only when Granite mode is selected) ─
        let gr_settings_box = Grid::new();
        gr_settings_box.set_row_spacing(12);
        gr_settings_box.set_column_spacing(16);
        gr_settings_box.set_margin_top(16);
        gr_settings_box.set_margin_start(16);
        gr_settings_box.attach(&Label::new(Some("Granite Settings")), 0, 0, 2, 1);

        // Credit where it's due: Granite is a re-creation, not an original
        // idea. Same text as the macOS Settings window.
        let lbl_gr_credit = Label::new(None);
        lbl_gr_credit.set_markup(
            "<small>Granite is an interpretation of the Geiss Winamp plugin \
             by Ryan Geiss. All credit to his amazing work on the original. \
             <a href=\"https://www.geisswerks.com/geiss/\">Click</a> for \
             more information.</small>",
        );
        lbl_gr_credit.set_wrap(true);
        lbl_gr_credit.set_xalign(0.0);
        lbl_gr_credit.set_halign(Align::Start);
        // Pin min width == natural width so the wrap point — and therefore
        // the measured height — is the same in every measure pass. A wrapped
        // label whose min and natural widths differ makes the fixed-size
        // Settings window log "Trying to measure GtkWindow for height of X,
        // but it needs at least Y" warnings.
        lbl_gr_credit.set_width_chars(52);
        lbl_gr_credit.set_max_width_chars(52);
        lbl_gr_credit.add_css_class("dim-label");
        gr_settings_box.attach(&lbl_gr_credit, 0, 1, 2, 1);

        // Speed slider (0.1–5.0).
        let lbl_gr_speed = Label::new(Some("Speed"));
        lbl_gr_speed.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_speed, 0, 2, 1, 1);
        let speed_adj = Adjustment::new(
            state.borrow().config.visualizer.granite.speed as f64,
            0.1, 5.0, 0.1, 0.5, 0.0,
        );
        let scale_gr_speed = Scale::new(Orientation::Horizontal, Some(&speed_adj));
        scale_gr_speed.set_hexpand(true);
        scale_gr_speed.set_digits(2);
        scale_gr_speed.set_draw_value(true);
        // A unitless multiplier — the raw number spoken alone already matches
        // what draw_value shows sighted users, so no ValueText is needed.
        scale_gr_speed.update_property(&[gtk4::accessible::Property::Label("Visualizer speed")]);
        {
            let state_rc = state.clone();
            speed_adj.connect_value_changed(move |a| {
                state_rc.borrow_mut().config.visualizer.granite.speed =
                    a.value().clamp(0.1, 5.0) as f32;
            });
        }
        gr_settings_box.attach(&scale_gr_speed, 1, 2, 1, 1);

        // Palette dropdown — order must match GranitePalette declaration.
        let lbl_gr_palette = Label::new(Some("Palette"));
        lbl_gr_palette.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_palette, 0, 3, 1, 1);
        let dd_gr_palette = DropDown::from_strings(&[
            "Granite", "Fire", "Neon", "Ocean", "Violet", "Sunset", "CRT", "Spectrum",
        ]);
        {
            use crate::granite::GranitePalette;
            let cur = state.borrow().config.visualizer.granite.palette;
            dd_gr_palette.set_selected(match cur {
                GranitePalette::Granite  => 0,
                GranitePalette::Fire     => 1,
                GranitePalette::Neon     => 2,
                GranitePalette::Ocean    => 3,
                GranitePalette::Violet   => 4,
                GranitePalette::Sunset   => 5,
                GranitePalette::Crt      => 6,
                GranitePalette::Spectrum => 7,
            });
        }
        {
            use crate::granite::GranitePalette;
            let state_rc = state.clone();
            dd_gr_palette.connect_selected_notify(move |d| {
                let p = match d.selected() {
                    1 => GranitePalette::Fire,
                    2 => GranitePalette::Neon,
                    3 => GranitePalette::Ocean,
                    4 => GranitePalette::Violet,
                    5 => GranitePalette::Sunset,
                    6 => GranitePalette::Crt,
                    7 => GranitePalette::Spectrum,
                    _ => GranitePalette::Granite,
                };
                let mut s = state_rc.borrow_mut();
                s.config.visualizer.granite.palette = p;
                // Apply to the live renderer too — it auto-rolls palettes on
                // beats, so the config value alone never reaches the screen.
                s.player.granite_set_palette(p);
            });
        }
        gr_settings_box.attach(&dd_gr_palette, 1, 3, 1, 1);

        // Feedback slider (0.0–0.9). Higher = stronger trail.
        let lbl_gr_fb = Label::new(Some("Feedback"));
        lbl_gr_fb.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_fb, 0, 4, 1, 1);
        let fb_adj = Adjustment::new(
            state.borrow().config.visualizer.granite.feedback as f64,
            0.0, 0.9, 0.05, 0.1, 0.0,
        );
        let scale_gr_fb = Scale::new(Orientation::Horizontal, Some(&fb_adj));
        scale_gr_fb.set_hexpand(true);
        scale_gr_fb.set_digits(2);
        scale_gr_fb.set_draw_value(true);
        // A unitless trail-strength factor — same reasoning as Speed above.
        scale_gr_fb.update_property(&[gtk4::accessible::Property::Label("Visualizer feedback")]);
        {
            let state_rc = state.clone();
            fb_adj.connect_value_changed(move |a| {
                state_rc.borrow_mut().config.visualizer.granite.feedback =
                    a.value().clamp(0.0, 0.9) as f32;
            });
        }
        gr_settings_box.attach(&scale_gr_fb, 1, 4, 1, 1);

        // Effect dropdown — one entry per warp-map family.
        let lbl_gr_effect = Label::new(Some("Effect"));
        lbl_gr_effect.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_effect, 0, 5, 1, 1);
        let dd_gr_effect = DropDown::from_strings(&[
            "Plasma", "Tunnel", "Swirl", "Spin", "Cells", "Explode",
            "Ripple", "Shear", "Kaleidoscope", "Gravity Well", "Drain", "Flag",
        ]);
        {
            use crate::granite::GraniteEffect;
            let cur = state.borrow().config.visualizer.granite.effect;
            dd_gr_effect.set_selected(match cur {
                GraniteEffect::Plasma      => 0,
                GraniteEffect::Tunnel      => 1,
                GraniteEffect::Swirl       => 2,
                GraniteEffect::RadialSweep => 3,
                GraniteEffect::Cells       => 4,
                GraniteEffect::Explode     => 5,
                GraniteEffect::Ripple      => 6,
                GraniteEffect::Shear       => 7,
                GraniteEffect::Kaleido     => 8,
                GraniteEffect::GravityWell => 9,
                GraniteEffect::Drain       => 10,
                GraniteEffect::Flag        => 11,
            });
        }
        {
            use crate::granite::GraniteEffect;
            let state_rc = state.clone();
            dd_gr_effect.connect_selected_notify(move |d| {
                let e = match d.selected() {
                    1  => GraniteEffect::Tunnel,
                    2  => GraniteEffect::Swirl,
                    3  => GraniteEffect::RadialSweep,
                    4  => GraniteEffect::Cells,
                    5  => GraniteEffect::Explode,
                    6  => GraniteEffect::Ripple,
                    7  => GraniteEffect::Shear,
                    8  => GraniteEffect::Kaleido,
                    9  => GraniteEffect::GravityWell,
                    10 => GraniteEffect::Drain,
                    11 => GraniteEffect::Flag,
                    _  => GraniteEffect::Plasma,
                };
                let mut s = state_rc.borrow_mut();
                s.config.visualizer.granite.effect = e;
                s.player.granite_set_effect(e);
            });
        }
        gr_settings_box.attach(&dd_gr_effect, 1, 5, 1, 1);

        // Auto-switch toggle (rotates effects every ~15s).
        let lbl_gr_auto = Label::new(Some("Auto-switch effect"));
        lbl_gr_auto.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_auto, 0, 6, 1, 1);
        let chk_gr_auto = CheckButton::new();
        chk_gr_auto.set_active(state.borrow().config.visualizer.granite.auto_switch);
        {
            let state_rc = state.clone();
            chk_gr_auto.connect_toggled(move |c| {
                state_rc.borrow_mut().config.visualizer.granite.auto_switch = c.is_active();
            });
        }
        gr_settings_box.attach(&chk_gr_auto, 1, 6, 1, 1);

        // Beat sensitivity slider — matches the FFI clamp in
        // src/ffi/granite.rs (1.05..=3.0). render_granite() re-reads the
        // config struct fresh every tick (see player.rs/viz.rs), so a plain
        // config write reaches the renderer next frame with no extra
        // live-apply call, same as Speed and Feedback above.
        let lbl_gr_sens = Label::new(Some("Beat sensitivity"));
        lbl_gr_sens.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_sens, 0, 7, 1, 1);
        let sens_adj = Adjustment::new(
            state.borrow().config.visualizer.granite.beat_sensitivity as f64,
            1.05, 3.0, 0.05, 0.1, 0.0,
        );
        let scale_gr_sens = Scale::new(Orientation::Horizontal, Some(&sens_adj));
        scale_gr_sens.set_hexpand(true);
        scale_gr_sens.set_digits(2);
        scale_gr_sens.set_draw_value(true);
        // A unitless multiplier — same reasoning as Speed above.
        scale_gr_sens.update_property(&[gtk4::accessible::Property::Label("Visualizer beat sensitivity")]);
        {
            let state_rc = state.clone();
            sens_adj.connect_value_changed(move |a| {
                state_rc.borrow_mut().config.visualizer.granite.beat_sensitivity =
                    a.value().clamp(1.05, 3.0) as f32;
            });
        }
        gr_settings_box.attach(&scale_gr_sens, 1, 7, 1, 1);

        // Brighten colors on beats toggle.
        let lbl_gr_bright = Label::new(Some("Brighten colors on beats"));
        lbl_gr_bright.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_bright, 0, 8, 1, 1);
        let chk_gr_bright = CheckButton::new();
        chk_gr_bright.set_active(state.borrow().config.visualizer.granite.beat_brightness);
        {
            let state_rc = state.clone();
            chk_gr_bright.connect_toggled(move |c| {
                state_rc.borrow_mut().config.visualizer.granite.beat_brightness = c.is_active();
            });
        }
        gr_settings_box.attach(&chk_gr_bright, 1, 8, 1, 1);

        // Show/hide based on mode (mirrors Bars/Waveform pattern).
        gr_settings_box.set_visible(false);
        {
            let gr_settings = gr_settings_box.clone();
            dd_mode.connect_selected_notify(move |d| {
                gr_settings.set_visible(d.selected() == 2);
            });
        }
        {
            let gr_settings = gr_settings_box.clone();
            gr_settings.set_visible(
                state.borrow().config.visualizer.mode == VisualizerMode::Granite,
            );
        }
        grid.attach(&gr_settings_box, 0, 3, 2, 1);

        let tab_lbl = Label::with_mnemonic(SETTINGS_TAB_LABELS[2]);
        notebook.append_page(&settings_scroll_page(&grid), Some(&tab_lbl));
}
