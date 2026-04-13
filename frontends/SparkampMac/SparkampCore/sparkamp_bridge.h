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
void    sparkamp_playlist_clear(SparkampCtx *ctx);
void    sparkamp_playlist_remove(SparkampCtx *ctx, int32_t index);
void    sparkamp_playlist_move(SparkampCtx *ctx, int32_t from, int32_t to);
int32_t sparkamp_playlist_len(const SparkampCtx *ctx);
int32_t sparkamp_playlist_current_index(const SparkampCtx *ctx);
char   *sparkamp_playlist_get_title(const SparkampCtx *ctx, int32_t index);
char   *sparkamp_playlist_get_artist(const SparkampCtx *ctx, int32_t index);
char   *sparkamp_playlist_get_album_artist(const SparkampCtx *ctx, int32_t index);
double  sparkamp_playlist_get_duration(const SparkampCtx *ctx, int32_t index);
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

/* ── Duration probing ────────────────────────────────────────────────────── */

void sparkamp_probe_duration(SparkampCtx *ctx, int32_t index);

/* ── String utilities ────────────────────────────────────────────────────── */

void sparkamp_free_string(char *s);

#endif /* sparkamp_bridge_h */
