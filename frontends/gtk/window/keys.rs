use super::*;

/// The main window's keyval dispatcher.
///
/// One `match` over every bound key, returned as an `Rc` so the five
/// controllers that need it — main window, playlist window, shortcuts
/// window, jump window, art window — and the lyrics window (through
/// `AppState::set_transport_key_handler`) can all share one copy of the
/// behaviour instead of each re-deriving it.
///
/// Split out of `player::build` (breakup step 9). The four arguments after
/// `ctx` are the Jump window's parts, which are born below the point where
/// [`PlayerCtx`] is assembled and so are passed in rather than bundled.
///
/// Every key this dispatches must also appear in [`shortcut_sections`] —
/// `shortcut_dialog_lists_every_phase6_key` fails the build otherwise.
pub(super) fn build(
    ctx: &PlayerCtx,
    jump_entry: &gtk4::SearchEntry,
    open_jump_mode: &Rc<dyn Fn(bool)>,
    rebuild_jump: &Rc<dyn Fn()>,
    step_volume: &Rc<dyn Fn(f64)>,
) -> Rc<dyn Fn(gdk::Key) -> glib::Propagation> {
    // Aliased under their original names so the moved `match` reads exactly
    // as it did inside `build`.
    let state = ctx.state.clone();
    let show_remaining = ctx.show_remaining.clone();
    let window = ctx.window.clone();
    let playlist_win = ctx.playlist_win.clone();
    let seek_bar = ctx.seek_bar.clone();
    let status_label = ctx.status_label.clone();
    let repeat_icon = ctx.repeat_icon.clone();
    let repeat_label = ctx.repeat_label.clone();
    let btn_repeat = ctx.btn_repeat.clone();
    let btn_shuffle = ctx.btn_shuffle.clone();
    let btn_ml = ctx.btn_ml.clone();
    let btn_eq = ctx.btn_eq.clone();
    let btn_info = ctx.btn_info.clone();
    let btn_add_files = ctx.btn_add_files.clone();
    let btn_add_dir = ctx.btn_add_dir.clone();
    let set_track = ctx.set_track.clone();
    let rebuild_playlist = ctx.rebuild_playlist.clone();
    let patch_pl_row = ctx.patch_pl_row.clone();
    let scroll_to_row_if_needed = ctx.scroll_to_row_if_needed.clone();
    let play_and_update = ctx.play_and_update.clone();
    let refresh_now_playing = ctx.refresh_now_playing.clone();
    let remove_selected = ctx.remove_selected.clone();
    let toggle_np_panel = ctx.toggle_np_panel.clone();
    let open_fullscreen_fn = ctx.open_fullscreen_fn.clone();
    let art_open = ctx.art_open.clone();
    let jump_entry = jump_entry.clone();
    let open_jump_mode = open_jump_mode.clone();
    let rebuild_jump = rebuild_jump.clone();
    let step_volume = step_volume.clone();

    let handle_key: Rc<dyn Fn(gdk::Key) -> glib::Propagation> = {
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let status_label = status_label.clone();
        let kbd_set_track = set_track.clone();
        let kbd_rebuild = rebuild_playlist.clone();
        let kbd_seek_bar = seek_bar.clone();
        let playlist_win_wk = playlist_win.downgrade();
        // Strong reference: keeps the window alive even when hidden, so
        // repeated open/close cycles work without recreating the widget tree.
        let kbd_open_jump = open_jump_mode.clone();
        let window_weak = window.downgrade();
        let remove_sel = remove_selected.clone();
        let kbd_rebuild_jump = rebuild_jump.clone();
        let kbd_jump_entry = jump_entry.clone();
        let kbd_btn_info = btn_info.clone();
        let kbd_btn_eq = btn_eq.clone();
        // Clones for r/s key handlers to update button visuals.
        let kbd_btn_repeat = btn_repeat.clone();
        let kbd_repeat_icon = repeat_icon.clone();
        let kbd_repeat_label = repeat_label.clone();
        let kbd_btn_shuffle = btn_shuffle.clone();
        // Clones for z/b (prev/next) handlers — use patch instead of rebuild
        // so the scroll position is preserved rather than reset to the top.
        let kbd_patch_row = patch_pl_row.clone();
        let kbd_scroll = scroll_to_row_if_needed.clone();
        let kbd_open_fs = open_fullscreen_fn.clone();
        let kbd_art_open = art_open.clone();
        let kbd_toggle_np = toggle_np_panel.clone();
        let kbd_refresh_np = refresh_now_playing.clone();
        let kbd_stop_status = status_label.clone();
        let kbd_btn_ml = btn_ml.clone();
        let kbd_btn_add_files = btn_add_files.clone();
        let kbd_btn_add_dir = btn_add_dir.clone();
        let kbd_step_volume = step_volume.clone();

        Rc::new(move |key: gdk::Key| -> glib::Propagation {
            match key {
                // ── Winamp transport bindings ──────────────────────────────
                gdk::Key::z => {
                    let old_idx = state.borrow().playlist.current_index;
                    let result = { state.borrow_mut().play_prev() };
                    if let Some(d) = result {
                        kbd_set_track(&d);
                        let new_idx = state.borrow().playlist.current_index;
                        if old_idx != new_idx {
                            kbd_patch_row(old_idx);
                        }
                        kbd_patch_row(new_idx);
                        kbd_scroll(new_idx);
                        // Explicit so a Prev-restart (same track) refreshes too.
                        kbd_refresh_np();
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::x => {
                    let ps = state.borrow().player.state().clone();
                    match ps {
                        // (Re)starting a track clears stop-after-current via
                        // play_current; a no-op play while already Playing must
                        // NOT clear it, and pause/resume goes through `c`.
                        PlayerState::Stopped | PlayerState::Paused => play_and_update(),
                        PlayerState::Playing => {}
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::c => {
                    let _ = state.borrow_mut().player.toggle_pause();
                    glib::Propagation::Stop
                }
                gdk::Key::v => {
                    let _ = state.borrow_mut().player.stop();
                    kbd_seek_bar.set_value(0.0);
                    // Manual stop cancels a pending stop-after-current.
                    state.borrow_mut().player.set_stop_after_current(false);
                    glib::Propagation::Stop
                }
                // ── Stop with fadeout (Shift+V) — ramp to silence, then stop.
                // The tick drives the ramp and resets the seek bar at the end. ─
                gdk::Key::V => {
                    let fade = {
                        let mut s = state.borrow_mut();
                        s.player.set_stop_after_current(false);
                        let d = s.config.playback.fadeout_duration();
                        s.player.begin_fadeout(d);
                        s.player.is_fading_out().then_some(d)
                    };
                    if let Some(d) = fade {
                        kbd_stop_status.set_text(&format!("Fading out over {}s…", d.as_secs()));
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::b => {
                    let old_idx = state.borrow().playlist.current_index;
                    let q_before = state.borrow().queue.len();
                    let result = { state.borrow_mut().play_next() };
                    if let Some(d) = result {
                        kbd_set_track(&d);
                        let new_idx = state.borrow().playlist.current_index;
                        // A queued entry was consumed → renumber every badge
                        // (positions shift), not just the two changed rows.
                        if state.borrow().queue.len() != q_before {
                            kbd_rebuild();
                            refresh_queue_manager();
                        } else {
                            if old_idx != new_idx {
                                kbd_patch_row(old_idx);
                            }
                            kbd_patch_row(new_idx);
                        }
                        kbd_scroll(new_idx);
                        kbd_refresh_np();
                    }
                    glib::Propagation::Stop
                }

                // ── Arrow keys: seek ±5 seconds ───────────────────────────
                // GTK fires key-repeat while the key is held, so holding Left
                // or Right continuously rewinds / fast-forwards the track.
                gdk::Key::Left => {
                    state.borrow_mut().seek_delta_secs(-5.0);
                    glib::Propagation::Stop
                }
                gdk::Key::Right => {
                    state.borrow_mut().seek_delta_secs(5.0);
                    glib::Propagation::Stop
                }

                // ── Volume: - decreases, = / + increases ──────────────────
                // GTK fires key-repeat while the key is held, so volume
                // continues to ramp as long as the key is held down.
                gdk::Key::minus => {
                    kbd_step_volume(-0.05);
                    glib::Propagation::Stop
                }
                gdk::Key::equal | gdk::Key::plus => {
                    kbd_step_volume(0.05);
                    glib::Propagation::Stop
                }

                // ── Visualizer mode toggle ─────────────────────────────────
                gdk::Key::a | gdk::Key::A => {
                    state.borrow_mut().toggle_visualizer_mode();
                    glib::Propagation::Stop
                }

                // ── Random Granite effect (e — Granite mode) ───────────────
                gdk::Key::e | gdk::Key::E => {
                    let mut s = state.borrow_mut();
                    if matches!(s.config.visualizer.mode, VisualizerMode::Granite) {
                        if let Some(eff) = s.player.granite_random_effect() {
                            // Record in config so pinned mode (auto-switch
                            // off) follows along instead of snapping back.
                            s.config.visualizer.granite.effect = eff;
                        }
                    }
                    glib::Propagation::Stop
                }

                // ── Visualizer fullscreen (f — Waveform or Granite mode) ──
                gdk::Key::f | gdk::Key::F => {
                    let supports_fs = matches!(
                        state.borrow().config.visualizer.mode,
                        VisualizerMode::Waveform | VisualizerMode::Granite,
                    );
                    if supports_fs {
                        if let Some(ref opener) = *kbd_open_fs.borrow() {
                            opener();
                        }
                    }
                    glib::Propagation::Stop
                }

                // ── Jump window ────────────────────────────────────────────
                gdk::Key::j | gdk::Key::J => {
                    kbd_jump_entry.set_text("");
                    kbd_rebuild_jump();
                    // Open in Jump mode (sets the radio, shows the search pane,
                    // focuses the entry, presents the window).
                    kbd_open_jump(false);
                    glib::Propagation::Stop
                }

                // ── Add file(s) (n) — same multi-select + background scan as
                // the "+ Files" button so the two paths stay identical ────────
                gdk::Key::n => {
                    kbd_btn_add_files.emit_clicked();
                    glib::Propagation::Stop
                }

                // ── Add folder (Shift+N) — same folder picker + background scan
                // as the "+ Folder" button. GTK delivers Shift+n as `N`. ───────
                gdk::Key::N => {
                    kbd_btn_add_dir.emit_clicked();
                    glib::Propagation::Stop
                }

                // ── Playlist window toggle ─────────────────────────────────
                gdk::Key::p | gdk::Key::P => {
                    if let Some(pw) = playlist_win_wk.upgrade() {
                        pw.set_visible(!pw.is_visible());
                    }
                    glib::Propagation::Stop
                }

                // ── Delete: remove all selected playlist rows ──────────────
                gdk::Key::Delete => {
                    remove_sel();
                    glib::Propagation::Stop
                }

                // ── Repeat mode cycle (r) ─────────────────────────────────
                gdk::Key::r | gdk::Key::R => {
                    let new_mode = {
                        let mut s = state.borrow_mut();
                        let m = s.config.playback.repeat_mode.cycle();
                        s.config.playback.repeat_mode = m;
                        m
                    };
                    kbd_repeat_icon.set_icon_name(Some(repeat_btn_icon(new_mode)));
                    kbd_repeat_label.set_text(repeat_btn_text(new_mode));
                    if new_mode == crate::shuffle::RepeatMode::Off {
                        kbd_btn_repeat.remove_css_class("mode-btn-active");
                    } else {
                        kbd_btn_repeat.add_css_class("mode-btn-active");
                    }
                    glib::Propagation::Stop
                }

                // ── Shuffle toggle (s — hidden; only shown in help) ───────
                gdk::Key::s | gdk::Key::S => {
                    let enabled = {
                        let mut s = state.borrow_mut();
                        s.shuffle_state.toggle();
                        s.shuffle_state.reset();
                        let on = s.shuffle_state.enabled;
                        // Mirror to config so the setting survives to the next session.
                        s.config.playback.shuffle_enabled = on;
                        on
                    };
                    if enabled {
                        kbd_btn_shuffle.add_css_class("mode-btn-active");
                    } else {
                        kbd_btn_shuffle.remove_css_class("mode-btn-active");
                    }
                    glib::Propagation::Stop
                }

                // ── Stop after current track (t) — toggle the engine flag and
                // badge the play button. Fires once at the next EOS, then clears. ─
                // h — same toggle as clicking the counter, for anyone who
                // would rather not reach for the mouse. Matches the TUI's key,
                // and macOS binds it too.
                gdk::Key::h | gdk::Key::H => {
                    super::player::toggle_time_mode(&state, &show_remaining);
                    glib::Propagation::Stop
                }

                gdk::Key::t | gdk::Key::T => {
                    let armed = {
                        let mut s = state.borrow_mut();
                        let now = !s.player.stop_after_current();
                        s.player.set_stop_after_current(now);
                        now
                    };
                    kbd_stop_status.set_text(if armed {
                        "Stopping after current track"
                    } else {
                        "Stop-after-current cancelled"
                    });
                    glib::Propagation::Stop
                }

                // ── Media Library window toggle (m) — routed through the ML
                // button so the open/focus logic stays in one place ──────────
                gdk::Key::m | gdk::Key::M => {
                    kbd_btn_ml.emit_clicked();
                    glib::Propagation::Stop
                }

                // ── ID3 tag editor (d) — open for the currently playing track ─
                gdk::Key::d | gdk::Key::D => {
                    let path = state.borrow().playlist.current().map(|t| t.path.clone());
                    if let Some(path) = path {
                        if let Some(w) = window_weak.upgrade() {
                            open_id3_editor_window(
                                Some(&w),
                                path,
                                state.clone(),
                                kbd_rebuild.clone(),
                                None,
                                None,
                            );
                        }
                    } else {
                        status_label.set_text("No track loaded");
                    }
                    glib::Propagation::Stop
                }

                // ── Lyrics (l) — open the window for the current track, in
                // follow-the-track mode, the same as the A1 panel's "Lyrics"
                // button. The Media Library's own controllers bind `l` to the
                // selected row instead; this arm is what the player and
                // playlist windows see.
                gdk::Key::l | gdk::Key::L => {
                    let cur = state.borrow().playlist.current().map(|t| {
                        (
                            t.path.clone(),
                            t.artist.clone(),
                            t.title.clone(),
                            t.album_artist.clone(),
                        )
                    });
                    if let Some((path, artist, title, album_artist)) = cur {
                        view_or_search_lyrics(
                            &state,
                            &path,
                            &artist,
                            &title,
                            &album_artist,
                            kbd_rebuild.clone(),
                            LyricsMode::Current,
                        );
                    } else {
                        status_label.set_text("No track loaded");
                    }
                    glib::Propagation::Stop
                }

                // ── Info / keyboard shortcuts window ──────────────────────
                gdk::Key::i | gdk::Key::I => {
                    kbd_btn_info.activate();
                    glib::Propagation::Stop
                }

                // F1 — the HIG binding for Help. Sparkamp has no help manual,
                // so the shortcuts window is the honest target.
                gdk::Key::F1 => {
                    kbd_btn_info.emit_clicked();
                    glib::Propagation::Stop
                }

                // ── Equalizer toggle (u) — same path as the EQ button so
                // the singleton/active-CSS logic stays in one place ────────
                gdk::Key::u | gdk::Key::U => {
                    kbd_btn_eq.emit_clicked();
                    glib::Propagation::Stop
                }

                // ── Now-playing panel toggle (w) — same path as the mode
                // button so the Stack-swap/viz-resize/persist logic stays
                // in one place ──────────────────────────────────────────
                gdk::Key::w | gdk::Key::W => {
                    kbd_toggle_np();
                    glib::Propagation::Stop
                }

                // ── Album-art window (k) — routed through the deferred
                // `art_open` slot, filled in once `handle_key` (this very
                // closure) is fully built; see the fill site below ────────
                gdk::Key::k | gdk::Key::K => {
                    if let Some(ref opener) = *kbd_art_open.borrow() {
                        opener();
                    }
                    glib::Propagation::Stop
                }

                // ── Open the Jump/Queue window in Queue mode ───────────────
                gdk::Key::q | gdk::Key::Q => {
                    kbd_open_jump(true);
                    glib::Propagation::Stop
                }

                // ── Quit (Esc) ─────────────────────────────────────────────
                // On the main window this quits; child windows install their
                // own Esc handler (hide/close) that fires first, so this arm
                // only ever runs for the main player window.
                gdk::Key::Escape => {
                    let _ = state.borrow().playlist.save_last();
                    if let Some(w) = window_weak.upgrade() {
                        // Closing the main window triggers connect_close_request
                        // which also saves the playlist — belt-and-suspenders.
                        w.close();
                    }
                    glib::Propagation::Stop
                }

                _ => glib::Propagation::Proceed,
            }
        })
    };

    handle_key
}
