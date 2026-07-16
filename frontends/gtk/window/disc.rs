//! Disc (optical media) UI pieces for the GTK Media Library window.
//!
//! Child module of [`super`] (window.rs): the widgets it drives are built by
//! `open_media_library_window`, which passes them in through [`DiscRipUi`].
//! All disc *logic* lives in `crate::disc` — this file is presentation and
//! thread plumbing only. New disc UI (Phase 4 submit, Phases 5–6 burn)
//! belongs here, not in window.rs.

use gtk4::prelude::*;
use gtk4::{gio, glib, Align, Box as GtkBox, Button, Entry, Label, ListBoxRow, Orientation,
    ScrolledWindow};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{gtk_safe, AppState};
use crate::disc::rip::RipOutcome;

/// Progress messages from the rip worker thread to the GTK poller.
enum DiscRipMsg {
    /// (track index, total, its title, within-track fraction 0.0–1.0).
    Progress(usize, usize, String, f64),
    /// Finished (or cancelled).
    Done(RipOutcome),
}

/// Overview-card detail line for one optical drive: total audio time for an
/// audio CD, writable size for a blank disc, or used-of-total for a data disc.
/// `None` when nothing meaningful is known (e.g. an empty tray, or capacities
/// the Phase-1 probe doesn't fill yet).
pub(super) fn disc_overview_detail_line(d: &crate::disc::OpticalDrive) -> Option<String> {
    if d.media.is_audio_cd {
        let toc = d.toc.as_ref()?;
        let first = toc.tracks.first().map(|t| t.start_frame / 75).unwrap_or(0);
        let total = (toc.leadout_frame / 75).saturating_sub(first);
        return Some(format!("{}:{:02} of audio", total / 60, total % 60));
    }
    if d.media.is_blank && d.media.capacity_bytes > 0 {
        return Some(format!("{:.0} MB writable", d.media.capacity_bytes as f64 / 1e6));
    }
    if d.media.present && !d.media.is_blank && d.media.capacity_bytes > 0 {
        let used = d.media.capacity_bytes.saturating_sub(d.media.free_bytes);
        return Some(format!(
            "{:.1} GB of {:.1} GB used",
            used as f64 / 1e9,
            d.media.capacity_bytes as f64 / 1e9,
        ));
    }
    None
}

/// The audio TOC + freedb disc id of the drive currently shown in the disc
/// detail view, when it holds an audio disc. `None` for no selection / no disc.
pub(super) fn selected_disc_discid(
    selected_disc_id: &Rc<RefCell<Option<String>>>,
    current_drives: &Rc<RefCell<Vec<crate::disc::OpticalDrive>>>,
) -> Option<(crate::disc::DiscToc, String)> {
    let id = selected_disc_id.borrow().clone()?;
    let drives = current_drives.borrow();
    let toc = drives.iter().find(|d| d.id == id)?.toc.clone()?;
    let discid = crate::disc::discid::freedb_discid(&toc);
    Some((toc, discid))
}

/// The watched library folder paths (empty when no library is open).
pub(super) fn watched_folders(state: &Rc<RefCell<AppState>>) -> Vec<String> {
    state
        .borrow()
        .media_lib
        .as_ref()
        .and_then(|l| l.list_folders().ok())
        .map(|folders| folders.into_iter().map(|(_, p)| p).collect())
        .unwrap_or_default()
}

/// Everything the rip flow needs from the Media Library window: shared app
/// state, the one-rip-at-a-time guards, and the progress widgets living on
/// the drive detail view.
#[derive(Clone)]
pub(super) struct DiscRipUi {
    pub state: Rc<RefCell<AppState>>,
    /// Cancel flag of the running rip (shared with its worker thread).
    pub rip_cancel: Rc<RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>>,
    pub rip_active: Rc<Cell<bool>>,
    pub rip_box: GtkBox,
    pub rip_bar: gtk4::ProgressBar,
    pub status: Label,
}

/// Spawn the rip worker + a main-thread progress poller. Runs off the UI
/// thread; imports the results and reports honestly about the watched-folder
/// policy. `tags` is the disc's tag set; `total` the disc's track count.
fn start_rip(
    ui: &DiscRipUi,
    entries: Vec<crate::disc::DiscTrackEntry>,
    dest: String,
    quality: u8,
    tags: crate::disc::xmcd::XmcdEntry,
    total: u8,
) {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    *ui.rip_cancel.borrow_mut() = Some(cancel.clone());
    ui.rip_active.set(true);
    // Keep every poller (incl. the app-level insertion watcher) off the
    // drive for the whole rip — status ioctls disturb the streaming read.
    ui.state.borrow().disc_reading.set(true);
    ui.rip_box.set_visible(true);
    ui.rip_bar.set_fraction(0.0);
    ui.rip_bar.set_text(Some("Starting…"));
    ui.status.set_text("");
    let (tx, rx) = std::sync::mpsc::channel::<DiscRipMsg>();

    std::thread::spawn(move || {
        use crate::disc::rip;
        let outcome = rip::run_job(
            &entries,
            std::path::Path::new(&dest),
            rip::Mp3Quality::from_config(quality),
            &tags,
            total,
            &cancel,
            |i, n, title, frac| {
                let _ = tx.send(DiscRipMsg::Progress(i, n, title.to_string(), frac));
            },
        );
        let _ = tx.send(DiscRipMsg::Done(outcome));
    });

    // Main-thread poller: drain progress, update the bar, import on done.
    let ui = ui.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
        let mut done: Option<RipOutcome> = None;
        loop {
            match rx.try_recv() {
                Ok(DiscRipMsg::Progress(i, n, title, track_frac)) => {
                    // Overall fraction: finished tracks + progress within the
                    // current one, so the bar moves during a single track too.
                    let overall = if n > 0 {
                        (i as f64 + track_frac) / n as f64
                    } else {
                        0.0
                    };
                    ui.rip_bar.set_fraction(overall.clamp(0.0, 1.0));
                    ui.rip_bar.set_text(Some(&gtk_safe(&format!(
                        "Ripping {}/{} · {} ({:.0}%)",
                        i + 1,
                        n,
                        title,
                        track_frac * 100.0
                    ))));
                }
                Ok(DiscRipMsg::Done(outcome)) => {
                    done = Some(outcome);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = Some(RipOutcome::default());
                    break;
                }
            }
        }
        if let Some(outcome) = done {
            ui.rip_bar.set_fraction(1.0);
            // Import so the library sees the new files immediately (only
            // registers files under a watched folder — the shared status
            // line reports honestly).
            let mut imported = 0;
            if !outcome.ripped.is_empty() {
                if let Some(lib) = ui.state.borrow().media_lib.as_ref() {
                    imported = lib.add_files_to_library(&outcome.ripped).unwrap_or(0);
                }
            }
            // An open Files view must show the fresh rips immediately.
            if imported > 0 {
                let rebuild_ml = ui.state.borrow().rebuild_ml_callback.clone();
                if let Some(cb) = rebuild_ml {
                    cb();
                }
            }
            ui.status.set_text(&outcome.status_message(imported));
            ui.rip_box.set_visible(false);
            ui.rip_active.set(false);
            ui.state.borrow().disc_reading.set(false);
            *ui.rip_cancel.borrow_mut() = None;
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// The media-format badge for a drive card: writable kinds by name; pressed
/// discs (no writable kind reported) split CD vs DVD by capacity; `None`
/// for an empty tray (bare drive glyph, no badge). Mirrors the macOS
/// `DiscDriveIcon` badge rules.
pub(super) fn media_badge(d: &crate::disc::OpticalDrive) -> Option<&'static str> {
    use crate::disc::MediaKind;
    if !d.media.present {
        return None;
    }
    Some(match d.media.kind {
        MediaKind::CdR => "CD-R",
        MediaKind::CdRw => "CD-RW",
        MediaKind::DvdR => "DVD-R",
        MediaKind::DvdRw => "DVD-RW",
        MediaKind::DvdRam => "DVD-RAM",
        MediaKind::Unknown => {
            if d.media.capacity_bytes > 1_000_000_000 {
                "DVD"
            } else {
                "CD"
            }
        }
    })
}

/// Overview-card icon: a disc glyph when media is loaded (media-optical),
/// a bare drive when the tray is empty, with the media-format badge overlaid
/// bottom-right.
pub(super) fn disc_card_icon(d: &crate::disc::OpticalDrive) -> gtk4::Overlay {
    let icon = gtk4::Image::from_icon_name(if d.media.present {
        "media-optical"
    } else {
        "drive-optical"
    });
    icon.set_pixel_size(40);
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&icon));
    if let Some(badge) = media_badge(d) {
        // GTK's built-in "osd" style: translucent dark pill, readable over
        // the glyph in both themes (mirrors the mac badge-on-background).
        let pill = GtkBox::new(Orientation::Horizontal, 0);
        pill.add_css_class("osd");
        pill.set_halign(gtk4::Align::End);
        pill.set_valign(gtk4::Align::End);
        let lbl = Label::new(None);
        lbl.set_markup(&format!("<small><b>{badge}</b></small>"));
        lbl.set_margin_start(3);
        lbl.set_margin_end(3);
        lbl.set_margin_top(1);
        lbl.set_margin_bottom(1);
        pill.append(&lbl);
        overlay.add_overlay(&pill);
    }
    overlay
}

// ─────────────────────────── Burn (Phases 5–6) ──────────────────────────────

/// Progress/result of the background burn, drained by a main-thread poller.
enum BurnMsg {
    Progress(crate::disc::burn::BurnProgress),
    Done(Result<String, String>),
}

/// The burn panel on the drive detail view, shown whenever writable
/// non-audio media is loaded (blank, rewritable-with-content, or a data
/// disc). Owns its widgets; `refresh` re-renders for the drive being shown.
pub(super) struct BurnUi {
    /// The whole panel — window.rs toggles visibility per media state.
    pub root: GtkBox,
    /// Centered card shown over the whole disc-detail view (added as the
    /// overlay child of media_library.rs's `disc_detail_overlay`) while the
    /// shown drive has a live burn: phase label + fraction bar + Cancel.
    /// Hidden by default; visibility is driven by `refresh_progress` below
    /// and by the burn poller itself while running.
    pub overlay_card: GtkBox,
    refresh_cb: Rc<dyn Fn(&crate::disc::OpticalDrive)>,
    progress_refresh_cb: Rc<dyn Fn(&str)>,
}

impl BurnUi {
    /// Re-render the queue, meters, and button sensitivity for `drive`.
    pub fn refresh(&self, drive: &crate::disc::OpticalDrive) {
        (self.refresh_cb)(drive);
    }

    /// Show/hide + restore the overlay card for `drive_id` from the shared
    /// progress map — called whenever the disc detail (re)populates for a
    /// drive, so navigating away from a running burn and back re-shows it
    /// (rather than only the panel's own in-place progress row, which lives
    /// only while this exact drive stays selected). A live burn's own 200 ms
    /// poller tick follows right behind to resume the pulse animation.
    pub fn refresh_progress(&self, drive_id: &str) {
        (self.progress_refresh_cb)(drive_id);
    }
}

/// Build + wire the burn panel. `burn_queues` is shared with the ML files
/// view's "Send to ▸ Disc Drive" context action; the panel reads/writes only the
/// queue for the drive currently shown in the detail view — every other
/// drive's queue is untouched. `refresh_discs` re-polls after a finished burn
/// so the detail reflects the disc's new content. `burn_progress_map` is
/// shared with media_library.rs's `populate_disc_detail` (Task 7): this panel
/// writes it as the burn progresses, `populate_disc_detail` reads it via
/// `refresh_progress` on every drive switch.
pub(super) fn build_burn_panel(
    state: Rc<RefCell<AppState>>,
    burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>>,
    refresh_discs_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    // Filled with a closure that re-renders the queue for the drive currently
    // shown; the Send-to actions call it so an external add updates the panel
    // live instead of only after a navigate-away-and-back.
    burn_refresh_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    burn_progress_map: Rc<RefCell<std::collections::HashMap<String, crate::disc::burn::BurnProgress>>>,
    win: &gtk4::Window,
) -> BurnUi {
    let root = GtkBox::new(Orientation::Vertical, 6);
    root.set_visible(false);

    let title = Label::builder()
        .label("Burn")
        .halign(Align::Start)
        .xalign(0.0)
        .build();
    title.add_css_class("ml-section-header");
    root.append(&title);

    // Disc artist/album (CD-TEXT for audio burns). Defaults track the queue
    // until the user types over them (Task 5); hidden when the loaded media
    // can't take a burn at all (see `writable` in refresh_cb below).
    let meta_row = GtkBox::new(Orientation::Horizontal, 6);
    let artist_lbl = Label::new(Some("Disc artist:"));
    let artist_entry = Entry::new();
    artist_entry.set_hexpand(true);
    let album_lbl = Label::new(Some("Disc album:"));
    let album_entry = Entry::new();
    album_entry.set_hexpand(true);
    meta_row.append(&artist_lbl);
    meta_row.append(&artist_entry);
    meta_row.append(&album_lbl);
    meta_row.append(&album_entry);
    root.append(&meta_row);

    // Queue rows ("Artist - Title — M:SS · size"), selectable for the
    // remove/reorder buttons; burn order = disc track order.
    let queue = gtk4::ListBox::new();
    queue.set_selection_mode(gtk4::SelectionMode::Single);
    queue.add_css_class("ml-col-view");
    let queue_scroll = ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(120)
        .child(&queue)
        .build();
    root.append(&queue_scroll);
    let empty_hint = Label::builder()
        .label("Burn list is empty — right-click files in the Library and pick Send to ▸ Disc Drive.")
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    empty_hint.add_css_class("dim-label");
    root.append(&empty_hint);

    // Capacity meters — over-capacity turns the line red and blocks that
    // mode's burn button.
    let audio_meter = Label::builder().halign(Align::Start).xalign(0.0).build();
    audio_meter.add_css_class("dim-label");
    let data_meter = Label::builder().halign(Align::Start).xalign(0.0).build();
    data_meter.add_css_class("dim-label");
    root.append(&audio_meter);
    root.append(&data_meter);

    // Queue management (left) + the two burn actions (right).
    let btn_row = GtkBox::new(Orientation::Horizontal, 6);
    let btn_remove = Button::with_label("− Remove");
    let btn_up = Button::with_label("↑");
    let btn_down = Button::with_label("↓");
    let btn_clear = Button::with_label("Clear");
    let spring = GtkBox::new(Orientation::Horizontal, 0);
    spring.set_hexpand(true);
    let btn_audio = Button::with_label("Burn Audio CD");
    let btn_data = Button::with_label("Burn Data Disc");
    for b in [&btn_remove, &btn_up, &btn_down, &btn_clear, &btn_audio, &btn_data] {
        b.add_css_class("pl-btn");
    }
    btn_row.append(&btn_remove);
    btn_row.append(&btn_up);
    btn_row.append(&btn_down);
    btn_row.append(&btn_clear);
    btn_row.append(&spring);
    btn_row.append(&btn_audio);
    btn_row.append(&btn_data);
    root.append(&btn_row);

    // Progress row (hidden while idle): phase text + Cancel.
    let progress_row = GtkBox::new(Orientation::Horizontal, 6);
    progress_row.set_visible(false);
    let phase_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .build();
    let btn_cancel = Button::with_label("Cancel");
    btn_cancel.add_css_class("pl-btn");
    progress_row.append(&phase_lbl);
    progress_row.append(&btn_cancel);
    root.append(&progress_row);

    // Result/refusal status line for this panel.
    let status = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    status.add_css_class("dim-label");
    root.append(&status);

    // ── Overlay card (Task 7) ───────────────────────────────────────────
    // Centered card media_library.rs floats over the whole disc-detail view
    // while the shown drive has a live burn — "osd"/"card" are GTK4's own
    // built-in style classes (same translucent-panel look the drive-icon
    // badge already uses), so this needs no custom CSS. Duplicates the
    // panel's phase text + Cancel (progress_row/btn_cancel above) because a
    // burn's own drive may not be the one on screen — see refresh_progress.
    let overlay_card = GtkBox::new(Orientation::Vertical, 8);
    overlay_card.add_css_class("osd");
    overlay_card.add_css_class("card");
    overlay_card.add_css_class("burn-overlay-card");
    overlay_card.set_halign(Align::Center);
    overlay_card.set_valign(Align::Center);
    overlay_card.set_margin_top(12);
    overlay_card.set_margin_bottom(12);
    overlay_card.set_margin_start(12);
    overlay_card.set_margin_end(12);
    overlay_card.set_width_request(280);
    overlay_card.set_visible(false);
    let overlay_phase_lbl = Label::builder()
        .halign(Align::Center)
        .justify(gtk4::Justification::Center)
        .wrap(true)
        .build();
    // show-text toggles per update below: percent text while determinate,
    // none while pulsing (a stale/frozen percentage next to a moving pulse
    // block would read as a bug).
    let overlay_bar = gtk4::ProgressBar::new();
    let overlay_cancel = Button::with_label("Cancel");
    overlay_cancel.add_css_class("pl-btn");
    overlay_cancel.set_halign(Align::Center);
    overlay_card.append(&overlay_phase_lbl);
    overlay_card.append(&overlay_bar);
    overlay_card.append(&overlay_cancel);

    let burn_running = Rc::new(Cell::new(false));
    let prep_cancel: Rc<RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>> =
        Rc::new(RefCell::new(None));
    let shown_drive: Rc<RefCell<Option<crate::disc::OpticalDrive>>> =
        Rc::new(RefCell::new(None));
    // Guards the disc artist/album entries' `connect_changed` against the
    // programmatic `set_text` in refresh_cb below — without it, a default
    // sync would look like a user edit, write an override, and the two would
    // fight (crash-adjacent feedback loop; see the three prior live crashes
    // on this branch).
    let meta_syncing = Rc::new(Cell::new(false));

    // ── Refresh: queue rows + meters + sensitivity for the shown drive ────
    let refresh_cb: Rc<dyn Fn(&crate::disc::OpticalDrive)> = {
        let burn_queues = burn_queues.clone();
        let queue = queue.clone();
        let queue_scroll = queue_scroll.clone();
        let empty_hint = empty_hint.clone();
        let audio_meter = audio_meter.clone();
        let data_meter = data_meter.clone();
        let btn_audio = btn_audio.clone();
        let btn_data = btn_data.clone();
        let burn_running = burn_running.clone();
        let shown_drive = shown_drive.clone();
        let meta_row = meta_row.clone();
        let artist_entry = artist_entry.clone();
        let album_entry = album_entry.clone();
        let meta_syncing = meta_syncing.clone();
        Rc::new(move |drive: &crate::disc::OpticalDrive| {
            *shown_drive.borrow_mut() = Some(drive.clone());
            while let Some(child) = queue.first_child() {
                queue.remove(&child);
            }
            let mut queues = burn_queues.borrow_mut();
            let list = queues.queue(&drive.id);
            for item in &list.items {
                let secs = item.duration_secs.unwrap_or(0);
                let line = format!(
                    "{} — {}:{:02} · {:.1} MB",
                    item.display,
                    secs / 60,
                    secs % 60,
                    item.bytes as f64 / 1e6
                );
                let lbl = Label::builder()
                    .label(&gtk_safe(&line))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .margin_start(6)
                    .margin_end(6)
                    .margin_top(2)
                    .margin_bottom(2)
                    .build();
                let row = ListBoxRow::new();
                row.set_child(Some(&lbl));
                queue.append(&row);
            }
            let empty = list.is_empty();
            queue_scroll.set_visible(!empty);
            empty_hint.set_visible(empty);

            // Disc artist/album: while the user hasn't overridden them, keep
            // the entries in sync with the queue's computed defaults. Only
            // set_text when the text actually differs, and only under the
            // syncing flag, so this programmatic write doesn't get mistaken
            // for a user edit by the entries' connect_changed handlers below.
            if list.meta_override.is_none() {
                let defaults = list.effective_meta();
                meta_syncing.set(true);
                let artist = gtk_safe(&defaults.artist);
                if artist_entry.text() != artist {
                    artist_entry.set_text(&artist);
                }
                let album = gtk_safe(&defaults.album);
                if album_entry.text() != album {
                    album_entry.set_text(&album);
                }
                meta_syncing.set(false);
            }

            // Audio meter: queued minutes vs the media's audio capacity.
            let cap = crate::disc::burn::audio_capacity_secs(drive);
            let total = list.total_secs();
            let over_audio = list.over_audio_capacity(cap);
            let unknown = if list.has_unknown_durations() {
                " (some durations unknown — total is a lower bound)"
            } else {
                ""
            };
            audio_meter.set_text(&format!(
                "Audio: {}:{:02} of {}:{:02}{unknown}",
                total / 60,
                total % 60,
                cap / 60,
                cap % 60
            ));
            set_meter_over(&audio_meter, over_audio);

            // Data meter: queued bytes vs free bytes (capacity for blanks).
            let free = if drive.media.free_bytes > 0 {
                drive.media.free_bytes
            } else {
                drive.media.capacity_bytes
            };
            let over_data = free > 0 && list.over_data_capacity(free);
            data_meter.set_text(&format!(
                "Data: {:.1} MB of {:.1} MB",
                list.total_bytes() as f64 / 1e6,
                free as f64 / 1e6
            ));
            set_meter_over(&data_meter, over_data);

            let decision = crate::disc::burn::erase_decision(drive);
            let writable = decision != crate::disc::burn::EraseDecision::Refuse;
            let idle = !burn_running.get();
            btn_audio.set_sensitive(idle && writable && !empty && !over_audio);
            btn_data.set_sensitive(idle && writable && !empty && !over_data);
            meta_row.set_visible(writable);
        })
    };

    // Disc artist/album: a user edit in either entry overrides both fields
    // together (a partial override reading through to a stale default would
    // be more surprising than not). Short borrow only — no rerender from
    // inside a `connect_changed` handler (established feedback-loop trap on
    // this branch; see refresh_cb's meta_syncing guard above).
    let write_meta_override: Rc<dyn Fn()> = {
        let burn_queues = burn_queues.clone();
        let shown_drive = shown_drive.clone();
        let artist_entry = artist_entry.clone();
        let album_entry = album_entry.clone();
        Rc::new(move || {
            let drive_id = shown_drive.borrow().as_ref().map(|d| d.id.clone());
            let Some(id) = drive_id else { return };
            let artist = artist_entry.text().to_string();
            let album = album_entry.text().to_string();
            burn_queues.borrow_mut().queue(&id).meta_override =
                Some(crate::disc::cdtext::DiscMeta { artist, album });
        })
    };
    for entry in [&artist_entry, &album_entry] {
        let meta_syncing = meta_syncing.clone();
        let write_meta_override = write_meta_override.clone();
        entry.connect_changed(move |_| {
            if !meta_syncing.get() {
                write_meta_override();
            }
        });
    }

    // Queue management buttons operate on the selected row.
    let selected_idx = {
        let queue = queue.clone();
        move || queue.selected_row().map(|r| r.index() as usize)
    };
    let rerender: Rc<dyn Fn()> = {
        let refresh_cb = refresh_cb.clone();
        let shown_drive = shown_drive.clone();
        Rc::new(move || {
            // Bind first: the borrow() Ref must drop before refresh_cb
            // re-borrows shown_drive mutably (live crash 2026-07-15).
            let drive = shown_drive.borrow().clone();
            if let Some(d) = drive {
                refresh_cb(&d);
            }
        })
    };
    // Expose the shown-drive rerender so an external "Send to ▸ Disc Drive"
    // add (files view, playlist editor, active playlist) can live-refresh the
    // queue while this detail is open — otherwise the new rows only appeared
    // after navigating away and back (2026-07-16).
    *burn_refresh_holder.borrow_mut() = Some(rerender.clone());
    // Remove row `i` from the shown drive's queue, rerender, then reselect a
    // neighbour so the list stays put instead of jumping to the top (the
    // rerender rebuilds every row, which otherwise drops the selection).
    let remove_at: Rc<dyn Fn(usize)> = {
        let burn_queues = burn_queues.clone();
        let shown_drive = shown_drive.clone();
        let rerender = rerender.clone();
        let queue = queue.clone();
        Rc::new(move |i: usize| {
            let drive_id = shown_drive.borrow().as_ref().map(|d| d.id.clone());
            let Some(id) = drive_id else { return };
            let new_len = {
                let mut q = burn_queues.borrow_mut();
                let list = q.queue(&id);
                list.remove(i);
                list.len()
            };
            rerender();
            if new_len > 0 {
                let sel = i.min(new_len - 1) as i32;
                if let Some(r) = queue.row_at_index(sel) {
                    queue.select_row(Some(&r));
                }
            }
        })
    };
    {
        let selected_idx = selected_idx.clone();
        let remove_at = remove_at.clone();
        btn_remove.connect_clicked(move |_| {
            if let Some(i) = selected_idx() {
                remove_at(i);
            }
        });
    }
    // Delete / Backspace on the queue list removes the selected row, matching
    // the Remove button (keyboard parity — users expect Delete to work here).
    {
        let selected_idx = selected_idx.clone();
        let remove_at = remove_at.clone();
        let key = gtk4::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, _| {
            use gtk4::gdk::Key;
            if matches!(keyval, Key::Delete | Key::KP_Delete | Key::BackSpace) {
                if let Some(i) = selected_idx() {
                    remove_at(i);
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        queue.add_controller(key);
    }
    {
        let burn_queues = burn_queues.clone();
        let shown_drive = shown_drive.clone();
        let selected_idx = selected_idx.clone();
        let rerender = rerender.clone();
        let queue = queue.clone();
        btn_up.connect_clicked(move |_| {
            let drive_id = shown_drive.borrow().as_ref().map(|d| d.id.clone());
            if let (Some(id), Some(i)) = (drive_id, selected_idx()) {
                burn_queues.borrow_mut().queue(&id).move_up(i);
                rerender();
                if i > 0 {
                    if let Some(r) = queue.row_at_index(i as i32 - 1) {
                        queue.select_row(Some(&r));
                    }
                }
            }
        });
    }
    {
        let burn_queues = burn_queues.clone();
        let shown_drive = shown_drive.clone();
        let selected_idx = selected_idx.clone();
        let rerender = rerender.clone();
        let queue = queue.clone();
        btn_down.connect_clicked(move |_| {
            let drive_id = shown_drive.borrow().as_ref().map(|d| d.id.clone());
            if let (Some(id), Some(i)) = (drive_id, selected_idx()) {
                burn_queues.borrow_mut().queue(&id).move_down(i);
                rerender();
                if let Some(r) = queue.row_at_index(i as i32 + 1) {
                    queue.select_row(Some(&r));
                }
            }
        });
    }
    {
        let burn_queues = burn_queues.clone();
        let shown_drive = shown_drive.clone();
        let rerender = rerender.clone();
        btn_clear.connect_clicked(move |_| {
            let drive_id = shown_drive.borrow().as_ref().map(|d| d.id.clone());
            if let Some(id) = drive_id {
                burn_queues.borrow_mut().queue(&id).clear();
                rerender();
            }
        });
    }

    // Cancel: stop between prepare tracks, kill the burn/erase subprocess.
    // Shared by the in-panel button and the overlay card's button (Task 7) —
    // whichever the user sees, it does the exact same thing.
    let do_cancel: Rc<dyn Fn()> = {
        let prep_cancel = prep_cancel.clone();
        let status = status.clone();
        Rc::new(move || {
            if let Some(flag) = prep_cancel.borrow().as_ref() {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            crate::disc::burn::request_cancel();
            status.set_text("Cancelling burn…");
        })
    };
    {
        let do_cancel = do_cancel.clone();
        btn_cancel.connect_clicked(move |_| do_cancel());
    }
    {
        let do_cancel = do_cancel.clone();
        overlay_cancel.connect_clicked(move |_| do_cancel());
    }

    // ── Start a burn (shared by both mode buttons) ─────────────────────────
    let start_burn: Rc<dyn Fn(bool)> = {
        let state = state.clone();
        let burn_queues = burn_queues.clone();
        let shown_drive = shown_drive.clone();
        let burn_running = burn_running.clone();
        let prep_cancel = prep_cancel.clone();
        let progress_row = progress_row.clone();
        let phase_lbl = phase_lbl.clone();
        let status = status.clone();
        let refresh_discs_holder = refresh_discs_holder.clone();
        let rerender = rerender.clone();
        let burn_progress_map = burn_progress_map.clone();
        let overlay_card = overlay_card.clone();
        let overlay_phase_lbl = overlay_phase_lbl.clone();
        let overlay_bar = overlay_bar.clone();
        let win_wk = win.downgrade();
        Rc::new(move |audio: bool| {
            if burn_running.get() {
                status.set_text("A burn is already running.");
                return;
            }
            let Some(drive) = shown_drive.borrow().clone() else {
                return;
            };
            if burn_queues.borrow_mut().queue(&drive.id).is_empty() {
                return;
            }
            use crate::disc::burn::{self, EraseDecision};
            let decision = burn::erase_decision(&drive);
            if decision == EraseDecision::Refuse {
                status.set_text(
                    "This disc can't be written — insert a blank or rewritable disc.",
                );
                return;
            }
            // Capacity guard before anything touches the disc.
            {
                let mut queues = burn_queues.borrow_mut();
                let list = queues.queue(&drive.id);
                if audio && list.over_audio_capacity(burn::audio_capacity_secs(&drive)) {
                    status.set_text("Over the media's audio capacity — remove tracks first.");
                    return;
                }
                let free = if drive.media.free_bytes > 0 {
                    drive.media.free_bytes
                } else {
                    drive.media.capacity_bytes
                };
                if !audio && free > 0 && list.over_data_capacity(free) {
                    status.set_text("Over the disc's free space — remove files first.");
                    return;
                }
            }

            // Everything below actually runs the burn; RW-with-content asks
            // for the explicit erase confirmation first (never auto-blank).
            let run = {
                let state = state.clone();
                let burn_queues = burn_queues.clone();
                let burn_running = burn_running.clone();
                let prep_cancel = prep_cancel.clone();
                let progress_row = progress_row.clone();
                let phase_lbl = phase_lbl.clone();
                let status = status.clone();
                let refresh_discs_holder = refresh_discs_holder.clone();
                let rerender = rerender.clone();
                let shown_drive = shown_drive.clone();
                let burn_progress_map = burn_progress_map.clone();
                let overlay_card = overlay_card.clone();
                let overlay_phase_lbl = overlay_phase_lbl.clone();
                let overlay_bar = overlay_bar.clone();
                let drive = drive.clone();
                Rc::new(move |erase_first: bool| {
                    let verify = state.borrow().config.disc.burn_verify;
                    let use_m3u = matches!(
                        state.borrow().config.media_library.playlist_format,
                        crate::config::PlaylistFormat::M3u
                    );
                    let drive_id = drive.id.clone();
                    let items = burn_queues.borrow_mut().queue(&drive_id).items.clone();
                    // Editing the sheet's contents before a burn is Task 5 —
                    // for now audio burns always use the queue's effective
                    // (override-or-default) metadata; data burns carry no
                    // CD-TEXT.
                    let disc_meta =
                        audio.then(|| burn_queues.borrow_mut().queue(&drive_id).effective_meta());
                    let cancel =
                        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    *prep_cancel.borrow_mut() = Some(cancel.clone());
                    burn_running.set(true);
                    state.borrow().disc_reading.set(true);
                    progress_row.set_visible(true);
                    phase_lbl.set_text("Starting…");
                    status.set_text("");
                    // Overlay card (Task 7): the drive we're about to burn is
                    // always the one currently shown (start_burn only reads
                    // `shown_drive`), so it's safe to show unconditionally
                    // here — the map entry is what makes it re-show on its
                    // own if the user navigates away and back mid-burn.
                    let starting = crate::disc::burn::BurnProgress {
                        label: "Starting…".to_string(),
                        fraction: None,
                    };
                    burn_progress_map
                        .borrow_mut()
                        .insert(drive_id.clone(), starting);
                    overlay_phase_lbl.set_text("Starting…");
                    overlay_bar.set_fraction(0.0);
                    overlay_bar.set_show_text(false);
                    overlay_card.set_visible(true);
                    let (tx, rx) = std::sync::mpsc::channel::<BurnMsg>();

                    let drive = drive.clone();
                    let audio_mode = audio;
                    std::thread::spawn(move || {
                        // The whole orchestration (staging, erase, prep, burn,
                        // cache invalidation, cleanup) is the shared core job —
                        // this worker only forwards its phases to the poller.
                        use crate::disc::burn;
                        let mode = if audio_mode {
                            burn::BurnMode::Audio
                        } else {
                            burn::BurnMode::Data { use_m3u }
                        };
                        let result = burn::run_job(
                            &drive, &items, mode, erase_first, verify,
                            disc_meta.as_ref(), &cancel,
                            |p| {
                                let _ = tx.send(BurnMsg::Progress(p));
                            },
                        );
                        let _ = tx.send(BurnMsg::Done(result));
                    });

                    // Main-thread poller: phases into the label, Done wraps up.
                    let state_p = state.clone();
                    let burn_queues_p = burn_queues.clone();
                    let drive_id_p = drive_id.clone();
                    let burn_running_p = burn_running.clone();
                    let prep_cancel_p = prep_cancel.clone();
                    let progress_row_p = progress_row.clone();
                    let phase_lbl_p = phase_lbl.clone();
                    let status_p = status.clone();
                    let refresh_holder_p = refresh_discs_holder.clone();
                    let rerender_p = rerender.clone();
                    let shown_drive_p = shown_drive.clone();
                    let burn_progress_map_p = burn_progress_map.clone();
                    let overlay_card_p = overlay_card.clone();
                    let overlay_phase_lbl_p = overlay_phase_lbl.clone();
                    let overlay_bar_p = overlay_bar.clone();
                    glib::timeout_add_local(
                        std::time::Duration::from_millis(200),
                        move || {
                            let mut done: Option<Result<String, String>> = None;
                            loop {
                                match rx.try_recv() {
                                    Ok(BurnMsg::Progress(p)) => {
                                        phase_lbl_p.set_text(&gtk_safe(&p.label));
                                        burn_progress_map_p
                                            .borrow_mut()
                                            .insert(drive_id_p.clone(), p);
                                    }
                                    Ok(BurnMsg::Done(r)) => {
                                        done = Some(r);
                                        break;
                                    }
                                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                        done = Some(Err("burn worker vanished".into()));
                                        break;
                                    }
                                }
                            }
                            // Live overlay refresh: only one burn ever runs
                            // at a time (the "already running" guard above),
                            // so if the shown drive is this burn's drive,
                            // this poller is the sole owner of the overlay
                            // widgets right now — safe to touch them
                            // directly. `fraction: None` is re-pulsed every
                            // tick even without a fresh message (Erasing and
                            // xorriso data burns only send one Progress at
                            // the phase's start — this is what makes the
                            // bar visibly animate instead of sitting stuck).
                            let showing_this = shown_drive_p
                                .borrow()
                                .as_ref()
                                .is_some_and(|d| d.id == drive_id_p);
                            if showing_this {
                                let snapshot =
                                    burn_progress_map_p.borrow().get(&drive_id_p).cloned();
                                if let Some(p) = snapshot {
                                    overlay_phase_lbl_p.set_text(&gtk_safe(&p.label));
                                    match p.fraction {
                                        Some(f) => {
                                            overlay_bar_p.set_fraction(f as f64);
                                            overlay_bar_p.set_show_text(true);
                                            overlay_bar_p
                                                .set_text(Some(&format!("{:.0}%", f * 100.0)));
                                        }
                                        None => {
                                            overlay_bar_p.pulse();
                                            overlay_bar_p.set_show_text(false);
                                        }
                                    }
                                }
                            }
                            if let Some(result) = done {
                                progress_row_p.set_visible(false);
                                burn_running_p.set(false);
                                state_p.borrow().disc_reading.set(false);
                                *prep_cancel_p.borrow_mut() = None;
                                burn_progress_map_p.borrow_mut().remove(&drive_id_p);
                                overlay_card_p.set_visible(false);
                                match result {
                                    Ok(summary) => {
                                        burn_queues_p.borrow_mut().queue(&drive_id_p).clear();
                                        status_p.set_text(&gtk_safe(&summary));
                                        // Re-poll so the detail shows the
                                        // disc's new content.
                                        if let Some(f) =
                                            refresh_holder_p.borrow().clone()
                                        {
                                            f();
                                        }
                                    }
                                    Err(e) => status_p
                                        .set_text(&gtk_safe(&format!("Burn failed: {e}"))),
                                }
                                rerender_p();
                                return glib::ControlFlow::Break;
                            }
                            glib::ControlFlow::Continue
                        },
                    );
                })
            };

            if decision == EraseDecision::EraseAfterConfirm {
                confirm_erase_dialog(win_wk.upgrade().as_ref(), {
                    let run = run.clone();
                    move || run(true)
                });
            } else {
                run(false);
            }
        })
    };
    {
        let start = start_burn.clone();
        btn_audio.connect_clicked(move |_| start(true));
    }
    {
        let start = start_burn.clone();
        btn_data.connect_clicked(move |_| start(false));
    }

    // Restore/hide the overlay card for one drive id from the shared
    // progress map — see `BurnUi::refresh_progress`. `fraction: None` here
    // just parks the bar at 0 (no pulse): if the map has an entry, that
    // drive's burn — and its poller — is still running by construction
    // (entries are inserted at burn start and removed together with the
    // burn's own completion), so the poller's own next 200 ms tick resumes
    // the animation; this call only needs to get the static parts right.
    let progress_refresh_cb: Rc<dyn Fn(&str)> = {
        let burn_progress_map = burn_progress_map.clone();
        let overlay_card = overlay_card.clone();
        let overlay_phase_lbl = overlay_phase_lbl.clone();
        let overlay_bar = overlay_bar.clone();
        Rc::new(move |drive_id: &str| {
            let snapshot = burn_progress_map.borrow().get(drive_id).cloned();
            match snapshot {
                Some(p) => {
                    overlay_phase_lbl.set_text(&gtk_safe(&p.label));
                    match p.fraction {
                        Some(f) => {
                            overlay_bar.set_fraction(f as f64);
                            overlay_bar.set_show_text(true);
                            overlay_bar.set_text(Some(&format!("{:.0}%", f * 100.0)));
                        }
                        None => {
                            overlay_bar.set_fraction(0.0);
                            overlay_bar.set_show_text(false);
                        }
                    }
                    overlay_card.set_visible(true);
                }
                None => overlay_card.set_visible(false),
            }
        })
    };

    BurnUi {
        root,
        overlay_card,
        refresh_cb,
        progress_refresh_cb,
    }
}

/// Mark a capacity meter line as over/OK (red via the shared "broken" class).
fn set_meter_over(meter: &Label, over: bool) {
    if over {
        meter.remove_css_class("dim-label");
        meter.add_css_class("broken");
    } else {
        meter.remove_css_class("broken");
        meter.add_css_class("dim-label");
    }
}

/// Modal "erase and burn?" confirmation for rewritable media with content.
/// `on_confirm` runs only on the explicit yes — never auto-blank.
fn confirm_erase_dialog(parent: Option<&gtk4::Window>, on_confirm: impl Fn() + 'static) {
    let dialog = gtk4::Window::builder()
        .title("Erase disc?")
        .modal(true)
        .default_width(400)
        .build();
    if let Some(p) = parent {
        dialog.set_transient_for(Some(p));
    }
    let vbox = GtkBox::new(Orientation::Vertical, 10);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);
    vbox.append(
        &Label::builder()
            .label(
                "This rewritable disc already has content. Burning will ERASE \
                 everything on it first. This cannot be undone.",
            )
            .wrap(true)
            .halign(Align::Start)
            .xalign(0.0)
            .build(),
    );
    let btns = GtkBox::new(Orientation::Horizontal, 6);
    btns.set_halign(Align::End);
    let cancel = Button::with_label("Cancel");
    let erase = Button::with_label("Erase and Burn");
    erase.add_css_class("destructive-action");
    btns.append(&cancel);
    btns.append(&erase);
    vbox.append(&btns);
    dialog.set_child(Some(&vbox));
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    erase.connect_clicked(move |_| {
        on_confirm();
        d.close();
    });
    dialog.present();
}

/// Whether the Submit-to-gnudb action applies: the disc is unknown to gnudb
/// (no official baseline) or the user's tags differ from the official match.
/// Same field set the macOS `discSubmittable` compares.
pub(super) fn disc_submittable(
    discid: &str,
    disc_tags: &std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>,
    disc_official: &std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>,
) -> bool {
    let Some(official) = disc_official.get(discid) else {
        return true;
    };
    let Some(user) = disc_tags.get(discid) else {
        return false;
    };
    user.artist != official.artist
        || user.album != official.album
        || user.year != official.year
        || user.genre != official.genre
        || user.track_titles != official.track_titles
}

/// Wire Phase 4: the Submit-to-gnudb button. Validates locally, collects the
/// email once (gnudb requires a personal address), picks a category
/// (prefilled from the genre), then POSTs on a worker thread. Honors the
/// test-mode setting; results land in the status label.
#[allow(clippy::type_complexity)]
pub(super) fn connect_submit(
    submit_btn: &Button,
    state: Rc<RefCell<AppState>>,
    status: Label,
    win: &gtk4::Window,
    disc_tags: Rc<RefCell<std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>>>,
    disc_official: Rc<RefCell<std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>>>,
    selected_disc_id: Rc<RefCell<Option<String>>>,
    current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>>,
) {
    let in_flight = Rc::new(Cell::new(false));
    let win_wk = win.downgrade();

    // The category dialog + POST, entered once the email is known.
    let open_category_dialog: Rc<dyn Fn()> = {
        let state = state.clone();
        let status = status.clone();
        let disc_tags = disc_tags.clone();
        let disc_official = disc_official.clone();
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let in_flight = in_flight.clone();
        let win_wk = win_wk.clone();
        Rc::new(move || {
            let Some((toc, discid)) = selected_disc_discid(&selected_disc_id, &current_drives)
            else {
                status.set_text("No audio disc loaded");
                return;
            };
            let Some(mut entry) = disc_tags.borrow().get(&discid).cloned() else {
                status.set_text("No tags yet — Identify or Edit Tags first");
                return;
            };
            // Revision: updating an official match needs old + 1; a disc
            // gnudb doesn't know starts at 0.
            entry.revision = disc_official
                .borrow()
                .get(&discid)
                .map(|o| o.revision + 1)
                .unwrap_or(0);
            // Fast local validation for immediate feedback (the server would
            // reject these anyway, after a round-trip).
            if let Err(reason) = crate::disc::xmcd::validate_for_submit(&entry, &toc) {
                status.set_text(&gtk_safe(&reason));
                return;
            }

            let test_mode = state.borrow().config.disc.gnudb_submit_mode_test;
            let dialog = gtk4::Window::builder()
                .title("Submit to gnudb")
                .modal(true)
                .default_width(400)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let outer = GtkBox::new(Orientation::Vertical, 8);
            outer.set_margin_top(12);
            outer.set_margin_bottom(12);
            outer.set_margin_start(12);
            outer.set_margin_end(12);
            outer.append(
                &Label::builder()
                    .label(&gtk_safe(&format!(
                        "Send \"{} — {}\" to gnudb.org so other players can identify this disc.",
                        entry.artist, entry.album
                    )))
                    .wrap(true)
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build(),
            );
            if test_mode {
                let note = Label::builder()
                    .label("Test mode is on (Settings): the server validates the submission but does not publish it.")
                    .wrap(true)
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                note.add_css_class("dim-label");
                outer.append(&note);
            }
            let cat_row = GtkBox::new(Orientation::Horizontal, 6);
            cat_row.append(
                &Label::builder()
                    .label("Category")
                    .halign(Align::Start)
                    .build(),
            );
            let cat_dd = gtk4::DropDown::from_strings(&crate::disc::gnudb::CATEGORIES);
            // Typeahead over the fixed CDDB category set.
            cat_dd.set_expression(Some(&gtk4::PropertyExpression::new(
                gtk4::StringObject::static_type(),
                None::<gtk4::Expression>,
                "string",
            )));
            cat_dd.set_enable_search(true);
            let suggested = crate::disc::gnudb::suggest_category(&entry.genre);
            let idx = crate::disc::gnudb::CATEGORIES
                .iter()
                .position(|c| *c == suggested)
                .unwrap_or(0);
            cat_dd.set_selected(idx as u32);
            cat_row.append(&cat_dd);
            outer.append(&cat_row);

            let btns = GtkBox::new(Orientation::Horizontal, 6);
            btns.set_halign(Align::End);
            let cancel = Button::with_label("Cancel");
            let send = Button::with_label("Submit");
            send.add_css_class("suggested-action");
            btns.append(&cancel);
            btns.append(&send);
            outer.append(&btns);
            dialog.set_child(Some(&outer));
            let d = dialog.clone();
            cancel.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let state = state.clone();
            let status = status.clone();
            let in_flight = in_flight.clone();
            send.connect_clicked(move |_| {
                let category = crate::disc::gnudb::CATEGORIES
                    [cat_dd.selected() as usize];
                let email = state.borrow().config.disc.gnudb_email.clone();
                in_flight.set(true);
                status.set_text(if test_mode {
                    "Submitting to gnudb (test mode)…"
                } else {
                    "Submitting to gnudb…"
                });
                let entry = entry.clone();
                let toc = toc.clone();
                let status = status.clone();
                let in_flight = in_flight.clone();
                glib::MainContext::default().spawn_local(async move {
                    let result = gio::spawn_blocking(move || {
                        use crate::disc::{discid as discid_mod, gnudb, xmcd};
                        let body = xmcd::build(&entry, &toc, entry.revision);
                        let id = discid_mod::freedb_discid(&toc);
                        gnudb::submit(&body, category, &id, &email, test_mode)
                            .map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|_| Err("submit task failed".into()));
                    in_flight.set(false);
                    match result {
                        Ok(server_msg) => status.set_text(&gtk_safe(&if test_mode {
                            format!("gnudb: {server_msg} (test mode — not published)")
                        } else {
                            format!("gnudb: {server_msg}")
                        })),
                        Err(e) => status.set_text(&gtk_safe(&format!("gnudb: {e}"))),
                    }
                });
                d.close();
            });
            dialog.present();
        })
    };

    submit_btn.connect_clicked(move |_| {
        if in_flight.get() {
            status.set_text("gnudb request already running…");
            return;
        }
        // gnudb requires a personal address (the config ships blank on
        // purpose) — capture it before submitting, and re-prompt when the
        // stored value doesn't look like a real address.
        let email = state.borrow().config.disc.gnudb_email.clone();
        if crate::disc::gnudb::is_unset_email(&email)
            || !crate::disc::gnudb::is_valid_email(&email)
        {
            super::prompt_gnudb_email(
                win_wk.upgrade().as_ref(),
                state.clone(),
                open_category_dialog.clone(),
            );
        } else {
            open_category_dialog();
        }
    });
}

/// Wire the Eject button: refuses while the drive is being read (playback of
/// a cdda:// track from this drive, or a running rip — the OS would refuse
/// anyway, with a worse message), then runs the blocking eject off the UI
/// thread and refreshes the drive list on success.
pub(super) fn connect_eject(
    eject_btn: &Button,
    state: Rc<RefCell<AppState>>,
    rip_active: Rc<Cell<bool>>,
    status: Label,
    selected_disc_id: Rc<RefCell<Option<String>>>,
    refresh_discs: Rc<dyn Fn()>,
) {
    eject_btn.connect_clicked(move |btn| {
        let Some(id) = selected_disc_id.borrow().clone() else {
            return;
        };
        if rip_active.get() {
            status.set_text("Can't eject during a rip.");
            return;
        }
        // Playing a cdda:// track from THIS drive holds the device open.
        {
            let s = state.borrow();
            let playing_this_drive = !matches!(
                s.player.state(),
                crate::engine::PlayerState::Stopped
            ) && s
                .playlist
                .current()
                .map(|t| t.path.to_string_lossy())
                .and_then(|p| {
                    crate::disc::parse_cdda_uri(&p).map(|(_, dev)| dev == Some(id.as_str()))
                })
                .unwrap_or(false);
            if playing_this_drive {
                status.set_text("Stop disc playback before ejecting.");
                return;
            }
        }
        // Spinner replaces the label while the tray moves (mac "Ejecting…").
        super::set_button_busy(btn, true, "Eject");
        status.set_text("Ejecting…");
        let btn = btn.clone();
        let status = status.clone();
        let refresh = refresh_discs.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(move || crate::disc::detect::eject(&id))
                .await
                .unwrap_or_else(|_| Err("eject task failed".into()));
            super::set_button_busy(&btn, false, "Eject");
            match result {
                Ok(()) => {
                    status.set_text("");
                    refresh();
                }
                Err(e) => status.set_text(&gtk_safe(&format!("Couldn't eject: {e}"))),
            }
        });
    });
}

/// Wire the Phase-3 rip flow: the "Rip…" button opens the setup dialog
/// (track multi-select, destination, quality), Cancel stops the running rip
/// after the current track.
#[allow(clippy::too_many_arguments)]
pub(super) fn connect_rip_ui(
    ui: DiscRipUi,
    rip_btn: &Button,
    cancel_btn: &Button,
    win: &gtk4::Window,
    entries_store: Rc<RefCell<Vec<crate::disc::DiscTrackEntry>>>,
    disc_tags: Rc<RefCell<std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>>>,
    selected_disc_id: Rc<RefCell<Option<String>>>,
    current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>>,
) {
    // Cancel the running rip (stops after the current track finishes).
    {
        let rip_cancel = ui.rip_cancel.clone();
        let status = ui.status.clone();
        cancel_btn.connect_clicked(move |_| {
            if let Some(flag) = rip_cancel.borrow().as_ref() {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                status.set_text("Cancelling after the current track…");
            }
        });
    }

    // Rip button: dialog to pick tracks / destination / quality, then rip.
    let win_wk = win.downgrade();
    rip_btn.connect_clicked(move |_| {
        let status = &ui.status;
        if ui.rip_active.get() {
            status.set_text("A rip is already running.");
            return;
        }
        // An active cdda:// playback shares the drive head with the rip's
        // cdiocddasrc — the device allows one reader, so both would thrash.
        // Refuse instead of wedging (same contention rule as the disc poll).
        {
            let s = ui.state.borrow();
            let playing_disc = !matches!(s.player.state(), crate::engine::PlayerState::Stopped)
                && s.playlist
                    .current()
                    .map(|t| t.path.to_string_lossy().starts_with("cdda://"))
                    .unwrap_or(false);
            if playing_disc {
                status.set_text("Stop disc playback before ripping (one reader per drive).");
                return;
            }
        }
        let Some((_, discid)) = selected_disc_discid(&selected_disc_id, &current_drives) else {
            status.set_text("No audio disc loaded");
            return;
        };
        let entries = entries_store.borrow().clone();
        if entries.is_empty() {
            return;
        }
        let watched = watched_folders(&ui.state);
        let dest_default = crate::disc::rip::default_dest(
            ui.state.borrow().config.disc.rip_dest_dir.as_deref(),
            watched.first().map(String::as_str),
        );
        let quality_cfg = ui.state.borrow().config.disc.rip_mp3_quality;

        let dialog = gtk4::Window::builder()
            .title("Rip Audio CD")
            .modal(true)
            .default_width(460)
            .default_height(520)
            .build();
        if let Some(w) = win_wk.upgrade() {
            dialog.set_transient_for(Some(&w));
        }
        let outer = GtkBox::new(Orientation::Vertical, 8);
        outer.set_margin_top(12);
        outer.set_margin_bottom(12);
        outer.set_margin_start(12);
        outer.set_margin_end(12);

        // Header row: label + Select All / Deselect All. Every track starts
        // selected — ripping the whole disc is the common case.
        let tracks_hdr = GtkBox::new(Orientation::Horizontal, 6);
        let tracks_lbl = Label::builder()
            .label("Tracks to rip")
            .halign(Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .build();
        let select_all = Button::with_label("Select All");
        let deselect_all = Button::with_label("Deselect All");
        for b in [&select_all, &deselect_all] {
            b.add_css_class("pl-btn");
        }
        tracks_hdr.append(&tracks_lbl);
        tracks_hdr.append(&select_all);
        tracks_hdr.append(&deselect_all);
        outer.append(&tracks_hdr);
        let list = gtk4::ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::Multiple);
        {
            let l = list.clone();
            select_all.connect_clicked(move |_| l.select_all());
        }
        {
            let l = list.clone();
            deselect_all.connect_clicked(move |_| l.unselect_all());
        }
        for e in entries.iter() {
            let lbl = Label::builder()
                .label(&gtk_safe(&format!(
                    "{}. {}",
                    e.number,
                    e.title.replace(" / ", " - ")
                )))
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(6)
                .margin_end(6)
                .margin_top(3)
                .margin_bottom(3)
                .build();
            let row = ListBoxRow::new();
            row.set_child(Some(&lbl));
            list.append(&row);
            list.select_row(Some(&row));
        }
        let list_scroll = ScrolledWindow::builder().vexpand(true).child(&list).build();
        outer.append(&list_scroll);

        let dest_row = GtkBox::new(Orientation::Horizontal, 6);
        dest_row.append(
            &Label::builder()
                .label("Folder")
                .width_chars(7)
                .halign(Align::Start)
                .build(),
        );
        let dest_entry = Entry::new();
        dest_entry.set_hexpand(true);
        dest_entry.set_text(&gtk_safe(&dest_default));
        let browse = Button::with_label("Browse…");
        dest_row.append(&dest_entry);
        dest_row.append(&browse);
        outer.append(&dest_row);

        let warn = Label::builder()
            .halign(Align::Start)
            .xalign(0.0)
            .wrap(true)
            .build();
        warn.add_css_class("dim-label");
        outer.append(&warn);
        let update_warn: Rc<dyn Fn()> = {
            let warn = warn.clone();
            let dest_entry = dest_entry.clone();
            Rc::new(move || {
                let dest = dest_entry.text().to_string();
                warn.set_text(if crate::disc::rip::dest_is_watched(&dest, &watched) {
                    ""
                } else {
                    "⚠ Not a watched folder — files rip here but won't appear in the library."
                });
            })
        };
        update_warn();
        {
            let uw = update_warn.clone();
            dest_entry.connect_changed(move |_| uw());
        }
        {
            let dest_entry = dest_entry.clone();
            let dialog2 = dialog.clone();
            browse.connect_clicked(move |_| {
                let fd = gtk4::FileDialog::builder()
                    .title("Choose rip destination")
                    .build();
                let dest_entry = dest_entry.clone();
                fd.select_folder(Some(&dialog2), gio::Cancellable::NONE, move |res| {
                    if let Ok(folder) = res {
                        if let Some(p) = folder.path() {
                            dest_entry.set_text(&p.display().to_string());
                        }
                    }
                });
            });
        }

        let qbox = GtkBox::new(Orientation::Horizontal, 6);
        qbox.append(
            &Label::builder()
                .label("Quality")
                .width_chars(7)
                .halign(Align::Start)
                .build(),
        );
        let qdd = gtk4::DropDown::from_strings(&[
            "VBR V0 (~245 kbps)",
            "VBR V2 (~190 kbps)",
            "320 kbps CBR",
        ]);
        qdd.set_selected(match quality_cfg {
            0 => 0,
            2 => 2,
            _ => 1,
        });
        qbox.append(&qdd);
        outer.append(&qbox);

        let btns = GtkBox::new(Orientation::Horizontal, 6);
        btns.set_halign(Align::End);
        let cancel = Button::with_label("Cancel");
        let start = Button::with_label("Rip");
        start.add_css_class("suggested-action");
        btns.append(&cancel);
        btns.append(&start);
        outer.append(&btns);
        dialog.set_child(Some(&outer));
        let d = dialog.clone();
        cancel.connect_clicked(move |_| d.close());

        let d = dialog.clone();
        let ui = ui.clone();
        let disc_tags = disc_tags.clone();
        let total = entries.len() as u8;
        start.connect_clicked(move |_| {
            let chosen: Vec<crate::disc::DiscTrackEntry> = list
                .selected_rows()
                .iter()
                .filter_map(|r| entries.get(r.index() as usize).cloned())
                .collect();
            if chosen.is_empty() {
                return;
            }
            let dest = dest_entry.text().to_string();
            if dest.trim().is_empty() {
                return;
            }
            let quality = match qdd.selected() {
                0 => 0u8,
                2 => 2u8,
                _ => 1u8,
            };
            {
                let mut s = ui.state.borrow_mut();
                s.config.disc.rip_dest_dir = Some(std::path::PathBuf::from(dest.trim()));
                s.config.disc.rip_mp3_quality = quality;
                let _ = s.config.save();
            }
            let tags = disc_tags.borrow().get(&discid).cloned().unwrap_or_default();
            start_rip(&ui, chosen, dest.trim().to_string(), quality, tags, total);
            d.close();
        });
        dialog.present();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::{MediaInfo, MediaKind, OpticalDrive};

    fn drive(present: bool, kind: MediaKind, capacity: u64) -> OpticalDrive {
        OpticalDrive {
            id: "/dev/sr0".into(),
            label: "TEST".into(),
            media: MediaInfo {
                present,
                kind,
                capacity_bytes: capacity,
                ..MediaInfo::none()
            },
            toc: None,
            mount_path: None,
        }
    }

    #[test]
    fn media_badge_rules() {
        assert_eq!(media_badge(&drive(false, MediaKind::Unknown, 0)), None);
        assert_eq!(media_badge(&drive(true, MediaKind::CdR, 0)), Some("CD-R"));
        assert_eq!(media_badge(&drive(true, MediaKind::DvdRam, 0)), Some("DVD-RAM"));
        // Pressed discs: split CD/DVD by capacity; an audio CD (capacity
        // unknown = 0) reads CD.
        assert_eq!(media_badge(&drive(true, MediaKind::Unknown, 0)), Some("CD"));
        assert_eq!(
            media_badge(&drive(true, MediaKind::Unknown, 4_700_000_000)),
            Some("DVD")
        );
    }
}
