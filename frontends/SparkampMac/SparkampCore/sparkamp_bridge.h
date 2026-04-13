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

/* ── Visualizer mode ─────────────────────────────────────────────────────── */

/** Return current viz mode: 0 = Bars, 1 = Waveform. */
int32_t sparkamp_get_viz_mode(const SparkampCtx *ctx);
/** Set viz mode: 0 = Bars, 1 = Waveform. */
void    sparkamp_set_viz_mode(SparkampCtx *ctx, int32_t mode);
/** Cycle Bars → Waveform → Bars → … */
void    sparkamp_cycle_viz_mode(SparkampCtx *ctx);

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

#endif /* sparkamp_bridge_h */
