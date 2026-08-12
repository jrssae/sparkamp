//! Media Library "Albums" page — the Phase 11 A4 gallery, wired into the
//! window's stack.
//!
//! Child module of [`super`] (window.rs), and the first page pulled out of
//! `open_media_library_window` by the breakup in docs/gtk-breakup-plan.md.
//! The grid itself lives in `album_gallery.rs`; this file is the glue that
//! gives it its three callbacks, adds it to the stack, and owns the
//! drill-down's return path.

use gtk4::prelude::*;
use std::rc::Rc;

use super::sidebar::Sidebar;
use super::{build_album_gallery, MlCtx};

/// Build the Albums page and attach it to `ctx.stack` under the name
/// `"albums"`.
///
/// Returns the shared "back to the gallery overview" closure. Two other places
/// call it: the sidebar's row-selected handler, and its row-activated handler
/// for the case where "Albums" is clicked while already selected. It is
/// idempotent, because both of those signals can fire for one click.
pub(super) fn build(ctx: &MlCtx, sb: &Sidebar) {
    // Activating a cell (double-click / Enter) sets `album_filter` and
    // switches straight to the Files page via `stack.set_visible_child_name`
    // + the `rebuild_ml_callback` seam (same one background rebuilds use,
    // see `state.borrow_mut().rebuild_ml_callback`) — deliberately
    // NOT via `sidebar.select_row(&files_row)`, because the "files" branch
    // of the sidebar's `connect_row_selected` clears `album_filter`
    // on entry, which would immediately undo the filter this callback just
    // set. The tradeoff: the sidebar's visual selection stays on "Albums"
    // while the stack shows Files. Acceptable — the Files content is what
    // matters, and the user can click "Files" to explicitly return to the
    // full library (which also updates the highlight).
    let (gallery_page, rebuild_gallery): (gtk4::Widget, Rc<dyn Fn()>) = {
        let on_album_activate: Rc<dyn Fn(String, String)> = {
            let state_activate = ctx.host.state.clone();
            let stack_activate = ctx.stack.clone();
            let album_filter_activate = ctx.album_filter.clone();
            let btn_album_back_activate = ctx.btn_album_back.clone();
            Rc::new(move |album: String, album_artist: String| {
                {
                    *album_filter_activate.borrow_mut() = Some((album, album_artist));
                }
                // Reveal the back-to-gallery button now that we're in an
                // album's track list.
                btn_album_back_activate.set_visible(true);
                stack_activate.set_visible_child_name("files");
                let cb = state_activate.borrow().rebuild_ml_callback.clone();
                if let Some(cb) = cb {
                    cb();
                }
            })
        };
        // Play/Enqueue an album straight from a tile's right-click menu —
        // the same album_tracks → replace/append seam the drill-down's
        // "Play Album" / "Enqueue Album" buttons use.
        let on_album_play: Rc<dyn Fn(String, String)> = {
            let state_p = ctx.host.state.clone();
            let rebuild_pl = ctx.host.rebuild_playlist.clone();
            Rc::new(move |album: String, album_artist: String| {
                let artist_as_album =
                    state_p.borrow().config.media_library.artist_as_album_artist;
                let tracks: Vec<crate::media_library::LibTrack> = state_p
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.album_tracks(&album, &album_artist, artist_as_album).ok())
                    .unwrap_or_default();
                if tracks.is_empty() {
                    return;
                }
                let _ = state_p.borrow_mut().player.stop();
                state_p.borrow_mut().playlist.clear();
                for lt in &tracks {
                    super::playlist_add::add_track(&state_p, crate::model::Track::from(lt), false);
                }
                if !state_p.borrow().playlist.is_empty() {
                    state_p.borrow_mut().play_current();
                }
                rebuild_pl();
            })
        };
        let on_album_enqueue: Rc<dyn Fn(String, String)> = {
            let state_e = ctx.host.state.clone();
            let rebuild_pl = ctx.host.rebuild_playlist.clone();
            Rc::new(move |album: String, album_artist: String| {
                let artist_as_album =
                    state_e.borrow().config.media_library.artist_as_album_artist;
                let tracks: Vec<crate::media_library::LibTrack> = state_e
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.album_tracks(&album, &album_artist, artist_as_album).ok())
                    .unwrap_or_default();
                if tracks.is_empty() {
                    return;
                }
                let was_empty = state_e.borrow().playlist.is_empty();
                for lt in &tracks {
                    super::playlist_add::add_track(&state_e, crate::model::Track::from(lt), false);
                }
                if state_e.borrow().config.behavior.autoplay_on_add && was_empty {
                    state_e.borrow_mut().play_current();
                }
                rebuild_pl();
            })
        };
        build_album_gallery(
            &ctx.host.state,
            on_album_activate,
            on_album_play,
            on_album_enqueue,
        )
    };
    ctx.stack.add_named(&gallery_page, Some("albums"));

    // Return from an album's track list to the gallery overview: clear the
    // drill-down filter, hide the back button, show the gallery page and
    // refresh it. Shared by the back button (in the Files search row) and by
    // clicking the "Albums" sidebar row while drilled in.
    let show_gallery_overview: Rc<dyn Fn()> = {
        let album_filter_ov = ctx.album_filter.clone();
        let btn_album_back_ov = ctx.btn_album_back.clone();
        let stack_ov = ctx.stack.clone();
        let rebuild_gallery_ov = rebuild_gallery.clone();
        Rc::new(move || {
            {
                *album_filter_ov.borrow_mut() = None;
            }
            btn_album_back_ov.set_visible(false);
            stack_ov.set_visible_child_name("albums");
            rebuild_gallery_ov();
        })
    };
    {
        let show_ov = show_gallery_overview.clone();
        ctx.btn_album_back.connect_clicked(move |_| {
            show_ov();
        });
    }

    // ── Sidebar routing ─────────────────────────────────────────────────
    // This page's own row-selected handler, split out of the shared
    // Files/Albums/Playlists one on 2026-08-10 so the Playlists page could be
    // extracted without taking the other two pages' navigation with it. Every
    // handler on this signal keys off a disjoint `widget_name` with no
    // catch-all, so registration order carries no meaning.
    {
        let show_ov = show_gallery_overview.clone();
        sb.list.connect_row_selected(move |_, opt_row| {
            let Some(row) = opt_row else { return };
            if row.widget_name() == "albums" {
                // Always land on the gallery overview (clears any drill-down).
                show_ov();
            }
        });
    }
    // Clicking "Albums" while it is ALREADY selected (the user drilled into an
    // album, so the row's highlight never left "Albums") does not re-emit
    // `row-selected`, so that path can't return to the gallery. `row-activated`
    // DOES fire on every click, so handle the return here too. Harmless when
    // arriving from another row — both signals fire and this is idempotent.
    {
        let show_ov = show_gallery_overview.clone();
        sb.list.connect_row_activated(move |_, row| {
            if row.widget_name() == "albums" {
                show_ov();
            }
        });
    }
}
