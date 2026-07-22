//! Manual play queue FFI (phase 5). Mirrors the GTK/TUI queue for the macOS
//! frontend: toggle membership by playlist index, read the `[n]` badge, and
//! drive the Queue view (reorder / remove / clear / randomize / play-now). The
//! queue lives in `ctx.queue`; the advance seam (`ffi/playback`) already drains
//! it ahead of shuffle/linear.
#![allow(unsafe_op_in_unsafe_fn)]

use std::os::raw::c_int;

use super::SparkampCtx;

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
        ctx.playlist.jump_to(idx);
        let uri = ctx.playlist.current().map(|t| t.uri()).unwrap_or_default();
        let _ = ctx.player.load(&uri);
        let _ = ctx.player.play();
    }
}
