# Phase 10 — F11 Play Threshold + F12 Niceties (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. Small settings cluster; F11
> closes the long-standing "20 s hardcode vs 50% spec" thread.

## F11 — configurable play-count threshold

- Today: a hardcoded 20-second timer marks a track "played" (feeds
  `record_play`). Locate it in engine/controller (search for the 20s
  constant near the play-count logic).
- Config (`playback.play_stats`): `enabled: bool = true`;
  `mode: Seconds | Percent = Seconds`; `seconds: u32 = 20`;
  `percent: u8 = 50`. Winamp exposes both; Sparkamp: radio picks ONE
  active mode (matches todo "whichever the user picks").
- Core decision fn `play_counted_at(length_secs, cfg) -> Option<f64>`
  (None when disabled; seconds mode → min(seconds, length) — a 15 s jingle
  still counts at its end? Winamp counts if you reach the threshold OR
  file end; propose: threshold clamped to length × 0.9 so short files
  count near their end); percent mode → length × pct. Pure, table-tested.
- Timer arms from the track-start seam (phase 2) with the computed
  deadline; `enabled=false` → no record_play at all (snapshot stats still
  read fine).
- Settings UI: Playback tab group (GTK + mac FFI get/set pairs + TUI
  where its settings reach).

## F12 — options niceties

1. **Remember search query per view** (`media_library.remember_search:
   bool = false` + persisted `last_search: HashMap<String,String>` keyed
   by view id — files/playlists/devices/…): ON → restore the search entry
   + filter when a view opens; OFF → clear as today. Persist on change
   (debounced) or on close — follow the config-save idiom of the ML window.
2. **Treat artist as album artist** (`media_library.artist_as_album_artist:
   bool = false`): display/grouping fallback — where album_artist is
   consulted (ml columns, A4 grouping later, lib_track_display's
   album-artist fallback), empty album_artist uses artist when ON. Core
   helper `effective_album_artist(track, cfg) -> &str` so all surfaces
   agree; A4 (phase 11) MUST use it.
3. **Skip database load at startup** (`media_library.skip_db_load:
   bool = false`): ON → don't open/load the ML DB until the ML window (or
   a feature needing it — device sync, watcher) first demands it; play
   paths that opportunistically read the DB (snapshot stats, auto-add)
   handle the not-yet-open case as None/skip. Startup time win for
   non-ML users.

## Automated tests

- `play_counted_at` table: disabled → None; seconds normal/clamped-short;
  percent 50 of 200 s = 100 s; percent of unknown length → fallback to
  seconds mode (decide + test).
- Threshold wiring: simulated playback reaching/missing the deadline →
  record_play called / not (drive the timer logic headlessly if the seam
  allows; else test the deadline computation + a thin integration).
- Search persistence: set → reopen view state → restored (config
  roundtrip level); OFF → absent.
- `effective_album_artist` table: both set, only artist, neither, toggle
  off/on.
- skip_db_load: with flag ON, code paths touching the DB pre-open return
  gracefully (unit the accessor's None branch).

## Manual test plan

1. Seconds=5: skip a track at 3 s → count unchanged; at 6 s → +1 exactly
   once per play. Percent=50 on a 4-min track → counts only past 2:00.
2. Stats toggle OFF → counts/last-played frozen; A1 panel still shows old
   values.
3. Remember-search ON: type in Files view, close/reopen ML → query + filter
   restored, per-view independent. OFF → clean each open.
4. Album-artist fallback ON: compilation-free library — ML album-artist
   column/grouping shows artist where blank.
5. skip_db_load ON: cold start visibly faster with the big library; first
   ML open loads normally; play-count still works after ML opens; A1
   stats show em-dashes before ML ever opened (acceptable — verify no
   crash).
6. mac + TUI settings walk.

## Open questions

1. Percent-of-unknown-length fallback (no duration probed): fall back to
   seconds mode (proposed) or never-count? Confirm.
2. skip_db_load + watcher (phase 8) interplay: watcher requires the DB —
   propose watcher simply starts on first DB open under this flag. Confirm.
