//! Manual play queue FFI (phase 5). Mirrors the GTK/TUI queue for the macOS
//! frontend: toggle membership by playlist index, read the `[n]` badge, and
//! drive the Queue view (reorder / remove / clear / randomize / play-now). The
//! queue lives in `ctx.queue`; the advance seam (`ffi/playback`) already drains
//! it ahead of shuffle/linear.
#![allow(unsafe_op_in_unsafe_fn)]

use std::os::raw::c_int;

use super::SparkampCtx;

/// Reconcile the queue with the playlist: stamp a queue id onto any entry that
/// still holds the id-0 sentinel (bulk paths push straight into `tracks`), then
/// drop queued ids whose entries are gone.
///
/// Every playlist mutation that can *remove* an entry must call this, otherwise
/// the queue keeps ids nothing resolves to: the count stays inflated and the
/// Queue view shows fewer rows than it claims. Reorder needs no call — ids ride
/// along with their entries. This is the FFI twin of
/// `Controller::sync_queue_to_playlist`, which GTK and the TUI use; the FFI
/// mutates `ctx.playlist` directly without building a `Controller`, so it needs
/// its own seam.
pub(super) fn sync_queue_to_playlist(ctx: &mut SparkampCtx) {
    ctx.playlist.ensure_ids();
    let live: std::collections::HashSet<u64> =
        ctx.playlist.tracks.iter().map(|t| t.id).collect();
    ctx.queue.retain_ids(&live);
}

/// Toggle the queue membership of the track at playlist `index` (enqueue if
/// absent, dequeue if present). No-op on a null ctx or out-of-range index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_queue_toggle(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() || index < 0 {
        return;
    }
    let ctx = &mut *ctx;
    ctx.playlist.ensure_ids();
    if let Some(id) = ctx.playlist.tracks.get(index as usize).map(|t| t.id) {
        ctx.queue.toggle(id);
    }
}

/// 1-based queue position of the track at playlist `index`, or -1 if it is not
/// queued. The frontend uses this to render the `[n]` badge per playlist row.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_queue_position(ctx: *const SparkampCtx, index: c_int) -> c_int {
    if ctx.is_null() || index < 0 {
        return -1;
    }
    let ctx = &*ctx;
    match ctx.playlist.tracks.get(index as usize) {
        Some(t) => ctx
            .queue
            .position_of(t.id)
            .map(|p| (p + 1) as c_int)
            .unwrap_or(-1),
        None => -1,
    }
}

/// Number of entries currently in the queue.
///
/// Not called by any frontend today — the Swift side reads the queue through
/// `sparkamp_queue_position` per row. Kept because this file's tests use it
/// and `sparkamp_queue_entry_index` below as the read half for asserting on
/// `queue_toggle`, `queue_move` and `queue_clear`, all of which are live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_queue_count(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    (*ctx).queue.len() as c_int
}

/// Playlist index of the queued entry at queue position `queue_pos` (0-based),
/// or -1. Lets the Queue view resolve queue order → playlist rows for display.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_queue_entry_index(
    ctx: *const SparkampCtx,
    queue_pos: c_int,
) -> c_int {
    if ctx.is_null() || queue_pos < 0 {
        return -1;
    }
    let ctx = &*ctx;
    let Some(&id) = ctx.queue.ids().get(queue_pos as usize) else {
        return -1;
    };
    ctx.playlist
        .tracks
        .iter()
        .position(|t| t.id == id)
        .map(|i| i as c_int)
        .unwrap_or(-1)
}

/// Empty the queue.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_queue_clear(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).queue.clear();
}

/// Randomize the queue order (membership unchanged).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_queue_shuffle(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).queue.shuffle();
}

/// Move the queued entry at `queue_pos` by `delta`: negative = up, positive =
/// down. No-op at the ends.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_queue_move(
    ctx: *mut SparkampCtx,
    queue_pos: c_int,
    delta: c_int,
) {
    if ctx.is_null() || queue_pos < 0 {
        return;
    }
    let ctx = &mut *ctx;
    let pos = queue_pos as usize;
    if delta < 0 {
        ctx.queue.move_up(pos);
    } else if delta > 0 {
        ctx.queue.move_down(pos);
    }
}

/// Play the queued entry at `queue_pos` now: dequeue it, jump to its playlist
/// position, and start playback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_queue_play_now(ctx: *mut SparkampCtx, queue_pos: c_int) {
    if ctx.is_null() || queue_pos < 0 {
        return;
    }
    let ctx = &mut *ctx;
    let Some(&id) = ctx.queue.ids().get(queue_pos as usize) else {
        return;
    };
    ctx.queue.dequeue(id);
    if let Some(idx) = ctx.playlist.tracks.iter().position(|t| t.id == id) {
        // Same reset `sparkamp_playlist_jump` does: the cached duration belongs
        // to the outgoing track and would otherwise size the seek bar wrong
        // until the new pipeline reports its own.
        ctx.last_known_duration = None;
        ctx.playlist.jump_to(idx);
        let uri = ctx.playlist.current().map(|t| t.uri()).unwrap_or_default();
        super::prime_rg_for_current(ctx);
        let _ = ctx.player.load(&uri);
        let _ = ctx.player.play();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn track(i: usize) -> Track {
        Track {
            path: std::path::PathBuf::from(format!("/fake/{i}.mp3")),
            title: format!("T{i}"),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only: false,
            id: 0,
        }
    }

    /// A ctx with `n` stamped playlist entries and an empty queue.
    fn ctx_with_tracks(n: usize) -> SparkampCtx {
        gstreamer::init().expect("GStreamer must be available for tests");
        let mut playlist = crate::model::Playlist::new();
        for i in 0..n {
            playlist.add(track(i));
        }
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (duration_tx, duration_rx) = std::sync::mpsc::channel();
        SparkampCtx {
            player: crate::engine::Player::new().expect("Player::new"),
            playlist,
            config: crate::config::Config::default(),
            shuffle_state: crate::shuffle::ShuffleState::new(),
            queue: crate::queue::Queue::new(),
            meta_tx,
            meta_rx,
            duration_tx,
            duration_rx,
            dirty_count: 0,
            last_known_duration: None,
            pending_seek: None,
            eos_cb: None,
            eos_userdata: std::ptr::null_mut(),
            error_cb: None,
            error_userdata: std::ptr::null_mut(),
            position_cb: None,
            position_userdata: std::ptr::null_mut(),
            media_library: None,
            ml_progress: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            ml_scanning: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ml_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rg_progress: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rg_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rg_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            watch: None,
            watch_rx: None,
        }
    }

    /// Toggling by playlist index must round-trip through the badge accessor,
    /// and the badge is 1-based (0 would be indistinguishable from "position 0"
    /// on the C side, which uses -1 for "not queued").
    #[test]
    fn toggle_and_position_round_trip() {
        let mut ctx = ctx_with_tracks(3);
        let p = &mut ctx as *mut SparkampCtx;
        unsafe {
            assert_eq!(sparkamp_queue_position(p, 1), -1);
            sparkamp_queue_toggle(p, 2);
            sparkamp_queue_toggle(p, 0);
            assert_eq!(sparkamp_queue_position(p, 2), 1);
            assert_eq!(sparkamp_queue_position(p, 0), 2);
            assert_eq!(sparkamp_queue_count(p), 2);
            // entry_index maps queue order back to playlist rows.
            assert_eq!(sparkamp_queue_entry_index(p, 0), 2);
            assert_eq!(sparkamp_queue_entry_index(p, 1), 0);
            sparkamp_queue_toggle(p, 2);
            assert_eq!(sparkamp_queue_position(p, 2), -1);
            assert_eq!(sparkamp_queue_position(p, 0), 1, "survivors renumber");
        }
    }

    /// Removing a queued row must drop it from the queue, not leave an id that
    /// nothing resolves to — otherwise the Queue view's count outruns its rows.
    #[test]
    fn removing_a_queued_row_prunes_the_queue() {
        let mut ctx = ctx_with_tracks(3);
        let p = &mut ctx as *mut SparkampCtx;
        unsafe {
            sparkamp_queue_toggle(p, 0);
            sparkamp_queue_toggle(p, 1);
            assert_eq!(sparkamp_queue_count(p), 2);
            crate::ffi::playlist::sparkamp_playlist_remove(p, 1);
            assert_eq!(sparkamp_queue_count(p), 1, "removed row leaves the queue");
            // The survivor kept its identity through the index shift.
            assert_eq!(sparkamp_queue_position(p, 0), 1);
        }
    }

    /// Clearing the playlist empties the queue with it.
    #[test]
    fn clearing_the_playlist_empties_the_queue() {
        let mut ctx = ctx_with_tracks(3);
        let p = &mut ctx as *mut SparkampCtx;
        unsafe {
            sparkamp_queue_toggle(p, 0);
            sparkamp_queue_toggle(p, 2);
            crate::ffi::playlist::sparkamp_playlist_clear(p);
            assert_eq!(sparkamp_queue_count(p), 0);
        }
    }

    /// Reorder keeps badges pointing at the same tracks — the whole reason the
    /// queue is keyed on `Track.id` rather than on the playlist index.
    #[test]
    fn reorder_moves_the_badge_with_its_track() {
        let mut ctx = ctx_with_tracks(3);
        let p = &mut ctx as *mut SparkampCtx;
        unsafe {
            sparkamp_queue_toggle(p, 0);
            assert_eq!(sparkamp_queue_position(p, 0), 1);
            // Drag row 0 to the end.
            crate::ffi::playlist::sparkamp_playlist_move(p, 0, 2);
            assert_eq!(sparkamp_queue_position(p, 0), -1);
            assert_eq!(sparkamp_queue_position(p, 2), 1, "badge followed the track");
        }
    }

    /// Entries pushed straight into `tracks` (the Media Library / dedupe bulk
    /// paths) keep the id-0 sentinel until something stamps them. Two such
    /// entries must not collapse into one queue identity.
    #[test]
    fn bulk_pushed_entries_get_distinct_ids_before_queueing() {
        let mut ctx = ctx_with_tracks(0);
        ctx.playlist.tracks.push(track(0));
        ctx.playlist.tracks.push(track(1));
        let p = &mut ctx as *mut SparkampCtx;
        unsafe {
            sparkamp_queue_toggle(p, 0);
            assert_eq!(sparkamp_queue_position(p, 0), 1);
            assert_eq!(
                sparkamp_queue_position(p, 1),
                -1,
                "the second unstamped entry must not share the first's identity"
            );
        }
    }

    #[test]
    fn move_reorders_the_queue_and_no_ops_at_the_ends() {
        let mut ctx = ctx_with_tracks(3);
        let p = &mut ctx as *mut SparkampCtx;
        unsafe {
            for i in 0..3 {
                sparkamp_queue_toggle(p, i);
            }
            sparkamp_queue_move(p, 0, -1); // already at the top
            assert_eq!(sparkamp_queue_entry_index(p, 0), 0);
            sparkamp_queue_move(p, 2, 1); // already at the bottom
            assert_eq!(sparkamp_queue_entry_index(p, 2), 2);
            sparkamp_queue_move(p, 2, -1);
            assert_eq!(sparkamp_queue_entry_index(p, 1), 2);
            assert_eq!(sparkamp_queue_entry_index(p, 2), 1);
        }
    }
}
