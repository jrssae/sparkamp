//! Media Library "Albums" page — the Phase 11 A4 gallery, wired into the
//! window's stack.
//!
//! Child module of [`super`] (window.rs), and the first page pulled out of
//! `open_media_library_window` by the breakup in docs/gtk-breakup-plan.md.
//! The grid itself lives in `album_gallery.rs`; this file is the glue that
//! gives it its three callbacks, adds it to the stack, and owns the
//! drill-down's return path.

use gtk4::glib;
use gtk4::prelude::*;
use std::cell::Cell;
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

    // The gallery's cache is deliberately not invalidated through this
    // file's callbacks. `rebuild_ml_callback` (used above, for the
    // drill-down's Files refresh) is the wrong seam for that: it misses
    // `purge_deleted_tracks()` (files.rs/files_menu.rs remove-from-library,
    // which runs on its own background-thread `Connection` and never
    // touches this callback), and it *over*-fires on every album drill-down
    // (`on_album_activate` above calls it to refresh the Files view), which
    // would mark the cache stale on the exact path the cache exists to make
    // instant. Instead, `album_gallery.rs`'s `rebuild` validates its cache
    // on every call against `MediaLibrary::change_token()` plus the sort
    // selection and the `artist_as_album_artist` config flag (the fold's
    // three inputs) — an O(1) check that can't miss a write, or a config
    // toggle, the way a hand-chained callback can.

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

    // The cheap half of `show_gallery_overview`: everything except the
    // rebuild. Split out because the two sidebar signals below both fire for
    // one click and only the rebuild is worth collapsing — deferring the
    // stack switch as well would leave the previous page on screen until the
    // idle callback ran, turning a doubled query into a visible lag.
    let navigate_to_gallery: Rc<dyn Fn()> = {
        let album_filter_nav = ctx.album_filter.clone();
        let btn_album_back_nav = ctx.btn_album_back.clone();
        let stack_nav = ctx.stack.clone();
        Rc::new(move || {
            {
                *album_filter_nav.borrow_mut() = None;
            }
            btn_album_back_nav.set_visible(false);
            stack_nav.set_visible_child_name("albums");
        })
    };

    // ── Sidebar routing ─────────────────────────────────────────────────
    // This page's own row-selected handler, split out of the shared
    // Files/Albums/Playlists one on 2026-08-10 so the Playlists page could be
    // extracted without taking the other two pages' navigation with it. Every
    // handler on this signal keys off a disjoint `widget_name` with no
    // catch-all, so registration order carries no meaning.
    //
    // Both signals are needed and both can fire for one click.
    //
    // `row-selected` is the normal arrival from another sidebar row.
    // `row-activated` is the only one that fires when "Albums" is clicked
    // while already selected — which happens on the way back from a
    // drill-down, since the highlight never left "Albums" (see
    // `on_album_activate` above).
    //
    // When the user arrives from another row they BOTH fire, and
    // `show_gallery_overview` re-queries the whole library and repopulates
    // 5,000-odd tiles.
    //
    // Only the REBUILD is coalesced, not the whole of
    // `show_gallery_overview`. Clearing the filter, hiding the back button and
    // switching the stack are cheap, idempotent, and what makes the click feel
    // instant — deferring those to idle would leave the previous page on
    // screen until the callback ran. So the navigation stays synchronous on
    // every signal and only the expensive half collapses to one call.
    {
        let navigate = navigate_to_gallery.clone();
        let rebuild_coalesced = rebuild_gallery.clone();
        let queued: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let show_once: Rc<dyn Fn()> = Rc::new(move || {
            // Synchronous every time — cheap, idempotent, and what makes the
            // click feel immediate.
            navigate();
            // Coalesced — the second signal of the same click finds the flag
            // already set and books no second rebuild.
            if queued.replace(true) {
                return;
            }
            let rebuild = rebuild_coalesced.clone();
            let queued = queued.clone();
            glib::idle_add_local_once(move || {
                queued.set(false);
                rebuild();
            });
        });

        {
            let show_once = show_once.clone();
            sb.list.connect_row_selected(move |_, opt_row| {
                let Some(row) = opt_row else { return };
                if row.widget_name() == "albums" {
                    show_once();
                }
            });
        }
        {
            let show_once = show_once.clone();
            sb.list.connect_row_activated(move |_, row| {
                if row.widget_name() == "albums" {
                    show_once();
                }
            });
        }
    }
}
