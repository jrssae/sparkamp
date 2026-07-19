# Phase 12 — F15 View/Search Lyrics (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. User-added feature (2026-07-17),
> scheduled last. Also the planned mitigation for the phase-0 known
> limitation (single-line lyric Entry flattens multi-line USLT — the viewer
> here is read-only and multi-line).

**Goal:** "View/Search Lyrics" on right-click menus across five track-row
surfaces + an affordance in the A1 panel. Saved USLT → read-only lyrics
window in the skin's font/size. No lyrics → default browser on a DuckDuckGo
search for `<artist> - <title> lyrics`.

## Architecture

- **Core decision fn** (new `src/lyrics.rs` or beside tags):
  `lyrics_action(path, meta) -> LyricsAction` where
  `enum LyricsAction { Show(String), Search(String /* url */) }`.
  - Lyric source: the file's USLT via `read_tag_fields(path).lyric`
    (fresh read — ML row may be stale); non-empty → `Show`.
  - Search URL: `https://duckduckgo.com/?q=<urlencoded query>`; query =
    `"{artist} - {title} lyrics"` using the SAME fallback chain as row
    display (artist → album_artist → none; title → filename stem). No
    artist → `"{title} lyrics"`. Pure fn, unit-tested (encoding incl.
    spaces/&/unicode, each fallback branch).
- **Browser open:** GTK — `gtk4::show_uri` / `gio::AppInfo::launch_default_for_uri`
  (whatever the Wikipedia links in phase 2 used — REUSE that helper);
  mac — `NSWorkspace.shared.open`; TUI — best-effort `xdg-open` spawn,
  else display the URL string (capability note).
- **Viewer window (GTK):** new `frontends/gtk/window/lyrics.rs` —
  singleton, titled "Lyrics — {title}", scrollable read-only TextView,
  skin-styled: add a `.lyrics-view` CSS class in `render_gtk_css` using
  the SAME font-family/size vars every other text surface uses (+
  `render_gtk_css_covers_lyrics` test). Esc closes; opening for another
  track replaces content (ID3-editor singleton pattern).
- **Surfaces (menu item "View/Search Lyrics"):**
  1. ML Files view row context menu
  2. Saved-playlist (ML playlist editor) row context menu
  3. Disc view track rows (ripped/audio tracks with files; disc tracks
     without local files → Search path using disc metadata)
  4. Device view rows (device files: fresh USLT read may hit slow MTP —
     read on the existing device IO worker, show spinner-less "Loading…"
     title state; fall back to Search on error)
  5. Active playlist row context menu (menu exists from Send-to work)
  6. A1 panel affordance (small "Lyrics" link/button near the tag column)
  All call the ONE core decision fn — no per-surface logic.
- mac: same menu item on its five surfaces + A1 panel; viewer = simple
  scrollable text window in theme font. Lyric fetch via existing tag FFI
  (`sparkamp_tag_open`/`sparkamp_tag_get("USLT")` — already works) — no
  new FFI expected. TUI: menu/key on its list rows → text screen.

## Automated tests

- `lyrics_action`: file with USLT → Show(text, multi-line preserved);
  without → Search; URL building table (fallback chains, encoding, the
  `-` separator only when artist present).
- Search-query fallback chain mirrors display fallback (reuse/compare
  against `lib_track_display` logic — assert consistency on shared
  fixtures so the two never drift).
- CSS coverage test for `.lyrics-view`.
- Device-path error → Search fallback (unit the decision fn's IO-error
  branch).

## Manual test plan

1. Track with saved multi-line lyrics: every surface's menu opens the
   viewer; line breaks intact; skin font/size matches the app; Esc closes;
   open from another track replaces content; singleton (no window pile).
2. Track without lyrics: browser opens DDG with `artist - title lyrics`;
   blank-artist file → `title lyrics`; artist with `&`/unicode encodes
   correctly.
3. A1 panel affordance works in expanded mode.
4. Disc view: file-backed row shows lyrics; metadata-only row searches.
5. Device row: works on a real MTP device; slow/error path degrades to
   search without hanging UI.
6. Skin switch (light/dark/user skin) restyles the open viewer.
7. mac all-surfaces walk + checklist; TUI text screen.

## Open questions

1. A1 affordance form: text link under the tags (proposed) vs button.
2. Editing lyrics from the viewer (jump to ID3 editor button)? Propose a
   small "Edit in tag editor" link — cheap, closes the loop with the
   phase-0 limitation. Confirm (YAGNI check with user).
