/*
 * sparkamp_bridge.h — Objective-C bridging header for SparkampMac.
 *
 * The full C API is inlined here so Xcode needs no extra include-path
 * configuration to find it. Keep this file in sync with include/sparkamp.h
 * and src/ffi.rs whenever FFI functions are added or changed.
 */

#ifndef sparkamp_bridge_h
#define sparkamp_bridge_h

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

/* ── Plugin ABI version constants ────────────────────────────────────────── */
#define SPARKAMP_FILETYPE_ABI_VERSION 1
#define SPARKAMP_PLUGIN_ABI_VERSION   2
#define SPARKAMP_VIZ_ABI_VERSION      1

/* ── Opaque context ──────────────────────────────────────────────────────── */

typedef struct SparkampCtx SparkampCtx;

/* ── Lifecycle ───────────────────────────────────────────────────────────── */

SparkampCtx *sparkamp_create(void);
void         sparkamp_destroy(SparkampCtx *ctx);
void         sparkamp_tick(SparkampCtx *ctx);

/* ── Playback ────────────────────────────────────────────────────────────── */

void    sparkamp_load_and_play(SparkampCtx *ctx, const char *uri);
void    sparkamp_play(SparkampCtx *ctx);
void    sparkamp_pause(SparkampCtx *ctx);
void    sparkamp_stop(SparkampCtx *ctx);
void    sparkamp_seek(SparkampCtx *ctx, double fraction);
void    sparkamp_set_volume(SparkampCtx *ctx, double vol);
double  sparkamp_get_volume(const SparkampCtx *ctx);
double  sparkamp_get_position(const SparkampCtx *ctx);
double  sparkamp_get_duration(const SparkampCtx *ctx);
int32_t sparkamp_get_state(const SparkampCtx *ctx);

/* ── Playlist ────────────────────────────────────────────────────────────── */

void    sparkamp_playlist_add(SparkampCtx *ctx, const char *path);
/** Fast add — uses filename as placeholder; call scan_metadata + probe_duration after.
 *  Returns the new track's playlist index, or -1 on failure. */
int32_t sparkamp_playlist_add_fast(SparkampCtx *ctx, const char *path);
void    sparkamp_playlist_clear(SparkampCtx *ctx);
void    sparkamp_playlist_remove(SparkampCtx *ctx, int32_t index);
void    sparkamp_playlist_move(SparkampCtx *ctx, int32_t from, int32_t to);
int32_t sparkamp_playlist_len(const SparkampCtx *ctx);
int32_t sparkamp_playlist_current_index(const SparkampCtx *ctx);
char   *sparkamp_playlist_get_title(const SparkampCtx *ctx, int32_t index);
char   *sparkamp_playlist_get_artist(const SparkampCtx *ctx, int32_t index);
char   *sparkamp_playlist_get_album_artist(const SparkampCtx *ctx, int32_t index);
double  sparkamp_playlist_get_duration(const SparkampCtx *ctx, int32_t index);
/** Mark track at index as broken; call before advancing on a playback error. */
void    sparkamp_playlist_mark_broken(SparkampCtx *ctx, int32_t index);
int32_t sparkamp_playlist_is_broken(const SparkampCtx *ctx, int32_t index);
/** Returns 1 if the file at playlist index is read-only on disk, 0 otherwise. */
int32_t sparkamp_playlist_is_read_only(const SparkampCtx *ctx, int32_t index);
void    sparkamp_playlist_jump(SparkampCtx *ctx, int32_t index);

/* ── Navigation ──────────────────────────────────────────────────────────── */

void sparkamp_nav_next(SparkampCtx *ctx);
void sparkamp_nav_prev(SparkampCtx *ctx);
/** Advance after EOS, respecting RepeatMode::Song and broken-track skipping. */
void sparkamp_advance_after_eos(SparkampCtx *ctx);

/* ── Repeat / Shuffle ────────────────────────────────────────────────────── */

int32_t sparkamp_get_repeat_mode(const SparkampCtx *ctx);
void    sparkamp_cycle_repeat(SparkampCtx *ctx);
int32_t sparkamp_get_shuffle(const SparkampCtx *ctx);
void    sparkamp_toggle_shuffle(SparkampCtx *ctx);

/* ── Config persistence ──────────────────────────────────────────────────── */

void sparkamp_save_config(SparkampCtx *ctx);
void sparkamp_load_config(SparkampCtx *ctx);

/* ── Callbacks ───────────────────────────────────────────────────────────── */

void sparkamp_set_eos_callback(
    SparkampCtx *ctx,
    void (*cb)(void *userdata),
    void *userdata);

void sparkamp_set_error_callback(
    SparkampCtx *ctx,
    void (*cb)(void *userdata, const char *error),
    void *userdata);

void sparkamp_set_position_callback(
    SparkampCtx *ctx,
    void (*cb)(void *userdata, double position, double duration),
    void *userdata);

/* ── Background metadata scanning ───────────────────────────────────────── */

/** Scan full ID3/Vorbis tags for track at index on a Rayon thread.
 *  Results are applied by the next sparkamp_tick call.
 *  Call immediately after sparkamp_playlist_add for each new track. */
void    sparkamp_scan_metadata(SparkampCtx *ctx, int32_t index);

/** Return and reset the count of playlist items updated since the last call.
 *  Non-zero means at least one title/artist/duration changed — refresh UI. */
int32_t sparkamp_take_playlist_dirty_count(SparkampCtx *ctx);

/* ── Duration probing ────────────────────────────────────────────────────── */

void sparkamp_probe_duration(SparkampCtx *ctx, int32_t index);

/* ── Visualizer data ─────────────────────────────────────────────────────── */

/** Fill `out` with `len` spectrum display-band amplitudes (0–1). */
void    sparkamp_get_spectrum(const SparkampCtx *ctx, float *out, int32_t len);
/** Return the configured number of spectrum display bands. */
int32_t sparkamp_get_spectrum_bands(const SparkampCtx *ctx);
/** Fill `out` with `len` waveform PCM samples in [-1, 1]. */
void    sparkamp_get_waveform(const SparkampCtx *ctx, float *out, int32_t len);

/** Render one frame of the Granite plasma visualizer into `out` (RGBA8,
 *  exactly w*h*4 bytes). The renderer keeps a previous-frame buffer between
 *  calls so the Geiss-style feedback trail builds up. Pass consistent (w, h);
 *  changing them resets the trail. Safe to call while paused — buffer fades. */
void    sparkamp_render_granite(SparkampCtx *ctx, uint8_t *out,
                                uint32_t w, uint32_t h);

/* ── Granite plasma settings (speed / palette / feedback) ───────────────── */

/** Granite animation speed multiplier (0.1–5.0). */
float   sparkamp_get_granite_speed(const SparkampCtx *ctx);
void    sparkamp_set_granite_speed(SparkampCtx *ctx, float speed);

/** Granite palette: 0 = Granite, 1 = Fire, 2 = Neon. */
int32_t sparkamp_get_granite_palette(const SparkampCtx *ctx);
void    sparkamp_set_granite_palette(SparkampCtx *ctx, int32_t palette);

/** Granite feedback strength (0.0–0.9). Higher = stronger trail. */
float   sparkamp_get_granite_feedback(const SparkampCtx *ctx);
void    sparkamp_set_granite_feedback(SparkampCtx *ctx, float fb);

/** Granite effect: 0=Plasma, 1=Tunnel, 2=Swirl, 3=RadialSweep, 4=Cells.
 *  When auto-switch is on, the get returns the live scheduler state. */
int32_t sparkamp_get_granite_effect(const SparkampCtx *ctx);
void    sparkamp_set_granite_effect(SparkampCtx *ctx, int32_t effect);

/** Granite auto-switch toggle. When on, the scheduler rotates effects
 *  every 12–24 s with a one-second crossfade. */
bool    sparkamp_get_granite_auto_switch(const SparkampCtx *ctx);
void    sparkamp_set_granite_auto_switch(SparkampCtx *ctx, bool on);

/* ── Visualizer mode ─────────────────────────────────────────────────────── */

/** Return current viz mode: 0 = Bars, 1 = Waveform, 2 = Granite. */
int32_t sparkamp_get_viz_mode(const SparkampCtx *ctx);
/** Set viz mode: 0 = Bars, 1 = Waveform, 2 = Granite. */
void    sparkamp_set_viz_mode(SparkampCtx *ctx, int32_t mode);
/** Cycle Bars → Waveform → Granite → Bars → … */
void    sparkamp_cycle_viz_mode(SparkampCtx *ctx);
/** Return whether bars mirror mode is on (bar extends above+below center). */
bool    sparkamp_get_viz_mirror(const SparkampCtx *ctx);
/** Set bars mirror mode. true = mirrored, false = grow from bottom. */
void    sparkamp_set_viz_mirror(SparkampCtx *ctx, bool mirror);

/* ── Waveform style ──────────────────────────────────────────────────────── */

/** Return waveform style: 0 = Lines, 1 = Filled. */
int32_t sparkamp_get_waveform_style(const SparkampCtx *ctx);
/** Set waveform style: 0 = Lines, 1 = Filled. */
void    sparkamp_set_waveform_style(SparkampCtx *ctx, int32_t style);

/* ── Bars zone config ────────────────────────────────────────────────────── */

/** Return the number of color zones for the bars visualizer (1–6). */
int32_t sparkamp_get_viz_zones(const SparkampCtx *ctx);
/** Set the number of color zones for the bars visualizer. */
void    sparkamp_set_viz_zones(SparkampCtx *ctx, int32_t count);
/** Return hex color for bars zone `zone_index` (0 = bottom). Free with sparkamp_free_string. */
char   *sparkamp_get_zone_color(const SparkampCtx *ctx, int32_t zone_index);
/** Set hex color for bars zone `zone_index`. */
void    sparkamp_set_zone_color(SparkampCtx *ctx, int32_t zone_index, const char *hex);

/* ── Waveform zone config ────────────────────────────────────────────────── */

/** Return the number of color zones for the waveform visualizer (1–6). */
int32_t sparkamp_get_waveform_zones(const SparkampCtx *ctx);
/** Set the number of color zones for the waveform visualizer. */
void    sparkamp_set_waveform_zones(SparkampCtx *ctx, int32_t count);
/** Return hex color for waveform zone `zone_index` (0 = bottom). Free with sparkamp_free_string. */
char   *sparkamp_get_waveform_zone_color(const SparkampCtx *ctx, int32_t zone_index);
/** Set hex color for waveform zone `zone_index`. */
void    sparkamp_set_waveform_zone_color(SparkampCtx *ctx, int32_t zone_index, const char *hex);

/* ── String utilities ────────────────────────────────────────────────────── */

void sparkamp_free_string(char *s);

// ---------------------------------------------------------------------------
// Equalizer
// ---------------------------------------------------------------------------
bool    sparkamp_has_eq(SparkampCtx *ctx);
bool    sparkamp_get_eq_enabled(SparkampCtx *ctx);
void    sparkamp_set_eq_enabled(SparkampCtx *ctx, bool enabled);
float   sparkamp_get_eq_band(SparkampCtx *ctx, int band);
void    sparkamp_set_eq_band(SparkampCtx *ctx, int band, float db);
void    sparkamp_apply_eq_preset(SparkampCtx *ctx, int preset_index);
int     sparkamp_eq_preset_count(SparkampCtx *ctx);
char   *sparkamp_eq_preset_name(SparkampCtx *ctx, int preset_index);
float   sparkamp_get_preamp(SparkampCtx *ctx);
void    sparkamp_set_preamp(SparkampCtx *ctx, float multiplier);
void    sparkamp_reset_eq(SparkampCtx *ctx);
char   *sparkamp_eq_band_label(int band);

/* EQ / pre-amp limit constants — mirror core's clamp ranges so frontends
 * don't have to hardcode the same numeric ranges and risk drift. */
double  sparkamp_eq_band_db_limit(void);
double  sparkamp_preamp_min(void);
double  sparkamp_preamp_max(void);

/* Audio extension whitelist — canonical list from core; use for file pickers
 * to avoid drift.  Strings are static lowercase ASCII without leading dot
 * (e.g. "mp3"), valid for process lifetime, must not be freed.  Returns
 * NULL when idx is out of range. */
int          sparkamp_audio_extension_count(void);
const char  *sparkamp_audio_extension(int idx);

// ---------------------------------------------------------------------------
// Settings / Behavior
// ---------------------------------------------------------------------------
int     sparkamp_get_playlist_add_behavior(SparkampCtx *ctx);
void    sparkamp_set_playlist_add_behavior(SparkampCtx *ctx, int value);
bool    sparkamp_get_autoplay_on_add(SparkampCtx *ctx);
void    sparkamp_set_autoplay_on_add(SparkampCtx *ctx, bool value);
int     sparkamp_get_ml_rescan_interval(SparkampCtx *ctx);
void    sparkamp_set_ml_rescan_interval(SparkampCtx *ctx, int mins);

// ---------------------------------------------------------------------------
// Playlist path
// ---------------------------------------------------------------------------
char   *sparkamp_playlist_get_path(SparkampCtx *ctx, int index);

// ---------------------------------------------------------------------------
// ID3 Tag Editor
// ---------------------------------------------------------------------------
typedef struct SparkampTagCtx SparkampTagCtx;
SparkampTagCtx *sparkamp_tag_open(const char *path);
void            sparkamp_tag_close(SparkampTagCtx *tag);
char           *sparkamp_tag_get(SparkampTagCtx *tag, const char *frame_id);
void            sparkamp_tag_set(SparkampTagCtx *tag, const char *frame_id, const char *value);
int             sparkamp_tag_frame_count(SparkampTagCtx *tag);
char           *sparkamp_tag_frame_id(SparkampTagCtx *tag, int index);
char           *sparkamp_tag_frame_value(SparkampTagCtx *tag, int index);
int             sparkamp_tag_save(SparkampTagCtx *tag);
uint8_t        *sparkamp_tag_get_artwork_data(SparkampTagCtx *tag, int *len_out);
void            sparkamp_tag_free_artwork(uint8_t *ptr, int len);

// ---------------------------------------------------------------------------
// Media Library
// ---------------------------------------------------------------------------

/** Track row returned from the media library.  All strings are null-terminated UTF-8. */
typedef struct {
    int64_t  id;
    uint8_t  path[512];
    uint8_t  title[256];
    uint8_t  artist[256];
    uint8_t  album[256];
    uint8_t  genre[64];
    int32_t  year;
    int32_t  track_num;
    double   length_secs;
    int32_t  bitrate;
    int32_t  play_count;
    int32_t  scanned;        /* 1 = full metadata read; 0 = filename only */
    uint8_t  album_artist[256];
    int32_t  disc_num;
    uint8_t  bpm[32];
    uint8_t  comment[512];
    uint8_t  composer[256];
    int32_t  read_only;       /* 1 = file is read-only on disk */
    int32_t  has_art;         /* 1 = cached album art exists */
    int32_t  file_missing;    /* 1 = file does not exist at recorded path */
    uint8_t  last_played[32]; /* ISO-8601 UTC ("YYYY-MM-DDTHH:MM:SSZ") or empty */
} SparkampLibTrack;

/** Open (or create) the media library DB.  Must be called before any sparkamp_ml_* function. */
void    sparkamp_ml_open(SparkampCtx *ctx);

/** Number of watched folders (0 if ML not open). */
int32_t sparkamp_ml_folder_count(const SparkampCtx *ctx);
/** Path of folder at index.  Caller frees with sparkamp_free_string.  Returns NULL on error. */
char   *sparkamp_ml_folder_path(const SparkampCtx *ctx, int32_t index);

/**
 * Add a folder and start a two-phase scan.
 * Phase 1 (fast, synchronous): filename-only entries inserted.
 * Phase 2 (background thread): full tag scan; progress_cb fires per file.
 * done_cb fires when complete.  Both may be NULL.
 */
void    sparkamp_ml_add_folder(
    SparkampCtx *ctx,
    const char  *path,
    void (*progress_cb)(void *userdata, int32_t done, int32_t total),
    void (*done_cb)(void *userdata),
    void        *userdata);

/** Remove a watched folder (matched by path) and all its tracks. */
void    sparkamp_ml_remove_folder(SparkampCtx *ctx, const char *path);
void    sparkamp_ml_remove_track(SparkampCtx *ctx, int64_t track_id);

/**
 * Rescan all watched folders.  Same two-phase approach as sparkamp_ml_add_folder.
 * Discovers new files first (fast), then reads tags in background.
 */
void    sparkamp_ml_rescan_all(
    SparkampCtx *ctx,
    void (*progress_cb)(void *userdata, int32_t done, int32_t total),
    void (*done_cb)(void *userdata),
    void        *userdata);

/** Request cancellation of the current background scan. */
void    sparkamp_ml_cancel_scan(SparkampCtx *ctx);
/** Returns 1 while a background scan is running, 0 otherwise. */
int32_t sparkamp_ml_scan_is_running(const SparkampCtx *ctx);
/** Writes current scan progress into *done_out and *total_out. */
void    sparkamp_ml_scan_progress(const SparkampCtx *ctx, int32_t *done_out, int32_t *total_out);

/** Number of tracks matching query ("" = all). */
int32_t sparkamp_ml_track_count(const SparkampCtx *ctx, const char *query);

/**
 * Fetch a page of tracks into caller-allocated array.
 * sort_col: "title"|"artist"|"album"|"duration"|"num"|"year"|"genre"|"bitrate"|"filename" (NULL = default).
 * sort_desc: 1 = descending.
 * Returns number of elements written.
 */
int32_t sparkamp_ml_get_tracks(
    const SparkampCtx *ctx,
    const char        *query,
    const char        *sort_col,
    int32_t            sort_desc,
    int32_t            offset,
    int32_t            limit,
    SparkampLibTrack  *out);

/** Append tracks (by library ID array) to the active playlist. */
void    sparkamp_ml_add_tracks_to_playlist(SparkampCtx *ctx, const int64_t *ids, int32_t count);

/** Number of saved playlists in the library. */
int32_t sparkamp_ml_playlist_count(const SparkampCtx *ctx);
/** Name of saved playlist at index.  Caller frees with sparkamp_free_string. */
char   *sparkamp_ml_playlist_name(const SparkampCtx *ctx, int32_t index);
/** Row ID of saved playlist at index, or -1 on error. */
int64_t sparkamp_ml_playlist_id(const SparkampCtx *ctx, int32_t index);
/** Replace the active playlist with the saved playlist at index. */
void    sparkamp_ml_set_current_playlist(SparkampCtx *ctx, int32_t index);

/** Create a new empty playlist with name.  Returns row id or -1 on failure. */
int64_t sparkamp_ml_create_playlist(SparkampCtx *ctx, const char *name);
/** Delete playlist by row id from the DB (file on disk is kept). */
void    sparkamp_ml_delete_playlist(SparkampCtx *ctx, int64_t playlist_id);
/** Rename playlist by row id; also renames the .m3u file on disk. */
void    sparkamp_ml_rename_playlist(SparkampCtx *ctx, int64_t playlist_id, const char *new_name);
/** Overwrite playlist .m3u with the given track IDs (in order). */
void    sparkamp_ml_save_playlist(SparkampCtx *ctx, int64_t playlist_id,
                                  const int64_t *track_ids, int32_t count);
/** Create a new playlist named new_name and write the given raw path strings to it.
    Preserves missing/stub entries verbatim.  Returns new row id or -1 on failure. */
int64_t sparkamp_ml_save_playlist_as(SparkampCtx *ctx, const char *new_name,
                                     const char **paths, int32_t count);
/** Returns 1 if the playlist lives in Sparkamp's managed playlists directory, 0 otherwise. */
int32_t sparkamp_ml_playlist_is_managed(const SparkampCtx *ctx, int64_t playlist_id);
/** Return the .m3u file path of the playlist as a heap string; free with sparkamp_free_string. */
char   *sparkamp_ml_playlist_path(const SparkampCtx *ctx, int64_t playlist_id);
/** Fill buf with up to limit tracks from playlist_id.  Returns count written. */
int32_t sparkamp_ml_get_playlist_tracks(const SparkampCtx *ctx, int64_t playlist_id,
                                        SparkampLibTrack *buf, int32_t limit);

/** Returns 1 if the file at playlist[index] is missing from disk. */
int32_t sparkamp_playlist_file_missing(const SparkampCtx *ctx, int32_t index);

/** Record a play event for the given path (increments play_count, updates last_played). */
void    sparkamp_ml_record_play(SparkampCtx *ctx, const char *path);

// ---------------------------------------------------------------------------
// Deduplication
// ---------------------------------------------------------------------------

typedef struct {
    uint8_t  path[512];
    uint8_t  title[256];
    uint8_t  artist[256];
    double   duration_secs;
} SparkampDedupTrack;

typedef struct {
    int32_t             confidence;    /* 0 = Probable, 1 = Less Likely */
    int32_t             track_count;
    SparkampDedupTrack *tracks;        /* owned by SparkampDedupCtx; do NOT free */
} SparkampDedupGroup;

typedef struct SparkampDedupCtx SparkampDedupCtx;

/**
 * Start a background dedup scan.
 * group_cb fires per group found (pointer valid only during callback — copy data out).
 * done_cb fires with total group count when scan completes.
 * Returns opaque ctx; free with sparkamp_dedup_free.  Returns NULL if ML not open.
 */
SparkampDedupCtx *sparkamp_dedup_start(
    SparkampCtx *ctx,
    void (*group_cb)(void *userdata, const SparkampDedupGroup *group),
    void (*done_cb)(void *userdata, int32_t group_count),
    void *userdata);

void sparkamp_dedup_cancel(SparkampDedupCtx *dedup_ctx);
void sparkamp_dedup_free(SparkampDedupCtx *dedup_ctx);

/** Append all paths to active playlist. paths is a C array of count strings. */
void sparkamp_dedup_add_to_playlist(SparkampCtx *ctx, const char **paths, int32_t count);
/** Replace active playlist with paths. */
void sparkamp_dedup_replace_playlist(SparkampCtx *ctx, const char **paths, int32_t count);

/** Reveal path's containing folder in Finder. */
void sparkamp_open_file_location(const char *path);

#endif /* sparkamp_bridge_h */
