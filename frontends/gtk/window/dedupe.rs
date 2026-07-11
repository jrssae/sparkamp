/// Messages sent from the background scan thread to the GTK tick loop.
enum DedupeMsg {
    Status(String),
    Done(Vec<crate::dedupe::DupeGroup>),
}

/// Open the standalone Deduplicate Music window.
///
/// Results are shown in a single virtualised `TreeView` backed by a
/// `TreeStore` so that scrolling stays smooth even with thousands of
/// duplicate groups.  Group rows are collapsed by default; clicking the
/// expander reveals each group's individual file rows.
///
/// The window immediately starts a background scan of the media library.  It
/// is independent and non-modal so the user can continue playback while
/// waiting.  A cancel button with a confirmation prompt guards against
/// accidental cancellation; closing the window while scanning also prompts.
///
/// ## TreeStore column layout
///
/// | # | Type   | Meaning                                          |
/// |---|--------|--------------------------------------------------|
/// | 0 | String | Primary label (group heading or track path)      |
/// | 1 | String | Secondary (confidence+count, or track title)     |
/// | 2 | String | Artist (empty for group rows)                    |
/// | 3 | String | Album  (empty for group rows)                    |
/// | 4 | String | Duration (empty for group rows)                  |
/// | 5 | String | File size (empty for group rows)                 |
/// | 6 | String | Bitrate  (empty for group rows)                  |
/// | 7 | String | Format   (empty for group rows)                  |
/// | 8 | i64    | Track ID (0 for group rows)                      |
/// | 9 | bool   | `true` → group row, `false` → track row          |
/// |10 | i32    | Pango weight (700 group, 400 track)              |
/// |11 | String | Full path (empty for group rows; for file-open)  |
fn open_dedupe_window(parent: Option<&gtk4::Window>, state: Rc<RefCell<AppState>>) {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    let win = gtk4::Window::new();
    win.set_title(Some("Deduplicate Music — Sparkamp"));
    win.set_default_size(900, 600);
    win.set_resizable(true);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    // ── Layout ───────────────────────────────────────────────────────────────
    let root = GtkBox::new(Orientation::Vertical, 0);

    // Status bar
    let status_row = GtkBox::new(Orientation::Horizontal, 8);
    status_row.set_margin_top(8);
    status_row.set_margin_bottom(4);
    status_row.set_margin_start(10);
    status_row.set_margin_end(10);

    let status_lbl = Label::new(Some("Preparing scan…"));
    status_lbl.set_hexpand(true);
    status_lbl.set_halign(Align::Start);

    let action_btn = Button::with_label("✕ Cancel");
    action_btn.add_css_class("pl-btn");

    status_row.append(&status_lbl);
    status_row.append(&action_btn);

    // ── Single virtualised TreeView for all groups and their tracks ───────────
    // Using TreeStore so GTK only creates widgets for visible rows regardless
    // of the total group count — essential for libraries with thousands of dupes.
    #[allow(deprecated)]
    let tree_store = gtk4::TreeStore::new(&[
        String::static_type(), // 0  primary label
        String::static_type(), // 1  secondary
        String::static_type(), // 2  artist
        String::static_type(), // 3  album
        String::static_type(), // 4  duration
        String::static_type(), // 5  size
        String::static_type(), // 6  bitrate
        String::static_type(), // 7  format
        i64::static_type(),    // 8  track id (0 for group rows)
        bool::static_type(),   // 9  is_group
        i32::static_type(),    // 10 pango weight
        String::static_type(), // 11 full path (empty for group rows)
    ]);

    #[allow(deprecated)]
    let tree_view = TreeView::with_model(&tree_store);
    tree_view.set_headers_visible(true);
    tree_view.set_enable_search(false);
    tree_view.set_activate_on_single_click(false);
    tree_view.add_css_class("playlist");
    tree_view.set_hexpand(true);
    tree_view.set_vexpand(true);

    // Build visible columns: expander on col 0, then cols 1-7.
    {
        let col_defs: &[(&str, i32, bool)] = &[
            ("Group / Path", 0, true),
            ("Title / Info", 1, false),
            ("Artist",       2, false),
            ("Album",        3, false),
            ("Duration",     4, false),
            ("Size",         5, false),
            ("Bitrate",      6, false),
            ("Format",       7, false),
        ];
        for (title, data_col, expands) in col_defs {
            #[allow(deprecated)]
            let renderer = CellRendererText::new();
            #[allow(deprecated)]
            let col = TreeViewColumn::new();
            col.set_title(title);
            col.set_resizable(true);
            col.set_expand(*expands);
            #[allow(deprecated)]
            col.pack_start(&renderer, true);
            #[allow(deprecated)]
            col.add_attribute(&renderer, "text", *data_col);
            #[allow(deprecated)]
            col.add_attribute(&renderer, "weight", 10); // pango weight col
            #[allow(deprecated)]
            tree_view.append_column(&col);
        }
    }

    let scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .hexpand(true)
        .vexpand(true)
        .child(&tree_view)
        .build();

    root.append(&status_row);
    root.append(&gtk4::Separator::new(Orientation::Horizontal));
    root.append(&scroll);
    win.set_child(Some(&root));

    // ── Shared scan state ────────────────────────────────────────────────────
    // cancel_flag is shared with the background thread.
    let cancel_flag: Rc<RefCell<Arc<AtomicBool>>> =
        Rc::new(RefCell::new(Arc::new(AtomicBool::new(false))));
    let is_scanning = Rc::new(Cell::new(false));
    // Channel receiver is replaceable so Rescan can start a new thread.
    let result_rx: Rc<RefCell<Option<std::sync::mpsc::Receiver<DedupeMsg>>>> =
        Rc::new(RefCell::new(None));

    // ── Helper: format a file size as "X.X MB" / "X KB" ────────────────────
    fn fmt_size(bytes: Option<u64>) -> String {
        match bytes {
            None => "—".to_string(),
            Some(b) if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1_000_000.0),
            Some(b) if b >= 1_000 => format!("{} KB", b / 1_000),
            Some(b) => format!("{} B", b),
        }
    }

    // ── Helper: format duration as "M:SS" ───────────────────────────────────
    fn fmt_dur(secs: Option<f64>) -> String {
        match secs {
            None => "—".to_string(),
            Some(s) => {
                let total = s as u64;
                format!("{}:{:02}", total / 60, total % 60)
            }
        }
    }

    // ── Helper: shorten a path for display ──────────────────────────────────
    fn shorten_path(path: &str, max_chars: usize) -> String {
        if path.len() <= max_chars {
            return path.to_string();
        }
        format!("…{}", &path[path.len().saturating_sub(max_chars)..])
    }

    // ── Track info lookup for playlist operations ────────────────────────────
    // Populated by `populate`; keyed by track id.
    let track_map: Rc<RefCell<std::collections::HashMap<i64, crate::dedupe::DupeTrackInfo>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // ── Populate the TreeStore after scan completes ──────────────────────────
    let populate = {
        let tree_store = tree_store.clone();
        let status_lbl = status_lbl.clone();
        let action_btn = action_btn.clone();
        let is_scanning = is_scanning.clone();
        let track_map = track_map.clone();

        Rc::new(move |groups: Vec<crate::dedupe::DupeGroup>| {
            let probable = groups
                .iter()
                .filter(|g| g.confidence == crate::dedupe::DupeConfidence::Probable)
                .count();
            let total = groups.len();

            #[allow(deprecated)]
            tree_store.clear();
            track_map.borrow_mut().clear();

            if total == 0 {
                status_lbl.set_text("No duplicates found.");
                action_btn.set_label("↺ Rescan");
                action_btn.set_visible(true);
                is_scanning.set(false);
                return;
            }
            status_lbl.set_text(&format!(
                "{probable} probable group(s), {} less-likely group(s) found",
                total - probable
            ));
            action_btn.set_label("↺ Rescan");
            action_btn.set_visible(true);
            is_scanning.set(false);

            let mut tm = track_map.borrow_mut();
            for group in &groups {
                let bullet = if group.confidence == crate::dedupe::DupeConfidence::Probable {
                    "●"
                } else {
                    "◎"
                };
                let conf_str = if group.confidence == crate::dedupe::DupeConfidence::Probable {
                    "Probable"
                } else {
                    "Less likely"
                };
                let n = group.tracks.len();
                let group_label =
                    format!("{bullet} {}  ({conf_str} · {n} files)", group.label);

                #[allow(deprecated)]
                let group_iter = tree_store.insert_with_values(
                    None,
                    None,
                    &[
                        (0, &group_label),
                        (1, &conf_str.to_string()),
                        (2, &String::new()),
                        (3, &String::new()),
                        (4, &String::new()),
                        (5, &String::new()),
                        (6, &String::new()),
                        (7, &String::new()),
                        (8, &0i64),
                        (9, &true),
                        (10, &700i32),
                        (11, &String::new()),
                    ],
                );

                for info in &group.tracks {
                    tm.insert(info.track.id, info.clone());
                    let title = info
                        .track
                        .title
                        .as_deref()
                        .unwrap_or(info.track.filename.as_str())
                        .to_string();
                    let artist =
                        info.track.artist.as_deref().unwrap_or("—").to_string();
                    let album =
                        info.track.album.as_deref().unwrap_or("—").to_string();
                    let dur = fmt_dur(info.track.length_secs);
                    let size = fmt_size(info.file_size_bytes);
                    let kbps = info
                        .track
                        .bitrate
                        .map_or("—".to_string(), |b| format!("{b} kbps"));
                    let fmt =
                        info.track.filetype.as_deref().unwrap_or("—").to_string();
                    let short = shorten_path(&info.track.path, 55);

                    #[allow(deprecated)]
                    tree_store.insert_with_values(
                        Some(&group_iter),
                        None,
                        &[
                            (0, &short),
                            (1, &title),
                            (2, &artist),
                            (3, &album),
                            (4, &dur),
                            (5, &size),
                            (6, &kbps),
                            (7, &fmt),
                            (8, &info.track.id),
                            (9, &false),
                            (10, &400i32),
                            (11, &info.track.path),
                        ],
                    );
                }
            }
        })
    };

    // ── Right-click on a group or track row ──────────────────────────────────
    {
        let tree_store_rc = tree_store.clone();
        let tree_view_rc = tree_view.clone();
        let track_map_rc = track_map.clone();
        let state_rc = state.clone();

        let rclick = GestureClick::new();
        rclick.set_button(gdk::BUTTON_SECONDARY);
        rclick.connect_pressed(move |_, _, x, y| {
            // GestureClick gives widget-space coordinates (origin at top-left of
            // the TreeView widget, including the column-header row).
            // path_at_pos expects bin-window coordinates (origin at the top of
            // the scrollable content area, below the headers).
            // Convert before calling so the header row does not cause an
            // off-by-one in row detection.
            #[allow(deprecated)]
            let (bx, by) = tree_view_rc.convert_widget_to_bin_window_coords(x as i32, y as i32);
            #[allow(deprecated)]
            let Some((Some(tpath), _, _, _)) =
                tree_view_rc.path_at_pos(bx, by)
            else {
                return;
            };
            #[allow(deprecated)]
            let Some(row_iter) = tree_store_rc.iter(&tpath) else { return };

            // Determine row type by position in the tree: top-level rows are
            // groups, child rows are individual tracks.
            #[allow(deprecated)]
            let is_group = tree_store_rc.iter_parent(&row_iter).is_none();

            let pop_box = GtkBox::new(Orientation::Vertical, 0);

            if is_group {
                // ── Group row: add/replace playlist ──────────────────────
                let add_group = {
                    let ts = tree_store_rc.clone();
                    let giter = row_iter.clone();
                    let tm = track_map_rc.clone();
                    let st = state_rc.clone();
                    move |replace: bool| {
                        let giter = giter.clone();
                        let autoplay = st.borrow().config.behavior.autoplay_on_add;
                        let was_empty = st.borrow().playlist.is_empty();
                        if replace {
                            let _ = st.borrow_mut().player.stop();
                            st.borrow_mut().playlist.clear();
                        }
                        let insert_start = st.borrow().playlist.len();
                        let tm_borrow = tm.borrow();
                        #[allow(deprecated)]
                        if let Some(ci) = ts.iter_children(Some(&giter)) {
                            loop {
                                #[allow(deprecated)]
                                let tid: i64 =
                                    ts.get_value(&ci, 8).get::<i64>().unwrap_or(0);
                                if let Some(info) = tm_borrow.get(&tid) {
                                    st.borrow_mut()
                                        .playlist
                                        .add(crate::model::Track::from(&info.track));
                                }
                                #[allow(deprecated)]
                                if !ts.iter_next(&ci) {
                                    break;
                                }
                            }
                        }
                        drop(tm_borrow);
                        if let Some(ref cb) =
                            st.borrow().rebuild_pl_callback.clone()
                        {
                            cb();
                        }
                        if autoplay && (was_empty || replace) {
                            st.borrow_mut().playlist.jump_to(insert_start);
                            if let Some(ref cb) =
                                st.borrow().play_and_update_callback.clone()
                            {
                                cb();
                            }
                        }
                    }
                };
                let add_group = Rc::new(add_group);

                let btn_add = Button::with_label("Add to playlist");
                btn_add.add_css_class("popover-button");
                {
                    let ag = add_group.clone();
                    btn_add.connect_clicked(move |_| ag(false));
                }
                let btn_replace = Button::with_label("Replace playlist");
                btn_replace.add_css_class("popover-button");
                {
                    let ag = add_group;
                    btn_replace.connect_clicked(move |_| ag(true));
                }

                pop_box.append(&btn_add);
                pop_box.append(&btn_replace);
            } else {
                // ── Track row: open file location / dismiss ───────────────
                #[allow(deprecated)]
                let full_path: String = tree_store_rc
                    .get_value(&row_iter, 11)
                    .get::<String>()
                    .unwrap_or_default();

                let parent_dir = std::path::Path::new(&full_path)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();

                let btn_open = Button::with_label("Open file location");
                btn_open.add_css_class("popover-button");
                {
                    let dir = parent_dir.clone();
                    btn_open.connect_clicked(move |_| {
                        let uri = format!("file://{dir}");
                        let _ = gio::AppInfo::launch_default_for_uri(
                            &uri,
                            None::<&gio::AppLaunchContext>,
                        );
                    });
                }

                let btn_dismiss = Button::with_label("Not a duplicate");
                btn_dismiss.add_css_class("popover-button");
                {
                    let ts = tree_store_rc.clone();
                    let path_str = tpath.to_str().map(|s| s.to_string()).unwrap_or_default();
                    btn_dismiss.connect_clicked(move |_| {
                        #[allow(deprecated)]
                        let Some(ti) = ts.iter_from_string(&path_str) else {
                            return;
                        };
                        #[allow(deprecated)]
                        let parent_opt = ts.iter_parent(&ti);
                        #[allow(deprecated)]
                        ts.remove(&ti);
                        // Remove the group row when fewer than 2 tracks remain.
                        if let Some(pi) = parent_opt {
                            #[allow(deprecated)]
                            let remaining = ts.iter_n_children(Some(&pi));
                            if remaining < 2 {
                                #[allow(deprecated)]
                                ts.remove(&pi);
                            }
                        }
                    });
                }

                pop_box.append(&btn_open);
                pop_box.append(&btn_dismiss);
            }

            let popover = gtk4::Popover::new();
            popover.set_child(Some(&pop_box));
            popover.set_parent(&tree_view_rc);
            popover.set_pointing_to(Some(&gdk::Rectangle::new(
                x as i32, y as i32, 1, 1,
            )));
            popover.popup();
        });
        #[allow(deprecated)]
        tree_view.add_controller(rclick);
    }

    // ── Start a background scan ──────────────────────────────────────────────
    let start_scan = {
        let cancel_flag = cancel_flag.clone();
        let result_rx = result_rx.clone();
        let is_scanning = is_scanning.clone();
        let status_lbl = status_lbl.clone();
        let action_btn = action_btn.clone();
        let tree_store = tree_store.clone();

        Rc::new(move || {
            #[allow(deprecated)]
            tree_store.clear();

            // Fresh cancel flag for the new scan.
            let new_cancel = Arc::new(AtomicBool::new(false));
            *cancel_flag.borrow_mut() = new_cancel.clone();

            let (tx, rx) = std::sync::mpsc::channel::<DedupeMsg>();
            *result_rx.borrow_mut() = Some(rx);

            is_scanning.set(true);
            status_lbl.set_text("Loading tracks from library…");
            action_btn.set_label("✕ Cancel");
            action_btn.set_visible(true);

            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            std::thread::spawn(move || {
                let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = tx.send(DedupeMsg::Status(format!("Error opening library: {e}")));
                        return;
                    }
                };

                if new_cancel.load(Ordering::Relaxed) {
                    return;
                }

                let tracks = match lib.scanned_tracks() {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = tx.send(DedupeMsg::Status(format!("Error reading tracks: {e}")));
                        return;
                    }
                };

                if new_cancel.load(Ordering::Relaxed) {
                    return;
                }

                let n = tracks.len();
                let _ = tx.send(DedupeMsg::Status(format!("Analyzing {n} tracks…")));

                let groups = crate::dedupe::find_duplicates(tracks);

                if new_cancel.load(Ordering::Relaxed) {
                    return;
                }

                let _ = tx.send(DedupeMsg::Done(groups));
            });
        })
    };

    // ── Tick loop — drain the channel while the window is open ───────────────
    {
        let result_rx = result_rx.clone();
        let status_lbl = status_lbl.clone();
        let is_scanning = is_scanning.clone();
        let populate = populate.clone();
        let win_wk = win.downgrade();

        glib::timeout_add_local(Duration::from_millis(200), move || {
            if win_wk.upgrade().is_none() {
                return ControlFlow::Break;
            }
            if !is_scanning.get() {
                return ControlFlow::Continue;
            }
            let msg = result_rx.borrow().as_ref().and_then(|rx| rx.try_recv().ok());
            match msg {
                Some(DedupeMsg::Status(s)) => {
                    status_lbl.set_text(&s);
                }
                Some(DedupeMsg::Done(groups)) => {
                    populate(groups);
                }
                None => {} // still scanning or disconnected
            }
            ControlFlow::Continue
        });
    }

    // ── Cancel / Rescan button ────────────────────────────────────────────────
    {
        let cancel_flag = cancel_flag.clone();
        let is_scanning = is_scanning.clone();
        let status_lbl = status_lbl.clone();
        let action_btn2 = action_btn.clone();
        let start_scan2 = start_scan.clone();
        let win_wk = win.downgrade();

        action_btn.connect_clicked(move |_btn| {
            if is_scanning.get() {
                // Show confirmation before cancelling.
                let dialog = gtk4::AlertDialog::builder()
                    .message("Cancel scan?")
                    .detail(
                        "The scan will need to restart from the beginning if you cancel.",
                    )
                    .buttons(vec!["Keep scanning".to_string(), "Cancel scan".to_string()])
                    .cancel_button(0)
                    .default_button(0)
                    .modal(true)
                    .build();
                let flag = cancel_flag.borrow().clone();
                let scanning = is_scanning.clone();
                let lbl = status_lbl.clone();
                let btn2 = action_btn2.clone();
                dialog.choose(
                    win_wk.upgrade().as_ref(),
                    None::<&gio::Cancellable>,
                    move |result| {
                        if result == Ok(1) {
                            flag.store(true, Ordering::Relaxed);
                            scanning.set(false);
                            lbl.set_text("Scan cancelled.");
                            btn2.set_label("↺ Rescan");
                        }
                    },
                );
            } else {
                // Rescan.
                start_scan2();
            }
        });
    }

    // ── Confirm close if scan is in progress ─────────────────────────────────
    win.connect_close_request({
        let cancel_flag = cancel_flag.clone();
        let is_scanning = is_scanning.clone();
        move |w| {
            if is_scanning.get() {
                let dialog = gtk4::AlertDialog::builder()
                    .message("Scan in progress")
                    .detail("Closing this window will cancel the scan.")
                    .buttons(vec!["Keep open".to_string(), "Close anyway".to_string()])
                    .cancel_button(0)
                    .default_button(0)
                    .modal(true)
                    .build();
                let flag = cancel_flag.borrow().clone();
                let scanning = is_scanning.clone();
                let win_wk = w.downgrade();
                dialog.choose(Some(w), None::<&gio::Cancellable>, move |result| {
                    if result == Ok(1) {
                        flag.store(true, Ordering::Relaxed);
                        scanning.set(false);
                        if let Some(w) = win_wk.upgrade() {
                            w.destroy();
                        }
                    }
                });
                return glib::Propagation::Stop; // prevent default close
            }
            glib::Propagation::Proceed
        }
    });

    win.present();

    // Start the initial scan immediately after presenting the window.
    start_scan();
}

// ---------------------------------------------------------------------------
// Media Library browser window
// ---------------------------------------------------------------------------

