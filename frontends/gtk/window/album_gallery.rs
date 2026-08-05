// Phase 11 A4: album gallery — a recycled-cell `GridView` of album cover
// thumbnails, with a zoom control and a sort dropdown.
//
// This module is a plain `include!`d file (see the note at the top of
// `mod.rs`), so it shares the flat window module with `media_library.rs`,
// `util.rs`, etc. and can call their private helpers directly (`gtk_safe`,
// `load_logo_pixbuf`, `ensure_media_lib_open`) with no `crate::` prefix.
//
// Scope: build the grid + zoom + sort + lazy thumbnails + no-art
// placeholder only. Sidebar/stack wiring, the album→Files drill-down, and
// play/enqueue actions are a follow-up task — this file exposes a single
// builder fn for that task to call.

/// Build the album gallery page: a header row (sort dropdown + zoom
/// controls) above a scrolled, recycled-cell `GridView` of album covers.
///
/// Returns `(page_widget, rebuild)`. `rebuild` reloads
/// `albums(sort, artist_as_album)` from `state.borrow().media_lib` and
/// repopulates the list store — call it whenever the gallery becomes
/// visible again (e.g. after a scan). `on_album_activate` fires with
/// `(album, album_artist)` when a cell is double-clicked or activated via
/// Enter.
///
/// Guards against `media_lib == None` (F12.3 `skip_db_load`): the grid is
/// simply empty until the DB is opened, never a panic.
fn build_album_gallery(
    state: &Rc<RefCell<AppState>>,
    on_album_activate: Rc<dyn Fn(String, String)>,
    // Right-click "Play Album" / "Enqueue Album" — fire with (album,
    // album_artist), the same identity `on_album_activate` uses.
    on_album_play: Rc<dyn Fn(String, String)>,
    on_album_enqueue: Rc<dyn Fn(String, String)>,
) -> (gtk4::Widget, Rc<dyn Fn()>) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();

    // Current thumb/cell edge length in px. Shared (not just read once at
    // construction) because the factory's `bind` closure re-reads it on
    // every rebind, so a zoom change takes effect on the next `rebuild()`
    // without tearing down and recreating the factory itself.
    let px: Rc<Cell<i32>> = Rc::new(Cell::new(
        state.borrow().config.window.gallery_thumb_px as i32,
    ));

    // Source artwork paths with a lazy thumbnail generation already in
    // flight, so a rebind during a scroll (GridView recycles cells) doesn't
    // spawn a second decode of the same file. Mirrors the ML artwork
    // column's `thumb_inflight` in media_library.rs.
    let inflight: Rc<RefCell<std::collections::HashSet<PathBuf>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));

    // ── Cell factory ────────────────────────────────────────────────────
    let factory = SignalListItemFactory::new();
    {
        let px_setup = px.clone();
        factory.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return; // recycled cell already has its widgets
            }

            let cell = GtkBox::new(Orientation::Vertical, 4);
            cell.add_css_class("album-cell");
            cell.set_margin_start(4);
            cell.set_margin_end(4);
            cell.set_margin_top(4);
            cell.set_margin_bottom(4);
            cell.set_width_request(px_setup.get() + 32);

            // The cover sits in an Overlay so the track-count pill can be
            // pinned to its bottom-right corner. `bind`/`unbind` below walk
            // this structure by sibling/child, so the order here is load
            // bearing: overlay(child = Image, overlay = count Label), then
            // title, then artist.
            let art = gtk4::Overlay::new();
            art.set_halign(Align::Center);
            let img = Image::builder()
                .pixel_size(px_setup.get())
                .halign(Align::Center)
                .build();
            art.set_child(Some(&img));

            let count = Label::builder()
                .css_classes(["album-cell-count"])
                .halign(Align::End)
                .valign(Align::End)
                .build();
            art.add_overlay(&count);
            cell.append(&art);

            let title = Label::builder()
                .css_classes(["album-cell-title"])
                .halign(Align::Center)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .max_width_chars(18)
                .build();
            cell.append(&title);

            let artist = Label::builder()
                .css_classes(["album-cell-artist"])
                .halign(Align::Center)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .max_width_chars(18)
                .build();
            cell.append(&artist);

            // Right-click a tile → Play Album / Enqueue Album. `li.item()`
            // returns the cell's CURRENTLY bound album at click time, so no
            // per-cell stash is needed even though GridView recycles cells.
            let gesture = gtk4::GestureClick::new();
            gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
            let li_g = li.clone();
            let play_cb = on_album_play.clone();
            let enq_cb = on_album_enqueue.clone();
            let cell_wk = cell.downgrade();
            gesture.connect_pressed(move |g, _, x, y| {
                let Some(boxed) = li_g
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };
                let (album, album_artist) = {
                    let a = boxed.borrow::<crate::media_library::AlbumGroup>();
                    (a.album.clone(), a.album_artist.clone())
                };
                let Some(cell) = cell_wk.upgrade() else { return };
                // Parent the menu on the enclosing GridView, not the
                // `.album-cell` box: parenting on the cell gave the popover the
                // cell's style context (different look/spacing from every other
                // menu) and left it as a child of a recycled cell. The GridView
                // is the neutral, stable ancestor the other list menus use.
                let Some(grid) = cell.ancestor(gtk4::GridView::static_type()) else { return };

                let group = gio::SimpleActionGroup::new();
                let a_play = gio::SimpleAction::new("play", None);
                {
                    let play_cb = play_cb.clone();
                    let al = album.clone();
                    let aa = album_artist.clone();
                    a_play.connect_activate(move |_, _| play_cb(al.clone(), aa.clone()));
                }
                group.add_action(&a_play);
                let a_enq = gio::SimpleAction::new("enqueue", None);
                {
                    let enq_cb = enq_cb.clone();
                    a_enq.connect_activate(move |_, _| enq_cb(album.clone(), album_artist.clone()));
                }
                group.add_action(&a_enq);
                // Group on the GridView so the popover's action walk (from its
                // parent up) reaches it — the popover is parented on the grid.
                grid.insert_action_group("album", Some(&group));

                let menu = gio::Menu::new();
                menu.append_item(&gio::MenuItem::new(
                    Some("▶ Play Album"),
                    Some("album.play"),
                ));
                menu.append_item(&gio::MenuItem::new(
                    Some("➕ Enqueue Album"),
                    Some("album.enqueue"),
                ));
                let popover =
                    context_popover(&menu);
                // Translate the click point into the GridView's coordinate
                // space, and unparent on close (no nested submenu → safe).
                let (px, py) = cell
                    .compute_point(&grid, &gtk4::graphene::Point::new(x as f32, y as f32))
                    .map(|p| (p.x() as i32, p.y() as i32))
                    .unwrap_or((x as i32, y as i32));
                popover.set_parent(&grid);
                popover.connect_closed(|p| p.unparent());
                let rect = gtk4::gdk::Rectangle::new(px, py, 1, 1);
                popover.set_pointing_to(Some(&rect));
                popover.popup();
                g.set_state(gtk4::EventSequenceState::Claimed);
            });
            cell.add_controller(gesture);

            li.set_child(Some(&cell));
        });
    }
    {
        let px_bind = px.clone();
        let inflight_bind = inflight.clone();
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(boxed) = li
                .item()
                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            // Copy the fields we need out, then drop the Ref before doing
            // any widget/GTK work (never hold a RefCell-style borrow across
            // a UI call).
            let (title_text, artist_text, artwork_path, is_no_album, year, track_count) = {
                let album = boxed.borrow::<crate::media_library::AlbumGroup>();
                (
                    album.album.clone(),
                    album.album_artist.clone(),
                    album.artwork_path.clone(),
                    album.is_no_album,
                    album.year,
                    album.track_count,
                )
            };

            let Some(cell) = li.child().and_then(|c| c.downcast::<GtkBox>().ok()) else {
                return;
            };
            let Some(art) = cell
                .first_child()
                .and_then(|c| c.downcast::<gtk4::Overlay>().ok())
            else {
                return;
            };
            let Some(img) = art.child().and_then(|c| c.downcast::<Image>().ok()) else {
                return;
            };
            if let Some(count_lbl) = art.last_child().and_then(|c| c.downcast::<Label>().ok()) {
                count_lbl.set_text(&track_count.to_string());
            }
            let Some(title_lbl) = art
                .next_sibling()
                .and_then(|c| c.downcast::<Label>().ok())
            else {
                return;
            };
            let Some(artist_lbl) = title_lbl
                .next_sibling()
                .and_then(|c| c.downcast::<Label>().ok())
            else {
                return;
            };

            let px_now = px_bind.get();
            img.set_pixel_size(px_now);
            cell.set_width_request(px_now + 32);

            let title_display = if is_no_album {
                "(No Album)".to_string()
            } else {
                title_text
            };
            title_lbl.set_text(&gtk_safe(&title_display));
            title_lbl.set_tooltip_text(Some(&gtk_safe(&title_display)));

            let artist_display = match year {
                Some(y) if y > 0 => format!("{artist_text} · {y}"),
                _ => artist_text,
            };
            artist_lbl.set_text(&gtk_safe(&artist_display));
            artist_lbl.set_tooltip_text(Some(&gtk_safe(&artist_display)));

            // Artwork: paint the cached thumb if it's already on disk;
            // otherwise show the placeholder and generate it off the main
            // thread (exact idiom as the ML artwork column in
            // media_library.rs's `~ML_ARTWORK_THUMB_PX` block).
            let Some(art_path) = artwork_path else {
                set_gallery_placeholder(&img, px_now);
                return;
            };
            let src = PathBuf::from(art_path.as_str());
            let Some(thumb) = crate::now_playing::thumb_path_for(&src, px_now as u32) else {
                set_gallery_placeholder(&img, px_now);
                return;
            };
            if thumb.exists() {
                img.set_opacity(1.0);
                img.set_from_file(Some(&thumb));
                return;
            }

            set_gallery_placeholder(&img, px_now);
            if inflight_bind.borrow_mut().insert(src.clone()) {
                let inflight2 = inflight_bind.clone();
                let img_wk = img.downgrade();
                let li_wk = li.downgrade();
                let src2 = src.clone();
                let thumb2 = thumb.clone();
                let want_px = px_now as u32;
                glib::spawn_future_local(async move {
                    let src_blk = src2.clone();
                    let thumb_blk = thumb2.clone();
                    let ok = gio::spawn_blocking(move || -> Result<(), ()> {
                        if let Some(parent) = thumb_blk.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(
                            &src_blk,
                            want_px as i32,
                            want_px as i32,
                            true,
                        )
                        .map_err(|_| ())?;
                        pixbuf.savev(&thumb_blk, "png", &[]).map_err(|_| ())
                    })
                    .await;

                    // Generation is done (success or not) — the source is no
                    // longer in flight either way.
                    inflight2.borrow_mut().remove(&src2);

                    if !matches!(ok, Ok(Ok(()))) {
                        return; // decode/encode failed — leave the placeholder
                    }

                    // GridView recycles cells: by the time the decode
                    // finished this cell may have scrolled on to a
                    // different album. Only paint if it still shows the
                    // same artwork source.
                    let (Some(li), Some(img)) = (li_wk.upgrade(), img_wk.upgrade()) else {
                        return;
                    };
                    let still_same = li
                        .item()
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|b| {
                            let cur = b.borrow::<crate::media_library::AlbumGroup>();
                            cur.artwork_path.as_deref() == src2.to_str()
                        })
                        .unwrap_or(false);
                    if still_same {
                        img.set_opacity(1.0);
                        img.set_from_file(Some(&thumb2));
                    }
                });
            }
        });
    }
    factory.connect_unbind(|_, obj| {
        let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
        if let Some(img) = li
            .child()
            .and_then(|c| c.downcast::<GtkBox>().ok())
            .and_then(|cell| cell.first_child())
            .and_then(|c| c.downcast::<gtk4::Overlay>().ok())
            .and_then(|art| art.child())
            .and_then(|c| c.downcast::<Image>().ok())
        {
            img.clear(); // never let a recycled cell show stale art
            img.set_opacity(1.0);
        }
    });

    // ── Grid + selection ────────────────────────────────────────────────
    let selection = NoSelection::new(Some(store.clone()));
    let grid_view = GridView::new(Some(selection), Some(factory));
    grid_view.set_vexpand(true);
    grid_view.set_hexpand(true);
    // A single click opens the album (not the GTK default double-click) so the
    // gallery behaves like a click-to-browse cover grid.
    grid_view.set_single_click_activate(true);
    {
        let store_act = store.clone();
        let on_activate = on_album_activate.clone();
        grid_view.connect_activate(move |_view, position| {
            let Some(boxed) = store_act
                .item(position)
                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let (album, album_artist) = {
                let a = boxed.borrow::<crate::media_library::AlbumGroup>();
                (a.album.clone(), a.album_artist.clone())
            };
            on_activate(album, album_artist);
        });
    }

    let scrolled = ScrolledWindow::new();
    scrolled.set_hexpand(true);
    scrolled.set_vexpand(true);
    scrolled.set_policy(PolicyType::Never, PolicyType::Automatic);
    scrolled.set_child(Some(&grid_view));

    // ── Rebuild closure ─────────────────────────────────────────────────
    // Reads the sort dropdown's current selection (set below) rather than
    // taking a sort parameter, so callers only ever need `rebuild()`.
    let sort_dd = DropDown::from_strings(&["Artist", "Album", "Year"]);
    sort_dd.set_selected(gallery_sort_idx(&state.borrow().config.window.gallery_sort));

    let rebuild: Rc<dyn Fn()> = {
        let store = store.clone();
        let state = state.clone();
        let sort_dd = sort_dd.clone();
        Rc::new(move || {
            ensure_media_lib_open(&state);
            let sort = gallery_sort_from_idx(sort_dd.selected());
            let albums: Vec<crate::media_library::AlbumGroup> = {
                let s = state.borrow();
                let artist_as_album = s.config.media_library.artist_as_album_artist;
                s.media_lib
                    .as_ref()
                    .and_then(|lib| lib.albums(sort, artist_as_album).ok())
                    .unwrap_or_default()
            };
            store.remove_all();
            for album in albums {
                store.append(&glib::BoxedAnyObject::new(album));
            }
        })
    };

    // ── Header row: sort dropdown + zoom controls ──────────────────────
    let header = GtkBox::new(Orientation::Horizontal, 8);
    header.set_margin_start(8);
    header.set_margin_end(8);
    header.set_margin_top(6);
    header.set_margin_bottom(6);

    let sort_label = Label::new(Some("Sort:"));
    header.append(&sort_label);
    header.append(&sort_dd);
    {
        let state_c = state.clone();
        let rebuild_c = rebuild.clone();
        sort_dd.connect_selected_notify(move |d| {
            let key = gallery_sort_key(gallery_sort_from_idx(d.selected()));
            {
                let mut s = state_c.borrow_mut();
                s.config.window.gallery_sort = key.to_string();
                let _ = s.config.save();
            }
            rebuild_c();
        });
    }

    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&spacer);

    // "Please wait" spinner shown while a zoom change regenerates thumbnails
    // at the new size; hidden once the in-flight generations drain.
    let zoom_spinner = gtk4::Spinner::new();
    zoom_spinner.set_visible(false);
    let zoom_out = Button::with_label("−");
    // The label is a fixed word, not the pixel size (users think in "bigger /
    // smaller", not exact px).
    let zoom_label = Label::new(Some("Zoom"));
    let zoom_in = Button::with_label("＋");
    header.append(&zoom_spinner);
    header.append(&zoom_out);
    header.append(&zoom_label);
    header.append(&zoom_in);

    const ZOOM_MIN: i32 = 96;
    const ZOOM_MAX: i32 = 256;
    const ZOOM_STEP: i32 = 32;

    // Shared by both buttons: persist the new size, show the please-wait
    // spinner, rebuild, then poll the `inflight` thumbnail set and hide the
    // spinner once every visible cell's thumbnail has been (re)generated at
    // the new size. `seen_work` avoids hiding during the brief window after
    // `rebuild()` but before the grid has bound its cells (inflight still
    // empty); the tick cap is a safety net so the spinner can never hang.
    let apply_zoom: Rc<dyn Fn(i32)> = {
        let px_c = px.clone();
        let state_c = state.clone();
        let rebuild_c = rebuild.clone();
        let inflight_c = inflight.clone();
        let spinner_c = zoom_spinner.clone();
        let zoom_out_c = zoom_out.clone();
        let zoom_in_c = zoom_in.clone();
        Rc::new(move |new_px: i32| {
            if new_px == px_c.get() {
                return;
            }
            px_c.set(new_px);
            {
                let mut s = state_c.borrow_mut();
                s.config.window.gallery_thumb_px = new_px as u32;
                let _ = s.config.save();
            }
            spinner_c.set_visible(true);
            spinner_c.start();
            zoom_out_c.set_sensitive(false);
            zoom_in_c.set_sensitive(false);
            rebuild_c();

            let inflight_p = inflight_c.clone();
            let spinner_p = spinner_c.clone();
            let zo = zoom_out_c.clone();
            let zi = zoom_in_c.clone();
            let ticks = Cell::new(0u32);
            let seen_work = Cell::new(false);
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                ticks.set(ticks.get() + 1);
                let empty = inflight_p.borrow().is_empty();
                if !empty {
                    seen_work.set(true);
                }
                // Done when the queue has drained after doing work, or it
                // stayed empty for 500ms (every size already cached — nothing
                // to generate), or the 5s cap trips.
                let done = (empty && seen_work.get())
                    || (empty && ticks.get() >= 5)
                    || ticks.get() >= 50;
                if done {
                    spinner_p.stop();
                    spinner_p.set_visible(false);
                    zo.set_sensitive(true);
                    zi.set_sensitive(true);
                    ControlFlow::Break
                } else {
                    ControlFlow::Continue
                }
            });
        })
    };
    {
        let px_c = px.clone();
        let apply = apply_zoom.clone();
        zoom_out.connect_clicked(move |_| {
            apply((px_c.get() - ZOOM_STEP).max(ZOOM_MIN));
        });
    }
    {
        let px_c = px.clone();
        let apply = apply_zoom.clone();
        zoom_in.connect_clicked(move |_| {
            apply((px_c.get() + ZOOM_STEP).min(ZOOM_MAX));
        });
    }

    // ── Assemble ────────────────────────────────────────────────────────
    let page = GtkBox::new(Orientation::Vertical, 0);
    page.set_hexpand(true);
    page.set_vexpand(true);
    page.append(&header);
    page.append(&scrolled);

    // Populate once up front so the grid isn't empty the first time it's
    // shown; the caller can also call `rebuild()` again later (e.g. on
    // stack-page switch or after a scan completes).
    rebuild();

    (page.upcast::<gtk4::Widget>(), rebuild)
}

/// No-art placeholder for a gallery cell: the app logo at 50% opacity,
/// scaled to the current thumb size. Same embedded `LOGO_BYTES` and
/// opacity as the A1/A6 placeholders (`now_playing.rs`/`art_window.rs`),
/// just without the caption text so it fits a small grid tile.
fn set_gallery_placeholder(img: &Image, px: i32) {
    img.set_opacity(0.5);
    match load_logo_pixbuf(px) {
        Some(pb) => img.set_from_pixbuf(Some(&pb)),
        None => img.clear(),
    }
}

/// Dropdown index (0/1/2) for the current `gallery_sort` config string.
/// Unknown values fall back to Artist, matching `gallery_sort_from_idx`'s
/// own default.
fn gallery_sort_idx(sort: &str) -> u32 {
    match sort {
        "album" => 1,
        "year" => 2,
        _ => 0,
    }
}

/// Map a sort-dropdown selection to `AlbumSort`.
fn gallery_sort_from_idx(idx: u32) -> crate::media_library::AlbumSort {
    match idx {
        1 => crate::media_library::AlbumSort::Album,
        2 => crate::media_library::AlbumSort::Year,
        _ => crate::media_library::AlbumSort::Artist,
    }
}

/// The `gallery_sort` config string for an `AlbumSort` value — inverse of
/// `gallery_sort_from_idx`/`gallery_sort_idx`.
fn gallery_sort_key(sort: crate::media_library::AlbumSort) -> &'static str {
    match sort {
        crate::media_library::AlbumSort::Artist => "artist",
        crate::media_library::AlbumSort::Album => "album",
        crate::media_library::AlbumSort::Year => "year",
    }
}
