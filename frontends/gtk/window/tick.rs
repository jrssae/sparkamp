use super::*;

/// What the tick loop reads that is not already on [`PlayerCtx`]: the
/// now-playing row's labels, the marquee's scroll state, the visualiser's
/// widgets and render height, and the three probe/metadata receivers.
///
/// The receivers are moved, not cloned — `mpsc::Receiver` is not `Clone`,
/// and the tick loop is their only consumer.
pub(super) struct Deps {
    pub(super) time_disp_label: Label,
    pub(super) title_label: Label,
    pub(super) state_label: Label,
    pub(super) state_stop_badge: Label,
    pub(super) show_remaining: Rc<Cell<bool>>,
    pub(super) last_np_key: Rc<RefCell<Option<String>>>,
    pub(super) marquee_chars: Rc<RefCell<Vec<char>>>,
    pub(super) marquee_offset: Rc<Cell<usize>>,
    pub(super) marquee_tick: Rc<Cell<u32>>,
    pub(super) viz: DrawingArea,
    pub(super) viz_stack: Stack,
    pub(super) granite_pic: Picture,
    pub(super) granite_render_h: Rc<Cell<i32>>,
    pub(super) viz_shutting_down: Rc<Cell<bool>>,
    pub(super) fs_viz_open: Rc<Cell<bool>>,
    pub(super) probe_rx: std::sync::mpsc::Receiver<(PathBuf, Duration)>,
    pub(super) broken_rx: std::sync::mpsc::Receiver<PathBuf>,
    pub(super) current_track_meta_rx:
        std::sync::mpsc::Receiver<(PathBuf, String, String, String, String)>,
    pub(super) open_rx: std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>,
    pub(super) row_facts_rx: std::sync::mpsc::Receiver<crate::file_status::RowFacts>,
}

/// Start the 100 ms tick: seek bar and time display, marquee scroll,
/// transport state, the visualiser frame, and draining the duration-probe,
/// broken-file and current-track-metadata channels.
///
/// Split out of `player::build` (breakup step 9b). Twenty-six bindings flow
/// in and none out — the two shutdown flags it reads (`viz_shutting_down`,
/// `fs_viz_open`) stay declared in `build`, because the window's
/// close-request and the fullscreen visualiser set them.
pub(super) fn start(ctx: &PlayerCtx, d: Deps) {
    // Aliased under their original names so the moved body is unchanged.
    let state = ctx.state.clone();
    let seek_bar = ctx.seek_bar.clone();
    let btn_play = ctx.btn_play.clone();
    let set_track = ctx.set_track.clone();
    let rebuild_playlist = ctx.rebuild_playlist.clone();
    let patch_pl_row = ctx.patch_pl_row.clone();
    let scroll_to_row_if_needed = ctx.scroll_to_row_if_needed.clone();
    let play_and_update = ctx.play_and_update.clone();
    let refresh_now_playing = ctx.refresh_now_playing.clone();
    let time_disp_label = d.time_disp_label.clone();
    let title_label = d.title_label.clone();
    let state_label = d.state_label.clone();
    let state_stop_badge = d.state_stop_badge.clone();
    let show_remaining = d.show_remaining.clone();
    let last_np_key = d.last_np_key.clone();
    let marquee_chars = d.marquee_chars.clone();
    let marquee_offset = d.marquee_offset.clone();
    let marquee_tick = d.marquee_tick.clone();
    let viz = d.viz.clone();
    let viz_stack = d.viz_stack.clone();
    let granite_pic = d.granite_pic.clone();
    let granite_render_h = d.granite_render_h.clone();
    let viz_shutting_down = d.viz_shutting_down.clone();
    let fs_viz_open = d.fs_viz_open.clone();
    let probe_rx = d.probe_rx;
    let broken_rx = d.broken_rx;
    let current_track_meta_rx = d.current_track_meta_rx;
    let open_rx = d.open_rx;
    let row_facts_rx = d.row_facts_rx;

    {
        let state = state.clone();
        let time_disp_label = time_disp_label.clone();
        let viz_shutting_down = viz_shutting_down.clone();
        let title_label = title_label.clone();
        let seek_bar = seek_bar.clone();
        let play_update = play_and_update.clone();
        let viz = viz.clone();
        let marquee_chars = marquee_chars.clone();
        let marquee_offset = marquee_offset.clone();
        let marquee_tick = marquee_tick.clone();
        let last_np_key = last_np_key.clone();
        let refresh_now_playing_tick = refresh_now_playing.clone();
        let show_remaining = show_remaining.clone();
        let state_label = state_label.clone();
        let btn_play = btn_play.clone();
        let patch_pl_row = patch_pl_row.clone();
        let current_track_meta_rx = std::cell::RefCell::new(current_track_meta_rx);
        let set_track = set_track.clone();
        let rebuild_playlist_tick = rebuild_playlist.clone();
        let play_update_tick = play_and_update.clone();
        let scroll_tick = scroll_to_row_if_needed.clone();
        // Granite-mode renderer state captured by the tick closure. Weak
        // refs so the timer doesn't keep widgets alive after the main window
        // closes — calling `set_paintable` on a destroyed widget triggers a
        // Gdk-CRITICAL and (on Wayland) a segfault during gsk paint.
        let viz_stack_tick = viz_stack.downgrade();
        let granite_pic_tick = granite_pic.downgrade();
        let granite_render_h_tick = granite_render_h.clone();
        let granite_buf_tick: std::rc::Rc<std::cell::RefCell<Vec<u8>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        // Last mini-Granite render instant → measured dt (in 30 fps frame
        // units) for the dt-aware sim, so timer jitter never changes the
        // plasma's speed.
        let granite_last_tick: std::rc::Rc<std::cell::Cell<Option<std::time::Instant>>> =
            std::rc::Rc::new(std::cell::Cell::new(None));
        // Tick-side handle on the shutdown flag declared above.
        let viz_shut_for_tick = viz_shutting_down.clone();
        let fs_viz_open_tick = fs_viz_open.clone();
        let state_stop_badge_tick = state_stop_badge.clone();
        // Counter for periodic cache saves: fires every 300 ticks = 30 seconds.
        let mut cache_save_countdown = 300u32;

        // 33 ms (~30 fps) so the visualizer (Bars / Waveform / Granite) animates
        // smoothly. Bars/Waveform queue_draw is cheap; Granite renders into a
        // ~640×360 buffer that gets GPU-upscaled by gsk.
        glib::timeout_add_local(Duration::from_millis(33), move || {
            // Shutdown short-circuit. Set in connect_close_request below.
            if viz_shut_for_tick.get() {
                return ControlFlow::Break;
            }
            // 0. Drain probe results from background threads.
            // patch_pl_row is O(1) per call (updates a single TreeView store row).
            // Cap to 50 per tick so we never block the main thread for long when
            // a large library delivers thousands of results at once.
            let is_scanning = state.borrow().playlist_scan.is_some();
            let probe_cap = if is_scanning { 50usize } else { 500usize };
            // Drain into a batch first, then apply in ONE playlist pass —
            // a per-result pass was O(rows × results) and stalled this tick
            // on large playlists.
            let mut probe_batch: std::collections::HashMap<std::path::PathBuf, Duration> =
                std::collections::HashMap::new();
            while probe_batch.len() < probe_cap {
                let Ok((path, dur)) = probe_rx.try_recv() else {
                    break;
                };
                probe_batch.insert(path, dur);
            }
            if !probe_batch.is_empty() {
                // Bind so the RefMut drops before patch_pl_row borrows again.
                let changed = state.borrow_mut().apply_probed_durations(&probe_batch);
                for idx in changed {
                    patch_pl_row(idx);
                }
            }
            // 0a2. Drain the row-finishing pass: existence and writability for
            // every newly added row, plus tags and duration for the ones the
            // media library had never seen. Capped like the probe drain above
            // so a 36k add patches rows steadily instead of in one long stall.
            {
                // Collect first, then apply in one pass — a per-result apply
                // scanned the whole playlist each time, which is what turned a
                // 36k add into a machine-wide lockup.
                let mut batch = Vec::new();
                // Capped like the probe drain: `patch_pl_row` finds its row by
                // position in a GtkListStore, which is not O(1) deep in a large
                // list, so a tick patches a bounded number of rows.
                while batch.len() < 500 {
                    let Ok(facts) = row_facts_rx.try_recv() else {
                        break;
                    };
                    batch.push(facts);
                }
                for idx in playlist_add::apply_facts(&state, &batch) {
                    patch_pl_row(idx);
                }
            }
            // 0b. Drain missing-file notifications; mark those tracks broken.
            // Collected into a set and applied in ONE pass, like the two drains
            // above: a pass per message was O(rows × messages), and stopping at
            // the first match left a second entry for the same file showing as
            // playable after it was gone.
            {
                let mut missing: std::collections::HashSet<std::path::PathBuf> =
                    std::collections::HashSet::new();
                while let Ok(path) = broken_rx.try_recv() {
                    missing.insert(path);
                }
                let changed = state.borrow_mut().mark_broken(&missing);
                for idx in changed {
                    patch_pl_row(idx);
                }
            }

            // 0c. Drain current track metadata scan results.
            // This is separate from the playlist scan (meta_rx) — it handles metadata
            // reads triggered by play_and_update when a track starts without metadata.
            while let Ok((path, title, artist, album_artist, album)) =
                current_track_meta_rx.borrow().try_recv()
            {
                let (updated_idx, is_current) = {
                    let mut s = state.borrow_mut();
                    let mut updated_idx = None;
                    let mut is_current = false;
                    for (idx, track) in s.playlist.tracks.iter_mut().enumerate() {
                        if track.path == path {
                            track.title = title;
                            track.artist = artist;
                            track.album_artist = album_artist;
                            track.album = album;
                            updated_idx = Some(idx);
                            is_current = idx == s.playlist.current_index;
                            break;
                        }
                    }
                    (updated_idx, is_current)
                };
                // Update the marquee with the new "Artist - Title" display name.
                if is_current {
                    let display = state
                        .borrow()
                        .playlist
                        .current()
                        .map(|t| t.display_name())
                        .unwrap_or_default();
                    if !display.is_empty() {
                        set_track(&display);
                    }
                }
                // Patch the row to show the new title/artist.
                if let Some(idx) = updated_idx {
                    patch_pl_row(idx);
                }
            }

            // 0d. Handle files received from "Open with Sparkamp" in the file manager.
            // Each batch respects playlist_add_behavior (append/replace) and
            // autoplay_on_add from config.
            while let Ok(paths) = open_rx.try_recv() {
                if paths.is_empty() {
                    continue;
                }
                use crate::config::PlaylistAddBehavior;
                let behavior = state.borrow().config.behavior.playlist_add_behavior.clone();
                let autoplay = state.borrow().config.behavior.autoplay_on_add;

                if behavior == PlaylistAddBehavior::Replace {
                    let _ = state.borrow_mut().player.stop();
                    {
                        let mut s = state.borrow_mut();
                        s.playlist.tracks.clear();
                        s.queue.clear();
                        s.playlist.current_index = 0;
                        s.last_duration = None;
                        s.pending_seek = None;
                        s.mute_pending = None;
                    }
                }

                // The shared add path: library rows are a data copy, and a
                // file the library has never seen gets a placeholder row now
                // and its tags, duration and markers from the background pass.
                let added = playlist_add::add_paths(&state, &paths);
                if !added.any() {
                    continue;
                }
                let insert_start = added.start;
                rebuild_playlist_tick();

                if autoplay
                    && (behavior == PlaylistAddBehavior::Replace || insert_start == 0)
                {
                    state.borrow_mut().playlist.jump_to(insert_start);
                    play_update_tick();
                    scroll_tick(insert_start);
                }
            }

            // 0b. Advance a stop-with-fadeout ramp (Shift+V). It stops the
            //     player itself at the end of the ramp, so this runs before
            //     the bus poll and leaves the rest of the tick reading the
            //     post-stop state.
            // Only the seek bar is reset here: `status_label` is not one of
            // this closure's captures, and the stop-after-current EOS guard
            // below settles for the same treatment.
            if state.borrow_mut().poll_fadeout() {
                seek_bar.set_value(0.0);
            }

            // 1. Check for end-of-stream or GStreamer error.
            let bus_event = state.borrow_mut().poll_bus();

            // 1b. Apply any pending seek once the pipeline is running.
            //     Covers two cases:
            //       1. Live scrubbing while Playing/Paused.
            //       2. Pressing Play while Stopped with a pending seek: play_current()
            //          mutes audio and starts playing; the seek is applied here on the
            //          first tick that duration becomes available, then volume is restored.
            {
                let should_seek = {
                    let s = state.borrow();
                    s.pending_seek.is_some()
                        && *s.player.state() != PlayerState::Stopped
                        && (s.player.duration().is_some() || s.last_duration.is_some())
                };
                if should_seek {
                    let restore_vol = {
                        let mut s = state.borrow_mut();
                        let rv = s.mute_pending.take();
                        if let Some(fraction) = s.pending_seek.take() {
                            s.seek_fraction(fraction);
                        }
                        rv
                    };
                    if let Some(vol) = restore_vol {
                        state.borrow_mut().player.set_volume(vol);
                    }
                }
            }
            if let Some(event) = bus_event {
                // Stop-after-current (phase 6): consume the flag on a normal EOS
                // and halt instead of advancing. Errors still fall through to the
                // broken-skip advance below (a failed track isn't "the current
                // track finishing"). Manual next/prev never enter this block.
                if matches!(event, BusEvent::Eos)
                    && state.borrow_mut().player.take_stop_after_current()
                {
                    let _ = state.borrow_mut().player.stop();
                    seek_bar.set_value(0.0);
                    return ControlFlow::Continue;
                }
                // Record which track just finished so we can de-highlight it
                // after the advance changes current_index.
                let pre_advance_idx = state.borrow().playlist.current_index;

                // On error, mark the current track broken so it shows a
                // warning indicator and is skipped in future auto-advances.
                if matches!(event, BusEvent::Error) {
                    let mut s = state.borrow_mut();
                    let idx = s.playlist.current_index;
                    if let Some(t) = s.playlist.tracks.get_mut(idx) {
                        t.broken = true;
                    }
                }
                // Advance to the next track. The manual queue wins over
                // shuffle/repeat; otherwise fall back to the shuffle engine,
                // skipping tracks already marked broken.
                let q_before = state.borrow().queue.len();
                let advanced = {
                    let mut s = state.borrow_mut();

                    // Manual queue takes precedence on auto-advance too.
                    if let Some(idx) = s.queue_next_index() {
                        s.playlist.jump_to(idx);
                        true
                    } else {
                        let total = s.playlist.len();
                        let repeat = s.config.playback.repeat_mode;
                        let current = s.playlist.current_index;

                        // Ask the shuffle engine for the next index.
                        let mut found = false;
                        if let Some(mut next_idx) = s.shuffle_state.next_index(current, total, repeat) {
                            // Skip broken tracks (bounded to avoid an infinite loop).
                            for _ in 0..total {
                                if s.playlist
                                    .tracks
                                    .get(next_idx)
                                    .map(|t| t.broken)
                                    .unwrap_or(false)
                                {
                                    s.shuffle_state.record_played(next_idx);
                                    match s.shuffle_state.next_index(next_idx, total, repeat) {
                                        Some(i) => {
                                            next_idx = i;
                                        }
                                        None => break,
                                    }
                                } else {
                                    s.playlist.jump_to(next_idx);
                                    found = true;
                                    break;
                                }
                            }
                        }
                        found
                    }
                };
                if advanced {
                    // play_update (play_and_update) patches the new current track.
                    // We also patch pre_advance_idx because jump_to() already
                    // updated current_index before play_and_update runs, so
                    // play_and_update won't know the finished track is different.
                    play_update();
                    // A queued entry was consumed → renumber every badge;
                    // otherwise just de-highlight the finished row.
                    if state.borrow().queue.len() != q_before {
                        rebuild_playlist_tick();
                        refresh_queue_manager();
                    } else {
                        let new_idx = state.borrow().playlist.current_index;
                        if pre_advance_idx != new_idx {
                            patch_pl_row(pre_advance_idx);
                        }
                    }
                }
            }

            // 2. Update time display and seek bar position.
            let (pos, dur_opt) = {
                let s = state.borrow();
                (s.player.position(), s.player.duration())
            };
            // Cache duration while it is available so seek-bar drags while
            // stopped can still show the correct time (GStreamer reports None
            // from a Null-state pipeline).
            let gst_dur_written = if let Some(dur) = dur_opt {
                let mut s = state.borrow_mut();
                s.last_duration = Some(dur);
                // Write GStreamer-queried duration back to the current track so
                // the playlist can show it even after playback stops.
                let idx = s.playlist.current_index;
                if let Some(track) = s.playlist.tracks.get_mut(idx) {
                    if track.duration.is_none() {
                        let path = track.path.clone();
                        track.duration = Some(dur);
                        s.duration_cache.insert(&path, dur);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if gst_dur_written {
                // Only the current track's duration changed; patch just that row.
                let idx = state.borrow().playlist.current_index;
                patch_pl_row(idx);
            }

            // Record play in media library once the configurable play-count
            // threshold (F11) is crossed — either N seconds or N% of the
            // track length, per config.playback.play_stats. The
            // rebuild_ml_callback borrows state immutably, so it must be
            // called AFTER the mutable borrow is released — extract the Rc
            // first, then drop the borrow, then invoke the callback.
            let ml_rebuild_needed: Option<Rc<dyn Fn()>> = {
                let mut s = state.borrow_mut();
                let pos = pos.unwrap_or(Duration::ZERO);
                // Track length in seconds, None when GStreamer hasn't
                // reported a (non-zero) duration yet.
                let track_len = dur_opt
                    .filter(|d| !d.is_zero())
                    .map(|d| d.as_secs_f64());
                let deadline = crate::play_stats::play_counted_at(
                    track_len,
                    &s.config.playback.play_stats,
                );
                let crossed = deadline
                    .map(|dl| pos.as_secs_f64() >= dl)
                    .unwrap_or(false);
                let path_str = s
                    .playlist
                    .current()
                    .map(|t| t.path.to_string_lossy().into_owned());
                if crossed {
                    if let Some(ref p) = path_str {
                        if s.counted_play_path.as_ref() != Some(p) {
                            if let Some(ref ml) = s.media_lib {
                                let _ = ml.record_play(p);
                                s.counted_play_path = Some(p.clone());
                                s.rebuild_ml_callback.clone()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(rebuild_ml) = ml_rebuild_needed {
                rebuild_ml();
                // Editor mirrors the same DB rows; reload its currently
                // open playlist so the just-recorded play count / last-
                // played timestamp / unread glyph reflect immediately.
                notify_editor_refresh();
            }

            {
                let (player_state, pending) = {
                    let s = state.borrow();
                    (s.player.state().clone(), s.pending_seek)
                };
                let show_rem = show_remaining.get();

                if player_state == PlayerState::Stopped {
                    // When stopped with a pending seek, hold the bar at the
                    // pending position and show its time.  set_value() does
                    // not re-trigger connect_change_value (GTK only emits
                    // change-value for user-initiated changes), so there is
                    // no feedback loop here.
                    if let Some(fraction) = pending {
                        seek_bar.set_value(fraction);
                        // Update the label if duration is known; otherwise
                        // leave whatever connect_change_value last set.
                        if let Some(text) =
                            state.borrow().time_display_for_fraction(fraction, show_rem)
                        {
                            time_disp_label.set_text(&text);
                        }
                    } else {
                        // Truly stopped with no pending seek — reset to zero.
                        seek_bar.set_value(0.0);
                        time_disp_label.set_text(if show_rem { "--:--" } else { "0:00" });
                    }
                } else {
                    // Playing or Paused — show live GStreamer position.
                    let pos = pos.unwrap_or(Duration::ZERO);
                    if show_rem {
                        if let Some(dur) = dur_opt {
                            let rem = dur.saturating_sub(pos);
                            let rs = rem.as_secs();
                            time_disp_label.set_text(&format!("-{}:{:02}", rs / 60, rs % 60));
                        } else {
                            time_disp_label.set_text("--:--");
                        }
                    } else {
                        let ps = pos.as_secs();
                        time_disp_label.set_text(&format!("{}:{:02}", ps / 60, ps % 60));
                    }
                    if let Some(dur) = dur_opt {
                        if dur.as_nanos() > 0 {
                            seek_bar.set_value(pos.as_nanos() as f64 / dur.as_nanos() as f64);
                        }
                    }
                }
            }

            // 3. Marquee / scrolling title.
            // Display a sliding window into the full "Title — Artist" text.
            // The window width is estimated from the label's allocated pixel
            // width divided by 8 (conservative px-per-char for the 13 px font).
            {
                let chars = marquee_chars.borrow();
                // Fallback to 30 chars before the label is laid out (width = 0).
                let label_w = title_label.allocated_width();
                let display_cols = if label_w > 0 {
                    (label_w / 8).max(10) as usize
                } else {
                    30
                };

                if chars.len() <= display_cols {
                    // Short enough to fit without scrolling.
                    title_label.set_text(&chars.iter().collect::<String>());
                    marquee_offset.set(0);
                } else {
                    // Advance offset every 3 ticks (≈ 300 ms, ~3 chars/second).
                    let tick = marquee_tick.get() + 1;
                    marquee_tick.set(tick);
                    if tick % 3 == 0 {
                        // 5-space visual gap between repetitions.
                        let cycle = chars.len() + 5;
                        marquee_offset.set((marquee_offset.get() + 1) % cycle);
                    }

                    let offset = marquee_offset.get();
                    // Pad with spaces so wrap-around reads cleanly.
                    let gap: Vec<char> = "     ".chars().collect();
                    let looped: Vec<char> = chars.iter().chain(gap.iter()).cloned().collect();
                    let loop_len = looped.len();
                    let visible: String = (0..display_cols)
                        .map(|i| *looped.get((offset + i) % loop_len).unwrap_or(&' '))
                        .collect();
                    title_label.set_text(&visible);
                }
            }

            // 3b. Now-playing fan-out choke point. The marquee above already
            // re-reads `playlist.current()` every tick to stay in sync no
            // matter which path changed the current track; do the same for
            // the A1 panel / A6 art window instead of relying on each play
            // path to call `refresh_now_playing()` explicitly (the ~17 Media
            // Library / device play_current() call sites never did, leaving
            // art stale). Read the current path under a short borrow, drop
            // it, then compare — never hold `state.borrow()` across the
            // `refresh_now_playing()` call, which takes its own borrow.
            {
                let current_key = state
                    .borrow()
                    .playlist
                    .current()
                    .map(|t| t.path.to_string_lossy().into_owned());
                let changed = *last_np_key.borrow() != current_key;
                if changed {
                    *last_np_key.borrow_mut() = current_key;
                    refresh_now_playing_tick();
                }
            }

            // 4. State icon (left of time display) + dynamic play-button accent.
            //    The play button gains the `.transport-play` skin accent while
            //    the engine is Playing or Paused, and loses it when Stopped.
            {
                let s = state.borrow();
                let icon = match s.player.state() {
                    PlayerState::Playing => "▶",
                    PlayerState::Paused => "⏸",
                    PlayerState::Stopped => "⏹",
                };
                state_label.set_text(icon);
                // Stop-after-current (phase 6): small stop-square badge on the
                // corner of the state indicator while armed — survives pause/
                // resume, cleared only by next/prev/jump/replay/stop.
                state_stop_badge_tick.set_visible(s.player.stop_after_current());
                match s.player.state() {
                    PlayerState::Playing | PlayerState::Paused => {
                        if !btn_play.has_css_class("transport-play") {
                            btn_play.add_css_class("transport-play");
                        }
                    }
                    PlayerState::Stopped => {
                        btn_play.remove_css_class("transport-play");
                    }
                }
            }

            // 5. Trigger a Cairo repaint of the visualizer (Bars / Waveform).
            // Granite renders into a Picture instead — see step 5b below.
            viz.queue_draw();

            // 5b. Granite plasma path. Cheap when not the active mode (the
            // match is the only cost). When active, render into the persistent
            // RGBA buffer and hand it to the GTK renderer as a MemoryTexture
            // — gsk uploads to the GPU once per frame and bilinear-upscales
            // for free in the compositor.
            {
                // Upgrade weak refs first; if the main window has closed,
                // both widgets are gone — break the timer instead of touching
                // freed Gdk surfaces.
                let (Some(stack), Some(pic)) = (
                    viz_stack_tick.upgrade(),
                    granite_pic_tick.upgrade(),
                ) else {
                    return ControlFlow::Break;
                };

                // If the widget has no root (no GtkWindow ancestor), the
                // surface is being torn down. Skip set_paintable to avoid a
                // gsk paint on a freed Gdk surface.
                if pic.root().is_none() {
                    return ControlFlow::Break;
                }

                let mode = state.borrow().config.visualizer.mode.clone();
                if mode == VisualizerMode::Granite {
                    if stack.visible_child_name().as_deref() != Some("granite") {
                        stack.set_visible_child_name("granite");
                    }
                    // Single-driver rule: yield while the fullscreen window
                    // owns the renderer (the mini keeps its last texture).
                    if !fs_viz_open_tick.get() {
                        // Aspect-matched internal width: viewport-aspect × fixed
                        // 360 short axis. Fall back to 16:9 when the widget hasn't
                        // been allocated yet.
                        let viewport_w = pic.width().max(1) as f64;
                        let viewport_h = pic.height().max(1) as f64;
                        let aspect = (viewport_w / viewport_h).max(0.5).min(4.0);
                        // Fixed per-mode render height (see GRANITE_RENDER_*),
                        // NOT the live allocation — content_fit=Fill upscales it
                        // to fill the row. Using the allocation here loops the
                        // Picture's intrinsic size and grows it unbounded.
                        let h: u32 = granite_render_h_tick.get().max(1) as u32;
                        let w: u32 = (h as f64 * aspect).round() as u32;
                        let mut buf = granite_buf_tick.borrow_mut();
                        let need = (w as usize) * (h as usize) * 4;
                        if buf.len() != need {
                            buf.resize(need, 0);
                        }
                        let cfg = state.borrow().config.visualizer.granite;
                        let now = std::time::Instant::now();
                        let dt_frames = granite_last_tick
                            .replace(Some(now))
                            .map(|prev| now.duration_since(prev).as_secs_f32() * 30.0)
                            .unwrap_or(1.0);
                        state
                            .borrow_mut()
                            .player
                            .render_granite(&mut buf, w, h, &cfg, dt_frames);
                        let bytes = glib::Bytes::from(&buf[..]);
                        let texture = gdk::MemoryTexture::new(
                            w as i32,
                            h as i32,
                            gdk::MemoryFormat::R8g8b8a8,
                            &bytes,
                            (w * 4) as usize,
                        );
                        pic.set_paintable(Some(&texture));
                    }
                } else if stack.visible_child_name().as_deref() != Some("cairo") {
                    stack.set_visible_child_name("cairo");
                }
            }

            // 6. Periodically flush the duration cache and config to disk (every 30 s).
            // Saving config here ensures settings survive force-kills.
            cache_save_countdown -= 1;
            if cache_save_countdown == 0 {
                cache_save_countdown = 300;
                state.borrow_mut().duration_cache.save_if_dirty();
                let _ = state.borrow().config.save();
            }

            ControlFlow::Continue
        });
    }

    // ══════════════════════════════════════════════════════════════════════════
}
