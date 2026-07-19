# Phase 2 — F14 Album Art (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. Expand with superpowers:writing-plans
> at execution; re-verify all anchors. This is the roadmap's headline phase.

**Goal:** Album art reaches playback: expandable now-playing panel on the main
window (A1), standalone art window (A6), inline ML thumbnails (A2), set-art
refinements + mac parity (A5/D14).

## Architecture

Two new core seams built here, both reused by phase 3:

1. **Now-playing-changed notification.** One core registry (in
   `src/controller.rs` or beside `AppState`'s existing `set_track_callback`
   pattern — follow whichever callback idiom `state.rs` already uses) firing
   on: track start (with full metadata), playback state change, track end.
   A1 panel, A6 window, and phase 3's MPRIS/NowPlaying all subscribe.
2. **Play-start snapshot.** At track start, BEFORE `record_play` increments,
   capture `(play_count, last_played)` from the ML row (`None`s when the
   file isn't in the library). Snapshot rides the track-start event payload.
   The existing `record_play` path in `src/media_library/` must not fire
   first — locate the current 20-second play-counted timer (engine/controller)
   and take the snapshot at pipeline start, not at count time.

**Panel data assembly is pure core:** `src/now_playing.rs` (new) builds a
`NowPlayingInfo` struct: every populated ID3 tag (18 TagFields, skip empties),
technical line (reuse `tech_summary` parts — LibTrack when in library, else
`read_track_tags` + `technical_probe` fallback so non-library files still
show data), snapshot stats, artwork path (with folder fallback already
inherited), Wikipedia SEARCH URLs. Frontends only render it.

Wikipedia links: `https://en.wikipedia.org/wiki/Special:Search?search=<urlencoded>`
for artist and album, only when the field is non-empty. URL building is a
pure fn with tests (encoding, empty-skip).

## A1 — expandable panel (GTK reference implementation)

- Toggle: key `w` + a mode button (btn row pattern like btn_eq/btn_info,
  `mode-btn-active` CSS when expanded), persisted as
  `config.window.player_expanded: bool` (serde default false — classic look
  preserved).
- Expanded state: the MARQUEE area is REPLACED by the art+data panel (art
  left, scrollable tag/tech/stats/links column right); the small visualizer
  STRETCHES larger. Those two regions grow; transport/seek/volume rows
  unchanged. Collapsed = exactly today's layout. Window is
  `resizable(false)` — expansion changes the natural size; verify collapse
  returns to the compact natural size (GTK may need `set_default_size`
  re-kick or a queue_resize; test interactively).
- Art click → opens/focuses A6. No art → placeholder: Sparkamp logo at 50%
  opacity + "No artwork available" label (logo asset: the existing
  `square logo.png` usage in `util.rs` — reuse its loading path).
- Carve-out: new `frontends/gtk/window/now_playing.rs` owns panel
  construction + subscription; `player.rs` only hosts the swap container
  and the toggle.
- mac: same panel in `PlayerWindow.swift` (marquee swap + viz stretch),
  persisted via the model's existing settings channel. New FFI needed for
  the snapshot/track-event payload — design one
  `sparkamp_now_playing_info(ctx) -> struct` (repr(C), bridge.h mirrored)
  polled on the existing mac track-change notification rather than a new
  callback bridge, unless Swift already observes track changes cleanly
  (it does — `currentIndex` publishes; piggyback).
- TUI: data-as-text screen or section (no art) — capability note; reuse
  `NowPlayingInfo`.

## A6 — standalone art window

- SINGLETON like every other window (toggle/open focuses existing — user
  decision). Resizable, cover only, follows every track change (subscribe
  to the seam), placeholder identical to A1's.
- Open: click A1 art OR key `k`. While focused, main shortcuts still work:
  route its EventControllerKey through the shared `handle_key` (exact
  pattern: the shortcuts window at `player.rs` — Esc handled locally, all
  else delegated).
- GTK: new `frontends/gtk/window/art_window.rs`. mac:
  `ArtworkWindow.swift` ALREADY EXISTS — read it first; it may be the old
  image viewer. Extend/replace to A6 spec (follow-track + singleton +
  placeholder), don't duplicate.
- `w` + `k` join the shortcuts dialog + mac help + mac handler (3-file rule).

## A2 — inline ML thumbnails

- GTK: the `artwork_path` column cell renders a small thumbnail
  (`gtk4::Picture`/`Image` from file) instead of the "View" text link.
  Keep click → viewer behavior on the thumbnail.
- PERFORMANCE (36k rows): never load full images per row. Scaled thumbnail
  cache: `~/.cache/sparkamp/thumbs/<hash>-<size>.png`, generated lazily
  on first display (background, like the metadata pass), cell shows
  placeholder-blank until ready. Core helper `thumb_path_for(artwork_path,
  px) -> Option<PathBuf>` in `src/` (pure-ish, testable: generates once,
  reuses cache).
- mac: add an art column to the ML table (has none — D-gap). Thumbnail via
  the same cache path crossing FFI (add `artwork_path`/thumb path to the
  ML row struct if not already crossing — check `SparkampLibTrack`).

## A5 + D14 — set-art refinements

- APIC picture type: verify `write_tag_fields` embeds as
  `PictureType::CoverFront` (phase-0 code already does — confirm, then
  this sub-item is a no-op).
- "Also write folder image" on embed: a checkbox in the GTK art-browse row
  (unchecked default) writing a `cover.<ext>` beside the file when embedding.
  Config-persisted last state optional — keep simple: plain checkbox.
- D14 mac parity: browse/embed/clear embedded art from the mac ID3 editor.
  FFI: `sparkamp_tag_set_artwork(ctx, path)` + `sparkamp_tag_clear_artwork(ctx)`
  operating on the tag ctx's `fields.artwork_path` (empty string = clear —
  mirrors GTK's entry semantics); save flows through existing
  `sparkamp_tag_save`. Two new symbols → bridge.h.

## Automated tests

- Snapshot: unit — track with (count=5, last=T1): snapshot taken at start
  shows (5, T1) while post-`record_play` row shows (6, T2). Not-in-library
  → (None, None). Drive via the core hook, not UI.
- `NowPlayingInfo` assembly: populated-tags-only filtering; technical
  fallback for non-library file (probe path); wiki URLs (encoding of
  "AC/DC", empty artist → no link).
- Thumbnail cache: generates file once, second call reuses (mtime same);
  bad image → None, no panic.
- A6/A1 placeholder decision fn (art path present/absent → variant).
- Skin CSS: new selectors for panel + art window get
  `render_gtk_css_covers_*` tests.
- FFI: set/clear artwork roundtrip through tag ctx (embed file, reopen,
  picture present; clear, reopen, gone).

## Manual test plan (user GTK pass)

1. `w` toggles expand; state survives restart; collapsed = pixel-identical
   classic layout; viz visibly larger expanded.
2. Panel shows only populated tags; technical line matches ID3 window;
   play count/last-played show PRE-play values and don't tick mid-song.
3. Artist/album links open browser on Wikipedia search; absent fields → no
   link rows.
4. Art click and `k` open ONE A6 window (repeat presses focus it); resizes;
   follows track changes incl. art→no-art (placeholder w/ 50% logo).
5. With A6 focused: z/x/c/v/b/j/i/f all work.
6. ML artwork column shows thumbnails; scroll a large view — no jank;
   click still opens viewer.
7. Embed art with "also write folder image" → cover.jpg appears beside
   file; embed GIF → correct render.
8. TUI: now-playing data text present.
9. Shortcuts dialog lists w + k.
mac checklist: all of the above minus TUI, plus D14 browse/embed/clear.

## Performance notes

Thumb generation on the metadata-pass thread pattern; panel updates on
track-change only (no per-tick work except position if shown — spec shows
static length, not live position). Image loads off the main thread where
GTK allows (gdk Texture from file is fine at thumb sizes).

## Open questions (resolve with user before expanding)

1. Panel tag column: fixed curated order (title/artist/album/… then rest)
   — propose yes; confirm.
2. A1 art size / expanded window target size — propose art ~200px, let the
   window take its natural expanded height; confirm during first
   interactive round (expect one iteration loop on layout taste).
3. "Also write folder image": file name `cover.jpg` vs `folder.jpg` —
   propose cover.<original ext>.
