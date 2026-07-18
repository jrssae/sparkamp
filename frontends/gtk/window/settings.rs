fn open_settings_window(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    initial_tab: Option<u32>,
    css_provider: Rc<gtk4::CssProvider>,
    text_rgba: Rc<RefCell<gdk::RGBA>>,
    accent_rgba: Rc<RefCell<Option<gdk::RGBA>>>,
    rebuild_playlist: Rc<dyn Fn()>,
) {
    let win = gtk4::Window::new();
    win.set_title(Some("Settings — Sparkamp"));
    win.set_default_size(480, 340);
    win.set_resizable(false);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    let notebook = Notebook::new();
    notebook.set_margin_top(8);
    notebook.set_margin_bottom(8);
    notebook.set_margin_start(8);
    notebook.set_margin_end(8);

    // ── Tab 0: Appearance ─────────────────────────────────────────────────
    {
        use gtk4::{Box as GtkBox, Button, Label, ListBox, ListBoxRow, Orientation,
                   PolicyType, ScrolledWindow, SelectionMode, FileDialog, FileFilter};

        let root = GtkBox::new(Orientation::Vertical, 10);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_margin_start(16);
        root.set_margin_end(16);

        // Header
        let header = Label::new(Some("Skin"));
        header.set_halign(Align::Start);
        header.add_css_class("heading");
        root.append(&header);

        // Scrollable list of skins
        let listbox = ListBox::new();
        listbox.set_selection_mode(SelectionMode::Single);
        listbox.add_css_class("rich-list");

        let scrolled = ScrolledWindow::new();
        scrolled.set_policy(PolicyType::Never, PolicyType::Automatic);
        scrolled.set_min_content_height(200);
        scrolled.set_child(Some(&listbox));
        root.append(&scrolled);

        // Suppress the row_selected handler while we programmatically
        // re-select the active row after rebuild. GtkNotebook tab switches
        // can also fire spurious row_selected events on re-show; we only
        // want user clicks to apply a skin.
        let suppress_sel: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        // Populate rows
        let rebuild_list = {
            let listbox = listbox.clone();
            let state_rc = state.clone();
            let suppress = suppress_sel.clone();
            Rc::new(move || {
                suppress.set(true);
                while let Some(row) = listbox.first_child() {
                    listbox.remove(&row);
                }
                let hidden = state_rc.borrow().config.appearance.hidden_skins.clone();
                let entries = crate::skin::list_skins(&hidden);
                let active = state_rc.borrow().config.appearance.active_skin.clone();
                let mut active_row: Option<ListBoxRow> = None;

                for entry in entries {
                    let row = ListBoxRow::new();
                    let hbox = GtkBox::new(Orientation::Horizontal, 8);
                    hbox.set_margin_top(4);
                    hbox.set_margin_bottom(4);
                    hbox.set_margin_start(8);
                    hbox.set_margin_end(8);

                    let name_lbl = Label::new(Some(&entry.display_name));
                    name_lbl.set_halign(Align::Start);
                    name_lbl.set_hexpand(true);
                    hbox.append(&name_lbl);

                    if entry.is_builtin {
                        let tag = Label::new(Some("(built-in)"));
                        tag.add_css_class("dim-label");
                        hbox.append(&tag);
                    }

                    if entry.name == active {
                        let mark = Label::new(Some("● Active"));
                        mark.add_css_class("dim-label");
                        hbox.append(&mark);
                    }

                    row.set_child(Some(&hbox));
                    row.set_widget_name(&entry.name);
                    listbox.append(&row);
                    if entry.name == active {
                        active_row = Some(row);
                    }
                }
                if let Some(r) = active_row {
                    listbox.select_row(Some(&r));
                }
                suppress.set(false);
            })
        };
        rebuild_list();

        // Selecting a row applies the skin live.
        {
            let state_rc = state.clone();
            let provider = css_provider.clone();
            let text_rgba = text_rgba.clone();
            let accent_rgba = accent_rgba.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let rebuild = rebuild_list.clone();
            let suppress = suppress_sel.clone();
            listbox.connect_row_selected(move |_, row| {
                if suppress.get() { return; }
                let Some(row) = row else { return };
                let name = row.widget_name().to_string();
                if name.is_empty() { return; }
                // User-clicked a row while the skin was already active
                // (e.g., re-click to re-apply) — nothing to do.
                if state_rc.borrow().config.appearance.active_skin == name {
                    return;
                }
                let Some(skin) = crate::skin::load_skin(&name) else { return };
                let css = crate::skin::render_gtk_css(&skin.vars);
                provider.load_from_data(&css);
                if let Some(gtk_settings) = gtk4::Settings::default() {
                    gtk_settings.set_gtk_application_prefer_dark_theme(
                        skin.vars.background.luminance() < 0.5);
                }
                *text_rgba.borrow_mut() = gdk::RGBA::new(
                    skin.vars.text_color.r as f32 / 255.0,
                    skin.vars.text_color.g as f32 / 255.0,
                    skin.vars.text_color.b as f32 / 255.0,
                    1.0,
                );
                // Playlist TreeView stores fg color per-row via RGBA column;
                // update the shared accent from the new skin's highlight so
                // the playing row re-renders in the new skin's accent rather
                // than the color captured at startup.
                *accent_rgba.borrow_mut() = Some(gdk::RGBA::new(
                    skin.vars.highlight.r as f32 / 255.0,
                    skin.vars.highlight.g as f32 / 255.0,
                    skin.vars.highlight.b as f32 / 255.0,
                    1.0,
                ));
                state_rc.borrow_mut().config.appearance.active_skin = name;
                // Refresh all playlist rows so the new text / accent colors
                // propagate — CSS alone doesn't reach the deprecated cell
                // renderer's foreground-rgba column.
                rebuild_pl();
                rebuild();
            });
        }

        // Row of action buttons
        let btn_row = GtkBox::new(Orientation::Horizontal, 8);
        let btn_add = Button::with_label("Add skin…");
        let btn_remove = Button::with_label("Remove");
        let btn_download = Button::with_label("Download skin…");
        btn_row.append(&btn_add);
        btn_row.append(&btn_remove);
        btn_row.append(&btn_download);
        root.append(&btn_row);

        // Wire Add
        {
            let state_rc = state.clone();
            let rebuild = rebuild_list.clone();
            let listbox = listbox.clone();
            let win_ref = win.clone();
            btn_add.connect_clicked(move |_| {
                let dialog = FileDialog::new();
                dialog.set_title("Add Sparkamp skin");
                let filter = FileFilter::new();
                filter.add_suffix("css");
                filter.set_name(Some("Sparkamp skin (*.css)"));
                let filters = gio::ListStore::new::<FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));

                let state_rc = state_rc.clone();
                let rebuild = rebuild.clone();
                let listbox = listbox.clone();
                let win_alert = win_ref.clone();
                dialog.open(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    match crate::skin::add_user_skin(&path) {
                        Ok(entry) => {
                            state_rc.borrow_mut().config.appearance.active_skin =
                                entry.name.clone();
                            state_rc.borrow_mut().config.appearance.hidden_skins
                                .retain(|n| !n.eq_ignore_ascii_case(&entry.name));
                            rebuild();
                            if let Some(row) = find_row_by_name(&listbox, &entry.name) {
                                listbox.select_row(Some(&row));
                            }
                        }
                        Err(e) => {
                            show_alert_parented(
                                Some(&win_alert),
                                &format!("Could not add skin: {e}"),
                            );
                        }
                    }
                });
            });
        }

        // Wire Remove (disabled for built-ins)
        {
            let state_rc = state.clone();
            let rebuild = rebuild_list.clone();
            let listbox = listbox.clone();
            btn_remove.connect_clicked(move |_| {
                let Some(row) = listbox.selected_row() else { return };
                let name = row.widget_name().to_string();
                if name == "dark" || name == "light" || name.is_empty() {
                    return;
                }
                {
                    let mut s = state_rc.borrow_mut();
                    if !s.config.appearance.hidden_skins.iter().any(|h| h.eq_ignore_ascii_case(&name)) {
                        s.config.appearance.hidden_skins.push(name.clone());
                    }
                    if s.config.appearance.active_skin == name {
                        s.config.appearance.active_skin = "dark".to_string();
                    }
                }
                rebuild();
            });
        }

        // Update Remove-disabled state reactively on selection changes.
        {
            let btn_remove = btn_remove.clone();
            listbox.connect_row_selected(move |_, row| {
                let name = row.map(|r| r.widget_name().to_string()).unwrap_or_default();
                let is_builtin = name == "dark" || name == "light" || name.is_empty();
                btn_remove.set_sensitive(!is_builtin);
            });
        }

        // Wire Download (Export template CSS…)
        {
            let listbox = listbox.clone();
            let win_ref = win.clone();
            btn_download.connect_clicked(move |_| {
                let Some(row) = listbox.selected_row() else { return };
                let name = row.widget_name().to_string();
                let Some(skin) = crate::skin::load_skin(&name) else { return };

                let dialog = FileDialog::new();
                dialog.set_title("Save Sparkamp skin");
                dialog.set_initial_name(Some(&format!("{name}.css")));

                let skin_copy = skin.clone();
                dialog.save(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let css = match &skin_copy.source {
                        crate::skin::SkinSource::BuiltIn => match skin_copy.name.as_str() {
                            "dark" => crate::skin::DARK_TEMPLATE_CSS.to_string(),
                            "light" => crate::skin::LIGHT_TEMPLATE_CSS.to_string(),
                            _ => crate::skin::DARK_TEMPLATE_CSS.to_string(),
                        },
                        crate::skin::SkinSource::UserFile(p) => {
                            std::fs::read_to_string(p).unwrap_or_default()
                        }
                    };
                    let _ = std::fs::write(&path, css);
                });
            });
        }

        // Separator
        let sep = gtk4::Separator::new(Orientation::Horizontal);
        sep.set_margin_top(8);
        sep.set_margin_bottom(8);
        root.append(&sep);

        // Documentation header + button
        let doc_header = Label::new(Some("Documentation"));
        doc_header.set_halign(Align::Start);
        doc_header.add_css_class("heading");
        root.append(&doc_header);

        let btn_guide = Button::with_label("Export how-to guide…");
        root.append(&btn_guide);
        {
            let win_ref = win.clone();
            btn_guide.connect_clicked(move |_| {
                let dialog = FileDialog::new();
                dialog.set_title("Save Sparkamp skin guide");
                dialog.set_initial_name(Some("sparkamp-skin-guide.md"));
                dialog.save(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let _ = std::fs::write(&path, crate::skin::SKIN_GUIDE_MD);
                });
            });
        }

        let tab_lbl = Label::new(Some("Appearance"));
        notebook.append_page(&root, Some(&tab_lbl));
    }

    // ── Tab 1: Behavior ───────────────────────────────────────────────────
    {
        use crate::config::PlaylistAddBehavior;

        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        let lbl = Label::new(Some("Autoplay on add"));
        lbl.set_halign(Align::Start);
        grid.attach(&lbl, 0, 0, 1, 1);

        let chk = CheckButton::new();
        chk.set_active(state.borrow().config.behavior.autoplay_on_add);
        {
            let state_rc = state.clone();
            chk.connect_toggled(move |c| {
                state_rc.borrow_mut().config.behavior.autoplay_on_add = c.is_active();
            });
        }
        grid.attach(&chk, 1, 0, 1, 1);

        // Row 1: Default playlist behavior for media library add.
        let lbl_add = Label::new(Some("Media library → playlist"));
        lbl_add.set_halign(Align::Start);
        grid.attach(&lbl_add, 0, 1, 1, 1);

        let dd_add = DropDown::from_strings(&["Append to current", "Replace current"]);
        {
            let behavior = state.borrow().config.behavior.playlist_add_behavior.clone();
            dd_add.set_selected(match behavior {
                PlaylistAddBehavior::Append => 0,
                PlaylistAddBehavior::Replace => 1,
            });
        }
        {
            let state_rc = state.clone();
            dd_add.connect_selected_notify(move |d| {
                let behavior = match d.selected() {
                    1 => PlaylistAddBehavior::Replace,
                    _ => PlaylistAddBehavior::Append,
                };
                state_rc.borrow_mut().config.behavior.playlist_add_behavior = behavior;
            });
        }
        grid.attach(&dd_add, 1, 1, 1, 1);

        // Row 2: gnudb email — used for the CDDB/gnudb handshake on disc
        // identify and (later) submission. Stored locally only.
        let lbl_email = Label::new(Some("gnudb email"));
        lbl_email.set_halign(Align::Start);
        lbl_email.set_tooltip_text(Some(
            "Your email for the gnudb/CDDB handshake — needed to identify and \
             submit disc metadata. Stored locally and used only to talk to gnudb.",
        ));
        grid.attach(&lbl_email, 0, 2, 1, 1);

        let email_entry = gtk4::Entry::new();
        email_entry.set_hexpand(true);
        email_entry.set_placeholder_text(Some("you@example.com"));
        email_entry.set_text(&gtk_safe(&state.borrow().config.disc.gnudb_email));
        {
            let state_rc = state.clone();
            email_entry.connect_changed(move |e| {
                let mut s = state_rc.borrow_mut();
                s.config.disc.gnudb_email = e.text().to_string();
                let _ = s.config.save();
            });
        }
        grid.attach(&email_entry, 1, 2, 1, 1);

        // Auto-open on audio-CD insert (mirrors the macOS Settings toggle;
        // the app-level insertion watcher reads this live).
        let lbl_autocd = Label::builder()
            .label("Audio CD inserted")
            .halign(Align::Start)
            .build();
        lbl_autocd.set_tooltip_text(Some(
            "When an audio CD is inserted, open the Media Library on that \
             drive's view. Also covers a CD already in the drive at launch, \
             so setting Sparkamp as the system's CD handler lands on the disc.",
        ));
        grid.attach(&lbl_autocd, 0, 3, 1, 1);
        let chk_autocd = CheckButton::with_label("Open the Media Library");
        chk_autocd.set_active(state.borrow().config.disc.auto_show_inserted_audio_cd);
        {
            let state_rc = state.clone();
            chk_autocd.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.disc.auto_show_inserted_audio_cd = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_autocd, 1, 3, 1, 1);

        // gnudb test mode (mirrors the macOS Settings toggle).
        let lbl_gnudb_test = Label::builder()
            .label("gnudb submissions")
            .halign(Align::Start)
            .build();
        lbl_gnudb_test.set_tooltip_text(Some(
            "gnudb validates test submissions without publishing them. \
             Turn off once a real submission is confirmed working.",
        ));
        grid.attach(&lbl_gnudb_test, 0, 4, 1, 1);
        let chk_gnudb_test = CheckButton::with_label("Submit in test mode");
        chk_gnudb_test.set_active(state.borrow().config.disc.gnudb_submit_mode_test);
        {
            let state_rc = state.clone();
            chk_gnudb_test.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.disc.gnudb_submit_mode_test = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_gnudb_test, 1, 4, 1, 1);

        // OS handler registration shortcut (mirrors the macOS "Open CDs &
        // DVDs Settings…" button): GNOME's "CD audio" handler choice lives
        // in Settings → Removable Media. We never write that preference
        // ourselves — the user points it at Sparkamp once there.
        let lbl_handler = Label::builder()
            .label("System CD handler")
            .halign(Align::Start)
            .build();
        lbl_handler.set_tooltip_text(Some(
            "To have GNOME launch Sparkamp automatically on insert, pick it \
             under \"CD audio\" in Settings → Removable Media.",
        ));
        grid.attach(&lbl_handler, 0, 5, 1, 1);
        let btn_handler = Button::with_label("Open Removable Media Settings…");
        btn_handler.add_css_class("pl-btn");
        {
            let win_alert = win.clone();
            btn_handler.connect_clicked(move |_| {
                let launched = std::process::Command::new("gnome-control-center")
                    .arg("removable-media")
                    .spawn()
                    .is_ok();
                if !launched {
                    show_alert_parented(
                        Some(&win_alert),
                        "Couldn't open GNOME Settings. Open Settings → Removable \
                         Media yourself and pick Sparkamp under \"CD audio\".",
                    );
                }
            });
        }
        grid.attach(&btn_handler, 1, 5, 1, 1);

        let tab_lbl = Label::new(Some("Behavior"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab 2: Visualizer ─────────────────────────────────────────────────
    {
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

        let tab_lbl = Label::new(Some("Visualizer"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab 3: Filetypes ──────────────────────────────────────────────────
    {
        use crate::config::PlaylistFormat;
        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        // Preferred playlist format for new saves.
        let lbl_fmt = Label::new(Some("Playlist format"));
        lbl_fmt.set_halign(Align::Start);
        grid.attach(&lbl_fmt, 0, 0, 1, 1);

        let dd_fmt = DropDown::from_strings(&["m3u8", "m3u"]);
        dd_fmt.set_selected(match state.borrow().config.media_library.playlist_format {
            PlaylistFormat::M3u8 => 0,
            PlaylistFormat::M3u => 1,
        });
        {
            let state_rc = state.clone();
            dd_fmt.connect_selected_notify(move |d| {
                let fmt = if d.selected() == 1 {
                    PlaylistFormat::M3u
                } else {
                    PlaylistFormat::M3u8
                };
                state_rc.borrow_mut().config.media_library.playlist_format = fmt;
            });
        }
        grid.attach(&dd_fmt, 1, 0, 1, 1);

        let hint = Label::new(Some(
            "New playlists, Save As, and device exports use this format. \
             Existing playlists keep their own.",
        ));
        hint.set_halign(Align::Start);
        hint.set_wrap(true);
        hint.add_css_class("status-label");
        grid.attach(&hint, 0, 1, 2, 1);

        let tab_lbl = Label::new(Some("Filetypes"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab 4: Media Library (watched folders) ───────────────────────────
    {
        let grid = Grid::new();
        grid.set_row_spacing(8);
        grid.set_column_spacing(12);
        grid.set_margin_top(12);
        grid.set_margin_bottom(12);
        grid.set_margin_start(12);
        grid.set_margin_end(12);

        // Row 0: Label + button row
        let lbl_folders = Label::new(Some("Watched folders:"));
        lbl_folders.set_halign(Align::Start);

        let btn_add_folder = Button::with_label("Add Folder…");
        let btn_remove = Button::with_label("Remove");
        btn_remove.set_sensitive(false);

        let folder_list = ListBox::new();
        folder_list.add_css_class("playlist");
        folder_list.set_selection_mode(gtk4::SelectionMode::Single);

        let folder_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .min_content_height(200)
            .width_request(300)
            .child(&folder_list)
            .build();

        let status_lbl = Label::new(None);
        status_lbl.set_halign(Align::Start);
        status_lbl.add_css_class("dim-label");

        let rebuild_list = {
            let state_rc = state.clone();
            let folder_list_rc = folder_list.clone();
            let status_rc = status_lbl.clone();
            let btn_rm = btn_remove.clone();
            Rc::new(move || {
                // Snapshot folders before mutating the list.
                let folders: Vec<(i64, String)> = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.list_folders().ok())
                    .unwrap_or_default();

                // Remove all rows.
                while let Some(child) = folder_list_rc.first_child() {
                    folder_list_rc.remove(&child);
                }

                // Repopulate.
                for (_, path) in &folders {
                    let row = gtk4::ListBoxRow::new();
                    let row_box = GtkBox::new(Orientation::Horizontal, 6);
                    let icon = Image::from_icon_name("folder-open");
                    let lbl = Label::new(Some(path));
                    lbl.set_hexpand(true);
                    lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                    lbl.set_halign(Align::Start);
                    row_box.append(&icon);
                    row_box.append(&lbl);
                    row.set_child(Some(&row_box));
                    row.set_activatable(true);
                    folder_list_rc.append(&row);
                }

                btn_rm.set_sensitive(!folders.is_empty());

                let count = folders.len();
                status_rc.set_text(&match count {
                    0 => "No folders — click \"Add Folder…\" to add music".to_string(),
                    1 => "1 folder".to_string(),
                    n => format!("{n} folders"),
                });
            })
        };

        rebuild_list();

        // Filled once the Rescan button is built (below). Lets "Add Folder"
        // trigger a rescan after a concurrent scan finishes.
        let rescan_holder: Rc<RefCell<Option<Button>>> = Rc::new(RefCell::new(None));

        let rebuild_for_add = rebuild_list.clone();
        let status_for_add = status_lbl.clone();
        let state_for_add = state.clone();
        let win_add = win.downgrade();
        let rescan_holder_add = rescan_holder.clone();
        btn_add_folder.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Select Music Folder")
                .build();
            let rebuild_cb = rebuild_for_add.clone();
            let status_rc = status_for_add.clone();
            let state_rc = state_for_add.clone();
            let rescan_holder = rescan_holder_add.clone();
            dialog.select_folder(
                win_add.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |result| {
                    let path = match result {
                        Ok(f) => f.path().map(|p| p.to_string_lossy().into_owned()),
                        Err(_) => None,
                    };
                    let Some(path_str) = path else {
                        return;
                    };
                    // A scan is already running (only one metadata scan may run
                    // at a time). Register + fast-scan the folder now so it
                    // appears immediately, then queue a full rescan to pick up
                    // its metadata once the current scan finishes.
                    if state_rc.borrow().ml_scan.is_some() {
                        let db_path = crate::media_library::MediaLibrary::db_path_pub();
                        let path_for_thread = path_str.clone();
                        status_rc.set_text(
                            "Adding folder — it will be scanned after the current scan finishes…",
                        );
                        let (fast_tx, fast_rx) =
                            std::sync::mpsc::channel::<Result<(), String>>();
                        std::thread::spawn(move || {
                            let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                                Ok(l) => l,
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                    return;
                                }
                            };
                            let folder_id = match lib.add_folder(&path_for_thread) {
                                Ok(r) => r.id(),
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("Could not add: {e}")));
                                    return;
                                }
                            };
                            if let Err(e) = lib.rescan_folder_fast(folder_id, &path_for_thread) {
                                let _ = fast_tx.send(Err(format!("Fast scan error: {e}")));
                                return;
                            }
                            let _ = fast_tx.send(Ok(()));
                        });
                        let fast_rx = std::cell::RefCell::new(fast_rx);
                        let fast_done = std::cell::Cell::new(false);
                        let rebuild_q = rebuild_cb.clone();
                        let status_q = status_rc.clone();
                        let state_q = state_rc.clone();
                        let rescan_q = rescan_holder.clone();
                        glib::timeout_add_local(
                            std::time::Duration::from_millis(400),
                            move || {
                                if !fast_done.get() {
                                    match fast_rx.borrow().try_recv() {
                                        Ok(Ok(())) => {
                                            fast_done.set(true);
                                            rebuild_q();
                                            if let Some(ref cb) =
                                                state_q.borrow().rebuild_ml_callback
                                            {
                                                cb();
                                            }
                                            status_q.set_text("Folder added — waiting to scan…");
                                        }
                                        Ok(Err(e)) => {
                                            status_q.set_text(&e);
                                            return glib::ControlFlow::Break;
                                        }
                                        Err(_) => {}
                                    }
                                    return glib::ControlFlow::Continue;
                                }
                                // Fast add done; once the running scan ends,
                                // trigger a rescan to scan the new folder.
                                if state_q.borrow().ml_scan.is_none() {
                                    if let Some(btn) = rescan_q.borrow().as_ref() {
                                        btn.emit_clicked();
                                    }
                                    return glib::ControlFlow::Break;
                                }
                                glib::ControlFlow::Continue
                            },
                        );
                        return;
                    }
                    let path_for_thread = path_str.clone();

                    let cancel_flag = start_ml_scan(&state_rc, ScanType::AddFolder, 0);
                    status_rc.set_text("Reading tags…");

                    // Three channels: fast done, metadata progress, final result.
                    let (fast_tx, fast_rx) = std::sync::mpsc::channel::<Result<usize, String>>();
                    let (progress_tx, progress_rx) = std::sync::mpsc::channel::<(usize, usize)>();
                    let (result_tx, result_rx) =
                        std::sync::mpsc::channel::<Result<(bool, usize), String>>();

                    std::thread::spawn(move || {
                        let lib = match crate::media_library::MediaLibrary::open_at(
                            &crate::media_library::MediaLibrary::db_path_pub(),
                        ) {
                            Ok(l) => l,
                            Err(e) => {
                                let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                return;
                            }
                        };

                        let folder_id = match lib.add_folder(&path_for_thread) {
                            Ok(r) => r.id(),
                            Err(e) => {
                                let _ = fast_tx.send(Err(format!("Could not add: {e}")));
                                return;
                            }
                        };

                        // Phase 1: fast scan
                        if let Err(e) = lib.rescan_folder_fast(folder_id, &path_for_thread) {
                            let _ = fast_tx.send(Err(format!("Fast scan error: {e}")));
                            return;
                        }
                        let _ = fast_tx.send(Ok(0usize));

                        // Phase 2: metadata scan. Reset tracks with no metadata first
                        // so scan_folder picks up any that a previous scan missed.
                        let _ = lib.reset_unscanned_metadata();
                        let count = lib
                            .scan_folder(folder_id, &cancel_flag, |c, t| {
                                let _ = progress_tx.send((c, t));
                            })
                            .map(|(scanned, _, _)| scanned)
                            .unwrap_or(0);
                        let _ = result_tx.send(Ok((true, count)));
                    });

                    let fast_rx = std::cell::RefCell::new(fast_rx);
                    let progress_rx = std::cell::RefCell::new(progress_rx);
                    let result_rx = std::cell::RefCell::new(result_rx);
                    let fast_handled = std::cell::Cell::new(false);
                    let path_str_clone = path_str.clone();
                    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                        // Handle fast scan completion
                        if !fast_handled.get() {
                            if let Ok(fast_result) = fast_rx.borrow().try_recv() {
                                fast_handled.set(true);
                                if let Err(e) = fast_result {
                                    status_rc.set_text(&e);
                                    complete_ml_scan(&state_rc);
                                    return glib::ControlFlow::Break;
                                }
                                rebuild_cb();
                                // Rebuild ML window to show added files
                                if let Some(ref cb) = state_rc.borrow().rebuild_ml_callback {
                                    cb();
                                }
                            }
                        }

                        // Drain progress updates
                        while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                            update_ml_scan_progress(&state_rc, current, total);
                            status_rc.set_text(&format!("Reading tags {}/{}…", current, total));
                        }

                        // Check for completion
                        if let Ok(result) = result_rx.borrow().try_recv() {
                            rebuild_cb();
                            match result {
                                Err(e) => status_rc.set_text(&e),
                                Ok((_, count)) => {
                                    let path_short = if path_str_clone.len() > 40 {
                                        format!("{}…", &path_str_clone[..40])
                                    } else {
                                        path_str_clone.clone()
                                    };
                                    status_rc.set_text(&format!(
                                        "Added: {} ({} tracks)",
                                        path_short, count
                                    ));
                                }
                            }
                            if let Some(ref cb) = state_rc.borrow().rebuild_ml_callback {
                                cb();
                            }
                            complete_ml_scan(&state_rc);
                            return glib::ControlFlow::Break;
                        }

                        glib::ControlFlow::Continue
                    });
                },
            );
        });

        let btn_rm_state = state.clone();
        let btn_rm_rebuild = rebuild_list.clone();
        let btn_rm_status = status_lbl.clone();
        let btn_rm_list = folder_list.clone();
        let btn_rm_win = win.downgrade();
        btn_remove.connect_clicked(move |_| {
            let idx = btn_rm_list.selected_row().map(|r| r.index() as usize);
            if let Some(idx) = idx {
                let folders: Vec<(i64, String)> = btn_rm_state
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.list_folders().ok())
                    .unwrap_or_default();
                if idx < folders.len() {
                    let (folder_id, folder_path) = folders[idx].clone();

                    // Clone for use in dialog callback
                    let state_for_dialog = btn_rm_state.clone();
                    let rebuild_for_dialog = btn_rm_rebuild.clone();
                    let status_for_dialog = btn_rm_status.clone();
                    let win_for_dialog = btn_rm_win.clone();

                    let dialog = gtk4::AlertDialog::builder()
                        .message("Remove Folder from Library")
                        .detail("Removing this folder will remove all files in this folder from the media library.\n\nNo files will be deleted from your disk, but they will not appear in the library any longer.\n\nContinue?")
                        .buttons(vec!["Cancel".to_string(), "Continue".to_string()])
                        .cancel_button(0)
                        .default_button(0)
                        .modal(true)
                        .build();

                    let folder_id_cb = folder_id;
                    let folder_path_cb = folder_path.clone();

                    dialog.choose(
                        win_for_dialog.upgrade().as_ref(),
                        None::<&gio::Cancellable>,
                        move |result| {
                            if result == Ok(1) {
                                status_for_dialog.set_text(&format!("Removing: {}", folder_path_cb));

                                // Soft-delete the tracks AND delete the folder
                                // row on the main thread so the watched-folder
                                // list reflects the removal immediately (the
                                // folder row is what `list_folders` reads). The
                                // heavy purge runs in the background.
                                if let Some(ref lib) = state_for_dialog.borrow().media_lib {
                                    if let Ok(track_ids) = lib.track_ids_for_folder(folder_id_cb) {
                                        let _ = lib.soft_delete_tracks(&track_ids);
                                    }
                                    let _ = lib.remove_folder(folder_id_cb);
                                }

                                // Rebuild UI immediately — folder is now gone.
                                rebuild_for_dialog();
                                status_for_dialog.set_text(&format!("Removed: {}", folder_path_cb));

                                // Trigger Media Library window to refresh if open
                                if let Some(ref cb) = state_for_dialog.borrow().rebuild_ml_callback {
                                    cb();
                                }

                                // Background: purge the soft-deleted track rows.
                                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                                std::thread::spawn(move || {
                                    if let Ok(lib) =
                                        crate::media_library::MediaLibrary::open_at(&db_path)
                                    {
                                        let _ = lib.purge_deleted_tracks();
                                    }
                                });
                            }
                        },
                    );
                }
            }
        });

        grid.attach(&lbl_folders, 0, 0, 2, 1);
        grid.attach(&btn_add_folder, 2, 0, 1, 1);
        grid.attach(&btn_remove, 3, 0, 1, 1);
        grid.attach(&folder_scroll, 0, 1, 4, 1);
        grid.attach(&status_lbl, 0, 2, 4, 1);

        // Row 3: Rescan button (shares state with media library window).
        let lbl_rescan = Label::new(Some("Scan:"));
        lbl_rescan.set_halign(Align::Start);

        let btn_rescan = Button::with_label("⟳ Rescan");
        let btn_cancel_scan = Button::with_label("✕ Cancel Scan");
        btn_cancel_scan.set_visible(false);
        // Let "Add Folder" trigger a rescan once a concurrent scan finishes.
        *rescan_holder.borrow_mut() = Some(btn_rescan.clone());

        let status_scan = Label::new(None);
        status_scan.set_halign(Align::Start);
        status_scan.add_css_class("dim-label");

        // Update button visibility based on scan state.
        // Clone references for the closure to avoid moving the originals.
        let state_rc_for_update = state.clone();
        let btn_rescan_ref = btn_rescan.clone();
        let btn_cancel_ref = btn_cancel_scan.clone();
        let btn_add_folder_ref = btn_add_folder.clone();
        let status_ref = status_scan.clone();
        let update_scan_ui = Rc::new(move || {
            let scan_state = state_rc_for_update.borrow().ml_scan.clone();
            if let Some(scan) = scan_state {
                btn_rescan_ref.set_visible(false);
                btn_cancel_ref.set_visible(true);
                // Disable Add Folder so a second concurrent scan cannot be started.
                btn_add_folder_ref.set_sensitive(false);
                if scan.total > 0 {
                    status_ref.set_text(&format!("Scanning {} / {}…", scan.current, scan.total));
                } else {
                    status_ref.set_text("Scanning…");
                }
            } else {
                btn_rescan_ref.set_visible(true);
                btn_cancel_ref.set_visible(false);
                btn_add_folder_ref.set_sensitive(true);
                status_ref.set_text("");
            }
        });

        // Initial UI state.
        update_scan_ui();

        // Refresh scan UI when this tab is shown.
        {
            let update_cb = update_scan_ui.clone();
            notebook.connect_switch_page(move |_, _, _| {
                update_cb();
            });
        }

        // Rescan button: trigger a full rescan of all watched folders.
        // Note: This shares state with the media library window via state.ml_scan.
        {
            let state_rc = state.clone();
            let btn_rescan_ref = btn_rescan.clone();
            let btn_cancel_ref = btn_cancel_scan.clone();
            let status_ref = status_scan.clone();

            btn_rescan.connect_clicked(move |_| {
                if state_rc.borrow().ml_scan.is_some() {
                    status_ref.set_text("Scan already in progress");
                    return;
                }
                if state_rc.borrow().media_lib.is_none() {
                    status_ref.set_text("Error: Media library not available");
                    return;
                }

                let db_path = crate::media_library::MediaLibrary::db_path_pub();

                let cancel_flag = start_ml_scan(&state_rc, ScanType::Rescan, 0);
                status_ref.set_text("Reading tags…");
                btn_rescan_ref.set_sensitive(false);
                btn_cancel_ref.set_visible(true);

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
                    // Clear last_scanned for tracks with no metadata so scan_folder
                    // re-processes them (handles recovery from a prior broken scan).
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
                let status_ref2 = status_ref.clone();
                let btn_rescan_ref2 = btn_rescan_ref.clone();
                let btn_cancel_ref2 = btn_cancel_ref.clone();
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
                        if let Some(ref cb) = state_rc2.borrow().rebuild_ml_callback {
                            cb();
                        }
                        match result {
                            Err(e) => status_ref2.set_text(&format!("Rescan error: {}", e)),
                            Ok(_) => status_ref2.set_text("Scan complete"),
                        }
                        btn_rescan_ref2.set_sensitive(true);
                        btn_cancel_ref2.set_visible(false);
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
            });
        }

        // Cancel scan button.
        {
            let state_rc = state.clone();
            let status_ref = status_scan.clone();
            btn_cancel_scan.connect_clicked(move |_| {
                cancel_ml_scan(&state_rc);
                status_ref.set_text("Cancelling…");
            });
        }

        // Polling timer to sync scan state with UI.
        {
            let update_ui = update_scan_ui.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                update_ui();
                glib::ControlFlow::Continue
            });
        }

        grid.attach(&lbl_rescan, 0, 3, 1, 1);
        grid.attach(&btn_rescan, 1, 3, 1, 1);
        grid.attach(&btn_cancel_scan, 1, 3, 1, 1);
        grid.attach(&status_scan, 2, 3, 2, 1);

        // Row 4: Deduplication
        let sep_row4 = gtk4::Separator::new(Orientation::Horizontal);
        sep_row4.set_margin_top(4);
        sep_row4.set_margin_bottom(4);
        grid.attach(&sep_row4, 0, 4, 4, 1);

        let btn_dedupe = Button::with_label("Deduplicate Music…");
        btn_dedupe.set_tooltip_text(Some(
            "Find tracks that appear more than once in your library",
        ));
        btn_dedupe.set_hexpand(false);
        btn_dedupe.set_halign(Align::Start);
        {
            let state_rc = state.clone();
            let win_wk = win.downgrade();
            btn_dedupe.connect_clicked(move |_| {
                open_dedupe_window(
                    win_wk.upgrade().as_ref(),
                    state_rc.clone(),
                );
            });
        }
        grid.attach(&btn_dedupe, 0, 5, 4, 1);

        let tab_lbl = Label::new(Some("Media Library"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab: About ─────────────────────────────────────────────────────────
    {
        let outer = GtkBox::new(Orientation::Vertical, 16);
        outer.set_margin_top(24);
        outer.set_margin_bottom(24);
        outer.set_margin_start(24);
        outer.set_margin_end(24);

        // Header: title + version + description.
        let header = GtkBox::new(Orientation::Vertical, 4);

        let title = Label::new(Some("Sparkamp"));
        title.set_halign(Align::Start);
        title.add_css_class("about-title");
        header.append(&title);

        let version = Label::new(Some(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
        version.set_halign(Align::Start);
        version.add_css_class("about-subtle");
        header.append(&version);

        let desc = Label::new(Some(
            "A compact, fast, open-source Winamp-style music player with the \
             backend built in Rust and support for UI in GNOME desktop with \
             GTK4 & macOS with Swift.",
        ));
        desc.set_halign(Align::Start);
        desc.set_xalign(0.0);
        desc.set_wrap(true);
        desc.set_max_width_chars(60);
        desc.add_css_class("about-subtle");
        header.append(&desc);

        outer.append(&header);
        outer.append(&gtk4::Separator::new(Orientation::Horizontal));

        // Engine.
        let engine_box = GtkBox::new(Orientation::Vertical, 4);
        let engine_h = Label::new(Some("Engine"));
        engine_h.set_halign(Align::Start);
        engine_h.add_css_class("about-section");
        let engine_b = Label::new(Some("GStreamer — playbin, equalizer-10bands, volume"));
        engine_b.set_halign(Align::Start);
        engine_b.add_css_class("about-subtle");
        engine_box.append(&engine_h);
        engine_box.append(&engine_b);
        outer.append(&engine_box);

        // License.
        let license_box = GtkBox::new(Orientation::Vertical, 4);
        let license_h = Label::new(Some("License"));
        license_h.set_halign(Align::Start);
        license_h.add_css_class("about-section");
        let license_link = gtk4::LinkButton::with_label(
            "https://www.gnu.org/licenses/agpl-3.0.html",
            "GNU Affero General Public License v3 (AGPL-3.0)",
        );
        license_link.set_halign(Align::Start);
        license_box.append(&license_h);
        license_box.append(&license_link);
        outer.append(&license_box);

        // GitHub.
        let gh_box = GtkBox::new(Orientation::Vertical, 4);
        let gh_h = Label::new(Some("Get the latest"));
        gh_h.set_halign(Align::Start);
        gh_h.add_css_class("about-section");
        let gh_b = Label::new(Some(
            "Source code, releases, and issue tracking are hosted on GitHub. \
             Clone the repository or grab the latest build there.",
        ));
        gh_b.set_halign(Align::Start);
        gh_b.set_xalign(0.0);
        gh_b.set_wrap(true);
        gh_b.set_max_width_chars(60);
        gh_b.add_css_class("about-subtle");
        let gh_link = gtk4::LinkButton::with_label(
            "https://github.com/jrssae/sparkamp",
            "github.com/jrssae/sparkamp",
        );
        gh_link.set_halign(Align::Start);
        gh_box.append(&gh_h);
        gh_box.append(&gh_b);
        gh_box.append(&gh_link);
        outer.append(&gh_box);

        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .child(&outer)
            .build();

        let tab_lbl = Label::new(Some("About"));
        notebook.append_page(&scroll, Some(&tab_lbl));
        // Move About to leftmost position.
        notebook.reorder_child(&scroll, Some(0));
    }

    // About tab is index 0 — the default landing tab when no specific tab
    // was requested by the caller. Other tabs shifted right by one:
    // Appearance(1), Behavior(2), Visualizer(3), Filetypes(4), Media Library(5).
    notebook.set_current_page(Some(initial_tab.unwrap_or(0)));

    // ── Close button ───────────────────────────────────────────────────────
    // Changes are applied immediately; this button just closes the window.
    let close_btn = Button::with_label("Close");
    close_btn.set_margin_top(4);
    close_btn.set_margin_bottom(8);
    close_btn.set_margin_start(8);
    close_btn.set_margin_end(8);
    close_btn.set_halign(Align::End);
    {
        let win_wk = win.downgrade();
        close_btn.connect_clicked(move |_| {
            if let Some(w) = win_wk.upgrade() {
                w.close();
            }
        });
    }

    // Save when the window is closed via the window-manager button.
    {
        let state_rc = state.clone();
        win.connect_close_request(move |_| {
            let _ = state_rc.borrow().config.save();
            glib::Propagation::Proceed
        });
    }

    let vbox = GtkBox::new(Orientation::Vertical, 0);
    vbox.append(&notebook);
    vbox.append(&close_btn);
    win.set_child(Some(&vbox));
    win.present();
}

// ---------------------------------------------------------------------------
// Equalizer window
// ---------------------------------------------------------------------------

