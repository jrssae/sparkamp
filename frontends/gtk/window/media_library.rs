fn open_media_library_window(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    rebuild_playlist: Rc<dyn Fn()>,
    set_track: Rc<dyn Fn(&str)>,
    // Owned by player.rs's build() and threaded in here so the active
    // playlist's Send-to menu shares the same drives/devices lists, burn
    // queue, and device-copy runner as this window's Files/Editor/Device
    // views (Task 8).
    current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>>,
    current_devices: Rc<RefCell<Vec<crate::devices::Device>>>,
    burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>>,
    copy_files_holder: Rc<
        RefCell<Option<Rc<dyn Fn(crate::devices::Device, Vec<std::path::PathBuf>)>>>,
    >,
    // Filled by the burn panel with a closure that re-renders the shown
    // drive's queue; the Send-to ▸ Disc Drive actions call it so an external
    // add updates the open panel live (2026-07-16).
    burn_refresh_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    init_width: i32,
    init_height: i32,
) -> gtk4::Window {
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
    // Wrap sidebar in a ScrolledWindow so many playlists don't overflow.
    let sidebar = ListBox::new();
    sidebar.set_selection_mode(gtk4::SelectionMode::Single);
    sidebar.add_css_class("ml-sidebar");
    sidebar.set_vexpand(true);

    // Latest detected devices — now a parameter (shared with player.rs's
    // active playlist Send-to menu), kept current by the poll below.

    // Per-device (song, playlist) counts for the overview cards, keyed by
    // backend_id. Computed off-thread on first show and cleared whenever a
    // device's contents change (see reload_device_store). `counts_in_flight`
    // guards against spawning the same count walk twice.
    let device_counts: Rc<RefCell<std::collections::HashMap<String, (usize, usize)>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let counts_in_flight: Rc<RefCell<std::collections::HashSet<String>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));

    // Live copy progress per device (backend_id → (done, total)); absent = idle.
    // `device_card_progress` maps a backend_id to its overview card's progress
    // bar (rebuilt each overview render). Together they let a copy show progress
    // on the card and survive a poll-driven rebuild mid-transfer.
    let device_transfers: Rc<RefCell<std::collections::HashMap<String, (usize, usize)>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let device_card_progress: Rc<RefCell<std::collections::HashMap<String, gtk4::ProgressBar>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // Apply (or clear) a transfer's progress to a card's bar. The bar always
    // occupies its space; idle just makes it transparent so the card never
    // changes size between copying and not.
    let update_card_progress: Rc<dyn Fn(&str, Option<(usize, usize)>)> = {
        let transfers = device_transfers.clone();
        let bars = device_card_progress.clone();
        Rc::new(move |backend: &str, state: Option<(usize, usize)>| {
            match state {
                Some(v) => {
                    transfers.borrow_mut().insert(backend.to_string(), v);
                }
                None => {
                    transfers.borrow_mut().remove(backend);
                }
            }
            if let Some(bar) = bars.borrow().get(backend) {
                apply_card_progress(bar, state);
            }
        })
    };

    // Sidebar DropTarget — accept FileList drags from the active playlist,
    // ML files view, or ML editor and append paths to the saved playlist
    // whose `pl:<id>` row is under the drop coordinate.  Drops landing on
    // the Files/Playlists header rows fall through to no-op.
    // Deferred handle to the playlist-send runner (defined later, in the
    // device-view section). Lets the sidebar drop handler send a playlist
    // dragged onto a device row.
    let send_playlist_holder: Rc<
        RefCell<Option<Rc<dyn Fn(crate::devices::Device, i64, String)>>>,
    > = Rc::new(RefCell::new(None));
    // copy_files_holder is now a parameter (shared with player.rs's active
    // playlist Send-to menu) — see the fn signature above.
    {
        let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        dt.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
        let sidebar_for_drop = sidebar.clone();
        let state_for_drop   = state.clone();
        let current_devices_drop = current_devices.clone();
        let send_holder_drop = send_playlist_holder.clone();
        let copy_holder_drop = copy_files_holder.clone();
        dt.connect_drop(move |_, value, _x, y| {
            // Locate the sidebar row under the drop coordinate.
            let mut hit: Option<ListBoxRow> = None;
            let mut i = 0i32;
            while let Some(r) = sidebar_for_drop.row_at_index(i) {
                if let Some(b) = r.compute_bounds(&sidebar_for_drop) {
                    if y as f32 >= b.y() && y as f32 <= b.y() + b.height() {
                        hit = Some(r);
                        break;
                    }
                }
                i += 1;
            }
            let Some(row) = hit else { return false };
            let name = row.widget_name().to_string();

            // Resolve the drag payload. A playlist row drags a `pl:<id>`
            // String. Track drags ship a FileList — but when the drop target
            // also advertises STRING (it does, for `pl:`), GTK may instead
            // deliver the FileList as a text/uri-list String. Handle both so a
            // drag from the active playlist works regardless of which format
            // gets negotiated.
            enum Payload {
                Playlist(i64),
                Files(Vec<std::path::PathBuf>),
            }
            let payload = if let Ok(s) = value.get::<String>() {
                if let Some(pid) = s.strip_prefix("pl:").and_then(|n| n.trim().parse::<i64>().ok())
                {
                    Payload::Playlist(pid)
                } else {
                    // A newline-separated uri-list or path-list.
                    let paths: Vec<std::path::PathBuf> = s
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty() && !l.starts_with('#'))
                        .map(|l| {
                            if l.starts_with("file://") {
                                gio::File::for_uri(l)
                                    .path()
                                    .unwrap_or_else(|| std::path::PathBuf::from(l))
                            } else {
                                std::path::PathBuf::from(l)
                            }
                        })
                        .collect();
                    if paths.is_empty() {
                        return false;
                    }
                    Payload::Files(paths)
                }
            } else if let Ok(file_list) = value.get::<gdk::FileList>() {
                let paths: Vec<std::path::PathBuf> =
                    file_list.files().iter().filter_map(|f| f.path()).collect();
                if paths.is_empty() {
                    return false;
                }
                Payload::Files(paths)
            } else {
                return false;
            };

            match payload {
                // Playlist dropped onto a device row → send files + .m3u8.
                Payload::Playlist(pid) => {
                    let Some(backend) = name.strip_prefix("dev:") else {
                        return false;
                    };
                    let Some(dev) = current_devices_drop
                        .borrow()
                        .iter()
                        .find(|d| d.backend_id == backend)
                        .cloned()
                    else {
                        return false;
                    };
                    let plname = state_for_drop
                        .borrow()
                        .media_lib
                        .as_ref()
                        .and_then(|l| l.playlist_by_id(pid).ok())
                        .map(|p| p.name)
                        .unwrap_or_default();
                    if let Some(send) = send_holder_drop.borrow().as_ref() {
                        send(dev, pid, plname);
                        return true;
                    }
                    false
                }
                Payload::Files(srcs) => {
                    // Onto a device row → copy the files (async, with progress).
                    if let Some(backend) = name.strip_prefix("dev:") {
                        let Some(dev) = current_devices_drop
                            .borrow()
                            .iter()
                            .find(|d| d.backend_id == backend)
                            .cloned()
                        else {
                            return false;
                        };
                        if let Some(copy) = copy_holder_drop.borrow().as_ref() {
                            copy(dev, srcs);
                            return true;
                        }
                        return false;
                    }
                    // Onto a saved-playlist row → append the files to it.
                    let Some(pid) =
                        name.strip_prefix("pl:").and_then(|n| n.parse::<i64>().ok())
                    else {
                        return false;
                    };
                    let path_strs: Vec<String> =
                        srcs.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                    if let Some(lib) = state_for_drop.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.append_paths_to_playlist(pid, &path_strs) {
                            eprintln!("append_paths_to_playlist {pid}: {e}");
                            return false;
                        }
                    }
                    notify_playlist_changed(pid);
                    true
                }
            }
        });
        sidebar.add_controller(dt);
    }

    let sidebar_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&sidebar)
        .build();

    // ── "Files" row ───────────────────────────────────────────────────────
    {
        let lbl = Label::builder()
            .label("Files")
            .halign(Align::Start)
            .xalign(0.0)
            .margin_start(10)
            .margin_end(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let row = ListBoxRow::new();
        row.set_widget_name("files");
        row.set_child(Some(&lbl));
        sidebar.append(&row);
    }

    // ── "Playlists" header row (with expand/collapse chevron) ─────────────
    let playlists_expanded = Rc::new(Cell::new(
        state.borrow().config.window.ml_playlists_expanded
    ));

    // Track sub-rows so we can show/hide them on toggle
    let pl_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let pl_header_box = GtkBox::new(Orientation::Horizontal, 0);

        let pl_lbl = Label::builder()
            .label("Playlists")
            .halign(Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .margin_start(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();

        // Chevron label — "▾" expanded, "▸" collapsed
        let chevron_lbl = Label::builder()
            .label(if playlists_expanded.get() { "▾" } else { "▸" })
            .margin_end(8)
            .build();

        pl_header_box.append(&pl_lbl);
        pl_header_box.append(&chevron_lbl);

        let row_playlists = ListBoxRow::new();
        row_playlists.set_widget_name("playlists");
        row_playlists.set_child(Some(&pl_header_box));
        sidebar.append(&row_playlists);

        // Chevron click toggles expansion (separate from navigation)
        let gesture = GestureClick::new();
        let expanded_rc = playlists_expanded.clone();
        let sub_rows_rc  = pl_sub_rows.clone();
        let chev = chevron_lbl.clone();
        gesture.connect_released(move |g, _n, x, _y| {
            // Only handle clicks in the right ~20px (chevron area)
            let widget = g.widget();
            let width  = widget.map(|w| w.width()).unwrap_or(0) as f64;
            if x < width - 24.0 {
                return; // let the row selection handle the left area
            }
            let new_val = !expanded_rc.get();
            expanded_rc.set(new_val);
            chev.set_text(if new_val { "▾" } else { "▸" });
            for r in sub_rows_rc.borrow().iter() {
                r.set_visible(new_val);
            }
        });
        row_playlists.add_controller(gesture);
    }

    // Populate initial playlist sub-rows
    {
        let playlists_initial = state
            .borrow()
            .media_lib
            .as_ref()
            .and_then(|lib| lib.all_playlists().ok())
            .unwrap_or_default();
        let expanded = playlists_expanded.get();
        for pl in &playlists_initial {
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(24)  // indent
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
            let row = ListBoxRow::new();
            row.set_widget_name(&format!("pl:{}", pl.id));
            row.set_child(Some(&lbl));
            row.set_visible(expanded);
            attach_pl_row_drag(&row, pl.id);
            sidebar.append(&row);
            pl_sub_rows.borrow_mut().push(row);
        }
    }

    // ── "Disc Drives" header row (optical drives via crate::disc) ─────────
    // Sits just above Devices. Disc sub-rows are inserted between this header
    // and the Devices header; device rows keep appending to the sidebar end, so
    // the two groups stay separate. Phase 1: detection + audio-CD playback.
    let discs_expanded = Rc::new(Cell::new(true));
    let disc_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));
    // current_drives is now a parameter (shared with player.rs's active
    // playlist Send-to menu).
    let selected_disc_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    // Per-drive burn queues — burn_queues is now a parameter (shared with
    // player.rs's active playlist Send-to menu). Each drive's list is
    // separate from the active playlist and from every other drive's list,
    // fed from the Files view's context menu, consumed by the burn panel
    // for the drive it shows.
    // refresh_discs is built much later; the burn panel takes this holder so
    // a finished burn can trigger a re-poll.
    let refresh_discs_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    // Live burn progress, keyed by drive id (Task 7). The burn poller in
    // `build_burn_panel` writes an entry on every `BurnMsg::Progress` and
    // removes it on Done/Failed/Cancelled; `populate_disc_detail` reads it to
    // decide whether the disc-detail overlay card should be showing when a
    // drive is (re)selected — this is what makes navigate-away-and-back
    // re-show a live burn instead of losing it. Borrows are always short and
    // never held across a populate/select call (see disc.rs's crash note).
    let burn_progress_map: Rc<RefCell<std::collections::HashMap<String, crate::disc::burn::BurnProgress>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let current_disc_entries: Rc<RefCell<Vec<crate::disc::DiscTrackEntry>>> =
        Rc::new(RefCell::new(Vec::new()));
    // Task 9 — data-disc file browsing. True while a mount+walk or a
    // to-library copy is in flight for the data-disc file list, so a second
    // trigger (a poll tick landing mid-fetch) is skipped rather than piling
    // on a second disc read.
    let disc_files_busy: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    // Phase 2 — per-disc gnudb tags, keyed by freedb id. `disc_tags` is the
    // user's current set (drives titles/artist/album, and rip/submit later);
    // `disc_official` keeps the untouched gnudb match as the submission
    // baseline. Both are seeded from the shared on-disk store so names survive
    // restarts. `pending_disc_matches` parks a multi-match result (discid +
    // candidates) when the user leaves the view before choosing.
    let disc_tags: Rc<RefCell<std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let disc_official: Rc<
        RefCell<std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>>,
    > = Rc::new(RefCell::new(std::collections::HashMap::new()));
    {
        let store = crate::disc::tagstore::DiscTagStore::load();
        for (id, rec) in store.discs {
            disc_tags.borrow_mut().insert(id.clone(), rec.user);
            if let Some(o) = rec.official {
                disc_official.borrow_mut().insert(id, o);
            }
        }
    }
    // CD-TEXT read off the physical disc (display-only, keyed by freedb id):
    // a burned/commercial disc with no gnudb match still shows real names.
    // Never persisted to the tag store; `disc_cdtext_tried` stops us
    // re-reading the same disc on every populate.
    let disc_cdtext: Rc<RefCell<std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let disc_cdtext_tried: Rc<RefCell<std::collections::HashSet<String>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));
    // Filled with populate_disc_detail after it's built, so the async CD-TEXT
    // read can re-render the shown drive once names arrive.
    let populate_holder: Rc<RefCell<Option<Rc<dyn Fn(&crate::disc::OpticalDrive)>>>> =
        Rc::new(RefCell::new(None));
    // Phase 3 rip state: a cancel flag shared with the worker thread, and a
    // guard so only one rip runs at a time.
    let rip_cancel: Rc<RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>> =
        Rc::new(RefCell::new(None));
    let rip_active = Rc::new(Cell::new(false));
    // True until the first drive poll finishes, so the overview shows a
    // "Detecting…" hint instead of a premature "No disc drives connected".
    let disc_detecting = Rc::new(Cell::new(true));
    // Spinner shown in the sidebar header while that first poll runs; stopped
    // and hidden by refresh_discs once detection completes.
    let disc_detect_spinner = gtk4::Spinner::new();
    // Sits immediately after the "Disc Drives" label (not far-right, where a wide
    // sidebar would push it off-screen). An unsized spinner in a header slot can
    // render 0×0, so give it an explicit size and center it vertically.
    disc_detect_spinner.set_margin_start(6);
    disc_detect_spinner.set_size_request(16, 16);
    disc_detect_spinner.set_valign(Align::Center);
    disc_detect_spinner.start();
    {
        let hdr = GtkBox::new(Orientation::Horizontal, 0);
        // Label takes only its text width (no hexpand) so the spinner can follow
        // it directly; a hexpanding spacer then keeps the chevron right-aligned.
        let lbl = Label::builder()
            .label("Disc Drives")
            .halign(Align::Start)
            .xalign(0.0)
            .margin_start(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let spacer = Label::new(None);
        spacer.set_hexpand(true);
        let chev = Label::builder()
            .label(if discs_expanded.get() { "▾" } else { "▸" })
            .margin_end(8)
            .build();
        hdr.append(&lbl);
        hdr.append(&disc_detect_spinner);
        hdr.append(&spacer);
        hdr.append(&chev);
        let row = ListBoxRow::new();
        row.set_widget_name("discs");
        row.set_child(Some(&hdr));
        sidebar.append(&row);

        let gesture = GestureClick::new();
        let exp = discs_expanded.clone();
        let subs = disc_sub_rows.clone();
        let chev2 = chev.clone();
        gesture.connect_released(move |g, _n, x, _y| {
            let w = g.widget().map(|w| w.width()).unwrap_or(0) as f64;
            if x < w - 24.0 {
                return; // left of the chevron = navigation, handled elsewhere
            }
            let v = !exp.get();
            exp.set(v);
            chev2.set_text(if v { "▾" } else { "▸" });
            for r in subs.borrow().iter() {
                r.set_visible(v);
            }
        });
        row.add_controller(gesture);
    }

    // ── "Devices" header row (external USB/SD storage via udisks2) ────────
    // Mirrors the Playlists header: an expand/collapse chevron, with device
    // sub-rows populated live by the poll below.
    let devices_expanded = Rc::new(Cell::new(true));
    let dev_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));
    // `current_devices` is declared earlier (before the sidebar DropTarget).
    {
        let hdr = GtkBox::new(Orientation::Horizontal, 0);
        let lbl = Label::builder()
            .label("Devices")
            .halign(Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .margin_start(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let chev = Label::builder()
            .label(if devices_expanded.get() { "▾" } else { "▸" })
            .margin_end(8)
            .build();
        hdr.append(&lbl);
        hdr.append(&chev);
        let row = ListBoxRow::new();
        row.set_widget_name("devices");
        row.set_child(Some(&hdr));
        sidebar.append(&row);

        let gesture = GestureClick::new();
        let exp = devices_expanded.clone();
        let subs = dev_sub_rows.clone();
        let chev2 = chev.clone();
        gesture.connect_released(move |g, _n, x, _y| {
            let w = g.widget().map(|w| w.width()).unwrap_or(0) as f64;
            if x < w - 24.0 {
                return; // left of the chevron = navigation, handled elsewhere
            }
            let v = !exp.get();
            exp.set(v);
            chev2.set_text(if v { "▾" } else { "▸" });
            for r in subs.borrow().iter() {
                r.set_visible(v);
            }
        });
        row.add_controller(gesture);
    }

    // ── Devices content page widgets (added to the stack below) ───────────
    let dev_page = GtkBox::new(Orientation::Vertical, 8);
    dev_page.set_margin_top(8);
    dev_page.set_margin_start(8);
    dev_page.set_margin_end(8);

    // Diagnostics banner — shown only when udisks2 can't be reached.
    let dev_banner = GtkBox::new(Orientation::Horizontal, 8);
    dev_banner.set_visible(false);
    let dev_banner_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    dev_banner_lbl.add_css_class("broken");
    let dev_banner_retry = Button::with_label("Retry");
    dev_banner_retry.add_css_class("pl-btn");
    dev_banner.append(&dev_banner_lbl);
    dev_banner.append(&dev_banner_retry);
    dev_page.append(&dev_banner);

    // ── Overview: a live list of all connected devices (shown when the
    // Devices header is selected). ───────────────────────────────────────
    let dev_overview = GtkBox::new(Orientation::Vertical, 6);
    let dev_overview_title = Label::builder()
        .label("Devices")
        .halign(Align::Start)
        .xalign(0.0)
        .build();
    dev_overview_title.add_css_class("ml-section-header");
    dev_overview.append(&dev_overview_title);
    let dev_overview_list = GtkBox::new(Orientation::Vertical, 12);
    dev_overview_list.set_margin_top(6);
    dev_overview.append(&dev_overview_list);
    dev_page.append(&dev_overview);

    // ── Detail: the selected device (hidden until one is picked) ─────────
    let dev_detail = GtkBox::new(Orientation::Vertical, 8);
    dev_detail.set_visible(false);

    // Header band: device icon · name + (filesystem · path) · status badges ·
    // Sync / Eject. Populated by the device-select handler.
    let dev_icon = Image::from_icon_name("drive-removable-media");
    dev_icon.set_pixel_size(40);
    dev_icon.set_valign(Align::Center);

    let dev_title = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_title.add_css_class("device-detail-name");
    // Filesystem + mount path subtitle (selectable so the path can be copied).
    let dev_path = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .selectable(true)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();
    dev_path.add_css_class("status-label");
    // Unsupported-filesystem tag sits under the "fs · path" line on the left,
    // left-aligned and a touch smaller than the read-only pill.
    let dev_warn_badge = Label::new(Some("⚠ Unsupported"));
    dev_warn_badge.add_css_class("device-badge");
    dev_warn_badge.add_css_class("device-badge-warn");
    dev_warn_badge.add_css_class("device-badge-sm");
    dev_warn_badge.set_halign(Align::Start);
    dev_warn_badge.set_margin_top(4);
    dev_warn_badge.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
    dev_warn_badge.set_visible(false);

    let dev_title_box = GtkBox::new(Orientation::Vertical, 0);
    dev_title_box.set_valign(Align::Center);
    dev_title_box.append(&dev_title);
    dev_title_box.append(&dev_path);
    dev_title_box.append(&dev_warn_badge);

    let dev_ro_badge = Label::new(Some("🔒 Read-only"));
    dev_ro_badge.add_css_class("device-badge");
    dev_ro_badge.set_valign(Align::Center);
    dev_ro_badge.set_visible(false);

    let dev_scan = Button::with_label("Scan");
    dev_scan.add_css_class("pl-btn");
    dev_scan.set_valign(Align::Center);
    dev_scan.set_tooltip_text(Some("Re-read tags + duration from the files on this device"));
    dev_scan.set_sensitive(false);
    let dev_sync = Button::with_label("Sync");
    dev_sync.add_css_class("pl-btn");
    dev_sync.set_valign(Align::Center);
    dev_sync.set_sensitive(false);
    let dev_eject = Button::with_label("Eject");
    dev_eject.add_css_class("pl-btn");
    dev_eject.set_valign(Align::Center);
    dev_eject.set_sensitive(false);

    // Capacity meter — capacity bar + used/free/total text. Lives in the header
    // band (between the name/path and the Sync/Eject buttons) to save vertical
    // space, taking the flexible middle column.
    let dev_levelbar = gtk4::LevelBar::new();
    dev_levelbar.set_min_value(0.0);
    dev_levelbar.set_max_value(1.0);
    dev_levelbar.add_css_class("device-capacity");
    dev_levelbar.set_valign(Align::Center);
    let dev_capacity = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_capacity.add_css_class("status-label");
    dev_capacity.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    // Third row of the capacity area: "X playlists - Y audio files".
    let dev_counts = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_counts.add_css_class("status-label");
    dev_counts.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    let dev_capacity_box = GtkBox::new(Orientation::Vertical, 2);
    dev_capacity_box.set_hexpand(true);
    dev_capacity_box.set_valign(Align::Center);
    // Triple the breathing room on either side of the capacity bar.
    dev_capacity_box.set_margin_start(30);
    dev_capacity_box.set_margin_end(30);
    dev_capacity_box.append(&dev_levelbar);
    dev_capacity_box.append(&dev_capacity);
    dev_capacity_box.append(&dev_counts);

    let dev_hdr_row = GtkBox::new(Orientation::Horizontal, 10);
    dev_hdr_row.add_css_class("device-detail-header");
    dev_hdr_row.append(&dev_icon);
    dev_hdr_row.append(&dev_title_box);
    dev_hdr_row.append(&dev_capacity_box);
    dev_hdr_row.append(&dev_ro_badge);
    dev_hdr_row.append(&dev_scan);
    dev_hdr_row.append(&dev_sync);
    dev_hdr_row.append(&dev_eject);
    dev_detail.append(&dev_hdr_row);

    // Copy progress bar — shown only while files are being copied to this
    // device; carries an "x/y · filename" label.
    // Thick accent bar matching the capacity bar above; the live "Copying x/y ·
    // filename" text rides in the status bar (`dev_hint`), so the bar itself
    // carries no inline text and can be slim/tall like the capacity meter.
    let dev_progress = gtk4::ProgressBar::new();
    dev_progress.set_show_text(false);
    dev_progress.set_visible(false);
    dev_progress.add_css_class("device-progress");
    dev_detail.append(&dev_progress);

    // Caution banner for a connected device with no readable filesystem (an
    // MTP phone whose storage isn't shared). Shown in place of the playlist and
    // file lists, which are hidden while it is up.
    let dev_nofs_banner = GtkBox::new(Orientation::Horizontal, 8);
    dev_nofs_banner.set_visible(false);
    dev_nofs_banner.set_margin_top(12);
    dev_nofs_banner.set_margin_bottom(12);
    let dev_nofs_lbl = Label::builder()
        .label(
            "⚠ No visible filesystem on this device. Set the phone to file-transfer \
             mode and allow access, or reconnect it, then press Scan.",
        )
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    dev_nofs_lbl.add_css_class("broken");
    dev_nofs_banner.append(&dev_nofs_lbl);
    dev_detail.append(&dev_nofs_banner);

    // Playlists section header: a "Playlists" label on the left and an always-
    // available "+ New" button on the right that creates a device-only playlist.
    let dev_pl_header_lbl = Label::builder()
        .label("Playlists")
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .build();
    dev_pl_header_lbl.add_css_class("ml-section-header");
    let dev_pl_new = Button::with_label("+ New");
    dev_pl_new.add_css_class("pl-btn");
    let dev_pl_header = GtkBox::new(Orientation::Horizontal, 6);
    dev_pl_header.append(&dev_pl_header_lbl);
    dev_pl_header.append(&dev_pl_new);
    dev_detail.append(&dev_pl_header);
    // Filter chips: "All files" + one toggle per device .m3u/.m3u8 (grouped so
    // exactly one is active, radio-style). Rebuilt per device by
    // reload_dev_playlists; the active chip drives the track filter.
    // Chips wrap onto multiple rows (no horizontal scroll that hid the names).
    let dev_pl_chips = gtk4::FlowBox::builder()
        .orientation(Orientation::Horizontal)
        .selection_mode(gtk4::SelectionMode::None)
        .row_spacing(4)
        .column_spacing(4)
        .min_children_per_line(1)
        .max_children_per_line(64)
        .homogeneous(false)
        .build();
    dev_pl_chips.add_css_class("device-chips");
    dev_pl_chips.set_valign(Align::Start);
    let dev_pl_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        // One chip row when there's a single row; grow as chips wrap, up to
        // ~2.5 rows before a vertical scrollbar appears. (No propagate-natural-
        // height: the FlowBox over-estimates row count and would inflate to the
        // max even for a single row.)
        .min_content_height(34)
        .max_content_height(80)
        .child(&dev_pl_chips)
        .build();
    dev_pl_scroll.set_vexpand(false);
    dev_detail.append(&dev_pl_scroll);

    // Per-playlist management actions — shown only when a specific playlist chip
    // (not "All files") is selected. Click handlers are wired further down, once
    // the device run-closures they depend on exist. A device playlist linked to
    // a library playlist (same safe name) is renamed via the library; a
    // device-only playlist is acted on in place.
    let dev_pl_rename = Button::with_label("Rename");
    let dev_pl_duplicate = Button::with_label("Duplicate");
    let dev_pl_delete = Button::with_label("Delete");
    for b in [&dev_pl_rename, &dev_pl_duplicate, &dev_pl_delete] {
        b.add_css_class("pl-btn");
    }
    dev_pl_delete.add_css_class("destructive");
    let dev_pl_actions = GtkBox::new(Orientation::Horizontal, 6);
    dev_pl_actions.append(&dev_pl_rename);
    dev_pl_actions.append(&dev_pl_duplicate);
    dev_pl_actions.append(&dev_pl_delete);
    dev_pl_actions.set_visible(false);
    dev_detail.append(&dev_pl_actions);
    // The device playlist file the active chip points at (None = "All files").
    let selected_dev_playlist: Rc<RefCell<Option<std::path::PathBuf>>> =
        Rc::new(RefCell::new(None));

    // Delete/Remove button for the device track view, created early so the
    // playlist filter can flip its label. It is placed into the bottom action
    // row further down. Label is "Delete" in the all-files view (delete off the
    // device + drop from every playlist) and "Remove" in a playlist view (drop
    // from that one playlist, keep the file). Disabled until files are selected.
    let dev_file_remove = Button::with_label("Delete");
    dev_file_remove.add_css_class("pl-btn");
    dev_file_remove.add_css_class("destructive");
    dev_file_remove.set_sensitive(false);

    // Live copy status ("Copying x/y · filename"). Empty when idle, so it acts
    // as the flexible spacer in the bottom action row (no dedicated status bar,
    // which left an empty strip at the bottom of the view).
    let dev_hint = Label::builder()
        .label("")
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    dev_hint.add_css_class("status-label");
    // Kept for the selection handler's unsupported-fs note; not shown directly
    // (the title-section badge now carries that), so it stays unparented.
    let dev_warn = Label::builder()
        .halign(Align::End)
        .xalign(1.0)
        .visible(false)
        .build();
    dev_warn.add_css_class("broken");

    // Track view mirroring the files-view columns, populated from device tags.
    // `dev_store` is the *displayed* model: in the all-files view it holds every
    // device file; in a playlist view it holds that playlist's entries in order,
    // duplicates included (a playlist may reference the same file more than
    // once). `dev_all_tracks` caches the full device file list so switching
    // views doesn't re-scan the device.
    let dev_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let dev_all_tracks: Rc<RefCell<Vec<crate::media_library::LibTrack>>> =
        Rc::new(RefCell::new(Vec::new()));
    // Device file path → the library file it was copied from (its sync pair), for
    // the device view's "Synced from" column so the user can see exactly which
    // computer file each device file is kept in step with. Rebuilt per device by
    // reload_device_store; read live by the column factory.
    let dev_pair_map: Rc<RefCell<std::collections::HashMap<String, String>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    // Per-view search over whatever the store currently shows (all files or
    // one playlist): store → filter → sort → selection, so every fill site
    // stays filter-oblivious and copy/delete still act on the selection.
    let dev_search_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let dev_filter = gtk4::CustomFilter::new({
        let q = dev_search_query.clone();
        move |obj| {
            let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                return true;
            };
            lib_track_matches_query(&boxed.borrow::<crate::media_library::LibTrack>(), &q.borrow())
        }
    });
    let dev_filter_model =
        gtk4::FilterListModel::new(Some(dev_store.clone()), Some(dev_filter.clone()));
    // Search filters just this device view's rows (all-files or the shown
    // playlist). Created here so reload_device_store can clear it when a
    // different device opens; packed above the track table below.
    let (dev_search_row, dev_search_entry) =
        make_view_search_row("Search this device — artist, title, album…");
    {
        // 150 ms debounce: the filter re-scans every row's text fields, so
        // re-running it per keystroke stutters on large device libraries.
        let q = dev_search_query.clone();
        let filter = dev_filter.clone();
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        dev_search_entry.connect_changed(move |e| {
            let text = e.text().to_lowercase();
            if let Some(src) = pending.borrow_mut().take() {
                src.remove();
            }
            let q = q.clone();
            let filter = filter.clone();
            let pending_inner = pending.clone();
            let src = glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                *q.borrow_mut() = text.clone();
                filter.changed(gtk4::FilterChange::Different);
                pending_inner.borrow_mut().take();
                glib::ControlFlow::Break
            });
            *pending.borrow_mut() = Some(src);
        });
    }
    let dev_sort_model = SortListModel::new(Some(dev_filter_model), None::<gtk4::Sorter>);
    let dev_selection = MultiSelection::new(Some(dev_sort_model.clone()));
    let dev_col_view = ColumnView::new(Some(dev_selection.clone()));
    dev_col_view.add_css_class("ml-col-view");
    dev_col_view.set_hexpand(true);
    dev_col_view.set_vexpand(true);

    // Playlist-order column (front): shown only while a playlist filter is
    // active, then made the default sort — like the editor's position column.
    let dev_pos_col = {
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::End)
                .xalign(1.0)
                .margin_start(6)
                .margin_end(6)
                .css_classes(["pl-duration"])
                .build();
            li.set_child(Some(&lbl));
        });
        // The playlist view holds entries in order (no sort), so the row's
        // position in the model is its 1-based playlist position. Each duplicate
        // entry is its own row and gets its own number.
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                return;
            };
            lbl.set_text(&(li.position() + 1).to_string());
        });
        let col = ColumnViewColumn::new(Some("#"), Some(factory));
        col.set_fixed_width(48);
        col.set_visible(false);
        dev_col_view.append_column(&col);
        col
    };

    // "Synced from" column (device view only): the library file each device file
    // was copied from. Lets the user confirm at a glance which computer file a
    // sync keeps in step, instead of guessing among same-named files. Reads the
    // live per-device pair map keyed by on-device path.
    {
        let pair_map = dev_pair_map.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(6)
                .margin_end(6)
                .ellipsize(gtk4::pango::EllipsizeMode::Middle)
                .css_classes(["status-label"])
                .build();
            li.set_child(Some(&lbl));
        });
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                return;
            };
            let Some(item) = li.item() else { return };
            let Some(boxed) = item.downcast_ref::<glib::BoxedAnyObject>() else {
                return;
            };
            let path = boxed.borrow::<crate::media_library::LibTrack>().path.clone();
            match pair_map.borrow().get(&path) {
                Some(libp) => {
                    let base = std::path::Path::new(libp)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(libp);
                    lbl.set_text(&gtk_safe(base));
                    lbl.set_tooltip_text(Some(&gtk_safe(libp)));
                }
                None => {
                    lbl.set_text("—");
                    lbl.set_tooltip_text(Some("Not synced from this computer"));
                }
            }
        });
        let col = ColumnViewColumn::new(Some("Synced from"), Some(factory));
        col.set_fixed_width(220);
        col.set_resizable(true);
        dev_col_view.append_column(&col);
    }

    let mut dev_named_cols: Vec<(String, ColumnViewColumn)> = Vec::new();
    // Buttons that already have a click handler wired (artwork "View"), so the
    // device factory connects each button instance only once.
    let dev_connected_artwork: Rc<RefCell<std::collections::HashSet<glib::Object>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));
    {
        // Columns that are library bookkeeping, not ID3 tags — irrelevant for a
        // device, so never shown here even if visible in the files view.
        const DEVICE_HIDDEN_COLS: &[&str] = &["play_count", "last_played", "last_scanned"];
        let visible_ids: Vec<String> =
            state.borrow().config.media_library.visible_columns.clone();
        let widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();
        let order = state.borrow().config.media_library.ml_file_col_order.clone();
        // Build columns in the saved order (unknown/leftover ids appended).
        let ordered: Vec<&MlColumnDef> = {
            let mut v: Vec<&MlColumnDef> = Vec::new();
            for id in &order {
                if let Some(c) = ALL_COLUMNS.iter().find(|c| &c.id == id) {
                    v.push(c);
                }
            }
            for c in ALL_COLUMNS.iter() {
                if !order.iter().any(|id| id == c.id) {
                    v.push(c);
                }
            }
            v
        };
        for c in ordered {
            if DEVICE_HIDDEN_COLS.contains(&c.id) {
                continue;
            }
            let id_str = c.id.to_string();
            let is_art = c.id == "artwork_path";
            let factory = SignalListItemFactory::new();
            factory.connect_setup(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() {
                    return;
                }
                // Artwork column shows a "View" button (mirrors the files view),
                // every other column a plain label.
                let child: gtk4::Widget = if is_art {
                    let btn = Button::with_label("View");
                    btn.add_css_class("link");
                    btn.set_halign(Align::Start);
                    btn.set_margin_start(4);
                    btn.set_margin_end(4);
                    btn.set_visible(false);
                    btn.upcast::<gtk4::Widget>()
                } else {
                    Label::builder()
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(6)
                        .margin_end(6)
                        .ellipsize(gtk4::pango::EllipsizeMode::End)
                        .css_classes(["ml-col-label"])
                        .build()
                        .upcast::<gtk4::Widget>()
                };
                li.set_child(Some(&child));
            });
            let bind_id = id_str.clone();
            let bind_connected = dev_connected_artwork.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };
                let t = boxed.borrow::<crate::media_library::LibTrack>();
                if is_art {
                    let Some(btn) = li.child().and_then(|c| c.downcast::<Button>().ok()) else {
                        return;
                    };
                    if let Some(ref art_path) = t.artwork_path {
                        btn.set_visible(true);
                        let btn_obj = btn.clone().upcast::<glib::Object>();
                        if !bind_connected.borrow().contains(&btn_obj) {
                            bind_connected.borrow_mut().insert(btn_obj);
                            let art = art_path.clone();
                            btn.connect_clicked(move |_| open_image_viewer(&art));
                        }
                    } else {
                        btn.set_visible(false);
                    }
                    return;
                }
                let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                    return;
                };
                lbl.set_text(&gtk_safe(&ml_cell_text(&t, &bind_id)));
            });
            let col = ColumnViewColumn::new(Some(c.header), Some(factory));
            col.set_resizable(true);
            if c.expand {
                col.set_expand(true);
            }
            col.set_visible(visible_ids.contains(&id_str));
            if let Some(&w) = widths.get(&id_str) {
                if w > 0 {
                    col.set_fixed_width(w);
                }
            }
            let sort_id = id_str.clone();
            let sorter = CustomSorter::new(move |a, b| {
                let ka = a
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id))
                    .unwrap_or_default();
                let kb = b
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id))
                    .unwrap_or_default();
                ka.cmp(&kb).into()
            });
            col.set_sorter(Some(&sorter));
            dev_named_cols.push((id_str.clone(), col.clone()));
            dev_col_view.append_column(&col);
        }
        // Header clicks drive the sort model.
        dev_sort_model.set_sorter(dev_col_view.sorter().as_ref());
    }
    let dev_named_cols = Rc::new(dev_named_cols);

    // Backend object id of the currently shown device (Eject/Sync target).
    let selected_dev_backend: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Reload a device's tracks into the column store (tags re-read on a worker
    // thread). Used on device select and after a sync so changed values show
    // immediately.
    let reload_device_store: Rc<dyn Fn(crate::devices::Device)> = {
        let store = dev_store.clone();
        let all_tracks = dev_all_tracks.clone();
        let hint = dev_hint.clone();
        let counts_lbl = dev_counts.clone();
        let state = state.clone();
        let counts_cache = device_counts.clone();
        let sel_backend = selected_dev_backend.clone();
        let pair_map = dev_pair_map.clone();
        let search = dev_search_entry.clone();
        Rc::new(move |dev: crate::devices::Device| {
            counts_lbl.set_text("Reading device…");
            hint.set_text(""); // clear any stale copy status
            // A previous device's search query must not filter this one.
            search.set_text("");
            store.remove_all();
            pair_map.borrow_mut().clear(); // drop the previous device's pairings
            // Device contents may have changed (copy/send/sync) — drop the
            // cached overview counts so the cards recompute next time shown, and
            // the cached MTP metadata so the next poll refreshes free space once.
            counts_cache.borrow_mut().remove(&dev.backend_id);
            invalidate_mtp_meta(&dev.backend_id);
            let store2 = store.clone();
            let all_tracks2 = all_tracks.clone();
            let counts_lbl2 = counts_lbl.clone();
            let state2 = state.clone();
            let pair_map2 = pair_map.clone();
            let mount = dev.mount_path.clone();
            // Guard against a slow scan landing after the user switched devices:
            // each scan is tagged with its device, and results are applied only
            // if that device is still the one shown (else a stale scan would
            // overwrite the current device's list — the "275 vs 18" flip).
            let backend = dev.backend_id.clone();
            let sel_backend2 = sel_backend.clone();
            // Non-writing device id (don't drop a marker just to browse).
            let device_id = if dev.id.is_empty() {
                crate::devices::marker::read_marker(&dev.mount_path).unwrap_or_default()
            } else {
                dev.id.clone()
            };
            // Backend-specific IO (POSIX today; gio/MTP later) — move it onto the
            // worker thread for the blocking scan.
            let io = crate::devices::io::for_device(&dev);
            glib::spawn_future_local(async move {
                let (mut tracks, pl_count) = gio::spawn_blocking(move || {
                    if device_io_shutting_down() {
                        return (Vec::new(), 0);
                    }
                    let tracks = io
                        .list_audio_files()
                        .iter()
                        .map(|p| crate::devices::browse::read_device_track(p))
                        .collect::<Vec<crate::media_library::LibTrack>>();
                    let pl_count = io.playlist_files().len();
                    (tracks, pl_count)
                })
                .await
                .unwrap_or_default();

                // Stale-scan guard: bail if the user has since switched devices.
                if sel_backend2.borrow().as_deref() != Some(backend.as_str()) {
                    return;
                }

                // Prefill calculated values (duration, bitrate, channels) from
                // the paired library track for files copied from this computer,
                // so device rows match the files view even when the on-device
                // tags don't carry that info.
                if !device_id.is_empty() {
                    let s = state2.borrow();
                    if let Some(lib) = s.media_lib.as_ref() {
                        if let Ok(pairs) = lib.sync_pairs_for_device(&device_id) {
                            // Populate the "Synced from" map: on-device path → the
                            // library file it was copied from.
                            let mut pm = std::collections::HashMap::new();
                            for p in &pairs {
                                pm.insert(
                                    mount.join(&p.device_relpath).to_string_lossy().into_owned(),
                                    p.library_path.clone(),
                                );
                            }
                            *pair_map2.borrow_mut() = pm;
                            for t in tracks.iter_mut() {
                                let tp = std::path::Path::new(&t.path);
                                let Some(pair) = pairs.iter().find(|p| {
                                    mount.join(&p.device_relpath) == tp
                                }) else {
                                    continue;
                                };
                                let Ok(libt) = lib.track_by_path(&pair.library_path) else {
                                    continue;
                                };
                                if t.length_secs.is_none() {
                                    t.length_secs = libt.length_secs;
                                }
                                if t.bitrate.is_none() {
                                    t.bitrate = libt.bitrate;
                                }
                                if t.channels.is_none() {
                                    t.channels = libt.channels;
                                }
                                t.sort_keys = crate::media_library::SortKeys::from_track(t);
                            }
                        }
                    }
                }

                // Cache the full file list (for playlist views) and show all
                // files. A playlist chip selection re-derives its rows from this
                // cache without re-scanning.
                *all_tracks2.borrow_mut() = tracks.clone();
                store2.remove_all();
                for t in &tracks {
                    store2.append(&glib::BoxedAnyObject::new(t.clone()));
                }
                counts_lbl2.set_text(&format!(
                    "{} playlist{} - {} audio file{}",
                    pl_count,
                    if pl_count == 1 { "" } else { "s" },
                    tracks.len(),
                    if tracks.len() == 1 { "" } else { "s" }
                ));
            });
        })
    };

    // Rebuild the device playlist-filter rows ("All files" + each device
    // .m3u/.m3u8) for a mount. Shared by the device-select handler and the
    // playlist-send completion so a just-copied playlist appears immediately.
    // Apply a playlist filter to the device track view by name ("all" clears
    // it; otherwise the device .m3u/.m3u8 path). Shared by every filter chip.
    let apply_pl_filter: Rc<dyn Fn(&str)> = {
        let store = dev_store.clone();
        let all_tracks = dev_all_tracks.clone();
        let sort_model = dev_sort_model.clone();
        let pos_col = dev_pos_col.clone();
        let col_view = dev_col_view.clone();
        let sel_pl = selected_dev_playlist.clone();
        let actions = dev_pl_actions.clone();
        let remove_btn = dev_file_remove.clone();
        Rc::new(move |name: &str| {
            store.remove_all();
            if name == "all" || name.is_empty() {
                *sel_pl.borrow_mut() = None;
                actions.set_visible(false);
                remove_btn.set_label("Delete");
                pos_col.set_visible(false);
                for t in all_tracks.borrow().iter() {
                    store.append(&glib::BoxedAnyObject::new(t.clone()));
                }
                // Restore column-driven sorting for the all-files view.
                sort_model.set_sorter(col_view.sorter().as_ref());
            } else {
                *sel_pl.borrow_mut() = Some(std::path::PathBuf::from(name));
                actions.set_visible(true);
                remove_btn.set_label("Remove");
                pos_col.set_visible(true);
                // Fixed playlist order: index the device files by filename, then
                // emit one row per playlist entry — duplicates included, in order.
                let order =
                    crate::devices::browse::playlist_entry_order(std::path::Path::new(name));
                let by_name: std::collections::HashMap<String, crate::media_library::LibTrack> =
                    all_tracks
                        .borrow()
                        .iter()
                        .map(|t| (t.filename.clone(), t.clone()))
                        .collect();
                // No sort in the playlist view, so insertion order = playlist order.
                sort_model.set_sorter(None::<&gtk4::Sorter>);
                for fname in order {
                    if let Some(t) = by_name.get(&fname) {
                        store.append(&glib::BoxedAnyObject::new(t.clone()));
                    }
                }
            }
        })
    };

    let reload_dev_playlists: Rc<dyn Fn(crate::devices::Device)> = {
        let chips = dev_pl_chips.clone();
        let apply = apply_pl_filter.clone();
        // Generation token: bumped on every call so an in-flight playlist walk
        // (slow over MTP) that finishes after the user switched devices is
        // discarded instead of appending stale chips.
        let generation = Rc::new(Cell::new(0u64));
        Rc::new(move |dev: crate::devices::Device| {
            let gen_id = generation.get().wrapping_add(1);
            generation.set(gen_id);
            while let Some(c) = chips.first_child() {
                chips.remove(&c);
            }
            // "All files" chip + cleared filter are shown immediately so the
            // detail page paints without waiting on the device walk.
            let all = gtk4::ToggleButton::with_label("All files");
            all.add_css_class("device-chip");
            {
                let apply2 = apply.clone();
                all.connect_toggled(move |btn| {
                    if btn.is_active() {
                        apply2("all");
                    }
                });
            }
            chips.insert(&all, -1);
            all.set_active(true);
            apply("all");

            // Walk the device for playlist files off the main thread (a recursive
            // tree walk over a gvfs/MTP FUSE mount would otherwise freeze the UI),
            // then append a chip per playlist if this is still the shown device.
            let chips2 = chips.clone();
            let all2 = all.clone();
            let apply3 = apply.clone();
            let generation2 = generation.clone();
            let io = crate::devices::io::for_device(&dev);
            glib::spawn_future_local(async move {
                let pls = gio::spawn_blocking(move || io.playlist_files())
                    .await
                    .unwrap_or_default();
                if generation2.get() != gen_id {
                    return; // device switched / chips rebuilt since this walk began
                }
                for pl in pls {
                    let nm = pl
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let path_name = pl.to_string_lossy().into_owned();
                    let chip = gtk4::ToggleButton::with_label(&gtk_safe(&nm));
                    chip.add_css_class("device-chip");
                    chip.set_group(Some(&all2));
                    let apply4 = apply3.clone();
                    chip.connect_toggled(move |btn| {
                        if btn.is_active() {
                            apply4(&path_name);
                        }
                    });
                    chips2.insert(&chip, -1);
                }
            });
        })
    };

    // Send a whole playlist (files + .m3u8) to a device, copying on a worker
    // thread with live progress shown on the device's sidebar row and detail.
    let send_playlist_run: Rc<dyn Fn(crate::devices::Device, i64, String)> = {
        let state = state.clone();
        let sidebar = sidebar.clone();
        let hint = dev_hint.clone();
        let progress = dev_progress.clone();
        let reload = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        let sel_backend = selected_dev_backend.clone();
        let update_card = update_card_progress.clone();
        let eject = dev_eject.clone();
        let win_wk = win.downgrade();
        Rc::new(move |dev: crate::devices::Device, playlist_id: i64, name: String| {
            let plan = match prepare_playlist_send(&state, &dev, playlist_id, &name) {
                Ok(p) => p,
                Err(e) => {
                    show_alert_parented(win_wk.upgrade().as_ref(), &e);
                    return;
                }
            };
            let backend = dev.backend_id.clone();
            let dname = if dev.label.is_empty() {
                "device".to_string()
            } else {
                dev.label.clone()
            };
            let row_base = format!(
                "{}{}",
                device_glyph_prefix(dev.read_only, &dev.fs_type),
                if dev.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    dev.label.clone()
                }
            );
            let set_row_label = {
                let sidebar = sidebar.clone();
                let row_name = format!("dev:{backend}");
                move |text: &str| {
                    if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                        if let Some(bx) = row.child().and_then(|c| c.downcast::<GtkBox>().ok()) {
                            if let Some(lbl) =
                                bx.first_child().and_then(|c| c.downcast::<Label>().ok())
                            {
                                lbl.set_text(text);
                            }
                        }
                    }
                }
            };

            let total = plan.srcs.len();
            let srcs = plan.srcs.clone();
            let device_id = plan.device_id.clone();
            let m3u_path = plan.m3u_path.clone();
            let mount = dev.mount_path.clone();
            let dev_for_reload = dev.clone();
            let state2 = state.clone();
            let hint2 = hint.clone();
            let progress2 = progress.clone();
            let reload2 = reload.clone();
            let reload_pls2 = reload_pls.clone();
            let sel2 = sel_backend.clone();
            let update_card2 = update_card.clone();
            let eject2 = eject.clone();
            let dev_ejectable = dev.ejectable;
            let win2 = win_wk.clone();
            glib::spawn_future_local(async move {
                // (device relpath, library source path) pairs so the written
                // .m3u8 carries #EXTINF metadata from the library.
                let mut entries: Vec<(String, String)> = Vec::new();
                let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                let on_dev = sel2.borrow().as_deref() == Some(backend.as_str());
                if on_dev {
                    eject2.set_sensitive(false); // no eject mid-copy
                }
                for (i, src) in srcs.iter().enumerate() {
                    let prog = format!("{}/{}", i + 1, total);
                    set_row_label(&format!("{row_base} — {prog}"));
                    update_card2(&backend, Some((i + 1, total)));
                    if sel2.borrow().as_deref() == Some(backend.as_str()) {
                        let fname = src.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        hint2.set_text(&format!("Copying {prog} · {fname}"));
                        progress2.set_visible(true);
                        progress2.set_text(Some(&format!("{prog} · {fname}")));
                        progress2.set_fraction((i + 1) as f64 / total.max(1) as f64);
                    }
                    // DB lookup on the main thread; FS plan + copy on the worker
                    // so a slow MTP FUSE op never blocks the UI.
                    let recorded = device_recorded_relpath(&state2, &device_id, src);
                    let s = src.clone();
                    let m = mount.clone();
                    let dc = dev_for_reload.clone();
                    let joined = gio::spawn_blocking(move || -> Result<(std::path::PathBuf, bool), ()> {
                        let (rel, present) = device_plan_fs(&m, &s, recorded);
                        if present {
                            return Ok((rel, false)); // already there → skipped
                        }
                        match crate::devices::io::for_device(&dc).copy_to_device(&s, &rel) {
                            Ok(_) => Ok((rel, true)),
                            Err(_) => Err(()),
                        }
                    })
                    .await;
                    match joined {
                        Ok(Ok((rel, copied_now))) => {
                            if copied_now {
                                copied += 1;
                            } else {
                                skipped += 1;
                            }
                            device_record_pair(&state2, &device_id, src, &rel);
                            entries.push((
                                rel.to_string_lossy().replace('\\', "/"),
                                src.to_string_lossy().into_owned(),
                            ));
                        }
                        _ => failed += 1,
                    }
                }
                // Write the playlist file, carrying #EXTINF metadata from the
                // library for each entry.
                let body = state2
                    .borrow()
                    .media_lib
                    .as_ref()
                    .map(|l| l.build_device_m3u(&entries))
                    .unwrap_or_else(|| {
                        format!(
                            "#EXTM3U\n{}\n",
                            entries.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>().join("\n")
                        )
                    });
                let mp = m3u_path.clone();
                let _ = gio::spawn_blocking(move || std::fs::write(&mp, body)).await;
                // Record the playlist sync baseline so a later edit on either
                // side syncs two-way instead of the library silently winning.
                if !device_id.is_empty() {
                    let dev_fname = m3u_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let basenames: Vec<String> = entries
                        .iter()
                        .map(|(e, _)| e.rsplit(['/', '\\']).next().unwrap_or(e).to_string())
                        .collect();
                    if let Some(lib) = state2.borrow().media_lib.as_ref() {
                        let _ = lib.upsert_playlist_baseline(&crate::media_library::PlaylistBaseline {
                            device_id: device_id.clone(),
                            library_playlist_id: playlist_id,
                            device_filename: dev_fname,
                            entries_hash: crate::devices::sync::entries_hash(&basenames),
                            last_sync_at: Some(crate::timeutil::format_current_timestamp()),
                        });
                    }
                }
                set_row_label(&row_base);
                progress2.set_visible(false);
                update_card2(&backend, None);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(dev_ejectable);
                }
                reload2(dev_for_reload.clone());
                // Refresh the playlist filter so the just-written .m3u8 shows
                // immediately, without needing to reselect the device.
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    reload_pls2(dev_for_reload.clone());
                }
                show_alert_parented(
                    win2.upgrade().as_ref(),
                    &format!(
                        "Sent to {dname}: {copied} copied, {skipped} skipped, {failed} failed, \
                         plus the playlist."
                    ),
                );
            });
        })
    };
    *send_playlist_holder.borrow_mut() = Some(send_playlist_run.clone());

    // ── Device playlist management actions (New / Rename / Duplicate / Delete) ─
    // Resolve the Device backing the currently-selected device row.
    let current_device_for_actions = {
        let current_devices = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        move || -> Option<crate::devices::Device> {
            let backend = sel_backend.borrow().clone()?;
            current_devices
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned()
        }
    };

    // Rename: rename the device .m3u/.m3u8; if it is linked to a library
    // playlist, rename that too so the link (safe-name match) is preserved.
    {
        let state = state.clone();
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let reload_store = reload_device_store.clone();
        let win_wk = win.downgrade();
        dev_pl_rename.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            let current_stem = pl_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = pl_path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "m3u8".to_string());

            let dialog = gtk4::Window::builder()
                .title("Rename Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("New name:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text(&gtk_safe(&current_stem));
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Rename");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let state2 = state.clone();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let reload_store2 = reload_store.clone();
            let win_wk2 = win_wk.clone();
            let ext2 = ext.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let new_path = pl_path2
                    .parent()
                    .map(|p| p.join(format!("{safe}.{ext2}")))
                    .unwrap_or_else(|| pl_path2.clone());
                if new_path != pl_path2 {
                    if let Err(err) = std::fs::rename(&pl_path2, &new_path) {
                        show_alert_parented(
                            win_wk2.upgrade().as_ref(),
                            &format!("Couldn't rename the playlist file: {err}"),
                        );
                        return;
                    }
                }
                // Keep a linked library playlist's name in step.
                if let Some((id, _)) = linked_library_playlist(&state2, &pl_path2) {
                    if let Some(lib) = state2.borrow().media_lib.as_ref() {
                        let _ = lib.rename_playlist(id, raw.trim());
                    }
                }
                reload_pls2(dev2.clone());
                reload_store2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // Duplicate: copy the selected device .m3u/.m3u8 to a new name on the same
    // device. The copy is a device-only playlist (referencing the same files).
    {
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let win_wk = win.downgrade();
        dev_pl_duplicate.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            let stem = pl_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = pl_path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "m3u8".to_string());

            let dialog = gtk4::Window::builder()
                .title("Duplicate Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("Name for the copy:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text(&gtk_safe(&format!("{stem} copy")));
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Duplicate");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let win_wk2 = win_wk.clone();
            let ext2 = ext.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let dest = dev2.mount_path.join(format!("{safe}.{ext2}"));
                if dest == pl_path2 {
                    return;
                }
                if dest.exists() {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        "A playlist with that name already exists on the device.",
                    );
                    return;
                }
                if let Err(err) = std::fs::copy(&pl_path2, &dest) {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't duplicate the playlist: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // New: create an empty device-only playlist (a bare .m3u8) on the device.
    // The user then adds device files to it. Always available (not tied to a
    // selected playlist).
    {
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let win_wk = win.downgrade();
        dev_pl_new.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            if device_fs_unsupported(&dev.fs_type) {
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    "This filesystem is unsupported — can't create a playlist on it yet.",
                );
                return;
            }
            let dialog = gtk4::Window::builder()
                .title("New Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("Playlist name:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text("New Playlist");
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Create");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let win_wk2 = win_wk.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let dest = dev2.mount_path.join(format!("{safe}.m3u8"));
                if dest.exists() {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        "A playlist with that name already exists on the device.",
                    );
                    return;
                }
                if let Err(err) = std::fs::write(&dest, "#EXTM3U\n") {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't create the playlist: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // Delete: remove the .m3u/.m3u8 from the device only. The audio files are
    // kept (they may belong to other playlists), and no library playlist or
    // on-disk music file is touched (Deletion Rule).
    {
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let reload_store = reload_device_store.clone();
        let win_wk = win.downgrade();
        dev_pl_delete.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            let name = pl_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dialog = gtk4::AlertDialog::builder()
                .message(format!("Remove \"{name}\" from the device?"))
                .detail("Only the playlist file is removed. The songs stay on the device.")
                .buttons(vec!["Cancel".to_string(), "Remove".to_string()])
                .cancel_button(0)
                .default_button(1)
                .modal(true)
                .build();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let reload_store2 = reload_store.clone();
            let win_wk2 = win_wk.clone();
            dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
                if res != Ok(1) {
                    return;
                }
                if let Err(err) = crate::devices::io::for_device(&dev2).delete(&pl_path2) {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't remove the playlist file: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                reload_store2(dev2.clone());
            });
        });
    }

    // Copy loose files (drag-drop from a view) onto a device on a worker
    // thread, with the same sidebar "(x/y)" label and detail progress bar the
    // playlist send uses. No .m3u8 is written — these are just files.
    let copy_files_run: Rc<dyn Fn(crate::devices::Device, Vec<std::path::PathBuf>)> = {
        let state = state.clone();
        let sidebar = sidebar.clone();
        let hint = dev_hint.clone();
        let progress = dev_progress.clone();
        let reload = reload_device_store.clone();
        let sel_backend = selected_dev_backend.clone();
        let update_card = update_card_progress.clone();
        let eject = dev_eject.clone();
        let win_wk = win.downgrade();
        Rc::new(move |dev: crate::devices::Device, srcs: Vec<std::path::PathBuf>| {
            if dev.read_only {
                let n = if dev.label.is_empty() { "This device" } else { &dev.label };
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    &format!("{n} is read-only — can't copy files to it."),
                );
                return;
            }
            if device_fs_unsupported(&dev.fs_type) {
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    &format!(
                        "{} is an unsupported filesystem — can't write to this device yet.",
                        dev.fs_type
                    ),
                );
                return;
            }
            let device_id = device_sync_id(&dev);
            let mount = dev.mount_path.clone();
            let srcs: Vec<std::path::PathBuf> =
                srcs.into_iter().filter(|p| p.exists()).collect();
            if srcs.is_empty() {
                return;
            }
            // Free-space guard — only when capacity is known (skips a pass of
            // slow per-file device checks on devices that can't report it, MTP).
            if dev.free_bytes > 0 {
                let mut need = 0u64;
                for src in &srcs {
                    if !device_plan_one(&state, &mount, &device_id, src).1 {
                        need += std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
                    }
                }
                if need > dev.free_bytes {
                    show_alert_parented(
                        win_wk.upgrade().as_ref(),
                        &format!(
                            "Not enough space on the device: need {:.1} GB, {:.1} GB free.",
                            need as f64 / 1e9,
                            dev.free_bytes as f64 / 1e9
                        ),
                    );
                    return;
                }
            }

            let backend = dev.backend_id.clone();
            let dname = if dev.label.is_empty() {
                "device".to_string()
            } else {
                dev.label.clone()
            };
            let row_base = format!(
                "{}{}",
                device_glyph_prefix(dev.read_only, &dev.fs_type),
                if dev.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    dev.label.clone()
                }
            );
            let set_row_label = {
                let sidebar = sidebar.clone();
                let row_name = format!("dev:{backend}");
                move |text: &str| {
                    if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                        if let Some(bx) = row.child().and_then(|c| c.downcast::<GtkBox>().ok()) {
                            if let Some(lbl) =
                                bx.first_child().and_then(|c| c.downcast::<Label>().ok())
                            {
                                lbl.set_text(text);
                            }
                        }
                    }
                }
            };

            let total = srcs.len();
            let dev_for_reload = dev.clone();
            let state2 = state.clone();
            let hint2 = hint.clone();
            let progress2 = progress.clone();
            let reload2 = reload.clone();
            let sel2 = sel_backend.clone();
            let update_card2 = update_card.clone();
            let eject2 = eject.clone();
            let dev_ejectable = dev.ejectable;
            let win2 = win_wk.clone();
            glib::spawn_future_local(async move {
                let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(false); // no eject mid-copy
                }
                for (i, src) in srcs.iter().enumerate() {
                    let prog = format!("{}/{}", i + 1, total);
                    set_row_label(&format!("{row_base} — {prog}"));
                    update_card2(&backend, Some((i + 1, total)));
                    if sel2.borrow().as_deref() == Some(backend.as_str()) {
                        let fname = src.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        hint2.set_text(&format!("Copying {prog} · {fname}"));
                        progress2.set_visible(true);
                        progress2.set_text(Some(&format!("{prog} · {fname}")));
                        progress2.set_fraction((i + 1) as f64 / total.max(1) as f64);
                    }
                    // DB lookup on the main thread; the FS plan + copy (slow over
                    // MTP) run on the worker so the UI never blocks on FUSE.
                    let recorded = device_recorded_relpath(&state2, &device_id, src);
                    let s = src.clone();
                    let m = mount.clone();
                    let dc = dev_for_reload.clone();
                    let joined = gio::spawn_blocking(move || -> Result<(std::path::PathBuf, bool), ()> {
                        let (rel, present) = device_plan_fs(&m, &s, recorded);
                        if present {
                            return Ok((rel, false)); // already there → skipped
                        }
                        match crate::devices::io::for_device(&dc).copy_to_device(&s, &rel) {
                            Ok(_) => Ok((rel, true)),
                            Err(_) => Err(()),
                        }
                    })
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
                set_row_label(&row_base);
                progress2.set_visible(false);
                update_card2(&backend, None);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(dev_ejectable);
                }
                reload2(dev_for_reload.clone());
                show_alert_parented(
                    win2.upgrade().as_ref(),
                    &format!("Copied {copied}, skipped {skipped}, failed {failed} to {dname}."),
                );
            });
        })
    };
    *copy_files_holder.borrow_mut() = Some(copy_files_run.clone());

    dev_detail.append(&dev_search_row);

    let dev_tracks_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&dev_col_view)
        .build();
    dev_detail.append(&dev_tracks_scroll);

    // ── Bottom action row for the device track view ──────────────────────────
    // Left: add files to the device + delete/remove the selected files. Right
    // (aligned like the rest of the Media Library): play / enqueue the selection.
    let dev_file_add = Button::with_label("Add Files…");
    let dev_file_play = Button::with_label("Play");
    let dev_file_enqueue = Button::with_label("Enqueue");
    for b in [&dev_file_add, &dev_file_play, &dev_file_enqueue] {
        b.add_css_class("pl-btn");
    }
    let dev_file_actions = GtkBox::new(Orientation::Horizontal, 6);
    dev_file_actions.append(&dev_file_add);
    dev_file_actions.append(&dev_file_remove);
    // dev_hint is the flexible middle element: empty (a spacer) when idle, live
    // copy status while files copy.
    dev_file_actions.append(&dev_hint);
    dev_file_actions.append(&dev_file_play);
    dev_file_actions.append(&dev_file_enqueue);
    dev_detail.append(&dev_file_actions);

    // Quiet status line (G3) — Send to Disc Drive reports here instead of
    // a success modal; mirrors the files view's `files_status`.
    let dev_status = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(6)
        .margin_end(6)
        .margin_bottom(2)
        .build();
    dev_status.add_css_class("status-label");
    dev_detail.append(&dev_status);

    // Collect the currently-selected device track rows (full LibTrack, so
    // already-known metadata like duration carries into the active playlist).
    let selected_device_tracks: Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>> = {
        let sel = dev_selection.clone();
        let model = dev_sort_model.clone();
        Rc::new(move || {
            let mut out = Vec::new();
            for i in 0..model.n_items() {
                if !sel.is_selected(i) {
                    continue;
                }
                if let Some(t) = model.item(i).and_downcast::<glib::BoxedAnyObject>() {
                    out.push(t.borrow::<crate::media_library::LibTrack>().clone());
                }
            }
            out
        })
    };

    // Enable the Delete/Remove button only while one or more files are selected.
    {
        let remove_btn = dev_file_remove.clone();
        let sel_tracks = selected_device_tracks.clone();
        dev_selection.connect_selection_changed(move |_, _, _| {
            remove_btn.set_sensitive(!sel_tracks().is_empty());
        });
    }

    // Add Files…: pick audio files and copy them to the device Music folder.
    {
        let get_dev = current_device_for_actions.clone();
        let copy = copy_files_run.clone();
        let win_wk = win.downgrade();
        dev_file_add.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let dialog = gtk4::FileDialog::builder().title("Add Files to Device").build();
            let copy2 = copy.clone();
            let dev2 = dev.clone();
            dialog.open_multiple(
                win_wk.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    let Ok(files) = res else { return };
                    let paths: Vec<std::path::PathBuf> = (0..files.n_items())
                        .filter_map(|i| files.item(i).and_downcast::<gio::File>())
                        .filter_map(|f| f.path())
                        .collect();
                    if !paths.is_empty() {
                        copy2(dev2.clone(), paths);
                    }
                },
            );
        });
    }

    // Play: replace the active playlist with the selected device files and play
    // from the first one (so "Play" plays just the selection, not whatever was
    // queued before). Built from the device LibTrack so known duration/tags
    // show immediately rather than "-:--" until played.
    {
        let sel_tracks = selected_device_tracks.clone();
        let state = state.clone();
        let rebuild = rebuild_playlist.clone();
        dev_file_play.connect_clicked(move |_| {
            let tracks = sel_tracks();
            if tracks.is_empty() {
                return;
            }
            let _ = state.borrow_mut().player.stop();
            state.borrow_mut().playlist.clear();
            for lt in &tracks {
                state.borrow_mut().playlist.add(crate::model::Track::from(lt));
            }
            if !state.borrow().playlist.is_empty() {
                state.borrow_mut().play_current();
            }
            rebuild();
        });
    }

    // Enqueue: append the selected device files to the active playlist, carrying
    // the device row's known metadata (duration etc.) so it shows immediately.
    {
        let sel_tracks = selected_device_tracks.clone();
        let state = state.clone();
        let rebuild = rebuild_playlist.clone();
        dev_file_enqueue.connect_clicked(move |_| {
            let tracks = sel_tracks();
            if tracks.is_empty() {
                return;
            }
            let was_empty = state.borrow().playlist.is_empty();
            for lt in &tracks {
                state.borrow_mut().playlist.add(crate::model::Track::from(lt));
            }
            if state.borrow().config.behavior.autoplay_on_add && was_empty {
                state.borrow_mut().play_current();
            }
            rebuild();
        });
    }

    // Delete / Remove on the selected device files. Behaviour depends on the
    // active view:
    //   • All files  → "Delete": permanently delete the files from the device
    //     AND drop them from every device playlist (Deletion Rule — allowed from
    //     this Media Library external-device view, after confirmation).
    //   • A playlist → "Remove": drop the files from THAT playlist only; the
    //     files stay on the device and in other playlists.
    {
        let sel_tracks = selected_device_tracks.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_store = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        let apply_filter = apply_pl_filter.clone();
        let sel_pl = selected_dev_playlist.clone();
        let state_del = state.clone();
        let win_wk = win.downgrade();
        dev_file_remove.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter()
                .map(|t| std::path::PathBuf::from(&t.path))
                .collect();
            if paths.is_empty() {
                return;
            }
            let n = paths.len();
            let in_playlist = sel_pl.borrow().clone();

            let (message, detail, confirm) = if let Some(pl) = &in_playlist {
                let pl_name = pl
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    format!(
                        "Remove {n} file{} from \"{pl_name}\"?",
                        if n == 1 { "" } else { "s" }
                    ),
                    "The file(s) stay on the device and in any other playlist.".to_string(),
                    "Remove".to_string(),
                )
            } else {
                (
                    format!(
                        "Delete {n} file{} from the device?",
                        if n == 1 { "" } else { "s" }
                    ),
                    "The file(s) are permanently deleted from the device and removed from every \
                     playlist. This can't be undone."
                        .to_string(),
                    "Delete".to_string(),
                )
            };

            let dialog = gtk4::AlertDialog::builder()
                .message(message)
                .detail(detail)
                .buttons(vec!["Cancel".to_string(), confirm])
                .cancel_button(0)
                .default_button(0)
                .modal(true)
                .build();
            let reload_store2 = reload_store.clone();
            let reload_pls2 = reload_pls.clone();
            let apply_filter2 = apply_filter.clone();
            let dev2 = dev.clone();
            let win_wk2 = win_wk.clone();
            let in_playlist2 = in_playlist.clone();
            let state2 = state_del.clone();
            dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
                if res != Ok(1) {
                    return;
                }
                match &in_playlist2 {
                    Some(pl_path) => {
                        // Remove from this playlist only — rewrite its .m3u8.
                        let basenames: std::collections::HashSet<String> = paths
                            .iter()
                            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                            .map(|s| s.to_string())
                            .collect();
                        device_m3u_remove_basenames(pl_path, &basenames);
                        // Re-apply the filter so the removed rows disappear.
                        apply_filter2(&pl_path.to_string_lossy());
                    }
                    None => {
                        // Delete off the device + drop from every playlist.
                        let failed = device_delete_files(&dev2, &paths);
                        reload_store2(dev2.clone());
                        reload_pls2(dev2.clone());
                        // Reconcile the ACTIVE playlist too: device files can
                        // be queued there (device Play/Enqueue), and a deleted
                        // file must show broken immediately — and stop the
                        // player if it was the one playing — instead of
                        // lingering until a read error.
                        let rebuild_pl = {
                            let deleted: std::collections::HashSet<&std::path::PathBuf> =
                                paths.iter().collect();
                            let mut s = state2.borrow_mut();
                            let cur = s.playlist.current_index;
                            let mut touched = false;
                            let mut current_deleted = false;
                            for (i, t) in s.playlist.tracks.iter_mut().enumerate() {
                                if deleted.contains(&t.path) {
                                    t.broken = true;
                                    touched = true;
                                    if i == cur {
                                        current_deleted = true;
                                    }
                                }
                            }
                            if current_deleted
                                && !matches!(
                                    *s.player.state(),
                                    crate::engine::PlayerState::Stopped
                                )
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
                        if failed > 0 {
                            show_alert_parented(
                                win_wk2.upgrade().as_ref(),
                                &format!("{failed} file(s) couldn't be deleted."),
                            );
                        }
                    }
                }
            });
        });
    }

    // Drop target on the device track list: dropping files (from the active
    // playlist, files view, or editor) copies them to the device currently
    // shown in the detail view; dropping a playlist row sends the playlist.
    // Same routing as the sidebar device row, just with a fixed target.
    {
        let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        dt.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
        let sel_backend_drop = selected_dev_backend.clone();
        let current_devices_drop = current_devices.clone();
        let state_drop = state.clone();
        let copy_holder = copy_files_holder.clone();
        let send_holder = send_playlist_holder.clone();
        dt.connect_drop(move |_, value, _x, _y| {
            // Resolve the device currently shown in the detail view.
            let Some(backend) = sel_backend_drop.borrow().clone() else {
                return false;
            };
            let Some(dev) = current_devices_drop
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned()
            else {
                return false;
            };

            // A playlist row (`pl:<id>` String) → send the whole playlist.
            if let Ok(s) = value.get::<String>() {
                if let Some(pid) = s.strip_prefix("pl:").and_then(|n| n.trim().parse::<i64>().ok())
                {
                    let plname = state_drop
                        .borrow()
                        .media_lib
                        .as_ref()
                        .and_then(|l| l.playlist_by_id(pid).ok())
                        .map(|p| p.name)
                        .unwrap_or_default();
                    if let Some(send) = send_holder.borrow().as_ref() {
                        send(dev, pid, plname);
                        return true;
                    }
                    return false;
                }
                // Otherwise a uri/path-list String → copy those files.
                let paths: Vec<std::path::PathBuf> = s
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(|l| {
                        if l.starts_with("file://") {
                            gio::File::for_uri(l)
                                .path()
                                .unwrap_or_else(|| std::path::PathBuf::from(l))
                        } else {
                            std::path::PathBuf::from(l)
                        }
                    })
                    .collect();
                if paths.is_empty() {
                    return false;
                }
                if let Some(copy) = copy_holder.borrow().as_ref() {
                    copy(dev, paths);
                    return true;
                }
                return false;
            }

            // A FileList drag → copy the dragged files.
            if let Ok(file_list) = value.get::<gdk::FileList>() {
                let paths: Vec<std::path::PathBuf> =
                    file_list.files().iter().filter_map(|f| f.path()).collect();
                if paths.is_empty() {
                    return false;
                }
                if let Some(copy) = copy_holder.borrow().as_ref() {
                    copy(dev, paths);
                    return true;
                }
            }
            false
        });
        dev_tracks_scroll.add_controller(dt);
    }

    // ── Right-click context menu on device files: View / Edit ID3 ────────────
    // Mirrors the active-playlist menu. The ID3 editor also shows/edits album
    // art, so this one item covers viewing artwork too. Operates on the current
    // selection (like the Play / Enqueue / Delete buttons in this view); the
    // editor binds one file, so the item appears only for a single selection.
    // Gesture + action group live on the ScrolledWindow, not the ColumnView, to
    // dodge the GTK4 bug where a PopoverMenu parented on the view misses hover.
    {
        let ctx_click = GestureClick::new();
        ctx_click.set_button(3); // right mouse button

        let dev_file_action_group = gio::SimpleActionGroup::new();
        dev_tracks_scroll.insert_action_group("dev-file", Some(&dev_file_action_group));

        // Send-to actions (Task 8) live in a separate "dev" group so the
        // pre-existing "dev-file" prefix (edit-id3) is untouched. Same five
        // action names / bodies as the other three Send-to consumers.
        // Device-to-device: current_devices already contains the device
        // being viewed, and it isn't filtered out here — sending to it is
        // a harmless skip-if-present copy, not worth special-casing.
        let dev_send_action_group = gio::SimpleActionGroup::new();
        dev_tracks_scroll.insert_action_group("dev", Some(&dev_send_action_group));

        // Send to Active Playlist — same body as the Enqueue button below.
        {
            let sel_tracks = selected_device_tracks.clone();
            let state = state.clone();
            let rebuild = rebuild_playlist.clone();
            let action = gio::SimpleAction::new("send-active", None);
            action.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                if tracks.is_empty() {
                    return;
                }
                let was_empty = state.borrow().playlist.is_empty();
                for lt in &tracks {
                    state.borrow_mut().playlist.add(crate::model::Track::from(lt));
                }
                if state.borrow().config.behavior.autoplay_on_add && was_empty {
                    state.borrow_mut().play_current();
                }
                rebuild();
            });
            dev_send_action_group.add_action(&action);
        }

        // Seed a brand new saved playlist from the selected device files.
        {
            let sel_tracks = selected_device_tracks.clone();
            let state = state.clone();
            let win_new = win.clone();
            let action = gio::SimpleAction::new("add-to-new", None);
            action.connect_activate(move |_, _| {
                let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
                if paths.is_empty() {
                    return;
                }
                let default_stem = glib::DateTime::now_local()
                    .ok()
                    .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Playlist".to_string());
                let state_cb = state.clone();
                let paths_cb = paths.clone();
                run_playlist_save_dialog(
                    state.clone(),
                    win_new.clone(),
                    &default_stem,
                    move |path, win_cb| {
                        if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                            if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                                eprintln!("save_playlist_tracks_to_path: {e}");
                                show_playlist_save_error(&win_cb, &path, &e);
                            }
                        }
                        notify_playlist_nav_refresh();
                    },
                );
            });
            dev_send_action_group.add_action(&action);
        }

        // Append selected device files to an existing saved playlist.
        {
            let sel_tracks = selected_device_tracks.clone();
            let state = state.clone();
            let action = gio::SimpleAction::new(
                "add-to-saved",
                Some(glib::VariantTy::INT64),
            );
            action.connect_activate(move |_, param| {
                let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
                let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
                if paths.is_empty() {
                    return;
                }
                let mut ok = false;
                if let Some(lib) = state.borrow().media_lib.as_ref() {
                    match lib.append_paths_to_playlist(pid, &paths) {
                        Ok(_) => ok = true,
                        Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                    }
                }
                if ok {
                    notify_playlist_changed(pid);
                }
            });
            dev_send_action_group.add_action(&action);
        }

        // Send to Disc Drive: probe-on-add, then queue onto THAT drive.
        // Same body as the Files view's ml.send-drive, but metadata comes
        // straight from the already-fetched device LibTrack rows — no
        // media_lib lookup, since device files are often not indexed there.
        {
            let sel_tracks = selected_device_tracks.clone();
            let burn_queues = burn_queues.clone();
            let burn_refresh_holder = burn_refresh_holder.clone();
            let current_drives = current_drives.clone();
            let win_wk = win.downgrade();
            let status = dev_status.clone();
            let action = gio::SimpleAction::new(
                "send-drive",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(drive_id) = target.and_then(|v| v.get::<String>()) else { return };
                let drive_label = current_drives
                    .borrow()
                    .iter()
                    .find(|d| d.id == drive_id)
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| drive_id.clone());
                // Live selection at dispatch (already correct here —
                // `selected_device_tracks` reads the selection model
                // fresh on every call, not a right-click stash).
                let tracks = sel_tracks();
                let paths: Vec<std::path::PathBuf> = tracks.iter()
                    .map(|t| std::path::PathBuf::from(&t.path))
                    .collect();
                // Metadata comes straight from the already-fetched device
                // LibTrack rows — no media_lib lookup, since device files
                // are often not indexed there.
                let metas: std::collections::HashMap<_, _> = tracks.iter().map(|t| {
                    let display = match (&t.artist, &t.title) {
                        (Some(a), Some(ti)) if !a.is_empty() => format!("{a} - {ti}"),
                        (_, Some(ti)) => ti.clone(),
                        _ => t.filename.clone(),
                    };
                    let secs = t.length_secs.map(|s| s as u32);
                    let bytes = std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0);
                    (std::path::PathBuf::from(&t.path), (display, secs, bytes))
                }).collect();
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
            dev_send_action_group.add_action(&action);
        }

        // Send to Removable Device: hand off to the Files view's copy
        // runner via the shared holder.
        {
            let sel_tracks = selected_device_tracks.clone();
            let current_devices = current_devices.clone();
            let copy_files_holder = copy_files_holder.clone();
            let action = gio::SimpleAction::new(
                "send-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) = target.and_then(|v| v.get::<String>()) else { return };
                let dev = current_devices
                    .borrow()
                    .iter()
                    .find(|d| d.id == dev_id)
                    .cloned();
                let paths: Vec<std::path::PathBuf> = sel_tracks().iter()
                    .map(|t| std::path::PathBuf::from(&t.path))
                    .collect();
                if let (Some(dev), false) = (dev, paths.is_empty()) {
                    if let Some(run) = copy_files_holder.borrow().clone() {
                        run(dev, paths);
                    }
                }
            });
            dev_send_action_group.add_action(&action);
        }

        let action_id3 = gio::SimpleAction::new("edit-id3", None);
        {
            let state_id3 = state.clone();
            let win_id3 = win.downgrade();
            let sel_tracks = selected_device_tracks.clone();
            let reload_store = reload_device_store.clone();
            let current_devices_id3 = current_devices.clone();
            let sel_backend_id3 = selected_dev_backend.clone();
            action_id3.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                let [track] = tracks.as_slice() else { return };
                let path = std::path::PathBuf::from(&track.path);
                // Re-read the edited device file's row so new tags show.
                let reload = reload_store.clone();
                let devices = current_devices_id3.clone();
                let backend = sel_backend_id3.clone();
                let rebuild_cb: Rc<dyn Fn()> = Rc::new(move || {
                    let Some(b) = backend.borrow().clone() else { return };
                    if let Some(dev) =
                        devices.borrow().iter().find(|d| d.backend_id == b).cloned()
                    {
                        reload(dev);
                    }
                });
                open_id3_editor_window(
                    win_id3.upgrade().as_ref(),
                    path,
                    state_id3.clone(),
                    rebuild_cb,
                    None,
                );
            });
        }
        dev_file_action_group.add_action(&action_id3);

        let sel_menu = selected_device_tracks.clone();
        let scroll_menu = dev_tracks_scroll.clone();
        let state_menu_dev = state.clone();
        let drives_menu_dev = current_drives.clone();
        let devices_menu_dev = current_devices.clone();
        ctx_click.connect_pressed(move |gest, _, x, y| {
            let sel = sel_menu();
            if sel.is_empty() {
                return;
            }
            let menu = gio::Menu::new();
            // Only a single-file selection is editable (the editor binds one file).
            if sel.len() == 1 {
                menu.append_item(&gio::MenuItem::new(
                    Some("🎵 View / Edit ID3"),
                    Some("dev-file.edit-id3"),
                ));
            }
            let send = build_send_to_menu(
                &state_menu_dev,
                &SendToActions {
                    active: "dev.send-active",
                    new_playlist: "dev.add-to-new",
                    saved_playlist: "dev.add-to-saved",
                    drive: "dev.send-drive",
                    device: "dev.send-device",
                    drives: drives_menu_dev.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    // Includes the device currently being viewed — sending
                    // to it is a harmless skip-if-present copy (Task 8).
                    devices: devices_menu_dev.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                },
            );
            menu.append_submenu(Some("Send to"), &send);
            let popover = gtk4::PopoverMenu::from_model_full(
                &menu,
                gtk4::PopoverMenuFlags::NESTED,
            );
            popover.set_parent(&scroll_menu);
            // Unparent on close so a right-click doesn't leak a popover per use.
            popover.connect_closed(|p| p.unparent());
            let rect = gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
            gest.set_state(gtk4::EventSequenceState::Claimed);
        });
        dev_tracks_scroll.add_controller(ctx_click);
    }

    dev_page.append(&dev_detail);

    let _vsep_unused = (); // replaced by Paned divider

    // ── "Disc Drives" content page (optical drives; Phase 1: play) ────────
    // Overview (one card per drive) + detail (audio track list + add actions).
    let disc_page = GtkBox::new(Orientation::Vertical, 8);
    disc_page.set_margin_top(8);
    disc_page.set_margin_start(8);
    disc_page.set_margin_end(8);

    // Overview: shown when the Disc Drives header is selected.
    let disc_overview = GtkBox::new(Orientation::Vertical, 6);
    let disc_overview_title = Label::builder()
        .label("Disc Drives")
        .halign(Align::Start)
        .xalign(0.0)
        .build();
    disc_overview_title.add_css_class("ml-section-header");
    disc_overview.append(&disc_overview_title);
    // Dismissible disconnect notice (Phase 7): shown when the drive being
    // viewed vanishes mid-session — mac's overview banner, GTK dress.
    let disc_disconnect_row = GtkBox::new(Orientation::Horizontal, 6);
    disc_disconnect_row.set_visible(false);
    let disc_disconnect_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .build();
    disc_disconnect_lbl.add_css_class("broken");
    let disc_disconnect_dismiss = Button::with_label("✕");
    disc_disconnect_dismiss.add_css_class("pl-btn");
    {
        let row = disc_disconnect_row.clone();
        disc_disconnect_dismiss.connect_clicked(move |_| row.set_visible(false));
    }
    disc_disconnect_row.append(&disc_disconnect_lbl);
    disc_disconnect_row.append(&disc_disconnect_dismiss);
    disc_overview.append(&disc_disconnect_row);
    let disc_overview_list = GtkBox::new(Orientation::Vertical, 12);
    disc_overview_list.set_margin_top(6);
    disc_overview.append(&disc_overview_list);
    disc_page.append(&disc_overview);

    // Detail: the selected drive (hidden until one is picked).
    let disc_detail = GtkBox::new(Orientation::Vertical, 8);
    disc_detail.set_visible(false);
    // Header: drive icon (media badge overlaid, rebuilt per populate) beside
    // the title/media/tag labels — same layout as the mac drive header.
    let disc_header_row = GtkBox::new(Orientation::Horizontal, 10);
    let disc_icon_box = GtkBox::new(Orientation::Horizontal, 0);
    disc_icon_box.set_valign(Align::Center);
    disc_header_row.append(&disc_icon_box);
    let disc_header_text = GtkBox::new(Orientation::Vertical, 2);
    let disc_title = Label::builder().halign(Align::Start).xalign(0.0).build();
    disc_title.add_css_class("ml-section-header");
    disc_header_text.append(&disc_title);
    let disc_media_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    disc_media_lbl.add_css_class("dim-label");
    disc_header_text.append(&disc_media_lbl);
    // "Artist — Album" once the disc has gnudb/edited tags (hidden otherwise).
    let disc_tag_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    disc_tag_lbl.add_css_class("ml-section-header");
    disc_tag_lbl.set_visible(false);
    disc_header_text.append(&disc_tag_lbl);
    disc_header_row.append(&disc_header_text);
    disc_detail.append(&disc_header_row);
    // Banner shown for non-audio media (no disc / blank / data).
    let disc_banner = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    disc_banner.add_css_class("broken");
    disc_banner.set_visible(false);
    disc_detail.append(&disc_banner);
    // Audio-track list: multi-select rows "Track N — MM:SS".
    let disc_track_list = gtk4::ListBox::new();
    disc_track_list.set_selection_mode(gtk4::SelectionMode::Multiple);
    // Single click only selects (for Add Selected); a double-click activates a
    // row to add just that track — matching the established double-click add.
    disc_track_list.set_activate_on_single_click(false);
    disc_track_list.add_css_class("ml-col-view");
    // Search filters just this disc's tracks. The filter hides rows without
    // reindexing them, so row.index() keeps mapping onto the entries store
    // (Add Selected, double-click add, rip preselection all stay correct).
    let disc_search_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let (disc_search_row, disc_search_entry) =
        make_view_search_row("Search this disc — track title…");
    {
        let q = disc_search_query.clone();
        let entries_store = current_disc_entries.clone();
        disc_track_list.set_filter_func(move |row| {
            let q = q.borrow();
            if q.is_empty() {
                return true;
            }
            let idx = row.index();
            if idx < 0 {
                return true;
            }
            entries_store
                .borrow()
                .get(idx as usize)
                .map(|e| e.title.to_lowercase().contains(q.as_str()))
                .unwrap_or(true)
        });
    }
    {
        let q = disc_search_query.clone();
        let list = disc_track_list.clone();
        disc_search_entry.connect_changed(move |e| {
            *q.borrow_mut() = e.text().to_lowercase();
            list.invalidate_filter();
        });
    }
    disc_detail.append(&disc_search_row);
    let disc_tracks_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&disc_track_list)
        .build();
    disc_detail.append(&disc_tracks_scroll);

    // Transient status for gnudb lookups + rip results; the data-disc file
    // browser below also reports its read/copy progress and errors through
    // it. Declared here (ahead of its append call further down, which stays
    // in its original position in the vertical layout) so the file-browser
    // closures built next can already capture it.
    let disc_status_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    disc_status_lbl.add_css_class("dim-label");

    // ── Data-disc file browser (Task 9) ─────────────────────────────────────
    // Shown instead of the audio track list when the loaded media is present,
    // not blank, and not an audio CD (or an audio CD whose TOC came back
    // empty). Modeled on the device track view (`dev_col_view`): a simplified
    // ColumnView — #, Title, Length, Size — over a ListStore of
    // `glib::BoxedAnyObject`-wrapped `DiscFile` rows.
    let disc_files_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let disc_files_sort_model =
        SortListModel::new(Some(disc_files_store.clone()), None::<gtk4::Sorter>);
    let disc_files_selection = MultiSelection::new(Some(disc_files_sort_model.clone()));
    let disc_files_col_view = ColumnView::new(Some(disc_files_selection.clone()));
    disc_files_col_view.add_css_class("ml-col-view");
    disc_files_col_view.set_hexpand(true);
    disc_files_col_view.set_vexpand(true);
    {
        // "#" — row position (mirrors dev_pos_col).
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::End)
                .xalign(1.0)
                .margin_start(6)
                .margin_end(6)
                .css_classes(["pl-duration"])
                .build();
            li.set_child(Some(&lbl));
        });
        factory.connect_bind(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                lbl.set_text(&(li.position() + 1).to_string());
            }
        });
        let col = ColumnViewColumn::new(Some("#"), Some(factory));
        col.set_fixed_width(48);
        disc_files_col_view.append_column(&col);

        // Title.
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(6)
                .margin_end(6)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .css_classes(["ml-col-label"])
                .build();
            li.set_child(Some(&lbl));
        });
        factory.connect_bind(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else { return };
            let Some(boxed) = li.item().and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            lbl.set_text(&gtk_safe(&boxed.borrow::<crate::disc::mount::DiscFile>().display));
        });
        let title_sorter = CustomSorter::new(|a, b| {
            let ka = a
                .downcast_ref::<glib::BoxedAnyObject>()
                .map(|o| o.borrow::<crate::disc::mount::DiscFile>().display.clone())
                .unwrap_or_default();
            let kb = b
                .downcast_ref::<glib::BoxedAnyObject>()
                .map(|o| o.borrow::<crate::disc::mount::DiscFile>().display.clone())
                .unwrap_or_default();
            ka.cmp(&kb).into()
        });
        let col = ColumnViewColumn::new(Some("Title"), Some(factory));
        col.set_resizable(true);
        col.set_expand(true);
        col.set_sorter(Some(&title_sorter));
        disc_files_col_view.append_column(&col);

        // Length — "M:SS", or "—" when the duration couldn't be probed.
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::End)
                .xalign(1.0)
                .margin_start(6)
                .margin_end(6)
                .css_classes(["pl-duration"])
                .build();
            li.set_child(Some(&lbl));
        });
        factory.connect_bind(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else { return };
            let Some(boxed) = li.item().and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let secs = boxed.borrow::<crate::disc::mount::DiscFile>().duration_secs;
            lbl.set_text(&match secs {
                Some(s) => format!("{}:{:02}", s / 60, s % 60),
                None => "—".to_string(),
            });
        });
        let len_sorter = CustomSorter::new(|a, b| {
            let ka = a
                .downcast_ref::<glib::BoxedAnyObject>()
                .map(|o| o.borrow::<crate::disc::mount::DiscFile>().duration_secs.unwrap_or(0))
                .unwrap_or(0);
            let kb = b
                .downcast_ref::<glib::BoxedAnyObject>()
                .map(|o| o.borrow::<crate::disc::mount::DiscFile>().duration_secs.unwrap_or(0))
                .unwrap_or(0);
            ka.cmp(&kb).into()
        });
        let col = ColumnViewColumn::new(Some("Length"), Some(factory));
        col.set_fixed_width(80);
        col.set_sorter(Some(&len_sorter));
        disc_files_col_view.append_column(&col);

        // Size in MB.
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::End)
                .xalign(1.0)
                .margin_start(6)
                .margin_end(6)
                .css_classes(["pl-duration"])
                .build();
            li.set_child(Some(&lbl));
        });
        factory.connect_bind(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else { return };
            let Some(boxed) = li.item().and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let bytes = boxed.borrow::<crate::disc::mount::DiscFile>().bytes;
            lbl.set_text(&format!("{:.1} MB", bytes as f64 / 1e6));
        });
        let size_sorter = CustomSorter::new(|a, b| {
            let ka = a
                .downcast_ref::<glib::BoxedAnyObject>()
                .map(|o| o.borrow::<crate::disc::mount::DiscFile>().bytes)
                .unwrap_or(0);
            let kb = b
                .downcast_ref::<glib::BoxedAnyObject>()
                .map(|o| o.borrow::<crate::disc::mount::DiscFile>().bytes)
                .unwrap_or(0);
            ka.cmp(&kb).into()
        });
        let col = ColumnViewColumn::new(Some("Size"), Some(factory));
        col.set_fixed_width(90);
        col.set_sorter(Some(&size_sorter));
        disc_files_col_view.append_column(&col);
        disc_files_sort_model.set_sorter(disc_files_col_view.sorter().as_ref());
    }
    let disc_files_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&disc_files_col_view)
        .build();
    disc_files_scroll.set_visible(false);
    disc_detail.append(&disc_files_scroll);

    // Currently-selected data-disc file rows, read fresh on every Send-to /
    // Add-to-Library dispatch (mirrors `selected_device_tracks`).
    let selected_disc_files: Rc<dyn Fn() -> Vec<crate::disc::mount::DiscFile>> = {
        let sel = disc_files_selection.clone();
        let model = disc_files_sort_model.clone();
        Rc::new(move || {
            let mut out = Vec::new();
            for i in 0..model.n_items() {
                if !sel.is_selected(i) {
                    continue;
                }
                if let Some(o) = model.item(i).and_downcast::<glib::BoxedAnyObject>() {
                    out.push(o.borrow::<crate::disc::mount::DiscFile>().clone());
                }
            }
            out
        })
    };

    // Off-thread mount + walk for the data-disc file list. Wrapped in the
    // same exclusive-read guard (`disc_reading`) the rip flow uses around its
    // own disc reads — `ensure_mounted` spins the drive and probes the
    // filesystem, exactly like a TOC probe. `disc_files_busy` skips a second
    // trigger landing mid-fetch (e.g. a poll tick); stale results (the user
    // navigated to a different drive before this finished) are discarded by
    // checking `selected_disc_id` still names this drive when the result lands.
    let load_disc_files: Rc<dyn Fn(crate::disc::OpticalDrive)> = {
        let state = state.clone();
        let store = disc_files_store.clone();
        let status = disc_status_lbl.clone();
        let busy = disc_files_busy.clone();
        let selected_disc_id = selected_disc_id.clone();
        Rc::new(move |drive: crate::disc::OpticalDrive| {
            if busy.get() {
                return;
            }
            busy.set(true);
            status.set_text("Reading disc…");
            // Both guards, like the rip flow: `disc_reading` makes the GTK
            // pollers skip outright; `begin_exclusive_read` is the core-level
            // flag `list_drives_cached`/`list_drives_shared` themselves check
            // (mount.rs's own doc: `ensure_mounted` doesn't take this guard
            // itself — the caller must).
            state.borrow().disc_reading.set(true);
            crate::disc::detect::begin_exclusive_read();
            let state2 = state.clone();
            let store2 = store.clone();
            let status2 = status.clone();
            let busy2 = busy.clone();
            let selected_disc_id2 = selected_disc_id.clone();
            let drive_id = drive.id.clone();
            glib::spawn_future_local(async move {
                let joined = gio::spawn_blocking(
                    move || -> Result<Vec<crate::disc::mount::DiscFile>, String> {
                        let mount = crate::disc::mount::ensure_mounted(&drive)?;
                        Ok(crate::disc::mount::list_disc_files(&mount))
                    },
                )
                .await;
                crate::disc::detect::end_exclusive_read();
                state2.borrow().disc_reading.set(false);
                busy2.set(false);
                let result = match joined {
                    Ok(inner) => inner,
                    Err(_) => Err("internal error reading the disc".to_string()),
                };
                // Discard a stale result — the user may have navigated to a
                // different drive while the mount+walk was in flight.
                if selected_disc_id2.borrow().as_deref() != Some(drive_id.as_str()) {
                    return;
                }
                store2.remove_all();
                match result {
                    Ok(files) => {
                        let n = files.len();
                        for f in files {
                            store2.append(&glib::BoxedAnyObject::new(f));
                        }
                        status2.set_text(&format!("{n} file{} on disc", if n == 1 { "" } else { "s" }));
                    }
                    Err(e) => status2.set_text(&gtk_safe(&format!("Couldn't read disc: {e}"))),
                }
            });
        })
    };

    // Copy disc files into the library's music folder (staged flat, with
    // " (2)", " (3)"… collision suffixes — the same `stage_data_files` helper
    // the data-disc burn flow uses to build its staging directory), then
    // register the copies the same way the rip flow imports its output
    // (`add_files_to_library` + the ML rebuild callback). The destination is
    // the first watched library folder (`rip::default_dest`'s same chain,
    // with no configured override — this button means "into the library",
    // so it must land somewhere `add_files_to_library` will actually pick
    // up); if there is no watched folder at all, the copy is refused up
    // front rather than silently copying files nothing will ever import.
    let add_disc_files_to_library: Rc<dyn Fn(Vec<crate::disc::mount::DiscFile>)> = {
        let state = state.clone();
        let status = disc_status_lbl.clone();
        let busy = disc_files_busy.clone();
        Rc::new(move |files: Vec<crate::disc::mount::DiscFile>| {
            if files.is_empty() {
                return;
            }
            if busy.get() {
                status.set_text("Disc is busy — try again in a moment.");
                return;
            }
            let watched = disc::watched_folders(&state);
            let dest_dir = crate::disc::rip::default_dest(None, watched.first().map(String::as_str));
            if !crate::disc::rip::dest_is_watched(&dest_dir, &watched) {
                status.set_text(
                    "Add a library folder first (Files → Add Folder) — nothing to import into.",
                );
                return;
            }
            busy.set(true);
            // Same double guard as the mount+walk above: the copy reads from
            // the still-mounted disc file by file, so it's a disc read for
            // exactly as long as `load_disc_files`'s mount+walk was.
            state.borrow().disc_reading.set(true);
            crate::disc::detect::begin_exclusive_read();
            let total = files.len();
            status.set_text(&format!("Copying 1/{total}…"));
            let state2 = state.clone();
            let status2 = status.clone();
            let busy2 = busy.clone();
            glib::spawn_future_local(async move {
                let mut copied_paths: Vec<String> = Vec::new();
                let mut failed = 0usize;
                for (i, f) in files.iter().enumerate() {
                    status2.set_text(&format!("Copying {}/{total}…", i + 1));
                    let src = f.path.clone();
                    let dest_dir2 = std::path::PathBuf::from(&dest_dir);
                    let joined = gio::spawn_blocking(move || {
                        crate::disc::burn::stage_data_files(&[src], &dest_dir2)
                    })
                    .await;
                    match joined {
                        Ok(Ok(mut out)) if !out.is_empty() => {
                            copied_paths.push(out.remove(0).display().to_string())
                        }
                        _ => failed += 1,
                    }
                }
                crate::disc::detect::end_exclusive_read();
                state2.borrow().disc_reading.set(false);
                busy2.set(false);
                let mut imported = 0;
                if !copied_paths.is_empty() {
                    if let Some(lib) = state2.borrow().media_lib.as_ref() {
                        imported = lib.add_files_to_library(&copied_paths).unwrap_or(0);
                    }
                }
                if imported > 0 {
                    let cb = state2.borrow().rebuild_ml_callback.clone();
                    if let Some(cb) = cb {
                        cb();
                    }
                }
                let mut msg = format!("Added {imported} of {total} to the library");
                if failed > 0 {
                    msg.push_str(&format!(" ({failed} failed to copy)"));
                }
                status2.set_text(&gtk_safe(&msg));
            });
        })
    };

    // Double-click / Enter plays the file — the mount makes these ordinary
    // file paths. Mirrors the Files view's replace-vs-append + autoplay
    // semantics exactly (col_view.connect_activate in the files view).
    {
        let state = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_df = set_track.clone();
        let sel_model = disc_files_selection.clone();
        disc_files_col_view.connect_activate(move |_, pos| {
            let Some(obj) = sel_model.item(pos).and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let path = obj.borrow::<crate::disc::mount::DiscFile>().path.clone();
            drop(obj);
            let Ok(track) = crate::model::Track::from_path(&path) else { return };
            let was_empty = state.borrow().playlist.is_empty();
            let autoplay = state.borrow().config.behavior.autoplay_on_add;
            let should_replace = state.borrow().config.behavior.playlist_add_behavior
                == crate::config::PlaylistAddBehavior::Replace;
            if should_replace {
                let _ = state.borrow_mut().player.stop();
                state.borrow_mut().playlist.clear();
            }
            state.borrow_mut().playlist.add(track);
            if autoplay && (was_empty || should_replace) {
                if let Some(display) = state.borrow_mut().play_current() {
                    set_track_df(&display);
                }
            }
            rebuild_pl();
        });
    }

    // ── Right-click context menu on data-disc files: Add to Library + the
    // standard Send-to submenu ────────────────────────────────────────────
    // Gesture + action group live on the ScrolledWindow, not the ColumnView
    // (same GTK4 hover-popover dodge as the device view's context menu).
    {
        let ctx_click = GestureClick::new();
        ctx_click.set_button(3);

        let disc_files_action_group = gio::SimpleActionGroup::new();
        disc_files_scroll.insert_action_group("disc-files", Some(&disc_files_action_group));

        // Send to Active Playlist.
        {
            let sel_files = selected_disc_files.clone();
            let state = state.clone();
            let rebuild = rebuild_playlist.clone();
            let action = gio::SimpleAction::new("send-active", None);
            action.connect_activate(move |_, _| {
                let files = sel_files();
                if files.is_empty() {
                    return;
                }
                let was_empty = state.borrow().playlist.is_empty();
                for f in &files {
                    if let Ok(track) = crate::model::Track::from_path(&f.path) {
                        state.borrow_mut().playlist.add(track);
                    }
                }
                if state.borrow().config.behavior.autoplay_on_add && was_empty {
                    state.borrow_mut().play_current();
                }
                rebuild();
            });
            disc_files_action_group.add_action(&action);
        }

        // Seed a brand new saved playlist from the selected disc files.
        {
            let sel_files = selected_disc_files.clone();
            let state = state.clone();
            let win_new = win.clone();
            let action = gio::SimpleAction::new("add-to-new", None);
            action.connect_activate(move |_, _| {
                let paths: Vec<String> = sel_files()
                    .iter()
                    .map(|f| f.path.display().to_string())
                    .collect();
                if paths.is_empty() {
                    return;
                }
                let default_stem = glib::DateTime::now_local()
                    .ok()
                    .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Playlist".to_string());
                let state_cb = state.clone();
                let paths_cb = paths.clone();
                run_playlist_save_dialog(
                    state.clone(),
                    win_new.clone(),
                    &default_stem,
                    move |path, win_cb| {
                        if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                            if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                                eprintln!("save_playlist_tracks_to_path: {e}");
                                show_playlist_save_error(&win_cb, &path, &e);
                            }
                        }
                        notify_playlist_nav_refresh();
                    },
                );
            });
            disc_files_action_group.add_action(&action);
        }

        // Append selected disc files to an existing saved playlist.
        {
            let sel_files = selected_disc_files.clone();
            let state = state.clone();
            let action =
                gio::SimpleAction::new("add-to-saved", Some(glib::VariantTy::INT64));
            action.connect_activate(move |_, param| {
                let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
                let paths: Vec<String> = sel_files()
                    .iter()
                    .map(|f| f.path.display().to_string())
                    .collect();
                if paths.is_empty() {
                    return;
                }
                let mut ok = false;
                if let Some(lib) = state.borrow().media_lib.as_ref() {
                    match lib.append_paths_to_playlist(pid, &paths) {
                        Ok(_) => ok = true,
                        Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                    }
                }
                if ok {
                    notify_playlist_changed(pid);
                }
            });
            disc_files_action_group.add_action(&action);
        }

        // Send to Disc Drive — a data disc's files can queue onto the OTHER
        // drive; the drive currently being browsed is excluded from the menu
        // (build_send_to_menu filters using `selected_disc_id` at popup time).
        {
            let sel_files = selected_disc_files.clone();
            let burn_queues = burn_queues.clone();
            let burn_refresh_holder = burn_refresh_holder.clone();
            let current_drives = current_drives.clone();
            let win_wk = win.downgrade();
            let status = disc_status_lbl.clone();
            let action =
                gio::SimpleAction::new("send-drive", Some(glib::VariantTy::STRING));
            action.connect_activate(move |_, target| {
                let Some(drive_id) = target.and_then(|v| v.get::<String>()) else { return };
                let drive_label = current_drives
                    .borrow()
                    .iter()
                    .find(|d| d.id == drive_id)
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| drive_id.clone());
                let files = sel_files();
                let paths: Vec<std::path::PathBuf> =
                    files.iter().map(|f| f.path.clone()).collect();
                let metas: std::collections::HashMap<_, _> = files
                    .iter()
                    .map(|f| (f.path.clone(), (f.display.clone(), f.duration_secs, f.bytes)))
                    .collect();
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
            disc_files_action_group.add_action(&action);
        }

        // Send to Removable Device — hand off to the Files/Device view's copy
        // runner via the shared holder.
        {
            let sel_files = selected_disc_files.clone();
            let current_devices = current_devices.clone();
            let copy_files_holder = copy_files_holder.clone();
            let action =
                gio::SimpleAction::new("send-device", Some(glib::VariantTy::STRING));
            action.connect_activate(move |_, target| {
                let Some(dev_id) = target.and_then(|v| v.get::<String>()) else { return };
                let dev = current_devices.borrow().iter().find(|d| d.id == dev_id).cloned();
                let paths: Vec<std::path::PathBuf> =
                    sel_files().iter().map(|f| f.path.clone()).collect();
                if let (Some(dev), false) = (dev, paths.is_empty()) {
                    if let Some(run) = copy_files_holder.borrow().clone() {
                        run(dev, paths);
                    }
                }
            });
            disc_files_action_group.add_action(&action);
        }

        // Add to Library.
        {
            let sel_files = selected_disc_files.clone();
            let add_to_lib = add_disc_files_to_library.clone();
            let action = gio::SimpleAction::new("add-to-library", None);
            action.connect_activate(move |_, _| {
                add_to_lib(sel_files());
            });
            disc_files_action_group.add_action(&action);
        }

        // View / Edit ID3 — opens the shared editor on the clicked disc file.
        // The file lives on a read-only iso9660 mount, so the editor detects
        // that and shows every field read-only with no Save button.
        {
            let sel_files = selected_disc_files.clone();
            let state_id3 = state.clone();
            let rebuild_id3 = rebuild_playlist.clone();
            let action = gio::SimpleAction::new("edit-id3", None);
            action.connect_activate(move |_, _| {
                let files = sel_files();
                let Some(f) = files.first() else { return };
                open_id3_editor_window(
                    None::<&gtk4::Window>,
                    f.path.clone(),
                    state_id3.clone(),
                    rebuild_id3.clone(),
                    None,
                );
            });
            disc_files_action_group.add_action(&action);
        }

        let sel_menu = selected_disc_files.clone();
        let scroll_menu = disc_files_scroll.clone();
        let state_menu = state.clone();
        let drives_menu = current_drives.clone();
        let devices_menu = current_devices.clone();
        let selected_disc_id_menu = selected_disc_id.clone();
        ctx_click.connect_pressed(move |gest, _, x, y| {
            if sel_menu().is_empty() {
                return;
            }
            let menu = gio::Menu::new();
            menu.append_item(&gio::MenuItem::new(
                Some("Add to Library"),
                Some("disc-files.add-to-library"),
            ));
            // Single selection only — the editor binds to one file.
            if sel_menu().len() == 1 {
                menu.append_item(&gio::MenuItem::new(
                    Some("View ID3 Tags"),
                    Some("disc-files.edit-id3"),
                ));
            }
            let this_drive = selected_disc_id_menu.borrow().clone();
            let send = build_send_to_menu(
                &state_menu,
                &SendToActions {
                    active: "disc-files.send-active",
                    new_playlist: "disc-files.add-to-new",
                    saved_playlist: "disc-files.add-to-saved",
                    drive: "disc-files.send-drive",
                    device: "disc-files.send-device",
                    drives: drives_menu
                        .borrow()
                        .iter()
                        .filter(|d| Some(&d.id) != this_drive.as_ref())
                        .map(|d| (d.id.clone(), d.label.clone()))
                        .collect(),
                    devices: devices_menu.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                },
            );
            menu.append_submenu(Some("Send to"), &send);
            let popover =
                gtk4::PopoverMenu::from_model_full(&menu, gtk4::PopoverMenuFlags::NESTED);
            // Parent on the group-holding widget and DON'T unparent on close:
            // the unparent severs the action-group link as a nested "Send to"
            // item dispatches (the bug fixed in the playlist editor). Match
            // the working files-view recipe.
            popover.set_parent(&scroll_menu);
            let rect = gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
            gest.set_state(gtk4::EventSequenceState::Claimed);
        });
        disc_files_scroll.add_controller(ctx_click);
    }

    // Add + identify/rip/tag/eject actions. Order matches the macOS drive
    // header (Identify · Rip… · Edit Tags · … · Eject last), with the GTK-only
    // Add buttons in front.
    // Disc management on the left; playlist actions (Enqueue / ▶ Play, acting
    // on the selection or the whole disc when nothing is selected) on the
    // right — same split as the files/playlist/device views.
    let disc_identify = Button::with_label("Identify");
    let disc_rip = Button::with_label("Rip…");
    let disc_edit_tags = Button::with_label("Edit Tags");
    // Shown only when the disc is unknown to gnudb or the user's tags differ
    // from the official match (visibility set in populate_disc_detail).
    let disc_submit = Button::with_label("Submit to gnudb");
    let disc_eject = Button::with_label("Eject");
    // Data-disc-only (Task 9): copies every file currently listed in the
    // browser into the library. Hidden for audio/blank/no-disc states —
    // visibility set alongside the file browser in populate_disc_detail.
    let disc_add_all_btn = Button::with_label("Add All to Library");
    let disc_enqueue = Button::with_label("Enqueue");
    let disc_play = Button::with_label("▶ Play");
    for b in [
        &disc_identify,
        &disc_rip,
        &disc_edit_tags,
        &disc_submit,
        &disc_eject,
        &disc_add_all_btn,
        &disc_enqueue,
        &disc_play,
    ] {
        b.add_css_class("pl-btn");
    }
    let disc_actions = GtkBox::new(Orientation::Horizontal, 6);
    disc_actions.append(&disc_identify);
    disc_actions.append(&disc_rip);
    disc_actions.append(&disc_edit_tags);
    disc_actions.append(&disc_submit);
    disc_actions.append(&disc_eject);
    disc_actions.append(&disc_add_all_btn);
    let disc_actions_spring = GtkBox::new(Orientation::Horizontal, 0);
    disc_actions_spring.set_hexpand(true);
    disc_actions.append(&disc_actions_spring);
    disc_actions.append(&disc_enqueue);
    disc_actions.append(&disc_play);
    disc_detail.append(&disc_actions);
    // Add All to Library: every file currently listed in the data-disc
    // browser, regardless of selection (the per-row context menu's "Add to
    // Library" handles a selection).
    {
        let store = disc_files_store.clone();
        let add_all = add_disc_files_to_library.clone();
        disc_add_all_btn.connect_clicked(move |_| {
            let files: Vec<crate::disc::mount::DiscFile> = (0..store.n_items())
                .filter_map(|i| store.item(i).and_downcast::<glib::BoxedAnyObject>())
                .map(|o| o.borrow::<crate::disc::mount::DiscFile>().clone())
                .collect();
            add_all(files);
        });
    }
    // Rip progress row (hidden unless a rip is running): a bar + Cancel.
    let disc_rip_box = GtkBox::new(Orientation::Horizontal, 6);
    disc_rip_box.set_visible(false);
    let disc_rip_bar = gtk4::ProgressBar::new();
    disc_rip_bar.set_hexpand(true);
    disc_rip_bar.set_show_text(true);
    let disc_rip_cancel = Button::with_label("Cancel");
    disc_rip_cancel.add_css_class("pl-btn");
    disc_rip_box.append(&disc_rip_bar);
    disc_rip_box.append(&disc_rip_cancel);
    disc_detail.append(&disc_rip_box);
    // Transient status for gnudb lookups + rip results (declared earlier,
    // just above the data-disc file browser, which also reports through it).
    disc_detail.append(&disc_status_lbl);
    // Burn panel (Phases 5–6): shown for writable non-audio media
    // (visibility handled by populate_disc_detail).
    let burn_ui = disc::build_burn_panel(
        state.clone(),
        burn_queues.clone(),
        refresh_discs_holder.clone(),
        burn_refresh_holder.clone(),
        burn_progress_map.clone(),
        &win,
    );
    disc_detail.append(&burn_ui.root);
    let burn_ui = Rc::new(burn_ui);
    // Wrap the detail content in an Overlay so the burn card can float over
    // whatever's showing (audio tracks or the burn panel itself) and survive
    // navigating to another drive and back — `populate_disc_detail` decides
    // per drive whether it's visible via `burn_ui.refresh_progress`.
    let disc_detail_overlay = gtk4::Overlay::new();
    disc_detail_overlay.set_child(Some(&disc_detail));
    disc_detail_overlay.add_overlay(&burn_ui.overlay_card);
    disc_page.append(&disc_detail_overlay);

    // ── Content stack ─────────────────────────────────────────────────────
    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(StackTransitionType::None);
    stack.add_named(&dev_page, Some("devices"));
    stack.add_named(&disc_page, Some("discs"));

    // Holders so close_request can save Files-tab state (col_view and all_cols are
    // defined inside the Files block scope below).
    let col_view_holder: Rc<RefCell<Option<ColumnView>>> = Rc::new(RefCell::new(None));
    let all_cols_holder: Rc<RefCell<Vec<(String, ColumnViewColumn)>>> =
        Rc::new(RefCell::new(Vec::new()));

    // ── Page: Files ──────────────────────────────────────────────────────
    {
        let files_vbox = GtkBox::new(Orientation::Vertical, 4);

        let search_entry = Entry::new();
        search_entry.set_placeholder_text(Some("Search artist, title, album…"));
        search_entry.set_hexpand(true);

        let search_clear_btn = Button::with_label("✕");
        search_clear_btn.add_css_class("pl-btn");
        {
            let e = search_entry.clone();
            search_clear_btn.connect_clicked(move |_| {
                e.set_text("");
            });
        }

        let search_row = GtkBox::new(Orientation::Horizontal, 4);
        search_row.set_margin_top(4);
        search_row.set_margin_start(4);
        search_row.set_margin_end(4);
        search_row.append(&search_entry);
        search_row.append(&search_clear_btn);
        files_vbox.append(&search_row);

        let track_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sort_model = SortListModel::new(Some(track_store.clone()), None::<gtk4::Sorter>);
        let multi_sel = MultiSelection::new(Some(sort_model.clone()));
        let col_view = ColumnView::new(Some(multi_sel.clone()));
        col_view.add_css_class("ml-col-view");
        col_view.set_show_row_separators(false);
        col_view.set_show_column_separators(false);
        col_view.set_hexpand(true);
        col_view.set_vexpand(true);
        col_view.set_reorderable(true);

        // Create action group and actions for ML right-click menu
        let ml_action_group = gio::SimpleActionGroup::new();
        col_view.insert_action_group("ml", Some(&ml_action_group));
        // Also on the window so the "Send to ▾" MenuButton in the button row
        // — which is NOT a descendant of col_view — can reach these actions.
        // Without this its menu items rendered disabled (2026-07-16).
        win.insert_action_group("ml", Some(&ml_action_group));

        // The files status label is created further down — the burn action
        // reports "Queued N…" through this holder.
        let files_status_holder: Rc<RefCell<Option<Label>>> = Rc::new(RefCell::new(None));

        // Store for selected tracks (used by action handlers)
        let ml_selected_tracks: Rc<RefCell<Vec<std::path::PathBuf>>> =
            Rc::new(RefCell::new(Vec::new()));

        // Live "currently selected files-view rows" reader. The "Send to ▾"
        // button doesn't go through the per-row right-click gesture, so its
        // actions must read `multi_sel` directly at dispatch time instead
        // of the `ml_selected_tracks` stash above (G1: that stash only
        // updates on right-click and went stale for the button path — the
        // button kept acting on whatever was last right-clicked). Mirrors
        // how `add_selected` (below) already reads `multi_sel` live for
        // "Active Playlist".
        let ml_live_selected_paths: Rc<dyn Fn() -> Vec<std::path::PathBuf>> = {
            let sel = multi_sel.clone();
            Rc::new(move || {
                let mut out = Vec::new();
                for i in 0..sel.n_items() {
                    if sel.is_selected(i) {
                        if let Some(obj) = sel
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let t = obj.borrow::<crate::media_library::LibTrack>();
                            out.push(std::path::PathBuf::from(&t.path));
                        }
                    }
                }
                out
            })
        };

        // Live "currently selected" reader that hands back full `LibTrack`s
        // rather than bare paths — the "Calculate ReplayGain" context action
        // needs the row's `id` (to call `set_replaygain`) and album/artist
        // (to batch the analysis), not just a path. Mirrors
        // `ml_live_selected_paths` above for the same live-vs-stale reason.
        let ml_live_selected_lib_tracks: Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>> = {
            let sel = multi_sel.clone();
            Rc::new(move || {
                let mut out = Vec::new();
                for i in 0..sel.n_items() {
                    if sel.is_selected(i) {
                        if let Some(obj) = sel
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            out.push(obj.borrow::<crate::media_library::LibTrack>().clone());
                        }
                    }
                }
                out
            })
        };

        // Append to Playlist action
        let ml_action_append_state = state.clone();
        let _ml_action_append_sel = multi_sel.clone();
        let ml_action_append_rebuild = rebuild_playlist.clone();
        let ml_action_append_tracks = ml_selected_tracks.clone();
        let action_append = gio::SimpleAction::new("append", None); // Note: action name without "ml." prefix
        action_append.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_append_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            let was_empty = ml_action_append_state.borrow().playlist.is_empty();
            for path in tracks {
                let track = crate::model::Track::from_path(&path).ok();
                if let Some(track) = track {
                    ml_action_append_state.borrow_mut().playlist.add(track);
                }
            }
            if ml_action_append_state
                .borrow()
                .config
                .behavior
                .autoplay_on_add
                && was_empty
            {
                ml_action_append_state.borrow_mut().play_current();
            }
            ml_action_append_rebuild();
        });
        ml_action_group.add_action(&action_append);

        // Send to Disc Drive: probe-on-add, then queue onto THAT drive.
        {
            let state_burn = state.clone();
            let paths_src = ml_live_selected_paths.clone();
            let burn_queues = burn_queues.clone();
            let burn_refresh_holder = burn_refresh_holder.clone();
            let current_drives = current_drives.clone();
            let status = files_status_holder.clone();
            let win_wk = win.downgrade();
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
                // Live selection at dispatch (G1) — not the right-click
                // gesture's stash, which the button never populates.
                let paths: Vec<_> = paths_src();
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
                    Rc::new(move |s: String| {
                        if let Some(lbl) = status.borrow().as_ref() {
                            lbl.set_text(&gtk_safe(&s));
                        }
                    }),
                    win_wk.clone(),
                );
            });
            ml_action_group.add_action(&action);
        }

        // Send to Removable Device: hand off to the existing copy runner,
        // which already reports progress through the files status line.
        {
            let current_devices = current_devices.clone();
            let paths_src = ml_live_selected_paths.clone();
            let copy_files_run = copy_files_run.clone();
            let action = gio::SimpleAction::new(
                "send-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) =
                    target.and_then(|v| v.get::<String>()) else { return };
                let dev = current_devices
                    .borrow()
                    .iter()
                    .find(|d| d.id == dev_id)
                    .cloned();
                // Live selection at dispatch (G1).
                let paths: Vec<_> = paths_src();
                if let (Some(dev), false) = (dev, paths.is_empty()) {
                    copy_files_run(dev, paths);
                }
            });
            ml_action_group.add_action(&action);
        }

        // Replace current playlist action
        let ml_action_replace_state = state.clone();
        let ml_action_replace_tracks = ml_selected_tracks.clone();
        let ml_action_replace_rebuild = rebuild_playlist.clone();
        let action_replace = gio::SimpleAction::new("replace", None); // Note: action name without "ml." prefix
        action_replace.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_replace_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            let _ = ml_action_replace_state.borrow_mut().player.stop();
            ml_action_replace_state.borrow_mut().playlist.clear();
            for path in tracks {
                let track = crate::model::Track::from_path(&path).ok();
                if let Some(track) = track {
                    ml_action_replace_state.borrow_mut().playlist.add(track);
                }
            }
            if ml_action_replace_state
                .borrow()
                .config
                .behavior
                .autoplay_on_add
                && !ml_action_replace_state.borrow().playlist.is_empty()
            {
                ml_action_replace_state.borrow_mut().play_current();
            }
            ml_action_replace_rebuild();
        });
        ml_action_group.add_action(&action_replace);

        // View/Edit ID3 Info action (for single selection)
        let ml_action_id3_state = state.clone();
        let ml_action_id3_tracks = ml_selected_tracks.clone();
        let ml_action_id3_rebuild = rebuild_playlist.clone();
        let action_id3 = gio::SimpleAction::new("edit-id3", None); // Note: action name without "ml." prefix
        action_id3.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_id3_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            // Only open for the first (single) selected track
            let path = tracks[0].clone();
            open_id3_editor_window(
                None::<&gtk4::Window>,
                path,
                ml_action_id3_state.clone(),
                ml_action_id3_rebuild.clone(),
                None,
            );
        });
        ml_action_group.add_action(&action_id3);

        // Rescan Metadata action
        let ml_action_rescan_state = state.clone();
        let ml_action_rescan_tracks = ml_selected_tracks.clone();
        let action_rescan = gio::SimpleAction::new("rescan", None); // Note: action name without "ml." prefix
        action_rescan.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_rescan_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            if ml_action_rescan_state.borrow().ml_scan.is_some() {
                return;
            }
            let paths: Vec<String> = tracks
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let total = paths.len();
            let cancel_flag = start_ml_scan(&ml_action_rescan_state, ScanType::AddFiles, total);
            let (progress_tx, progress_rx) = std::sync::mpsc::channel();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                    Ok(l) => l,
                    Err(_) => {
                        let _ = result_tx.send(());
                        return;
                    }
                };
                for (i, path) in paths.iter().enumerate() {
                    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let _ = lib.rescan_track(path);
                    let _ = progress_tx.send(i + 1);
                }
                let _ = result_tx.send(());
            });
            let progress_rx = std::cell::RefCell::new(progress_rx);
            let result_rx = std::cell::RefCell::new(result_rx);
            let state_for_timer = ml_action_rescan_state.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                while let Ok(current) = progress_rx.borrow().try_recv() {
                    update_ml_scan_progress(&state_for_timer, current, total);
                }
                if result_rx.borrow().try_recv().is_ok() {
                    {
                        let mut s = state_for_timer.borrow_mut();
                        s.media_lib = crate::media_library::MediaLibrary::open().ok();
                    }
                    complete_ml_scan(&state_for_timer);
                    if let Some(ref cb) = state_for_timer.borrow().rebuild_ml_callback {
                        cb();
                    }
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        });
        ml_action_group.add_action(&action_rescan);

        // Calculate ReplayGain action — force-analyzes the current
        // selection (skips the missing-or-stale filter; "Calculate" always
        // re-measures exactly what the user picked). Shares the
        // `analyze_job` worker/progress plumbing with the bulk "Analyze
        // ReplayGain" button defined further down. `files_status` isn't
        // built yet at this point in the function, so — like `action_rescan`
        // above reaches for `rebuild_ml_callback` instead of the not-yet-
        // built `rebuild_files` — this reads the label out of
        // `files_status_holder`, populated once the button row is built.
        let ml_action_calc_rg_state = state.clone();
        let ml_action_calc_rg_tracks = ml_live_selected_lib_tracks.clone();
        let ml_action_calc_rg_status = files_status_holder.clone();
        let action_calc_rg = gio::SimpleAction::new("calc-rg", None);
        action_calc_rg.connect_activate(move |_, _| {
            if !crate::replaygain::rg_analysis_available() {
                return; // feature silently unavailable (house rule)
            }
            let tracks = ml_action_calc_rg_tracks();
            let Some(status_label) = ml_action_calc_rg_status.borrow().clone() else {
                return;
            };
            let state_rc = ml_action_calc_rg_state.clone();
            let rebuild: Rc<dyn Fn()> = {
                let state_for_rb = state_rc.clone();
                Rc::new(move || {
                    let cb = state_for_rb.borrow().rebuild_ml_callback.clone();
                    if let Some(cb) = cb {
                        cb();
                    }
                })
            };
            analyze_job(&state_rc, tracks, true, &status_label, rebuild);
        });
        ml_action_group.add_action(&action_calc_rg);

        // Remove from Media Library action
        let ml_action_remove_tracks = ml_selected_tracks.clone();
        let ml_action_remove_store = track_store.clone();
        let action_remove = gio::SimpleAction::new("remove", None);
        action_remove.connect_activate(move |_, _| {
            let paths = ml_action_remove_tracks.borrow().clone();
            if paths.is_empty() {
                return;
            }

            let path_set: std::collections::HashSet<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let paths_owned: Vec<String> = path_set.iter().cloned().collect();

            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            std::thread::spawn(move || {
                if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                    let _ = lib.soft_delete_tracks_by_paths(&paths_owned);
                    let _ = lib.purge_deleted_tracks();
                }
            });

            let mut rows_to_remove: Vec<u32> = Vec::new();
            for i in 0..ml_action_remove_store.n_items() {
                if let Some(item) = ml_action_remove_store.item(i) {
                    if let Some(boxed) = item.downcast_ref::<glib::BoxedAnyObject>() {
                        let track = boxed.borrow::<crate::media_library::LibTrack>();
                        if path_set.contains(&track.path) {
                            rows_to_remove.push(i);
                        }
                    }
                }
            }

            for idx in rows_to_remove.into_iter().rev() {
                ml_action_remove_store.remove(idx);
            }
        });
        ml_action_group.add_action(&action_remove);

        // Seed a brand new saved playlist from the current ML selection.
        let ml_action_new_state  = state.clone();
        let ml_action_new_paths  = ml_live_selected_paths.clone();
        let ml_action_new_win    = win.clone();
        let action_add_to_new    = gio::SimpleAction::new("add-to-new", None);
        action_add_to_new.connect_activate(move |_, _| {
            // Live selection at dispatch (G1).
            let paths: Vec<String> = ml_action_new_paths()
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if paths.is_empty() { return }
            let default_stem = glib::DateTime::now_local()
                .ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = ml_action_new_state.clone();
            let paths_cb = paths.clone();
            run_playlist_save_dialog(
                ml_action_new_state.clone(),
                ml_action_new_win.clone(),
                &default_stem,
                move |path, win_cb| {
                    if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                        }
                    }
                    // The new playlist must appear in the sidebar + manage
                    // list right away (same call the active playlist's
                    // Save-as flow already makes).
                    notify_playlist_nav_refresh();
                },
            );
        });
        ml_action_group.add_action(&action_add_to_new);

        // Add-to-saved-playlist action (parameterised by target playlist id).
        // Append currently selected ML file paths to the chosen saved playlist.
        let ml_action_add_state = state.clone();
        let ml_action_add_paths = ml_live_selected_paths.clone();
        let ml_action_add_win = win.downgrade();
        let action_add_to_saved = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action_add_to_saved.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            // Live selection at dispatch (G1).
            let paths: Vec<String> = ml_action_add_paths()
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if paths.is_empty() { return }
            let mut ok = false;
            let mut err_msg: Option<String> = None;
            if let Some(lib) = ml_action_add_state.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_)  => ok = true,
                    // Surface the failure instead of only logging it — e.g.
                    // a read-only playlist folder must not fail silently.
                    Err(e) => err_msg = Some(format!("Couldn't save to the playlist:\n{e}")),
                }
            }
            if ok {
                notify_playlist_changed(pid);
            } else if let Some(msg) = err_msg {
                show_alert_parented(
                    ml_action_add_win.upgrade().as_ref(),
                    &gtk_safe(&msg),
                );
            }
        });
        ml_action_group.add_action(&action_add_to_saved);

        let col_defs: &[(&str, &str, i32, bool)] = ALL_COLUMNS
            .iter()
            .map(|c| (c.id, c.header, 80, c.expand))
            .collect::<Vec<_>>()
            .leak();

        let visible_ids: Vec<String> = state.borrow().config.media_library.visible_columns.clone();
        let saved_widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();

        // Track which artwork buttons have been connected to avoid duplicate click handlers
        // (connect_bind fires each time an item is shown after a scroll).
        let connected_artwork: Rc<RefCell<std::collections::HashSet<glib::Object>>> =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        // Source artwork paths with a lazy thumbnail generation already in
        // flight, so a rebind during a scroll (ColumnView recycles cells)
        // doesn't spawn a second decode of the same file.
        let thumb_inflight: Rc<RefCell<std::collections::HashSet<PathBuf>>> =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        // 36k-row library: never decode a full-size image on the render
        // path. This is the on-disk cached-thumbnail edge length.
        const ML_ARTWORK_THUMB_PX: i32 = 40;

        // Capture store_ref before factory so it's available for the factory's right-click handler
        let store_for_ctx = track_store.clone();

        // ── Unscanned indicator column (always first, always visible) ──────────
        {
            let unscanned_factory = SignalListItemFactory::new();

            unscanned_factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() {
                    return;
                }
                let lbl = Label::builder()
                    .halign(Align::Center)
                    .valign(Align::Center)
                    .css_classes(["ml-col-label"])
                    .build();
                li.set_child(Some(&lbl));
            });

            unscanned_factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let boxed = li
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok());
                let Some(boxed) = boxed else {
                    return;
                };
                let t = boxed.borrow::<crate::media_library::LibTrack>();
                let lbl = li.child().and_then(|c| c.downcast::<Label>().ok());
                let Some(lbl) = lbl else {
                    return;
                };
                let path = std::path::Path::new(&t.path);
                // A row can carry a `last_scanned` timestamp yet have no real
                // metadata: `update_last_scanned` runs after every scan pass
                // even when extraction produced nothing (e.g. the duration
                // probe failed). So "scanned" for the status glyph means
                // metadata was actually extracted — duration is the reliable
                // tell — not merely that a timestamp exists.
                //   ❓ never (properly) scanned — no metadata
                //   🔄 scanned, but the file changed since (rescan to refresh)
                //   🔒 read-only
                let scanned = t.length_secs.is_some() && t.last_scanned.is_some();
                if !scanned {
                    lbl.set_label("❓");
                    lbl.set_tooltip_text(Some(
                        "Not scanned yet — metadata loads on the next scan",
                    ));
                } else if crate::media_library::MediaLibrary::needs_metadata_scan(
                    &t.path,
                    t.last_scanned.as_deref(),
                ) {
                    lbl.set_label("🔄");
                    lbl.set_tooltip_text(Some(
                        "File changed since last scan — rescan to refresh its metadata",
                    ));
                } else if crate::media_library::is_read_only(path) {
                    lbl.set_label("🔒");
                    lbl.set_tooltip_text(Some("Read-only file"));
                } else {
                    lbl.set_label("");
                    lbl.set_tooltip_text(None);
                }
            });

            let unscanned_col = ColumnViewColumn::new(Some(""), Some(unscanned_factory));
            unscanned_col.set_fixed_width(24);
            col_view.append_column(&unscanned_col);
        }

        let all_cols: Vec<(String, ColumnViewColumn)> = col_defs
            .iter()
            .map(|(id, header, _min_w, expand)| {
                let factory = SignalListItemFactory::new();
                let id_str = id.to_string();
                let is_artwork = id_str == "artwork_path";
                let connected = connected_artwork.clone();
                let inflight = thumb_inflight.clone();
                let ctx_multi_sel = multi_sel.clone();
                let ctx_col_view = col_view.clone();
                let _ctx_store = store_for_ctx.clone();
                let ml_tracks_gest = ml_selected_tracks.clone();
                let state_for_ctx = state.clone();
                let ctx_drives = current_drives.clone();
                let ctx_devices = current_devices.clone();

                factory.connect_setup(move |_, obj| {
                    let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();

                    // Skip if child already exists (row is being recycled)
                    if li.child().is_some() {
                        return;
                    }

                    let child: gtk4::Widget;

                    if is_artwork {
                        // Thumbnail image, blank until the cached PNG exists
                        // (connect_bind fills it in, generating it lazily
                        // off-thread the first time a row is shown).
                        let img = Image::builder()
                            .pixel_size(ML_ARTWORK_THUMB_PX)
                            .build();
                        let btn = Button::builder()
                            .child(&img)
                            .halign(Align::Start)
                            .margin_start(6)
                            .margin_end(6)
                            .margin_top(3)
                            .margin_bottom(3)
                            .hexpand(true)
                            .vexpand(true)
                            .halign(Align::Fill)
                            .valign(Align::Fill)
                            .build();
                        child = btn.upcast::<gtk4::Widget>();
                    } else {
                        let lbl = Label::builder()
                            .margin_start(6)
                            .margin_end(6)
                            .margin_top(3)
                            .margin_bottom(3)
                            .hexpand(true)
                            .vexpand(true)
                            .halign(Align::Fill)
                            .valign(Align::Fill)
                            .xalign(0.0)
                            .ellipsize(gtk4::pango::EllipsizeMode::End)
                            .css_classes(["ml-col-label"])
                            .build();
                        child = lbl.upcast::<gtk4::Widget>();
                    }

                    // Per-cell DragSource — collects every currently-selected
                    // ML row as a FileList content provider so the user can
                    // drag library tracks out to the active playlist's
                    // pl_scroll drop target (which accepts FileList).  Plain
                    // single-track drag works too: if the row under the
                    // pointer is not in the selection it still ships its
                    // own path.
                    {
                        let ds = gtk4::DragSource::new();
                        ds.set_actions(gtk4::gdk::DragAction::COPY);
                        let ds_sel = ctx_multi_sel.clone();
                        let ds_li  = li.clone();
                        ds.connect_prepare(move |_, _, _| {
                            let mut paths: Vec<std::path::PathBuf> = Vec::new();
                            let mut self_path: Option<std::path::PathBuf> = None;
                            if let Some(obj) = ds_li.item()
                                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            {
                                let t = obj.borrow::<crate::media_library::LibTrack>();
                                self_path = Some(std::path::PathBuf::from(&t.path));
                            }
                            for i in 0..ds_sel.n_items() {
                                if ds_sel.is_selected(i) {
                                    if let Some(obj) = ds_sel.item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        paths.push(std::path::PathBuf::from(&t.path));
                                    }
                                }
                            }
                            if paths.is_empty() {
                                if let Some(p) = self_path { paths.push(p); }
                            }
                            if paths.is_empty() { return None }
                            let files: Vec<gio::File> = paths.iter()
                                .map(|p| gio::File::for_path(p))
                                .collect();
                            let fl = gdk::FileList::from_array(&files);
                            Some(gdk::ContentProvider::for_value(&fl.to_value()))
                        });
                        child.add_controller(ds);
                    }

                    // Add right-click gesture to each row.  Capture phase
                    // pre-empts ColumnView's default secondary-button
                    // handler so multi-selection survives long enough for
                    // our is_selected guard to inspect it.
                    let gesture = gtk4::GestureClick::new();
                    gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
                    gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
                    let sel_gest = ctx_multi_sel.clone();
                    let col_popup = ctx_col_view.clone();
                    let li_gest = li.clone();
                    let ml_tracks_for_gest = ml_tracks_gest.clone();
                    let state_for_gest = state_for_ctx.clone();
                    let drives_for_gest = ctx_drives.clone();
                    let devices_for_gest = ctx_devices.clone();
                    gesture.connect_pressed(move |gest, n_press, x, y| {
                        if n_press != 1 {
                            return;
                        }
                        // Get the item directly from the ListItem - no coordinate math needed!
                        let Some(item) = li_gest.item() else {
                            return;
                        };
                        let item_clone = item.clone();

                        // Find the index of the clicked item by checking each item
                        let mut clicked_index: Option<u32> = None;
                        for i in 0..sel_gest.n_items() {
                            if let Some(model_item) = sel_gest.item(i) {
                                if model_item == item_clone {
                                    clicked_index = Some(i);
                                    break;
                                }
                            }
                        }

                        // Only change selection if clicked on non-selected item
                        // This preserves multi-selection when right-clicking on selected items
                        if let Some(idx) = clicked_index {
                            if !sel_gest.is_selected(idx) {
                                sel_gest.unselect_all();
                                sel_gest.select_item(idx, true);
                            }
                        }

                        // Collect selected tracks into shared state for action handlers
                        let mut paths: Vec<std::path::PathBuf> = Vec::new();
                        let mut selected_count = 0usize;
                        for i in 0..sel_gest.n_items() {
                            if sel_gest.is_selected(i) {
                                if let Some(obj) = sel_gest
                                    .item(i)
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                {
                                    let t = obj.borrow::<crate::media_library::LibTrack>();
                                    paths.push(std::path::PathBuf::from(&t.path));
                                    selected_count += 1;
                                }
                            }
                        }
                        *ml_tracks_for_gest.borrow_mut() = paths;

                        // Convert coordinates from gesture widget to ColumnView
                        // The gesture gives coords in the child widget's space
                        let child = li_gest.child();
                        let (popup_x, popup_y) = if let Some(child_widget) = child {
                            if let Some((rel_x, rel_y)) =
                                child_widget.translate_coordinates(&col_popup, x, y)
                            {
                                (rel_x, rel_y)
                            } else {
                                (x, y)
                            }
                        } else {
                            (x, y)
                        };

                        // Build menu model
                        let menu = gio::Menu::new();
                        menu.append_item(&gio::MenuItem::new(
                            Some("Append to Playlist"),
                            Some("ml.append"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("Replace current playlist"),
                            Some("ml.replace"),
                        ));

                        // Only show View/Edit ID3 for single selection
                        if selected_count == 1 {
                            menu.append_item(&gio::MenuItem::new(
                                Some("View/Edit ID3 Info"),
                                Some("ml.edit-id3"),
                            ));
                        }

                        menu.append_item(&gio::MenuItem::new(
                            Some("Rescan Metadata"),
                            Some("ml.rescan"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("Calculate ReplayGain"),
                            Some("ml.calc-rg"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("Remove from Media Library"),
                            Some("ml.remove"),
                        ));

                        let send = build_send_to_menu(
                            &state_for_gest,
                            &SendToActions {
                                active: "ml.send-active",
                                new_playlist: "ml.add-to-new",
                                saved_playlist: "ml.add-to-saved",
                                drive: "ml.send-drive",
                                device: "ml.send-device",
                                drives: drives_for_gest.borrow().iter()
                                    .map(|d| (d.id.clone(), d.label.clone()))
                                    .collect(),
                                devices: devices_for_gest.borrow().iter()
                                    .map(|d| (d.id.clone(), d.label.clone()))
                                    .collect(),
                            },
                        );
                        menu.append_submenu(Some("Send to"), &send);

                        // Create popover menu — NESTED so the "Send to"
                        // submenu opens as its own popover with an
                        // independent height instead of sliding inside
                        // the parent popover (which would clip it to the
                        // parent's content height).
                        let popover = gtk4::PopoverMenu::from_model_full(
                            &menu,
                            gtk4::PopoverMenuFlags::NESTED,
                        );
                        popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(
                            popup_x as i32,
                            popup_y as i32,
                            1,
                            1,
                        )));
                        popover.set_parent(&col_popup);
                        popover.popup();
                        gest.set_state(gtk4::EventSequenceState::Claimed);
                    });
                    child.add_controller(gesture);
                    if li.child().is_none() {
                        li.set_child(Some(&child));
                    }
                });
                factory.connect_bind(move |_, obj| {
                    let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                    let boxed = li
                        .item()
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok());
                    let Some(boxed) = boxed else {
                        return;
                    };
                    let t = boxed.borrow::<crate::media_library::LibTrack>();

                    if is_artwork {
                        let btn = li.child().and_then(|c| c.downcast::<Button>().ok());
                        if let Some(btn) = btn {
                            let btn_obj = btn.clone().upcast::<glib::Object>();
                            if let Some(ref art_path) = t.artwork_path {
                                btn.set_visible(true);
                                btn.set_sensitive(true);
                                // Only connect once per button instance.
                                if !connected.borrow().contains(&btn_obj) {
                                    let art_clone = art_path.clone();
                                    connected.borrow_mut().insert(btn_obj.clone());
                                    btn.connect_clicked(move |_| {
                                        open_image_viewer(&art_clone);
                                    });
                                }

                                // Thumbnail: paint the cached PNG if it's
                                // already on disk; otherwise leave the cell
                                // blank and generate it off the main thread.
                                if let Some(img) = btn.child().and_then(|c| c.downcast::<Image>().ok()) {
                                    let src = PathBuf::from(art_path.as_str());
                                    if let Some(thumb) = crate::now_playing::thumb_path_for(
                                        &src,
                                        ML_ARTWORK_THUMB_PX as u32,
                                    ) {
                                        if thumb.exists() {
                                            img.set_from_file(Some(&thumb));
                                        } else {
                                            img.clear(); // don't show a stale/recycled thumbnail
                                            if inflight.borrow_mut().insert(src.clone()) {
                                                let inflight2 = inflight.clone();
                                                let img_wk = img.downgrade();
                                                let li_wk = li.downgrade();
                                                let src2 = src.clone();
                                                let thumb2 = thumb.clone();
                                                glib::spawn_future_local(async move {
                                                    let src_blk = src2.clone();
                                                    let thumb_blk = thumb2.clone();
                                                    let ok = gio::spawn_blocking(move || -> Result<(), ()> {
                                                        if let Some(parent) = thumb_blk.parent() {
                                                            let _ = std::fs::create_dir_all(parent);
                                                        }
                                                        let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(
                                                            &src_blk,
                                                            ML_ARTWORK_THUMB_PX,
                                                            ML_ARTWORK_THUMB_PX,
                                                            true,
                                                        )
                                                        .map_err(|_| ())?;
                                                        pixbuf.savev(&thumb_blk, "png", &[]).map_err(|_| ())
                                                    })
                                                    .await;

                                                    // Generation is done (success or not) — the
                                                    // source is no longer in flight either way.
                                                    inflight2.borrow_mut().remove(&src2);

                                                    if !matches!(ok, Ok(Ok(()))) {
                                                        return; // decode/encode failed — leave it blank
                                                    }

                                                    // ColumnView recycles cells: by the time the
                                                    // decode finished this row may have scrolled
                                                    // on to a different track. Only paint if the
                                                    // ListItem still shows the same artwork path.
                                                    let (Some(li), Some(img)) = (li_wk.upgrade(), img_wk.upgrade()) else {
                                                        return;
                                                    };
                                                    let still_same = li
                                                        .item()
                                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                                        .map(|b| {
                                                            let cur = b.borrow::<crate::media_library::LibTrack>();
                                                            cur.artwork_path.as_deref()
                                                                == src2.to_str()
                                                        })
                                                        .unwrap_or(false);
                                                    if still_same {
                                                        img.set_from_file(Some(&thumb2));
                                                    }
                                                });
                                            }
                                        }
                                    }
                                }
                            } else {
                                btn.set_visible(false);
                            }
                        }
                        return;
                    }

                    let lbl = li.child().and_then(|c| c.downcast::<Label>().ok());
                    let Some(lbl) = lbl else {
                        return;
                    };
                    let text = match id_str.as_str() {
                        "num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
                        "title" => t.title.as_deref().unwrap_or(&t.filename).to_string(),
                        "artist" => t.artist.as_deref().unwrap_or("").to_string(),
                        "album" => t.album.as_deref().unwrap_or("").to_string(),
                        "album_artist" => t.album_artist.as_deref().unwrap_or("").to_string(),
                        "duration" => t
                            .length_secs
                            .map(|s| {
                                let ss = s as u64;
                                format!("{}:{:02}", ss / 60, ss % 60)
                            })
                            .unwrap_or_else(|| "-:--".to_string()),
                        "filename" => t.filename.clone(),
                        "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
                        "genre" => t.genre.as_deref().unwrap_or("").to_string(),
                        "bitrate" => t.bitrate.map(|b| format!("{b}k")).unwrap_or_default(),
                        "channels" => match t.channels.unwrap_or(0) {
                            1 => "mono".to_string(),
                            2 => "stereo".to_string(),
                            n => format!("{}ch", n),
                        },
                        "path" => t.path.clone(),
                        "play_count" => t.play_count.to_string(),
                        "last_played" => format_last_played(t.last_played.as_deref().unwrap_or("")),
                        "last_scanned" => t.last_scanned.as_deref().unwrap_or("").to_string(),
                        "disc_num" => {
                            let d = t.disc_num.unwrap_or(0);
                            if d == 0 {
                                String::new()
                            } else if let Some(total) = t.disc_total {
                                if total > 0 {
                                    format!("{}/{}", d, total)
                                } else {
                                    d.to_string()
                                }
                            } else {
                                d.to_string()
                            }
                        }
                        "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
                        "composer" => t.composer.as_deref().unwrap_or("").to_string(),
                        "original_artist" => t.original_artist.as_deref().unwrap_or("").to_string(),
                        "copyright" => t.copyright.as_deref().unwrap_or("").to_string(),
                        "url" => t.url.as_deref().unwrap_or("").to_string(),
                        "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_string(),
                        "bpm" => t.bpm.as_deref().unwrap_or("").to_string(),
                        "lyric" => {
                            let ly = t.lyric.as_deref().unwrap_or("");
                            if ly.is_empty() {
                                String::new()
                            } else if ly.len() > 30 {
                                format!("{}…", &ly[..30])
                            } else {
                                ly.to_string()
                            }
                        }
                        "comment" => t.comment.as_deref().unwrap_or("").to_string(),
                        "artwork_path" => {
                            if t.artwork_path.is_some() {
                                "Yes".to_string()
                            } else {
                                String::new()
                            }
                        }
                        // Every column this match doesn't special-case falls
                        // through to the shared renderer — the phase-1 columns
                        // (filetype, sample rate, size, date added, mtime,
                        // mode) silently rendered blank here while the DB had
                        // the data, because `_ => String::new()` swallowed
                        // them (found in the phase-1 user pass).
                        other => ml_cell_text(&t, other),
                    };
                    lbl.set_text(&gtk_safe(&text));
                });

                let col = ColumnViewColumn::new(Some(header), Some(factory));
                col.set_resizable(true);
                if *expand {
                    col.set_expand(true);
                }
                col.set_visible(visible_ids.contains(&id.to_string()));
                if let Some(&w) = saved_widths.get(&id.to_string()) {
                    if w > 0 {
                        col.set_fixed_width(w);
                    }
                }

                let sort_id = id.to_string();
                let sorter = CustomSorter::new(move |a, b| {
                    let a_val = a
                        .downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id)
                        })
                        .unwrap_or_default();
                    let b_val = b
                        .downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id)
                        })
                        .unwrap_or_default();
                    a_val.cmp(&b_val).into()
                });
                col.set_sorter(Some(&sorter));

                col_view.append_column(&col);
                (id.to_string(), col)
            })
            .collect();
        let all_cols = Rc::new(all_cols);

        // Expose col_view and all_cols for close_request (outside this block scope).
        *col_view_holder.borrow_mut() = Some(col_view.clone());
        *all_cols_holder.borrow_mut() = all_cols.iter().cloned().collect();

        // Restore column order from config (empty list means use default order).
        // The unscanned indicator column is always first (position 0); named
        // columns start at position 1.
        {
            let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
            if !saved_order.is_empty() {
                // Remove all named columns from their current positions.
                for (_, col) in all_cols.iter() {
                    col_view.remove_column(col);
                }
                // Re-insert in saved order starting after the unscanned column.
                let mut pos = 1u32;
                for col_id in &saved_order {
                    if let Some((_, col)) = all_cols.iter().find(|(id, _)| id == col_id) {
                        col_view.insert_column(pos, col);
                        pos += 1;
                    }
                }
                // Append columns not present in saved_order (e.g. newly added columns).
                for (id, col) in all_cols.iter() {
                    if !saved_order.contains(id) {
                        col_view.insert_column(pos, col);
                        pos += 1;
                    }
                }
            }
        }

        let rebuild_files: Rc<dyn Fn() -> usize> = {
            let state_rc = state.clone();
            let store_ref = track_store.clone();
            let search_ref = search_entry.clone();
            Rc::new(move || {
                // Respect any active search filter so that background rebuilds
                // (rescan, folder add, ID3 save) don't discard the current query.
                let query = search_ref.text().to_lowercase();
                let tracks: Vec<crate::media_library::LibTrack> = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| {
                        if query.is_empty() {
                            lib.all_tracks().ok()
                        } else {
                            lib.search_tracks(&query).ok()
                        }
                    })
                    .unwrap_or_default();
                let count = tracks.len();
                let boxed: Vec<glib::BoxedAnyObject> =
                    tracks.into_iter().map(glib::BoxedAnyObject::new).collect();
                store_ref.splice(0, store_ref.n_items(), &boxed);
                count
            })
        };

        rebuild_files();
        sort_model.set_sorter(col_view.sorter().as_ref());

        let track_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Automatic)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .min_content_height(300)
            .child(&col_view)
            .build();
        files_vbox.append(&track_scroll);

        // Live search with 300ms debounce to avoid rebuilding on every keystroke.
        {
            let state_rc = state.clone();
            let store_ref = track_store.clone();
            let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
            search_entry.connect_changed(move |entry| {
                let query = entry.text().to_lowercase();
                // Cancel any pending search.
                if let Some(src) = pending.borrow_mut().take() {
                    src.remove();
                }
                // Schedule a new search after 300ms of inactivity.
                let state_inner = state_rc.clone();
                let store_inner = store_ref.clone();
                let pending_inner = pending.clone();
                let src =
                    glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                        let tracks: Vec<crate::media_library::LibTrack> = state_inner
                            .borrow()
                            .media_lib
                            .as_ref()
                            .and_then(|lib| {
                                if query.is_empty() {
                                    lib.all_tracks().ok()
                                } else {
                                    lib.search_tracks(&query).ok()
                                }
                            })
                            .unwrap_or_default();
                        let boxed: Vec<glib::BoxedAnyObject> =
                            tracks.into_iter().map(glib::BoxedAnyObject::new).collect();
                        store_inner.splice(0, store_inner.n_items(), &boxed);
                        pending_inner.borrow_mut().take();
                        glib::ControlFlow::Break
                    });
                *pending.borrow_mut() = Some(src);
            });
        }

        let files_status = Label::builder()
            .label("")
            .halign(Align::Start)
            .margin_start(6)
            .margin_end(6)
            .margin_bottom(2)
            .build();
        files_status.add_css_class("status-label");
        files_vbox.append(&files_status);
        *files_status_holder.borrow_mut() = Some(files_status.clone());

        // Button row.
        let btn_row = GtkBox::new(Orientation::Horizontal, 4);
        btn_row.set_margin_start(4);
        btn_row.set_margin_end(4);
        btn_row.set_margin_bottom(4);

        let btn_send_to = gtk4::MenuButton::builder()
            .label("Send to ▾")
            .build();
        btn_send_to.add_css_class("pl-btn");
        // Install "ml" directly on the button. Window-level alone enabled the
        // top-level items but the NESTED submenu popovers (Saved Playlist ▸,
        // Disc Drive ▸) resolve actions against the button's own popover
        // chain, so their items didn't dispatch until the group sits on the
        // button itself — the closest ancestor of every nested popover
        // (2026-07-16).
        btn_send_to.insert_action_group("ml", Some(&ml_action_group));
        let btn_customize = Button::with_label("⚙ Columns");
        btn_customize.add_css_class("pl-btn");
        let btn_add_folder = Button::with_label("+ Add Folder");
        btn_add_folder.add_css_class("pl-btn");
        let btn_rescan = Button::with_label("⟳ Rescan");
        btn_rescan.add_css_class("pl-btn");
        let btn_cancel = Button::with_label("✕ Cancel Scan");
        btn_cancel.add_css_class("pl-btn");
        btn_cancel.add_css_class("destructive");
        btn_cancel.set_visible(false);
        let btn_rm_from_ml = Button::with_label("✕ Remove");
        btn_rm_from_ml.add_css_class("pl-btn");
        btn_rm_from_ml.add_css_class("destructive");

        // Bulk ReplayGain analysis (missing-or-stale set only — the forced,
        // "analyze exactly this selection" variant lives in the row context
        // menu as "Calculate ReplayGain", ml.calc-rg). Disabled with a
        // tooltip when the `rganalysis` GStreamer element isn't installed —
        // silently-unavailable rather than an error dialog (house rule).
        let btn_analyze_rg = Button::with_label("Analyze ReplayGain");
        btn_analyze_rg.add_css_class("pl-btn");
        let rg_available = crate::replaygain::rg_analysis_available();
        if !rg_available {
            btn_analyze_rg.set_sensitive(false);
            btn_analyze_rg.set_tooltip_text(Some("rganalysis plugin not installed"));
        }
        let btn_cancel_rg = Button::with_label("✕ Cancel Analysis");
        btn_cancel_rg.add_css_class("pl-btn");
        btn_cancel_rg.add_css_class("destructive");
        btn_cancel_rg.set_visible(false);

        // Button row: send-to on the left, management buttons on the right.
        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        btn_row.append(&btn_send_to);
        btn_row.append(&spring);
        btn_row.append(&btn_rm_from_ml);
        btn_row.append(&btn_customize);
        btn_row.append(&btn_add_folder);
        btn_row.append(&btn_rescan);
        btn_row.append(&btn_cancel);
        btn_row.append(&btn_analyze_rg);
        btn_row.append(&btn_cancel_rg);
        files_vbox.append(&btn_row);

        // Add selected tracks to playlist.
        let add_selected: Rc<dyn Fn()> = {
            let state_rc = state.clone();
            let sel_ref = multi_sel.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track_add = set_track.clone();
            Rc::new(move || {
                let was_empty = state_rc.borrow().playlist.is_empty();
                let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                    == crate::config::PlaylistAddBehavior::Replace;
                if should_replace {
                    let _ = state_rc.borrow_mut().player.stop();
                    state_rc.borrow_mut().playlist.clear();
                }
                let mut added = 0usize;
                for i in 0..sel_ref.n_items() {
                    if sel_ref.is_selected(i) {
                        if let Some(obj) = sel_ref
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let t = obj.borrow::<crate::media_library::LibTrack>();
                            let track = crate::model::Track::from(&*t);
                            state_rc.borrow_mut().playlist.add(track);
                            added += 1;
                        }
                    }
                }
                if added > 0 {
                    // Autoplay when replacing (always start fresh) or when the
                    // playlist was empty and a track just arrived.
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_add(&display);
                        }
                    }
                    rebuild_pl();
                }
            })
        };

        // "Active Playlist" in the Send-to menu reuses this same logic.
        {
            let add = add_selected.clone();
            let action_send_active = gio::SimpleAction::new("send-active", None);
            action_send_active.connect_activate(move |_, _| {
                add();
            });
            ml_action_group.add_action(&action_send_active);
        }

        // Rebuild the Send-to menu model fresh on every open — drives/devices
        // may have come or gone. `set_create_popup_func` is invoked by GTK
        // right before the popover is shown; `connect_activate` does NOT fire
        // on a plain click, so the button appeared dead (2026-07-16).
        {
            let state_menu = state.clone();
            let current_drives = current_drives.clone();
            let current_devices = current_devices.clone();
            btn_send_to.set_create_popup_func(move |btn| {
                let menu = build_send_to_menu(
                    &state_menu,
                    &SendToActions {
                        active: "ml.send-active",
                        new_playlist: "ml.add-to-new",
                        saved_playlist: "ml.add-to-saved",
                        drive: "ml.send-drive",
                        device: "ml.send-device",
                        drives: current_drives.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        devices: current_devices.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    },
                );
                btn.set_menu_model(Some(&menu));
            });
        }

        // Double-click / Enter to add a single track.
        {
            let state_rc = state.clone();
            let sel_ref = multi_sel.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track_ml = set_track.clone();
            col_view.connect_activate(move |_, pos| {
                if let Some(obj) = sel_ref
                    .item(pos)
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                {
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                        == crate::config::PlaylistAddBehavior::Replace;
                    let t = obj.borrow::<crate::media_library::LibTrack>();
                    let track = crate::model::Track::from(&*t);
                    drop(t);
                    if should_replace {
                        // Stop before clearing so the current track doesn't
                        // keep playing after the playlist is replaced.
                        let _ = state_rc.borrow_mut().player.stop();
                        state_rc.borrow_mut().playlist.clear();
                    }
                    state_rc.borrow_mut().playlist.add(track);
                    // Autoplay when: the playlist was empty (append mode), or
                    // when replacing (the new track should always start playing).
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_ml(&display);
                        }
                    }
                    rebuild_pl();
                }
            });
        }

        // Customize columns dialog.
        {
            let state_rc = state.clone();
            let all_cols_rc = all_cols.clone();
            let cv_holder = col_view_holder.clone();
            let ac_holder = all_cols_holder.clone();
            let state_reorder = state.clone();
            let win_wk = win.downgrade();
            btn_customize.connect_clicked(move |_| {
                let cols_for_callback = all_cols_rc.clone();
                let cv_h = cv_holder.clone();
                let ac_h = ac_holder.clone();
                let st_r = state_reorder.clone();
                open_customize_columns_dialog(
                    win_wk.upgrade().as_ref(),
                    state_rc.clone(),
                    "Customize Columns",
                    ColumnCustomizerMode::MediaLibrary,
                    Some(Rc::new(move |id: String, visible: bool| {
                        if let Some((_, col)) =
                            cols_for_callback.iter().find(|(col_id, _)| col_id == &id)
                        {
                            col.set_visible(visible);
                        }
                    }) as Rc<dyn Fn(String, bool)>),
                    Some(Rc::new(move || {
                        let saved_order =
                            st_r.borrow().config.media_library.ml_file_col_order.clone();
                        if saved_order.is_empty() {
                            return;
                        }
                        let cv_opt = cv_h.borrow();
                        let all_cols = ac_h.borrow();
                        if let Some(col_view) = &*cv_opt {
                            for (_, col) in all_cols.iter() {
                                col_view.remove_column(col);
                            }
                            let mut pos = 1u32;
                            for col_id in &saved_order {
                                if let Some((_, col)) =
                                    all_cols.iter().find(|(id, _)| id == col_id)
                                {
                                    col_view.insert_column(pos, col);
                                    pos += 1;
                                }
                            }
                            for (id, col) in all_cols.iter() {
                                if !saved_order.contains(id) {
                                    col_view.insert_column(pos, col);
                                    pos += 1;
                                }
                            }
                        }
                    }) as Rc<dyn Fn()>),
                );
            });
        }

        // Add Folder handler.
        {
            let state_rc = state.clone();
            let win_wk = win.downgrade();
            let rebuild_ref = rebuild_files.clone();
            let status_ref = files_status.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            btn_add_folder.connect_clicked(move |_| {
                let chooser = gtk4::FileDialog::new();
                chooser.set_title("Add Folder to Media Library");
                let state_inner = state_rc.clone();
                let rebuild_inner = rebuild_ref.clone();
                let status_inner = status_ref.clone();
                let cancel_btn = cancel_ref.clone();
                let rescan_btn = rescan_ref.clone();
                if let Some(w) = win_wk.upgrade() {
                    chooser.select_folder(Some(&w), None::<&gio::Cancellable>, move |result| {
                        let Ok(file) = result else {
                            return;
                        };
                        let Some(folder) = file.path() else {
                            return;
                        };
                        let path_str = folder.to_string_lossy().to_string();

                        let db_path = {
                            let s = state_inner.borrow();
                            s.media_lib
                                .as_ref()
                                .map(|_| crate::media_library::MediaLibrary::db_path_pub())
                        };
                        let Some(db_path) = db_path else {
                            status_inner.set_text("Media library not available");
                            return;
                        };
                        // Refuse to start a second concurrent scan.
                        if state_inner.borrow().ml_scan.is_some() {
                            status_inner.set_text("Scan already in progress — please wait");
                            return;
                        }

                        // Set up scan state: shows cancel button and disables rescan.
                        let cancel_flag = start_ml_scan(&state_inner, ScanType::AddFolder, 0);
                        status_inner.set_text("Reading tags…");
                        cancel_btn.set_visible(true);
                        rescan_btn.set_sensitive(false);

                        // Three channels: fast done, metadata progress, final result.
                        let (fast_tx, fast_rx) =
                            std::sync::mpsc::channel::<Result<usize, String>>();
                        let (progress_tx, progress_rx) =
                            std::sync::mpsc::channel::<(usize, usize)>();
                        let (result_tx, result_rx) =
                            std::sync::mpsc::channel::<Result<usize, String>>();

                        let cancel_thread = cancel_flag.clone();
                        std::thread::spawn(move || {
                            let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                                Ok(l) => l,
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                    return;
                                }
                            };
                            let folder_id = match lib.add_folder(&path_str) {
                                Err(e) => {
                                    let _ = fast_tx
                                        .send(Err(format!("Could not add '{}': {e}", path_str)));
                                    return;
                                }
                                Ok(r) => r.id(),
                            };
                            // Phase 1: insert file paths into DB (fast).
                            if let Err(e) = lib.rescan_folder_fast(folder_id, &path_str) {
                                let _ = fast_tx
                                    .send(Err(format!("Scan error for '{}': {e}", path_str)));
                                return;
                            }
                            let _ = fast_tx.send(Ok(folder_id as usize));
                            // Phase 2: read metadata. Reset tracks with no metadata
                            // first so any missed by a prior scan are re-processed.
                            let _ = lib.reset_unscanned_metadata();
                            let count = lib
                                .scan_folder(folder_id, &cancel_thread, |c, t| {
                                    let _ = progress_tx.send((c, t));
                                })
                                .map(|(scanned, _, _)| scanned)
                                .unwrap_or(0);
                            let _ = result_tx.send(Ok(count));
                        });

                        let fast_rx = std::cell::RefCell::new(fast_rx);
                        let progress_rx = std::cell::RefCell::new(progress_rx);
                        let result_rx = std::cell::RefCell::new(result_rx);
                        let fast_handled = std::cell::Cell::new(false);
                        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                            // Handle fast scan completion — rebuild immediately so
                            // tracks appear in the library while metadata loads.
                            if !fast_handled.get() {
                                if let Ok(fast_result) = fast_rx.borrow().try_recv() {
                                    fast_handled.set(true);
                                    {
                                        let mut s = state_inner.borrow_mut();
                                        s.media_lib =
                                            crate::media_library::MediaLibrary::open().ok();
                                    }
                                    if let Err(e) = fast_result {
                                        status_inner.set_text(&e);
                                        complete_ml_scan(&state_inner);
                                        cancel_btn.set_visible(false);
                                        rescan_btn.set_sensitive(true);
                                        return glib::ControlFlow::Break;
                                    }
                                    rebuild_inner();
                                    status_inner.set_text("Reading tags…");
                                }
                            }

                            // Drain metadata progress updates.
                            while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                                update_ml_scan_progress(&state_inner, current, total);
                                status_inner
                                    .set_text(&format!("Reading tags {}/{}…", current, total));
                            }

                            // Check for final completion.
                            if let Ok(result) = result_rx.borrow().try_recv() {
                                {
                                    let mut s = state_inner.borrow_mut();
                                    s.media_lib = crate::media_library::MediaLibrary::open().ok();
                                }
                                complete_ml_scan(&state_inner);
                                match result {
                                    Err(e) => status_inner.set_text(&e),
                                    Ok(_) => {
                                        let count = rebuild_inner();
                                        status_inner
                                            .set_text(&format!("{count} tracks in library"));
                                    }
                                }
                                cancel_btn.set_visible(false);
                                rescan_btn.set_sensitive(true);
                                return glib::ControlFlow::Break;
                            }

                            glib::ControlFlow::Continue
                        });
                    });
                }
            });
        }

        // Rescan handler — runs in a background thread to avoid blocking the UI.
        {
            let state_rc = state.clone();
            let rebuild_ref = rebuild_files.clone();
            let status_ref = files_status.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            btn_rescan.connect_clicked(move |_| {
                let db_path = {
                    let s = state_rc.borrow();
                    match s.media_lib.as_ref() {
                        None => {
                            status_ref.set_text("Media library not available");
                            return;
                        }
                        Some(_) => crate::media_library::MediaLibrary::db_path_pub(),
                    }
                };

                let cancel_flag = start_ml_scan(&state_rc, ScanType::Rescan, 0);
                status_ref.set_text("Reading tags…");
                cancel_ref.set_visible(true);
                rescan_ref.set_sensitive(false);

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
                let rebuild_ref2 = rebuild_ref.clone();
                let status_ref2 = status_ref.clone();
                let cancel_ref2 = cancel_ref.clone();
                let rescan_ref2 = rescan_ref.clone();
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
                        match result {
                            Err(e) => status_ref2.set_text(&format!("Rescan error: {}", e)),
                            Ok(_) => {
                                let count = rebuild_ref2();
                                status_ref2.set_text(&format!("{count} tracks in library"));
                            }
                        }
                        cancel_ref2.set_visible(false);
                        rescan_ref2.set_sensitive(true);
                        return glib::ControlFlow::Break;
                    }

                    glib::ControlFlow::Continue
                });
            });
        }

        // Bulk "Analyze ReplayGain" handler — analyzes the missing-or-stale
        // set across the whole library (not just the current selection/
        // search filter). Shares `analyze_job` with the context action.
        {
            let state_rc = state.clone();
            let rebuild_ref = rebuild_files.clone();
            let status_ref = files_status.clone();
            btn_analyze_rg.connect_clicked(move |_| {
                if !crate::replaygain::rg_analysis_available() {
                    return; // button is disabled in this case; defensive only
                }
                let tracks: Vec<crate::media_library::LibTrack> = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.all_tracks().ok())
                    .unwrap_or_default();
                // `rebuild_files` returns the new row count (for the "N
                // tracks" search-result label); `analyze_job` just wants a
                // refresh signal, so discard it here.
                let rebuild_ref2 = rebuild_ref.clone();
                let rebuild: Rc<dyn Fn()> = Rc::new(move || {
                    rebuild_ref2();
                });
                analyze_job(&state_rc, tracks, false, &status_ref, rebuild);
            });
        }

        // Cancel scan handler
        {
            let state_rc = state.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            let status_ref = files_status.clone();
            btn_cancel.connect_clicked(move |_| {
                cancel_ml_scan(&state_rc);
                status_ref.set_text("Cancelling…");
                cancel_ref.set_visible(false);
                rescan_ref.set_sensitive(true);
            });
        }

        // Cancel ReplayGain analysis handler.
        {
            let state_rc = state.clone();
            let status_ref = files_status.clone();
            btn_cancel_rg.connect_clicked(move |_| {
                cancel_rg_job(&state_rc);
                status_ref.set_text("Cancelling…");
            });
        }

        // Polling timer to sync scan/analysis state with UI. Single timer
        // owns all these buttons + the shared status label so a metadata
        // scan (`ml_scan`) and an RG analysis job (`rg_job`) — which are
        // mutually exclusive, see `start_rg_job` — can't fight over the same
        // widgets from two independent tickers.
        {
            let state_rc = state.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            let add_folder_ref = btn_add_folder.clone();
            let analyze_ref = btn_analyze_rg.clone();
            let cancel_rg_ref = btn_cancel_rg.clone();
            let status_ref = files_status.clone();
            let rg_was_running = std::cell::Cell::new(false);
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                let scan_state = state_rc.borrow().ml_scan.clone();
                let scan_busy = scan_state.is_some();
                // RG buttons + status: shared with the Settings window via
                // `sync_rg_ui`. A running metadata scan owns the status label
                // this tick, so render_status = !scan_busy.
                let rg_running = sync_rg_ui(
                    &state_rc,
                    &analyze_ref,
                    &cancel_rg_ref,
                    &status_ref,
                    rg_available,
                    scan_busy,
                    !scan_busy,
                    rg_was_running.get(),
                );
                rg_was_running.set(rg_running);
                let busy = scan_busy || rg_running;
                rescan_ref.set_sensitive(!busy);
                add_folder_ref.set_sensitive(!busy);
                if let Some(scan) = scan_state {
                    cancel_ref.set_visible(true);
                    if scan.total > 0 {
                        status_ref
                            .set_text(&format!("Reading tags {}/{}…", scan.current, scan.total));
                    } else {
                        status_ref.set_text("Reading tags…");
                    }
                } else {
                    cancel_ref.set_visible(false);
                }
                glib::ControlFlow::Continue
            });
        }

        // Remove selected tracks from library.
        {
            let sel_ref = multi_sel.clone();
            let store_ref = track_store.clone();
            let status_ref = files_status.clone();
            btn_rm_from_ml.connect_clicked(move |_| {
                // Collect IDs of every selected item in one pass.
                let mut ids_vec: Vec<i64> = Vec::new();
                for i in 0..sel_ref.n_items() {
                    if sel_ref.is_selected(i) {
                        if let Some(obj) = sel_ref
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            ids_vec.push(obj.borrow::<crate::media_library::LibTrack>().id);
                        }
                    }
                }
                if ids_vec.is_empty() {
                    return;
                }
                let ids_set: std::collections::HashSet<i64> =
                    ids_vec.iter().copied().collect();
                let n_items = store_ref.n_items();

                // Build the kept list and splice in one shot — a single
                // items-changed signal instead of one per removed row.
                // This is the same pattern used by rebuild_files/search and
                // avoids blocking the main thread on large selections.
                let kept: Vec<glib::Object> = (0..n_items)
                    .filter_map(|i| store_ref.item(i))
                    .filter(|obj| {
                        obj.downcast_ref::<glib::BoxedAnyObject>()
                            .map(|b| !ids_set.contains(
                                &b.borrow::<crate::media_library::LibTrack>().id,
                            ))
                            .unwrap_or(true)
                    })
                    .collect();
                let removed = n_items as usize - kept.len();
                store_ref.splice(0, n_items, &kept);

                status_ref.set_text(&format!(
                    "Removed {removed} track{}. {} tracks in library",
                    if removed == 1 { "" } else { "s" },
                    kept.len(),
                ));

                // Soft-delete in background, then purge — same pattern as
                // folder removal.  Opens its own DB connection because
                // rusqlite::Connection is not Send.
                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                std::thread::spawn(move || {
                    if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                        let _ = lib.soft_delete_tracks(&ids_vec);
                        let _ = lib.purge_deleted_tracks();
                    }
                });
            });
        }

        stack.add_named(&files_vbox, Some("files"));
        let rf = rebuild_files.clone();
        state.borrow_mut().rebuild_ml_callback = Some(Rc::new(move || {
            rf();
        }));
    }

    // ── Page: Playlists ──────────────────────────────────────────────────
    //
    // Two sub-pages within the "playlists" stack page:
    //   "pl-manage" – full-width list of saved playlists + New/Rename/Delete
    //   "pl-edit"   – track editor for the selected playlist
    //
    // pl_sub_stack is stored in an Rc so the sidebar wiring can switch pages.
    let pl_sub_stack: Rc<Stack> = Rc::new({
        let s = Stack::new();
        s.set_hexpand(true);
        s.set_vexpand(true);
        s.set_transition_type(StackTransitionType::None);
        s
    });

    // Shared: currently-editing playlist id and LibTrack list
    let editing_tracks: Rc<RefCell<Vec<crate::media_library::LibTrack>>> =
        Rc::new(RefCell::new(Vec::new()));
    let saved_track_ids: Rc<RefCell<Vec<i64>>> = Rc::new(RefCell::new(Vec::new()));
    // The DB row id of the playlist currently open in the editor (-1 = none)
    let editing_pl_id: Rc<Cell<i64>> = Rc::new(Cell::new(-1));

    // "Send to" and row-scoped actions for the editor's per-cell context
    // menu. Task 8 originally built this as a flat Popover of plain Buttons
    // because a PopoverMenu parented on the ColumnView cell tree lost
    // dispatch. The Files-view menu (~col_view, "ml" prefix) proves the
    // real fix: put the SimpleActionGroup on the SAME stable widget the
    // PopoverMenu is parented to (single widget, no ancestor walk) instead
    // of scattering it across track_list/win as the abandoned "ple" group
    // above did. Here that stable widget is the editor's ScrolledWindow
    // (`track_scroll`, exposed via `track_scroll_holder` since it doesn't
    // exist yet at this point in the function) — see its
    // `insert_action_group("ed", ...)` call right after it's built. The
    // group is *also* inserted directly on each popped-up PopoverMenu
    // instance (see the per-cell gesture) as defense in depth: the
    // `ple_action_group_holder` comment above documents a GTK4 version
    // where the NESTED PopoverMenu flag breaks the ancestor-chain walk
    // entirely, and installing the group straight on the popup sidesteps
    // that regardless of GTK version.
    // Canonical play-order indices (selection, or the single clicked row
    // as fallback) captured once per right-click so every "ed.*" action —
    // not just send-drive/send-device — can read row-scoped context
    // without needing per-item closures. Still used by the row-scoped
    // "Replace Current Playlist" / "Edit ID3" / "Remove" items, which are
    // right-click-only (never exposed on the "Send to ▾" button below).
    let ed_ctx_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    // Live "currently selected editor rows" reader for the actions that
    // ARE exposed on the "Send to ▾" button (send-active, add-to-new,
    // add-to-saved, send-drive, send-device): the button doesn't go
    // through the per-cell right-click gesture, so it must read the
    // editor's own MultiSelection directly at dispatch time instead of a
    // stash the gesture populates (same G1 fix as the files view's "Send
    // to ▾" button). `edit_multi_sel` doesn't exist yet at this point in
    // the function, so a holder defers the actual model until it's built
    // below (filled in right after `edit_multi_sel` is constructed).
    let edit_multi_sel_holder: Rc<RefCell<Option<gtk4::MultiSelection>>> =
        Rc::new(RefCell::new(None));
    let ed_selected_tracks: Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>> = {
        let sel_holder = edit_multi_sel_holder.clone();
        Rc::new(move || {
            let Some(sel) = sel_holder.borrow().clone() else { return Vec::new() };
            let mut out = Vec::new();
            for i in 0..sel.n_items() {
                if sel.is_selected(i) {
                    if let Some(obj) = sel
                        .item(i)
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    {
                        out.push(obj.borrow::<EditorEntry>().track.clone());
                    }
                }
            }
            out
        })
    };
    // Quiet status line (G3) — the editor's Send to Disc Drive reports
    // here instead of dropping the success message on the floor.
    let ed_status = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(6)
        .margin_end(6)
        .margin_bottom(2)
        .build();
    ed_status.add_css_class("status-label");
    let ed_action_group = gio::SimpleActionGroup::new();
    // Kept as named bindings (not scoped to a block) so later code in this
    // function (the "ed"-group insertion on track_scroll, and the
    // additional actions registered further down once
    // rebuild_track_list_holder/track_scroll_holder exist) can add more
    // actions to the same group.
    let ed_action_drive = gio::SimpleAction::new("send-drive", Some(glib::VariantTy::STRING));
    let ed_action_device = gio::SimpleAction::new("send-device", Some(glib::VariantTy::STRING));
    {
        let state_burn = state.clone();
        let burn_queues = burn_queues.clone();
        let burn_refresh_holder = burn_refresh_holder.clone();
        let current_drives = current_drives.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let win_wk = win.downgrade();
        let status = ed_status.clone();
        ed_action_drive.connect_activate(move |_, target| {
            let Some(drive_id) = target.and_then(|v| v.get::<String>()) else { return };
            let drive_label = current_drives
                .borrow()
                .iter()
                .find(|d| d.id == drive_id)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| drive_id.clone());
            // Live selection at dispatch (G1) — read straight from the
            // editor's MultiSelection, not a right-click stash, so the
            // "Send to ▾" button sees the actual current selection.
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter().map(|t| std::path::PathBuf::from(&t.path)).collect();
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
        ed_action_group.add_action(&ed_action_drive);
    }
    {
        // Send to Removable Device: hand off to the Files view's copy
        // runner via the shared holder (populated once the Device view's
        // widgets exist — see copy_files_holder's own doc comment).
        let current_devices = current_devices.clone();
        let copy_files_holder = copy_files_holder.clone();
        let sel_tracks = ed_selected_tracks.clone();
        ed_action_device.connect_activate(move |_, target| {
            let Some(dev_id) = target.and_then(|v| v.get::<String>()) else { return };
            let dev = current_devices
                .borrow()
                .iter()
                .find(|d| d.id == dev_id)
                .cloned();
            // Live selection at dispatch (G1).
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter().map(|t| std::path::PathBuf::from(&t.path)).collect();
            if let (Some(dev), false) = (dev, paths.is_empty()) {
                if let Some(run) = copy_files_holder.borrow().clone() {
                    run(dev, paths);
                }
            }
        });
        ed_action_group.add_action(&ed_action_device);
    }
    win.insert_action_group("ed", Some(&ed_action_group));

    // Widget handles for pl-manage playlist list (shared with sidebar)
    let pl_manage_list: Rc<ListBox> = Rc::new({
        let lb = ListBox::new();
        lb.add_css_class("playlist");
        lb.set_selection_mode(gtk4::SelectionMode::Single);
        lb.set_vexpand(true);
        lb
    });

    // Canonical play-order index of the row most recently right-clicked
    // in the editor; the ple.edit-id3 / ple.remove actions read this when
    // they need a single row to operate on.  Used instead of LibTrack.id
    // so duplicate entries (same track listed several times in the
    // playlist file) can be disambiguated by position.
    let ctx_canonical_idx: Rc<Cell<i64>> = Rc::new(Cell::new(-1));

    // Canonical play-order indices selected for an in-progress drag from
    // the editor.  Populated by the per-cell DragSource at prepare time
    // and consumed by the editor DropTarget when handling a reorder.
    // Cleared on every new drag prepare so a previous drag's selection
    // can't leak into a subsequent unrelated drop.
    let drag_selection: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Path → first canonical slot.  Used by the editor DropTarget when a
    // cross-window drop ships only paths (no canonical indices) and we
    // need to know whether every dropped path is already in the playlist.
    // For duplicates only the first slot is recorded; the drag_selection
    // path is preferred when the drag originated in the editor itself.
    let position_map: Rc<RefCell<std::collections::HashMap<String, usize>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // Wrapper put into the editor's ListStore.  Carrying `canonical_idx`
    // alongside the track lets every cell — even duplicates of the same
    // file in the playlist — bind to its own play-order slot, so the
    // position column reads the correct row instead of all duplicates
    // collapsing onto the last occurrence's index.  Cloned cheaply on
    // splice because `LibTrack` is `Clone` already.
    #[derive(Clone)]
    struct EditorEntry {
        track: crate::media_library::LibTrack,
        canonical_idx: usize,
    }

    // True when the editor's current display sort allows intra-list drag
    // reorder (only the canonical play-order ascending state preserves the
    // bijection between display index and play-order index).  Flipped by
    // a sorter-change handler installed once the ColumnView exists.
    let reorder_allowed: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    // Track editor: ListStore → SortListModel → MultiSelection → ColumnView.
    // Sort lives in the SortListModel so the user's column-header clicks
    // produce a display-only sort.  `editing_tracks` (the canonical play
    // order) is never reordered by sort — Save always writes that order.
    let edit_store: gio::ListStore = gio::ListStore::new::<glib::BoxedAnyObject>();
    // Per-view search over this playlist's rows: store → filter → sort →
    // selection. Rows keep their canonical_idx, so delete/context actions
    // stay correct under a filter; drag-reorder is refused while one is
    // active (display order no longer maps onto play order).
    let pl_edit_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let edit_filter = gtk4::CustomFilter::new({
        let q = pl_edit_query.clone();
        move |obj| {
            let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                return true;
            };
            lib_track_matches_query(&boxed.borrow::<EditorEntry>().track, &q.borrow())
        }
    });
    let edit_filter_model =
        gtk4::FilterListModel::new(Some(edit_store.clone()), Some(edit_filter.clone()));
    // Search filters just this playlist's rows (drag-reorder pauses while a
    // query is active — see the drop handler). Created here so
    // load_pl_by_id can clear it when a different playlist opens; packed
    // into the pl-edit page below.
    let (pl_search_row, pl_search_entry) =
        make_view_search_row("Search this playlist — artist, title, album…");
    {
        // 150 ms debounce — same rationale as the device search: the filter
        // walks every row per change, heavy on multi-thousand-row playlists.
        let q = pl_edit_query.clone();
        let filter = edit_filter.clone();
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        pl_search_entry.connect_changed(move |e| {
            let text = e.text().to_lowercase();
            if let Some(src) = pending.borrow_mut().take() {
                src.remove();
            }
            let q = q.clone();
            let filter = filter.clone();
            let pending_inner = pending.clone();
            let src = glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                *q.borrow_mut() = text.clone();
                filter.changed(gtk4::FilterChange::Different);
                pending_inner.borrow_mut().take();
                glib::ControlFlow::Break
            });
            *pending.borrow_mut() = Some(src);
        });
    }
    let edit_sort_model = gtk4::SortListModel::new(
        Some(edit_filter_model),
        None::<gtk4::Sorter>,
    );
    let edit_multi_sel: gtk4::MultiSelection =
        gtk4::MultiSelection::new(Some(edit_sort_model.clone()));
    // Fill the deferred holder now that the real model exists — see its
    // declaration above (`edit_multi_sel_holder`) for why this is deferred.
    *edit_multi_sel_holder.borrow_mut() = Some(edit_multi_sel.clone());
    let track_list: Rc<gtk4::ColumnView> = Rc::new({
        let cv = gtk4::ColumnView::new(Some(edit_multi_sel.clone()));
        cv.add_css_class("playlist");
        cv.set_vexpand(true);
        cv.set_show_row_separators(false);
        cv.set_show_column_separators(false);
        cv
    });

    // ── Editor columns: walk ALL_COLUMNS so files view + editor stay in
    //    lock-step on which columns exist and which order they default to.
    // Position column reference is captured here so the sorter-change
    // listener below can detect when the user has selected position-ASC
    // (the only sort that allows intra-list drag-reorder).
    let pos_col_holder: Rc<RefCell<Option<ColumnViewColumn>>> = Rc::new(RefCell::new(None));
    // Editor named columns (skipping the leading status + position pinned
    // columns) — captured so we can apply the files-view saved order so
    // the user only has to arrange columns in one place.
    let mut editor_named_cols: Vec<(String, ColumnViewColumn)> = Vec::new();
    // Holder for the rebuild closure — populated right after the closure
    // is defined.  Cell factories install per-cell drop targets that need
    // to refresh the editor after a successful reorder, but those factory
    // setups live above the rebuild definition in source order.
    type RebuildClosure = Rc<dyn Fn()>;
    let rebuild_track_list_holder: Rc<RefCell<Option<RebuildClosure>>> =
        Rc::new(RefCell::new(None));

    // Holder for the editor's "ple" action group.  Cell factories pop
    // PopoverMenus parented to track_list; the popover's action lookup
    // walks the GTK widget chain back to track_list where the group is
    // also attached, but some GTK4 versions break that walk with the
    // NESTED PopoverMenu flag.  Installing the group directly on each
    // popup makes dispatch reliable regardless of GTK version.
    let ple_action_group_holder: Rc<RefCell<Option<gio::SimpleActionGroup>>> =
        Rc::new(RefCell::new(None));
    // Holder for the editor's ScrolledWindow — populated right after it
    // is built so the cell right-click handler can use it as the popover
    // parent (cell-label parents render invisible on this GTK4 build), and
    // so the "ed" action group can be installed on it once (see below).
    let track_scroll_holder: Rc<RefCell<Option<gtk4::ScrolledWindow>>> =
        Rc::new(RefCell::new(None));

    // Row-scoped "ed" actions for the per-cell context menu's non-Send-to
    // items (Replace Current Playlist, Edit/View ID3, Remove from
    // Playlist) plus the Send-to family's Active Playlist / Saved
    // Playlist entries (send-drive/send-device were already registered
    // above). Registered here — after rebuild_track_list_holder and
    // track_scroll_holder exist — because `remove` needs the rebuild
    // closure holder. All read row context from `ed_ctx_indices`
    // (selection, falling back to the single clicked row), populated once
    // per right-click by the per-cell gesture below instead of being
    // recomputed per menu item.
    {
        // Send to Active Playlist — reachable from both the per-row
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_c = set_track.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let action = gio::SimpleAction::new("send-active", None);
        action.connect_activate(move |_, _| {
            let tracks = sel_tracks();
            if tracks.is_empty() { return }
            let was_empty = state_c.borrow().playlist.is_empty();
            let autoplay = state_c.borrow().config.behavior.autoplay_on_add;
            {
                let mut s = state_c.borrow_mut();
                for lt in &tracks { s.playlist.add(crate::model::Track::from(lt)); }
            }
            if autoplay && was_empty {
                if let Some(d) = state_c.borrow_mut().play_current() { set_track_c(&d); }
            }
            rebuild_pl();
        });
        ed_action_group.add_action(&action);
    }
    {
        // Replace Current Playlist — same body as the old flat button.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_c = set_track.clone();
        let idxs_src = ed_ctx_indices.clone();
        let action = gio::SimpleAction::new("replace", None);
        action.connect_activate(move |_, _| {
            let tracks: Vec<crate::media_library::LibTrack> = {
                let et_b = et.borrow();
                idxs_src.borrow().iter().filter_map(|&i| et_b.get(i).cloned()).collect()
            };
            if tracks.is_empty() { return }
            let autoplay = state_c.borrow().config.behavior.autoplay_on_add;
            {
                let mut s = state_c.borrow_mut();
                let _ = s.player.stop();
                s.playlist = crate::model::Playlist::new();
                for lt in &tracks { s.playlist.add(crate::model::Track::from(lt)); }
            }
            if autoplay {
                if let Some(d) = state_c.borrow_mut().play_current() { set_track_c(&d); }
            }
            rebuild_pl();
        });
        ed_action_group.add_action(&action);
    }
    {
        // Edit / View ID3 — single-row only, reads the clicked row (not
        // the selection) via ctx_canonical_idx, same as the old flat button.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let ctx_c = ctx_canonical_idx.clone();
        let action = gio::SimpleAction::new("edit-id3", None);
        action.connect_activate(move |_, _| {
            let c = ctx_c.get();
            if c < 0 { return }
            let path = et.borrow().get(c as usize).map(|t| t.path.clone());
            let Some(path) = path else { return };
            open_id3_editor_window(
                None::<&gtk4::Window>,
                path.into(),
                state_c.clone(),
                rebuild_pl.clone(),
                None,
            );
        });
        ed_action_group.add_action(&action);
    }
    {
        // Remove from Playlist — same body as the old flat button.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let ep_id = editing_pl_id.clone();
        let rb_holder = rebuild_track_list_holder.clone();
        let idxs_src = ed_ctx_indices.clone();
        let action = gio::SimpleAction::new("remove", None);
        action.connect_activate(move |_, _| {
            let mut idxs = idxs_src.borrow().clone();
            if idxs.is_empty() { return }
            idxs.sort_unstable_by(|a, b| b.cmp(a));
            {
                let mut e = et.borrow_mut();
                for i in idxs.iter() {
                    if *i < e.len() { e.remove(*i); }
                }
            }
            let pid = ep_id.get();
            if pid >= 0 {
                let s = state_c.borrow();
                if let Some(lib) = s.media_lib.as_ref() {
                    let paths: Vec<String> = et.borrow()
                        .iter().map(|t| t.path.clone()).collect();
                    if let Ok(pl) = lib.playlist_by_id(pid) {
                        let _ = lib.save_playlist_tracks_to_path(
                            std::path::Path::new(&pl.path),
                            &paths,
                        );
                    }
                }
            }
            if let Some(rb) = rb_holder.borrow().as_ref() { rb(); }
        });
        ed_action_group.add_action(&action);
    }
    {
        // Seed a brand new saved playlist — reachable from both the
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let win_c = win.clone();
        let action = gio::SimpleAction::new("add-to-new", None);
        action.connect_activate(move |_, _| {
            let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
            if paths.is_empty() { return }
            let default_stem = glib::DateTime::now_local().ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = state_c.clone();
            let paths_cb = paths.clone();
            run_playlist_save_dialog(
                state_c.clone(),
                win_c.clone(),
                &default_stem,
                move |path, win_cb| {
                    if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                        }
                    }
                },
            );
        });
        ed_action_group.add_action(&action);
    }
    {
        // Append to an existing saved playlist — reachable from both the
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let action = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
            if paths.is_empty() { return }
            let mut ok = false;
            if let Some(lib) = state_c.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_) => ok = true,
                    Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                }
            }
            if ok { notify_playlist_changed(pid); }
        });
        ed_action_group.add_action(&action);
    }
    {
        let visible_ids: Vec<String> =
            state.borrow().config.media_library.visible_columns.clone();
        let saved_widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();

        // Leading status-glyph column (⚠/🔒) — playlist-editor-only, mirrors
        // the unscanned-indicator column on the files side.
        {
            let factory = gtk4::SignalListItemFactory::new();
            factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                let lbl = Label::builder()
                    .halign(Align::Center)
                    .valign(Align::Center)
                    .build();
                li.set_child(Some(&lbl));
            });
            factory.connect_bind(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let t = &entry.track;
                let path = std::path::Path::new(&t.path);
                // Missing == the file is gone, mirroring the macOS/FFI
                // `file_missing` flag. `id == 0` only means "not catalogued";
                // an uncatalogued file that exists is a normal playable track.
                let missing  = !path.exists();
                let readonly = !missing && crate::media_library::is_read_only(path);
                let glyph = if missing { "⚠" } else if readonly { "🔒" } else { "" };
                if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                    lbl.set_label(glyph);
                }
            });
            let col = ColumnViewColumn::new(Some(""), Some(factory));
            col.set_fixed_width(24);
            track_list.append_column(&col);
        }

        // Position column (editor-only) — shows the 1-based playlist slot
        // resolved against the canonical play order in `editing_tracks`.
        // Pinned: fixed width, no resize/reorder.  Sorter is installed
        // below so clicking the header toggles position ASC/DESC.
        {
            let pos_factory = gtk4::SignalListItemFactory::new();
            pos_factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                let lbl = Label::builder()
                    .halign(Align::End)
                    .xalign(1.0)
                    .margin_start(6).margin_end(6)
                    .css_classes(["pl-duration"])
                    .build();
                li.set_child(Some(&lbl));
            });
            pos_factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let text = (entry.canonical_idx + 1).to_string();
                if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                    lbl.set_label(&text);
                }
            });
            let pos_col = ColumnViewColumn::new(Some("#"), Some(pos_factory));
            pos_col.set_fixed_width(48);
            pos_col.set_resizable(false);
            // Canonical-order sorter: compare each entry's slot directly.
            let sorter = CustomSorter::new(move |a, b| {
                let pa = a.downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .unwrap_or(usize::MAX);
                let pb = b.downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .unwrap_or(usize::MAX);
                pa.cmp(&pb).into()
            });
            pos_col.set_sorter(Some(&sorter));
            track_list.append_column(&pos_col);
            *pos_col_holder.borrow_mut() = Some(pos_col);
        }

        for c in ALL_COLUMNS.iter() {
            let id_str = c.id.to_string();
            let factory = gtk4::SignalListItemFactory::new();

            let setup_sel        = edit_multi_sel.clone();
            let setup_state      = state.clone();
            let setup_ctx_id     = ctx_canonical_idx.clone();
            let setup_et         = editing_tracks.clone();
            let setup_ep_id      = editing_pl_id.clone();
            let setup_drag_sel   = drag_selection.clone();
            let setup_ra         = reorder_allowed.clone();
            // rebuild_track_list isn't yet defined at this point of the
            // outer scope, so capture the Rc via a deferred holder filled
            // immediately after the rebuild closure is created.
            let setup_rebuild    = rebuild_track_list_holder.clone();
            let setup_scroll     = track_scroll_holder.clone();
            let setup_ed_ctx_idx    = ed_ctx_indices.clone();
            let setup_drives     = current_drives.clone();
            let setup_devices    = current_devices.clone();
            let setup_id         = id_str.clone();
            let is_artwork_col   = id_str == "artwork_path";
            factory.connect_setup(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                // Artwork column gets a "View" Button instead of a Label —
                // matches the files view affordance.  Drag-source / drop-
                // target / right-click gesture attach to the Button just
                // like they would to a Label (both are Widget).
                let child: gtk4::Widget = if setup_id == "artwork_path" {
                    let btn = Button::with_label("View");
                    btn.add_css_class("link");
                    btn.set_margin_start(4);
                    btn.set_margin_end(4);
                    btn.set_halign(Align::Start);
                    btn.set_visible(false);
                    btn.upcast::<gtk4::Widget>()
                } else {
                    let lbl = Label::builder()
                        .margin_start(6).margin_end(6)
                        .margin_top(3).margin_bottom(3)
                        .hexpand(true).vexpand(true)
                        .halign(Align::Fill).valign(Align::Fill)
                        .xalign(0.0)
                        .ellipsize(gtk4::pango::EllipsizeMode::End)
                        .build();
                    lbl.upcast::<gtk4::Widget>()
                };
                let lbl = child.clone();
                let _ = is_artwork_col;

                // Per-cell DropTarget — handles intra-editor reorder.  When
                // the source drag originated in the editor (drag_selection
                // populated) and the current sort allows reorder, splice
                // those canonical rows to this cell's canonical slot.
                // Drops from other windows (drag_selection empty) fall
                // through to the outer track_scroll DropTarget which
                // appends the external paths.
                {
                    let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
                    let dt_li      = li.clone();
                    let dt_et      = setup_et.clone();
                    let dt_state   = setup_state.clone();
                    let dt_ep_id   = setup_ep_id.clone();
                    let dt_ra      = setup_ra.clone();
                    let dt_dragsel = setup_drag_sel.clone();
                    let dt_rebuild = setup_rebuild.clone();
                    dt.connect_drop(move |_, value, _, _| {
                        if !dt_ra.get() { return false }
                        // Reject the drop unless the drag originated in
                        // the editor itself — otherwise let the outer
                        // track_scroll DropTarget handle external add.
                        let src_indices: Vec<usize> = dt_dragsel.borrow().clone();
                        if src_indices.is_empty() { return false }
                        // Validate we still received the expected number
                        // of paths (sanity check; not used for indices).
                        if value.get::<gdk::FileList>().is_err() { return false }

                        // Resolve drop slot directly from this cell's
                        // EditorEntry so duplicate paths in the playlist
                        // collapse to the correct row, not the first one.
                        let Some(dst_canon) = dt_li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        else { return false };

                        // Splice in canonical order: remove src indices
                        // highest-first, then re-insert in original order
                        // at the adjusted destination.
                        let mut sorted = src_indices.clone();
                        sorted.sort_unstable_by(|a, b| b.cmp(a));
                        let mut adjusted_dst = dst_canon;
                        let mut removed: Vec<crate::media_library::LibTrack> = Vec::new();
                        {
                            let mut et = dt_et.borrow_mut();
                            for src in sorted.iter() {
                                if *src < et.len() {
                                    let t = et.remove(*src);
                                    if *src < adjusted_dst { adjusted_dst -= 1; }
                                    removed.push(t);
                                }
                            }
                            removed.reverse();
                            let cap = et.len();
                            let insert_at = adjusted_dst.min(cap);
                            for (i, t) in removed.into_iter().enumerate() {
                                et.insert(insert_at + i, t);
                            }
                        }

                        // Persist canonical order through the library so
                        // the on-disk M3U8 reflects the reorder immediately.
                        // Rewrites the existing playlist file in place;
                        // `add_playlist_file` upserts the row so registering
                        // the same path again is a no-op.
                        let pid = dt_ep_id.get();
                        if pid >= 0 {
                            let s = dt_state.borrow();
                            if let Some(lib) = s.media_lib.as_ref() {
                                let paths: Vec<String> = dt_et.borrow()
                                    .iter().map(|t| t.path.clone()).collect();
                                if let Ok(pl) = lib.playlist_by_id(pid) {
                                    if let Err(e) = lib.save_playlist_tracks_to_path(
                                        std::path::Path::new(&pl.path),
                                        &paths,
                                    ) {
                                        eprintln!("editor reorder persist {pid}: {e}");
                                    }
                                }
                            }
                        }

                        // Drag completed — clear selection so a stray
                        // subsequent drop (e.g. external) doesn't reorder.
                        dt_dragsel.borrow_mut().clear();

                        // Defer rebuild to next idle tick so we don't
                        // splice the backing ListStore while GTK is still
                        // unwinding the drop event chain — splicing mid-
                        // drop segfaults on some GTK4 versions.
                        if let Some(rb) = dt_rebuild.borrow().as_ref().cloned() {
                            glib::idle_add_local_once(move || rb());
                        }
                        true
                    });
                    lbl.add_controller(dt);
                }

                // Per-cell DragSource — ships every currently-selected editor
                // row as a FileList so the user can drag tracks out of the
                // playlist editor into the active playlist (pl_scroll accepts
                // FileList).  Single-row drag works too: if the row under
                // the pointer is not in the selection it still ships its
                // own path.
                {
                    let ds = gtk4::DragSource::new();
                    ds.set_actions(gtk4::gdk::DragAction::COPY);
                    let ds_sel       = setup_sel.clone();
                    let ds_li        = li.clone();
                    let ds_dragsel   = setup_drag_sel.clone();
                    ds.connect_prepare(move |_, _, _| {
                        // Clear any stale canonical indices from a prior
                        // drag, then record this drag's selection by
                        // canonical_idx so duplicates of the same path
                        // resolve to the correct rows on reorder.
                        ds_dragsel.borrow_mut().clear();
                        let mut paths: Vec<std::path::PathBuf> = Vec::new();
                        let mut indices: Vec<usize> = Vec::new();
                        let mut self_entry: Option<(std::path::PathBuf, usize)> = None;
                        if let Some(obj) = ds_li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let entry = obj.borrow::<EditorEntry>();
                            self_entry = Some((
                                std::path::PathBuf::from(&entry.track.path),
                                entry.canonical_idx,
                            ));
                        }
                        for i in 0..ds_sel.n_items() {
                            if ds_sel.is_selected(i) {
                                if let Some(obj) = ds_sel.item(i)
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                {
                                    let entry = obj.borrow::<EditorEntry>();
                                    paths.push(std::path::PathBuf::from(&entry.track.path));
                                    indices.push(entry.canonical_idx);
                                }
                            }
                        }
                        if paths.is_empty() {
                            if let Some((p, i)) = self_entry {
                                paths.push(p);
                                indices.push(i);
                            }
                        }
                        if paths.is_empty() { return None }
                        *ds_dragsel.borrow_mut() = indices;
                        let files: Vec<gio::File> = paths.iter()
                            .map(|p| gio::File::for_path(p))
                            .collect();
                        let fl = gdk::FileList::from_array(&files);
                        Some(gdk::ContentProvider::for_value(&fl.to_value()))
                    });
                    lbl.add_controller(ds);
                }

                // Per-cell right-click gesture. Builds a real gio::Menu +
                // PopoverMenu (NESTED), same as the Files view and the
                // device-tracks view — see the big comment above
                // `ed_ctx_indices`/`ed_selected_tracks` for why this now works
                // where Task 8's flat-button popover was a workaround: the
                // "ed" action group lives on `track_scroll` (installed once,
                // not per-cell) and the popover is parented on that same
                // widget, so action lookup never has to walk the ColumnView
                // cell tree at all.
                let gesture = gtk4::GestureClick::new();
                gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
                let g_sel        = setup_sel.clone();
                let g_state      = setup_state.clone();
                let g_ctx_id     = setup_ctx_id.clone();
                let g_li         = li.clone();
                let g_lbl        = lbl.clone();
                let g_scroll     = setup_scroll.clone();
                let g_ed_ctx_idx = setup_ed_ctx_idx.clone();
                let g_drives     = setup_drives.clone();
                let g_devices    = setup_devices.clone();
                gesture.connect_pressed(move |g, _n, x, y| {
                    let Some(scroll_widget) = g_scroll.borrow().clone() else {
                        return;
                    };
                    let Some(item) = g_li.item() else {
                        return;
                    };
                    let item_clone = item.clone();
                    let mut clicked_idx: Option<u32> = None;
                    for i in 0..g_sel.n_items() {
                        if g_sel.item(i).as_ref() == Some(&item_clone) {
                            clicked_idx = Some(i);
                            break;
                        }
                    }
                    if let Some(idx) = clicked_idx {
                        if !g_sel.is_selected(idx) {
                            g_sel.unselect_all();
                            g_sel.select_item(idx, true);
                        }
                    }
                    // Stash this row's canonical play-order slot so the
                    // single-row actions (edit-id3) operate on the exact
                    // row that was clicked even when the playlist lists
                    // duplicates of the same path.
                    let (cidx, is_lib_track) = item.downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            let e = o.borrow::<EditorEntry>();
                            (e.canonical_idx as i64, e.track.id > 0)
                        })
                        .unwrap_or((-1, false));
                    g_ctx_id.set(cidx);

                    let sel_count: usize = (0..g_sel.n_items())
                        .filter(|i| g_sel.is_selected(*i)).count();

                    // Gather canonical indices the row-scoped actions
                    // (Replace / Edit ID3 / Remove) operate on — selection
                    // first, falling back to the single clicked row when
                    // nothing is selected — and stash them once per
                    // right-click. send-active/add-to-new/add-to-saved/
                    // send-drive/send-device instead read the live
                    // selection straight off `edit_multi_sel` at dispatch
                    // (`ed_selected_tracks`), since they're also reachable
                    // from the "Send to ▾" button, which never fires this
                    // gesture.
                    let mut idxs: Vec<usize> = (0..g_sel.n_items())
                        .filter(|i| g_sel.is_selected(*i))
                        .filter_map(|i| g_sel.item(i))
                        .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        .collect();
                    if idxs.is_empty() {
                        let c = g_ctx_id.get();
                        if c >= 0 { idxs.push(c as usize); }
                    }
                    *g_ed_ctx_idx.borrow_mut() = idxs.clone();

                    // ── Build the real menu model ---------------------
                    let menu = gio::Menu::new();
                    menu.append_item(&gio::MenuItem::new(
                        Some("Replace Current Playlist"),
                        Some("ed.replace"),
                    ));
                    // Edit / View ID3 (single + library only)
                    if is_lib_track && sel_count <= 1 {
                        menu.append_item(&gio::MenuItem::new(
                            Some("Edit / View ID3 Tags"),
                            Some("ed.edit-id3"),
                        ));
                    }
                    menu.append_item(&gio::MenuItem::new(
                        Some("Remove from Playlist"),
                        Some("ed.remove"),
                    ));
                    let send = build_send_to_menu(
                        &g_state,
                        &SendToActions {
                            active: "ed.send-active",
                            new_playlist: "ed.add-to-new",
                            saved_playlist: "ed.add-to-saved",
                            drive: "ed.send-drive",
                            device: "ed.send-device",
                            drives: g_drives.borrow().iter()
                                .map(|d| (d.id.clone(), d.label.clone())).collect(),
                            devices: g_devices.borrow().iter()
                                .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        },
                    );
                    menu.append_submenu(Some("Send to"), &send);

                    let popover = gtk4::PopoverMenu::from_model_full(
                        &menu,
                        gtk4::PopoverMenuFlags::NESTED,
                    );
                    // EXACT mirror of the working Files-view context menu
                    // (~line 3630): parent the popover on the same widget the
                    // "ed" action group is installed on (track_scroll), and
                    // do NOT unparent on close. An earlier `connect_closed(||
                    // unparent)` severed the widget-tree link to the group as
                    // a nested item dispatched, so ed.send-drive never fired
                    // (2026-07-16). The Files menu leaves its popover parented
                    // too; matching that is what makes nested dispatch work.
                    let (px, py) = g_lbl
                        .translate_coordinates(&scroll_widget, x, y)
                        .unwrap_or((x, y));
                    let rect = gtk4::gdk::Rectangle::new(px as i32, py as i32, 1, 1);
                    popover.set_parent(&scroll_widget);
                    popover.set_pointing_to(Some(&rect));
                    popover.popup();
                    g.set_state(gtk4::EventSequenceState::Claimed);
                });
                lbl.add_controller(gesture);

                li.set_child(Some(&lbl));
            });

            let bind_id = id_str.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let t = &entry.track;
                // Stash this cell's canonical play-order index on whatever
                // child widget the cell currently holds so the editor-area
                // drop target can resolve a drop coordinate to a canonical
                // insert position via track_list.pick(x, y) → walk_up →
                // parse "pos:<N>".  Works for both Label and Button cells.
                if let Some(c) = li.child() {
                    c.set_widget_name(&format!("pos:{}", entry.canonical_idx));
                }
                // Artwork column gets the Button affordance, mirroring the
                // files view.  Click opens the cached cover-art image.
                if bind_id == "artwork_path" {
                    let Some(btn) = li.child().and_then(|c| c.downcast::<Button>().ok())
                    else { return };
                    if let Some(art_path) = t.artwork_path.clone() {
                        btn.set_visible(true);
                        btn.set_sensitive(true);
                        btn.set_label("View");
                        // Replace any prior click handler so the captured
                        // path always matches the row currently bound to
                        // this recycled cell.
                        let handler = btn.connect_clicked(move |_| {
                            open_image_viewer(&art_path);
                        });
                        // Disconnect previous handler if present to avoid
                        // accumulating across binds on the same widget.
                        unsafe {
                            if let Some(old) = btn.steal_data::<glib::SignalHandlerId>("art-handler") {
                                btn.disconnect(old);
                            }
                            btn.set_data("art-handler", handler);
                        }
                    } else {
                        btn.set_visible(false);
                    }
                    return;
                }
                let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok())
                else { return };
                let text = match bind_id.as_str() {
                    "num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
                    "title" => t.title.as_deref().unwrap_or(&t.filename).to_string(),
                    "artist" => t.artist.as_deref().unwrap_or("").to_string(),
                    "album" => t.album.as_deref().unwrap_or("").to_string(),
                    "album_artist" => t.album_artist.as_deref().unwrap_or("").to_string(),
                    "duration" => t.length_secs
                        .map(|s| { let ss = s as u64; format!("{}:{:02}", ss/60, ss%60) })
                        .unwrap_or_else(|| "-:--".to_string()),
                    "filename" => t.filename.clone(),
                    "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
                    "genre" => t.genre.as_deref().unwrap_or("").to_string(),
                    "bitrate" => t.bitrate.map(|b| format!("{b}k")).unwrap_or_default(),
                    "channels" => match t.channels.unwrap_or(0) {
                        1 => "mono".to_string(),
                        2 => "stereo".to_string(),
                        n => format!("{}ch", n),
                    },
                    "path" => t.path.clone(),
                    "play_count" => t.play_count.to_string(),
                    "last_played" => format_last_played(t.last_played.as_deref().unwrap_or("")),
                    "last_scanned" => t.last_scanned.as_deref().unwrap_or("").to_string(),
                    "disc_num" => {
                        let d = t.disc_num.unwrap_or(0);
                        if d == 0 { String::new() }
                        else if let Some(total) = t.disc_total {
                            if total > 0 { format!("{}/{}", d, total) } else { d.to_string() }
                        } else { d.to_string() }
                    }
                    "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
                    "composer" => t.composer.as_deref().unwrap_or("").to_string(),
                    "original_artist" => t.original_artist.as_deref().unwrap_or("").to_string(),
                    "copyright" => t.copyright.as_deref().unwrap_or("").to_string(),
                    "url" => t.url.as_deref().unwrap_or("").to_string(),
                    "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_string(),
                    "bpm" => t.bpm.as_deref().unwrap_or("").to_string(),
                    "lyric" => {
                        let ly = t.lyric.as_deref().unwrap_or("");
                        if ly.is_empty() { String::new() }
                        else if ly.len() > 30 { format!("{}…", &ly[..30]) }
                        else { ly.to_string() }
                    }
                    "comment" => t.comment.as_deref().unwrap_or("").to_string(),
                    "artwork_path" => if t.artwork_path.is_some() { "Yes".to_string() } else { String::new() },
                    _ => String::new(),
                };
                lbl.set_text(&gtk_safe(&text));
                // Unavailable file → broken color, mirroring the macOS
                // editor's red rows for missing files. Existence — not library
                // membership — decides this, so an uncatalogued but present
                // file shows normally.
                let missing = !std::path::Path::new(&t.path).exists();
                if missing {
                    lbl.add_css_class("broken");
                } else {
                    lbl.remove_css_class("broken");
                }
            });

            let col = ColumnViewColumn::new(Some(c.header), Some(factory));
            col.set_resizable(true);
            if c.expand { col.set_expand(true); }
            col.set_visible(visible_ids.contains(&id_str));
            if let Some(&w) = saved_widths.get(&id_str) {
                if w > 0 { col.set_fixed_width(w); }
            }

            // Display-only sorter — sort is applied via SortListModel so
            // `editing_tracks` (canonical play order) is never mutated.
            let sort_id = id_str.clone();
            let sorter = CustomSorter::new(move |a, b| {
                let a_val = a
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<EditorEntry>().track, &sort_id))
                    .unwrap_or_default();
                let b_val = b
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<EditorEntry>().track, &sort_id))
                    .unwrap_or_default();
                a_val.cmp(&b_val).into()
            });
            col.set_sorter(Some(&sorter));
            track_list.append_column(&col);
            editor_named_cols.push((id_str, col));
        }

        // Apply the files-view saved column order so the editor matches
        // it — the user only arranges columns once.  Columns not present
        // in saved_order keep their default position at the tail.
        let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
        if !saved_order.is_empty() {
            for (_, col) in editor_named_cols.iter() {
                track_list.remove_column(col);
            }
            // Position 0 = status glyph, 1 = position; named columns start at 2.
            let mut pos = 2u32;
            for col_id in &saved_order {
                if let Some((_, col)) = editor_named_cols.iter()
                    .find(|(id, _)| id == col_id)
                {
                    track_list.insert_column(pos, col);
                    pos += 1;
                }
            }
            for (id, col) in editor_named_cols.iter() {
                if !saved_order.contains(id) {
                    track_list.insert_column(pos, col);
                    pos += 1;
                }
            }
        }
    }
    // Allow drag-reorder of editor column headers — same affordance as
    // the files view.  Pinned columns (status + position) remain in
    // place because they aren't reorderable individually; GTK keeps them
    // in their declared positions.
    track_list.set_reorderable(true);

    // Shared closure that re-applies the files-view column state
    // (visibility, widths, order) to the editor's ColumnView.  Called
    // every time a saved playlist is loaded so the editor mirrors the
    // user's latest customization without needing a full ML reopen.
    let editor_cols_rc: Rc<Vec<(String, ColumnViewColumn)>> =
        Rc::new(editor_named_cols);
    let apply_editor_columns: Rc<dyn Fn()> = {
        let cols = editor_cols_rc.clone();
        let state_rc = state.clone();
        let tl = track_list.clone();
        // 2 pinned leading columns: status glyph + position.
        Rc::new(move || apply_ml_columns_to(&tl, cols.as_slice(), &state_rc, 2))
    };

    // Connect the sort model to the ColumnView's column-driven sorter so
    // header clicks produce a display sort.  Then listen for sorter changes
    // and update `reorder_allowed` — true when the active sort is "position
    // ASC" or no sort, false for any other column / order.
    {
        let sorter = track_list.sorter();
        edit_sort_model.set_sorter(sorter.as_ref());
        if let Some(s) = sorter {
            let pos_holder = pos_col_holder.clone();
            let ra = reorder_allowed.clone();
            let update = move |s: &gtk4::Sorter| {
                let pos_col = pos_holder.borrow().clone();
                let allowed = if let Some(cv_sorter) =
                    s.downcast_ref::<gtk4::ColumnViewSorter>()
                {
                    let primary = cv_sorter.primary_sort_column();
                    let order   = cv_sorter.primary_sort_order();
                    match (primary, pos_col) {
                        (None, _) => true, // default sort = canonical
                        (Some(pc), Some(target)) =>
                            pc == target && order == gtk4::SortType::Ascending,
                        _ => false,
                    }
                } else {
                    true
                };
                ra.set(allowed);
            };
            update(&s);
            s.connect_changed(move |s, _| update(s));
        }
    }

    // Rebuild track editor: splice the entire `editing_tracks` Vec into the
    // backing ListStore as `EditorEntry` items so each row carries its
    // canonical slot.  ColumnView recycles visible rows so this stays
    // cheap for big playlists.  Also rebuilds `position_map` for first-
    // occurrence path lookups by the cross-window drop target.
    let rebuild_track_list: Rc<dyn Fn()> = {
        let store    = edit_store.clone();
        let et       = editing_tracks.clone();
        let pos_map  = position_map.clone();
        Rc::new(move || {
            let mut map = pos_map.borrow_mut();
            map.clear();
            let items: Vec<glib::BoxedAnyObject> = et
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    map.entry(t.path.clone()).or_insert(i);
                    glib::BoxedAnyObject::new(EditorEntry {
                        track: t.clone(),
                        canonical_idx: i,
                    })
                })
                .collect();
            drop(map);
            store.splice(0, store.n_items(), &items);
        })
    };
    // Populate the holder so the column factories' per-cell drop targets
    // can refresh the editor after a successful reorder.
    *rebuild_track_list_holder.borrow_mut() = Some(rebuild_track_list.clone());

    // Error banner shown when a playlist's file can't be read (e.g. the
    // library was scanned in another sandbox and the stored path doesn't
    // resolve here).  Hidden while the playlist loads normally.  Hoisted
    // here so load_pl_by_id below can capture it; packed into the
    // pl-edit page further down.
    let edit_error_label: Label = Label::builder()
        .label("")
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .margin_start(8).margin_end(8)
        .margin_top(4).margin_bottom(4)
        .visible(false)
        .build();
    edit_error_label.add_css_class("broken");

    // ── Helper: load a playlist by DB id into editing state ───────────────
    let load_pl_by_id: Rc<dyn Fn(i64)> = {
        let state_rc   = state.clone();
        let et         = editing_tracks.clone();
        let saved      = saved_track_ids.clone();
        let rebuild    = rebuild_track_list.clone();
        let ep_id      = editing_pl_id.clone();
        let apply_cols = apply_editor_columns.clone();
        let err_lbl    = edit_error_label.clone();
        let search     = pl_search_entry.clone();
        Rc::new(move |id: i64| {
            ep_id.set(id);
            // A previous playlist's search query must not filter this one.
            search.set_text("");
            // Re-apply files-view column state so customizations made
            // while the editor was elsewhere take effect immediately.
            apply_cols();
            let loaded = state_rc
                .borrow()
                .media_lib
                .as_ref()
                .map(|lib| {
                    lib.playlist_by_id(id)
                        .and_then(|pl| lib.load_playlist_tracks(&pl))
                });
            let tracks = match loaded {
                Some(Ok(tracks)) => {
                    err_lbl.set_visible(false);
                    tracks
                }
                Some(Err(e)) => {
                    // Playlist entries live only in the .m3u8 file, so an
                    // unreadable file means there is nothing to show — say
                    // why instead of presenting a silently empty playlist.
                    err_lbl.set_text(&gtk_safe(&format!(
                        "This playlist has not been scanned yet and its \
                         file is not accessible from here ({e:#})."
                    )));
                    err_lbl.set_visible(true);
                    Vec::new()
                }
                None => {
                    err_lbl.set_visible(false);
                    Vec::new()
                }
            };
            let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
            *et.borrow_mut() = tracks;
            *saved.borrow_mut() = ids;
            rebuild();
        })
    };

    // Register the editor-refresh hook so any cross-window add-to-saved
    // action that targets the currently-open playlist reloads the editor.
    {
        let load = load_pl_by_id.clone();
        let ep_id = editing_pl_id.clone();
        let hook: Rc<dyn Fn(i64)> = Rc::new(move |target_pid: i64| {
            if ep_id.get() == target_pid {
                load(target_pid);
            }
        });
        EDITOR_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }
    // Refresh-current hook: reloads whatever playlist is open in the
    // editor.  Fired after a track is recorded as played so the editor
    // mirrors the files view's updated metadata + unread state.
    {
        let load = load_pl_by_id.clone();
        let ep_id = editing_pl_id.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let id = ep_id.get();
            if id >= 0 { load(id); }
        });
        EDITOR_CURRENT_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }
    // Nav-refresh hook: re-sync the playlist sidebar sub-rows and the
    // manage list with the playlists table after a playlist is created
    // from another window (e.g. active-playlist "Add to new playlist").
    {
        let state_rc     = state.clone();
        let sidebar_ref  = sidebar.clone();
        let sub_rows_ref = pl_sub_rows.clone();
        let expanded_ref = playlists_expanded.clone();
        let manage_ref   = pl_manage_list.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let playlists = state_rc
                .borrow()
                .media_lib
                .as_ref()
                .and_then(|lib| lib.all_playlists().ok())
                .unwrap_or_default();

            // Remember the selected sidebar playlist (if any) so the
            // rebuild doesn't visually drop the user's place.
            let selected = sidebar_ref
                .selected_row()
                .map(|r| r.widget_name().to_string());

            // Clear both lists, then rebuild from the playlists table.
            // Sidebar sub-rows are tracked in `pl_sub_rows`, so drain that;
            // the manage list isn't tracked, so empty it by index.
            for row in sub_rows_ref.borrow_mut().drain(..) {
                sidebar_ref.remove(&row);
            }
            while let Some(row) = manage_ref.row_at_index(0) {
                manage_ref.remove(&row);
            }

            // Insert the rebuilt rows right after the Playlists header — not at
            // the sidebar end, which is below the Devices section.
            let mut insert_at = {
                let mut idx = 0i32;
                let mut after = 1i32;
                while let Some(r) = sidebar_ref.row_at_index(idx) {
                    if r.widget_name() == "playlists" {
                        after = idx + 1;
                        break;
                    }
                    idx += 1;
                }
                after
            };

            for pl in &playlists {
                let s_lbl = Label::builder()
                    .label(&pl.name)
                    .halign(Align::Start)
                    .xalign(0.0)
                    .margin_start(24).margin_end(8)
                    .margin_top(4).margin_bottom(4)
                    .build();
                let s_row = ListBoxRow::new();
                s_row.set_widget_name(&format!("pl:{}", pl.id));
                s_row.set_child(Some(&s_lbl));
                s_row.set_visible(expanded_ref.get());
                attach_pl_row_drag(&s_row, pl.id);
                sidebar_ref.insert(&s_row, insert_at);
                insert_at += 1;
                if selected.as_deref() == Some(s_row.widget_name().as_str()) {
                    sidebar_ref.select_row(Some(&s_row));
                }
                sub_rows_ref.borrow_mut().push(s_row);

                let m_lbl = Label::builder()
                    .label(&pl.name)
                    .halign(Align::Start)
                    .margin_start(8).margin_end(8)
                    .margin_top(3).margin_bottom(3)
                    .build();
                let m_row = ListBoxRow::new();
                m_row.set_widget_name(&pl.id.to_string());
                m_row.set_child(Some(&m_lbl));
                attach_pl_row_drag(&m_row, pl.id);
                manage_ref.append(&m_row);
            }
        });
        PLAYLIST_NAV_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }

    // ── Helper: add a sub-row to both the sidebar and pl_manage_list ──────
    // Returns the sidebar row so the caller can select it.
    let _add_pl_sidebar_row = {
        let sidebar_ref  = sidebar.clone();
        let sub_rows_ref = pl_sub_rows.clone();
        let expanded_ref = playlists_expanded.clone();
        Rc::new(move |id: i64, name: &str| -> ListBoxRow {
            // Sidebar sub-row
            let s_lbl = Label::builder()
                .label(name)
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(24).margin_end(8)
                .margin_top(4).margin_bottom(4)
                .build();
            let s_row = ListBoxRow::new();
            s_row.set_widget_name(&format!("pl:{}", id));
            s_row.set_child(Some(&s_lbl));
            s_row.set_visible(expanded_ref.get());
            attach_pl_row_drag(&s_row, id);
            sidebar_ref.append(&s_row);
            sub_rows_ref.borrow_mut().push(s_row.clone());
            s_row
        })
    };

    // ── Build "pl-manage" page ────────────────────────────────────────────
    {
        let manage_vbox = GtkBox::new(Orientation::Vertical, 0);

        // Populate the manage list from DB
        let playlists_initial = state
            .borrow()
            .media_lib
            .as_ref()
            .and_then(|lib| lib.all_playlists().ok())
            .unwrap_or_default();
        for pl in &playlists_initial {
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .margin_start(8).margin_end(8)
                .margin_top(3).margin_bottom(3)
                .build();
            let row = ListBoxRow::new();
            row.set_widget_name(&pl.id.to_string());
            row.set_child(Some(&lbl));
            attach_pl_row_drag(&row, pl.id);
            pl_manage_list.append(&row);
        }

        let manage_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .child(&*pl_manage_list)
            .build();
        manage_vbox.append(&manage_scroll);

        // Clicking a manage-list row → select its sidebar sub-row
        {
            let sidebar_ref   = sidebar.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            pl_manage_list.connect_row_selected(move |_, opt_row| {
                let row = match opt_row { Some(r) => r, None => return };
                let id_str = row.widget_name().to_string();
                // Find matching sidebar "pl:ID" row and select it
                let target = format!("pl:{}", id_str);
                let mut i = 0i32;
                loop {
                    match sidebar_ref.row_at_index(i) {
                        Some(sr) if sr.widget_name() == target => {
                            sidebar_ref.select_row(Some(&sr));
                            break;
                        }
                        Some(_) => { i += 1; }
                        None => break,
                    }
                }
                // Also switch sub-stack directly (sidebar handler may not fire
                // if the row is already selected)
                pl_sub_ref.set_visible_child_name("pl-edit");
            });
        }

        // Manage list bottom buttons: New / Rename / Delete
        let manage_btn_row = GtkBox::new(Orientation::Horizontal, 4);
        manage_btn_row.set_margin_start(4);
        manage_btn_row.set_margin_end(4);
        manage_btn_row.set_margin_top(4);
        manage_btn_row.set_margin_bottom(4);

        let btn_new_pl    = Button::with_label("+ New");
        btn_new_pl.add_css_class("pl-btn");
        let btn_rename_pl = Button::with_label("Rename");
        btn_rename_pl.add_css_class("pl-btn");
        btn_rename_pl.set_sensitive(false);
        let btn_delete_pl = Button::with_label("Delete");
        btn_delete_pl.add_css_class("pl-btn");
        btn_delete_pl.set_sensitive(false);

        manage_btn_row.append(&btn_new_pl);
        manage_btn_row.append(&btn_rename_pl);
        manage_btn_row.append(&btn_delete_pl);
        manage_vbox.append(&manage_btn_row);

        // Enable/disable rename+delete based on manage list selection
        {
            let btn_ren = btn_rename_pl.clone();
            let btn_del = btn_delete_pl.clone();
            pl_manage_list.connect_row_selected(move |_, opt| {
                let has = opt.is_some();
                btn_ren.set_sensitive(has);
                btn_del.set_sensitive(has);
            });
        }

        // ── New playlist ──────────────────────────────────────────────────
        {
            let state_rc      = state.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let sub_rows_ref  = pl_sub_rows.clone();
            let expanded_ref  = playlists_expanded.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            let load          = load_pl_by_id.clone();
            let win_wk        = win.downgrade();
            btn_new_pl.connect_clicked(move |_| {
                let Some(win) = win_wk.upgrade() else { return };
                let state2  = state_rc.clone();
                let pl_ref2 = pl_list_ref.clone();
                let sid2    = sidebar_ref.clone();
                let sub2    = sub_rows_ref.clone();
                let exp2    = expanded_ref.clone();
                let pls2    = pl_sub_ref.clone();
                let load2   = load.clone();
                // Save dialog replaces the previous name-only popup —
                // user picks BOTH the filename and the target folder so
                // the new playlist no longer lands silently in Sparkamp's
                // managed `~/.config/sparkamp/playlists/` directory (which
                // had the side effect of registering itself as a watched
                // folder via `add_playlist_file`).
                run_playlist_save_dialog(state_rc.clone(), win, "New Playlist", move |path, win_cb| {
                    let name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let save_result = state2.borrow().media_lib.as_ref()
                        .map(|lib| lib.save_playlist_tracks_to_path(&path, &[]));
                    let new_id = match save_result {
                        Some(Ok(id)) => id,
                        Some(Err(e)) => {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                            return;
                        }
                        None => return,
                    };

                    // Add to manage list
                    let row_lbl = Label::builder().label(&name)
                        .halign(Align::Start)
                        .margin_start(8).margin_end(8)
                        .margin_top(3).margin_bottom(3).build();
                    let manage_row = ListBoxRow::new();
                    manage_row.set_widget_name(&new_id.to_string());
                    manage_row.set_child(Some(&row_lbl));
                    attach_pl_row_drag(&manage_row, new_id);
                    pl_ref2.append(&manage_row);
                    pl_ref2.select_row(Some(&manage_row));

                    // Add sidebar sub-row and select it
                    let s_lbl = Label::builder().label(&name)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(24).margin_end(8)
                        .margin_top(4).margin_bottom(4).build();
                    let s_row = ListBoxRow::new();
                    s_row.set_widget_name(&format!("pl:{}", new_id));
                    s_row.set_child(Some(&s_lbl));
                    s_row.set_visible(exp2.get());
                    attach_pl_row_drag(&s_row, new_id);
                    sid2.insert(&s_row, sidebar_pl_end_index(&sid2));
                    sub2.borrow_mut().push(s_row.clone());
                    sid2.select_row(Some(&s_row));

                    load2(new_id);
                    pls2.set_visible_child_name("pl-edit");
                });
            });
        }

        // ── Rename playlist ───────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let pl_list_ref = pl_manage_list.clone();
            let sidebar_ref = sidebar.clone();
            let win_wk      = win.downgrade();
            btn_rename_pl.connect_clicked(move |_| {
                let sel_row = match pl_list_ref.selected_row() { Some(r) => r, None => return };
                let id = match sel_row.widget_name().to_string().parse::<i64>() {
                    Ok(v) => v, Err(_) => return,
                };
                let current = sel_row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.text().to_string()).unwrap_or_default();

                let dialog = gtk4::Window::builder()
                    .title("Rename Playlist").modal(true).resizable(false).default_width(300)
                    .build();
                if let Some(w) = win_wk.upgrade() { dialog.set_transient_for(Some(&w)); }
                let vbox = GtkBox::new(Orientation::Vertical, 8);
                vbox.set_margin_top(12); vbox.set_margin_bottom(12);
                vbox.set_margin_start(12); vbox.set_margin_end(12);
                let lbl = Label::builder().label("New name:").halign(Align::Start).build();
                let name_entry = Entry::new();
                name_entry.set_text(&gtk_safe(&current));
                name_entry.set_hexpand(true);
                let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
                dialog_btns.set_halign(Align::End);
                let cancel_btn = Button::with_label("Cancel");
                let ok_btn     = Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                dialog_btns.append(&cancel_btn); dialog_btns.append(&ok_btn);
                vbox.append(&lbl); vbox.append(&name_entry); vbox.append(&dialog_btns);
                dialog.set_child(Some(&vbox));
                let d = dialog.clone();
                cancel_btn.connect_clicked(move |_| { d.close(); });
                let d        = dialog.clone();
                let e        = name_entry.clone();
                let state2   = state_rc.clone();
                let sel2     = sel_row.clone();
                let sid2     = sidebar_ref.clone();
                ok_btn.connect_clicked(move |_| {
                    let name = e.text().to_string();
                    if name.is_empty() { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.rename_playlist(id, &name);
                    }
                    // Update manage-list label
                    if let Some(c) = sel2.child() {
                        if let Ok(l) = c.downcast::<Label>() { l.set_text(&gtk_safe(&name)); }
                    }
                    // Update sidebar sub-row label
                    let target = format!("pl:{}", id);
                    let mut i = 0i32;
                    loop {
                        match sid2.row_at_index(i) {
                            Some(sr) if sr.widget_name() == target => {
                                if let Some(c) = sr.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(&name));
                                    }
                                }
                                break;
                            }
                            Some(_) => { i += 1; }
                            None => break,
                        }
                    }
                    d.close();
                });
                let ok2 = ok_btn.clone();
                name_entry.connect_activate(move |_| { ok2.activate(); });
                dialog.present();
            });
        }

        // ── Delete playlist ───────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let pl_list_ref = pl_manage_list.clone();
            let sidebar_ref = sidebar.clone();
            let sub_rows_ref = pl_sub_rows.clone();
            let pl_sub_ref  = pl_sub_stack.clone();
            let et          = editing_tracks.clone();
            let saved       = saved_track_ids.clone();
            let rebuild     = rebuild_track_list.clone();
            let win_wk      = win.downgrade();
            btn_delete_pl.connect_clicked(move |_| {
                let sel_row = match pl_list_ref.selected_row() { Some(r) => r, None => return };
                let id = match sel_row.widget_name().to_string().parse::<i64>() {
                    Ok(v) => v, Err(_) => return,
                };
                let pl_name = sel_row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.text().to_string()).unwrap_or_default();

                let dialog = gtk4::AlertDialog::builder()
                    .message(format!("Delete \"{}\"?", pl_name))
                    .detail("The playlist file on disk is not deleted.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0).default_button(1).modal(true).build();

                let state2    = state_rc.clone();
                let pl_ref2   = pl_list_ref.clone();
                let sid2      = sidebar_ref.clone();
                let sub2      = sub_rows_ref.clone();
                let pls2      = pl_sub_ref.clone();
                let sel2      = sel_row.clone();
                let et2       = et.clone();
                let saved2    = saved.clone();
                let rebuild2  = rebuild.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                    if result != Ok(1) { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.remove_playlist(id);
                    }
                    // Remove from manage list
                    pl_ref2.remove(&sel2);
                    // Remove sidebar sub-row
                    let target = format!("pl:{}", id);
                    let mut sub = sub2.borrow_mut();
                    sub.retain(|r| {
                        if r.widget_name() == target { sid2.remove(r); false } else { true }
                    });
                    // Go back to manage page
                    et2.borrow_mut().clear();
                    saved2.borrow_mut().clear();
                    rebuild2();
                    pls2.set_visible_child_name("pl-manage");
                });
            });
        }

        pl_sub_stack.add_named(&manage_vbox, Some("pl-manage"));
    }

    // Hoisted: title + rename button + path label (sidebar handler updates
    // the title text on selection change).
    let edit_header: Label = Label::builder()
        .label("Playlist Editor")
        .halign(Align::Start)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .margin_start(8).margin_top(4).margin_bottom(0)
        .build();
    edit_header.add_css_class("ml-section-header");

    let btn_rename_pl_inline: Button = {
        let b = Button::with_label("Rename");
        b.add_css_class("pl-btn");
        b.set_margin_end(8);
        b.set_margin_top(2);
        b
    };

    // File path bar — shows the .m3u path so the user can see if it is an
    // external playlist (not managed by Sparkamp).
    let edit_path_label: Label = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(8).margin_top(0).margin_bottom(4)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .selectable(true)
        .build();
    edit_path_label.add_css_class("status-label");

    // Save button (hoisted so the sidebar handler can toggle its sensitivity)
    let btn_save_pl_outer: Button = {
        let b = Button::with_label("Save");
        b.add_css_class("pl-btn");
        b
    };

    // ── Build "pl-edit" page ──────────────────────────────────────────────
    {
        let edit_vbox = GtkBox::new(Orientation::Vertical, 0);

        let header_row = GtkBox::new(Orientation::Horizontal, 4);
        header_row.append(&edit_header);
        header_row.append(&btn_rename_pl_inline);
        edit_vbox.append(&header_row);
        edit_vbox.append(&edit_path_label);
        edit_vbox.append(&edit_error_label);

        edit_vbox.append(&pl_search_row);

        let track_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Automatic)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&*track_list)
            .build();
        edit_vbox.append(&track_scroll);
        // Expose track_scroll so cell right-click popovers can parent
        // themselves to it (parented-to-leaf popovers don't render).
        *track_scroll_holder.borrow_mut() = Some(track_scroll.clone());
        // Install the "ed" action group ONCE on this stable ScrolledWindow —
        // mirrors dev_tracks_scroll.insert_action_group("dev", ...) in the
        // device-tracks view. The per-cell PopoverMenu is parented here too
        // (see the per-cell gesture), so action lookup never has to walk
        // more than zero ancestors.
        track_scroll.insert_action_group("ed", Some(&ed_action_group));

        // Delete key on the editor's ColumnView removes the selected
        // rows from the playlist (canonical play order) and rewrites
        // the on-disk M3U8 — same behavior as the Remove from Playlist
        // menu item.
        {
            let key = EventControllerKey::new();
            let sel    = edit_multi_sel.clone();
            let et     = editing_tracks.clone();
            let ep_id  = editing_pl_id.clone();
            let rb     = rebuild_track_list.clone();
            let st     = state.clone();
            key.connect_key_pressed(move |_, keyval, _keycode, _mods| {
                if keyval != gdk::Key::Delete && keyval != gdk::Key::KP_Delete {
                    return glib::Propagation::Proceed;
                }
                let mut idxs: Vec<usize> = (0..sel.n_items())
                    .filter(|i| sel.is_selected(*i))
                    .filter_map(|i| sel.item(i))
                    .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .collect();
                if idxs.is_empty() { return glib::Propagation::Proceed }
                idxs.sort_unstable_by(|a, b| b.cmp(a));
                {
                    let mut e = et.borrow_mut();
                    for i in idxs.iter() {
                        if *i < e.len() { e.remove(*i); }
                    }
                }
                let pid = ep_id.get();
                if pid >= 0 {
                    let s = st.borrow();
                    if let Some(lib) = s.media_lib.as_ref() {
                        let paths: Vec<String> = et.borrow()
                            .iter().map(|t| t.path.clone()).collect();
                        if let Ok(pl) = lib.playlist_by_id(pid) {
                            let _ = lib.save_playlist_tracks_to_path(
                                std::path::Path::new(&pl.path),
                                &paths,
                            );
                        }
                    }
                }
                rb();
                glib::Propagation::Stop
            });
            track_list.add_controller(key);
        }

        // Editor DropTarget — handles two drop kinds:
        //
        //   1. Reorder (every dropped path already in `editing_tracks`):
        //      splice the rows to the canonical insert position resolved
        //      from the drop coordinate.  Gated by `reorder_allowed` so
        //      drops while a non-position sort is active no-op rather than
        //      adding duplicates at the bottom.
        //   2. External add (any dropped path not in `editing_tracks`):
        //      append the *new* paths to the on-disk M3U8 via
        //      `append_paths_to_playlist` and mirror them into the
        //      editor's in-memory state.
        {
            let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
            let state_drop  = state.clone();
            let et_drop     = editing_tracks.clone();
            let ep_drop     = editing_pl_id.clone();
            let rebuild_drop = rebuild_track_list.clone();
            let _posmap_drop = position_map.clone();
            let ra_drop     = reorder_allowed.clone();
            let query_drop  = pl_edit_query.clone();
            let tl_drop     = track_list.clone();
            let dragsel_drop = drag_selection.clone();
            dt.connect_drop(move |_, value, x, y| {
                let file_list = match value.get::<gdk::FileList>() {
                    Ok(fl) => fl,
                    Err(_) => return false,
                };
                let paths: Vec<String> = file_list.files().iter()
                    .filter_map(|f| f.path())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                if paths.is_empty() { return false }
                let pid = ep_drop.get();
                let lib_opt_has = state_drop.borrow().media_lib.is_some();
                if !lib_opt_has { return false }

                // Prefer drag_selection (canonical indices captured by
                // our DragSource) so duplicates in the playlist resolve
                // correctly.  If the drag came from another window the
                // selection is empty — treat the paths as external add.
                let drag_src_indices: Vec<usize> = dragsel_drop.borrow().clone();
                let is_internal_reorder = !drag_src_indices.is_empty();

                if is_internal_reorder {
                    // Pure reorder.  Refuse silently when the current sort
                    // doesn't make reorder semantically sensible — avoids
                    // appending duplicates at the bottom in that case.  A
                    // live search filter breaks the display↔play-order
                    // mapping the same way, so it refuses too.
                    if !ra_drop.get() || !query_drop.borrow().is_empty() {
                        dragsel_drop.borrow_mut().clear();
                        return true;
                    }

                    // Resolve the drop coordinate to a canonical insert
                    // position.  First try pick(x, y) + walk up — works
                    // when the cursor is over a cell.  Falls back to a
                    // scan of every visible cell when the cursor lands
                    // between rows (no cell directly under it), inserting
                    // before the first cell whose vertical midpoint is
                    // past the drop y.  Last-resort default is append.
                    let dst_canon: usize = (|| {
                        let mut w = tl_drop.pick(x, y, gtk4::PickFlags::DEFAULT)?;
                        loop {
                            let name = w.widget_name().to_string();
                            if let Some(rest) = name.strip_prefix("pos:") {
                                if let Ok(n) = rest.parse::<usize>() {
                                    return Some(n);
                                }
                            }
                            w = w.parent()?;
                        }
                    })()
                    .or_else(|| {
                        let root_widget: &gtk4::Widget = tl_drop.upcast_ref();
                        let mut cells = editor_cell_positions(root_widget);
                        cells.sort_by(|a, b| a.1.partial_cmp(&b.1)
                            .unwrap_or(std::cmp::Ordering::Equal));
                        let drop_y = y as f32;
                        cells.iter()
                            .find(|c| c.1 + c.2 / 2.0 > drop_y)
                            .map(|c| c.0)
                    })
                    .unwrap_or_else(|| et_drop.borrow().len());

                    let mut sorted = drag_src_indices.clone();
                    sorted.sort_unstable_by(|a, b| b.cmp(a));
                    let mut adjusted_dst = dst_canon;
                    let mut removed: Vec<crate::media_library::LibTrack> = Vec::new();
                    {
                        let mut et = et_drop.borrow_mut();
                        for src in sorted.iter() {
                            if *src < et.len() {
                                let t = et.remove(*src);
                                if *src < adjusted_dst { adjusted_dst -= 1; }
                                removed.push(t);
                            }
                        }
                        removed.reverse();
                        let cap = et.len();
                        let insert_at = adjusted_dst.min(cap);
                        for (i, t) in removed.into_iter().enumerate() {
                            et.insert(insert_at + i, t);
                        }
                    }

                    if pid >= 0 {
                        let s = state_drop.borrow();
                        if let Some(lib) = s.media_lib.as_ref() {
                            let paths_now: Vec<String> = et_drop.borrow()
                                .iter().map(|t| t.path.clone()).collect();
                            if let Ok(pl) = lib.playlist_by_id(pid) {
                                if let Err(e) = lib.save_playlist_tracks_to_path(
                                    std::path::Path::new(&pl.path),
                                    &paths_now,
                                ) {
                                    eprintln!("editor reorder persist {pid}: {e}");
                                }
                            }
                        }
                    }
                    dragsel_drop.borrow_mut().clear();
                    let rb = rebuild_drop.clone();
                    glib::idle_add_local_once(move || rb());
                    return true;
                }

                // External add: append every dropped path; the user's
                // playlist may already contain some of them but treating
                // a cross-window drop as add is the least-surprising
                // semantics (duplicates can be removed afterwards).
                let new_paths: Vec<String> = paths.clone();
                if new_paths.is_empty() { return true }
                // Persist to disk first; only mutate in-memory editor state
                // if the save succeeded so failures don't leave the editor
                // diverged from the file on disk.
                if pid >= 0 {
                    let s = state_drop.borrow();
                    let lib = s.media_lib.as_ref().unwrap();
                    if let Err(e) = lib.append_paths_to_playlist(pid, &new_paths) {
                        eprintln!("editor drop append_paths_to_playlist {pid}: {e}");
                        return false;
                    }
                }
                let paths = new_paths;
                // Mirror the new entries into editing_tracks so the visible
                // ColumnView reflects them without needing a full reload.
                let new_libtracks: Vec<crate::media_library::LibTrack> = {
                    let s = state_drop.borrow();
                    let lib = s.media_lib.as_ref().unwrap();
                    paths.iter()
                        .map(|p| {
                            if let Ok(t) = lib.track_by_path(p) { return t }
                            let filename = std::path::Path::new(p)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            crate::media_library::LibTrack {
                                id: 0,
                                path: p.clone(),
                                filename,
                                artist: None, title: None, album: None,
                                track_num: None, genre: None, year: None,
                                bpm: None, length_secs: None, bitrate: None,
                                channels: None, filetype: None,
                                play_count: 0, last_played: None,
                                comment: None, album_artist: None,
                                disc_num: None, disc_total: None,
                                composer: None, original_artist: None,
                                copyright: None, url: None, encoded_by: None,
                                lyric: None, artwork_path: None,
                                last_scanned: None,
                                sample_rate: None, file_size: None,
                                file_mtime: None, added_at: None,
                                bitrate_mode: None,
                                rg_track_gain: None,
                                rg_track_peak: None,
                                rg_album_gain: None,
                                rg_album_peak: None,
                                sort_keys: Default::default(),
                            }
                        })
                        .collect()
                };
                et_drop.borrow_mut().extend(new_libtracks);
                let rb = rebuild_drop.clone();
                glib::idle_add_local_once(move || rb());
                true
            });
            track_scroll.add_controller(dt);
        }

        // Track editor controls
        let edit_btn_row = GtkBox::new(Orientation::Horizontal, 4);
        edit_btn_row.set_margin_start(4); edit_btn_row.set_margin_end(4);
        edit_btn_row.set_margin_top(4);  edit_btn_row.set_margin_bottom(4);

        let btn_add_files_pl  = Button::with_label("+ Files");    btn_add_files_pl.add_css_class("pl-btn");
        let btn_add_folder_pl = Button::with_label("+ Folder");   btn_add_folder_pl.add_css_class("pl-btn");
        let btn_remove_tracks = Button::with_label("− Remove");   btn_remove_tracks.add_css_class("pl-btn");
        let btn_delete_pl     = Button::with_label("🗑 Delete Playlist"); btn_delete_pl.add_css_class("pl-btn");
        let spring_pl         = GtkBox::new(Orientation::Horizontal, 0); spring_pl.set_hexpand(true);
        let btn_revert_pl     = Button::with_label("↺ Revert");  btn_revert_pl.add_css_class("pl-btn");
        let btn_save_as_pl    = Button::with_label("Save As…");  btn_save_as_pl.add_css_class("pl-btn");
        let btn_save_pl       = btn_save_pl_outer.clone();
        let btn_enqueue_pl    = Button::with_label("Enqueue"); btn_enqueue_pl.add_css_class("pl-btn");
        let btn_send_to_ed    = gtk4::MenuButton::builder().label("Send to ▾").build();
        btn_send_to_ed.add_css_class("pl-btn");
        // Install "ed" directly on the button too — mirrors the files
        // view's btn_send_to: window-level alone enables the top-level
        // items but the NESTED submenu popovers (Saved Playlist ▸, Disc
        // Drive ▸, Entire playlist to device ▸) resolve actions against
        // the button's own popover chain, so their items don't dispatch
        // unless the group also sits on the button itself.
        btn_send_to_ed.insert_action_group("ed", Some(&ed_action_group));
        let btn_play_pl       = Button::with_label("▶ Play");  btn_play_pl.add_css_class("pl-btn");

        edit_btn_row.append(&btn_add_files_pl);
        edit_btn_row.append(&btn_add_folder_pl);
        edit_btn_row.append(&btn_remove_tracks);
        edit_btn_row.append(&btn_delete_pl);
        edit_btn_row.append(&spring_pl);
        edit_btn_row.append(&btn_revert_pl);
        edit_btn_row.append(&btn_save_as_pl);
        edit_btn_row.append(&btn_save_pl);
        edit_btn_row.append(&btn_enqueue_pl);
        edit_btn_row.append(&btn_send_to_ed);
        edit_btn_row.append(&btn_play_pl);
        edit_vbox.append(&ed_status);
        edit_vbox.append(&edit_btn_row);

        // Whole playlist (files + .m3u8) to a device — the old flat
        // "Send to…" popover's only action, now a target-parameterised
        // action so it can live inside the standard Send-to ▾ menu as an
        // appended "Entire playlist to device" submenu (one item per
        // device). Body moved verbatim from the old per-device button.
        {
            let devices = current_devices.clone();
            let ep_id = editing_pl_id.clone();
            let state_rc = state.clone();
            let send = send_playlist_run.clone();
            let action = gio::SimpleAction::new(
                "send-playlist-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) = target.and_then(|v| v.get::<String>()) else { return };
                let Some(dev) = devices.borrow().iter().find(|d| d.id == dev_id).cloned()
                else {
                    return;
                };
                let id = ep_id.get();
                if id < 0 {
                    return;
                }
                let name = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|l| l.playlist_by_id(id).ok())
                    .map(|p| p.name)
                    .unwrap_or_default();
                send(dev, id, name);
            });
            ed_action_group.add_action(&action);
        }

        // Rebuild the Send-to menu model fresh on every open — drives/
        // devices may have come or gone. `set_create_popup_func` is
        // invoked by GTK right before the popover is shown; mirrors the
        // files view's btn_send_to.
        {
            let state_menu = state.clone();
            let current_drives = current_drives.clone();
            let current_devices = current_devices.clone();
            btn_send_to_ed.set_create_popup_func(move |btn| {
                let menu = build_send_to_menu(
                    &state_menu,
                    &SendToActions {
                        active: "ed.send-active",
                        new_playlist: "ed.add-to-new",
                        saved_playlist: "ed.add-to-saved",
                        drive: "ed.send-drive",
                        device: "ed.send-device",
                        drives: current_drives.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        devices: current_devices.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    },
                );
                let devs = current_devices.borrow();
                if !devs.is_empty() {
                    let sub = gio::Menu::new();
                    for d in devs.iter() {
                        let label = if d.label.is_empty() {
                            "Untitled device".to_string()
                        } else {
                            d.label.clone()
                        };
                        let item = gio::MenuItem::new(Some(&gtk_safe(&label)), None);
                        item.set_action_and_target_value(
                            Some("ed.send-playlist-device"),
                            Some(&d.id.to_variant()),
                        );
                        sub.append_item(&item);
                    }
                    menu.append_submenu(Some("Entire playlist to device"), &sub);
                }
                btn.set_menu_model(Some(&menu));
            });
        }

        // ── Add Files ─────────────────────────────────────────────────────
        {
            let state_rc = state.clone();
            let et       = editing_tracks.clone();
            let rebuild  = rebuild_track_list.clone();
            let win_wk   = win.downgrade();
            btn_add_files_pl.connect_clicked(move |_| {
                let dialog = gtk4::FileDialog::builder().title("Add Audio Files").build();
                let filter = gtk4::FileFilter::new();
                filter.set_name(Some("Audio files"));
                // add_suffix (not add_mime_type) so files appear even when
                // the desktop has no MIME registration (.ape, .tta, …).
                for ext in crate::model::AUDIO_EXTENSIONS {
                    filter.add_suffix(ext);
                }
                let fs = gio::ListStore::new::<gtk4::FileFilter>();
                fs.append(&filter);
                dialog.set_filters(Some(&fs));
                let state2  = state_rc.clone();
                let et2     = et.clone();
                let rebuild2 = rebuild.clone();
                let parent  = win_wk.upgrade();
                dialog.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    let Ok(list) = result else { return };
                    let paths: Vec<PathBuf> = (0..list.n_items())
                        .filter_map(|i| list.item(i))
                        .filter_map(|o| o.downcast::<gio::File>().ok())
                        .filter_map(|f| f.path())
                        .collect();
                    if paths.is_empty() { return; }
                    let s = state2.borrow();
                    if let Some(ref lib) = s.media_lib {
                        let existing: std::collections::HashSet<String> =
                            et2.borrow().iter().map(|t| t.path.clone()).collect();
                        for p in &paths {
                            if let Some(p_str) = p.to_str() {
                                if !existing.contains(p_str) {
                                    if let Ok(t) = lib.track_by_path(p_str) {
                                        et2.borrow_mut().push(t);
                                    }
                                }
                            }
                        }
                    }
                    drop(s);
                    rebuild2();
                });
            });
        }

        // ── Add Folder ────────────────────────────────────────────────────
        {
            let state_rc = state.clone();
            let et       = editing_tracks.clone();
            let rebuild  = rebuild_track_list.clone();
            let win_wk   = win.downgrade();
            btn_add_folder_pl.connect_clicked(move |_| {
                let dialog = gtk4::FileDialog::builder().title("Add Folder").build();
                let state2   = state_rc.clone();
                let et2      = et.clone();
                let rebuild2 = rebuild.clone();
                let parent   = win_wk.upgrade();
                dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else { return };
                    let Some(folder) = file.path() else { return };
                    let Some(folder_str) = folder.to_str() else { return };
                    let s = state2.borrow();
                    if let Some(ref lib) = s.media_lib {
                        let existing: std::collections::HashSet<String> =
                            et2.borrow().iter().map(|t| t.path.clone()).collect();
                        let new_tracks: Vec<_> = lib.all_tracks().unwrap_or_default()
                            .into_iter()
                            .filter(|t| t.path.starts_with(folder_str) && !existing.contains(&t.path))
                            .collect();
                        et2.borrow_mut().extend(new_tracks);
                    }
                    drop(s);
                    rebuild2();
                });
            });
        }

        // ── Remove selected tracks ────────────────────────────────────────
        {
            let sel     = edit_multi_sel.clone();
            let et      = editing_tracks.clone();
            let rebuild = rebuild_track_list.clone();
            btn_remove_tracks.connect_clicked(move |_| {
                // Map display-index selection through EditorEntry so each
                // selected row resolves to its canonical play-order slot.
                // Otherwise duplicates / a non-default sort cause the wrong
                // rows to be removed from `editing_tracks`.
                let mut to_remove: Vec<usize> = (0..sel.n_items())
                    .filter(|i| sel.is_selected(*i))
                    .filter_map(|i| sel.item(i))
                    .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .collect();
                if to_remove.is_empty() { return }
                to_remove.sort_unstable_by(|a, b| b.cmp(a));
                let mut tracks = et.borrow_mut();
                for idx in to_remove.into_iter() {
                    if idx < tracks.len() { tracks.remove(idx); }
                }
                drop(tracks);
                rebuild();
            });
        }

        // ── Revert ────────────────────────────────────────────────────────
        {
            let load    = load_pl_by_id.clone();
            let sidebar_ref = sidebar.clone();
            btn_revert_pl.connect_clicked(move |_| {
                // Find currently-selected sidebar pl: row
                let mut i = 0i32;
                loop {
                    match sidebar_ref.row_at_index(i) {
                        Some(row) => {
                            let name = row.widget_name().to_string();
                            if row.is_selected() {
                                if let Some(id_str) = name.strip_prefix("pl:") {
                                    if let Ok(id) = id_str.parse::<i64>() { load(id); }
                                }
                                break;
                            }
                            i += 1;
                        }
                        None => break,
                    }
                }
            });
        }

        // ── Save As playlist ──────────────────────────────────────────────
        {
            let state_rc     = state.clone();
            let et           = editing_tracks.clone();
            let ep_id        = editing_pl_id.clone();
            let load         = load_pl_by_id.clone();
            let sidebar_ref  = sidebar.clone();
            let pl_ml_ref    = pl_manage_list.clone();
            let win_wk       = win.downgrade();
            btn_save_as_pl.connect_clicked(move |_| {
                let Some(win) = win_wk.upgrade() else { return };
                // Pre-fill the Save dialog with the current playlist's name
                // (or "New Playlist" when the editor has no playlist loaded).
                let initial_stem = if ep_id.get() >= 0 {
                    state_rc.borrow().media_lib.as_ref()
                        .and_then(|lib| lib.playlist_by_id(ep_id.get()).ok())
                        .map(|pl| pl.name)
                        .unwrap_or_else(|| "New Playlist".to_string())
                } else {
                    "New Playlist".to_string()
                };
                let paths: Vec<String> = et.borrow().iter().map(|t| t.path.clone()).collect();
                let state2   = state_rc.clone();
                let ep_id2   = ep_id.clone();
                let load2    = load.clone();
                let sidebar2 = sidebar_ref.clone();
                let pl_ml2   = pl_ml_ref.clone();
                // Native Save dialog replaces the previous name-only popup —
                // user chooses both filename and folder so the new .m3u8
                // doesn't silently land in the managed-playlists dir (which
                // `add_playlist_file` then registered as a watched folder).
                run_playlist_save_dialog(state_rc.clone(), win, &initial_stem, move |path, win_cb| {
                    let new_name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let save_result = state2.borrow().media_lib.as_ref()
                        .map(|lib| lib.save_playlist_tracks_to_path(&path, &paths));
                    let new_id = match save_result {
                        Some(Ok(id)) => id,
                        Some(Err(e)) => {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                            return;
                        }
                        None => return,
                    };

                    // Add row to manage list + sidebar
                    let lbl = Label::builder()
                        .label(&new_name)
                        .halign(Align::Start)
                        .margin_start(8).margin_end(8)
                        .margin_top(3).margin_bottom(3)
                        .build();
                    let manage_row = ListBoxRow::new();
                    manage_row.set_widget_name(&new_id.to_string());
                    manage_row.set_child(Some(&lbl));
                    attach_pl_row_drag(&manage_row, new_id);
                    pl_ml2.append(&manage_row);

                    let s_lbl = Label::builder()
                        .label(&new_name)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(24).margin_end(8)
                        .margin_top(4).margin_bottom(4)
                        .build();
                    let s_row = ListBoxRow::new();
                    s_row.set_widget_name(&format!("pl:{}", new_id));
                    s_row.set_child(Some(&s_lbl));
                    attach_pl_row_drag(&s_row, new_id);
                    sidebar2.insert(&s_row, sidebar_pl_end_index(&sidebar2));
                    sidebar2.select_row(Some(&s_row));

                    ep_id2.set(new_id);
                    load2(new_id);
                });
            });
        }

        // ── Save playlist ─────────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let et          = editing_tracks.clone();
            let saved       = saved_track_ids.clone();
            let ep_id       = editing_pl_id.clone();
            btn_save_pl.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let track_ids: Vec<i64> = et.borrow().iter().map(|t| t.id).collect();
                if let Some(ref lib) = state_rc.borrow().media_lib {
                    let _ = lib.save_playlist_tracks(id, &track_ids);
                    *saved.borrow_mut() = track_ids;
                }
            });
        }

        // ── Play (replace active playlist; honour autoplay) ──────────────
        {
            let state_rc   = state.clone();
            let et         = editing_tracks.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track2 = set_track.clone();
            btn_play_pl.connect_clicked(move |_| {
                let tracks: Vec<crate::media_library::LibTrack> = et.borrow().clone();
                if tracks.is_empty() { return; }
                let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                {
                    let mut s = state_rc.borrow_mut();
                    let _ = s.player.stop();
                    s.playlist = crate::model::Playlist::new();
                    for lt in &tracks {
                        s.playlist.add(crate::model::Track::from(lt));
                    }
                }
                if autoplay {
                    if let Some(display) = state_rc.borrow_mut().play_current() {
                        set_track2(&display);
                    }
                }
                rebuild_pl();
            });
        }

        // ── Enqueue (append to active playlist) ──────────────────────────
        {
            let state_rc   = state.clone();
            let et         = editing_tracks.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track2 = set_track.clone();
            btn_enqueue_pl.connect_clicked(move |_| {
                let tracks: Vec<crate::media_library::LibTrack> = et.borrow().clone();
                if tracks.is_empty() { return; }
                let was_empty = state_rc.borrow().playlist.is_empty();
                let autoplay  = state_rc.borrow().config.behavior.autoplay_on_add;
                {
                    let mut s = state_rc.borrow_mut();
                    for lt in &tracks {
                        s.playlist.add(crate::model::Track::from(lt));
                    }
                }
                // Don't interrupt a track the user is already listening to.
                if autoplay && was_empty {
                    if let Some(display) = state_rc.borrow_mut().play_current() {
                        set_track2(&display);
                    }
                }
                rebuild_pl();
            });
        }

        // ── Delete this playlist ─────────────────────────────────────────
        {
            let state_rc      = state.clone();
            let ep_id         = editing_pl_id.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let sub_rows_ref  = pl_sub_rows.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            let et            = editing_tracks.clone();
            let saved         = saved_track_ids.clone();
            let rebuild       = rebuild_track_list.clone();
            let win_wk        = win.downgrade();
            btn_delete_pl.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let pl_name = state_rc.borrow().media_lib.as_ref()
                    .and_then(|lib| lib.playlist_by_id(id).ok())
                    .map(|pl| pl.name.clone())
                    .unwrap_or_default();

                let dialog = gtk4::AlertDialog::builder()
                    .message(format!("Delete \"{}\"?", pl_name))
                    .detail("The playlist file on disk is not deleted.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0).default_button(1).modal(true).build();

                let state2   = state_rc.clone();
                let ep_id2   = ep_id.clone();
                let pl_ref2  = pl_list_ref.clone();
                let sid2     = sidebar_ref.clone();
                let sub2     = sub_rows_ref.clone();
                let pls2     = pl_sub_ref.clone();
                let et2      = et.clone();
                let saved2   = saved.clone();
                let rebuild2 = rebuild.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                    if result != Ok(1) { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.remove_playlist(id);
                    }
                    // Drop the manage-list row whose widget_name == id.
                    let target = id.to_string();
                    let mut i = 0i32;
                    loop {
                        match pl_ref2.row_at_index(i) {
                            Some(r) if r.widget_name() == target => {
                                pl_ref2.remove(&r);
                                break;
                            }
                            Some(_) => i += 1,
                            None => break,
                        }
                    }
                    // Drop the matching sidebar sub-row.
                    let target_s = format!("pl:{}", id);
                    sub2.borrow_mut().retain(|r| {
                        if r.widget_name() == target_s {
                            sid2.remove(r);
                            false
                        } else { true }
                    });
                    // Clear editing state and bounce back to the manage page.
                    ep_id2.set(-1);
                    et2.borrow_mut().clear();
                    saved2.borrow_mut().clear();
                    rebuild2();
                    pls2.set_visible_child_name("pl-manage");
                });
            });
        }

        // ── Rename this playlist (header-row button) ─────────────────────
        {
            let state_rc      = state.clone();
            let ep_id         = editing_pl_id.clone();
            let header_ref    = edit_header.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let win_wk        = win.downgrade();
            btn_rename_pl_inline.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let current = state_rc.borrow().media_lib.as_ref()
                    .and_then(|lib| lib.playlist_by_id(id).ok())
                    .map(|pl| pl.name.clone())
                    .unwrap_or_default();

                let dialog = gtk4::Window::builder()
                    .title("Rename Playlist").modal(true).resizable(false).default_width(300)
                    .build();
                if let Some(w) = win_wk.upgrade() { dialog.set_transient_for(Some(&w)); }
                let vbox = GtkBox::new(Orientation::Vertical, 8);
                vbox.set_margin_top(12); vbox.set_margin_bottom(12);
                vbox.set_margin_start(12); vbox.set_margin_end(12);
                let lbl = Label::builder().label("New name:").halign(Align::Start).build();
                let name_entry = Entry::new();
                name_entry.set_text(&gtk_safe(&current));
                name_entry.set_hexpand(true);
                let btns_box = GtkBox::new(Orientation::Horizontal, 6);
                btns_box.set_halign(Align::End);
                let cancel_btn = Button::with_label("Cancel");
                let ok_btn     = Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                btns_box.append(&cancel_btn); btns_box.append(&ok_btn);
                vbox.append(&lbl); vbox.append(&name_entry); vbox.append(&btns_box);
                dialog.set_child(Some(&vbox));

                let d = dialog.clone();
                cancel_btn.connect_clicked(move |_| { d.close(); });

                let d        = dialog.clone();
                let e        = name_entry.clone();
                let state2   = state_rc.clone();
                let header2  = header_ref.clone();
                let pl_ref2  = pl_list_ref.clone();
                let sid2     = sidebar_ref.clone();
                ok_btn.connect_clicked(move |_| {
                    let name = e.text().to_string();
                    let name = name.trim();
                    if name.is_empty() { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.rename_playlist(id, name);
                    }
                    header2.set_text(&gtk_safe(name));
                    // Update manage-list row label.
                    let target = id.to_string();
                    let mut i = 0i32;
                    loop {
                        match pl_ref2.row_at_index(i) {
                            Some(r) if r.widget_name() == target => {
                                if let Some(c) = r.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(name));
                                    }
                                }
                                break;
                            }
                            Some(_) => i += 1,
                            None => break,
                        }
                    }
                    // Update sidebar sub-row label.
                    let target_s = format!("pl:{}", id);
                    let mut j = 0i32;
                    loop {
                        match sid2.row_at_index(j) {
                            Some(r) if r.widget_name() == target_s => {
                                if let Some(c) = r.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(name));
                                    }
                                }
                                break;
                            }
                            Some(_) => j += 1,
                            None => break,
                        }
                    }
                    d.close();
                });
                let ok2 = ok_btn.clone();
                name_entry.connect_activate(move |_| { ok2.activate(); });
                dialog.present();
            });
        }

        // ── Right-click context menu on track rows ───────────────────────
        // Add to / Replace active playlist, Edit ID3 (single only), Remove
        // from Library.  No album-art viewer in GTK so that entry is
        // omitted here.
        {
            // ctx_canonical_idx is now hoisted above the column builder so each
            // editor cell's right-click gesture can record into it.  Reuse
            // the outer binding so action handlers see the same Cell.
            let action_group = gio::SimpleActionGroup::new();

            // Helper: collect the canonical indices the action should
            // operate on — the current multi-selection, falling back to
            // the single right-clicked row when nothing is selected.
            let selected_canonical_indices = {
                let sel = edit_multi_sel.clone();
                let id_ref = ctx_canonical_idx.clone();
                Rc::new(move || -> Vec<usize> {
                    let mut idxs: Vec<usize> = (0..sel.n_items())
                        .filter(|i| sel.is_selected(*i))
                        .filter_map(|i| sel.item(i))
                        .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        .collect();
                    if idxs.is_empty() {
                        let c = id_ref.get();
                        if c >= 0 { idxs.push(c as usize); }
                    }
                    idxs
                })
            };

            // ─── Append (add to active playlist) ─────────────────────────
            {
                let state_rc   = state.clone();
                let et         = editing_tracks.clone();
                let rebuild_pl = rebuild_playlist.clone();
                let set_track2 = set_track.clone();
                let pick_idxs  = selected_canonical_indices.clone();
                let action     = gio::SimpleAction::new("append", None);
                action.connect_activate(move |_, _| {
                    let tracks: Vec<crate::media_library::LibTrack> = {
                        let et_b = et.borrow();
                        pick_idxs().into_iter()
                            .filter_map(|i| et_b.get(i).cloned())
                            .collect()
                    };
                    if tracks.is_empty() { return }
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay  = state_rc.borrow().config.behavior.autoplay_on_add;
                    {
                        let mut s = state_rc.borrow_mut();
                        for lt in &tracks {
                            s.playlist.add(crate::model::Track::from(lt));
                        }
                    }
                    if autoplay && was_empty {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track2(&display);
                        }
                    }
                    rebuild_pl();
                });
                action_group.add_action(&action);
            }

            // ─── Replace (active playlist becomes the selection) ─────────
            {
                let state_rc   = state.clone();
                let et         = editing_tracks.clone();
                let rebuild_pl = rebuild_playlist.clone();
                let set_track2 = set_track.clone();
                let pick_idxs  = selected_canonical_indices.clone();
                let action     = gio::SimpleAction::new("replace", None);
                action.connect_activate(move |_, _| {
                    let tracks: Vec<crate::media_library::LibTrack> = {
                        let et_b = et.borrow();
                        pick_idxs().into_iter()
                            .filter_map(|i| et_b.get(i).cloned())
                            .collect()
                    };
                    if tracks.is_empty() { return }
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    {
                        let mut s = state_rc.borrow_mut();
                        let _ = s.player.stop();
                        s.playlist = crate::model::Playlist::new();
                        for lt in &tracks {
                            s.playlist.add(crate::model::Track::from(lt));
                        }
                    }
                    if autoplay {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track2(&display);
                        }
                    }
                    rebuild_pl();
                });
                action_group.add_action(&action);
            }

            // ─── Edit ID3 (single only) ──────────────────────────────────
            {
                let state_rc      = state.clone();
                let id_ref        = ctx_canonical_idx.clone();
                let et            = editing_tracks.clone();
                let rebuild_pl    = rebuild_playlist.clone();
                let action        = gio::SimpleAction::new("edit-id3", None);
                action.connect_activate(move |_, _| {
                    let c = id_ref.get();
                    if c < 0 { return }
                    let path = et.borrow().get(c as usize)
                        .map(|t| t.path.clone());
                    let Some(path) = path else {
                        return;
                    };
                    open_id3_editor_window(
                        None::<&gtk4::Window>,
                        path.into(),
                        state_rc.clone(),
                        rebuild_pl.clone(),
                        None,
                    );
                });
                action_group.add_action(&action);
            }

            // ─── Remove from Playlist (mutate editing_tracks + persist) ──
            // Removes selected rows from the canonical play order and
            // immediately rewrites the on-disk M3U8.  Does NOT delete the
            // track from the media library — the user's library DB is
            // untouched.
            {
                let state_rc = state.clone();
                let et       = editing_tracks.clone();
                let ep_id    = editing_pl_id.clone();
                let rebuild  = rebuild_track_list.clone();
                let pick_idxs = selected_canonical_indices.clone();
                let action   = gio::SimpleAction::new("remove", None);
                action.connect_activate(move |_, _| {
                    let mut idxs = pick_idxs();
                    if idxs.is_empty() { return }
                    idxs.sort_unstable_by(|a, b| b.cmp(a));
                    {
                        let mut e = et.borrow_mut();
                        for i in idxs.iter() {
                            if *i < e.len() { e.remove(*i); }
                        }
                    }
                    let pid = ep_id.get();
                    if pid >= 0 {
                        let s = state_rc.borrow();
                        if let Some(lib) = s.media_lib.as_ref() {
                            let paths: Vec<String> = et.borrow()
                                .iter().map(|t| t.path.clone()).collect();
                            if let Ok(pl) = lib.playlist_by_id(pid) {
                                if let Err(e) = lib.save_playlist_tracks_to_path(
                                    std::path::Path::new(&pl.path),
                                    &paths,
                                ) {
                                    eprintln!("ple.remove persist {pid}: {e}");
                                }
                            }
                        }
                    }
                    rebuild();
                });
                action_group.add_action(&action);
            }

            // ─── Seed a new saved playlist from the editor selection ─────
            {
                let state_rc = state.clone();
                let sel      = edit_multi_sel.clone();
                let et       = editing_tracks.clone();
                let win_atn  = win.clone();
                let action   = gio::SimpleAction::new("add-to-new", None);
                action.connect_activate(move |_, _| {
                    let paths: Vec<String> = {
                        let et_b = et.borrow();
                        // Selection indices are display positions in the
                        // sorted model — map each through EditorEntry to
                        // the canonical play-order slot so duplicates and
                        // non-default sorts both resolve correctly.
                        let mut p: Vec<String> = (0..sel.n_items())
                            .filter(|i| sel.is_selected(*i))
                            .filter_map(|i| sel.item(i))
                            .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                            .filter_map(|c| et_b.get(c))
                            .map(|t| t.path.clone())
                            .collect();
                        if p.is_empty() {
                            p = et_b.iter().map(|t| t.path.clone()).collect();
                        }
                        p
                    };
                    if paths.is_empty() { return }
                    let default_stem = glib::DateTime::now_local()
                        .ok()
                        .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Playlist".to_string());
                    let state_cb = state_rc.clone();
                    let paths_cb = paths.clone();
                    run_playlist_save_dialog(
                        state_rc.clone(),
                        win_atn.clone(),
                        &default_stem,
                        move |path, win_cb| {
                            if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                                if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                                    eprintln!("save_playlist_tracks_to_path: {e}");
                                    show_playlist_save_error(&win_cb, &path, &e);
                                }
                            }
                        },
                    );
                });
                action_group.add_action(&action);
            }

            // ─── Add selection to a saved playlist (parameterised by id) ─
            {
                let state_rc = state.clone();
                let sel      = edit_multi_sel.clone();
                let et       = editing_tracks.clone();
                let action   = gio::SimpleAction::new(
                    "add-to-saved",
                    Some(glib::VariantTy::INT64),
                );
                action.connect_activate(move |_, param| {
                    let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
                    let paths: Vec<String> = {
                        let et_borrow = et.borrow();
                        (0..sel.n_items())
                            .filter(|i| sel.is_selected(*i))
                            .filter_map(|i| sel.item(i))
                            .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                            .filter_map(|c| et_borrow.get(c))
                            .map(|t| t.path.clone())
                            .collect()
                    };
                    if paths.is_empty() { return }
                    let mut ok = false;
                    if let Some(lib) = state_rc.borrow().media_lib.as_ref() {
                        match lib.append_paths_to_playlist(pid, &paths) {
                            Ok(_)  => ok = true,
                            Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                        }
                    }
                    if ok { notify_playlist_changed(pid); }
                });
                action_group.add_action(&action);
            }

            track_list.insert_action_group("ple", Some(&action_group));
            if let Some(ref ts) = *track_scroll_holder.borrow() {
                ts.insert_action_group("ple", Some(&action_group));
            }
            win.insert_action_group("ple", Some(&action_group));
            // ALSO attach the actions to the GtkApplication (app-level)
            // under "app-ple-*" names — PopoverMenu dispatch via the
            // app prefix is the reliable code path in GTK4, even when
            // widget-tree action lookup fails for nested popovers.
            if let Some(app) = win.application() {
                let app_action_names = ["append", "replace", "edit-id3",
                                        "remove", "add-to-new", "add-to-saved"];
                for name in app_action_names {
                    if let Some(act) = action_group.lookup_action(name) {
                        let app_name = format!("ple-{name}");
                        let simple = act.downcast_ref::<gio::SimpleAction>();
                        if let Some(sa) = simple {
                            // Build a parallel app-level SimpleAction
                            // that forwards activate to the editor's
                            // group action.  Same parameter type.
                            let app_action = gio::SimpleAction::new(
                                &app_name,
                                sa.parameter_type().as_ref().map(|v| &**v),
                            );
                            let sa_clone = sa.clone();
                            app_action.connect_activate(move |_, param| {
                                eprintln!("[app.{app_name}] forwarding to ple.{name}");
                                sa_clone.activate(param);
                            });
                            app.add_action(&app_action);
                        }
                    }
                }
            }
            *ple_action_group_holder.borrow_mut() = Some(action_group.clone());
            // Per-cell right-click gesture lives inside each column's
            // factory.connect_setup — see the editor column builder at the
            // top of this scope.  Nothing to register here at the row level.

            // Double-click / Enter activates the row: append to the active
            // playlist (matches the ML files view affordance).  Respects
            // the user's playlist_add_behavior preference (Append vs Replace)
            // and autoplay_on_add config.
            {
                let state_rc     = state.clone();
                let et           = editing_tracks.clone();
                let rebuild_pl   = rebuild_playlist.clone();
                let set_track_pe = set_track.clone();
                let sel_act = edit_multi_sel.clone();
                track_list.connect_activate(move |_, pos| {
                    // `pos` is a display position; resolve through the
                    // sorted model to the canonical row in `editing_tracks`.
                    let canon = sel_act.item(pos)
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx);
                    let Some(canon) = canon else { return };
                    let lt = et.borrow().get(canon).cloned();
                    let Some(lt) = lt else { return };
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                        == crate::config::PlaylistAddBehavior::Replace;
                    if should_replace {
                        let _ = state_rc.borrow_mut().player.stop();
                        state_rc.borrow_mut().playlist.clear();
                    }
                    state_rc.borrow_mut().playlist.add(crate::model::Track::from(&lt));
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_pe(&display);
                        }
                    }
                    rebuild_pl();
                });
            }
        }

        pl_sub_stack.add_named(&edit_vbox, Some("pl-edit"));
    }

    {
        let pl_vbox = GtkBox::new(Orientation::Vertical, 0);
        pl_vbox.append(&*pl_sub_stack);
        stack.add_named(&pl_vbox, Some("playlists"));
    }

    // Wire sidebar to stack.
    {
        let stack_ref      = stack.clone();
        let pl_sub_ref     = pl_sub_stack.clone();
        let load           = load_pl_by_id.clone();
        let state_rc       = state.clone();
        let expanded_rc    = playlists_expanded.clone();
        let hdr_lbl        = edit_header.clone();
        let path_lbl       = edit_path_label.clone();
        let save_btn       = btn_save_pl_outer.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let row = match opt_row { Some(r) => r, None => return };
            let name = row.widget_name().to_string();

            if name == "files" {
                stack_ref.set_visible_child_name("files");
            } else if name == "playlists" {
                stack_ref.set_visible_child_name("playlists");
                pl_sub_ref.set_visible_child_name("pl-manage");
                // Expand sub-rows on navigation
                if !expanded_rc.get() {
                    expanded_rc.set(true);
                }
            } else if let Some(id_str) = name.strip_prefix("pl:") {
                if let Ok(id) = id_str.parse::<i64>() {
                    stack_ref.set_visible_child_name("playlists");
                    load(id);
                    pl_sub_ref.set_visible_child_name("pl-edit");
                    // Update editor header, path bar, and Save sensitivity.
                    if let Some(ref lib) = state_rc.borrow().media_lib {
                        if let Ok(pl) = lib.playlist_by_id(id) {
                            hdr_lbl.set_text(&gtk_safe(&pl.name));
                            path_lbl.set_text(&gtk_safe(&pl.path));
                            // Disable Save for external playlists; user should
                            // use Save As to get a Sparkamp-managed copy.
                            let is_managed = lib.playlist_is_managed(id);
                            save_btn.set_sensitive(is_managed);
                        }
                    }
                }
            }
        });
    }

    // Persist sidebar expansion state on window close (handled in close_request below).


    // ── Device detection: poll udisks2 and keep the sidebar live ──────────
    // A 2 s poll (rather than D-Bus signal wiring) keeps this simple while
    // still updating in place — devices appear/disappear and free space
    // refreshes without reopening the window.
    // Deferred handles to the eject / sync runners (defined further down, once
    // the refresh + reload closures they need exist). The overview rows' Sync
    // and Eject buttons call through these.
    let eject_run_holder: Rc<RefCell<Option<Rc<dyn Fn(String)>>>> =
        Rc::new(RefCell::new(None));
    let sync_run_holder: Rc<RefCell<Option<Rc<dyn Fn(crate::devices::Device, Button)>>>> =
        Rc::new(RefCell::new(None));

    // Rebuild the device overview list (shown when the Devices header is
    // selected) from the latest detection results. Each device is its own row
    // with Sync and Eject buttons on the right.
    let rebuild_overview: Rc<dyn Fn()> = {
        let list = dev_overview_list.clone();
        let current = current_devices.clone();
        let eject_holder = eject_run_holder.clone();
        let sync_holder = sync_run_holder.clone();
        let counts_cache = device_counts.clone();
        let counts_inflight = counts_in_flight.clone();
        let transfers = device_transfers.clone();
        let card_bars = device_card_progress.clone();
        let sidebar_ov = sidebar.clone();
        Rc::new(move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            // Card progress bars are rebuilt below; drop the stale references.
            card_bars.borrow_mut().clear();
            let devs = current.borrow();
            if devs.is_empty() {
                let l = Label::builder()
                    .label("No devices connected.")
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                l.add_css_class("status-label");
                list.append(&l);
                return;
            }
            for d in devs.iter() {
                let name = if d.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    d.label.clone()
                };

                // ── Card ────────────────────────────────────────────────
                let card = GtkBox::new(Orientation::Vertical, 6);
                card.add_css_class("device-card");

                // Header: icon · name + filesystem · status badges.
                let header = GtkBox::new(Orientation::Horizontal, 10);
                let icon = Image::from_icon_name(device_icon_name(d));
                icon.set_pixel_size(32);
                icon.set_valign(Align::Center);
                header.append(&icon);

                let title_box = GtkBox::new(Orientation::Vertical, 0);
                title_box.set_hexpand(true);
                title_box.set_valign(Align::Center);
                let name_lbl = Label::builder()
                    .label(&gtk_safe(&name))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                name_lbl.add_css_class("device-card-name");
                let fs_lbl = Label::builder()
                    .label(if d.fs_type.is_empty() { "unknown" } else { &d.fs_type })
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                fs_lbl.add_css_class("status-label");
                title_box.append(&name_lbl);
                title_box.append(&fs_lbl);
                header.append(&title_box);

                let badges = GtkBox::new(Orientation::Horizontal, 4);
                badges.set_valign(Align::Center);
                if d.read_only {
                    let b = Label::new(Some("🔒 Read-only"));
                    b.add_css_class("device-badge");
                    badges.append(&b);
                }
                if device_fs_unsupported(&d.fs_type) {
                    let b = Label::new(Some("⚠ Unsupported"));
                    b.add_css_class("device-badge");
                    b.add_css_class("device-badge-warn");
                    b.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                    badges.append(&b);
                }
                header.append(&badges);
                // Clicking the card's banner (icon + name area) opens that
                // device's detail page by selecting its sidebar row, which the
                // row-selected handler turns into the detail view. The Sync/Eject
                // buttons live in their own row below and claim their own clicks.
                {
                    let click = gtk4::GestureClick::new();
                    let sidebar = sidebar_ov.clone();
                    let row_name = format!("dev:{}", d.backend_id);
                    click.connect_released(move |_, _, _, _| {
                        if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                            sidebar.select_row(Some(&row));
                        }
                    });
                    header.add_controller(click);
                    header.set_cursor_from_name(Some("pointer"));
                }
                card.append(&header);

                // Capacity bar + free/total text.
                let used = if d.total_bytes > 0 {
                    1.0 - (d.free_bytes as f64 / d.total_bytes as f64)
                } else {
                    0.0
                };
                let bar = gtk4::LevelBar::new();
                bar.set_min_value(0.0);
                bar.set_max_value(1.0);
                bar.set_value(used);
                set_levelbar_fullness(&bar, used);
                card.append(&bar);

                let cap_lbl = Label::builder()
                    .label(&format!(
                        "{:.1} GB free of {:.1} GB",
                        d.free_bytes as f64 / 1e9,
                        d.total_bytes as f64 / 1e9,
                    ))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                cap_lbl.add_css_class("status-label");
                card.append(&cap_lbl);

                // Song / playlist counts — cached, computed off-thread on miss.
                let counts_lbl = Label::builder()
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                counts_lbl.add_css_class("status-label");
                match counts_cache.borrow().get(&d.backend_id).copied() {
                    Some((songs, pls)) => {
                        counts_lbl.set_text(&counts_text(songs, pls));
                    }
                    None => {
                        counts_lbl.set_text("counting…");
                        let backend = d.backend_id.clone();
                        if counts_inflight.borrow_mut().insert(backend.clone()) {
                            let mount = d.mount_path.clone();
                            let cache = counts_cache.clone();
                            let inflight = counts_inflight.clone();
                            let lbl = counts_lbl.clone();
                            glib::spawn_future_local(async move {
                                let res = gio::spawn_blocking(move || {
                                    if device_io_shutting_down() {
                                        return (0, 0);
                                    }
                                    let songs =
                                        crate::devices::browse::list_audio_files(&mount).len();
                                    let pls = crate::devices::browse::device_playlist_files(&mount)
                                        .len();
                                    (songs, pls)
                                })
                                .await
                                .unwrap_or((0, 0));
                                cache.borrow_mut().insert(backend.clone(), res);
                                inflight.borrow_mut().remove(&backend);
                                lbl.set_text(&counts_text(res.0, res.1));
                            });
                        }
                    }
                }
                card.append(&counts_lbl);

                // Copy progress bar — always present (reserves its space) so the
                // card height is identical whether or not a transfer is running.
                // Transparent when idle; the runners drive it via backend_id.
                let prog = gtk4::ProgressBar::new();
                prog.set_show_text(true);
                apply_card_progress(&prog, transfers.borrow().get(&d.backend_id).copied());
                card.append(&prog);
                card_bars.borrow_mut().insert(d.backend_id.clone(), prog);

                // Sync / Eject buttons, right-aligned.
                let btn_row = GtkBox::new(Orientation::Horizontal, 6);
                btn_row.set_halign(Align::End);
                btn_row.set_margin_top(2);

                let sync_btn = Button::with_label("Sync");
                sync_btn.add_css_class("pl-btn");
                {
                    let holder = sync_holder.clone();
                    let dev = d.clone();
                    sync_btn.connect_clicked(move |btn| {
                        if let Some(run) = holder.borrow().as_ref() {
                            run(dev.clone(), btn.clone());
                        }
                    });
                }
                btn_row.append(&sync_btn);

                let eject_btn = Button::with_label("Eject");
                eject_btn.add_css_class("pl-btn");
                // Unavailable while a copy to this device is running.
                eject_btn.set_sensitive(
                    d.ejectable && !transfers.borrow().contains_key(&d.backend_id),
                );
                {
                    let holder = eject_holder.clone();
                    let backend = d.backend_id.clone();
                    eject_btn.connect_clicked(move |btn| {
                        btn.set_sensitive(false);
                        if let Some(run) = holder.borrow().as_ref() {
                            run(backend.clone());
                        }
                    });
                }
                btn_row.append(&eject_btn);
                card.append(&btn_row);

                list.append(&card);
            }
        })
    };

    let refresh_devices: Rc<dyn Fn()> = {
        let sidebar = sidebar.clone();
        let dev_sub_rows = dev_sub_rows.clone();
        let devices_expanded = devices_expanded.clone();
        let current_devices = current_devices.clone();
        let banner = dev_banner.clone();
        let banner_lbl = dev_banner_lbl.clone();
        let rebuild_overview = rebuild_overview.clone();
        // Guard against overlapping polls stacking up.
        let in_flight = Rc::new(Cell::new(false));
        Rc::new(move || {
            let sidebar = sidebar.clone();
            let dev_sub_rows = dev_sub_rows.clone();
            let devices_expanded = devices_expanded.clone();
            let current_devices_cb = current_devices.clone();
            let banner = banner.clone();
            let banner_lbl = banner_lbl.clone();
            let rebuild_overview = rebuild_overview.clone();
            // udisks2 access runs on a worker thread so a stalled D-Bus call
            // can never freeze the UI — a main-thread block previously made
            // the app impossible to quit or eject after a copy.
            refresh_device_cache(
                current_devices.clone(),
                in_flight.clone(),
                Rc::new(move |outcome| {
                    match outcome {
                        DeviceRefreshOutcome::Ok => {
                            banner.set_visible(false);
                            // `refresh_device_cache` already wrote the merged,
                            // sorted list into `current_devices`.
                            let devs = current_devices_cb.borrow();
                            let want: Vec<String> =
                                devs.iter().map(|d| format!("dev:{}", d.backend_id)).collect();
                            // Remove rows for devices that went away.
                            dev_sub_rows.borrow_mut().retain(|r| {
                                let keep = want.contains(&r.widget_name().to_string());
                                if !keep {
                                    sidebar.remove(r);
                                }
                                keep
                            });
                            // Add rows for new devices; update free-space bars in
                            // place so selection isn't disturbed when unchanged.
                            let expanded = devices_expanded.get();
                            for d in devs.iter() {
                                let name = format!("dev:{}", d.backend_id);
                                let used = if d.total_bytes > 0 {
                                    1.0 - (d.free_bytes as f64 / d.total_bytes as f64)
                                } else {
                                    0.0
                                };
                                let base = if d.label.is_empty() {
                                    "Untitled device".to_string()
                                } else {
                                    d.label.clone()
                                };
                                // Status glyphs: ⚠ unsupported fs, 🔒 read-only.
                                let label_text = format!(
                                    "{}{base}",
                                    device_glyph_prefix(d.read_only, &d.fs_type)
                                );
                                let existing = dev_sub_rows
                                    .borrow()
                                    .iter()
                                    .find(|r| r.widget_name().as_str() == name)
                                    .cloned();
                                match existing {
                                    Some(row) => {
                                        if let Some(bx) =
                                            row.child().and_then(|c| c.downcast::<GtkBox>().ok())
                                        {
                                            // Keep the label current (e.g. an MTP
                                            // device whose friendly name resolved
                                            // after the first poll).
                                            if let Some(lbl) = bx
                                                .first_child()
                                                .and_then(|c| c.downcast::<Label>().ok())
                                            {
                                                lbl.set_text(&gtk_safe(&label_text));
                                            }
                                            if let Some(bar) = bx
                                                .last_child()
                                                .and_then(|c| c.downcast::<gtk4::LevelBar>().ok())
                                            {
                                                bar.set_value(used);
                                                set_levelbar_fullness(&bar, used);
                                            }
                                        }
                                    }
                                    None => {
                                        let bx = GtkBox::new(Orientation::Vertical, 2);
                                        bx.set_margin_start(24);
                                        bx.set_margin_end(8);
                                        bx.set_margin_top(4);
                                        bx.set_margin_bottom(4);
                                        let lbl = Label::builder()
                                            .label(&gtk_safe(&label_text))
                                            .halign(Align::Start)
                                            .xalign(0.0)
                                            .build();
                                        let bar = gtk4::LevelBar::new();
                                        bar.set_min_value(0.0);
                                        bar.set_max_value(1.0);
                                        bar.set_value(used);
                                        set_levelbar_fullness(&bar, used);
                                        bx.append(&lbl);
                                        bx.append(&bar);
                                        let row = ListBoxRow::new();
                                        row.set_widget_name(&name);
                                        row.set_child(Some(&bx));
                                        row.set_visible(expanded);
                                        if device_fs_unsupported(&d.fs_type) {
                                            row.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                                        }
                                        sidebar.append(&row);
                                        dev_sub_rows.borrow_mut().push(row);
                                    }
                                }
                            }
                        }
                        // udisks failed — MTP (if any) is hidden until it recovers.
                        // `refresh_device_cache` already cleared `current_devices`.
                        DeviceRefreshOutcome::UdisksError(e) => {
                            for r in dev_sub_rows.borrow_mut().drain(..) {
                                sidebar.remove(&r);
                            }
                            use crate::devices::diagnostics::{self, Diagnosis};
                            let diag = diagnostics::classify(
                                diagnostics::has_udisks_grant(&diagnostics::read_flatpak_info()),
                                &diagnostics::read_distro_info(),
                                crate::devices::detect::classify_error(&e),
                            );
                            let msg = match diag {
                                Diagnosis::PermissionOff => {
                                    "Can't access drives — Sparkamp needs permission to use the \
                                     system disk service. Enable org.freedesktop.UDisks2 under \
                                     System Bus in Flatseal, then Retry."
                                }
                                Diagnosis::NotInstalled => {
                                    "Can't access drives — your system's disk service (udisks2) \
                                     isn't installed. Install it, then Retry."
                                }
                                Diagnosis::EjectUnavailable => {
                                    "Couldn't reach the disk service. Retry, or manage the device \
                                     through your file browser."
                                }
                            };
                            banner_lbl.set_text(msg);
                            banner.set_visible(true);
                        }
                        // The worker thread panicked. `refresh_device_cache`
                        // already cleared `current_devices`.
                        DeviceRefreshOutcome::WorkerPanicked => {
                            for r in dev_sub_rows.borrow_mut().drain(..) {
                                sidebar.remove(&r);
                            }
                            banner_lbl.set_text("Couldn't query the device service.");
                            banner.set_visible(true);
                        }
                    }
                    // Keep the overview list in sync with the latest results.
                    rebuild_overview();
                }),
            );
        })
    };

    // Initial scan + 2 s poll (stops once the window — hence the sidebar — is gone).
    refresh_devices();
    {
        let refresh = refresh_devices.clone();
        let sidebar_weak = sidebar.downgrade();
        glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            if sidebar_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            refresh();
            glib::ControlFlow::Continue
        });
    }
    {
        let refresh = refresh_devices.clone();
        dev_banner_retry.connect_clicked(move |_| refresh());
    }

    // ── Disc Drives: playlist adds, detail population, overview, poll ────────
    // Turn DiscTrackEntry values into active-playlist rows, honoring the same
    // add-behavior + autoplay rules as the ML double-click path. Phase 1 has no
    // gnudb tags yet, so titles are "Track N" and artist/album stay empty (the
    // " / " sampler split still applies to future matched discs).
    // How disc tracks land in the active playlist:
    //   Behavior — the double-click path: honor the replace/append setting
    //   and autoplay-on-add (same as the ML files double-click).
    //   PlayNow — the "▶ Play" button: replace the playlist with the picked
    //   tracks and play (same as the device/files views' Play).
    //   Enqueue — append only; start playing only when the playlist was
    //   empty and autoplay-on-add is set (same as the views' Enqueue).
    #[derive(Clone, Copy, PartialEq)]
    enum DiscAdd {
        Behavior,
        PlayNow,
        Enqueue,
    }
    let add_disc_entries: Rc<dyn Fn(&[crate::disc::DiscTrackEntry], DiscAdd)> = {
        let state = state.clone();
        let rebuild = rebuild_playlist.clone();
        let disc_tags = disc_tags.clone();
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        Rc::new(move |entries: &[crate::disc::DiscTrackEntry], mode: DiscAdd| {
            if entries.is_empty() {
                return;
            }
            use crate::config::PlaylistAddBehavior;
            let behavior = state.borrow().config.behavior.playlist_add_behavior.clone();
            let autoplay = state.borrow().config.behavior.autoplay_on_add;
            let replace = match mode {
                DiscAdd::Behavior => behavior == PlaylistAddBehavior::Replace,
                DiscAdd::PlayNow => true,
                DiscAdd::Enqueue => false,
            };
            // Disc-level artist/album for the currently shown drive (empty until
            // identified/edited); used for the non-sampler title case.
            let (disc_artist, disc_album) =
                selected_disc_discid(&selected_disc_id, &current_drives)
                    .and_then(|(_, id)| {
                        disc_tags
                            .borrow()
                            .get(&id)
                            .map(|t| (t.artist.clone(), t.album.clone()))
                    })
                    .unwrap_or_default();
            if replace {
                let _ = state.borrow_mut().player.stop();
                let mut s = state.borrow_mut();
                s.playlist.tracks.clear();
                s.playlist.current_index = 0;
                s.last_duration = None;
                s.pending_seek = None;
                s.mute_pending = None;
            }
            let insert_start = state.borrow().playlist.len();
            for e in entries {
                // Sampler discs put the per-track artist in the title.
                let meta = crate::disc::track_meta(&e.title, &disc_artist);
                state.borrow_mut().playlist.tracks.push(crate::model::Track {
                    path: std::path::PathBuf::from(&e.path),
                    title: meta.title,
                    artist: meta.artist,
                    album_artist: String::new(),
                    album: disc_album.clone(),
                    duration: Some(std::time::Duration::from_secs(e.duration_secs as u64)),
                    broken: false,
                    read_only: true, // disc media is never writable in place
                    id: 0,
                });
            }
            rebuild();
            let start = match mode {
                DiscAdd::PlayNow => true,
                DiscAdd::Behavior => autoplay && (replace || insert_start == 0),
                DiscAdd::Enqueue => autoplay && insert_start == 0,
            };
            if start {
                state.borrow_mut().playlist.jump_to(insert_start);
                state.borrow_mut().play_current();
            }
        })
    };

    // Fill the drive detail view for one drive: header, media state, and either
    // the audio-track list or a banner for no-disc/blank/data media.
    let populate_disc_detail: Rc<dyn Fn(&crate::disc::OpticalDrive)> = {
        let title = disc_title.clone();
        let icon_box = disc_icon_box.clone();
        let media_lbl = disc_media_lbl.clone();
        let tag_lbl = disc_tag_lbl.clone();
        let banner = disc_banner.clone();
        let track_list = disc_track_list.clone();
        let tracks_scroll = disc_tracks_scroll.clone();
        let actions = disc_actions.clone();
        // Audio-only actions hide on non-audio media; Eject shows whenever a
        // disc is present (mac parity).
        let audio_btns = [
            disc_enqueue.clone(),
            disc_play.clone(),
            disc_identify.clone(),
            disc_rip.clone(),
            disc_edit_tags.clone(),
        ];
        let eject_btn = disc_eject.clone();
        let submit_btn = disc_submit.clone();
        let entries_store = current_disc_entries.clone();
        let disc_tags = disc_tags.clone();
        let disc_official = disc_official.clone();
        let disc_cdtext = disc_cdtext.clone();
        let disc_cdtext_tried = disc_cdtext_tried.clone();
        let populate_holder = populate_holder.clone();
        let current_drives_ct = current_drives.clone();
        let search_row = disc_search_row.clone();
        let search_entry = disc_search_entry.clone();
        let burn_ui = burn_ui.clone();
        // Task 9 — data-disc file browser.
        let files_scroll = disc_files_scroll.clone();
        let files_store = disc_files_store.clone();
        let add_all_btn = disc_add_all_btn.clone();
        let load_files = load_disc_files.clone();
        // Which drive the detail last showed — a switch clears the search
        // (the 10 s poll repopulates the SAME drive and must not).
        let last_drive: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        Rc::new(move |drive: &crate::disc::OpticalDrive| {
            if last_drive.borrow().as_deref() != Some(drive.id.as_str()) {
                *last_drive.borrow_mut() = Some(drive.id.clone());
                search_entry.set_text("");
            }
            // Data-disc file browser: hidden/cleared unconditionally up front.
            // The non-audio branch below re-shows/refills it for a data disc;
            // without this, a data->audio swap on the same drive (e.g. via
            // the fingerprint auto-refresh re-populating this same drive)
            // left the stale file browser visible under the new track list,
            // its rows still pointing at the now-unmounted data disc.
            files_scroll.set_visible(false);
            add_all_btn.set_visible(false);
            files_store.remove_all();
            // Header icon reflects the loaded media (badge included).
            while let Some(child) = icon_box.first_child() {
                icon_box.remove(&child);
            }
            icon_box.append(&disc::disc_card_icon(drive));
            title.set_text(&gtk_safe(&drive.label));
            media_lbl.set_text(&drive.media_summary());
            while let Some(child) = track_list.first_child() {
                track_list.remove(&child);
            }
            let mut entries = crate::disc::toc::track_entries(drive);
            // Overlay stored gnudb/edited titles + surface "Artist — Album".
            let discid = drive.toc.as_ref().map(crate::disc::discid::freedb_discid);
            let mut header: Option<String> = None;
            if let Some(id) = &discid {
                // Prefer a real gnudb/user entry; fall back to CD-TEXT read
                // off the disc for discs gnudb doesn't know (e.g. our own
                // burns). Same overlay for both.
                let entry = disc_tags
                    .borrow()
                    .get(id)
                    .cloned()
                    .or_else(|| disc_cdtext.borrow().get(id).cloned());
                if let Some(tags) = entry {
                    for e in &mut entries {
                        if let Some(t) = tags.track_titles.get(e.number as usize - 1) {
                            if !t.is_empty() {
                                e.title = t.clone();
                            }
                        }
                    }
                    if !tags.artist.is_empty() || !tags.album.is_empty() {
                        // Same shape as the macOS drive header:
                        // "Artist — Album (year)", each part optional.
                        let mut h = tags.artist.clone();
                        if !tags.album.is_empty() {
                            h.push_str(&format!(" — {}", tags.album));
                        }
                        if !tags.year.is_empty() {
                            h.push_str(&format!(" ({})", tags.year));
                        }
                        header = Some(h);
                    }
                } else if drive.media.is_audio_cd
                    && !disc_cdtext_tried.borrow().contains(id)
                {
                    // First time we've shown this unknown audio disc: read its
                    // CD-TEXT off-thread (guarded — it spins the drive), cache
                    // it, and re-render. `_tried` guarantees one attempt only.
                    disc_cdtext_tried.borrow_mut().insert(id.clone());
                    let id2 = id.clone();
                    let drive_id = drive.id.clone();
                    let cdtext = disc_cdtext.clone();
                    let holder = populate_holder.clone();
                    let drives = current_drives_ct.clone();
                    glib::spawn_future_local(async move {
                        let id_for_read = id2.clone();
                        let drive_id_for_read = drive_id.clone();
                        let result = gio::spawn_blocking(move || {
                            crate::disc::detect::begin_exclusive_read();
                            let r = crate::disc::cdtext::read_cdtext(&drive_id_for_read);
                            crate::disc::detect::end_exclusive_read();
                            r.map(|cd| cd.to_xmcd(&id_for_read))
                        })
                        .await
                        .ok()
                        .flatten();
                        if let Some(x) = result {
                            cdtext.borrow_mut().insert(id2.clone(), x);
                            // Re-render only if that drive is still shown.
                            let still =
                                drives.borrow().iter().find(|d| d.id == drive_id).cloned();
                            if let (Some(d), Some(p)) =
                                (still, holder.borrow().clone())
                            {
                                p(&d);
                            }
                        }
                    });
                }
            }
            match &header {
                Some(h) => {
                    tag_lbl.set_text(&gtk_safe(h));
                    tag_lbl.set_visible(true);
                }
                None => tag_lbl.set_visible(false),
            }
            if drive.media.is_audio_cd && !entries.is_empty() {
                banner.set_visible(false);
                search_row.set_visible(true);
                tracks_scroll.set_visible(true);
                actions.set_visible(true);
                for b in &audio_btns {
                    b.set_visible(true);
                }
                eject_btn.set_visible(true);
                // Submit only makes sense with something to send: the disc is
                // unknown to gnudb, or the tags differ from the official match.
                submit_btn.set_visible(discid.as_ref().is_some_and(|id| {
                    disc::disc_submittable(id, &disc_tags.borrow(), &disc_official.borrow())
                }));
                // Audio discs get the play view. A REWRITABLE audio disc
                // (CD-RW/DVD-RW/DVD-RAM) also gets the burn panel below it, so
                // it can be erased and re-burned — its erase-confirm handles
                // wiping the audio content. A write-once audio CD-R stays
                // play-only (erase_decision == Refuse), matching the old
                // behaviour (2026-07-17).
                let burnable = crate::disc::burn::erase_decision(drive)
                    != crate::disc::burn::EraseDecision::Refuse;
                if burnable {
                    burn_ui.refresh(drive);
                }
                burn_ui.root.set_visible(burnable);
                for e in &entries {
                    let (m, s) = (e.duration_secs / 60, e.duration_secs % 60);
                    // Show the real title once known; otherwise the placeholder.
                    let disp = if e.title == format!("Track {}", e.number) {
                        format!("Track {} — {}:{:02}", e.number, m, s)
                    } else {
                        format!("{}. {} — {}:{:02}", e.number, e.title.replace(" / ", " - "), m, s)
                    };
                    let row_lbl = Label::builder()
                        .label(&gtk_safe(&disp))
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(8)
                        .margin_end(8)
                        .margin_top(4)
                        .margin_bottom(4)
                        .build();
                    let row = ListBoxRow::new();
                    row.set_child(Some(&row_lbl));
                    track_list.append(&row);
                }
            } else {
                search_row.set_visible(false);
                tracks_scroll.set_visible(false);
                // A loaded non-audio disc still gets Eject; the audio actions
                // make no sense for it.
                actions.set_visible(drive.media.present);
                for b in &audio_btns {
                    b.set_visible(false);
                }
                submit_btn.set_visible(false);
                eject_btn.set_visible(drive.media.present);
                tag_lbl.set_visible(false);
                // Present + not blank covers both a true data disc and an
                // audio disc whose TOC came back empty — same boundary the
                // banner text below already drew; `ensure_mounted` degrades
                // the latter to a clean "couldn't read disc" status instead
                // of a crash (it isn't a mountable filesystem).
                let is_data_disc = drive.media.present && !drive.media.is_blank;
                let msg = if !drive.media.present {
                    "No disc in the drive. Insert an audio CD to play its tracks."
                } else if drive.media.is_blank {
                    "Blank disc — ready to burn."
                } else {
                    "Data disc — browse, play, and add its files to your library below."
                };
                banner.set_text(msg);
                banner.set_visible(true);
                files_scroll.set_visible(is_data_disc);
                add_all_btn.set_visible(is_data_disc);
                if is_data_disc {
                    load_files(drive.clone());
                } else {
                    files_store.remove_all();
                }
                // Burn panel for writable/loaded non-audio media (blank,
                // RW-with-content, data disc); hidden on an empty tray.
                if drive.media.present {
                    burn_ui.refresh(drive);
                }
                burn_ui.root.set_visible(drive.media.present);
            }
            *entries_store.borrow_mut() = entries;
            // Fresh rows + fresh entries: re-run the search filter over them.
            track_list.invalidate_filter();
            // Overlay card: shows iff this drive has a live burn in the
            // shared progress map (Task 7) — restores the last-known
            // phase/fraction immediately; the burn poller's own 200 ms tick
            // resumes the pulse animation right after, if indeterminate.
            burn_ui.refresh_progress(&drive.id);
        })
    };
    // Let the async CD-TEXT read re-render the shown drive once it resolves.
    *populate_holder.borrow_mut() = Some(populate_disc_detail.clone());

    // Store a disc's tags (user set + optional official baseline), persist to
    // the shared store, refresh the detail if it's showing that disc, and push
    // the new titles/artist/album into already-added playlist rows.
    #[allow(clippy::type_complexity)]
    let commit_disc_tags: Rc<
        dyn Fn(String, crate::disc::xmcd::XmcdEntry, Option<crate::disc::xmcd::XmcdEntry>),
    > = {
        let disc_tags = disc_tags.clone();
        let disc_official = disc_official.clone();
        let state = state.clone();
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let populate = populate_disc_detail.clone();
        let entries_store = current_disc_entries.clone();
        let rebuild = rebuild_playlist.clone();
        Rc::new(move |discid: String, user: crate::disc::xmcd::XmcdEntry, official| {
            disc_tags.borrow_mut().insert(discid.clone(), user.clone());
            if let Some(o) = official {
                disc_official.borrow_mut().insert(discid.clone(), o);
            }
            // Persist (user set + the untouched official baseline for submit).
            {
                let mut store = crate::disc::tagstore::DiscTagStore::load();
                let off = disc_official.borrow().get(&discid).cloned();
                store.set(&discid, user, off);
                store.save();
            }
            // Only refresh/propagate when the committed disc is on screen.
            let showing = selected_disc_discid(&selected_disc_id, &current_drives)
                .map(|(_, id)| id == discid)
                .unwrap_or(false);
            if !showing {
                return;
            }
            if let Some(id) = selected_disc_id.borrow().clone() {
                if let Some(drive) = current_drives.borrow().iter().find(|d| d.id == id).cloned() {
                    populate(&drive);
                }
            }
            // Path-keyed propagation to already-added playlist rows, using the
            // same sampler " / " split as add_disc_entries.
            let (disc_artist, disc_album) = disc_tags
                .borrow()
                .get(&discid)
                .map(|t| (t.artist.clone(), t.album.clone()))
                .unwrap_or_default();
            let updates: Vec<(String, String, String)> = entries_store
                .borrow()
                .iter()
                .map(|e| {
                    let meta = crate::disc::track_meta(&e.title, &disc_artist);
                    (e.path.clone(), meta.title, meta.artist)
                })
                .collect();
            {
                let mut s = state.borrow_mut();
                for track in &mut s.playlist.tracks {
                    let tp = track.path.display().to_string();
                    if let Some((_, title, artist)) = updates.iter().find(|(p, _, _)| *p == tp) {
                        track.title = title.clone();
                        track.artist = artist.clone();
                        track.album = disc_album.clone();
                    }
                }
            }
            rebuild();
        })
    };

    // Overview cards (one per drive); clicking a card opens that drive's detail.
    let rebuild_disc_overview: Rc<dyn Fn()> = {
        let drives = current_drives.clone();
        let list = disc_overview_list.clone();
        let sidebar_ov = sidebar.clone();
        let detecting = disc_detecting.clone();
        Rc::new(move || {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let ds = drives.borrow();
            if ds.is_empty() {
                if detecting.get() {
                    // Still running the first poll: show a working indicator.
                    let row = GtkBox::new(Orientation::Horizontal, 8);
                    let spinner = gtk4::Spinner::new();
                    spinner.start();
                    let lbl = Label::builder()
                        .label("Detecting disc drives…")
                        .halign(Align::Start)
                        .xalign(0.0)
                        .build();
                    lbl.add_css_class("dim-label");
                    row.append(&spinner);
                    row.append(&lbl);
                    list.append(&row);
                } else {
                    let empty = Label::builder()
                        .label("No disc drives connected")
                        .halign(Align::Start)
                        .xalign(0.0)
                        .build();
                    empty.add_css_class("dim-label");
                    list.append(&empty);
                }
                return;
            }
            for d in ds.iter() {
                // Card: disc glyph (format badge overlaid) + the text column.
                let card = GtkBox::new(Orientation::Horizontal, 10);
                card.set_margin_top(4);
                card.set_margin_bottom(4);
                let icon = disc::disc_card_icon(d);
                icon.set_valign(Align::Center);
                card.append(&icon);
                let text_col = GtkBox::new(Orientation::Vertical, 4);
                let name = Label::builder()
                    .label(&gtk_safe(&d.label))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                let state_lbl = Label::builder()
                    .label(&d.media_summary())
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                state_lbl.add_css_class("dim-label");
                text_col.append(&name);
                text_col.append(&state_lbl);
                if let Some(detail) = disc_overview_detail_line(d) {
                    let dl = Label::builder()
                        .label(&detail)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .build();
                    dl.add_css_class("dim-label");
                    text_col.append(&dl);
                }
                card.append(&text_col);
                let gesture = GestureClick::new();
                let sidebar_c = sidebar_ov.clone();
                let target = format!("disc:{}", d.id);
                gesture.connect_released(move |_, _, _, _| {
                    if let Some(r) = find_row_by_name(&sidebar_c, &target) {
                        sidebar_c.select_row(Some(&r));
                    }
                });
                card.add_controller(gesture);
                list.append(&card);
            }
        })
    };

    // Poll every optical drive off the UI thread (detection shells out to
    // cd-info). Diff the sidebar rows in place, keeping selection stable.
    let refresh_discs: Rc<dyn Fn()> = {
        let sidebar = sidebar.clone();
        let disc_sub_rows = disc_sub_rows.clone();
        let discs_expanded = discs_expanded.clone();
        let current_drives = current_drives.clone();
        let selected_disc_id = selected_disc_id.clone();
        let burn_queues = burn_queues.clone();
        let burn_refresh_holder = burn_refresh_holder.clone();
        let rebuild_overview = rebuild_disc_overview.clone();
        let populate_detail = populate_disc_detail.clone();
        let state = state.clone();
        let disc_detecting = disc_detecting.clone();
        let disc_detect_spinner = disc_detect_spinner.clone();
        let rip_active = rip_active.clone();
        let disconnect_row = disc_disconnect_row.clone();
        let disconnect_lbl = disc_disconnect_lbl.clone();
        let entries_store = current_disc_entries.clone();
        let disc_status_lbl = disc_status_lbl.clone();
        let win_wk = win.downgrade();
        let in_flight = Rc::new(Cell::new(false));
        let disc_fingerprints: Rc<RefCell<std::collections::HashMap<String, u64>>> = Rc::new(RefCell::new(std::collections::HashMap::new()));
        Rc::new(move || {
            if in_flight.get() {
                return;
            }
            // Never run cd-info on a drive we're actively reading from — cdiocddasrc
            // (playback OR a rip) seeks the same head, and the device only allows
            // one reader, so a concurrent cd-info thrashes it. Skip while a cdda://
            // track plays, a rip is in progress, or `disc_reading` is set (burn,
            // and the data-disc browse/import mount+walk, both flip it for their
            // duration — a full probe landing mid-burn or mid-mount is the same
            // hardware hazard as the cases above, just on whichever drive that
            // scope owns rather than necessarily this one).
            {
                let s = state.borrow();
                let playing_disc = !matches!(s.player.state(), PlayerState::Stopped)
                    && s
                        .playlist
                        .current()
                        .map(|t| t.path.to_string_lossy().starts_with("cdda://"))
                        .unwrap_or(false);
                if playing_disc || rip_active.get() || s.disc_reading.get() {
                    // Not detecting right now — clear any spinner a show/map set.
                    disc_detect_spinner.stop();
                    disc_detect_spinner.set_visible(false);
                    return;
                }
            }
            in_flight.set(true);
            let sidebar = sidebar.clone();
            let disc_sub_rows = disc_sub_rows.clone();
            let discs_expanded = discs_expanded.clone();
            let current_drives = current_drives.clone();
            let selected_disc_id = selected_disc_id.clone();
            let burn_queues = burn_queues.clone();
            let burn_refresh_holder = burn_refresh_holder.clone();
            let rebuild_overview = rebuild_overview.clone();
            let populate_detail = populate_detail.clone();
            let disc_detecting = disc_detecting.clone();
            let disc_detect_spinner = disc_detect_spinner.clone();
            let state = state.clone();
            let disconnect_row = disconnect_row.clone();
            let disconnect_lbl = disconnect_lbl.clone();
            let entries_store = entries_store.clone();
            let disc_status_lbl = disc_status_lbl.clone();
            let win_wk = win_wk.clone();
            let in_flight = in_flight.clone();
            let disc_fingerprints = disc_fingerprints.clone();
            glib::spawn_future_local(async move {
                // Shared cached poll: an unchanged loaded disc is answered by
                // the kernel status ioctl and NOT re-probed (probing touches
                // the drive), and the cache is shared with the insertion
                // watcher so a new disc is probed exactly once.
                let result =
                    gio::spawn_blocking(crate::disc::detect::list_drives_shared).await;
                in_flight.set(false);
                // First poll finished — drop the "Detecting…" hint + sidebar
                // spinner and show the real state.
                disc_detecting.set(false);
                disc_detect_spinner.stop();
                disc_detect_spinner.set_visible(false);
                let Ok(drives) = result else { return };
                let want: Vec<String> =
                    drives.iter().map(|d| format!("disc:{}", d.id)).collect();
                // Remove rows for drives that went away.
                disc_sub_rows.borrow_mut().retain(|r| {
                    let keep = want.contains(&r.widget_name().to_string());
                    if !keep {
                        sidebar.remove(r);
                    }
                    keep
                });
                let expanded = discs_expanded.get();
                for d in &drives {
                    let name = format!("disc:{}", d.id);
                    let label_text = if d.label.is_empty() {
                        d.id.clone()
                    } else {
                        d.label.clone()
                    };
                    let summary = d.media_summary();
                    let existing = disc_sub_rows
                        .borrow()
                        .iter()
                        .find(|r| r.widget_name().as_str() == name)
                        .cloned();
                    match existing {
                        Some(row) => {
                            // Keep the media-state line current (disc in/out).
                            if let Some(bx) =
                                row.child().and_then(|c| c.downcast::<GtkBox>().ok())
                            {
                                if let Some(lbl) =
                                    bx.last_child().and_then(|c| c.downcast::<Label>().ok())
                                {
                                    lbl.set_text(&summary);
                                }
                            }
                        }
                        None => {
                            let bx = GtkBox::new(Orientation::Vertical, 2);
                            bx.set_margin_start(24);
                            bx.set_margin_end(8);
                            bx.set_margin_top(4);
                            bx.set_margin_bottom(4);
                            let lbl = Label::builder()
                                .label(&gtk_safe(&label_text))
                                .halign(Align::Start)
                                .xalign(0.0)
                                .build();
                            let state_lbl = Label::builder()
                                .label(&summary)
                                .halign(Align::Start)
                                .xalign(0.0)
                                .build();
                            state_lbl.add_css_class("dim-label");
                            bx.append(&lbl);
                            bx.append(&state_lbl);
                            let row = ListBoxRow::new();
                            row.set_widget_name(&name);
                            row.set_child(Some(&bx));
                            row.set_visible(expanded);
                            // Drag-to-drive: dropping files straight onto a
                            // drive's sidebar row queues them, same as
                            // picking that drive from any "Send to ▾"
                            // menu. No capacity gate at drop (spec) — the
                            // burn panel is where over-capacity is caught.
                            {
                                let dt = DropTarget::new(
                                    gdk::FileList::static_type(),
                                    gdk::DragAction::COPY,
                                );
                                let drive_id = d.id.clone();
                                let current_drives_dt = current_drives.clone();
                                let state_dt = state.clone();
                                let burn_queues_dt = burn_queues.clone();
                                let burn_refresh_holder_dt = burn_refresh_holder.clone();
                                let status_dt = disc_status_lbl.clone();
                                let win_wk_dt = win_wk.clone();
                                dt.connect_drop(move |_, value, _x, _y| {
                                    let Ok(file_list) = value.get::<gdk::FileList>() else {
                                        return false;
                                    };
                                    let paths: Vec<std::path::PathBuf> = file_list
                                        .files()
                                        .iter()
                                        .filter_map(|f| f.path())
                                        .collect();
                                    if paths.is_empty() {
                                        return false;
                                    }
                                    let drive_label = current_drives_dt
                                        .borrow()
                                        .iter()
                                        .find(|dr| dr.id == drive_id)
                                        .map(|dr| dr.label.clone())
                                        .unwrap_or_else(|| drive_id.clone());
                                    // Metadata from the library NOW (SQLite
                                    // is not Send) — same lookup the files
                                    // action uses, with a filename fallback.
                                    let metas: std::collections::HashMap<_, _> = {
                                        let s = state_dt.borrow();
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
                                    let status_cl = status_dt.clone();
                                    queue_paths_to_drive(
                                        drive_id.clone(),
                                        drive_label,
                                        paths,
                                        metas,
                                        burn_queues_dt.clone(),
                                        burn_refresh_holder_dt.clone(),
                                        Rc::new(move |s: String| {
                                            status_cl.set_text(&gtk_safe(&s));
                                        }),
                                        win_wk_dt.clone(),
                                    );
                                    true
                                });
                                row.add_controller(dt);
                            }
                            // Insert between the Disc Drives and Devices headers
                            // so disc rows stay grouped above the device rows.
                            let at = find_row_by_name(&sidebar, "devices")
                                .map(|r| r.index())
                                .unwrap_or(-1);
                            sidebar.insert(&row, at);
                            disc_sub_rows.borrow_mut().push(row);
                        }
                    }
                }
                // Unplug fallback (Phase 7): the drive being viewed vanished —
                // invalidate the loaded-disc session (entries cleared, so
                // nothing stale can be added/ripped), return to the discs
                // overview, and say so in the dismissible banner instead of
                // silently dropping out. In-flight subprocess ops die with
                // the device (unchanged).
                // Snapshot the selected drive id ONCE. Holding a borrow on
                // selected_disc_id across sidebar.select_row() below would
                // re-enter connect_row_selected (which borrow_muts the same
                // cell) and abort with "RefCell already borrowed" — hit live
                // when hot-plugging a drive (2026-07-16).
                let sel_now = selected_disc_id.borrow().clone();
                if let Some(sel) = sel_now.clone() {
                    if !drives.iter().any(|d| d.id == sel) {
                        entries_store.borrow_mut().clear();
                        disconnect_lbl.set_text(
                            "Drive disconnected — reconnect it to continue with the disc.",
                        );
                        disconnect_row.set_visible(true);
                        if let Some(r) = find_row_by_name(&sidebar, "discs") {
                            sidebar.select_row(Some(&r));
                        }
                    }
                }
                // If the drive being viewed changed state (disc ejected,
                // inserted, or swapped), repopulate the open detail view —
                // otherwise it keeps showing the previous disc's tracks.
                // Unchanged drives skip this so the 10 s poll never disturbs
                // the user's row selection.
                let mut detail_update: Option<crate::disc::OpticalDrive> = sel_now
                    .clone()
                    .and_then(|sel| {
                        let new_d = drives.iter().find(|d| d.id == sel).cloned()?;
                        let old_d = current_drives
                            .borrow()
                            .iter()
                            .find(|d| d.id == sel)
                            .cloned();
                        (old_d.as_ref() != Some(&new_d)).then_some(new_d)
                    });
                // Disc-swap auto-refresh: use fingerprints to catch changes the
                // equality check missed. Snapshot the selected id and old
                // fingerprint ONCE before any updates (borrow-discipline).
                if detail_update.is_none() {
                    if let Some(sel) = sel_now {
                        let old_fp = disc_fingerprints.borrow().get(&sel).copied();
                        if let Some(new_d) = drives.iter().find(|d| d.id == sel).cloned() {
                            let new_fp = crate::disc::detect::media_fingerprint(&new_d);
                            if old_fp.is_some() && Some(new_fp) != old_fp {
                                detail_update = Some(new_d);
                            }
                        }
                    }
                }
                *current_drives.borrow_mut() = drives.clone();
                // Store fingerprints for all drives for next poll.
                {
                    let mut fps = disc_fingerprints.borrow_mut();
                    fps.clear();
                    for d in &drives {
                        fps.insert(d.id.clone(), crate::disc::detect::media_fingerprint(d));
                    }
                }
                // Drop burn queues for drives that are no longer attached —
                // they'd otherwise linger invisibly (no panel shows them).
                {
                    let drives = current_drives.borrow();
                    let live: Vec<&str> = drives.iter().map(|d| d.id.as_str()).collect();
                    burn_queues.borrow_mut().remove_gone(&live);
                }
                rebuild_overview();
                if let Some(d) = detail_update {
                    populate_detail(&d);
                }
                // Auto-open navigation: the insertion watcher parked a drive
                // id — jump to it now that its sidebar row exists. A request
                // whose drive this refresh doesn't know is dropped (the disc
                // was pulled again); the watcher parks a fresh one next time.
                // Take the parked nav id out BEFORE select_row so the state
                // borrow doesn't span the row-selected callback (same
                // re-entrancy hazard as the disconnect path above).
                let pending_nav = state.borrow_mut().pending_disc_nav.take();
                if let Some(id) = pending_nav {
                    if let Some(r) = find_row_by_name(&sidebar, &format!("disc:{id}")) {
                        sidebar.select_row(Some(&r));
                    }
                }
            });
        })
    };

    // Selecting a drive (or the Disc Drives header) shows the discs page.
    {
        let stack_ref = stack.clone();
        let drives = current_drives.clone();
        let overview = disc_overview.clone();
        let detail = disc_detail.clone();
        let populate = populate_disc_detail.clone();
        let rebuild_overview = rebuild_disc_overview.clone();
        let sel_id = selected_disc_id.clone();
        let exp = discs_expanded.clone();
        let disconnect_row = disc_disconnect_row.clone();
        let burn_ui = burn_ui.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let Some(row) = opt_row else { return };
            let name = row.widget_name().to_string();
            if name == "discs" {
                stack_ref.set_visible_child_name("discs");
                rebuild_overview();
                overview.set_visible(true);
                detail.set_visible(false);
                // No drive shown — nothing for the overlay to key off (a
                // background burn is still running; it re-shows once its
                // drive is selected again, via `populate`'s refresh_progress).
                burn_ui.overlay_card.set_visible(false);
                *sel_id.borrow_mut() = None;
                if !exp.get() {
                    exp.set(true);
                }
            } else if let Some(id) = name.strip_prefix("disc:") {
                stack_ref.set_visible_child_name("discs");
                if let Some(d) = drives.borrow().iter().find(|d| d.id == id) {
                    // Opening a drive supersedes any disconnect notice.
                    disconnect_row.set_visible(false);
                    overview.set_visible(false);
                    detail.set_visible(true);
                    populate(d);
                    *sel_id.borrow_mut() = Some(id.to_string());
                }
            }
        });
    }

    // Playlist actions: ▶ Play / Enqueue act on the selected rows, or the
    // whole disc when nothing is selected (a whole-disc play is the common
    // case); a double-clicked row honors the add-behavior setting, like the
    // ML files double-click.
    let picked_disc_entries: Rc<dyn Fn() -> Vec<crate::disc::DiscTrackEntry>> = {
        let entries = current_disc_entries.clone();
        let track_list = disc_track_list.clone();
        Rc::new(move || {
            let sel = track_list.selected_rows();
            let all = entries.borrow();
            if sel.is_empty() {
                all.clone()
            } else {
                sel.iter()
                    .filter_map(|r| all.get(r.index() as usize).cloned())
                    .collect()
            }
        })
    };
    {
        let picked = picked_disc_entries.clone();
        let add = add_disc_entries.clone();
        disc_play.connect_clicked(move |_| {
            add(&picked(), DiscAdd::PlayNow);
        });
    }
    {
        let picked = picked_disc_entries.clone();
        let add = add_disc_entries.clone();
        disc_enqueue.connect_clicked(move |_| {
            add(&picked(), DiscAdd::Enqueue);
        });
    }
    {
        let entries = current_disc_entries.clone();
        let add = add_disc_entries.clone();
        disc_track_list.connect_row_activated(move |_, row| {
            if let Some(e) = entries.borrow().get(row.index() as usize).cloned() {
                add(&[e], DiscAdd::Behavior);
            }
        });
    }

    // ── gnudb identify + tag override (Phase 2) ─────────────────────────────
    // Fetch one chosen match in the background, parse its xmcd, and commit it as
    // both the user tags and the official (submission-baseline) copy.
    let apply_disc_match: Rc<dyn Fn(String, String, String)> = {
        let state = state.clone();
        let commit = commit_disc_tags.clone();
        let status = disc_status_lbl.clone();
        Rc::new(move |discid: String, category: String, matched_id: String| {
            let email = state.borrow().config.disc.gnudb_email.clone();
            status.set_text("Fetching entry…");
            let commit = commit.clone();
            let status = status.clone();
            glib::spawn_future_local(async move {
                let res = gio::spawn_blocking(move || {
                    match crate::disc::gnudb::read(&category, &matched_id, &email) {
                        Ok(text) => crate::disc::xmcd::parse(&text)
                            .ok_or_else(|| "gnudb entry was unreadable".to_string()),
                        Err(e) => Err(e.to_string()),
                    }
                })
                .await;
                match res {
                    Ok(Ok(entry)) => {
                        let label = format!("{} — {}", entry.artist, entry.album);
                        commit(discid, entry.clone(), Some(entry));
                        status.set_text(&gtk_safe(&label));
                    }
                    Ok(Err(msg)) => status.set_text(&gtk_safe(&msg)),
                    Err(_) => status.set_text("gnudb lookup failed"),
                }
            });
        })
    };

    // Modal picker for an inexact/multi-candidate match list.
    let open_match_picker: Rc<dyn Fn(String, Vec<crate::disc::gnudb::DiscMatch>)> = {
        let apply = apply_disc_match.clone();
        let win_wk = win.downgrade();
        Rc::new(move |discid: String, matches: Vec<crate::disc::gnudb::DiscMatch>| {
            let dialog = gtk4::Window::builder()
                .title("Choose a gnudb match")
                .modal(true)
                .default_width(440)
                .default_height(320)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let list = gtk4::ListBox::new();
            list.set_selection_mode(gtk4::SelectionMode::Single);
            for m in &matches {
                let text = format!("{}{}", m.title, if m.exact { "  (exact)" } else { "" });
                let lbl = Label::builder()
                    .label(&gtk_safe(&text))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .margin_start(6)
                    .margin_end(6)
                    .margin_top(4)
                    .margin_bottom(4)
                    .build();
                let row = ListBoxRow::new();
                row.set_child(Some(&lbl));
                list.append(&row);
            }
            list.select_row(list.row_at_index(0).as_ref());
            let scroll = ScrolledWindow::builder().vexpand(true).child(&list).build();
            vbox.append(&scroll);
            let btns = GtkBox::new(Orientation::Horizontal, 6);
            btns.set_halign(Align::End);
            let cancel = Button::with_label("Cancel");
            let ok = Button::with_label("Use This");
            ok.add_css_class("suggested-action");
            btns.append(&cancel);
            btns.append(&ok);
            vbox.append(&btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel.connect_clicked(move |_| d.close());
            let d = dialog.clone();
            let apply = apply.clone();
            ok.connect_clicked(move |_| {
                let idx = list.selected_row().map(|r| r.index()).unwrap_or(-1);
                if idx >= 0 {
                    if let Some(m) = matches.get(idx as usize) {
                        apply(discid.clone(), m.category.clone(), m.discid.clone());
                    }
                }
                d.close();
            });
            dialog.present();
        })
    };

    // The actual gnudb query, factored out so the email prompt can retry it.
    // Single exact match auto-applies; several open the picker; none points the
    // user at Edit Tags. Never blocks the UI.
    let run_identify: Rc<dyn Fn()> = {
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let state = state.clone();
        let status = disc_status_lbl.clone();
        let apply = apply_disc_match.clone();
        let picker = open_match_picker.clone();
        let identify_btn = disc_identify.clone();
        Rc::new(move || {
            let Some((toc, discid)) = selected_disc_discid(&selected_disc_id, &current_drives)
            else {
                status.set_text("No audio disc to identify");
                return;
            };
            let email = state.borrow().config.disc.gnudb_email.clone();
            status.set_text("Asking gnudb…");
            identify_btn.set_sensitive(false);
            let status = status.clone();
            let apply = apply.clone();
            let picker = picker.clone();
            let identify_btn2 = identify_btn.clone();
            glib::spawn_future_local(async move {
                let res =
                    gio::spawn_blocking(move || crate::disc::gnudb::query(&toc, &email)).await;
                identify_btn2.set_sensitive(true);
                match res {
                    Ok(Ok(matches)) if matches.is_empty() => {
                        status.set_text("No gnudb match — use Edit Tags to fill them in.");
                    }
                    Ok(Ok(matches)) if matches.len() == 1 && matches[0].exact => {
                        let m = &matches[0];
                        apply(discid, m.category.clone(), m.discid.clone());
                    }
                    Ok(Ok(matches)) => picker(discid, matches),
                    Ok(Err(e)) => status.set_text(&gtk_safe(&e.to_string())),
                    Err(_) => status.set_text("gnudb lookup failed"),
                }
            });
        })
    };

    // Identify button: gnudb needs an email for its handshake, so collect one
    // (stored in Settings) before the first lookup when it's unset.
    {
        let state = state.clone();
        let status = disc_status_lbl.clone();
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let run_identify = run_identify.clone();
        let win_wk = win.downgrade();
        disc_identify.connect_clicked(move |_| {
            if selected_disc_discid(&selected_disc_id, &current_drives).is_none() {
                status.set_text("No audio disc to identify");
                return;
            }
            let email = state.borrow().config.disc.gnudb_email.clone();
            if crate::disc::gnudb::is_unset_email(&email) {
                // Prompt, store, then run the lookup with the entered address.
                prompt_gnudb_email(
                    win_wk.upgrade().as_ref(),
                    state.clone(),
                    run_identify.clone(),
                );
            } else {
                run_identify();
            }
        });
    }

    // Edit Tags: modal editor for disc fields + per-track titles, editable with
    // or without a match. Save commits, persists, overlays, and propagates.
    {
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let disc_tags = disc_tags.clone();
        let entries_store = current_disc_entries.clone();
        let commit = commit_disc_tags.clone();
        let status = disc_status_lbl.clone();
        let win_wk = win.downgrade();
        disc_edit_tags.connect_clicked(move |_| {
            let Some((_, discid)) = selected_disc_discid(&selected_disc_id, &current_drives) else {
                status.set_text("No audio disc loaded");
                return;
            };
            let stored = disc_tags.borrow().get(&discid).cloned();
            let entries = entries_store.borrow().clone();
            let dialog = gtk4::Window::builder()
                .title("Edit Disc Tags")
                .modal(true)
                .default_width(460)
                .default_height(500)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let outer = GtkBox::new(Orientation::Vertical, 8);
            outer.set_margin_top(12);
            outer.set_margin_bottom(12);
            outer.set_margin_start(12);
            outer.set_margin_end(12);
            let mk_field = |label: &str, val: &str| -> (GtkBox, Entry) {
                let row = GtkBox::new(Orientation::Horizontal, 8);
                let l = Label::builder()
                    .label(label)
                    .width_chars(7)
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                let e = Entry::new();
                e.set_hexpand(true);
                e.set_text(&gtk_safe(val));
                row.append(&l);
                row.append(&e);
                (row, e)
            };
            let (artist_row, artist_e) =
                mk_field("Artist", stored.as_ref().map(|s| s.artist.as_str()).unwrap_or(""));
            let (album_row, album_e) =
                mk_field("Album", stored.as_ref().map(|s| s.album.as_str()).unwrap_or(""));
            let (year_row, year_e) =
                mk_field("Year", stored.as_ref().map(|s| s.year.as_str()).unwrap_or(""));
            let (genre_row, genre_e) =
                mk_field("Genre", stored.as_ref().map(|s| s.genre.as_str()).unwrap_or(""));
            outer.append(&artist_row);
            outer.append(&album_row);
            outer.append(&year_row);
            outer.append(&genre_row);
            let sep = Label::builder()
                .label("Track titles (use \"Artist / Title\" for compilations)")
                .halign(Align::Start)
                .xalign(0.0)
                .build();
            sep.add_css_class("dim-label");
            outer.append(&sep);
            let title_box = GtkBox::new(Orientation::Vertical, 4);
            let mut title_entries: Vec<Entry> = Vec::new();
            for e in &entries {
                let idx = e.number as usize - 1;
                let init = stored
                    .as_ref()
                    .and_then(|s| s.track_titles.get(idx).cloned())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| {
                        if e.title == format!("Track {}", e.number) {
                            String::new()
                        } else {
                            e.title.clone()
                        }
                    });
                let row = GtkBox::new(Orientation::Horizontal, 8);
                let l = Label::builder()
                    .label(&format!("{}.", e.number))
                    .width_chars(3)
                    .halign(Align::Start)
                    .build();
                let ent = Entry::new();
                ent.set_hexpand(true);
                ent.set_text(&gtk_safe(&init));
                row.append(&l);
                row.append(&ent);
                title_box.append(&row);
                title_entries.push(ent);
            }
            let scroll = ScrolledWindow::builder().vexpand(true).child(&title_box).build();
            outer.append(&scroll);
            let btns = GtkBox::new(Orientation::Horizontal, 6);
            btns.set_halign(Align::End);
            let cancel = Button::with_label("Cancel");
            let save = Button::with_label("Save");
            save.add_css_class("suggested-action");
            btns.append(&cancel);
            btns.append(&save);
            outer.append(&btns);
            dialog.set_child(Some(&outer));
            let d = dialog.clone();
            cancel.connect_clicked(move |_| d.close());
            let d = dialog.clone();
            let commit = commit.clone();
            save.connect_clicked(move |_| {
                // Base on the stored entry so extd/extt/revision survive edits.
                let mut entry = stored.clone().unwrap_or_default();
                entry.discid = discid.clone();
                entry.artist = artist_e.text().to_string();
                entry.album = album_e.text().to_string();
                entry.year = year_e.text().to_string();
                entry.genre = genre_e.text().to_string();
                entry.track_titles =
                    title_entries.iter().map(|e| e.text().to_string()).collect();
                commit(discid.clone(), entry, None);
                d.close();
            });
            dialog.present();
        });
    }

    // ── Rip to MP3 (Phase 3) ────────────────────────────────────────────────
    // Dialog + worker live in the `disc` module; this wires the buttons to
    // the shared state and the progress widgets on the drive detail view.
    disc::connect_rip_ui(
        disc::DiscRipUi {
            state: state.clone(),
            rip_cancel: rip_cancel.clone(),
            rip_active: rip_active.clone(),
            rip_box: disc_rip_box.clone(),
            rip_bar: disc_rip_bar.clone(),
            status: disc_status_lbl.clone(),
        },
        &disc_rip,
        &disc_rip_cancel,
        &win,
        current_disc_entries.clone(),
        disc_tags.clone(),
        selected_disc_id.clone(),
        current_drives.clone(),
    );

    // Submit to gnudb (Phase 4): category picker + background POST; the
    // button's visibility (unknown disc / tags differ from the official
    // match) is maintained by populate_disc_detail.
    disc::connect_submit(
        &disc_submit,
        state.clone(),
        disc_status_lbl.clone(),
        &win,
        disc_tags.clone(),
        disc_official.clone(),
        selected_disc_id.clone(),
        current_drives.clone(),
    );

    // Eject: blocking subprocess off the UI thread, then re-poll the drives.
    disc::connect_eject(
        &disc_eject,
        state.clone(),
        rip_active.clone(),
        disc_status_lbl.clone(),
        selected_disc_id.clone(),
        refresh_discs.clone(),
    );

    // Let the app-level insertion watcher trigger an immediate re-poll (and
    // consume its pending navigation) instead of waiting for the window's
    // own cadence.
    state.borrow_mut().disc_refresh_callback = Some(refresh_discs.clone());
    // …and the burn panel too (a finished burn re-polls the disc's content).
    *refresh_discs_holder.borrow_mut() = Some(refresh_discs.clone());

    // Initial scan + 2 s poll (stops once the window/sidebar is gone). Cheap:
    // unchanged ticks are one status ioctl through the shared cache; only an
    // actual media change probes the drive.
    refresh_discs();
    {
        let refresh = refresh_discs.clone();
        let sidebar_weak = sidebar.downgrade();
        glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            if sidebar_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            refresh();
            glib::ControlFlow::Continue
        });
    }
    // Re-detect every time the window is shown (this ML window uses
    // hide-on-close, so it's reused across opens). Spinning the header spinner
    // here means the "detecting…" indicator is actually visible when the user
    // opens the Media Library, not only during the one-off build at startup.
    {
        let refresh = refresh_discs.clone();
        let spinner = disc_detect_spinner.clone();
        win.connect_map(move |_| {
            spinner.set_visible(true);
            spinner.start();
            refresh();
        });
    }

    // Selecting a device (or the Devices header) shows the devices page.
    {
        let stack_ref = stack.clone();
        let current = current_devices.clone();
        let title = dev_title.clone();
        let capacity = dev_capacity.clone();
        let levelbar = dev_levelbar.clone();
        let eject = dev_eject.clone();
        let sel_backend = selected_dev_backend.clone();
        let exp = devices_expanded.clone();
        let path_lbl = dev_path.clone();
        let overview = dev_overview.clone();
        let detail = dev_detail.clone();
        let warn = dev_warn.clone();
        let ro_badge = dev_ro_badge.clone();
        let warn_badge = dev_warn_badge.clone();
        let transfers_sel = device_transfers.clone();
        let rebuild_overview_sel = rebuild_overview.clone();
        let reload_dev_playlists_sel = reload_dev_playlists.clone();
        let reload_device_store_sel = reload_device_store.clone();
        let dev_named_cols_sel = dev_named_cols.clone();
        let dev_col_view_sel = dev_col_view.clone();
        let state_devcols = state.clone();
        let sync_btn = dev_sync.clone();
        let scan_btn = dev_scan.clone();
        // Sections hidden behind the "no filesystem" banner.
        let nofs_banner = dev_nofs_banner.clone();
        let nofs_lbl_sel = dev_nofs_lbl.clone();
        let pl_header_sel = dev_pl_header.clone();
        let pl_scroll_sel = dev_pl_scroll.clone();
        let pl_actions_sel = dev_pl_actions.clone();
        let tracks_scroll_sel = dev_tracks_scroll.clone();
        let file_actions_sel = dev_file_actions.clone();
        let store_sel = dev_store.clone();
        let counts_sel = dev_counts.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let Some(row) = opt_row else { return };
            let name = row.widget_name().to_string();
            if name == "devices" {
                // Overview mode: list every connected device.
                stack_ref.set_visible_child_name("devices");
                rebuild_overview_sel();
                overview.set_visible(true);
                detail.set_visible(false);
                *sel_backend.borrow_mut() = None;
                if !exp.get() {
                    exp.set(true);
                }
            } else if let Some(backend) = name.strip_prefix("dev:") {
                stack_ref.set_visible_child_name("devices");
                if let Some(d) = current.borrow().iter().find(|d| d.backend_id == backend) {
                    // Detail mode for the selected device.
                    overview.set_visible(false);
                    detail.set_visible(true);
                    // Re-apply the shared column config so device columns track
                    // changes made in the files view (same as the editor does).
                    apply_ml_columns_to(&dev_col_view_sel, &dev_named_cols_sel, &state_devcols, 1);
                    let base = if d.label.is_empty() {
                        "Untitled device".to_string()
                    } else {
                        d.label.clone()
                    };
                    // Name in the header; status shown as pill badges instead
                    // of inline glyphs.
                    title.set_text(&gtk_safe(&base));
                    path_lbl.set_text(&gtk_safe(&format!(
                        "{} · {}",
                        if d.fs_type.is_empty() { "unknown" } else { &d.fs_type },
                        d.mount_path.to_string_lossy(),
                    )));
                    ro_badge.set_visible(d.read_only);
                    let unsupported = device_fs_unsupported(&d.fs_type);
                    warn_badge.set_visible(unsupported);
                    let used_bytes = d.total_bytes.saturating_sub(d.free_bytes);
                    capacity.set_text(&format!(
                        "{:.1} GB used · {:.1} GB free · {:.1} GB total",
                        used_bytes as f64 / 1e9,
                        d.free_bytes as f64 / 1e9,
                        d.total_bytes as f64 / 1e9,
                    ));
                    if unsupported {
                        warn.set_text("⚠ NTFS/exFAT — limited support");
                        warn.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                        warn.set_visible(true);
                    } else {
                        warn.set_visible(false);
                    }
                    let unsupported_dev =
                        d.backend == crate::devices::DeviceBackend::Unsupported;
                    let used = if d.total_bytes > 0 {
                        1.0 - d.free_bytes as f64 / d.total_bytes as f64
                    } else {
                        0.0
                    };
                    levelbar.set_value(used);
                    set_levelbar_fullness(&levelbar, used);
                    // No capacity is knowable for a photo/iOS mount — hide the bar.
                    levelbar.set_visible(!unsupported_dev);
                    // Eject is unavailable while a copy to this device is running.
                    let busy = transfers_sel.borrow().contains_key(&d.backend_id);
                    eject.set_sensitive(d.ejectable && !busy);
                    sync_btn.set_sensitive(true);
                    scan_btn.set_sensitive(true);
                    *sel_backend.borrow_mut() = Some(d.backend_id.clone());

                    if unsupported_dev {
                        // Apple iOS / PTP photo device: detected, but not a music
                        // sync target. Explain why and disable Sync/Scan. Eject
                        // stays available so the user can disconnect cleanly.
                        warn.set_visible(false);
                        capacity.set_text("Capacity unavailable");
                        nofs_lbl_sel.set_text(unsupported_device_banner(&d.backend_id));
                        nofs_banner.set_visible(true);
                        pl_header_sel.set_visible(false);
                        pl_scroll_sel.set_visible(false);
                        pl_actions_sel.set_visible(false);
                        tracks_scroll_sel.set_visible(false);
                        file_actions_sel.set_visible(false);
                        store_sel.remove_all();
                        counts_sel.set_text("Not a music-sync device");
                        sync_btn.set_sensitive(false);
                        scan_btn.set_sensitive(false);
                    } else if d.fs_visible {
                        // Normal device: show the lists, hide the banner.
                        nofs_banner.set_visible(false);
                        pl_header_sel.set_visible(true);
                        pl_scroll_sel.set_visible(true);
                        tracks_scroll_sel.set_visible(true);
                        file_actions_sel.set_visible(true);
                        sync_btn.set_sensitive(true);
                        scan_btn.set_sensitive(true);

                        // Rebuild the playlist filter rows ("All files" + each
                        // device .m3u/.m3u8); selecting "All files" resets the
                        // filter via the playlist-list handler.
                        reload_dev_playlists_sel(d.clone());

                        // Read device tags off the UI thread, then fill columns.
                        reload_device_store_sel(d.clone());
                    } else {
                        // Connected but no readable filesystem: show the banner
                        // in place of empty lists. Eject stays available so the
                        // user can disconnect; Sync/Scan are pointless here.
                        nofs_lbl_sel.set_text(
                            "⚠ No visible filesystem on this device. Set the phone to \
                             file-transfer mode and allow access, or reconnect it, then \
                             press Scan.",
                        );
                        nofs_banner.set_visible(true);
                        pl_header_sel.set_visible(false);
                        pl_scroll_sel.set_visible(false);
                        pl_actions_sel.set_visible(false);
                        tracks_scroll_sel.set_visible(false);
                        file_actions_sel.set_visible(false);
                        store_sel.remove_all();
                        counts_sel.set_text("No visible filesystem");
                        sync_btn.set_sensitive(false);
                        scan_btn.set_sensitive(false);
                    }
                }
            }
        });
    }

    // Scan: re-read tags + duration from the files on the selected device, and
    // refresh the playlist chips. Same work the device-select does, on demand.
    {
        let devices_scan = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        let reload_store = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        dev_scan.connect_clicked(move |_| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            let dev = devices_scan
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned();
            let Some(dev) = dev else { return };
            reload_pls(dev.clone());
            reload_store(dev);
        });
    }

    // Eject: unmount + power off a device, then refresh the list. Shared by
    // the detail Eject button and each overview row's Eject button.
    let eject_run: Rc<dyn Fn(String)> = {
        let refresh = refresh_devices.clone();
        let sidebar_ej = sidebar.clone();
        let win_wk_ej = win.downgrade();
        Rc::new(move |backend: String| {
            let refresh = refresh.clone();
            let sidebar_ej = sidebar_ej.clone();
            let win_wk = win_wk_ej.clone();
            // MTP devices have no udisks2 block object — unmount through gvfs
            // (gio) on the main thread instead; the unmount itself is async.
            if backend.starts_with("mtp://") || backend.starts_with("gphoto2://") {
                // Forget cached metadata so a later replug of the same URI
                // re-reads the device rather than showing stale capacity.
                invalidate_mtp_meta(&backend);
                let monitor = gio::VolumeMonitor::get();
                let mount = monitor
                    .mounts()
                    .into_iter()
                    .find(|m| m.root().uri() == backend);
                let Some(mount) = mount else {
                    refresh();
                    return;
                };
                let refresh2 = refresh.clone();
                let sidebar2 = sidebar_ej.clone();
                let win2 = win_wk.clone();
                mount.unmount_with_operation(
                    gio::MountUnmountFlags::NONE,
                    None::<&gio::MountOperation>,
                    gio::Cancellable::NONE,
                    move |res| match res {
                        Ok(()) => {
                            refresh2();
                            if let Some(r) = find_row_by_name(&sidebar2, "devices") {
                                sidebar2.select_row(Some(&r));
                            }
                        }
                        Err(e) => {
                            show_alert_parented(
                                win2.upgrade().as_ref(),
                                &format!(
                                    "Couldn't disconnect the device ({e}). Close anything \
                                     using it and try again."
                                ),
                            );
                        }
                    },
                );
                return;
            }
            // Run the unmount/power-off on a worker thread so a busy device
            // can't freeze the UI.
            glib::spawn_future_local(async move {
                let res =
                    gio::spawn_blocking(move || crate::devices::detect::eject(&backend)).await;
                match res {
                    Ok(Ok(())) => {
                        refresh();
                        // The detail view may now show a device that's gone —
                        // return to the Devices overview.
                        if let Some(r) = find_row_by_name(&sidebar_ej, "devices") {
                            sidebar_ej.select_row(Some(&r));
                        }
                    }
                    Ok(Err(e)) => {
                        let dialog = gtk4::AlertDialog::builder()
                            .message("Couldn't eject")
                            .detail(format!(
                                "The device is still busy or couldn't be ejected ({e}). \
                                 Close anything using it and try again, or eject it from \
                                 your file browser."
                            ))
                            .modal(true)
                            .build();
                        dialog.show(win_wk.upgrade().as_ref());
                    }
                    Err(_) => {
                        show_alert_parented(
                            win_wk.upgrade().as_ref(),
                            "Eject failed unexpectedly.",
                        );
                    }
                }
            });
        })
    };
    *eject_run_holder.borrow_mut() = Some(eject_run.clone());
    {
        let sel_backend = selected_dev_backend.clone();
        let eject_run = eject_run.clone();
        dev_eject.connect_clicked(move |btn| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            btn.set_sensitive(false);
            eject_run(backend);
        });
    }

    // Sync: compare tags on each side of every pair, confirm en masse, apply.
    // Shared by the detail Sync button and each overview row's Sync button.
    let sync_run: Rc<dyn Fn(crate::devices::Device, Button)> = {
        let state_sync = state.clone();
        let win_wk = win.downgrade();
        let reload_sync = reload_device_store.clone();
        Rc::new(move |dev: crate::devices::Device, sync_btn: Button| {
            use crate::devices::sync::{PlaylistSyncDir, SyncAction};
            // Show activity while the device is read/planned (slow over MTP);
            // restored on every exit path below, just before a dialog/alert.
            set_button_busy(&sync_btn, true, "Sync");
            // Compute both sync plans on a worker thread — reading device tags
            // and playlist files over a slow MTP FUSE mount on the UI thread
            // froze the app. A throwaway read-only library handle is opened on
            // that thread (same pattern as the scan workers).
            let ext = state_sync
                .borrow()
                .config
                .media_library
                .playlist_format
                .extension()
                .to_string();
            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            let state_sync = state_sync.clone();
            let win_wk = win_wk.clone();
            let reload_sync = reload_sync.clone();
            glib::spawn_future_local(async move {
                let dev_b = dev.clone();
                let (plan, pl_plan) = gio::spawn_blocking(move || {
                    if device_io_shutting_down() {
                        return (Vec::new(), Vec::new());
                    }
                    match crate::media_library::MediaLibrary::open_at(&db_path) {
                        Ok(lib) => (
                            device_sync_plan(&lib, &dev_b),
                            device_playlist_sync_plan(&lib, &dev_b, &ext),
                        ),
                        Err(_) => (Vec::new(), Vec::new()),
                    }
                })
                .await
                .unwrap_or((Vec::new(), Vec::new()));
            let to_lib = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::DeviceToLibrary)
                .count();
            let to_dev = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::LibraryToDevice)
                .count();
            let song_conflict = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::Conflict)
                .count();
            let pl_push = pl_plan.iter().filter(|i| i.dir == PlaylistSyncDir::Push).count();
            let pl_pull = pl_plan.iter().filter(|i| i.dir == PlaylistSyncDir::Pull).count();
            let pl_conflict = pl_plan
                .iter()
                .filter(|i| i.dir == PlaylistSyncDir::Conflict)
                .count();
            if to_lib == 0
                && to_dev == 0
                && song_conflict == 0
                && pl_push == 0
                && pl_pull == 0
                && pl_conflict == 0
            {
                set_button_busy(&sync_btn, false, "Sync");
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    "Already in sync — no tag or playlist changes to apply.",
                );
                return;
            }
            let dname = if dev.label.is_empty() {
                "The device".to_string()
            } else {
                dev.label.clone()
            };
            let mut pl_bits: Vec<String> = Vec::new();
            if song_conflict > 0 {
                pl_bits.push(format!(
                    "{song_conflict} song conflict{} to resolve",
                    if song_conflict == 1 { "" } else { "s" }
                ));
            }
            if pl_push + pl_pull > 0 {
                pl_bits.push(format!(
                    "{} playlist{} to update",
                    pl_push + pl_pull,
                    if pl_push + pl_pull == 1 { "" } else { "s" }
                ));
            }
            if pl_conflict > 0 {
                pl_bits.push(format!(
                    "{pl_conflict} playlist conflict{} to resolve",
                    if pl_conflict == 1 { "" } else { "s" }
                ));
            }
            let pl_line = if pl_bits.is_empty() {
                String::new()
            } else {
                format!(" {}.", pl_bits.join(", "))
            };
            let detail = format!(
                "{dname} has {to_lib} updated song{}, this computer has {to_dev} updated song{}.{pl_line} \
                 Sync all changes?",
                if to_lib == 1 { "" } else { "s" },
                if to_dev == 1 { "" } else { "s" },
            );
            // Planning done — restore the button; the modal dialog now drives
            // the rest of the flow.
            set_button_busy(&sync_btn, false, "Sync");
            let dialog = gtk4::AlertDialog::builder()
                .message("Sync device")
                .detail(detail)
                .buttons(vec!["Cancel".to_string(), "Sync".to_string()])
                .cancel_button(0)
                .default_button(1)
                .modal(true)
                .build();
            let state2 = state_sync.clone();
            let dev2 = dev.clone();
            let plan2 = plan;
            let pl_plan2 = pl_plan;
            let win_wk2 = win_wk.clone();
            let reload2 = reload_sync.clone();
            dialog.choose(
                win_wk.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    if res != Ok(1) {
                        return;
                    }
                    let (applied, failed) = apply_device_sync(&state2, &dev2, &plan2);
                    // Auto-apply the unambiguous playlist directions; collect the
                    // both-changed conflicts to prompt for afterwards.
                    let mut pl_updated = 0usize;
                    let mut pl_copied = 0usize;
                    let mut conflicts: Vec<PlaylistSyncItem> = Vec::new();
                    for item in &pl_plan2 {
                        match item.dir {
                            PlaylistSyncDir::Push => {
                                let (c, ok) = apply_playlist_push(&state2, &dev2, item);
                                pl_copied += c;
                                if ok {
                                    pl_updated += 1;
                                }
                            }
                            PlaylistSyncDir::Pull => {
                                if apply_playlist_pull(&state2, item) {
                                    pl_updated += 1;
                                }
                            }
                            PlaylistSyncDir::Conflict => conflicts.push(item.clone()),
                            PlaylistSyncDir::None => {}
                        }
                    }
                    reload2(dev2.clone());

                    let summary = {
                        let tail = if failed > 0 {
                            format!(", {failed} failed")
                        } else {
                            String::new()
                        };
                        let pl_tail = if pl_updated > 0 {
                            format!(
                                "; updated {pl_updated} playlist{} ({pl_copied} new file{} copied)",
                                if pl_updated == 1 { "" } else { "s" },
                                if pl_copied == 1 { "" } else { "s" },
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "Synced {applied} song{}{pl_tail}{tail}.",
                            if applied == 1 { "" } else { "s" }
                        )
                    };

                    // Per-file tag conflicts (both sides changed a song's tags).
                    let tag_conflicts = build_tag_conflicts(&dev2, &plan2);

                    // Final step: refresh + show the summary.
                    let final_done: Rc<dyn Fn()> = {
                        let reload_done = reload2.clone();
                        let dev_done = dev2.clone();
                        let win_done = win_wk2.clone();
                        Rc::new(move || {
                            reload_done(dev_done.clone());
                            show_alert_parented(win_done.upgrade().as_ref(), &summary);
                        })
                    };
                    // After tag conflicts, resolve playlist conflicts, then finish.
                    let after_tags: Rc<dyn Fn()> = if conflicts.is_empty() {
                        final_done
                    } else {
                        let state_pl = state2.clone();
                        let dev_pl = dev2.clone();
                        let win_pl = win_wk2.clone();
                        Rc::new(move || {
                            prompt_playlist_conflicts(
                                state_pl.clone(),
                                dev_pl.clone(),
                                conflicts.clone(),
                                win_pl.clone(),
                                final_done.clone(),
                            );
                        })
                    };
                    if tag_conflicts.is_empty() {
                        (after_tags)();
                    } else {
                        prompt_tag_conflicts(
                            state2.clone(),
                            dev2.clone(),
                            tag_conflicts,
                            win_wk2.clone(),
                            after_tags,
                        );
                    }
                },
            );
            });
        })
    };
    *sync_run_holder.borrow_mut() = Some(sync_run.clone());
    {
        let devices_sync = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        let sync_run = sync_run.clone();
        dev_sync.connect_clicked(move |btn| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            let dev = devices_sync
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned();
            let Some(dev) = dev else { return };
            sync_run(dev, btn.clone());
        });
    }

    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    let init_sidebar_width = state.borrow().config.window.ml_sidebar_width;
    paned.set_start_child(Some(&sidebar_scroll));
    paned.set_end_child(Some(&stack));
    paned.set_position(init_sidebar_width);
    win.set_child(Some(&paned));

    win.connect_close_request({
        let state = state.clone();
        let playlists_expanded = playlists_expanded.clone();
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
                s.rebuild_ml_callback = None;
            }
            let _ = state.borrow().config.save();
            state.borrow_mut().ml_window = None;
            // Drop the editor-refresh hooks so we don't pin closed-window
            // Rcs in thread-local storage across an ML reopen.
            EDITOR_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
            EDITOR_CURRENT_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
            PLAYLIST_NAV_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
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
fn analyze_job(
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

