/// All mutable runtime state backing the GTK4 window.
///
/// This struct is the single source of truth for the player, playlist, and
/// configuration.  It intentionally contains no GTK widget references;
/// those live in the surrounding closures.  This separation makes the core
/// logic independently testable without a display server.
struct AppState {
    player: Player,
    playlist: Playlist,
    config: Config,
    /// Session-only shuffle and playback-history state.
    /// Not persisted — reset on each launch.
    shuffle_state: ShuffleState,
    /// Seek fraction [0, 1] to apply on the first tick after the pipeline starts
    /// playing.  Set when the user scrubs the seek bar while the player is
    /// Stopped (pipeline not loaded), so the desired position is remembered and
    /// applied once GStreamer has a duration to seek against.
    pending_seek: Option<f64>,
    /// The most recently observed track duration.  Updated every tick while
    /// playing or paused.  Kept after stop so that seek-bar drags in the
    /// Stopped state (where GStreamer cannot report duration) can still
    /// compute and display the correct time offset.
    last_duration: Option<Duration>,
    /// When `Some(vol)`, the player was muted before play to hide the brief
    /// audio from position 0 while GStreamer starts.  The tick loop restores
    /// this volume after the pending seek is applied.
    mute_pending: Option<f64>,
    /// On-disk cache of audio file durations, keyed by canonical path.
    /// Populated by background probes and saved periodically to
    /// `~/.cache/gnomamp/duration_cache.toml`.
    duration_cache: DurationCache,
    /// Media library — open on startup, or `None` when the DB cannot be opened.
    media_lib: Option<crate::media_library::MediaLibrary>,
    /// The media library browser window, if one is currently open.
    ml_window: Option<gtk4::Window>,
    /// The ID3 tag editor window, if one is currently open.
    id3_editor_window: Option<gtk4::Window>,
    /// Callback to refresh the media library window, registered by the window itself.
    rebuild_ml_callback: Option<Rc<dyn Fn()>>,
    /// Callback that re-polls the ML window's disc drives, registered by the
    /// ML window — the audio-CD insertion watcher uses it so navigation
    /// doesn't wait for the window's own 10 s poll.
    disc_refresh_callback: Option<Rc<dyn Fn()>>,
    /// Drive id the ML window should navigate to after its next disc
    /// refresh. Set by the insertion watcher (auto-open setting); consumed
    /// once the refresh has built that drive's sidebar row.
    pending_disc_nav: Option<String>,
    /// True while a rip holds the optical drive. EVERY poller must stay
    /// completely off the device then — even the "harmless" status ioctls
    /// interleave SCSI commands with the streaming reads and make flaky
    /// drives fault mid-read (verified live: one CDROM_DRIVE_STATUS during
    /// cdda streaming killed the stream).
    disc_reading: std::cell::Cell<bool>,
    /// Callback to update ML scan UI in all windows, registered by each window.
    ml_scan_ui_callback: Option<Rc<dyn Fn()>>,
    /// Callback to rebuild the playlist widget, set during build().
    rebuild_pl_callback: Option<Rc<dyn Fn()>>,
    /// Callback that plays the current track and updates all UI labels, set during build().
    play_and_update_callback: Option<Rc<dyn Fn()>>,
    /// Callback that updates the marquee with a new display string, set during build().
    set_track_callback: Option<Rc<dyn Fn(&str)>>,
    /// Number of background operations (rescan, add folder, etc.) currently in flight.
    /// Used to force-exit the main loop if the user closes the main window while
    /// a background operation is still running.
    pending_bg_ops: std::cell::Cell<usize>,
    /// Path whose play has already been recorded in the media library this session.
    /// Reset to `None` when a new track starts playing so the same track can be
    /// counted again after a user-initiated restart.
    counted_play_path: Option<String>,
    /// Scan state for media library operations.
    ml_scan: Option<ScanState>,
    /// Scan state for playlist operations.
    playlist_scan: Option<ScanState>,
}

/// State for tracking background scan operations.
#[derive(Clone)]
#[allow(dead_code)]
struct ScanState {
    /// Type of scan operation.
    scan_type: ScanType,
    /// Number of files processed so far.
    current: usize,
    /// Total number of files to process.
    total: usize,
    /// Flag to signal cancellation.
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Type of scan operation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ScanType {
    AddFolder,
    AddFiles,
    Rescan,
}

/// Shared helper: start an ML scan with the given scan type and total count.
fn start_ml_scan(
    state: &Rc<RefCell<AppState>>,
    scan_type: ScanType,
    total: usize,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_clone = cancel_flag.clone();
    {
        let mut s = state.borrow_mut();
        s.ml_scan = Some(ScanState {
            scan_type,
            current: 0,
            total,
            cancel: cancel_clone,
        });
        s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
    cancel_flag
}

/// Shared helper: update ML scan progress and notify UI.
fn update_ml_scan_progress(state: &Rc<RefCell<AppState>>, current: usize, total: usize) {
    {
        let mut s = state.borrow_mut();
        if let Some(ref mut scan) = s.ml_scan {
            scan.current = current;
            scan.total = total;
        }
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
}

/// Shared helper: complete an ML scan and notify UI.
fn complete_ml_scan(state: &Rc<RefCell<AppState>>) {
    {
        let mut s = state.borrow_mut();
        s.ml_scan = None;
        s.pending_bg_ops.set(s.pending_bg_ops.get() - 1);
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
}

/// Shared helper: cancel an ML scan and notify UI.
fn cancel_ml_scan(state: &Rc<RefCell<AppState>>) {
    {
        let s = state.borrow_mut();
        if let Some(ref scan) = s.ml_scan {
            scan.cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
}

/// Shared helper: update scan UI elements based on current ml_scan state.
/// Returns true if scanning is in progress.
#[allow(dead_code)]
fn update_scan_ui_elements(
    state: &Rc<RefCell<AppState>>,
    status_label: &gtk4::Label,
    rescan_btn: &gtk4::Button,
    cancel_btn: &gtk4::Button,
) -> bool {
    let scan_state = state.borrow().ml_scan.clone();
    if let Some(scan) = scan_state {
        rescan_btn.set_visible(false);
        cancel_btn.set_visible(true);
        if scan.total > 0 {
            status_label.set_text(&format!("Reading tags {}/{}…", scan.current, scan.total));
        } else {
            status_label.set_text("Reading tags…");
        }
        true
    } else {
        rescan_btn.set_visible(true);
        cancel_btn.set_visible(false);
        status_label.set_text("");
        false
    }
}

impl AppState {
    /// Initialise `AppState` from the given playlist and config.
    ///
    /// Creates a new GStreamer player and immediately applies the configured
    /// volume.  Returns an error if the GStreamer `playbin` element is
    /// unavailable.
    fn new(playlist: Playlist, config: Config) -> Result<Self> {
        let mut player = Player::new()?;
        player.set_volume(config.playback.volume);
        // Apply the saved EQ config so the correct settings are active from
        // the very first track — even before the user opens the EQ window.
        player.apply_eq_bands(&config.equalizer.effective_bands());
        let media_lib = crate::media_library::MediaLibrary::open().ok();

        // Startup cleanup: purge any soft-deleted records from previous sessions
        {
            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            std::thread::spawn(move || {
                if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                    let _ = lib.cleanup_on_startup();
                }
            });
        }

        let shuffle_state = {
            let mut s = ShuffleState::new();
            s.enabled = config.playback.shuffle_enabled;
            s
        };
        Ok(AppState {
            player,
            playlist,
            config,
            shuffle_state,
            pending_seek: None,
            last_duration: None,
            mute_pending: None,
            duration_cache: DurationCache::load(),
            media_lib,
            ml_window: None,
            id3_editor_window: None,
            rebuild_ml_callback: None,
            disc_refresh_callback: None,
            pending_disc_nav: None,
            disc_reading: std::cell::Cell::new(false),
            ml_scan_ui_callback: None,
            rebuild_pl_callback: None,
            play_and_update_callback: None,
            set_track_callback: None,
            pending_bg_ops: std::cell::Cell::new(0),
            counted_play_path: None,
            ml_scan: None,
            playlist_scan: None,
        })
    }

    /// Load and start playback of the track at `playlist.current_index`.
    ///
    /// Returns `Some(display_name)` so the caller can update the marquee, or
    /// `None` if the playlist is empty.  Load / play errors surface on the
    /// next `poll_bus()` call in the tick loop.
    fn play_current(&mut self) -> Option<String> {
        let track = self.playlist.current()?;
        let uri = track.uri();
        let display = track.display_name();
        // Record this track in shuffle history so the previous button can step back.
        let idx = self.playlist.current_index;
        self.shuffle_state.record_played(idx);
        // Reset so the new track can be counted when it plays long enough.
        self.counted_play_path = None;
        let _ = self.player.load(&uri);
        if self.pending_seek.is_some() {
            // HACK: GStreamer's playbin does not expose a duration query while
            // the pipeline is in the Paused state on this system, so we cannot
            // seek-before-play the way e.g. XMMS does (preroll → seek → play).
            // Instead we start playing immediately (so GStreamer decodes audio
            // and a duration becomes available) but mute first so the brief
            // audio from position 0 is inaudible.  The tick loop restores the
            // volume after it successfully applies the pending seek.
            //
            // TODO: Investigate whether a GStreamer pipeline bus watch (rather
            // than polling) could give us a reliable ASYNC_DONE + duration
            // signal that would let us seek silently before play() instead.
            self.mute_pending = Some(self.config.playback.volume);
            self.player.set_volume(0.0);
        }
        let _ = self.player.play();
        Some(display)
    }

    /// Same as `play_current()` but does not record to shuffle history.
    /// Used for back navigation via history to avoid corrupting the history cursor.
    fn play_current_no_record(&mut self) -> Option<String> {
        let track = self.playlist.current()?;
        let uri = track.uri();
        let display = track.display_name();
        // Reset so the new track can be counted when it plays long enough.
        self.counted_play_path = None;
        let _ = self.player.load(&uri);
        if self.pending_seek.is_some() {
            self.mute_pending = Some(self.config.playback.volume);
            self.player.set_volume(0.0);
        }
        let _ = self.player.play();
        Some(display)
    }

    /// Advance to the next track, respecting shuffle and repeat modes.
    ///
    /// Returns `Some(display_name)` if a next track was found, or `None` if
    /// playback should stop (end of playlist with repeat off).
    ///
    /// In shuffle mode, the session history is walked forward first (so
    /// pressing Forward after Back replays the same track) before falling
    /// back to a fresh random pick.  When stopped, fresh picks are still
    /// recorded into shuffle history so a subsequent Back can return to the
    /// original track instead of falling through to linear-prev.
    fn play_next(&mut self) -> Option<String> {
        let total = self.playlist.len();
        let current = self.playlist.current_index;
        let repeat = self.config.playback.repeat_mode;

        // Try walking forward through existing shuffle history first.
        // Seed history with the current track so even a fresh stopped-state
        // session leaves something for Back to step into afterwards.
        if self.shuffle_state.enabled {
            self.shuffle_state.ensure_seeded(current);
            if let Some(idx) = self.shuffle_state.next_from_history() {
                self.playlist.jump_to(idx);
                return if *self.player.state() != PlayerState::Stopped {
                    // History walk — don't re-record (would truncate the
                    // remaining future entries the user might still want).
                    self.play_current_no_record()
                } else {
                    self.playlist.current().map(|t| t.display_name())
                };
            }
        }

        let idx = self.shuffle_state.next_index(current, total, repeat)?;
        self.playlist.jump_to(idx);
        if *self.player.state() != PlayerState::Stopped {
            self.play_current()
        } else {
            // Stopped-state pre-load: record the fresh pick manually so the
            // shuffle history reflects the navigation even though the
            // playback layer never gets a chance to call play_current.
            self.shuffle_state.record_played(idx);
            self.playlist.current().map(|t| t.display_name())
        }
    }

    /// Implement the "back button" behaviour with shuffle history support.
    ///
    /// - ≥ 5 s elapsed → restart the current track from the beginning.
    /// - < 5 s elapsed + shuffle on → step back through session history.
    /// - < 5 s elapsed + shuffle off → linear previous track (wraps with Repeat::Playlist).
    ///
    /// Returns `Some(display_name)` of the track that will now play.
    fn play_prev(&mut self) -> Option<String> {
        let pos = self.player.position().unwrap_or(Duration::ZERO);
        let do_play = *self.player.state() != PlayerState::Stopped;

        if pos.as_secs() >= 5 {
            return if do_play {
                self.play_current()
            } else {
                self.playlist.current().map(|t| t.display_name())
            };
        }

        if self.shuffle_state.enabled {
            // Seed history with the current track if shuffle is on but
            // nothing has been recorded yet — Back after a stopped-state
            // Next must return to the original current track, not a
            // linear-prev surprise.
            self.shuffle_state.ensure_seeded(self.playlist.current_index);
            if let Some(idx) = self.shuffle_state.prev_from_history() {
                self.playlist.jump_to(idx);
                return if do_play {
                    self.play_current_no_record()
                } else {
                    self.playlist.current().map(|t| t.display_name())
                };
            }
        } else {
            if self.playlist.current_index == 0 {
                if self.config.playback.repeat_mode == crate::shuffle::RepeatMode::Playlist {
                    self.playlist.jump_to(self.playlist.len().saturating_sub(1));
                }
            } else {
                self.playlist.previous();
            }
            return if do_play {
                self.play_current()
            } else {
                self.playlist.current().map(|t| t.display_name())
            };
        }

        if self.playlist.current_index == 0 {
            return None;
        }
        self.playlist.previous();
        if do_play {
            self.play_current_no_record()
        } else {
            self.playlist.current().map(|t| t.display_name())
        }
    }

    /// Cycle the visualizer to the next built-in mode.
    ///
    /// Cycle order: Bars → Waveform → Granite → Bars.
    fn toggle_visualizer_mode(&mut self) {
        self.config.visualizer.mode = match self.config.visualizer.mode {
            VisualizerMode::Bars => VisualizerMode::Waveform,
            VisualizerMode::Waveform => VisualizerMode::Granite,
            VisualizerMode::Granite => VisualizerMode::Bars,
        };
    }

    /// Attempt to retry spectrum initialization.
    ///
    /// Returns Ok(()) if retry was initiated, Err if spectrum is not available.
    fn retry_spectrum(&mut self) -> Result<(), &'static str> {
        if !self.player.has_spectrum() {
            return Err("Spectrum element not available");
        }

        // If currently playing, just trigger a pipeline state change to help
        // re-establish links. Don't stop playback.
        let current_state = self.player.state().clone();
        if current_state == PlayerState::Playing {
            // The spectrum element is already in the pipeline; a state nudge
            // can help re-establish links if no data is flowing.
        }

        Ok(())
    }

    /// Seek to a fractional position `[0.0, 1.0]` within the current track.
    ///
    /// Values outside the range are clamped silently.  Does nothing if no
    /// track duration is available yet (e.g. during initial buffering).
    fn seek_fraction(&mut self, fraction: f64) {
        let fraction = fraction.clamp(0.0, 1.0);
        // Use the live GStreamer duration first; fall back to the cached
        // last_duration so seeks work even when the pipeline just started
        // and has not yet reported duration (e.g. right after set_state(Playing)).
        let dur = match self
            .player
            .duration()
            .or(self.last_duration)
            .or_else(|| self.playlist.current().and_then(|t| t.duration))
        {
            Some(d) => d,
            None => return,
        };
        let nanos = (fraction * dur.as_nanos() as f64) as u64;
        let _ = self.player.seek(Duration::from_nanos(nanos));
    }

    /// Seek to `fraction` immediately when playing/paused, or store it in
    /// `pending_seek` when the pipeline is stopped so it can be applied once
    /// GStreamer has a duration to seek against.
    ///
    /// This is the canonical entry point for seek-bar interaction.
    fn seek_fraction_or_pend(&mut self, fraction: f64) {
        let fraction = fraction.clamp(0.0, 1.0);
        if *self.player.state() == PlayerState::Stopped {
            self.pending_seek = Some(fraction);
        } else {
            self.seek_fraction(fraction);
        }
    }

    /// Seek forward (`secs` > 0) or backward (`secs` < 0) by that many
    /// seconds within the current track.
    ///
    /// The new position is clamped to `[0, duration]`.  Does nothing if no
    /// position or duration is available (pipeline not loaded).
    fn seek_delta_secs(&mut self, secs: f64) {
        if let (Some(pos), Some(dur)) = (self.player.position(), self.player.duration()) {
            let new_secs = (pos.as_secs_f64() + secs).clamp(0.0, dur.as_secs_f64());
            let _ = self.player.seek(Duration::from_secs_f64(new_secs));
        }
    }

    /// Pre-populate `Track.duration` for every track in the playlist from the
    /// on-disk duration cache.  Should be called once after startup so that
    /// the seek bar can display correct time immediately for known files.
    ///
    /// Also seeds `last_duration` for the current track so that seek-bar drags
    /// in the initial Stopped state work without waiting for a probe result.
    fn apply_cached_durations(&mut self) {
        for track in &mut self.playlist.tracks {
            if track.duration.is_none() {
                track.duration = self.duration_cache.get(&track.path);
            }
        }
        if *self.player.state() == PlayerState::Stopped {
            if let Some(dur) = self.playlist.current().and_then(|t| t.duration) {
                self.last_duration = Some(dur);
            }
        }
    }

    /// Apply a duration result that arrived from a background probe.
    ///
    /// Updates the matching track in the playlist, persists the value to the
    /// in-memory cache (written to disk on the next save tick), and refreshes
    /// `last_duration` when the player is stopped so seek-bar drags show the
    /// correct time immediately.
    /// Collect paths of tracks added at or after `start` that still lack a
    /// cached duration.  Pass the result straight to `duration_probe::spawn_probes`
    /// to schedule background header reads for newly-added files.
    fn uncached_paths_from(&self, start: usize) -> Vec<std::path::PathBuf> {
        self.playlist.tracks[start..]
            .iter()
            .filter(|t| t.duration.is_none())
            .map(|t| t.path.clone())
            .collect()
    }

    /// Apply a batch of background probe results in ONE playlist pass.
    ///
    /// Results arrive hundreds at a time while a big folder scans; the old
    /// one-result-at-a-time version rescanned the whole playlist per result
    /// (O(rows × results) — ~8.5M path compares per tick on a 17k playlist),
    /// stalling the UI thread exactly when the playlist is busiest. It also
    /// stopped at the first match, so duplicate rows of the same file never
    /// received their duration.
    ///
    /// Returns the indices of every updated row for per-row repaints.
    fn apply_probed_durations(
        &mut self,
        batch: &std::collections::HashMap<std::path::PathBuf, Duration>,
    ) -> Vec<usize> {
        let mut changed = Vec::new();
        for (i, track) in self.playlist.tracks.iter_mut().enumerate() {
            if track.duration.is_none() {
                if let Some(dur) = batch.get(&track.path) {
                    track.duration = Some(*dur);
                    changed.push(i);
                }
            }
        }
        for (path, dur) in batch {
            self.duration_cache.insert(path, *dur);
        }
        // Refresh last_duration so the seek bar shows correct time right away
        // when the player is stopped (GStreamer reports None from a Null pipeline).
        if *self.player.state() == PlayerState::Stopped {
            if let Some(dur) = self.playlist.current().and_then(|t| batch.get(&t.path)) {
                self.last_duration = Some(*dur);
            }
        }
        changed
    }

    /// Format a time display string for the given seek `fraction` [0.0, 1.0].
    ///
    /// Uses the live GStreamer duration when the pipeline is loaded, or falls
    /// back to the cached `last_duration` when the pipeline is Stopped (Null
    /// state) and GStreamer cannot report a duration.
    ///
    /// Returns `None` when no duration is available at all (e.g. on first
    /// launch with no track ever loaded).
    fn time_display_for_fraction(&self, fraction: f64, show_remaining: bool) -> Option<String> {
        let dur = self
            .player
            .duration()
            .or(self.last_duration)
            .or_else(|| self.playlist.current().and_then(|t| t.duration))?;
        let fraction = fraction.clamp(0.0, 1.0);
        let pos_secs = (fraction * dur.as_secs_f64()) as u64;
        if show_remaining {
            let rem_secs = dur.as_secs().saturating_sub(pos_secs);
            Some(format!("-{}:{:02}", rem_secs / 60, rem_secs % 60))
        } else {
            Some(format!("{}:{:02}", pos_secs / 60, pos_secs % 60))
        }
    }

    /// Remove the track at `index` (0-based) from the playlist.
    ///
    /// If the removed track was the one currently playing (or paused),
    /// playback of the new current track begins automatically.  Removing the
    /// merely-highlighted current row while stopped must NOT start music —
    /// the marquee just moves to the new current row.  If the playlist
    /// becomes empty, the player is stopped.
    ///
    /// Returns the string the marquee should show now, or `None` when it
    /// needn't change; `Some("")` means "clear it" (playlist emptied — the
    /// removed song's name must not linger).  Returns `None` immediately for
    /// out-of-bounds indices (playlist is unchanged).
    fn remove_track(&mut self, index: usize) -> Option<String> {
        if index >= self.playlist.tracks.len() {
            return None;
        }
        let was_current = index == self.playlist.current_index;
        let was_playing = !matches!(*self.player.state(), crate::engine::PlayerState::Stopped);
        self.playlist.remove(index);

        if self.playlist.is_empty() {
            let _ = self.player.stop();
            Some(String::new())
        } else if was_current {
            if was_playing {
                self.play_current()
            } else {
                self.playlist.current().map(|t| t.display_name())
            }
        } else {
            None
        }
    }

    /// Add a single audio file from a raw path string.
    ///
    /// Leading and trailing whitespace is trimmed before the path is
    /// resolved.  Returns `Ok(display_name)` on success or `Err(message)`
    /// on failure.  Use [`add_path`] when the input might be a directory.
    fn add_track_from_path(&mut self, raw_path: &str) -> Result<String, String> {
        let path = std::path::Path::new(raw_path.trim());
        match Track::from_path(path) {
            Ok(track) => {
                let name = track.display_name();
                self.playlist.add(track);
                Ok(name)
            }
            Err(e) => Err(format!("Cannot add '{}': {}", raw_path.trim(), e)),
        }
    }

    /// Add audio content from a filesystem path that may be a file **or** a
    /// directory.
    ///
    /// - **File**: added as a single track (delegates to [`add_track_from_path`]).
    /// - **Directory**: scanned recursively; every audio file found is added.
    ///   The scan uses [`Playlist::collect_audio_files`] which already handles
    ///   permission errors gracefully.
    ///
    /// Returns a human-readable summary string suitable for the status bar, or
    /// an error string if the path does not exist / cannot be resolved at all.
    fn add_path(&mut self, path: &std::path::Path) -> Result<String, String> {
        if path.is_dir() {
            // Recursively collect all audio files under the directory.
            let files = Playlist::collect_audio_files(path);
            let total = files.len();
            if total == 0 {
                return Err(format!("No audio files found in '{}'", path.display()));
            }
            let mut added = 0usize;
            for file in files {
                if let Ok(track) = Track::from_path(&file) {
                    self.playlist.add(track);
                    added += 1;
                }
            }
            Ok(format!(
                "Added {} / {} files from '{}'",
                added,
                total,
                path.display()
            ))
        } else {
            // Treat as a single audio file.
            self.add_track_from_path(&path.to_string_lossy())
        }
    }

    /// Poll the GStreamer message bus for end-of-stream or error events.
    ///
    /// Returns `Some(BusEvent)` when the current track ended or failed, or
    /// `None` when nothing noteworthy is pending.  The caller is responsible
    /// for marking broken tracks and advancing the playlist.
    fn poll_bus(&mut self) -> Option<BusEvent> {
        self.player.poll_bus()
    }
}

// ---------------------------------------------------------------------------
// Window construction
// ---------------------------------------------------------------------------

/// Build and present the SparkAmp main window and companion playlist window.
///
/// ## Layout overview
///
/// **Main window** (always visible):
/// ```text
/// [mini viz | title / artist]   ← now-playing row
/// [seek bar                  ]
/// [⏮ ▶ ⏸ ⏹ ⏭  VOL  PL     ]   ← transport + PL toggle
/// [status bar                ]
/// ```
///
/// **Playlist window** (shown/hidden with `p` or the PL button):
/// ```text
/// [Playlist — N tracks              ]
/// [+ File] [+ Files] [+ Folder] [✕ Remove]
/// [scrollable playlist ListBox      ]
/// [status bar                       ]
/// ```
///
/// ## Playlist window positioning / snap
///
/// GTK4 on Wayland does **not** allow applications to control window
/// positions programmatically — the compositor exclusively manages
/// placement.  We use `set_transient_for` to hint to the window manager
/// that the playlist window belongs with the main window; most WMs will
/// group them in the taskbar and may place the playlist near the main
/// window on first display.
///
/// On X11 / XWayland, position control is possible via platform-specific
/// GDK APIs (`gdk_x11_surface_get_xid` + `XMoveWindow`), but doing so
/// requires `unsafe` FFI and is not implemented here to keep the code
/// portable.  The Winamp-style "snap within 10–20 px" behaviour would
/// require that platform path.
///
/// In practice, with `set_transient_for` and a modern WM the windows
/// behave as a logical unit: they share the taskbar and are typically
/// raised/lowered together.
/// Re-export built-in skin CSS from the skin module for use in this file.
use crate::skin::{self, render_gtk_css, SkinVars};

