//! The playlist editor's row context menu.
//!
//! Split from [`super::playlists`] (plan step 7) — what [`super::files_menu`]
//! is to the Files page and [`super::devices_menu`] is to the device view,
//! and what §3.3 of docs/gtk-breakup-plan.md sketched as `editor_menu.rs`.
//!
//! Add to / Replace the active playlist, Edit ID3 (single selection only),
//! and Remove from Playlist. Artwork is reached by clicking the thumbnail in
//! the artwork column rather than through a menu entry, the same way it works
//! in the Files and device views.
//!
//! **Deletion Rule**: "Remove" takes the row out of *this playlist only*. The
//! file stays on disk and the track stays in the library — removing from a
//! playlist is never a delete (see CLAUDE.md).
//!
//! Two things this inherited and has to keep, both learned the hard way and
//! both shared with the device menu:
//!
//! - The action group goes on the same stable widget the `PopoverMenu` is
//!   parented to, not scattered across the view and the window. A group the
//!   popover has to reach by walking ancestors loses dispatch.
//! - The previous popover is unparented at the *top* of the popup closure,
//!   before the new one calls `set_parent`.

use gtk4::prelude::*;
use gtk4::{gio, glib, ColumnView, ScrolledWindow};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::playlists::EditorEntry;
use super::{
    notify_playlist_changed, open_id3_editor_window, run_playlist_save_dialog,
    show_playlist_save_error, view_or_search_lyrics, LyricsMode, MlCtx,
};

/// The editor state the menu's actions read and write.
pub(super) struct EditorMenuUi<'a> {
    /// Gesture + action-group host, and the popover's parent.
    pub track_scroll_holder: &'a Rc<RefCell<Option<ScrolledWindow>>>,
    /// The editor's table, for translating cell coordinates.
    pub track_list: &'a Rc<ColumnView>,
    /// The right-clicked row's canonical play-order slot, recorded by the
    /// cell gesture so single-row actions hit the exact row even when the
    /// playlist lists duplicates of one path.
    pub ctx_canonical_idx: &'a Rc<Cell<i64>>,
    /// The tracks being edited.
    pub editing_tracks: &'a Rc<RefCell<Vec<crate::media_library::LibTrack>>>,
    /// Re-render the editor table after a removal.
    pub rebuild_track_list: &'a Rc<dyn Fn()>,
    /// Filled here; called by each row cell's right-click gesture.
    pub ple_action_group_holder: &'a Rc<RefCell<Option<gio::SimpleActionGroup>>>,
    /// The editor's live selection.
    pub edit_multi_sel: &'a gtk4::MultiSelection,
}

/// Build the editor's row context menu and publish its action group.
pub(super) fn connect(ctx: &MlCtx, ui: EditorMenuUi<'_>) {
    // Local names for what this takes from its context and its caller, so the
    // body below reads as it did inside `playlists::build`.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let set_track = ctx.host.set_track.clone();
    let win = ctx.win.clone();
    let track_scroll_holder = ui.track_scroll_holder.clone();
    let track_list = ui.track_list.clone();
    let ctx_canonical_idx = ui.ctx_canonical_idx.clone();
    let editing_tracks = ui.editing_tracks.clone();
    let rebuild_track_list = ui.rebuild_track_list.clone();
    let ple_action_group_holder = ui.ple_action_group_holder.clone();
    let edit_multi_sel = ui.edit_multi_sel.clone();

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
                    None,
                );
            });
            action_group.add_action(&action);
        }

        // ─── View/Search Lyrics (F15, single only) ───────────────────
        {
            let state_rc      = state.clone();
            let id_ref        = ctx_canonical_idx.clone();
            let et            = editing_tracks.clone();
            let rebuild_pl    = rebuild_playlist.clone();
            let action        = gio::SimpleAction::new("lyrics", None);
            action.connect_activate(move |_, _| {
                let c = id_ref.get();
                if c < 0 { return }
                let t = et.borrow().get(c as usize).map(|t| {
                    (
                        std::path::PathBuf::from(&t.path),
                        t.artist.clone().unwrap_or_default(),
                        t.title.clone().unwrap_or_default(),
                        t.album_artist.clone().unwrap_or_default(),
                    )
                });
                let Some((path, artist, title, album_artist)) = t else { return };
                view_or_search_lyrics(&state_rc, &path, &artist, &title, &album_artist, rebuild_pl.clone(), LyricsMode::Specific);
            });
            action_group.add_action(&action);
        }

        // ─── Remove from Playlist (mutate editing_tracks + persist) ──
        // Removes selected rows from the canonical play order and
        // immediately rewrites the on-disk M3U8.  Does NOT delete the
        // track from the media library — the user's library DB is
        // untouched.
        {
            let et       = editing_tracks.clone();
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
                // No write here — see the same note in playlists.rs. Removing
                // a row is an edit like any other: it lands on disk when the
                // user presses Save (2026-08-10).
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
            let app_action_names = ["append", "replace", "edit-id3", "lyrics",
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
}
