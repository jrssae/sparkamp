//! The Media Library's "Disc Drives" page — optical media.
//!
//! Child module of [`super`] (window.rs), extracted from
//! `open_media_library_window` by plan step 5. It owns the drive overview
//! cards, the drive detail view (audio-track list or data-disc file browser),
//! the 2 s drive poll that keeps both and the sidebar's sub-rows live, and the
//! wiring for identify / tag override / rip / submit / eject / burn.
//!
//! Disc *logic* — TOC probing, gnudb, the rip and burn workers, the burn panel
//! — lives in the sibling [`super::disc`] module and in `crate::disc`. This
//! file is the page: the widgets, and the closures that drive them.
//!
//! ## Why it is one function
//!
//! The page is a single `build()` for the same reason the window was: its
//! closures capture each other. The widgets are declared first and the wiring
//! last, and a closure can only capture a local declared above it. Until step
//! 5 the two halves sat ~4,600 lines apart in `media_library.rs` with the
//! whole Playlists page between them; the widgets moved down to meet the
//! wiring so both could travel here together.
//!
//! Three cells break cycles the declaration order cannot: `populate_holder`
//! (the async CD-TEXT read re-renders a drive that was shown before the read
//! finished), `refresh_discs_holder` (the burn panel re-polls after a burn),
//! and `ctx.host.burn_refresh_holder` (a Send-to ▸ Disc Drive add from the
//! Files page updates an open burn panel). All three are the holder pattern
//! from docs/gtk-breakup-plan.md §3.1 — **a holder left `None` is not an
//! error, it is a silent no-op**, which is what smoke-test group X checks.

use gtk4::prelude::*;
use gtk4::{
    gdk, gio, glib, Align, Box as GtkBox, Button, DropTarget, GestureClick, Label, ListBoxRow,
    Orientation, PolicyType, ScrolledWindow,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// Sibling modules this page drives. `sidebar` is imported as a module (for its
// `SUB_ROW_INSET`) as well as by type — the `let sidebar` binding below shadows
// only the value namespace, so `sidebar::…` still resolves.
use super::sidebar::{self, Sidebar};
use super::{disc, disc_data, disc_gnudb};
// Disc helpers that live with the disc logic rather than with the page.
use super::disc::{disc_overview_detail_line, selected_disc_discid};
// Everything else is private to the parent module, which a child may still
// use: the shared view/search row and sidebar row lookup, the Send-to ▸ Disc
// Drive queue, and the player state the transport checks read.
use super::{
    context_popover, find_row_by_name, gtk_safe, make_view_search_row, queue_paths_to_drive,
    MlCtx, PlayerState,
};

/// Build the Disc Drives page and attach it to `ctx.stack` under the name
/// `"discs"`.
///
/// Takes `sb` as well as `ctx` because three of the cells this page drives —
/// its sidebar sub-rows, the Disc Drives chevron state and the header
/// spinner — are built by `sidebar.rs` and handed straight through. They are
/// touched by this page alone, so by the plan's §3.2 test they do not belong
/// on [`MlCtx`].
pub(super) fn build(ctx: &MlCtx, sb: &Sidebar) {
    // Local names for what this page takes from its context, so the body below
    // reads exactly as it did inside `open_media_library_window`. Same device
    // steps 1–4 used: cloning an `Rc` is an integer increment, and rewriting
    // several hundred capture sites would bury a move in an unreviewable diff.
    // `set_track`, `current_devices` and `copy_files_holder` are absent
    // deliberately: the only part of this page that needed them was the
    // data-disc browser's Send-to menu, which took them with it.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let current_drives = ctx.host.current_drives.clone();
    let burn_queues = ctx.host.burn_queues.clone();
    let burn_refresh_holder = ctx.host.burn_refresh_holder.clone();
    let win = ctx.win.clone();
    let stack = ctx.stack.clone();
    let sidebar = sb.list.clone();
    let discs_expanded = sb.discs_expanded.clone();
    let disc_sub_rows = sb.disc_sub_rows.clone();
    let disc_detect_spinner = sb.disc_detect_spinner.clone();

    // ── Page-private state ───────────────────────────────────────────────
    // Moved here from `open_media_library_window`, where plan step 5a had
    // already parked it out of the sidebar block. Nothing outside this page
    // touches any of it.
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
    // Source pill (gnudb / edited / CD-TEXT) for the tags shown above —
    // hidden until populate_disc_detail has a source to badge.
    let disc_source_pill = Label::builder().halign(Align::Start).xalign(0.0).build();
    disc_source_pill.add_css_class("disc-source-pill");
    disc_source_pill.set_visible(false);
    disc_header_text.append(&disc_source_pill);
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
    // F12.1: restore this view's last search query if the feature is on.
    if state.borrow().config.media_library.remember_search {
        let last = state.borrow().config.media_library.last_search.get("discs").cloned();
        if let Some(last) = last {
            disc_search_entry.set_text(&last);
        }
    }
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
        let state_rc = state.clone();
        disc_search_entry.connect_changed(move |e| {
            let raw_text = e.text().to_string();
            *q.borrow_mut() = raw_text.to_lowercase();
            list.invalidate_filter();
            // F12.1: remember this view's query for next open.
            let mut s = state_rc.borrow_mut();
            if s.config.media_library.remember_search {
                s.config.media_library.last_search.insert("discs".to_string(), raw_text);
            }
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
    // Extracted to `window/disc_data.rs` (plan step 5, second cut). It appends
    // its ColumnView and its status bar to `disc_detail` here, so the call
    // stays where the widgets belong in the detail view's order, and hands
    // back the four handles the rest of this page drives it through.
    let data_browser = disc_data::build(
        ctx,
        &disc_detail,
        &disc_status_lbl,
        &disc_files_busy,
        &selected_disc_id,
    );
    let disc_files_store = data_browser.store.clone();
    let disc_files_scroll = data_browser.scroll.clone();
    let disc_status_bar = data_browser.status_bar.clone();
    let load_disc_files = data_browser.load.clone();
    let add_disc_files_to_library = data_browser.add_to_library.clone();

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
    stack.add_named(&disc_page, Some("discs"));

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
        let disc_cdtext = disc_cdtext.clone();
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
            // identified/edited); used for the non-sampler title case. Falls
            // back to CD-TEXT on a gnudb miss (whole-entry precedence), so a
            // CD-TEXT-only disc's added tracks carry its artist/album too —
            // matching the TUI add path and the rip path.
            let (disc_artist, disc_album) =
                selected_disc_discid(&selected_disc_id, &current_drives)
                    .and_then(|(_, id)| {
                        let entry = disc_tags
                            .borrow()
                            .get(&id)
                            .cloned()
                            .or_else(|| disc_cdtext.borrow().get(&id).cloned());
                        entry.map(|t| (t.artist.clone(), t.album.clone()))
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
        let source_pill = disc_source_pill.clone();
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
        // Where a failed CD-TEXT read reports itself. Without this the read is
        // silent on failure, and a missing `cdrskin` is indistinguishable from
        // a disc that carries no CD-TEXT (2026-08-09).
        let cdtext_status = disc_status_lbl.clone();
        let current_drives_ct = current_drives.clone();
        let search_row = disc_search_row.clone();
        let search_entry = disc_search_entry.clone();
        let burn_ui = burn_ui.clone();
        // Task 9 — data-disc file browser.
        let files_scroll = disc_files_scroll.clone();
        let status_bar = disc_status_bar.clone();
        let files_store = disc_files_store.clone();
        let add_all_btn = disc_add_all_btn.clone();
        let load_files = load_disc_files.clone();
        // Which drive the detail last showed — a switch clears the search
        // (the 10 s poll repopulates the SAME drive and must not).
        let last_drive: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let state_disc = state.clone();
        Rc::new(move |drive: &crate::disc::OpticalDrive| {
            if last_drive.borrow().as_deref() != Some(drive.id.as_str()) {
                *last_drive.borrow_mut() = Some(drive.id.clone());
                // F12.1: restore the "discs" view's saved query instead of
                // clearing when remember_search is on.
                if state_disc.borrow().config.media_library.remember_search {
                    let last = state_disc
                        .borrow()
                        .config
                        .media_library
                        .last_search
                        .get("discs")
                        .cloned();
                    search_entry.set_text(last.as_deref().unwrap_or(""));
                } else {
                    search_entry.set_text("");
                }
            }
            // Data-disc file browser: hidden/cleared unconditionally up front.
            // The non-audio branch below re-shows/refills it for a data disc;
            // without this, a data->audio swap on the same drive (e.g. via
            // the fingerprint auto-refresh re-populating this same drive)
            // left the stale file browser visible under the new track list,
            // its rows still pointing at the now-unmounted data disc.
            files_scroll.set_visible(false);
            status_bar.set_visible(false);
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
                    let status_ct = cdtext_status.clone();
                    glib::spawn_future_local(async move {
                        let id_for_read = id2.clone();
                        let drive_id_for_read = drive_id.clone();
                        let result = gio::spawn_blocking(move || {
                            crate::disc::detect::begin_exclusive_read();
                            let r = crate::disc::cdtext::read_cdtext(&drive_id_for_read);
                            crate::disc::detect::end_exclusive_read();
                            r.map(|cd| cd.to_xmcd(&id_for_read))
                        })
                        .await;
                        match result {
                            Ok(Ok(x)) => {
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
                            // A disc with no CD-TEXT says nothing; a missing or
                            // broken reader tool says so, because otherwise the
                            // track list just reads "Track 1…N" forever with no
                            // hint that anything went wrong.
                            Ok(Err(miss)) => {
                                if let Some(msg) = miss.user_message() {
                                    status_ct.set_text(&msg);
                                }
                            }
                            // The blocking task itself failed (pool shut down).
                            Err(_) => {}
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
            // Source pill: which cache produced the tags shown above
            // (whole-entry classification — same three caches the header
            // block just read; each `.borrow()` is released at its own
            // statement, so nothing is held across the `resolve()` call).
            match discid.as_ref().and_then(|id| {
                crate::disc::source::DiscMetaSource::resolve(
                    disc_official.borrow().contains_key(id),
                    disc_tags.borrow().get(id).is_some(),
                    disc_cdtext.borrow().get(id).is_some(),
                )
                .badge()
            }) {
                Some(text) => {
                    source_pill.set_text(text);
                    source_pill.set_visible(true);
                }
                None => source_pill.set_visible(false),
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
                source_pill.set_visible(false);
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
                status_bar.set_visible(is_data_disc);
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
                            bx.set_margin_start(sidebar::SUB_ROW_INSET);
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
    // Which entry indices the "Rip Track(s)" row menu wants pre-checked in the
    // rip dialog; read (and cleared) by disc::connect_rip_ui on open.
    let rip_preselect: Rc<RefCell<Option<Vec<usize>>>> = Rc::new(RefCell::new(None));
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

    // Right-click an audio-CD track → Enqueue to Playlist · Replace Current
    // Playlist · ─ · Rip Track(s). Rip opens the rip dialog with only the
    // selected rows pre-checked (rip_preselect + the toolbar Rip… button).
    {
        let group = gio::SimpleActionGroup::new();

        let a_enqueue = gio::SimpleAction::new("enqueue", None);
        {
            let picked = picked_disc_entries.clone();
            let add = add_disc_entries.clone();
            a_enqueue.connect_activate(move |_, _| add(&picked(), DiscAdd::Enqueue));
        }
        group.add_action(&a_enqueue);

        let a_replace = gio::SimpleAction::new("replace", None);
        {
            let picked = picked_disc_entries.clone();
            let add = add_disc_entries.clone();
            a_replace.connect_activate(move |_, _| add(&picked(), DiscAdd::PlayNow));
        }
        group.add_action(&a_replace);

        let a_rip = gio::SimpleAction::new("rip", None);
        {
            let track_list = disc_track_list.clone();
            let rip_preselect = rip_preselect.clone();
            let rip_btn = disc_rip.clone();
            a_rip.connect_activate(move |_, _| {
                let idxs: Vec<usize> = track_list
                    .selected_rows()
                    .iter()
                    .map(|r| r.index() as usize)
                    .collect();
                // Empty (nothing selected) → let the dialog default to the
                // whole disc rather than pre-check nothing.
                *rip_preselect.borrow_mut() = if idxs.is_empty() { None } else { Some(idxs) };
                rip_btn.emit_clicked();
            });
        }
        group.add_action(&a_rip);

        // Also on the ScrolledWindow, because that is what the popover below
        // parents itself to and where the action lookup therefore starts.
        disc_track_list.insert_action_group("disc-audio", Some(&group));
        disc_tracks_scroll.insert_action_group("disc-audio", Some(&group));

        let gesture = gtk4::GestureClick::new();
        gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
        let list_g = disc_track_list.clone();
        let scroll_g = disc_tracks_scroll.clone();
        gesture.connect_pressed(move |g, _, x, y| {
            if let Some(row) = list_g.row_at_y(y as i32) {
                if !row.is_selected() {
                    list_g.select_row(Some(&row));
                }
            }
            let menu = gio::Menu::new();
            menu.append_item(&gio::MenuItem::new(
                Some("➕ Enqueue to Playlist"),
                Some("disc-audio.enqueue"),
            ));
            menu.append_item(&gio::MenuItem::new(
                Some("♻ Replace Current Playlist"),
                Some("disc-audio.replace"),
            ));
            menu.append_item(&gio::MenuItem::new(
                Some("💿 Rip Track(s)"),
                Some("disc-audio.rip"),
            ));
            let popover =
                context_popover(&menu);
            // Parent on the ScrolledWindow that holds the action group, and do
            // NOT unparent on close — the same recipe the disc-files menu
            // above documents. GTK4 closes a PopoverMenu *before* dispatching
            // the chosen action, so unparenting in `closed` severs the
            // action-group link a moment too early and the action silently
            // never runs: the menu appeared, the click did nothing.
            //
            // Parenting on the scroll rather than the ListBox is what makes
            // dropping the unparent safe. The old comment here unparented to
            // keep a stale popover out of track_list's children, because
            // populate clears them with `while first_child { remove }` and
            // would log "Tried to remove non-child". A popover parented on the
            // scroll is never in that child list to begin with.
            popover.set_parent(&scroll_g);
            let rect = gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
            g.set_state(gtk4::EventSequenceState::Claimed);
        });
        disc_track_list.add_controller(gesture);
    }

    // ── gnudb identify + tag override (Phase 2) ─────────────────────────────
    // Wired in `window/disc_gnudb.rs` (plan step 5, third cut): the Identify
    // lookup + match chooser, and the manual per-track tag editor. Both write
    // through `commit_disc_tags`, and neither hands anything back.
    disc_gnudb::connect(
        ctx,
        &disc_identify,
        &disc_edit_tags,
        disc_gnudb::TagUi {
            selected_disc_id: &selected_disc_id,
            disc_tags: &disc_tags,
            disc_cdtext: &disc_cdtext,
            current_disc_entries: &current_disc_entries,
            commit_disc_tags: &commit_disc_tags,
            status_lbl: &disc_status_lbl,
        },
    );

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
        disc_cdtext.clone(),
        selected_disc_id.clone(),
        current_drives.clone(),
        rip_preselect.clone(),
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
}
