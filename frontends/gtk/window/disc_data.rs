//! The data-disc file browser inside the Media Library's "Disc Drives" page.
//!
//! Split from [`super::disc_page`] (plan step 5, second cut) so neither half
//! sits far over the plan's ~800-line goal — the same shape step 4 used for
//! `files.rs` and `files_menu.rs`.
//!
//! This is the view shown in place of the audio-track list when the loaded
//! media is present, not blank, and not an audio CD: a `ColumnView` of
//! `crate::disc::mount::DiscFile` rows (#, Title, Length, Size), a status bar,
//! and a right-click menu whose Send-to submenu ends in "Copy to library".
//! It appends both widgets to the drive detail box it is given, so it must be
//! built at the point in the detail view where they belong.
//!
//! The audio side of the page owns *when* this is visible:
//! `populate_disc_detail` shows or hides [`DataBrowser::scroll`] and calls
//! [`DataBrowser::load`] per selected drive.

use gtk4::prelude::*;
use gtk4::{
    gdk, gio, glib, Align, Box as GtkBox, ColumnView, ColumnViewColumn, CustomSorter,
    EventControllerKey, Label, MultiSelection, PolicyType, ScrolledWindow,
    SignalListItemFactory, SortListModel,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{
    art_window, attach_cell_context_menu, build_send_to_menu, context_popover, disc,
    gtk_safe, ml_status_bar_for,
    notify_playlist_changed, notify_playlist_nav_refresh, open_id3_editor_window,
    queue_paths_to_drive, run_playlist_save_dialog, show_playlist_save_error,
    view_or_search_lyrics, LyricsMode, MlCtx, SendToActions,
};

/// What the rest of the Disc Drives page needs back from the browser.
pub(super) struct DataBrowser {
    /// The row model. `populate_disc_detail` clears it when a drive with no
    /// data disc is selected.
    pub store: gio::ListStore,
    /// The scroller holding the `ColumnView`, already appended to the detail
    /// box. Shown for a data disc, hidden for an audio CD or an empty tray.
    pub scroll: ScrolledWindow,
    /// The file-count/duration label under the list, also already appended.
    /// It hides and shows in lockstep with [`Self::scroll`] — the audio-CD
    /// branch of the same detail box has no file list for it to describe.
    pub status_bar: Label,
    /// Mount the drive, walk its filesystem off the UI thread and fill
    /// [`Self::store`]. Guarded by the `busy` flag passed to [`build`], so a
    /// poll tick landing mid-walk is skipped rather than piling on a second
    /// disc read.
    pub load: Rc<dyn Fn(crate::disc::OpticalDrive)>,
    /// Copy the given disc files into the library. Shared with the audio
    /// side's "Copy all to library" button.
    pub add_to_library: Rc<dyn Fn(Vec<crate::disc::mount::DiscFile>)>,
}

/// Build the browser into `detail` and return its handles.
///
/// `status_lbl` is the detail view's shared status label — the browser writes
/// mount/walk progress and copy results to it, and the audio side writes to
/// the same label, which is why it is passed in rather than owned here.
pub(super) fn build(
    ctx: &MlCtx,
    detail: &GtkBox,
    status_lbl: &Label,
    busy: &Rc<Cell<bool>>,
    selected_disc_id: &Rc<RefCell<Option<String>>>,
) -> DataBrowser {
    // Local names for what this view takes from its context and its caller, so
    // the body below reads as it did inside `disc_page::build`.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let set_track = ctx.host.set_track.clone();
    let current_drives = ctx.host.current_drives.clone();
    let current_devices = ctx.host.current_devices.clone();
    let burn_queues = ctx.host.burn_queues.clone();
    let copy_files_holder = ctx.host.copy_files_holder.clone();
    let burn_refresh_holder = ctx.host.burn_refresh_holder.clone();
    let win = ctx.win.clone();
    let disc_detail = detail.clone();
    let disc_status_lbl = status_lbl.clone();
    let disc_files_busy = busy.clone();
    let selected_disc_id = selected_disc_id.clone();

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
    // The row context menu, filled in further down once its action group and
    // menu model exist. The cells below are built before any of that but each
    // one needs to reach it, which is the holder pattern this file already
    // leans on (docs/gtk-breakup-plan.md §3.1). Left None it is a silent
    // no-op, which is exactly the bug being fixed here, so it is filled
    // unconditionally at the end of the gesture block.
    let row_menu_holder: Rc<RefCell<Option<Rc<dyn Fn(f64, f64)>>>> =
        Rc::new(RefCell::new(None));
    disc_files_col_view.add_css_class("ml-col-view");
    disc_files_col_view.set_hexpand(true);
    disc_files_col_view.set_vexpand(true);
    {
        // "#" — row position (mirrors dev_pos_col).
        let sel_ctx = disc_files_selection.clone();
        let anchor_ctx = disc_files_col_view.clone();
        let holder_ctx = row_menu_holder.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
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
            // Right-click has to be handled per cell: ColumnView has no
            // `row_at_y`, so the ScrolledWindow-level gesture this used to
            // rely on could not tell which row it hit.
            attach_cell_context_menu(
                li,
                lbl.upcast_ref(),
                &sel_ctx,
                anchor_ctx.upcast_ref(),
                {
                    let holder = holder_ctx.clone();
                    move |x, y| {
                        let f = holder.borrow().clone();
                        if let Some(f) = f {
                            f(x, y);
                        }
                    }
                },
            );
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
        let sel_ctx = disc_files_selection.clone();
        let anchor_ctx = disc_files_col_view.clone();
        let holder_ctx = row_menu_holder.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
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
            // Right-click has to be handled per cell: ColumnView has no
            // `row_at_y`, so the ScrolledWindow-level gesture this used to
            // rely on could not tell which row it hit.
            attach_cell_context_menu(
                li,
                lbl.upcast_ref(),
                &sel_ctx,
                anchor_ctx.upcast_ref(),
                {
                    let holder = holder_ctx.clone();
                    move |x, y| {
                        let f = holder.borrow().clone();
                        if let Some(f) = f {
                            f(x, y);
                        }
                    }
                },
            );
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
        let sel_ctx = disc_files_selection.clone();
        let anchor_ctx = disc_files_col_view.clone();
        let holder_ctx = row_menu_holder.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
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
            // Right-click has to be handled per cell: ColumnView has no
            // `row_at_y`, so the ScrolledWindow-level gesture this used to
            // rely on could not tell which row it hit.
            attach_cell_context_menu(
                li,
                lbl.upcast_ref(),
                &sel_ctx,
                anchor_ctx.upcast_ref(),
                {
                    let holder = holder_ctx.clone();
                    move |x, y| {
                        let f = holder.borrow().clone();
                        if let Some(f) = f {
                            f(x, y);
                        }
                    }
                },
            );
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
        let sel_ctx = disc_files_selection.clone();
        let anchor_ctx = disc_files_col_view.clone();
        let holder_ctx = row_menu_holder.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
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
            // Right-click has to be handled per cell: ColumnView has no
            // `row_at_y`, so the ScrolledWindow-level gesture this used to
            // rely on could not tell which row it hit.
            attach_cell_context_menu(
                li,
                lbl.upcast_ref(),
                &sel_ctx,
                anchor_ctx.upcast_ref(),
                {
                    let holder = holder_ctx.clone();
                    move |x, y| {
                        let f = holder.borrow().clone();
                        if let Some(f) = f {
                            f(x, y);
                        }
                    }
                },
            );
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

    // ── Disc data-file browser status bar ───────────────────────────────────
    // Rows are `BoxedAnyObject<DiscFile>` (not LibTrack), so this goes through
    // `ml_status_bar_for` with a `DiscFile::duration_secs` extractor. Only
    // meaningful for a data disc's file list, so it hides/shows in lockstep
    // with `disc_files_scroll` (populate_disc_detail, below) rather than
    // living at the literal bottom of `disc_detail` — the audio-CD branch of
    // that same container has no file list for it to describe.
    let (disc_status_bar, _) =
        ml_status_bar_for::<crate::disc::mount::DiscFile>(&disc_files_selection, |f| {
            f.duration_secs.map(|s| s as f64)
        });
    disc_status_bar.set_visible(false);
    disc_detail.append(&disc_status_bar);

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

    // ── Right-click context menu on data-disc files: Copy to library + the
    // standard Send-to submenu ────────────────────────────────────────────
    // Gesture + action group live on the ScrolledWindow, not the ColumnView
    // (same GTK4 hover-popover dodge as the device view's context menu).
    {
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

        // Replace the active playlist with the selected disc files.
        {
            let sel_files = selected_disc_files.clone();
            let state = state.clone();
            let rebuild = rebuild_playlist.clone();
            let action = gio::SimpleAction::new("replace", None);
            action.connect_activate(move |_, _| {
                let files = sel_files();
                if files.is_empty() {
                    return;
                }
                let _ = state.borrow_mut().player.stop();
                state.borrow_mut().playlist.clear();
                for f in &files {
                    if let Ok(track) = crate::model::Track::from_path(&f.path) {
                        state.borrow_mut().playlist.add(track);
                    }
                }
                if state.borrow().config.behavior.autoplay_on_add {
                    state.borrow_mut().play_current();
                }
                rebuild();
            });
            disc_files_action_group.add_action(&action);
        }

        // View Album Art for the single selected disc file.
        {
            let sel_files = selected_disc_files.clone();
            let state = state.clone();
            let action = gio::SimpleAction::new("view-art", None);
            action.connect_activate(move |_, _| {
                let files = sel_files();
                let Some(f) = files.first() else { return };
                art_window::open_track_art(&state, &f.path);
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

        // Copy to library (Send-to submenu; also the bottom bar's Copy all).
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
                    None,
                );
            });
            disc_files_action_group.add_action(&action);
        }

        // View/Search Lyrics (F15) on disc files. Mounted files have real paths,
        // so USLT reads normally; DiscFile carries no separate artist/title, so
        // the search fallback uses the file stem as the title.
        {
            let sel_files = selected_disc_files.clone();
            let state_lyr = state.clone();
            let rebuild_lyr = rebuild_playlist.clone();
            let action = gio::SimpleAction::new("lyrics", None);
            action.connect_activate(move |_, _| {
                let files = sel_files();
                let Some(f) = files.first() else { return };
                let title = f
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                view_or_search_lyrics(&state_lyr, &f.path, "", &title, "", rebuild_lyr.clone(), LyricsMode::Specific);
            });
            disc_files_action_group.add_action(&action);
        }

        // `l` — View/Search Lyrics for the single selected disc file in Specific
        // mode. No-op on a multi-row or empty selection, matching the row menu.
        // DiscFile carries no artist/title, so the search fallback uses the file
        // stem as the title, the same as the row action above.
        {
            let key = EventControllerKey::new();
            let sel_files = selected_disc_files.clone();
            let state_l = state.clone();
            let rebuild_l = rebuild_playlist.clone();
            key.connect_key_pressed(move |_, keyval, _, _| {
                if !matches!(keyval, gdk::Key::l | gdk::Key::L) {
                    return glib::Propagation::Proceed;
                }
                let files = sel_files();
                let [f] = files.as_slice() else {
                    return glib::Propagation::Proceed;
                };
                let title = f
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                view_or_search_lyrics(
                    &state_l, &f.path, "", &title, "", rebuild_l.clone(),
                    LyricsMode::Specific,
                );
                glib::Propagation::Stop
            });
            disc_files_col_view.add_controller(key);
        }

        let sel_menu = selected_disc_files.clone();
        let scroll_menu = disc_files_scroll.clone();
        let state_menu = state.clone();
        let drives_menu = current_drives.clone();
        let devices_menu = current_devices.clone();
        let selected_disc_id_menu = selected_disc_id.clone();
        // Filled here rather than connected to the ScrolledWindow: each cell
        // calls this through `row_menu_holder` after selecting its own row, so
        // `x`/`y` arrive in ColumnView space and the selection is never empty.
        let col_view_menu = disc_files_col_view.clone();
        *row_menu_holder.borrow_mut() = Some(Rc::new(move |x: f64, y: f64| {
            if sel_menu().is_empty() {
                return;
            }
            // Order: Send to · Replace · ─ · ID3 · Album Art · Lyrics. Matches
            // the macOS disc data-files menu.
            //
            // "Copy to library" is the last entry of the Send-to submenu
            // rather than a top-level item: the library is one more place the
            // selection can be sent, and grouping it with the drives and
            // devices is what a user looking for "put this somewhere" reads
            // first. It was previously reachable only from the bottom-bar
            // button, which testing found people did not associate with a
            // row selection at all (2026-08-09).
            //
            // It is appended here, not inside `build_send_to_menu`, because
            // that builder is shared with the Files view and the playlist
            // editor — whose rows are already IN the library, so the item
            // would be meaningless there.
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
            send.append_item(&gio::MenuItem::new(
                Some("📚 Copy to library"),
                Some("disc-files.add-to-library"),
            ));
            let menu = gio::Menu::new();
            menu.append_submenu(Some("↪ Send to"), &send);
            menu.append_item(&gio::MenuItem::new(
                Some("♻ Replace Current Playlist"),
                Some("disc-files.replace"),
            ));
            // Single selection only — these bind one file.
            if sel_menu().len() == 1 {
                menu.append_item(&gio::MenuItem::new(
                    Some("🎵 View/Edit ID3"),
                    Some("disc-files.edit-id3"),
                ));
                menu.append_item(&gio::MenuItem::new(
                    Some("🖼 View Album Art"),
                    Some("disc-files.view-art"),
                ));
                menu.append_item(&gio::MenuItem::new(
                    Some("📝 View/Search Lyrics"),
                    Some("disc-files.lyrics"),
                ));
            }
            let popover =
                context_popover(&menu);
            // Parent on the group-holding widget and DON'T unparent on close:
            // the unparent severs the action-group link as a nested "Send to"
            // item dispatches (the bug fixed in the playlist editor). Match
            // the working files-view recipe.
            popover.set_parent(&scroll_menu);
            // The cell handed us ColumnView coordinates; the popover is
            // parented to the ScrolledWindow, so make the last hop here.
            let (sx, sy) = col_view_menu
                .translate_coordinates(&scroll_menu, x, y)
                .unwrap_or((x, y));
            let rect = gtk4::gdk::Rectangle::new(sx as i32, sy as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
        }));
    }

    DataBrowser {
        store: disc_files_store,
        scroll: disc_files_scroll,
        status_bar: disc_status_bar,
        load: load_disc_files,
        add_to_library: add_disc_files_to_library,
    }
}
