//! gnudb identify and manual tag override for the Media Library's Disc
//! Drives page.
//!
//! Split from [`super::disc_page`] (plan step 5, third cut). Two buttons'
//! worth of behaviour, and both write through the same seam: the page's
//! `commit_disc_tags`, which persists a disc's tags to the shared store,
//! re-renders the detail if that disc is showing, and pushes the new titles
//! into playlist rows already added from it.
//!
//! **Identify** looks the loaded disc up by its freedb id, offers a chooser
//! when the lookup returns more than one match, fetches the chosen entry's
//! xmcd in the background and commits it as both the user tags and the
//! official (submission-baseline) copy.
//!
//! **Edit Tags** is the manual path: a dialog of per-track title entries plus
//! album and artist, committed as user tags with the official copy left
//! alone, so a later Submit still knows what the disc originally claimed.
//!
//! Nothing here is read back by the rest of the page — the block declares no
//! bindings that outlive it, which is what made it the next narrow cut after
//! the data-disc browser.

use gtk4::prelude::*;
use gtk4::{gio, glib, Align, Box as GtkBox, Button, Entry, Label, ListBoxRow, Orientation,
    ScrolledWindow};
use std::cell::RefCell;
use std::rc::Rc;

use super::disc::selected_disc_discid;
use super::{gtk_safe, prompt_gnudb_email, MlCtx};

/// The disc state these two buttons read and write. Bundled rather than
/// passed as eight positional arguments, for the reason [`MlCtx`] exists.
pub(super) struct TagUi<'a> {
    /// The drive currently shown in the detail view.
    pub selected_disc_id: &'a Rc<RefCell<Option<String>>>,
    /// The user's tag set per freedb id — what the UI displays.
    pub disc_tags:
        &'a Rc<RefCell<std::collections::HashMap<String, sparkamp::disc::xmcd::XmcdEntry>>>,
    /// CD-TEXT read off the disc itself, used to prefill the manual editor
    /// when there are no gnudb tags yet.
    pub disc_cdtext:
        &'a Rc<RefCell<std::collections::HashMap<String, sparkamp::disc::xmcd::XmcdEntry>>>,
    /// The audio tracks of the loaded disc, for the editor's row count.
    pub current_disc_entries: &'a Rc<RefCell<Vec<sparkamp::disc::DiscTrackEntry>>>,
    /// Persist + re-render + push-into-playlist. Built by the page.
    pub commit_disc_tags: &'a Rc<
        dyn Fn(String, sparkamp::disc::xmcd::XmcdEntry, Option<sparkamp::disc::xmcd::XmcdEntry>),
    >,
    /// The detail view's shared status label.
    pub status_lbl: &'a Label,
}

/// Wire the "Identify" and "Edit Tags" buttons of the drive detail view.
pub(super) fn connect(ctx: &MlCtx, identify: &Button, edit_tags: &Button, ui: TagUi<'_>) {
    // Local names for what this takes from its context and its caller, so the
    // body below reads as it did inside `disc_page::build`.
    let state = ctx.host.state.clone();
    let current_drives = ctx.host.current_drives.clone();
    let win = ctx.win.clone();
    let disc_identify = identify.clone();
    let disc_edit_tags = edit_tags.clone();
    let selected_disc_id = ui.selected_disc_id.clone();
    let disc_tags = ui.disc_tags.clone();
    let disc_cdtext = ui.disc_cdtext.clone();
    let current_disc_entries = ui.current_disc_entries.clone();
    let commit_disc_tags = ui.commit_disc_tags.clone();
    let disc_status_lbl = ui.status_lbl.clone();

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
                    match sparkamp::disc::gnudb::read(&category, &matched_id, &email) {
                        Ok(text) => sparkamp::disc::xmcd::parse(&text)
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
    let open_match_picker: Rc<dyn Fn(String, Vec<sparkamp::disc::gnudb::DiscMatch>)> = {
        let apply = apply_disc_match.clone();
        let win_wk = win.downgrade();
        Rc::new(move |discid: String, matches: Vec<sparkamp::disc::gnudb::DiscMatch>| {
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
                    gio::spawn_blocking(move || sparkamp::disc::gnudb::query(&toc, &email)).await;
                identify_btn2.set_sensitive(true);
                match res {
                    Ok(Ok(matches)) if matches.is_empty() => {
                        status.set_text("No gnudb match. Use Edit Tags to fill them in.");
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
            if sparkamp::disc::gnudb::is_unset_email(&email) {
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
        let disc_cdtext = disc_cdtext.clone();
        let entries_store = current_disc_entries.clone();
        let commit = commit_disc_tags.clone();
        let status = disc_status_lbl.clone();
        let win_wk = win.downgrade();
        disc_edit_tags.connect_clicked(move |_| {
            let Some((_, discid)) = selected_disc_discid(&selected_disc_id, &current_drives) else {
                status.set_text("No audio disc loaded");
                return;
            };
            // Prefer a real gnudb/user entry; fall back to CD-TEXT so a
            // CD-TEXT-only disc (gnudb has no match) prefills artist/album
            // instead of opening blank. Bind the gnudb lookup to a local
            // first so the two RefCell borrows never overlap.
            let gnudb = disc_tags.borrow().get(&discid).cloned();
            let stored = gnudb.or_else(|| disc_cdtext.borrow().get(&discid).cloned());
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
}
