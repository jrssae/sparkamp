use super::*;

/// All mutable runtime state backing the GTK4 window.
///
/// This struct is the single source of truth for the player, playlist, and
/// configuration.  It intentionally contains no GTK widget references;
/// those live in the surrounding closures.  This separation makes the core
/// logic independently testable without a display server.
pub(super) struct AppState {
    pub(super) player: Player,
    pub(super) playlist: Playlist,
    pub(super) config: Config,
    /// Session-only shuffle and playback-history state.
    /// Not persisted — reset on each launch.
    pub(super) shuffle_state: ShuffleState,
    /// Manual play queue (session-only). Drained ahead of shuffle/linear in
    /// `play_next`; keyed on `Track.id`.
    pub(crate) queue: crate::queue::Queue,
    /// Seek fraction [0, 1] to apply on the first tick after the pipeline starts
    /// playing.  Set when the user scrubs the seek bar while the player is
    /// Stopped (pipeline not loaded), so the desired position is remembered and
    /// applied once GStreamer has a duration to seek against.
    pub(super) pending_seek: Option<f64>,
    /// The most recently observed track duration.  Updated every tick while
    /// playing or paused.  Kept after stop so that seek-bar drags in the
    /// Stopped state (where GStreamer cannot report duration) can still
    /// compute and display the correct time offset.
    pub(super) last_duration: Option<Duration>,
    /// When `Some(vol)`, the player was muted before play to hide the brief
    /// audio from position 0 while GStreamer starts.  The tick loop restores
    /// this volume after the pending seek is applied.
    pub(super) mute_pending: Option<f64>,
    /// On-disk cache of audio file durations, keyed by canonical path.
    /// Populated by background probes and saved periodically to
    /// `~/.cache/gnomamp/duration_cache.toml`.
    pub(super) duration_cache: DurationCache,
    /// Media library — open on startup, or `None` when the DB cannot be opened.
    pub(super) media_lib: Option<crate::media_library::MediaLibrary>,
    /// Live filesystem watcher over the watched folders (Phase 8 Task 10).
    /// `None` whenever watching is off (`config.media_library.watch_folders`
    /// false), `media_lib` is unavailable, or the underlying OS watcher
    /// failed to start (e.g. inotify `max_user_watches` exhausted) — the
    /// last case is graceful degradation, never a hard error. Rebuilt via
    /// `watch::rebuild_watcher` whenever folders, per-folder recurse, or the
    /// toggle change.
    pub(super) watch: Option<crate::watch::FolderWatcher>,
    /// Paired with `watch` above — the channel its debounced events arrive
    /// on. Drained by the tick registered once in `watch::start_drain_tick`.
    pub(super) watch_rx: Option<std::sync::mpsc::Receiver<crate::watch::WatchAction>>,
    /// Where the background pass that finishes a newly added playlist row
    /// sends its answers. Set once by `player::build`, and read by every add
    /// site through `playlist_add`.
    ///
    /// It lives here rather than being threaded through because the alternative
    /// was proved not to work: there are 27 places that add to the active
    /// playlist, and when the duration probe had to be passed in, three of them
    /// silently went without it. A field every site can reach is what makes one
    /// shared add path possible.
    ///
    /// `None` outside the GTK window — the FFI and test paths add rows without
    /// a main loop to deliver results to, and simply skip the background work.
    pub(super) row_facts_tx: Option<std::sync::mpsc::Sender<crate::file_status::RowFacts>>,
    /// The media library browser window, if one is currently open.
    pub(super) ml_window: Option<gtk4::Window>,
    /// The settings window, if one is currently open. Singleton (like
    /// `ml_window`): a second open request just `present()`s this one.
    pub(super) settings_window: Option<gtk4::Window>,
    /// The ID3 tag editor window, if one is currently open.
    pub(super) id3_editor_window: Option<gtk4::Window>,
    /// The read-only lyrics viewer window (F15), if one is open. Singleton
    /// like `id3_editor_window`: opening for another track replaces content.
    pub(super) lyrics_window: Option<gtk4::Window>,
    /// Whether the open lyrics window tracks a fixed song or the currently
    /// playing one (F15 revision, point 4). A `Cell` so the now-playing
    /// subscriber can read it without a `borrow_mut` on `AppState`.
    pub(super) lyrics_mode: Rc<std::cell::Cell<LyricsMode>>,
    /// Set while the lyrics window is open in Current mode: re-reads the
    /// current track and refreshes the window's title + body. Called by the
    /// now-playing subscriber on every track change; cleared on window close.
    pub(super) lyrics_refresh: Option<Rc<dyn Fn()>>,
    /// The track path the open lyrics window is currently showing. Drives the
    /// `l`-key toggle: pressing `l` on this same track closes the window, while
    /// `l` on a different track retargets it. Updated on open and (in Current
    /// mode) by the refresh closure; cleared on close. A shared cell so the
    /// refresh closure can update it without a `borrow_mut` on `AppState`.
    pub(super) lyrics_shown_path: Rc<RefCell<Option<std::path::PathBuf>>>,
    /// The main window's key handler, published by `player.rs` once built, so
    /// satellite windows (the lyrics viewer) can forward the Winamp transport
    /// keys (z/x/c/v/b/j/r/s) to it (F15 revision, point 5).
    pub(super) transport_key_handler: Option<Rc<dyn Fn(gtk4::gdk::Key) -> gtk4::glib::Propagation>>,
    /// The A6 standalone album-art window, once built. Unlike `ml_window` /
    /// `id3_editor_window` this is never cleared back to `None` — it is
    /// built once, kept alive for the app's lifetime (hidden, not destroyed,
    /// on close), and reused on every `k` / art-click via `present()` so its
    /// `now_playing` subscription is only ever registered once.
    pub(super) art_window: Option<gtk4::Window>,
    /// Owns the MPRIS D-Bus bus-name + object registration for the app's
    /// lifetime (dropping it would unexport the service). Set once by
    /// `mpris::init`; `#[allow(dead_code)]` — held only to own the lifetime.
    #[allow(dead_code)]
    pub(super) mpris_guard: Option<mpris::MprisGuard>,
    /// Callback to refresh the media library window, registered by the window itself.
    pub(super) rebuild_ml_callback: Option<Rc<dyn Fn()>>,
    /// Callback that re-polls the ML window's disc drives, registered by the
    /// ML window — the audio-CD insertion watcher uses it so navigation
    /// doesn't wait for the window's own 10 s poll.
    pub(super) disc_refresh_callback: Option<Rc<dyn Fn()>>,
    /// Drive id the ML window should navigate to after its next disc
    /// refresh. Set by the insertion watcher (auto-open setting); consumed
    /// once the refresh has built that drive's sidebar row.
    pub(super) pending_disc_nav: Option<String>,
    /// True while a rip holds the optical drive. EVERY poller must stay
    /// completely off the device then — even the "harmless" status ioctls
    /// interleave SCSI commands with the streaming reads and make flaky
    /// drives fault mid-read (verified live: one CDROM_DRIVE_STATUS during
    /// cdda streaming killed the stream).
    pub(super) disc_reading: std::cell::Cell<bool>,
    /// Callback to update ML scan UI in all windows, registered by each window.
    pub(super) ml_scan_ui_callback: Option<Rc<dyn Fn()>>,
    /// Callback to rebuild the playlist widget, set during build().
    pub(super) rebuild_pl_callback: Option<Rc<dyn Fn()>>,
    /// Callback that plays the current track and updates all UI labels, set during build().
    pub(super) play_and_update_callback: Option<Rc<dyn Fn()>>,
    /// Callback that updates the marquee with a new display string, set during build().
    pub(super) set_track_callback: Option<Rc<dyn Fn(&str)>>,
    /// Subscribers notified whenever a new track starts (A1 panel, A6 window,
    /// phase-3 MPRIS). Fan-out only — callers must never hold a `borrow_mut()`
    /// across the notify loop; extract the Vec under a short borrow first.
    pub(super) now_playing_subscribers: Vec<Rc<dyn Fn(&crate::now_playing::NowPlayingInfo)>>,
    /// Now-playing info for the currently loaded track, set at play-start
    /// alongside the `now_playing_subscribers` fan-out (see `play_and_update`
    /// in player.rs). Lets a panel built or shown mid-playback (A1 toggle,
    /// A6 window open) populate immediately via `current_now_playing()`
    /// instead of waiting for the *next* track change, which is the only
    /// thing that fires the subscriber fan-out.
    pub(super) current_now_playing: Option<crate::now_playing::NowPlayingInfo>,
    /// Number of background operations (rescan, add folder, etc.) currently in flight.
    /// Used to force-exit the main loop if the user closes the main window while
    /// a background operation is still running.
    pub(super) pending_bg_ops: std::cell::Cell<usize>,
    /// Path whose play has already been recorded in the media library this session.
    /// Reset to `None` when a new track starts playing so the same track can be
    /// counted again after a user-initiated restart.
    pub(super) counted_play_path: Option<String>,
    /// Scan state for media library operations.
    pub(super) ml_scan: Option<ScanState>,
    /// Scan state for playlist operations.
    pub(super) playlist_scan: Option<ScanState>,
    /// Progress/cancel state for a background ReplayGain analysis job (the
    /// Files-view bulk "Analyze ReplayGain" button, or the per-selection
    /// "Calculate ReplayGain" force action). Kept separate from `ml_scan`
    /// rather than reusing it: the existing `ml_scan` UI pollers hard-code
    /// "Reading tags…" status text, which would be the wrong label while
    /// analysis is running.
    pub(super) rg_job: Option<RgJobState>,
    /// One-shot completion/error message for the last ReplayGain job, set when
    /// the job finishes and consumed (taken) by whichever view's poller
    /// renders it next. Lets the Settings window and the Files view show the
    /// same "Analyzed N track(s)" result without either one writing the shared
    /// status label directly (which raced the progress poller).
    pub(super) rg_ui_msg: Option<String>,
}

/// State for tracking background scan operations.
#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct ScanState {
    /// Type of scan operation.
    pub(super) scan_type: ScanType,
    /// Number of files processed so far.
    pub(super) current: usize,
    /// Total number of files to process.
    pub(super) total: usize,
    /// Flag to signal cancellation.
    pub(super) cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Type of scan operation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum ScanType {
    AddFolder,
    AddFiles,
    Rescan,
}

/// Progress/cancel state for a background ReplayGain analysis job. Shape
/// mirrors `ScanState` (minus `scan_type` — there's only one kind of job
/// here) but lives in its own `AppState.rg_job` field; see that field's doc
/// comment for why it isn't folded into `ml_scan`.
#[derive(Clone)]
pub(super) struct RgJobState {
    /// Tracks analyzed so far (see `crate::replaygain::RgJobProgress::done`).
    pub(super) current: usize,
    pub(super) total: usize,
    pub(super) cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Shared helper: start an ML scan with the given scan type and total count.
pub(super) fn start_ml_scan(
    state: &Rc<RefCell<AppState>>,
    scan_type: ScanType,
    total: usize,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_clone = cancel_flag.clone();
    {
        let mut s = state.borrow_mut();
        // A cancelled scan is already cleared (see `cancel_ml_scan`), so only
        // count a background op when we are not replacing a live one.
        if s.ml_scan.is_none() {
            s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
        }
        s.ml_scan = Some(ScanState {
            scan_type,
            current: 0,
            total,
            cancel: cancel_clone,
        });
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
    cancel_flag
}

/// Shared helper: update ML scan progress and notify UI.
pub(super) fn update_ml_scan_progress(state: &Rc<RefCell<AppState>>, current: usize, total: usize) {
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
///
/// No-ops when there is nothing to complete. A cancelled scan is torn down at
/// the moment of cancelling, so its worker's eventual "finished" message
/// arrives after the fact and must not decrement `pending_bg_ops` a second
/// time or clear a scan the user has since started.
pub(super) fn complete_ml_scan(state: &Rc<RefCell<AppState>>) {
    {
        let mut s = state.borrow_mut();
        if s.ml_scan.is_none() {
            return;
        }
        s.ml_scan = None;
        s.pending_bg_ops.set(s.pending_bg_ops.get().saturating_sub(1));
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
}

/// Shared helper: cancel an ML scan and notify UI.
///
/// Clears the scan straight away rather than waiting for the worker to notice
/// the flag. The worker checks it between files, so on a slow disk that wait
/// is seconds long — and until 2026-08-11 the scan stayed "in flight" for all
/// of it: the progress numbers kept the cancelled scan's totals on screen, and
/// starting a new scan was silently refused by the `ml_scan.is_some()` guard
/// every caller has. Cancel then Rescan appeared to do nothing at all.
///
/// The worker still runs to its next check and exits on its own.
/// [`complete_ml_scan`] ignores its late "finished" message. Its last progress
/// message can still land on a scan started in the meantime, moving the bar by
/// one file; the next real update corrects it. Closing that window properly
/// needs the worker's cancel flag at the update site, and it is moved into the
/// worker thread at all six of them.
pub(super) fn cancel_ml_scan(state: &Rc<RefCell<AppState>>) {
    {
        let mut s = state.borrow_mut();
        if let Some(scan) = s.ml_scan.take() {
            scan.cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            s.pending_bg_ops.set(s.pending_bg_ops.get().saturating_sub(1));
        }
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
}

/// Start a ReplayGain analysis job. Refuses (returns `None`) if one is
/// already running, or a metadata scan (`ml_scan`) is in flight — both spin
/// up a worker-local `MediaLibrary` writer, and while SQLite's WAL mode
/// tolerates the concurrent writes just fine, running two background jobs
/// against the library at once is confusing UI-wise for no benefit.
pub(super) fn start_rg_job(
    state: &Rc<RefCell<AppState>>,
    total: usize,
) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
    let mut s = state.borrow_mut();
    if s.rg_job.is_some() || s.ml_scan.is_some() {
        return None;
    }
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    s.rg_job = Some(RgJobState {
        current: 0,
        total,
        cancel: cancel_flag.clone(),
    });
    // Drop any stale completion message from a previous run so pollers don't
    // flash last time's "Analyzed N" while this job is spinning up.
    s.rg_ui_msg = None;
    s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
    Some(cancel_flag)
}

/// Shared helper: update RG analysis job progress.
pub(super) fn update_rg_job_progress(state: &Rc<RefCell<AppState>>, current: usize, total: usize) {
    let mut s = state.borrow_mut();
    if let Some(ref mut job) = s.rg_job {
        job.current = current;
        job.total = total;
    }
}

/// Shared helper: complete an RG analysis job, stashing the one-shot result
/// message (`msg`) for whichever view's poller renders it next.
pub(super) fn complete_rg_job(state: &Rc<RefCell<AppState>>, msg: String) {
    let mut s = state.borrow_mut();
    s.rg_job = None;
    s.rg_ui_msg = Some(msg);
    s.pending_bg_ops.set(s.pending_bg_ops.get() - 1);
}

/// Shared helper: signal cancellation of the running RG analysis job.
pub(super) fn cancel_rg_job(state: &Rc<RefCell<AppState>>) {
    let s = state.borrow();
    if let Some(ref job) = s.rg_job {
        job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Sync the ReplayGain analyze/cancel buttons + status label to the shared
/// `rg_job` / `rg_ui_msg` state. Called from every view's poll timer (the
/// Settings window and the Files view) so both render identical progress,
/// completion, and Analyze⇄Cancel toggling. Returns `true` while a job runs.
///
/// - `other_busy`: a non-RG background op (e.g. an `ml_scan`) is also running;
///   keeps Analyze disabled even though no RG job holds it.
/// - `render_status`: when `false` the caller owns the status label this tick
///   (e.g. a metadata scan is writing "Reading tags…"), so we touch neither
///   the label nor the completion message.
/// - `prev_running`: this poller's own view of whether a job was running on
///   its *previous* tick. The completion message is rendered only on the
///   running→idle edge, and PEEKED (not consumed) — so every view that was
///   watching the job renders it exactly once. A shared one-shot `take()`
///   let whichever poller ticked first swallow the message, leaving the
///   other view stuck on its last "Analyzing N/M" (notably after Cancel).
///
/// Returns the current running state; callers store it for next tick's
/// `prev_running`.
pub(super) fn sync_rg_ui(
    state: &Rc<RefCell<AppState>>,
    analyze_btn: &gtk4::Button,
    cancel_btn: &gtk4::Button,
    status: &gtk4::Label,
    rg_available: bool,
    other_busy: bool,
    render_status: bool,
    prev_running: bool,
) -> bool {
    let (rg_state, msg) = {
        let s = state.borrow();
        (s.rg_job.clone(), s.rg_ui_msg.clone())
    };
    let running = rg_state.is_some();
    // Analyze ⇄ Cancel: never both visible at once.
    analyze_btn.set_visible(!running);
    analyze_btn.set_sensitive(!running && !other_busy && rg_available);
    cancel_btn.set_visible(running);
    if render_status {
        if let Some(rg) = rg_state {
            if rg.total > 0 {
                status.set_text(&format!("Analyzing ReplayGain {}/{}…", rg.current, rg.total));
            } else {
                status.set_text("Analyzing ReplayGain…");
            }
        } else if prev_running {
            // Just finished (from this view's perspective) — render the shared
            // completion message once. Peek, don't take: the other view's
            // poller needs it too.
            if let Some(m) = msg {
                status.set_text(&m);
            }
        }
    }
    running
}

/// Shared helper: update scan UI elements based on current ml_scan state.
/// Returns true if scanning is in progress.
#[allow(dead_code)]
pub(super) fn update_scan_ui_elements(
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

/// Lazily open the Media-Library database if it isn't already (F12.3
/// `skip_db_load`). A no-op if `media_lib` is already `Some` — safe to call
/// from every first-demand site without checking first.
///
/// Call this at the app's real first-demand sites: the ML window open
/// handler (which also covers the device-sync view nested inside it) is the
/// only one wired today. On a successful first open it also kicks the
/// folder watcher via `watch::rebuild_watcher` — under `skip_db_load` the
/// watcher stays dormant until this fires (binding user decision: never
/// force the DB open at startup just because `watch_folders` is on).
pub(super) fn ensure_media_lib_open(state: &Rc<RefCell<AppState>>) {
    if state.borrow().media_lib.is_some() {
        return;
    }
    {
        let mut s = state.borrow_mut();
        s.media_lib = crate::media_library::MediaLibrary::open().ok();
    }
    if state.borrow().media_lib.is_some() {
        // Match the eager-open path: purge soft-deleted rows from prior
        // sessions on this first open (own connection — MediaLibrary isn't
        // Send). Skipped entirely at startup under `skip_db_load`, so this
        // is the lazy path's only chance to run it.
        let db_path = crate::media_library::MediaLibrary::db_path_pub();
        std::thread::spawn(move || {
            if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                let _ = lib.cleanup_on_startup();
            }
        });
        watch::rebuild_watcher(state);
    }
}

/// Build the engine's ReplayGain chain shape from config. Shared by startup
/// and the settings-change apply path so they never drift.
pub(super) fn rg_chain(cfg: &Config) -> crate::engine::RgChain {
    let rg = &cfg.playback.replaygain;
    crate::engine::RgChain {
        enabled: rg.enabled,
        clip_protection: rg.clip_protection,
        fallback_db: rg.fallback_db as f64,
    }
}

impl AppState {
    /// Re-apply the ReplayGain chain from config (settings changed). Reshapes
    /// the pipeline now if Stopped, else defers to the next track (engine).
    /// Also refreshes album-mode from the current source + shuffle state.
    pub(crate) fn apply_replaygain(&mut self) {
        self.player.set_replaygain(rg_chain(&self.config));
        self.apply_rg_album_mode();
        // A chain reshape (enable / clip-protection) needs a Null pipeline, so
        // the engine deferred it while Playing. Reload the current track at its
        // position so the toggle takes effect on what the user is hearing now.
        if self.player.rg_reload_pending()
            && *self.player.state() == crate::engine::PlayerState::Playing
        {
            self.reload_current_at_position();
        }
    }

    /// Reload the current track and resume near its previous position — the
    /// only way to apply a ReplayGain chain reshape (which needs a Null
    /// pipeline) without waiting for the next track. `load()` applies the
    /// pending chain at Null; the pending-seek machinery (see `play_current`)
    /// restores the position on the next tick.
    pub(super) fn reload_current_at_position(&mut self) {
        let (pos, dur) = (self.player.position(), self.player.duration());
        let Some(track) = self.playlist.current() else {
            return;
        };
        let uri = track.uri();
        if let (Some(p), Some(d)) = (pos, dur) {
            let secs = d.as_secs_f64();
            if secs > 0.0 {
                self.pending_seek = Some((p.as_secs_f64() / secs).clamp(0.0, 1.0));
            }
        }
        let rg_album_mode = crate::config::rg_album_mode(
            self.config.playback.replaygain.source,
            self.config.playback.shuffle_enabled,
        );
        let rg_path = track.path.to_string_lossy().into_owned();
        crate::replaygain::prime_player_gain(
            &mut self.player,
            self.media_lib.as_ref(),
            &rg_path,
            rg_album_mode,
        );
        let _ = self.player.load(&uri); // applies the pending RG chain at Null
        if self.pending_seek.is_some() {
            // Mute the brief position-0 audio until the tick applies the seek.
            self.mute_pending = Some(self.config.playback.volume);
            self.player.set_volume(0.0);
        }
        let _ = self.player.play();
    }

    /// Live fallback-gain change (slider) — no pipeline rebuild.
    pub(crate) fn set_rg_fallback_db(&mut self, db: f64) {
        self.config.playback.replaygain.fallback_db = db as f32;
        self.player.set_rg_fallback_db(db);
    }

    /// Set rgvolume album-mode from the ReplayGain source + shuffle state.
    /// Automatic → album when playing sequentially, track when shuffling.
    pub(crate) fn apply_rg_album_mode(&mut self) {
        let album = crate::config::rg_album_mode(
            self.config.playback.replaygain.source,
            self.shuffle_state.enabled,
        );
        self.player.set_rg_album_mode(album);
    }

    /// Initialise `AppState` from the given playlist and config.
    ///
    /// Creates a new GStreamer player and immediately applies the configured
    /// volume.  Returns an error if the GStreamer `playbin` element is
    /// unavailable.
    pub(super) fn new(playlist: Playlist, config: Config) -> Result<Self> {
        let mut player = Player::new()?;
        player.set_volume(config.playback.volume);
        // Apply the saved EQ config so the correct settings are active from
        // the very first track — even before the user opens the EQ window.
        player.apply_eq_bands(&config.equalizer.effective_bands());
        // Apply the saved ReplayGain chain from the first track. The player is
        // Stopped here, so this reshapes the pipeline immediately.
        player.set_replaygain(rg_chain(&config));
        player.set_rg_album_mode(crate::config::rg_album_mode(
            config.playback.replaygain.source,
            config.playback.shuffle_enabled,
        ));
        // F12.3: when `skip_db_load` is on, leave the Media-Library database
        // unopened at startup — `ensure_media_lib_open` (below) opens it
        // lazily the first time something actually needs it (ML window open,
        // which also covers the device-sync view nested inside it). This
        // also skips the soft-delete cleanup sweep, which otherwise opens
        // the DB unconditionally on every launch.
        let media_lib = if config.media_library.skip_db_load {
            None
        } else {
            let lib = crate::media_library::MediaLibrary::open().ok();

            // Startup cleanup: purge any soft-deleted records from previous sessions
            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            std::thread::spawn(move || {
                if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                    let _ = lib.cleanup_on_startup();
                }
            });

            lib
        };

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
            queue: crate::queue::Queue::new(),
            pending_seek: None,
            last_duration: None,
            mute_pending: None,
            duration_cache: DurationCache::load(),
            media_lib,
            watch: None,
            watch_rx: None,
            row_facts_tx: None,
            ml_window: None,
            settings_window: None,
            id3_editor_window: None,
            lyrics_window: None,
            lyrics_mode: Rc::new(std::cell::Cell::new(LyricsMode::Specific)),
            lyrics_refresh: None,
            lyrics_shown_path: Rc::new(RefCell::new(None)),
            transport_key_handler: None,
            art_window: None,
            mpris_guard: None,
            rebuild_ml_callback: None,
            disc_refresh_callback: None,
            pending_disc_nav: None,
            disc_reading: std::cell::Cell::new(false),
            ml_scan_ui_callback: None,
            rebuild_pl_callback: None,
            play_and_update_callback: None,
            set_track_callback: None,
            now_playing_subscribers: Vec::new(),
            current_now_playing: None,
            pending_bg_ops: std::cell::Cell::new(0),
            counted_play_path: None,
            ml_scan: None,
            playlist_scan: None,
            rg_job: None,
            rg_ui_msg: None,
        })
    }

    /// Load and start playback of the track at `playlist.current_index`.
    ///
    /// Returns `Some(display_name)` so the caller can update the marquee, or
    /// `None` if the playlist is empty.  Load / play errors surface on the
    /// next `poll_bus()` call in the tick loop.
    pub(super) fn play_current(&mut self) -> Option<String> {
        // Manual play cancels a pending stop-after-current (phase 6). The EOS
        // auto-advance in the tick uses play_current_no_record, not this seam.
        self.player.set_stop_after_current(false);
        let track = self.playlist.current()?;
        let uri = track.uri();
        let display = track.display_name();
        // Captured now — `track` borrows `self.playlist` and that borrow
        // ends at this statement's last use below, before the `&mut self`
        // field accesses that follow. `auto_add_played` needs the path
        // after `play()`, once `track` is long gone.
        let played_path = track.path.clone();
        // Record this track in shuffle history so the previous button can step back.
        let idx = self.playlist.current_index;
        self.shuffle_state.record_played(idx);
        // Reset so the new track can be counted when it plays long enough.
        self.counted_play_path = None;
        // This track's stored ReplayGain, handed to the pipeline before the
        // load consumes it — rgvolume only reads tags off the stream, so a
        // gain that lives only in the library needs feeding in explicitly.
        let rg_album_mode = crate::config::rg_album_mode(
            self.config.playback.replaygain.source,
            self.config.playback.shuffle_enabled,
        );
        crate::replaygain::prime_player_gain(
            &mut self.player,
            self.media_lib.as_ref(),
            &played_path.to_string_lossy(),
            rg_album_mode,
        );
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
        self.maybe_auto_add_played(&played_path);
        Some(display)
    }

    /// Same as `play_current()` but does not record to shuffle history.
    /// Used for back navigation via history to avoid corrupting the history cursor.
    pub(super) fn play_current_no_record(&mut self) -> Option<String> {
        let track = self.playlist.current()?;
        let uri = track.uri();
        let display = track.display_name();
        let played_path = track.path.clone();
        // Reset so the new track can be counted when it plays long enough.
        self.counted_play_path = None;
        let rg_album_mode = crate::config::rg_album_mode(
            self.config.playback.replaygain.source,
            self.config.playback.shuffle_enabled,
        );
        crate::replaygain::prime_player_gain(
            &mut self.player,
            self.media_lib.as_ref(),
            &played_path.to_string_lossy(),
            rg_album_mode,
        );
        let _ = self.player.load(&uri);
        if self.pending_seek.is_some() {
            self.mute_pending = Some(self.config.playback.volume);
            self.player.set_volume(0.0);
        }
        let _ = self.player.play();
        self.maybe_auto_add_played(&played_path);
        Some(display)
    }

    /// Auto-add-played (Phase 8 Task 10, guard added in the fix wave): make
    /// sure a track that just started playing, and lives OUTSIDE every
    /// watched folder, has a row in the media library — gated on
    /// `config.media_library.auto_add_played`. This is the documented
    /// intent (config field doc + Task 5/7): tracks under a watched folder
    /// are already managed by the watcher/rescan, so auto-add only exists
    /// to catch playback from outside that set (the folder_id-NULL bucket).
    ///
    /// No-op if the setting is off, the library isn't open, or
    /// `add_played_track` reports the path is already known (`Ok(false)`).
    ///
    /// The inside/outside check is why this guards with `owning_folder_id`
    /// rather than calling `add_played_track` unconditionally:
    /// `Track::path` is canonicalized (`Track::from_path`/`from_path_fast`,
    /// once at load time), but the library's scan paths are stored
    /// UN-canonicalized (`rescan_folder_fast` skips the canonicalize() stat
    /// for perf). For a track already indexed under a watched folder —
    /// especially one reached via a symlink — those two strings can differ,
    /// so `add_played_track`'s exact-string "already known" check could
    /// miss the existing row and INSERT A DUPLICATE. Skipping the call
    /// entirely whenever `owning_folder_id` resolves to `Some(_)` (inside a
    /// watched folder) avoids that risk outright, rather than trying to
    /// normalize the two path representations to match.
    ///
    /// Deliberately does NOT invoke `rebuild_ml_callback` on a new row: this
    /// method runs inside `play_current`/`play_current_no_record`, which
    /// every call site invokes as `state.borrow_mut().play_current()` — the
    /// `RefCell` borrow is still live for the whole expression, so firing a
    /// UI callback here (which may itself need to borrow `state`) risks a
    /// borrow panic. An open Files view simply won't show the new row until
    /// its next natural refresh.
    pub(super) fn maybe_auto_add_played(&self, path: &std::path::Path) {
        if !self.config.media_library.auto_add_played {
            return;
        }
        let Some(lib) = self.media_lib.as_ref() else {
            return;
        };
        let Some(path_str) = path.to_str() else {
            return;
        };
        match lib.owning_folder_id(path_str) {
            // Inside a watched folder — the watcher/rescan already owns
            // this path; adding it here risks a duplicate row (see doc
            // comment above), so skip.
            Ok(Some(_)) => {}
            // Outside every watched folder — the case auto-add-played
            // exists for.
            Ok(None) => {
                if let Err(e) = lib.add_played_track(path_str) {
                    eprintln!("auto_add_played: failed for {}: {e}", path.display());
                }
            }
            Err(e) => {
                eprintln!(
                    "auto_add_played: owning_folder_id lookup failed for {}: {e}",
                    path.display()
                );
            }
        }
    }

    /// Register a now-playing subscriber (A1 panel, A6 window, phase-3 MPRIS).
    /// Fired once per track start, after the play-start snapshot is captured.
    pub fn subscribe_now_playing(&mut self, cb: Rc<dyn Fn(&crate::now_playing::NowPlayingInfo)>) {
        self.now_playing_subscribers.push(cb);
    }

    /// Publish the main window's key handler so the lyrics window can forward
    /// the Winamp transport keys to it (F15 revision, point 5).
    pub fn set_transport_key_handler(
        &mut self,
        h: Rc<dyn Fn(gtk4::gdk::Key) -> gtk4::glib::Propagation>,
    ) {
        self.transport_key_handler = Some(h);
    }

    /// The published transport key handler, if `player.rs` has built it yet.
    pub fn transport_key_handler(
        &self,
    ) -> Option<Rc<dyn Fn(gtk4::gdk::Key) -> gtk4::glib::Propagation>> {
        self.transport_key_handler.clone()
    }

    /// The now-playing info for the currently loaded track, if any. Cloned
    /// out (rather than returning a reference) so callers can drop the
    /// borrow before doing any widget construction with it — see the
    /// borrow-safety note on `notify_now_playing` below.
    pub fn current_now_playing(&self) -> Option<crate::now_playing::NowPlayingInfo> {
        self.current_now_playing.clone()
    }

    /// Fan out `info` to every subscriber.
    ///
    /// Takes `&self`, not `&mut self` — callers holding a `Rc<RefCell<AppState>>`
    /// must NOT call this while a `borrow_mut()` is still live, since a
    /// subscriber may itself need to borrow `state` (e.g. to read widgets or
    /// re-render). Build the `NowPlayingInfo` first, drop the borrow, then call.
    ///
    /// The play-start seam in player.rs notifies by hand (extract subscriber
    /// Vec under a short borrow, drop it, then loop) rather than calling this,
    /// for the same borrow-safety reason. This method is here for phase-3
    /// pause/resume/end re-notify, which is deferred (see task-5-report.md).
    #[allow(dead_code)]
    pub fn notify_now_playing(&self, info: &crate::now_playing::NowPlayingInfo) {
        for cb in &self.now_playing_subscribers {
            cb(info);
        }
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
    /// Pop the next still-present queued entry's playlist index (draining any
    /// ids no longer present), or `None`. Mirrors `Controller::queue_next_index`
    /// — GTK runs its own advance loop rather than the shared controller.
    pub(super) fn queue_next_index(&mut self) -> Option<usize> {
        while let Some(id) = self.queue.pop_next() {
            if let Some(idx) = self.playlist.tracks.iter().position(|t| t.id == id) {
                return Some(idx);
            }
        }
        None
    }

    /// Drop queued ids whose entries no longer exist (playlist remove/clear).
    pub(crate) fn sync_queue_to_playlist(&mut self) {
        let live: std::collections::HashSet<u64> =
            self.playlist.tracks.iter().map(|t| t.id).collect();
        self.queue.retain_ids(&live);
    }

    pub(super) fn play_next(&mut self) -> Option<String> {
        // Manual skip cancels a pending stop-after-current (phase 6).
        self.player.set_stop_after_current(false);
        let total = self.playlist.len();
        let current = self.playlist.current_index;
        let repeat = self.config.playback.repeat_mode;

        // Manual queue wins over shuffle/linear. Jump to the queued entry's
        // position (resume point) and play without recording into shuffle
        // history — queue playback is manual, not a shuffle pick.
        // phase 6: stop-after-current guards ABOVE this.
        if let Some(idx) = self.queue_next_index() {
            self.playlist.jump_to(idx);
            return if *self.player.state() != PlayerState::Stopped {
                self.play_current_no_record()
            } else {
                self.playlist.current().map(|t| t.display_name())
            };
        }

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
    pub(super) fn play_prev(&mut self) -> Option<String> {
        // Manual skip cancels a pending stop-after-current (phase 6).
        self.player.set_stop_after_current(false);
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
    pub(super) fn toggle_visualizer_mode(&mut self) {
        self.config.visualizer.mode = match self.config.visualizer.mode {
            VisualizerMode::Bars => VisualizerMode::Waveform,
            VisualizerMode::Waveform => VisualizerMode::Granite,
            VisualizerMode::Granite => VisualizerMode::Bars,
        };
    }

    /// Attempt to retry spectrum initialization.
    ///
    /// Returns Ok(()) if retry was initiated, Err if spectrum is not available.
    pub(super) fn retry_spectrum(&mut self) -> Result<(), &'static str> {
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
    pub(super) fn seek_fraction(&mut self, fraction: f64) {
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
    pub(super) fn seek_fraction_or_pend(&mut self, fraction: f64) {
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
    pub(super) fn seek_delta_secs(&mut self, secs: f64) {
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
    pub(super) fn apply_cached_durations(&mut self) {
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
    pub(super) fn uncached_paths_from(&self, start: usize) -> Vec<std::path::PathBuf> {
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
    pub(super) fn apply_probed_durations(
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
    pub(super) fn time_display_for_fraction(&self, fraction: f64, show_remaining: bool) -> Option<String> {
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
    pub(super) fn remove_track(&mut self, index: usize) -> Option<String> {
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

    /// Sort the active playlist by `key`. Resets shuffle history since the
    /// track order (and therefore what "already played" means) has changed.
    pub(crate) fn sort_playlist(&mut self, key: crate::model::SortKey) {
        self.playlist.sort_by(key);
        self.shuffle_state.reset();
    }

    /// Reverse the active playlist. Resets shuffle history (see `sort_playlist`).
    pub(crate) fn reverse_playlist(&mut self) {
        self.playlist.reverse();
        self.shuffle_state.reset();
    }

    /// Randomly permute the active playlist once. Resets shuffle history
    /// (see `sort_playlist`).
    pub(crate) fn randomize_playlist(&mut self) {
        self.playlist.randomize();
        self.shuffle_state.reset();
    }

    /// Add a single audio file from a raw path string.
    ///
    /// Leading and trailing whitespace is trimmed before the path is
    /// resolved.  Returns `Ok(display_name)` on success or `Err(message)`
    /// Fill in a duration we already know but the track was built without.
    ///
    /// `Track::from_path` reads tags, not length — measuring a duration means
    /// decoding, which is far too slow to do while a drop of several hundred
    /// files is in flight. So a track added by path arrives with `duration:
    /// None` and the playlist shows a blank length.
    ///
    /// Two places already hold the answer, both free to consult: the library
    /// row (`length_secs`, filled during the scan) and the on-disk duration
    /// cache. Dragging from the Files view is the obvious case — those rows
    /// are *showing* a duration the moment before the drop, and it looked like
    /// a bug for it to vanish on landing (reported 2026-08-11). Anything still
    /// unknown is left for the background prober, as before.
    // Superseded for every GTK add path by `playlist_add`, which resolves
    // against the library in one batched query instead of one lookup per file
    // and defers the filesystem checks to the background pass. Kept because
    // `add_track_from_path` still backs the unit tests below it, and because
    // both remain the correct shape for adding a single known path.
    #[allow(dead_code)]
    fn fill_known_duration(&self, track: &mut Track) {
        if track.duration.is_some() {
            return;
        }
        let path = track.path.to_string_lossy();
        let secs = self
            .media_lib
            .as_ref()
            .and_then(|lib| lib.track_by_path(&path).ok())
            .and_then(|t| t.length_secs);
        if let Some(secs) = secs {
            track.duration = Some(Duration::from_secs_f64(secs));
            return;
        }
        if let Some(d) = self.duration_cache.get(&track.path) {
            track.duration = Some(d);
        }
    }

    /// on failure.  Use [`add_path`] when the input might be a directory.
    #[allow(dead_code)]
    pub(super) fn add_track_from_path(&mut self, raw_path: &str) -> Result<String, String> {
        let path = std::path::Path::new(raw_path.trim());
        match Track::from_path(path) {
            Ok(mut track) => {
                self.fill_known_duration(&mut track);
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
    #[allow(dead_code)]
    pub(super) fn add_path(&mut self, path: &std::path::Path) -> Result<String, String> {
        if path.is_dir() {
            // Recursively collect all audio files under the directory.
            let files = Playlist::collect_audio_files(path);
            let total = files.len();
            if total == 0 {
                return Err(format!("No audio files found in '{}'", path.display()));
            }
            let mut added = 0usize;
            for file in files {
                if let Ok(mut track) = Track::from_path(&file) {
                    self.fill_known_duration(&mut track);
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
    pub(super) fn poll_bus(&mut self) -> Option<BusEvent> {
        self.player.poll_bus()
    }

    /// Advance a stop-with-fadeout ramp (Shift+V). Returns true on the tick
    /// that finishes it, by which point the player is already stopped — the
    /// caller uses that to reset the seek bar and status line.
    pub(super) fn poll_fadeout(&mut self) -> bool {
        self.player.poll_fadeout()
    }
}
