//! Behavior / settings accessors and the read-only audio-extension list
//! (mirrors `model::AUDIO_EXTENSIONS`).
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use super::SparkampCtx;

// ---------------------------------------------------------------------------
// Audio extensions (read-only, mirrors model::AUDIO_EXTENSIONS)
// ---------------------------------------------------------------------------
//
// Exposed so frontends building file pickers can use the canonical list
// instead of maintaining their own (drift-prone) copy.  Strings are static
// and null-terminated; the returned pointer is valid for the lifetime of
// the process and must not be freed.

/// Number of supported audio file extensions.
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_audio_extension_count() -> c_int {
    crate::model::AUDIO_EXTENSIONS.len() as c_int
}

/// Get the audio extension at `idx` as a null-terminated lowercase ASCII
/// string (no leading dot — e.g. "mp3", "flac").  Returns NULL if `idx` is
/// out of range.  The returned pointer is static and must not be freed.
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_audio_extension(idx: c_int) -> *const c_char {
    use std::sync::OnceLock;
    // OnceLock so each extension gets one stable CString pointer for the
    // process lifetime (callers may cache them).
    static CACHE: OnceLock<Vec<CString>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        crate::model::AUDIO_EXTENSIONS
            .iter()
            .map(|s| CString::new(*s).expect("audio extensions are static ASCII"))
            .collect()
    });
    if idx < 0 {
        return std::ptr::null();
    }
    cache
        .get(idx as usize)
        .map(|cs| cs.as_ptr())
        .unwrap_or(std::ptr::null())
}

// ---------------------------------------------------------------------------
// Behavior / Settings
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_playlist_add_behavior(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    match ctx.config.behavior.playlist_add_behavior {
        crate::config::PlaylistAddBehavior::Append => 0,
        crate::config::PlaylistAddBehavior::Replace => 1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_playlist_add_behavior(
    ctx: *mut SparkampCtx,
    value: c_int,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.behavior.playlist_add_behavior = match value {
        1 => crate::config::PlaylistAddBehavior::Replace,
        _ => crate::config::PlaylistAddBehavior::Append,
    };
}

/// Preferred new-playlist format: 0 = m3u8 (default), 1 = m3u.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_playlist_format(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    match ctx.config.media_library.playlist_format {
        crate::config::PlaylistFormat::M3u8 => 0,
        crate::config::PlaylistFormat::M3u => 1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_playlist_format(ctx: *mut SparkampCtx, value: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.playlist_format = match value {
        1 => crate::config::PlaylistFormat::M3u,
        _ => crate::config::PlaylistFormat::M3u8,
    };
}

/// The configured gnudb submission email, or "" when effectively unset
/// (blank, or the retired app-wide default an older config may carry) — the
/// frontends prompt for a real address before the first submission. Heap C
/// string — free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_gnudb_email(ctx: *const SparkampCtx) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let email = &ctx.config.disc.gnudb_email;
    let out = if crate::disc::gnudb::is_unset_email(email) {
        ""
    } else {
        email.as_str()
    };
    CString::new(out)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Set the gnudb email (ignored when empty after trimming).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_gnudb_email(ctx: *mut SparkampCtx, email: *const c_char) {
    if ctx.is_null() || email.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let s = std::ffi::CStr::from_ptr(email)
        .to_string_lossy()
        .trim()
        .to_string();
    if !s.is_empty() {
        ctx.config.disc.gnudb_email = s;
    }
}

/// Whether gnudb submissions run in test mode (validated, not published).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_gnudb_submit_test(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    let ctx = &*ctx;
    ctx.config.disc.gnudb_submit_mode_test
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_gnudb_submit_test(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.disc.gnudb_submit_mode_test = value;
}

/// Whether the time counter shows time left rather than time played.
///
/// Persisted, unlike the stop-after-current flag below. Clicking the counter is
/// how this gets changed, and nobody opens a preferences window to set it, so
/// it has to come back the way it was left.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_show_remaining(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.display.show_remaining()
}

/// Set the counter's mode. The caller persists it with `sparkamp_save_config`,
/// as with every other setter here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_show_remaining(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.display.set_show_remaining(value);
}

/// Stop-after-current-track flag (phase 6, transient — not persisted). Lives on
/// the engine Player so the mac key `t` and any menu item share one source.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_stop_after_current(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.player.stop_after_current()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_stop_after_current(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.player.set_stop_after_current(value);
}

/// How long stop-with-fadeout (Shift+V) takes, in seconds. Persisted under
/// `playback.fadeout_secs`; the setter clamps to the accepted range so the UI
/// cannot store a value the fade would then ignore.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_fadeout_secs(ctx: *const SparkampCtx) -> u32 {
    if ctx.is_null() {
        return crate::config::DEFAULT_FADEOUT_SECS;
    }
    (*ctx).config.playback.fadeout_secs
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_fadeout_secs(ctx: *mut SparkampCtx, value: u32) {
    if ctx.is_null() {
        return;
    }
    let range = crate::config::FADEOUT_SECS_RANGE;
    (*ctx).config.playback.fadeout_secs = value.clamp(*range.start(), *range.end());
}

/// Last chosen rip destination directory ("" when unset — the UI then
/// defaults to the first watched folder and prompts before the first rip).
/// Heap C string — free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rip_dest(ctx: *const SparkampCtx) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let s = ctx
        .config
        .disc
        .rip_dest_dir
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rip_dest(ctx: *mut SparkampCtx, dir: *const c_char) {
    if ctx.is_null() || dir.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let s = std::ffi::CStr::from_ptr(dir).to_string_lossy().trim().to_string();
    ctx.config.disc.rip_dest_dir = if s.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(s))
    };
}

/// MP3 rip preset: 0 = VBR V0, 1 = VBR V2 (default), 2 = 320 CBR.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rip_quality(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 1;
    }
    (&*ctx).config.disc.rip_mp3_quality as c_int
}

/// Verify discs after burning where the tool supports it (default true).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_burn_verify(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (&*ctx).config.disc.burn_verify
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_burn_verify(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.disc.burn_verify = value;
}

/// Auto-open the Media Library to a drive when it receives an audio CD
/// (default true). Only takes effect once the app is running — OS-level
/// default-handler registration is a separate manual step.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_auto_show_inserted_cd(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (&*ctx).config.disc.auto_show_inserted_audio_cd
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_auto_show_inserted_cd(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.disc.auto_show_inserted_audio_cd = value;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rip_quality(ctx: *mut SparkampCtx, preset: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.disc.rip_mp3_quality = match preset {
        0 => 0,
        2 => 2,
        _ => 1,
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_autoplay_on_add(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.behavior.autoplay_on_add
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_autoplay_on_add(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.behavior.autoplay_on_add = value;
}

// ---------------------------------------------------------------------------
// Watch folders (Phase 8 Task 9) — live background filesystem watcher.
// Plain config mutators, mirroring the sparkamp_get/set_stop_after_current
// idiom, EXCEPT sparkamp_set_watch_folders which also starts/stops the
// watcher (see `media_library::rebuild_watcher`) since that flag has a live
// side effect the others don't. None of these call `sparkamp_save_config`
// themselves — persistence happens wherever the frontend already persists
// other settings.
// ---------------------------------------------------------------------------

/// Whether Sparkamp watches library folders for filesystem changes and
/// auto-applies them (add/remove tracks) instead of relying on manual or
/// interval rescans.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_watch_folders(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.media_library.watch_folders
}

/// Setting this also (re)builds or tears down the live watcher immediately
/// — see `media_library::rebuild_watcher`. A failed watcher start degrades
/// gracefully (logged, not fatal); it never panics or blocks Swift.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_watch_folders(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.watch_folders = value;
    super::media_library::rebuild_watcher(ctx);
}

/// Whether playing a file not yet in the library auto-adds it (first-play
/// auto-add). Gating only — `MediaLibrary::add_played_track` does the work;
/// wiring the playback call site is a separate task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_auto_add_played(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.media_library.auto_add_played
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_auto_add_played(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.auto_add_played = value;
}

/// Whether a track with no album-artist tag displays/groups under its artist
/// instead of blank (F12.2 — `play_stats::effective_album_artist`). Gating
/// only, mirroring `sparkamp_get/set_auto_add_played` above — persistence
/// happens wherever the frontend already persists other settings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_artist_as_album_artist(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.media_library.artist_as_album_artist
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_artist_as_album_artist(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.artist_as_album_artist = value;
}

/// Whether the Media-Library database is left unopened at startup, to be
/// opened lazily on first demand (F12.3). Gating only, mirroring
/// `sparkamp_get/set_auto_add_played` above — persistence happens wherever
/// the frontend already persists other settings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_skip_db_load(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.media_library.skip_db_load
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_skip_db_load(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.skip_db_load = value;
}

/// Whether each Media-Library view's search query (F12) is restored the next
/// time that view is opened. Gating only, mirroring
/// `sparkamp_get/set_auto_add_played` above — persistence happens wherever
/// the frontend already persists other settings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_remember_search(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.media_library.remember_search
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_remember_search(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.remember_search = value;
}

/// Last search query saved for `view_id` ("files"/"playlists"/"devices"/
/// "discs"), or "" when none is saved. Only meaningful when
/// `remember_search` is on — callers should still check that flag before
/// prefilling a search box, since the map may hold stale entries from when
/// the feature was previously enabled. Heap C string — free with
/// `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_last_search(
    ctx: *const SparkampCtx,
    view_id: *const c_char,
) -> *mut c_char {
    if ctx.is_null() || view_id.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let view_id = std::ffi::CStr::from_ptr(view_id).to_string_lossy();
    let query = ctx
        .config
        .media_library
        .last_search
        .get(view_id.as_ref())
        .cloned()
        .unwrap_or_default();
    CString::new(query)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Save `query` as the last search for `view_id`. A no-op if either string is
/// not valid UTF-8-ish C data; an empty `query` still records (clears) the
/// entry rather than removing it, matching a cleared search box.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_last_search(
    ctx: *mut SparkampCtx,
    view_id: *const c_char,
    query: *const c_char,
) {
    if ctx.is_null() || view_id.is_null() || query.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let view_id = std::ffi::CStr::from_ptr(view_id).to_string_lossy().into_owned();
    let query = std::ffi::CStr::from_ptr(query).to_string_lossy().into_owned();
    ctx.config.media_library.last_search.insert(view_id, query);
}

// ---------------------------------------------------------------------------
// Play-count threshold (F11) — `[playback.play_stats]`. Plain config
// mutators, mirroring the sparkamp_get/set_auto_add_played idiom above; none
// of these call `sparkamp_save_config` themselves — persistence happens
// wherever the frontend already persists other settings.
// ---------------------------------------------------------------------------

/// Position (seconds) at which the current track should be counted as played,
/// given its length (`length_secs <= 0` means unknown). Returns `-1.0` when
/// play-stats are disabled or `ctx` is null — the caller then never records.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_play_deadline_secs(
    ctx: *const SparkampCtx,
    length_secs: f64,
) -> f64 {
    if ctx.is_null() {
        return -1.0;
    }
    let ctx = &*ctx;
    let len = if length_secs > 0.0 { Some(length_secs) } else { None };
    crate::play_stats::play_counted_at(len, &ctx.config.playback.play_stats).unwrap_or(-1.0)
}

/// Whether a play is ever recorded once its threshold is reached.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_play_stats_enabled(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    let ctx = &*ctx;
    ctx.config.playback.play_stats.enabled
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_play_stats_enabled(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.play_stats.enabled = value;
}

/// Active measurement mode: 0 = seconds, 1 = percent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_play_stats_mode(ctx: *const SparkampCtx) -> u32 {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    match ctx.config.playback.play_stats.mode {
        crate::config::PlayStatsMode::Seconds => 0,
        crate::config::PlayStatsMode::Percent => 1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_play_stats_mode(ctx: *mut SparkampCtx, value: u32) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.play_stats.mode = if value == 1 {
        crate::config::PlayStatsMode::Percent
    } else {
        crate::config::PlayStatsMode::Seconds
    };
}

/// Threshold in seconds (Seconds mode).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_play_stats_seconds(ctx: *const SparkampCtx) -> u32 {
    if ctx.is_null() {
        return 20;
    }
    let ctx = &*ctx;
    ctx.config.playback.play_stats.seconds
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_play_stats_seconds(ctx: *mut SparkampCtx, value: u32) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.play_stats.seconds = value.max(1);
}

/// Threshold as a percent of track length, 1..=100 (Percent mode).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_play_stats_percent(ctx: *const SparkampCtx) -> u32 {
    if ctx.is_null() {
        return 50;
    }
    let ctx = &*ctx;
    u32::from(ctx.config.playback.play_stats.percent)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_play_stats_percent(ctx: *mut SparkampCtx, value: u32) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.play_stats.percent = value.clamp(1, 100) as u8;
}

/// Whether a rescan (manual, interval, or a watch `Remove` event) hard-deletes
/// library rows for files that no longer exist on disk, vs. leaving them in
/// place (e.g. for temporarily-offline removable media).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_remove_missing_on_rescan(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.media_library.remove_missing_on_rescan
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_remove_missing_on_rescan(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.remove_missing_on_rescan = value;
}

/// Whether the library DB is compacted (VACUUM) after a rescan.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_compact_on_rescan(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.media_library.compact_on_rescan
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_compact_on_rescan(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.compact_on_rescan = value;
}

/// Whether the library rescans all watched folders automatically at startup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rescan_on_startup(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.media_library.rescan_on_startup
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rescan_on_startup(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.rescan_on_startup = value;
}

// ---------------------------------------------------------------------------
// ReplayGain (playback normalization) — mirrors the GTK/TUI settings surface.
// Playback-affecting setters (enabled/source/clip/fallback) rebuild the
// engine's rgvolume/rglimiter chain immediately if stopped, else the engine
// defers to the next track. auto_analyze/write_tags are library-side flags
// with no live playback effect (config only). Callers persist via
// `sparkamp_save_config`.
// ---------------------------------------------------------------------------

/// Rebuild + reapply the ReplayGain chain and album-vs-track mode from the
/// current config. Values are copied out before touching `ctx.player` so no
/// `ctx.config` borrow is held across the `&mut` player calls.
unsafe fn ffi_apply_replaygain(ctx: &mut SparkampCtx) {
    let chain = crate::engine::RgChain {
        enabled: ctx.config.playback.replaygain.enabled,
        clip_protection: ctx.config.playback.replaygain.clip_protection,
        fallback_db: ctx.config.playback.replaygain.fallback_db as f64,
    };
    let album = crate::config::rg_album_mode(
        ctx.config.playback.replaygain.source,
        ctx.config.playback.shuffle_enabled,
    );
    ctx.player.set_replaygain(chain);
    ctx.player.set_rg_album_mode(album);
    // Enabling/disabling ReplayGain or clip protection relinks the pipeline,
    // which is only legal at Null — so the engine deferred it to the next
    // load. Reload the current track at its position so the toggle applies to
    // what the user is hearing right now instead of silently waiting for the
    // next song (GTK's apply_replaygain does the same).
    if ctx.player.rg_reload_pending() && *ctx.player.state() == crate::engine::PlayerState::Playing
    {
        let pos = ctx.player.position();
        let dur = ctx.player.duration();
        let Some(uri) = ctx.playlist.current().map(|t| t.uri()) else {
            return;
        };
        super::prime_rg_for_current(ctx);
        super::load_or_report(&mut ctx.player, &uri);
        let _ = ctx.player.play();
        // Seek back only once the fresh pipeline can report a duration —
        // load() leaves it Null and play() is asynchronous, so an immediate
        // seek would be dropped. Handing the fraction to the same
        // pending-seek slot the tick already drains keeps that timing in one
        // place (GTK solves it the same way).
        if let (Some(p), Some(d)) = (pos, dur) {
            let secs = d.as_secs_f64();
            if secs > 0.0 {
                ctx.pending_seek = Some((p.as_secs_f64() / secs).clamp(0.0, 1.0));
            }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rg_enabled(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    (*ctx).config.playback.replaygain.enabled
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rg_enabled(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.replaygain.enabled = value;
    ffi_apply_replaygain(ctx);
}

/// ReplayGain source: 0 = Track, 1 = Album, 2 = Automatic (album unless
/// shuffle is on).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rg_source(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 2;
    }
    match (*ctx).config.playback.replaygain.source {
        crate::config::RgSource::Track => 0,
        crate::config::RgSource::Album => 1,
        crate::config::RgSource::Automatic => 2,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rg_source(ctx: *mut SparkampCtx, value: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.replaygain.source = match value {
        0 => crate::config::RgSource::Track,
        1 => crate::config::RgSource::Album,
        _ => crate::config::RgSource::Automatic,
    };
    ffi_apply_replaygain(ctx);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rg_clip_protection(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (*ctx).config.playback.replaygain.clip_protection
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rg_clip_protection(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.replaygain.clip_protection = value;
    ffi_apply_replaygain(ctx);
}

/// Fallback gain (dB) applied to tracks that carry no ReplayGain tags.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rg_fallback_db(ctx: *const SparkampCtx) -> f32 {
    if ctx.is_null() {
        return 0.0;
    }
    (*ctx).config.playback.replaygain.fallback_db
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rg_fallback_db(ctx: *mut SparkampCtx, db: f32) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.replaygain.fallback_db = db;
    // Fallback is a live one-liner on the engine — no chain rebuild needed.
    ctx.player.set_rg_fallback_db(db as f64);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rg_auto_analyze(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    (*ctx).config.playback.replaygain.auto_analyze
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rg_auto_analyze(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.playback.replaygain.auto_analyze = value;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rg_write_tags(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    (*ctx).config.playback.replaygain.write_tags
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rg_write_tags(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.playback.replaygain.write_tags = value;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real `SparkampCtx` for exercising `sparkamp_get/set_*` through
    /// the actual FFI functions (not just the config layer), mirroring the
    /// direct-construction pattern in `playlist.rs`'s
    /// `playlist_sort_resets_shuffle_history` test. `media_library` stays
    /// `None`, so `sparkamp_set_watch_folders`'s call into
    /// `media_library::rebuild_watcher` is a guaranteed no-op here (it
    /// returns before touching `FolderWatcher::start`) — this exercises the
    /// config round-trip and the call path, not the live OS watcher, per the
    /// task's instruction not to unit-test the live watcher through FFI.
    fn test_ctx() -> SparkampCtx {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("GStreamer must be available for tests");
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (duration_tx, duration_rx) = std::sync::mpsc::channel();
        SparkampCtx {
            player: crate::engine::Player::new().expect("Player::new"),
            playlist: crate::model::Playlist::new(),
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

    #[test]
    fn watch_folders_round_trips_and_rebuild_is_a_safe_noop_without_ml() {
        let mut ctx = test_ctx();
        // Default is true (MediaLibraryConfig::default_watch_folders).
        assert!(unsafe { sparkamp_get_watch_folders(&ctx) });
        unsafe { sparkamp_set_watch_folders(&mut ctx, false) };
        assert!(!unsafe { sparkamp_get_watch_folders(&ctx) });
        assert!(ctx.watch.is_none());
        assert!(ctx.watch_rx.is_none());
        unsafe { sparkamp_set_watch_folders(&mut ctx, true) };
        assert!(unsafe { sparkamp_get_watch_folders(&ctx) });
        // media_library is None, so rebuild_watcher must still leave both
        // fields cleared rather than attempting to start a real watcher.
        assert!(ctx.watch.is_none());
        assert!(ctx.watch_rx.is_none());
    }

    #[test]
    fn auto_add_played_round_trips() {
        let mut ctx = test_ctx();
        assert!(!unsafe { sparkamp_get_auto_add_played(&ctx) });
        unsafe { sparkamp_set_auto_add_played(&mut ctx, true) };
        assert!(unsafe { sparkamp_get_auto_add_played(&ctx) });
    }

    #[test]
    fn artist_as_album_artist_round_trips() {
        let mut ctx = test_ctx();
        assert!(!unsafe { sparkamp_get_artist_as_album_artist(&ctx) });
        unsafe { sparkamp_set_artist_as_album_artist(&mut ctx, true) };
        assert!(unsafe { sparkamp_get_artist_as_album_artist(&ctx) });
    }

    #[test]
    fn skip_db_load_round_trips() {
        let mut ctx = test_ctx();
        assert!(!unsafe { sparkamp_get_skip_db_load(&ctx) });
        unsafe { sparkamp_set_skip_db_load(&mut ctx, true) };
        assert!(unsafe { sparkamp_get_skip_db_load(&ctx) });
    }

    #[test]
    fn remove_missing_on_rescan_round_trips() {
        let mut ctx = test_ctx();
        assert!(!unsafe { sparkamp_get_remove_missing_on_rescan(&ctx) });
        unsafe { sparkamp_set_remove_missing_on_rescan(&mut ctx, true) };
        assert!(unsafe { sparkamp_get_remove_missing_on_rescan(&ctx) });
    }

    #[test]
    fn compact_on_rescan_round_trips() {
        let mut ctx = test_ctx();
        assert!(!unsafe { sparkamp_get_compact_on_rescan(&ctx) });
        unsafe { sparkamp_set_compact_on_rescan(&mut ctx, true) };
        assert!(unsafe { sparkamp_get_compact_on_rescan(&ctx) });
    }

    #[test]
    fn rescan_on_startup_round_trips() {
        let mut ctx = test_ctx();
        assert!(!unsafe { sparkamp_get_rescan_on_startup(&ctx) });
        unsafe { sparkamp_set_rescan_on_startup(&mut ctx, true) };
        assert!(unsafe { sparkamp_get_rescan_on_startup(&ctx) });
        unsafe { sparkamp_set_rescan_on_startup(&mut ctx, false) };
        assert!(!unsafe { sparkamp_get_rescan_on_startup(&ctx) });
    }

    #[test]
    fn folder_recurse_defaults_true_without_ml() {
        let ctx = test_ctx();
        let path = std::ffi::CString::new("/no/such/folder").unwrap();
        assert!(unsafe {
            crate::ffi::media_library::sparkamp_ml_folder_recurse(&ctx, path.as_ptr())
        });
    }

    #[test]
    fn poll_watch_event_returns_null_without_a_running_watcher() {
        let mut ctx = test_ctx();
        let mut kind: c_int = -1;
        let out = unsafe {
            crate::ffi::media_library::sparkamp_ml_poll_watch_event(&mut ctx, &mut kind)
        };
        assert!(out.is_null());
    }

    #[test]
    fn play_deadline_null_ctx_is_negative() {
        unsafe {
            assert_eq!(sparkamp_play_deadline_secs(std::ptr::null(), 200.0), -1.0);
        }
    }

    #[test]
    fn play_deadline_disabled_is_negative() {
        let mut ctx = test_ctx();
        unsafe { sparkamp_set_play_stats_enabled(&mut ctx, false) };
        assert_eq!(unsafe { sparkamp_play_deadline_secs(&ctx, 200.0) }, -1.0);
    }

    #[test]
    fn play_stats_enabled_round_trips() {
        let mut ctx = test_ctx();
        assert!(unsafe { sparkamp_get_play_stats_enabled(&ctx) });
        unsafe { sparkamp_set_play_stats_enabled(&mut ctx, false) };
        assert!(!unsafe { sparkamp_get_play_stats_enabled(&ctx) });
    }

    #[test]
    fn play_stats_mode_round_trips() {
        let mut ctx = test_ctx();
        assert_eq!(unsafe { sparkamp_get_play_stats_mode(&ctx) }, 0);
        unsafe { sparkamp_set_play_stats_mode(&mut ctx, 1) };
        assert_eq!(unsafe { sparkamp_get_play_stats_mode(&ctx) }, 1);
        unsafe { sparkamp_set_play_stats_mode(&mut ctx, 0) };
        assert_eq!(unsafe { sparkamp_get_play_stats_mode(&ctx) }, 0);
    }

    #[test]
    fn play_stats_seconds_round_trips() {
        let mut ctx = test_ctx();
        assert_eq!(unsafe { sparkamp_get_play_stats_seconds(&ctx) }, 20);
        unsafe { sparkamp_set_play_stats_seconds(&mut ctx, 45) };
        assert_eq!(unsafe { sparkamp_get_play_stats_seconds(&ctx) }, 45);
    }

    #[test]
    fn play_stats_percent_round_trips() {
        let mut ctx = test_ctx();
        assert_eq!(unsafe { sparkamp_get_play_stats_percent(&ctx) }, 50);
        unsafe { sparkamp_set_play_stats_percent(&mut ctx, 75) };
        assert_eq!(unsafe { sparkamp_get_play_stats_percent(&ctx) }, 75);
    }

    #[test]
    fn remember_search_round_trips() {
        let mut ctx = test_ctx();
        assert!(!unsafe { sparkamp_get_remember_search(&ctx) });
        unsafe { sparkamp_set_remember_search(&mut ctx, true) };
        assert!(unsafe { sparkamp_get_remember_search(&ctx) });
    }

    #[test]
    fn last_search_round_trips_per_view_and_defaults_to_empty() {
        let mut ctx = test_ctx();
        let files = std::ffi::CString::new("files").unwrap();
        let playlists = std::ffi::CString::new("playlists").unwrap();

        // Unset view id returns an empty (non-null) string.
        unsafe {
            let out = sparkamp_get_last_search(&ctx, files.as_ptr());
            assert!(!out.is_null());
            assert_eq!(std::ffi::CStr::from_ptr(out).to_str().unwrap(), "");
            super::super::sparkamp_free_string(out);
        }

        let query = std::ffi::CString::new("beatles").unwrap();
        unsafe { sparkamp_set_last_search(&mut ctx, files.as_ptr(), query.as_ptr()) };

        unsafe {
            let out = sparkamp_get_last_search(&ctx, files.as_ptr());
            assert_eq!(std::ffi::CStr::from_ptr(out).to_str().unwrap(), "beatles");
            super::super::sparkamp_free_string(out);
        }
        // A different view id is unaffected.
        unsafe {
            let out = sparkamp_get_last_search(&ctx, playlists.as_ptr());
            assert_eq!(std::ffi::CStr::from_ptr(out).to_str().unwrap(), "");
            super::super::sparkamp_free_string(out);
        }
    }

    #[test]
    fn play_deadline_reflects_seconds_mode_config() {
        let mut ctx = test_ctx();
        unsafe { sparkamp_set_play_stats_seconds(&mut ctx, 20) };
        assert_eq!(unsafe { sparkamp_play_deadline_secs(&ctx, 200.0) }, 20.0);
        // Unknown length (<= 0) still uses the raw seconds threshold.
        assert_eq!(unsafe { sparkamp_play_deadline_secs(&ctx, 0.0) }, 20.0);
    }
}


#[cfg(test)]
mod time_mode_tests {
    use super::*;

    /// The time counter's mode crosses the boundary as a bool and round-trips
    /// through the config, so the macOS player can restore what the user left.
    #[test]
    fn the_time_mode_round_trips_through_the_ffi() {
        let mut cfg = crate::config::Config::default();
        assert!(!cfg.display.show_remaining());
        cfg.display.set_show_remaining(true);
        assert_eq!(cfg.display.time_mode, "remaining");

        // A null context answers with the default rather than dereferencing.
        assert!(!unsafe { sparkamp_get_show_remaining(std::ptr::null()) });
        unsafe { sparkamp_set_show_remaining(std::ptr::null_mut(), true) };
    }
}
