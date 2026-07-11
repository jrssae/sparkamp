/// Get the display value for an ID3 editable field.
fn get_id3_field_value(
    fields: &crate::id3_editor::TagFields,
    track_meta: &Option<crate::media_library::LibTrack>,
    id: &str,
) -> String {
    match id {
        "title" => fields.title.clone(),
        "artist" => fields.artist.clone(),
        "album" => fields.album.clone(),
        "album_artist" => fields.album_artist.clone(),
        "year" => fields.year.clone(),
        "genre" => fields.genre.clone(),
        "track_num" => fields.track_number.clone(),
        "track_total" => fields.track_total.clone(),
        "disc_num" => fields.disc_number.clone(),
        "disc_total" => fields.disc_total.clone(),
        "bpm" => fields.bpm.clone(),
        "comment" => fields.comment.clone(),
        "composer" => track_meta
            .as_ref()
            .and_then(|t| t.composer.clone())
            .unwrap_or_default(),
        "original_artist" => track_meta
            .as_ref()
            .and_then(|t| t.original_artist.clone())
            .unwrap_or_default(),
        "copyright" => track_meta
            .as_ref()
            .and_then(|t| t.copyright.clone())
            .unwrap_or_default(),
        "url" => track_meta
            .as_ref()
            .and_then(|t| t.url.clone())
            .unwrap_or_default(),
        "encoded_by" => track_meta
            .as_ref()
            .and_then(|t| t.encoded_by.clone())
            .unwrap_or_default(),
        "lyric" => track_meta
            .as_ref()
            .and_then(|t| t.lyric.clone())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// ID3 field customizer — two-column layout with up/down reorder and DnD
// ---------------------------------------------------------------------------

fn open_id3_field_customizer(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    on_close: Option<Rc<dyn Fn()>>,
) {
    #[derive(Clone)]
    struct FE {
        id: String,
        label: String,
        visible: bool,
        column: usize, // 0 = left, 1 = right
    }

    let visible_ids = state.borrow().config.media_library.id3_visible_columns.clone();
    let col_pos = state.borrow().config.media_library.id3_column_position.clone();
    let editable: Vec<&MlColumnDef> = ALL_COLUMNS.iter().filter(|c| c.id3_editable).collect();

    // Visible fields first (in their saved order), then invisible fields appended
    let mut entries: Vec<FE> = visible_ids
        .iter()
        .filter_map(|id| editable.iter().find(|c| c.id == id.as_str()))
        .map(|c| FE {
            id: c.id.to_string(),
            label: c.header.to_string(),
            visible: true,
            column: if col_pos.get(c.id).map_or(false, |p| p == "right") { 1 } else { 0 },
        })
        .collect();
    for c in &editable {
        if !visible_ids.contains(&c.id.to_string()) {
            entries.push(FE {
                id: c.id.to_string(),
                label: c.header.to_string(),
                visible: false,
                column: if col_pos.get(c.id).map_or(false, |p| p == "right") { 1 } else { 0 },
            });
        }
    }

    let fs: Rc<RefCell<Vec<FE>>> = Rc::new(RefCell::new(entries));

    // Persist current fs → config
    let save_cfg = {
        let fs = fs.clone();
        let st = state.clone();
        Rc::new(move || {
            let entries = fs.borrow();
            let vis: Vec<String> = entries.iter().filter(|e| e.visible).map(|e| e.id.clone()).collect();
            let pos: std::collections::HashMap<String, String> = entries
                .iter()
                .map(|e| (e.id.clone(), if e.column == 1 { "right" } else { "left" }.to_string()))
                .collect();
            let mut s = st.borrow_mut();
            s.config.media_library.id3_visible_columns = vis;
            s.config.media_library.id3_column_position = pos;
            let _ = s.config.save();
        })
    };

    // Window
    let dlg = gtk4::Window::new();
    dlg.set_title(Some("Customize ID3 Fields"));
    dlg.set_default_size(520, 440);
    dlg.set_resizable(true);
    if let Some(p) = parent {
        dlg.set_transient_for(Some(p));
    }

    let root_vbox = GtkBox::new(Orientation::Vertical, 0);

    // ── Header ──────────────────────────────────────────────────────────────
    {
        let hdr = GtkBox::new(Orientation::Horizontal, 8);
        hdr.set_margin_top(8);
        hdr.set_margin_bottom(8);
        hdr.set_margin_start(12);
        hdr.set_margin_end(12);
        let hint = Label::builder()
            .label("Use ▲ ▼ to reorder within a column, or drag rows. Use → ← to switch columns.")
            .halign(Align::Start)
            .hexpand(true)
            .build();
        hint.add_css_class("status-label");
        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        let done = Button::with_label("Done");
        done.add_css_class("suggested-action");
        hdr.append(&hint);
        hdr.append(&spring);
        hdr.append(&done);
        root_vbox.append(&hdr);
        root_vbox.append(&Separator::new(Orientation::Horizontal));

        let dlg_wk = dlg.downgrade();
        let oc = on_close.clone();
        done.connect_clicked(move |_| {
            if let Some(d) = dlg_wk.upgrade() { d.close(); }
            if let Some(ref cb) = oc { cb(); }
        });
    }

    // ── Column header row ────────────────────────────────────────────────────
    {
        let chr = GtkBox::new(Orientation::Horizontal, 0);
        let lh = Label::builder()
            .label("Left Column")
            .halign(Align::Center)
            .hexpand(true)
            .build();
        lh.add_css_class("ml-section-header");
        let rh = Label::builder()
            .label("Right Column")
            .halign(Align::Center)
            .hexpand(true)
            .build();
        rh.add_css_class("ml-section-header");
        chr.append(&lh);
        chr.append(&Separator::new(Orientation::Vertical));
        chr.append(&rh);
        root_vbox.append(&chr);
        root_vbox.append(&Separator::new(Orientation::Horizontal));
    }

    // ── Two-column list area ─────────────────────────────────────────────────
    let panels = GtkBox::new(Orientation::Horizontal, 0);
    panels.set_vexpand(true);
    panels.set_hexpand(true);

    let left_lb: Rc<ListBox> = Rc::new({
        let lb = ListBox::new();
        lb.add_css_class("playlist");
        lb.set_selection_mode(gtk4::SelectionMode::None);
        lb
    });
    let right_lb: Rc<ListBox> = Rc::new({
        let lb = ListBox::new();
        lb.add_css_class("playlist");
        lb.set_selection_mode(gtk4::SelectionMode::None);
        lb
    });

    let left_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&*left_lb)
        .build();
    let right_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&*right_lb)
        .build();

    panels.append(&left_scroll);
    panels.append(&Separator::new(Orientation::Vertical));
    panels.append(&right_scroll);
    root_vbox.append(&panels);

    dlg.set_child(Some(&root_vbox));

    // ── Rebuild holder (allows rebuild closure to call itself) ───────────────
    let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let rebuild = {
        let fs = fs.clone();
        let left_ref = left_lb.clone();
        let right_ref = right_lb.clone();
        let sc = save_cfg.clone();
        let rh = rebuild_holder.clone();
        Rc::new(move || {
            // Clear both panels
            while let Some(c) = left_ref.first_child() { left_ref.remove(&c); }
            while let Some(c) = right_ref.first_child() { right_ref.remove(&c); }

            let entries = fs.borrow().clone();

            // Indices per column, in Vec order
            let col0: Vec<usize> = entries.iter().enumerate()
                .filter(|(_, e)| e.column == 0).map(|(i, _)| i).collect();
            let col1: Vec<usize> = entries.iter().enumerate()
                .filter(|(_, e)| e.column == 1).map(|(i, _)| i).collect();

            for (col_idx, col_globals) in [col0.as_slice(), col1.as_slice()].iter().enumerate() {
                let lb: &ListBox = if col_idx == 0 { &left_ref } else { &right_ref };
                let n = col_globals.len();

                for (col_pos, &g_idx) in col_globals.iter().enumerate() {
                    let entry = &entries[g_idx];
                    let rb_box = GtkBox::new(Orientation::Horizontal, 4);
                    rb_box.set_margin_top(2);
                    rb_box.set_margin_bottom(2);
                    rb_box.set_margin_start(4);
                    rb_box.set_margin_end(4);

                    // ▲ button
                    let up_btn = Button::with_label("▲");
                    up_btn.add_css_class("pl-btn");
                    up_btn.set_sensitive(col_pos > 0);
                    if col_pos > 0 {
                        let fs2 = fs.clone(); let sc2 = sc.clone(); let rh2 = rh.clone();
                        let g = g_idx; let prev = col_globals[col_pos - 1];
                        up_btn.connect_clicked(move |_| {
                            fs2.borrow_mut().swap(g, prev);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }

                    // ▼ button
                    let dn_btn = Button::with_label("▼");
                    dn_btn.add_css_class("pl-btn");
                    dn_btn.set_sensitive(col_pos + 1 < n);
                    if col_pos + 1 < n {
                        let fs2 = fs.clone(); let sc2 = sc.clone(); let rh2 = rh.clone();
                        let g = g_idx; let next = col_globals[col_pos + 1];
                        dn_btn.connect_clicked(move |_| {
                            fs2.borrow_mut().swap(g, next);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }

                    // Visibility checkbox
                    let cb = CheckButton::new();
                    cb.set_active(entry.visible);
                    {
                        let fs2 = fs.clone(); let sc2 = sc.clone(); let rh2 = rh.clone();
                        let g = g_idx;
                        cb.connect_toggled(move |btn| {
                            fs2.borrow_mut()[g].visible = btn.is_active();
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }

                    // Field label (greyed when invisible)
                    let lbl = Label::builder()
                        .label(entry.label.as_str())
                        .halign(Align::Start)
                        .hexpand(true)
                        .build();
                    if !entry.visible {
                        lbl.add_css_class("status-label");
                    }

                    // → / ← column-switch button
                    let sw_lbl = if col_idx == 0 { "→" } else { "←" };
                    let sw_btn = Button::with_label(sw_lbl);
                    sw_btn.add_css_class("pl-btn");
                    {
                        let fs2 = fs.clone(); let sc2 = sc.clone(); let rh2 = rh.clone();
                        let g = g_idx;
                        let new_col: usize = if col_idx == 0 { 1 } else { 0 };
                        sw_btn.connect_clicked(move |_| {
                            // Move to end of the destination column
                            let insert_at = {
                                let e = fs2.borrow();
                                e.iter().enumerate().rev()
                                    .find(|(j, ent)| *j != g && ent.column == new_col)
                                    .map(|(j, _)| j + 1)
                                    .unwrap_or(e.len())
                            };
                            {
                                let mut e = fs2.borrow_mut();
                                e[g].column = new_col;
                                let entry = e.remove(g);
                                let adj = if insert_at > g { insert_at - 1 } else { insert_at };
                                let cap = e.len();
                                e.insert(adj.min(cap), entry);
                            }
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }

                    // DragSource — provides global index as string
                    {
                        let drag_src = DragSource::new();
                        drag_src.set_actions(gtk4::gdk::DragAction::MOVE);
                        let g_str = g_idx.to_string();
                        drag_src.connect_prepare(move |_, _, _| {
                            Some(gdk::ContentProvider::for_value(&(&g_str).to_value()))
                        });
                        rb_box.add_controller(drag_src);
                    }

                    rb_box.append(&up_btn);
                    rb_box.append(&dn_btn);
                    rb_box.append(&cb);
                    rb_box.append(&lbl);
                    rb_box.append(&sw_btn);

                    let row = ListBoxRow::new();
                    row.set_widget_name(&g_idx.to_string());
                    row.set_child(Some(&rb_box));
                    lb.append(&row);
                }
            }
        })
    };

    *rebuild_holder.borrow_mut() = Some(rebuild.clone());

    // ── DropTargets — one per column panel; supports cross-column DnD ────────
    for (col_target, lb_rc) in [(0usize, left_lb.clone()), (1usize, right_lb.clone())] {
        let dt = DropTarget::new(glib::Type::STRING, gtk4::gdk::DragAction::MOVE);
        let lb_dt = lb_rc.clone();
        let fs_dt = fs.clone();
        let sc_dt = save_cfg.clone();
        let rh_dt = rebuild_holder.clone();
        dt.connect_drop(move |_, value, _x, y| {
            let src_global: usize = match value.get::<String>() {
                Ok(s) => match s.parse() { Ok(n) => n, Err(_) => return false },
                Err(_) => return false,
            };
            {
                let e = fs_dt.borrow();
                if src_global >= e.len() { return false; }
            }

            // Find the target row by y-coordinate in this ListBox
            let target_global: Option<usize> = lb_dt.row_at_y(y as i32)
                .and_then(|r| r.widget_name().to_string().parse::<usize>().ok());

            {
                let mut e = fs_dt.borrow_mut();
                e[src_global].column = col_target;
                let entry = e.remove(src_global);
                if let Some(tg) = target_global {
                    let adj = if tg > src_global { tg - 1 } else { tg };
                    let cap = e.len();
                    e.insert(adj.min(cap), entry);
                } else {
                    // Dropped below all rows — append to end of target column
                    let insert_at = e.iter().enumerate().rev()
                        .find(|(_, ent)| ent.column == col_target)
                        .map(|(j, _)| j + 1)
                        .unwrap_or_else(|| e.len());
                    let cap = e.len();
                    e.insert(insert_at.min(cap), entry);
                }
            }

            sc_dt();
            if let Some(ref r) = *rh_dt.borrow() { r(); }
            true
        });
        lb_rc.add_controller(dt);
    }

    rebuild();
    dlg.present();
}

// ---------------------------------------------------------------------------

#[derive(Clone)]
enum ColumnCustomizerMode {
    MediaLibrary,
    Id3Editor,
}

fn open_customize_columns_dialog(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    title: &str,
    mode: ColumnCustomizerMode,
    on_toggle: Option<Rc<dyn Fn(String, bool)>>,
    on_close: Option<Rc<dyn Fn()>>,
) {
    use gtk4::prelude::*;

    // ID3 editor gets its own two-column customizer
    if matches!(mode, ColumnCustomizerMode::Id3Editor) {
        open_id3_field_customizer(parent, state, on_close);
        return;
    }

    let dlg = gtk4::Window::new();
    dlg.set_title(Some(title));
    dlg.set_default_size(400, 480);
    dlg.set_resizable(true);
    if let Some(p) = parent {
        dlg.set_transient_for(Some(p));
    }

    let main_vbox = GtkBox::new(Orientation::Vertical, 8);
    main_vbox.set_margin_top(12);
    main_vbox.set_margin_bottom(12);
    main_vbox.set_margin_start(12);
    main_vbox.set_margin_end(12);

    // ── Build ordered entry list ─────────────────────────────────────────────
    #[derive(Clone)]
    struct ColEntry {
        id: String,
        header: String,
        visible: bool,
    }

    let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
    let visible_vec: Vec<String> = state.borrow().config.media_library.visible_columns.clone();
    let visible_set: std::collections::HashSet<String> = visible_vec.iter().cloned().collect();

    let mut init_entries: Vec<ColEntry> = Vec::new();
    // 1. Visible columns in saved order
    for id in &saved_order {
        if visible_set.contains(id) {
            if let Some(col) = ALL_COLUMNS.iter().find(|c| c.id == id.as_str()) {
                init_entries.push(ColEntry { id: id.clone(), header: col.header.to_string(), visible: true });
            }
        }
    }
    // 2. Visible columns not in saved order (newly enabled)
    for id in &visible_vec {
        if !saved_order.contains(id) {
            if let Some(col) = ALL_COLUMNS.iter().find(|c| c.id == id.as_str()) {
                init_entries.push(ColEntry { id: id.clone(), header: col.header.to_string(), visible: true });
            }
        }
    }
    // 3. Hidden columns (no order controls needed)
    for col in ALL_COLUMNS.iter() {
        if !visible_set.contains(col.id) {
            init_entries.push(ColEntry { id: col.id.to_string(), header: col.header.to_string(), visible: false });
        }
    }

    let entries: Rc<RefCell<Vec<ColEntry>>> = Rc::new(RefCell::new(init_entries));

    // Persist entries → config on every change
    let save_cfg: Rc<dyn Fn()> = {
        let entries = entries.clone();
        let st = state.clone();
        Rc::new(move || {
            let es = entries.borrow();
            let order: Vec<String> = es.iter().filter(|e| e.visible).map(|e| e.id.clone()).collect();
            let mut s = st.borrow_mut();
            s.config.media_library.visible_columns = order.clone();
            s.config.media_library.ml_file_col_order = order;
            let _ = s.config.save();
        })
    };

    let hdr = Label::builder()
        .label("Use ▲ ▼ to reorder visible columns:")
        .halign(Align::Start)
        .build();
    main_vbox.append(&hdr);

    let scrolled = ScrolledWindow::new();
    scrolled.set_hexpand(true);
    scrolled.set_vexpand(true);
    scrolled.set_has_frame(true);

    let list_lb = ListBox::new();
    list_lb.add_css_class("playlist");
    list_lb.set_selection_mode(gtk4::SelectionMode::None);
    scrolled.set_child(Some(&list_lb));
    main_vbox.append(&scrolled);

    // rebuild_holder allows the rebuild closure to call itself recursively
    let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let rebuild: Rc<dyn Fn()> = {
        let entries = entries.clone();
        let lb_ref = list_lb.clone();
        let sc = save_cfg.clone();
        let rh = rebuild_holder.clone();
        let on_toggle_rb = on_toggle.clone();
        let scrolled_rb = scrolled.clone();
        Rc::new(move || {
            // Preserve scroll position: clearing and re-adding rows resets
            // the ScrolledWindow's vadjustment to 0, which yanks the user
            // back to the top on every toggle. Snapshot → rebuild → restore.
            let prev_scroll = scrolled_rb.vadjustment().value();
            while let Some(c) = lb_ref.first_child() { lb_ref.remove(&c); }

            let es = entries.borrow().clone();

            for (i, entry) in es.iter().enumerate() {
                let row_box = GtkBox::new(Orientation::Horizontal, 4);
                row_box.set_margin_top(2);
                row_box.set_margin_bottom(2);
                row_box.set_margin_start(4);
                row_box.set_margin_end(4);

                if entry.visible {
                    // ▲ button — enabled when a visible column precedes this one
                    let up_btn = Button::with_label("▲");
                    up_btn.add_css_class("pl-btn");
                    let prev_idx = es[..i].iter().rposition(|e| e.visible);
                    up_btn.set_sensitive(prev_idx.is_some());
                    if let Some(prev) = prev_idx {
                        let entries2 = entries.clone();
                        let sc2 = sc.clone();
                        let rh2 = rh.clone();
                        up_btn.connect_clicked(move |_| {
                            entries2.borrow_mut().swap(i, prev);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }
                    row_box.append(&up_btn);

                    // ▼ button — enabled when a visible column follows this one
                    let dn_btn = Button::with_label("▼");
                    dn_btn.add_css_class("pl-btn");
                    let next_rel = es[i + 1..].iter().position(|e| e.visible);
                    dn_btn.set_sensitive(next_rel.is_some());
                    if let Some(rel) = next_rel {
                        let next = i + 1 + rel;
                        let entries2 = entries.clone();
                        let sc2 = sc.clone();
                        let rh2 = rh.clone();
                        dn_btn.connect_clicked(move |_| {
                            entries2.borrow_mut().swap(i, next);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }
                    row_box.append(&dn_btn);
                } else {
                    // Spacer to align labels with visible rows
                    let spacer = GtkBox::new(Orientation::Horizontal, 4);
                    spacer.set_width_request(64);
                    row_box.append(&spacer);
                }

                // Visibility checkbox
                let cb = CheckButton::new();
                cb.set_active(entry.visible);
                {
                    let entries2 = entries.clone();
                    let sc2 = sc.clone();
                    let rh2 = rh.clone();
                    let on_tgl = on_toggle_rb.clone();
                    cb.connect_toggled(move |btn| {
                        let visible = btn.is_active();
                        let id = entries2.borrow()[i].id.clone();
                        entries2.borrow_mut()[i].visible = visible;
                        sc2();
                        if let Some(ref cb) = on_tgl { cb(id, visible); }
                        if let Some(ref r) = *rh2.borrow() { r(); }
                    });
                }
                row_box.append(&cb);

                let lbl = Label::builder()
                    .label(entry.header.as_str())
                    .halign(Align::Start)
                    .xalign(0.0)
                    .hexpand(true)
                    .build();
                if !entry.visible { lbl.add_css_class("status-label"); }
                row_box.append(&lbl);

                let row = ListBoxRow::new();
                row.set_child(Some(&row_box));
                lb_ref.append(&row);
            }
            // Restore scroll position after the new rows lay out. idle_add
            // defers until GTK finishes sizing the listbox so the adjustment
            // upper bound reflects the post-rebuild content height.
            let adj = scrolled_rb.vadjustment();
            glib::idle_add_local_once(move || {
                let upper = adj.upper();
                let page  = adj.page_size();
                let max   = (upper - page).max(0.0);
                adj.set_value(prev_scroll.min(max));
            });
        })
    };

    *rebuild_holder.borrow_mut() = Some(rebuild.clone());
    rebuild();

    // ── Buttons ──────────────────────────────────────────────────────────────
    let btn_row = GtkBox::new(Orientation::Horizontal, 8);

    let btn_reset = Button::with_label("Reset Defaults");
    {
        let entries2 = entries.clone();
        let sc2 = save_cfg.clone();
        let rb2 = rebuild.clone();
        let on_tgl = on_toggle.clone();
        let st2 = state.clone();
        btn_reset.connect_clicked(move |_| {
            let defaults = crate::config::MediaLibraryConfig::default_visible_columns();
            let default_set: std::collections::HashSet<String> = defaults.iter().cloned().collect();
            {
                let mut es = entries2.borrow_mut();
                for e in es.iter_mut() { e.visible = default_set.contains(&e.id); }
                es.sort_by_key(|e| {
                    if e.visible {
                        defaults.iter().position(|d| d == &e.id).unwrap_or(usize::MAX)
                    } else {
                        usize::MAX
                    }
                });
            }
            if let Some(ref cb) = on_tgl {
                for e in entries2.borrow().iter() { cb(e.id.clone(), e.visible); }
            }
            {
                let mut s = st2.borrow_mut();
                s.config.media_library.visible_columns = defaults.clone();
                s.config.media_library.ml_file_col_order = defaults;
                let _ = s.config.save();
            }
            sc2();
            rb2();
        });
    }
    btn_row.append(&btn_reset);

    let spring = GtkBox::new(Orientation::Horizontal, 0);
    spring.set_hexpand(true);
    btn_row.append(&spring);

    let btn_close = Button::with_label("Close");
    {
        let dlg_wk = dlg.downgrade();
        let oc = on_close.clone();
        btn_close.connect_clicked(move |_| {
            if let Some(ref cb) = oc { cb(); }
            if let Some(w) = dlg_wk.upgrade() { w.close(); }
        });
    }
    btn_row.append(&btn_close);

    main_vbox.append(&btn_row);
    dlg.set_child(Some(&main_vbox));

    dlg.connect_close_request(move |_| {
        if let Some(ref cb) = on_close { cb(); }
        glib::Propagation::Proceed
    });

    dlg.present();
}

/// Open the ID3 tag editor window for `path`.
///
/// Pre-populates all 12 default fields from the file's existing tag and lets
/// the user edit them in a two-column grid.  Ctrl+S or the Save button writes
/// the tag back to disk and reloads the in-memory track so the playlist row
/// immediately shows the updated title/artist.  Esc or Cancel discards changes.
///
/// A "Customize…" button opens a secondary window ([`open_id3_extra_window`])
/// for any additional ID3v2 frames present in the file.
///
/// This is a singleton: if an editor is already open, it will be updated
/// with the new file instead of opening a second window.
fn open_id3_editor_window(
    _parent: Option<&impl gtk4::prelude::IsA<gtk4::Window>>,
    path: std::path::PathBuf,
    state: Rc<RefCell<AppState>>,
    rebuild_cb: Rc<dyn Fn()>,
    initial_values: Option<std::collections::HashMap<String, String>>,
) {
    use crate::id3_editor::{read_tag_fields, write_tag_fields, TagFields};
    use gtk4::prelude::*;

    // If an editor is already open, close it and build a fresh one for the new
    // file — the same filename can live at a different path, so the window must
    // reflect the exact file just requested rather than being reused as-is.
    // Take in its own statement so the borrow is released before `close()`,
    // which synchronously fires the close-request handler (it borrows too).
    let existing_editor = state.borrow_mut().id3_editor_window.take();
    if let Some(existing_win) = existing_editor {
        existing_win.close();
    }

    let fields = read_tag_fields(&path);
    let fname = gtk_safe(path.file_name().and_then(|n| n.to_str()).unwrap_or("?"));
    let path_str = path.to_string_lossy().into_owned();

    let track_meta = state
        .borrow()
        .media_lib
        .as_ref()
        .and_then(|ml| ml.track_by_path(&path_str).ok());

    let ro = crate::media_library::read_only_track_fields(&path, track_meta.as_ref());

    let win = gtk4::Window::builder()
        .title(format!("ID3 Tag Editor — {fname}"))
        .default_width(600)
        .default_height(480)
        .build();

    let state_for_close = state.clone();
    win.connect_close_request(move |w| {
        // Only clear the handle if it still points at *this* window — a newer
        // editor may have replaced it (close fires as the old one is swapped).
        let mut s = state_for_close.borrow_mut();
        if s.id3_editor_window.as_ref() == Some(w) {
            s.id3_editor_window = None;
        }
        glib::Propagation::Proceed
    });
    state.borrow_mut().id3_editor_window = Some(win.clone());

    // ── Get visible columns from config (preserve order for left/right split) ──
    let visible_ids: Vec<String> = state
        .borrow()
        .config
        .media_library
        .id3_visible_columns
        .clone();

    // ── Collect entry widgets for the save handler ───────────────────────────
    // Stores (field_id, Entry) for editable fields.
    let entries: Rc<RefCell<std::collections::HashMap<String, Entry>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // ── 2-column field grid ───────────────────────────────────────────────
    let grid = Grid::new();
    grid.set_margin_top(12);
    grid.set_margin_bottom(8);
    grid.set_margin_start(12);
    grid.set_margin_end(12);
    grid.set_row_spacing(6);
    grid.set_column_spacing(8);
    grid.set_hexpand(true);

    // Get column positions from config
    let column_positions: std::collections::HashMap<String, String> = state
        .borrow()
        .config
        .media_library
        .id3_column_position
        .clone();

    // Get editable columns in visible order
    let editable_ids: std::collections::HashSet<&str> = ALL_COLUMNS
        .iter()
        .filter(|c| c.id3_editable)
        .map(|c| c.id)
        .collect();

    let visible_editable: Vec<&str> = visible_ids
        .iter()
        .filter(|id| editable_ids.contains(id.as_str()))
        .map(|s| s.as_str())
        .collect();

    // Separate into left/right based on column position config
    let mut left_ids: Vec<&str> = Vec::new();
    let mut right_ids: Vec<&str> = Vec::new();
    for id in &visible_editable {
        let pos = column_positions
            .get(*id)
            .map(|s| s.as_str())
            .unwrap_or("left");
        if pos == "right" {
            right_ids.push(*id);
        } else {
            left_ids.push(*id);
        }
    }

    // Build left column (cols 0-1)
    let mut left_entries: Vec<(String, gtk4::Entry)> = Vec::new();
    for (row, id) in left_ids.iter().enumerate() {
        let col_def = ALL_COLUMNS.iter().find(|c| c.id == *id).unwrap();
        let lbl = Label::new(Some(col_def.header));
        lbl.set_xalign(1.0);
        lbl.set_margin_end(4);
        grid.attach(&lbl, 0, row as i32, 1, 1);

        let value = if let Some(ref vals) = initial_values {
            vals.get(*id)
                .cloned()
                .unwrap_or_else(|| get_id3_field_value(&fields, &track_meta, id))
        } else {
            get_id3_field_value(&fields, &track_meta, id)
        };
        if *id == "genre" {
            let (combo, entry) = make_genre_combo(&value);
            combo.set_hexpand(true);
            grid.attach(&combo, 1, row as i32, 1, 1);
            // Register the hidden carrier entry so Save picks the genre up;
            // without it the save handler writes an empty genre.
            left_entries.push((id.to_string(), entry));
        } else {
            let entry = Entry::new();
            entry.set_hexpand(true);
            entry.set_text(&gtk_safe(&value));
            grid.attach(&entry, 1, row as i32, 1, 1);
            left_entries.push((id.to_string(), entry));
        }
    }

    // Build right column (cols 2-3)
    let mut right_entries: Vec<(String, gtk4::Entry)> = Vec::new();
    for (row, id) in right_ids.iter().enumerate() {
        let col_def = ALL_COLUMNS.iter().find(|c| c.id == *id).unwrap();
        let lbl = Label::new(Some(col_def.header));
        lbl.set_xalign(1.0);
        lbl.set_margin_end(4);
        grid.attach(&lbl, 2, row as i32, 1, 1);

        let value = if let Some(ref vals) = initial_values {
            vals.get(*id)
                .cloned()
                .unwrap_or_else(|| get_id3_field_value(&fields, &track_meta, id))
        } else {
            get_id3_field_value(&fields, &track_meta, id)
        };
        if *id == "genre" {
            let (combo, entry) = make_genre_combo(&value);
            combo.set_hexpand(true);
            grid.attach(&combo, 3, row as i32, 1, 1);
            right_entries.push((id.to_string(), entry));
        } else {
            let entry = Entry::new();
            entry.set_hexpand(true);
            entry.set_text(&gtk_safe(&value));
            grid.attach(&entry, 3, row as i32, 1, 1);
            right_entries.push((id.to_string(), entry));
        }
    }

    // Insert all entries into the HashMap in one operation
    for (id, entry) in left_entries.into_iter().chain(right_entries) {
        entries.borrow_mut().insert(id, entry);
    }

    // ── Check if file is read-only ───────────────────────────────────────────
    let is_read_only = crate::media_library::is_read_only(&path);

    // ── Artwork section ─────────────────────────────────────────────────────
    let artwork_vbox = GtkBox::new(Orientation::Vertical, 4);
    artwork_vbox.set_margin_start(12);
    artwork_vbox.set_margin_end(12);
    artwork_vbox.set_margin_top(8);
    artwork_vbox.set_margin_bottom(8);

    let art_path_entry = Entry::new();
    art_path_entry.set_text(&gtk_safe(&ro.artwork_path));
    art_path_entry.set_hexpand(true);

    let btn_browse = Button::with_label("Browse…");
    let btn_view = Button::with_label("View");
    btn_view.set_sensitive(!ro.artwork_path.is_empty());

    let art_entry_clone = art_path_entry.clone();
    let btn_view_for_browse = btn_view.clone();
    btn_browse.connect_clicked(move |b| {
        let dialog = gtk4::FileDialog::new();
        dialog.set_title("Select Artwork");
        let filters = gtk4::FileFilter::new();
        filters.set_name(Some("Images"));
        filters.add_mime_type("image/png");
        filters.add_mime_type("image/jpeg");
        filters.add_mime_type("image/jpg");
        filters.add_mime_type("image/gif");
        filters.add_mime_type("image/webp");
        dialog.set_default_filter(Some(&filters));
        let entry_clone = art_entry_clone.clone();
        let btn_view_clone = btn_view_for_browse.clone();
        // Parent to the editor window (the button's toplevel) so the chooser
        // has a transient parent instead of a throwaway, unmapped window.
        let parent = b.root().and_downcast::<gtk4::Window>();
        dialog.open(
            parent.as_ref(),
            None::<&gtk4::gio::Cancellable>,
            move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().into_owned();
                        entry_clone.set_text(&path_str);
                        btn_view_clone.set_sensitive(true);
                    }
                }
            },
        );
    });

    let art_path_clone = art_path_entry.clone();
    btn_view.connect_clicked(move |_| {
        let p = art_path_clone.text();
        if !p.is_empty() {
            open_image_viewer(&p);
        }
    });

    let art_path_row = GtkBox::new(Orientation::Horizontal, 8);
    art_path_row.append(&Label::new(Some("Artwork:")));
    art_path_row.append(&art_path_entry);
    art_path_row.append(&btn_browse);
    art_path_row.append(&btn_view);
    artwork_vbox.append(&art_path_row);

    // Track art_path_entry in the entries HashMap
    entries
        .borrow_mut()
        .insert("artwork_path".to_string(), art_path_entry);

    // Show 128x128 thumbnail preview
    if visible_ids.contains(&"artwork_path".to_string()) && !ro.artwork_path.is_empty() {
        let art_picture = gtk4::Picture::new();
        art_picture.set_width_request(128);
        art_picture.set_height_request(128);
        art_picture.set_can_shrink(true);
        art_picture.set_content_fit(gtk4::ContentFit::Contain);
        art_picture.set_filename(Some(&ro.artwork_path));

        let art_clone = ro.artwork_path.clone();
        let click = gtk4::GestureClick::new();
        click.connect_pressed(move |_, _, _, _| {
            open_image_viewer(&art_clone);
        });
        art_picture.add_controller(click);
        artwork_vbox.append(&art_picture);
    }

    // ── Status label ─────────────────────────────────────────────────────────
    let status_lbl = Label::builder()
        .label("")
        .halign(Align::Start)
        .css_classes(["status-label"])
        .build();
    status_lbl.set_margin_start(12);
    status_lbl.set_margin_bottom(4);

    // ── Read-only notice (only shown for read-only files) ────────────────────
    let read_only_notice = Label::builder()
        .label("🔒 This file is read only")
        .halign(Align::Center)
        .build();
    read_only_notice.set_margin_start(12);
    read_only_notice.set_margin_end(12);
    read_only_notice.set_margin_top(8);
    read_only_notice.set_margin_bottom(4);
    read_only_notice.set_visible(is_read_only);

    // Disable all entry widgets for read-only files
    if is_read_only {
        for (_, entry) in entries.borrow().iter() {
            entry.set_sensitive(false);
        }
    }

    // ── Button row ───────────────────────────────────────────────────────────
    let btn_row = GtkBox::new(Orientation::Horizontal, 8);
    btn_row.set_margin_top(4);
    btn_row.set_margin_start(12);
    btn_row.set_margin_end(12);
    btn_row.set_margin_bottom(8);

    let btn_customize = Button::with_label("Customize…");
    let btn_cancel = Button::with_label("Cancel");
    let btn_save = Button::with_label("Save");
    btn_save.add_css_class("suggested-action");
    btn_save.set_visible(!is_read_only);

    let spring = GtkBox::new(Orientation::Horizontal, 0);
    spring.set_hexpand(true);
    btn_row.append(&btn_customize);
    btn_row.append(&spring);
    btn_row.append(&btn_cancel);
    btn_row.append(&btn_save);

    // ── Main layout ──────────────────────────────────────────────────────────
    // Full path + filename header — a read-only Entry so a long path that
    // doesn't fit scrolls horizontally (cursor/drag) without a scrollbar, and
    // stays selectable/copyable, confirming the file's exact source location.
    let path_entry = Entry::new();
    path_entry.set_text(&gtk_safe(&path_str));
    path_entry.set_editable(false);
    path_entry.set_can_focus(true);
    path_entry.set_hexpand(true);
    path_entry.set_margin_top(10);
    path_entry.set_margin_bottom(10);
    path_entry.set_margin_start(12);
    path_entry.set_margin_end(12);
    path_entry.set_tooltip_text(Some(&path_str));
    // Show the end (filename) first rather than the start of the path.
    path_entry.set_position(-1);

    let vbox = GtkBox::new(Orientation::Vertical, 0);
    vbox.append(&path_entry);
    vbox.append(&Separator::new(Orientation::Horizontal));
    vbox.append(&grid);
    vbox.append(&artwork_vbox);
    vbox.append(&Separator::new(Orientation::Horizontal));
    vbox.append(&status_lbl);
    vbox.append(&read_only_notice);
    vbox.append(&btn_row);
    win.set_child(Some(&vbox));

    // ── Collect fields → TagFields and write to disk ─────────────────────────
    let do_save = {
        let path = path.clone();
        let state_s = state.clone();
        let rebuild_s = rebuild_cb.clone();
        let status_s = status_lbl.clone();
        let win_wk = win.downgrade();
        let entries_r = entries.clone();

        move || {
            let entries = entries_r.borrow();
            let new_fields = TagFields {
                title: entries
                    .get("title")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                artist: entries
                    .get("artist")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                album: entries
                    .get("album")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                album_artist: entries
                    .get("album_artist")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                genre: entries
                    .get("genre")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                year: entries
                    .get("year")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                track_number: entries
                    .get("track_num")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                track_total: entries
                    .get("track_total")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                disc_number: entries
                    .get("disc_num")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                disc_total: entries
                    .get("disc_total")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                bpm: entries
                    .get("bpm")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                comment: entries
                    .get("comment")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                artwork_path: entries
                    .get("artwork_path")
                    .map(|e| e.text().to_string())
                    .unwrap_or_default(),
            };

            match write_tag_fields(&path, &new_fields) {
                Ok(()) => {
                    for track in &mut state_s.borrow_mut().playlist.tracks {
                        if track.path == path {
                            if let Ok(fresh) = crate::model::Track::from_path(&path) {
                                track.title = fresh.title;
                                track.artist = fresh.artist;
                                track.album_artist = fresh.album_artist;
                                track.album = fresh.album;
                            }
                            break;
                        }
                    }

                    // If the saved track is currently playing, update the marquee
                    // immediately so the new artist/title is reflected without
                    // requiring a track change.
                    let is_current = state_s
                        .borrow()
                        .playlist
                        .current()
                        .map(|t| t.path == path)
                        .unwrap_or(false);
                    if is_current {
                        let display = state_s
                            .borrow()
                            .playlist
                            .current()
                            .map(|t| t.display_name())
                            .unwrap_or_default();
                        if let Some(ref cb) = state_s.borrow().set_track_callback.clone() {
                            cb(&display);
                        }
                    }

                    // Update the Media Library DB record and artwork cache for
                    // the edited file, then refresh the ML window if it is open.
                    if let Some(lib) = state_s.borrow().media_lib.as_ref() {
                        let path_str = path.to_string_lossy().into_owned();
                        let _ = lib.rescan_track(&path_str);
                        if let Ok(lib_track) = lib.track_by_path(&path_str) {
                            let _ = lib.refresh_artwork(lib_track.id, &path_str);
                        }
                    }

                    let rebuild = rebuild_s.clone();
                    let rebuild_ml = state_s.borrow().rebuild_ml_callback.clone();
                    if let Some(w) = win_wk.upgrade() {
                        w.close();
                    }
                    glib::idle_add_local(move || {
                        rebuild();
                        if let Some(ref cb) = rebuild_ml {
                            cb();
                        }
                        glib::ControlFlow::Break
                    });
                }
                Err(e) => {
                    status_s.set_text(&format!("Save error: {e}"));
                }
            }
        }
    };

    // ── Cancel button ────────────────────────────────────────────────────────
    btn_cancel.connect_clicked({
        let win_wk = win.downgrade();
        move |_| {
            if let Some(w) = win_wk.upgrade() {
                w.close();
            }
        }
    });

    // ── Save button ──────────────────────────────────────────────────────────
    btn_save.connect_clicked({
        let save = do_save.clone();
        move |_| {
            save();
        }
    });

    // ── Customize button — open column customization dialog ──────────────────
    btn_customize.connect_clicked({
        let state_outer = state.clone();
        let win_wk_outer = win.downgrade();
        let path_outer = path.clone();
        let rebuild_outer = rebuild_cb.clone();
        let entries_outer = entries.clone();
        move |_| {
            let state_inner = state_outer.clone();
            let win_wk = win_wk_outer.clone();
            let path_clone = path_outer.clone();
            let rebuild_clone = rebuild_outer.clone();
            let entries_clone = entries_outer.clone();
            let current_values: std::collections::HashMap<String, String> = entries_clone
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.text().to_string()))
                .collect();
            open_customize_columns_dialog(
                win_wk.upgrade().as_ref(),
                state_inner.clone(),
                "Customize ID3 Fields",
                ColumnCustomizerMode::Id3Editor,
                None::<Rc<dyn Fn(String, bool)>>,
                Some(Rc::new(move || {
                    if let Some(w) = win_wk.upgrade() {
                        w.close();
                    }
                    open_id3_editor_window(
                        None::<&gtk4::Window>,
                        path_clone.clone(),
                        state_inner.clone(),
                        rebuild_clone.clone(),
                        Some(current_values.clone()),
                    );
                }) as Rc<dyn Fn()>),
            );
        }
    });

    // ── Keyboard: Ctrl+S saves, Esc cancels ──────────────────────────────────
    {
        let key_ctrl = gtk4::EventControllerKey::new();
        let save_fn = do_save.clone();
        let win_wk2 = win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, modifiers| match key {
            gdk::Key::Escape => {
                if let Some(w) = win_wk2.upgrade() {
                    w.close();
                }
                glib::Propagation::Stop
            }
            gdk::Key::s | gdk::Key::S if modifiers.contains(gdk::ModifierType::CONTROL_MASK) => {
                save_fn();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        win.add_controller(key_ctrl);
    }

    win.present();
}

// ---------------------------------------------------------------------------
// Settings window
// ---------------------------------------------------------------------------
// Settings window
// ---------------------------------------------------------------------------

/// Open the Settings window with tabs: Appearance, Behavior, Visualizer,
/// Filetypes, Media Library.
///
/// Changes made in any tab are written back to `state.config` immediately
/// when a control's value changes.  Pressing "Close" (or closing the
/// window) persists the config to disk.  `initial_tab` selects the starting
/// tab page (0-indexed), or opens at the default page if `None`.
/// `css_provider` is updated live when the user switches skins in the
/// Appearance tab.
#[allow(deprecated)]
/// Modal asking for the gnudb/CDDB email, prefilled from config. On Save it
/// stores + persists the address and runs `on_done` (e.g. retry the lookup);
/// Cancel just closes. Used when a disc action needs an email that's unset.
fn prompt_gnudb_email(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    on_done: Rc<dyn Fn()>,
) {
    let dialog = gtk4::Window::builder()
        .title("gnudb email")
        .modal(true)
        .default_width(380)
        .build();
    if let Some(p) = parent {
        dialog.set_transient_for(Some(p));
    }
    let vbox = GtkBox::new(Orientation::Vertical, 8);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);
    let info = Label::builder()
        .label(
            "gnudb needs an email address for its lookup/submission handshake. \
             It's stored locally and used only to talk to gnudb.",
        )
        .wrap(true)
        .halign(Align::Start)
        .xalign(0.0)
        .build();
    let entry = Entry::new();
    entry.set_placeholder_text(Some("you@example.com"));
    entry.set_text(&gtk_safe(&state.borrow().config.disc.gnudb_email));
    // Save stays disabled until the address has a deliverable shape
    // (x@y.z — the shared core rule); the hint explains why.
    let hint = Label::builder()
        .label("Enter a valid address (you@example.com).")
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    hint.add_css_class("dim-label");
    let btns = GtkBox::new(Orientation::Horizontal, 6);
    btns.set_halign(Align::End);
    let cancel = Button::with_label("Cancel");
    let save = Button::with_label("Save");
    save.add_css_class("suggested-action");
    save.set_sensitive(crate::disc::gnudb::is_valid_email(&entry.text()));
    hint.set_visible(!crate::disc::gnudb::is_valid_email(&entry.text()));
    {
        let save = save.clone();
        let hint = hint.clone();
        entry.connect_changed(move |e| {
            let ok = crate::disc::gnudb::is_valid_email(&e.text());
            save.set_sensitive(ok);
            hint.set_visible(!ok);
        });
    }
    btns.append(&cancel);
    btns.append(&save);
    vbox.append(&info);
    vbox.append(&entry);
    vbox.append(&hint);
    vbox.append(&btns);
    dialog.set_child(Some(&vbox));
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    {
        let save = save.clone();
        entry.connect_activate(move |_| {
            save.activate();
        });
    }
    let d = dialog.clone();
    save.connect_clicked(move |_| {
        let email = entry.text().trim().to_string();
        // Defense in depth — the button is insensitive on an invalid
        // address, but never persist one either way.
        if !crate::disc::gnudb::is_valid_email(&email) {
            return;
        }
        {
            let mut s = state.borrow_mut();
            s.config.disc.gnudb_email = email;
            let _ = s.config.save();
        }
        on_done();
        d.close();
    });
    dialog.present();
}

