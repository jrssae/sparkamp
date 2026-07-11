/// Open the 10-band parametric equalizer window.
///
/// The window shows a row of 10 vertical `Scale` sliders (one per band),
/// a preset `DropDown`, an Enable toggle, and a Reset button.
///
/// All control changes update `state.config.equalizer` immediately AND apply
/// to the live GStreamer pipeline so the user hears the result in real time.
/// Config is saved to disk when the window is closed.
fn open_eq_window(parent: Option<&gtk4::Window>, state: Rc<RefCell<AppState>>) -> gtk4::Window {
    use crate::config::EQ_PRESETS;
    use gtk4::{Adjustment, Box as GtkBox, CheckButton, DropDown, Label, Orientation, Scale};

    let win = gtk4::Window::new();
    win.set_title(Some("Equalizer — SparkAmp"));
    win.set_default_size(560, 240);
    win.set_resizable(false);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    let vbox = GtkBox::new(Orientation::Vertical, 8);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    // ── Enable toggle + preset row ───────────────────────────────────────────
    let top_row = GtkBox::new(Orientation::Horizontal, 8);

    let enable_btn = CheckButton::with_label("Enable EQ");
    enable_btn.set_active(state.borrow().config.equalizer.enabled);

    // Build preset list: the names + "Custom" entry at the end.
    let mut preset_names: Vec<&str> = EQ_PRESETS.iter().map(|(n, _)| *n).collect();
    preset_names.push("Custom");
    let preset_dd = DropDown::from_strings(&preset_names);
    preset_dd.set_tooltip_text(Some("EQ Preset"));

    // Select the current preset (or "Custom" if not found).
    {
        let current = state.borrow().config.equalizer.preset.clone();
        let idx = EQ_PRESETS
            .iter()
            .position(|(n, _)| *n == current)
            .unwrap_or(EQ_PRESETS.len()); // fallback: Custom
        preset_dd.set_selected(idx as u32);
    }

    let reset_btn = gtk4::Button::with_label("Reset");
    reset_btn.set_tooltip_text(Some("Set all bands to 0 dB"));

    top_row.append(&enable_btn);
    top_row.append(&preset_dd);
    top_row.append(&reset_btn);
    vbox.append(&top_row);

    // ── Pre-amp slider ────────────────────────────────────────────────────────
    let preamp_row = GtkBox::new(Orientation::Horizontal, 8);
    preamp_row.set_margin_top(4);
    preamp_row.set_margin_bottom(4);

    let preamp_label = Label::new(Some("Pre-amp"));
    preamp_label.set_halign(gtk4::Align::Start);
    preamp_label.set_width_request(70);

    let init_preamp = state.borrow().config.equalizer.preamp.clamp(0.5, 1.5);
    let preamp_adj = Adjustment::new(init_preamp, 0.5, 1.5, 0.01, 0.1, 0.0);
    let preamp_scale = Scale::new(Orientation::Horizontal, Some(&preamp_adj));
    preamp_scale.add_css_class("eq-scale");
    preamp_scale.set_hexpand(true);
    preamp_scale.set_draw_value(false);
    preamp_scale.add_mark(0.5, gtk4::PositionType::Bottom, Some("50%"));
    preamp_scale.add_mark(1.0, gtk4::PositionType::Bottom, Some("100%"));
    preamp_scale.add_mark(1.5, gtk4::PositionType::Bottom, Some("150%"));

    let preamp_pct_label = Label::new(Some(&format!("{:.0}%", init_preamp * 100.0)));
    preamp_pct_label.set_width_request(40);
    preamp_pct_label.set_halign(gtk4::Align::End);

    preamp_scale.set_sensitive(state.borrow().config.equalizer.enabled);

    preamp_row.append(&preamp_label);
    preamp_row.append(&preamp_scale);
    preamp_row.append(&preamp_pct_label);
    vbox.append(&preamp_row);

    preamp_scale.connect_value_changed({
        let state_rc = state.clone();
        let preamp_pct_label = preamp_pct_label.clone();
        move |s| {
            let clamped = s.value().clamp(0.5, 1.5);
            {
                let mut st = state_rc.borrow_mut();
                st.config.equalizer.preamp = clamped;
                st.player.set_preamp(clamped);
            }
            preamp_pct_label.set_text(&format!("{:.0}%", clamped * 100.0));
        }
    });

    // ── Band sliders ─────────────────────────────────────────────────────────
    // One column per band: frequency label on top, vertical scale in the middle,
    // gain-value label at the bottom.
    let bands_row = GtkBox::new(Orientation::Horizontal, 2);
    bands_row.set_hexpand(true);

    let mut sliders: Vec<Scale> = Vec::with_capacity(10);
    let bands_snapshot: Vec<f64> = {
        let eq = &state.borrow().config.equalizer;
        let mut v = eq.bands.clone();
        v.resize(10, 0.0);
        v
    };

    for i in 0..10 {
        let col = GtkBox::new(Orientation::Vertical, 2);
        col.set_hexpand(true);

        // Vertical scale: user-facing range ±12 dB (engine clamps internally).
        let adj = Adjustment::new(bands_snapshot[i].clamp(-12.0, 12.0),
                                  -12.0, 12.0, 1.0, 3.0, 0.0);
        let scale = Scale::new(Orientation::Vertical, Some(&adj));
        scale.add_css_class("eq-scale");
        scale.set_inverted(true); // top = positive, bottom = negative
        scale.set_draw_value(false);
        scale.set_vexpand(true);
        scale.set_height_request(100);
        scale.add_mark(0.0, gtk4::PositionType::Right, Some("0"));
        scale.add_mark(12.0, gtk4::PositionType::Right, Some("+12"));
        scale.add_mark(-12.0, gtk4::PositionType::Right, Some("-12"));
        col.append(&scale);

        // Gain value label (updated live as the slider moves).
        let gain_label = Label::new(Some(&format!("{:+.0}", bands_snapshot[i])));
        gain_label.set_halign(gtk4::Align::Center);
        col.append(&gain_label);

        // Wire slider to live-update engine + config.
        scale.connect_value_changed({
            let state_rc = state.clone();
            let gain_label = gain_label.clone();
            move |s| {
                let gain = s.value();
                gain_label.set_text(&format!("{:+.0}", gain));
                let mut st = state_rc.borrow_mut();
                if st.config.equalizer.bands.len() < 10 {
                    st.config.equalizer.bands.resize(10, 0.0);
                }
                st.config.equalizer.bands[i] = gain;
                st.config.equalizer.preset = String::new(); // custom
                if st.config.equalizer.enabled {
                    st.player.set_eq_band(i, gain);
                }
            }
        });

        sliders.push(scale);
        bands_row.append(&col);
    }
    vbox.append(&bands_row);

    // ── Enable toggle callback: apply / zero all bands ───────────────────────
    enable_btn.connect_toggled({
        let state_rc = state.clone();
        let sliders = sliders.clone();
        let preamp_sc = preamp_scale.clone();
        move |btn| {
            let enabled = btn.is_active();
            let mut st = state_rc.borrow_mut();
            st.config.equalizer.enabled = enabled;
            let effective = st.config.equalizer.effective_bands();
            st.player.apply_eq_bands(&effective);
            // Grey-out sliders when EQ is disabled.
            preamp_sc.set_sensitive(enabled);
            for s in &sliders {
                s.set_sensitive(enabled);
            }
        }
    });

    // ── Preset dropdown callback ──────────────────────────────────────────────
    preset_dd.connect_selected_notify({
        let state_rc = state.clone();
        let sliders = sliders.clone();
        move |dd| {
            let idx = dd.selected() as usize;
            if idx >= EQ_PRESETS.len() {
                return;
            } // "Custom"
            let (name, bands) = EQ_PRESETS[idx];
            let mut st = state_rc.borrow_mut();
            st.config.equalizer.preset = name.to_string();
            st.config.equalizer.bands = bands.to_vec();
            // Move sliders without retriggering the band change callback.
            drop(st); // release borrow before calling set_value
            for (i, s) in sliders.iter().enumerate() {
                s.set_value(bands[i]);
            }
            // Re-borrow mutably to apply to engine.
            let mut st = state_rc.borrow_mut();
            if st.config.equalizer.enabled {
                st.player.apply_eq_bands(&bands);
            }
        }
    });

    // ── Reset button: all bands to 0 dB ──────────────────────────────────────
    reset_btn.connect_clicked({
        let state_rc = state.clone();
        let sliders = sliders.clone();
        let preset_dd = preset_dd.clone();
        move |_| {
            let flat = [0.0f64; 10];
            // Find "Flat" preset index to select it, or leave as Custom.
            let flat_idx = EQ_PRESETS
                .iter()
                .position(|(n, _)| *n == "Flat")
                .unwrap_or(EQ_PRESETS.len());
            preset_dd.set_selected(flat_idx as u32);
            let mut st = state_rc.borrow_mut();
            st.config.equalizer.preset = "Flat".to_string();
            st.config.equalizer.bands = flat.to_vec();
            drop(st);
            for (i, s) in sliders.iter().enumerate() {
                s.set_value(flat[i]);
            }
            let mut st = state_rc.borrow_mut();
            st.player.apply_eq_bands(&flat);
        }
    });

    // ── Save config on close ─────────────────────────────────────────────────
    win.connect_close_request({
        let state_rc = state.clone();
        move |_w| {
            let _ = state_rc.borrow().config.save();
            glib::Propagation::Proceed
        }
    });

    win.set_child(Some(&vbox));
    win.set_hide_on_close(true);
    win.present();
    win
}

// ---------------------------------------------------------------------------
// Deduplication window
// ---------------------------------------------------------------------------

