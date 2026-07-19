# Phase 11 — A4 Album Gallery (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. Scope was user-confirmed
> 2026-07-19 (no longer "note only"): ML browse-by-album cover grid.

**Goal:** A Media Library view mode showing albums as a grid of cover
thumbnails; clicking an album shows its tracks; double-click plays.

## Scope (decided)

- Grouping: `(album, effective_album_artist)` — MUST use phase 10's
  `effective_album_artist` helper; blank album → tracks grouped under a
  single "(no album)" bucket at the end.
- Grid cell: cover thumb (phase 2's thumbnail cache at a larger size,
  e.g. 160 px), album title, album artist, year (min year of tracks).
  No-art album → the 50%-logo placeholder at thumb size.
- Sort dropdown: Artist / Album / Year (asc; artist default).
- Click album → track list (filtered Files-view table or a simple track
  pane) with the album's tracks in disc/track order; double-click track
  plays (append-or-replace per the existing ML play behavior); an
  "Play album" / "Enqueue album" affordance reuses existing ML actions.
- Entry point: view switcher where the ML sidebar/views live (beside
  Files) — follow the sidebar's existing section pattern.
- mac: LazyVGrid equivalent with the same grouping/sort (grouping crosses
  FFI — see below). TUI: text album list → track drill-down (no art);
  capability parity where reachable.

## Architecture

- **Album query in core** (`src/media_library/queries.rs`):
  `albums_sorted(sort) -> Vec<AlbumGroup { album, album_artist, year:
  Option<i64>, track_count, artwork_path: Option<String> }>` — one SQL
  GROUP BY with representative artwork chosen as the first non-NULL
  artwork_path (MIN/MAX or a correlated pick; deterministic). The
  artist-as-album-artist toggle applies via COALESCE/CASE in SQL or
  post-map with the core helper — keep ONE source of truth with phase 10
  (prefer post-map in Rust using `effective_album_artist` to avoid dual
  logic).
- `album_tracks(album, album_artist) -> Vec<LibTrack>` ordered by
  disc_num, track_num, filename.
- GTK: new `frontends/gtk/window/album_gallery.rs` — `GtkGridView` with a
  ListStore model (recycled cells — REQUIRED for perf; a FlowBox of 3k
  widgets is not acceptable), thumb loaded lazily per bound cell from the
  thumbnail cache (background generation like A2).
- FFI for mac: `sparkamp_ml_album_count/albums(sort)` returning a repr(C)
  array (strings as fixed buffers per the SparkampLibTrack pattern) +
  `sparkamp_ml_album_tracks(...)` reusing the existing track-array
  transport. bridge.h + checklist.

## Automated tests

- Grouping SQL/fn: multi-disc album groups once; same album name by two
  artists = two groups; album_artist fallback honored (toggle on/off);
  blank-album bucket; representative art deterministic; year = min.
- `album_tracks` ordering (disc 2 track 1 after disc 1 track 9; NULL
  track_num last by filename).
- Sort modes ordering.
- Thumbnail-size variant of the cache helper (160 px alongside A2's small
  size — same cache, different size key).
- FFI array roundtrip smoke.

## Manual test plan

1. Gallery renders the real ~36k-track library: scroll is smooth (recycled
   cells, thumbs pop in lazily without blocking), memory sane.
2. Grouping sanity: a known multi-disc album appears once; compilations
   split/merge correctly per the album-artist toggle.
3. Click → correct tracks in disc/track order; double-click plays; play/
   enqueue album actions behave like the equivalent Files-view actions.
4. Sort dropdown reorders correctly.
5. No-art albums show placeholder; art appears after tags/folder images
   added (thumb cache invalidation: re-open view after art change —
   verify `refresh_artwork` bumps the thumb, or note as limitation).
6. mac gallery walk + checklist; TUI album list drill-down.

## Performance notes

- One SQL grouping query, not 36k-row Rust-side grouping per open; cache
  the album list per view-open, refresh on scan-complete events.
- Thumb generation burst on first open: cap concurrent generation (reuse
  A2's background single-worker), placeholder-first rendering.
- Thumb cache invalidation: `refresh_artwork` should delete matching
  `thumbs/<hash>-*` entries when it deletes/replaces cached art (add in
  this phase if A2 didn't).

## Open questions

1. Track pane style: filtered Files table (all columns, familiar) vs
   compact album-specific list — propose filtered Files table. Confirm.
2. Grid cell size fixed 160 px vs zoom control — propose fixed (YAGNI).
