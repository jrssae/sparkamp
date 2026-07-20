# Phase 2 — F14 Album Art (execution plan)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Read `2026-07-19-opus-handoff.md` and `2026-07-19-phase2-album-art.md` (design)
> FIRST. This plan expands that design into step-level TDD tasks with verified
> anchors (git HEAD `f4e2a46`). Re-grep any anchor before editing — earlier tasks
> in THIS plan move lines.

**Goal:** Album art reaches playback — expandable now-playing panel on the main
window (A1), standalone art window (A6), inline ML thumbnails (A2), set-art
refinements + mac parity (A5/D14) — on GTK (reference), mac (blind), and TUI
(text where its surface reaches).

**Architecture:** Pure-core data + path layer in `src/now_playing.rs` and
`media_library` (snapshot, `NowPlayingInfo`, wiki URLs, thumb cache path).
Frontends render only. A GTK-side subscription seam fires now-playing events on
track start / state change / end; the play-start snapshot is captured at pipeline
start BEFORE the existing 20-second `record_play` timer fires. Thumbnails are
generated per-frontend (gdk-pixbuf on GTK, NSImage on mac) into a core-owned
deterministic cache path.

**Tech Stack:** Rust core (rusqlite, symphonia probe, `DefaultHasher`), GTK4
(`gdk-pixbuf`, `gtk4::Picture`), SwiftUI (blind), Ratatui TUI, C FFI (`#[repr(C)]`
+ `sparkamp_bridge.h`).

## Global Constraints

- Build/test ONLY inside distrobox:
  `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`.
  Host builds fail. NEVER gate on `cargo build --lib` (GTK code only compiles in bin).
- New `src/` modules need `mod x;` in BOTH `src/lib.rs` AND `src/main.rs`.
- Zero warnings, zero failures before any "done" claim. Quote BOTH `test result:`
  lines (lib + bin). Floor at plan start: **1061 passed** (per progress ledger,
  post P1 user-pass round 3).
- Config: new fields use `#[serde(default)]` + `Default` impl.
- Keyboard shortcuts sync across THREE files: `player.rs` `sections` array
  (`player.rs:3913`), mac `KeyboardShortcutsView.swift` `sections` (`:22`), mac
  handler (`SparkampModel+Keys.swift` / `SparkampModel.swift`). GTK binds upper +
  lowercase; mac lowercase only. This phase adds `w` and `k`.
- Every new C-visible FFI symbol/struct hand-mirrored byte-for-byte in
  `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`; mac verification items
  appended to `docs/mac-pass-checklist.md` in the SAME commit.
- RefCell borrows short-lived — never held across a UI call, `.await`, or
  `select_row`.
- New skin widget surface → skin selectors in `src/skin.rs::render_gtk_css` + a
  `render_gtk_css_covers_*` test.
- Commits: conventional prefix, body = why + a verification line, trailer
  `Co-Authored-By:` the executing model. Casing "Sparkamp". No push without a
  fresh explicit user instruction.

## Resolved open questions (user, 2026-07-19)

1. Panel tag column order: **curated** — reuse `TagFields::field_pairs()` order
   (`id3_editor.rs:264`), which already leads title/artist/album.
2. A1 art size: **~200px**, window takes natural expanded height. Expect one
   interactive layout iteration.
3. Folder image filename on embed: **`cover.<original ext>`** (cover.* already
   outranks folder.* in the fallback, commit `f4e2a46`).
4. Thumbnail generation: **per-frontend** (gdk-pixbuf / NSImage); core owns the
   deterministic path only. `thumb_path_for` computes + returns the path, does
   NOT decode/resize.

## File structure

- `src/now_playing.rs` (NEW): `NowPlayingInfo`, `build_now_playing_info`,
  `wiki_search_url`, `percent_encode_query`. `mod now_playing;` in lib.rs + main.rs.
- `src/media_library/playlists.rs` (MODIFY): add `PlaySnapshot` + `play_snapshot`.
- `src/media_library/mod.rs` (MODIFY): re-export `PlaySnapshot`; `thumb_path_for`
  lives here beside the existing cache helpers, or in `now_playing.rs` — put it in
  `now_playing.rs` (UI-agnostic, no DB).
- `frontends/gtk/window/now_playing.rs` (NEW): A1 panel construction + subscription.
- `frontends/gtk/window/art_window.rs` (NEW): A6 singleton art window.
- `frontends/gtk/window/state.rs` (MODIFY): now-playing subscriber registry +
  snapshot capture hook.
- `frontends/gtk/window/player.rs` (MODIFY): swap container, `w` toggle, `k` key,
  mode button, shortcuts `sections` entries.
- `frontends/gtk/window/media_library.rs` (MODIFY): artwork column → thumbnail cell.
- `frontends/gtk/window/id3.rs` (MODIFY): "also write folder image" checkbox.
- `frontends/gtk/window/mod.rs` (MODIFY): `mod now_playing; mod art_window;`.
- `src/config.rs` (MODIFY): `WindowConfig.player_expanded: bool`.
- `src/skin.rs` (MODIFY): panel + art-window selectors + tests.
- `src/ffi/id3.rs`, `src/ffi/media_library.rs`, new `src/ffi/now_playing.rs`
  (MODIFY/NEW): artwork set/clear, `sparkamp_now_playing_info`, ML art path.
- `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` + Swift (MODIFY, blind).
- TUI: `frontends/tui/` now-playing text section (locate at execution).

---

## Task 1: Play-start snapshot (core)

Capture `(play_count, last_played)` from the ML row at pipeline start, BEFORE the
20-second `record_play` timer at `player.rs:3021-3037` increments.

**Files:**
- Modify: `src/media_library/playlists.rs` (add beside `record_play` at :656)
- Modify: `src/media_library/mod.rs` (re-export `PlaySnapshot` if not glob-exported)
- Test: `src/media_library/tests.rs` (near the `record_play` tests at :1025)

**Interfaces:**
- Produces: `pub struct PlaySnapshot { pub play_count: Option<i64>, pub last_played: Option<String> }`
  and `impl MediaLibrary { pub fn play_snapshot(&self, path: &str) -> PlaySnapshot }`
  (mirror the receiver type used by `record_play` — same `&self` handle).

- [ ] **Step 1: Write the failing test**

```rust
// in src/media_library/tests.rs, after the record_play tests (~:1100)
#[test]
fn play_snapshot_reads_preplay_values() {
    let (lib, _tmp) = new_test_lib_with_track(); // reuse the record_play tests' setup helper
    let path = test_track_path();               // same helper the record_play tests use
    // Track present, never played.
    let snap0 = lib.play_snapshot(path);
    assert_eq!(snap0.play_count, Some(0));
    assert_eq!(snap0.last_played, None);
    // After a recorded play, the ROW advances but a snapshot taken earlier is stale.
    lib.record_play(path).unwrap();
    let snap1 = lib.play_snapshot(path);
    assert_eq!(snap1.play_count, Some(1));
    assert!(snap1.last_played.is_some());
}

#[test]
fn play_snapshot_none_for_unknown_path() {
    let (lib, _tmp) = new_test_lib_with_track();
    let snap = lib.play_snapshot("/nonexistent/x.mp3");
    assert_eq!(snap.play_count, None);
    assert_eq!(snap.last_played, None);
}
```

> Re-verify the exact setup-helper names by reading `record_play_increments_play_count`
> (`tests.rs:1028`) and copy its construction verbatim — do not invent helper names.

- [ ] **Step 2: Run test to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test -p sparkamp --lib play_snapshot'`
Expected: FAIL — `no method named play_snapshot`.

- [ ] **Step 3: Write minimal implementation**

```rust
// src/media_library/playlists.rs, immediately after record_play (~:666)

/// Pre-play snapshot of a track's play statistics, read at pipeline start
/// BEFORE `record_play` increments.  `None` fields mean the file is not in
/// the library.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaySnapshot {
    pub play_count: Option<i64>,
    pub last_played: Option<String>,
}

impl MediaLibrary {
    /// Read `(play_count, last_played)` for the track at `path` without
    /// mutating it.  Returns an all-`None` snapshot when the path is not in
    /// the library.
    pub fn play_snapshot(&self, path: &str) -> PlaySnapshot {
        self.conn
            .query_row(
                "SELECT play_count, last_played FROM tracks WHERE path = ?1",
                params![path],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map(|(pc, lp)| PlaySnapshot { play_count: Some(pc), last_played: lp })
            .unwrap_or_default()
    }
}
```

> Confirm the `impl` target: `record_play` is `impl MediaLibrary` (or whatever the
> struct at `playlists.rs` is — re-grep `impl .* {` above :656). Reuse the SAME
> struct/receiver. Ensure `PlaySnapshot` is exported from `mod.rs` (`pub use`) if
> the crate uses explicit re-exports.

- [ ] **Step 4: Run tests to verify they pass**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test -p sparkamp --lib play_snapshot'`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/media_library/playlists.rs src/media_library/mod.rs src/media_library/tests.rs
git commit -m "feat(ml): play_snapshot reads pre-play stats at pipeline start"
```

---

## Task 2: Wikipedia search URL builder (core)

Zero-dependency percent-encoding + Special:Search URL, only for non-empty fields.

**Files:**
- Create: `src/now_playing.rs`
- Modify: `src/lib.rs`, `src/main.rs` (add `mod now_playing;` to BOTH)
- Test: inline `#[cfg(test)]` in `src/now_playing.rs`

**Interfaces:**
- Produces: `pub fn wiki_search_url(query: &str) -> Option<String>` and
  `fn percent_encode_query(s: &str) -> String` (private, tested via `wiki_search_url`).

- [ ] **Step 1: Write the failing test**

```rust
// src/now_playing.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_url_encodes_and_wraps() {
        assert_eq!(
            wiki_search_url("AC/DC"),
            Some("https://en.wikipedia.org/wiki/Special:Search?search=AC%2FDC".to_string())
        );
        assert_eq!(
            wiki_search_url("Miles Davis"),
            Some("https://en.wikipedia.org/wiki/Special:Search?search=Miles%20Davis".to_string())
        );
    }

    #[test]
    fn wiki_url_empty_is_none() {
        assert_eq!(wiki_search_url(""), None);
        assert_eq!(wiki_search_url("   "), None);
    }

    #[test]
    fn wiki_url_preserves_unreserved() {
        assert_eq!(
            wiki_search_url("A-B_C.D~E"),
            Some("https://en.wikipedia.org/wiki/Special:Search?search=A-B_C.D~E".to_string())
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test -p sparkamp --lib wiki_url'`
Expected: FAIL — module/function not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Now-playing data assembly — pure, UI-agnostic.  Frontends render the
//! `NowPlayingInfo` this module builds; they compute no metadata of their own.

/// Percent-encode `s` for a URL query value.  Unreserved characters
/// (RFC 3986: A–Z a–z 0–9 `-` `_` `.` `~`) pass through; everything else is
/// `%XX` (spaces become `%20`, not `+`, so `Special:Search` treats them as a
/// literal phrase).
fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Wikipedia Special:Search URL for `query`, or `None` when it is empty or
/// whitespace-only.
pub fn wiki_search_url(query: &str) -> Option<String> {
    if query.trim().is_empty() {
        return None;
    }
    Some(format!(
        "https://en.wikipedia.org/wiki/Special:Search?search={}",
        percent_encode_query(query)
    ))
}
```

Add to `src/lib.rs` and `src/main.rs` (BOTH): `mod now_playing;` (or `pub mod`
in lib.rs if FFI/frontends import it — Task 3/5 import `NowPlayingInfo`, so use
`pub mod now_playing;` in lib.rs).

- [ ] **Step 4: Run tests to verify they pass**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test -p sparkamp --lib wiki_url'`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/now_playing.rs src/lib.rs src/main.rs
git commit -m "feat(core): wikipedia Special:Search URL builder with dependency-free encoding"
```

---

## Task 3: NowPlayingInfo assembly (core)

Build the render struct: populated tags only (curated order), technical line
(library row or probe fallback), snapshot stats, artwork path, wiki URLs.

**Files:**
- Modify: `src/now_playing.rs`
- Test: inline tests in `src/now_playing.rs`

**Interfaces:**
- Consumes: `PlaySnapshot` (Task 1); `TagFields` + `read_track_tags`
  (`tags.rs:81`, `pub(crate)` — may need `pub(crate)`→visible or a wrapper);
  `read_only_track_fields` + `tech_summary` (`media_library/mod.rs:206`, `:318`);
  `technical_probe` (`src/technical_probe.rs`); `wiki_search_url` (Task 2).
- Produces:
  ```rust
  pub struct NowPlayingInfo {
      pub tags: Vec<(&'static str, String)>, // curated, non-empty only
      pub tech_line: String,                 // may be empty
      pub artwork_path: Option<PathBuf>,
      pub play_count: Option<i64>,
      pub last_played: Option<String>,
      pub artist_wiki_url: Option<String>,
      pub album_wiki_url: Option<String>,
  }
  pub fn build_now_playing_info(
      path: &Path,
      lib_row: Option<&LibTrack>,   // Some when in library, else None → probe fallback
      snapshot: PlaySnapshot,
  ) -> NowPlayingInfo
  ```

> Re-verify at execution: the exact `LibTrack` type path/name and the signature
> of `read_only_track_fields` (it takes a `LibTrack`/`ReadOnlyTrackFields` and
> `read_track_tags` for non-library files — trace `read_only_track_fields`'s own
> body at `mod.rs:206-316` for how it fuses library row + probe; reuse that fusion
> so the tech line matches the ID3 window byte-for-byte). Do NOT duplicate probe
> logic — call the existing seam.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn info_keeps_only_populated_tags_in_curated_order() {
    // Build a TagFields with title + artist set, rest empty; drive assembly with
    // a non-library path so the probe fallback runs. First two tags must be
    // ("Title", ...) then ("Artist", ...); empty fields absent.
    let dir = tempfile::tempdir().unwrap();
    let f = write_min_mp3_with_tags(dir.path(), "My Song", "AC/DC"); // helper: tags a tiny mp3
    let info = build_now_playing_info(&f, None, PlaySnapshot::default());
    assert_eq!(info.tags.first(), Some(&("Title", "My Song".to_string())));
    assert!(info.tags.iter().any(|(l, _)| *l == "Artist"));
    assert!(!info.tags.iter().any(|(_, v)| v.is_empty()));
}

#[test]
fn info_builds_wiki_urls_from_artist_and_album() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_min_mp3_with_tags(dir.path(), "S", "AC/DC");
    let info = build_now_playing_info(&f, None, PlaySnapshot::default());
    assert_eq!(
        info.artist_wiki_url.as_deref(),
        Some("https://en.wikipedia.org/wiki/Special:Search?search=AC%2FDC")
    );
    assert_eq!(info.album_wiki_url, None); // album empty → no link
}

#[test]
fn info_carries_snapshot_stats() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_min_mp3_with_tags(dir.path(), "S", "A");
    let snap = PlaySnapshot { play_count: Some(5), last_played: Some("2026-07-01T00:00:00Z".into()) };
    let info = build_now_playing_info(&f, None, snap);
    assert_eq!(info.play_count, Some(5));
    assert_eq!(info.last_played.as_deref(), Some("2026-07-01T00:00:00Z"));
}

#[test]
fn info_tech_line_present_for_probeable_nonlibrary_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_min_mp3_with_tags(dir.path(), "S", "A");
    let info = build_now_playing_info(&f, None, PlaySnapshot::default());
    assert!(!info.tech_line.is_empty()); // probe fallback filled it
}
```

> `write_min_mp3_with_tags` / `tempfile`: check how `tags.rs` / `technical_probe.rs`
> tests fabricate a probeable file. If the suite ships a tiny fixture mp3, point at
> it read-only instead of synthesizing. Reuse the existing fixture pattern; do not
> add a new binary asset without checking for one.

- [ ] **Step 2: Run tests to verify they fail**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test -p sparkamp --lib now_playing::tests'`
Expected: FAIL — `build_now_playing_info` / `NowPlayingInfo` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
use std::path::{Path, PathBuf};
use crate::id3_editor::TagFields;
use crate::media_library::PlaySnapshot;

pub struct NowPlayingInfo {
    pub tags: Vec<(&'static str, String)>,
    pub tech_line: String,
    pub artwork_path: Option<PathBuf>,
    pub play_count: Option<i64>,
    pub last_played: Option<String>,
    pub artist_wiki_url: Option<String>,
    pub album_wiki_url: Option<String>,
}

pub fn build_now_playing_info(
    path: &Path,
    lib_row: Option<&crate::media_library::LibTrack>,
    snapshot: PlaySnapshot,
) -> NowPlayingInfo {
    // 1. Tags: read from disk (authoritative for what's embedded), curated order.
    let fields: TagFields = crate::tags::read_tag_fields_for(path); // see note
    let tags: Vec<(&'static str, String)> = fields
        .field_pairs()
        .into_iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .collect();

    // 2. Technical line: reuse the ID3-window seam so output matches exactly.
    let tech_line = crate::media_library::tech_line_for(path, lib_row); // see note

    // 3. Artwork: library row's path if present, else the disk fallback chain.
    let artwork_path = lib_row
        .and_then(|t| t.artwork_path.clone())
        .or_else(|| crate::tags::read_track_tags(path).artwork_path);

    // 4. Wiki links from artist/album.
    let artist = fields.artist.clone();
    let album = fields.album.clone();

    NowPlayingInfo {
        tags,
        tech_line,
        artwork_path,
        play_count: snapshot.play_count,
        last_played: snapshot.last_played,
        artist_wiki_url: wiki_search_url(&artist),
        album_wiki_url: wiki_search_url(&album),
    }
}
```

> IMPLEMENTATION NOTES — resolve at execution, do not guess:
> - `read_tag_fields_for` / `tech_line_for` are PLACEHOLDER names for existing
>   seams. Trace how the ID3 window builds its `TagFields` and its tech line
>   (`read_only_track_fields` + `tech_summary`, `mod.rs:206/318`; `read_track_tags`,
>   `tags.rs:81`). Call those directly. If `read_track_tags` is `pub(crate)` and
>   `now_playing` is in the same crate, it is already reachable. If a clean
>   "fields from path" helper doesn't exist, factor the ID3 window's existing
>   assembly into a shared fn rather than copy-pasting.
> - `LibTrack.artwork_path` type: confirm `Option<PathBuf>` vs `Option<String>`
>   (`SparkampLibTrack::from_lib_track` at `ffi/media_library.rs:92` reads
>   `t.artwork_path.is_some()`) and adapt the `.or_else` branch.

- [ ] **Step 4: Run tests to verify they pass**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test -p sparkamp --lib now_playing::tests'`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/now_playing.rs src/tags.rs src/media_library/mod.rs
git commit -m "feat(core): NowPlayingInfo assembly — curated tags, tech line, artwork, wiki links"
```

---

## Task 4: Thumbnail cache path (core, path-only)

Deterministic cache path per `(artwork_path, px)`. Core does NOT decode/resize
(per user decision — generation is per-frontend).

**Files:**
- Modify: `src/now_playing.rs`
- Test: inline

**Interfaces:**
- Produces: `pub fn thumb_path_for(artwork_path: &Path, px: u32) -> Option<PathBuf>`
  returning `~/.cache/sparkamp/thumbs/<hash>-<px>.png`; `None` if the home/cache
  dir is unresolvable.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn thumb_path_is_deterministic_and_size_specific() {
    let a = thumb_path_for(Path::new("/music/cover.jpg"), 48).unwrap();
    let b = thumb_path_for(Path::new("/music/cover.jpg"), 48).unwrap();
    let c = thumb_path_for(Path::new("/music/cover.jpg"), 96).unwrap();
    assert_eq!(a, b);            // same inputs → same path
    assert_ne!(a, c);           // size in the filename
    assert!(a.ends_with(&format!("thumbs")).not() && a.to_string_lossy().contains("/thumbs/"));
    assert!(a.extension().unwrap() == "png");
    assert!(a.file_name().unwrap().to_string_lossy().ends_with("-48.png"));
}
```

> `.not()` needs `std::ops::Not` — simplify to `assert!(a.to_string_lossy().contains("/thumbs/"))`.

- [ ] **Step 2: Run test to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test -p sparkamp --lib thumb_path'`
Expected: FAIL — not found.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Deterministic cache path for a `px`-sized thumbnail of `artwork_path`.
/// Frontends generate the PNG here on first display (gdk-pixbuf / NSImage);
/// core only owns the path so every frontend shares one cache. Mirrors the
/// artwork-cache hashing idiom in `tags.rs`.
pub fn thumb_path_for(artwork_path: &Path, px: u32) -> Option<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    artwork_path.hash(&mut h);
    let hash = h.finish();
    let dir = dirs::cache_dir()?.join("sparkamp").join("thumbs");
    Some(dir.join(format!("{:016x}-{}.png", hash, px)))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test -p sparkamp --lib thumb_path'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/now_playing.rs
git commit -m "feat(core): deterministic thumbnail cache path (generation is per-frontend)"
```

---

## Task 5: GTK now-playing subscription seam + snapshot capture

Add a subscriber registry to `AppState` and capture the play-start snapshot at
pipeline start, BEFORE the 20-second `record_play` timer (`player.rs:3021`).

**Files:**
- Modify: `frontends/gtk/window/state.rs` (near `set_track_callback`, `:61`)
- Modify: `frontends/gtk/window/player.rs` (play-and-update path; timer at :3021)

**Interfaces:**
- Produces on `AppState`:
  - field `now_playing_subscribers: Vec<Rc<dyn Fn(&NowPlayingInfo)>>`
  - `pub fn subscribe_now_playing(&mut self, cb: Rc<dyn Fn(&NowPlayingInfo)>)`
  - `pub fn notify_now_playing(&self, info: &NowPlayingInfo)` (fan-out)
  - field `current_snapshot: PlaySnapshot` set at play start
- Consumes: `build_now_playing_info`, `PlaySnapshot`, `play_snapshot` (Tasks 1/3).

**Design:** No automated test (GTK window has no test infra — established phase-0/1
pattern). Gate = build + suite green + manual pass. Follow the `set_track_callback`
registration idiom exactly (`Option<Rc<dyn Fn(&str)>>` → here a `Vec` since A1
panel + A6 window + phase-3 MPRIS all subscribe).

- [ ] **Step 1: Add the registry fields + methods to `AppState`**

Mirror `set_track_callback` (`state.rs:61`, initialized `:237`). Add:
```rust
// field
now_playing_subscribers: Vec<Rc<dyn Fn(&crate...::now_playing::NowPlayingInfo)>>,
current_snapshot: crate...::media_library::PlaySnapshot,
```
Initialize `now_playing_subscribers: Vec::new()`, `current_snapshot: Default::default()`
in the constructor (`:230-237` region). Add:
```rust
pub fn subscribe_now_playing(&mut self, cb: Rc<dyn Fn(&NowPlayingInfo)>) {
    self.now_playing_subscribers.push(cb);
}
pub fn notify_now_playing(&self, info: &NowPlayingInfo) {
    for cb in &self.now_playing_subscribers { cb(info); }
}
```
> Borrow rule: `notify_now_playing` takes `&self` and calls subscribers — the
> caller must NOT hold a `borrow_mut()` on the `Rc<RefCell<AppState>>` across it.
> Build the `NowPlayingInfo`, drop the borrow, then notify. Document this at the
> call site.

- [ ] **Step 2: Capture the snapshot at play start**

Find the single "play current track and update UI labels" seam
(`play_and_update_callback`, `state.rs:59`) — this fires on every track start,
before the 20-second timer. There, BEFORE playback advances the counter:
```rust
// Snapshot pre-play stats for the now-playing panel (must precede record_play,
// which the position timer fires at 20s — see player.rs:3021).
let snap = state.media_lib
    .as_ref()
    .map(|ml| ml.play_snapshot(&path))
    .unwrap_or_default();
// store on state, then build + notify now-playing info
```
> Verify the media-lib handle field name on `AppState` (grep `media_lib`,
> `record_play` caller context isn't in state — the timer at player.rs:3036 uses
> `s.<handle>.record_play`; reuse that exact accessor). Build `NowPlayingInfo` via
> `build_now_playing_info(&path, lib_row, snap)` and call `notify_now_playing`.
> `lib_row`: look up the `LibTrack` by path if the handle exposes a getter; else
> pass `None` (probe fallback still yields data).

- [ ] **Step 3: Fire on state change + track end**

State-change (play/pause) and track-end also notify (A6/panel + phase-3 need it).
Locate the engine event handling in `player.rs` (the `TrackEvent`/EOS handling
near the position timer, :2761/:3021) and call `notify_now_playing` with a
refreshed (or same) info on pause/resume/end. For track-end, notifying with the
next track's info is handled by the play-start path; only ensure pause/resume
re-notify so subscribers can reflect state. Keep minimal — full playback-state in
the payload is phase 3; here subscribers just need the track+snapshot.

- [ ] **Step 4: Gate**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: builds clean (bin target compiles the GTK code), suite green, 0 warnings.
Quote both `test result:` lines.

- [ ] **Step 5: Commit**

```bash
git add frontends/gtk/window/state.rs frontends/gtk/window/player.rs
git commit -m "feat(gtk): now-playing subscriber seam + pre-play snapshot capture"
```

---

## Task 6: GTK A1 — expandable now-playing panel

`w` key + mode button toggles; marquee area is REPLACED by art (~200px, left) +
scrollable tag/tech/stats/links column (right); the small visualizer STRETCHES.
Collapsed = today's exact layout. Persisted via `config.window.player_expanded`.

**Files:**
- Create: `frontends/gtk/window/now_playing.rs`
- Modify: `frontends/gtk/window/mod.rs` (`mod now_playing;`)
- Modify: `frontends/gtk/window/player.rs` (swap container around marquee_frame at
  :335-355; `w` in `handle_key`; mode button in the vol_row near btn_eq/btn_info :408-458)
- Modify: `src/config.rs` (`WindowConfig.player_expanded`)

**Interfaces:**
- Consumes: `NowPlayingInfo`, `subscribe_now_playing` (Task 5); logo bytes
  (`util.rs:318` `LOGO_BYTES`).
- Produces: `now_playing::build_panel(...) -> (gtk4::Widget /* swap child */,
  subscription closure)` and a placeholder helper
  `now_playing::art_or_placeholder(info: &NowPlayingInfo) -> gtk4::Widget`.

- [ ] **Step 1: Config field (TDD in core)**

Add to `WindowConfig` (`config.rs:274`, after `ml_sidebar_width`):
```rust
/// Whether the main window's expandable now-playing panel was open at exit.
#[serde(default)]
pub player_expanded: bool,
```
Add `player_expanded: false` to the `Default` impl (`config.rs:~309`). Test:
```rust
// in config.rs tests (grep existing WindowConfig test module)
#[test]
fn window_config_defaults_player_collapsed() {
    assert!(!WindowConfig::default().player_expanded);
}
#[test]
fn window_config_player_expanded_roundtrips() {
    let mut c = WindowConfig::default();
    c.player_expanded = true;
    let s = toml::to_string(&c).unwrap();
    let back: WindowConfig = toml::from_str(&s).unwrap();
    assert!(back.player_expanded);
}
```
Run: `cargo test -p sparkamp --lib player_expanded` → PASS after adding.

- [ ] **Step 2: Build the panel module**

`now_playing.rs`: construct a horizontal box — art (`gtk4::Picture`/`Image`,
~200px, click → open/focus A6 via a callback passed in) on the left; a
`ScrolledWindow` wrapping a vertical box of rows on the right:
- tag rows from `info.tags` (label: value),
- tech line (`info.tech_line`) if non-empty,
- play count / last-played (`info.play_count`, `format_last_played` on
  `info.last_played` — reuse `timeutil`/`format_last_played`),
- Wikipedia link rows: `gtk4::LinkButton` for `artist_wiki_url` / `album_wiki_url`
  when `Some`.
Placeholder (no artwork): `LOGO_BYTES` at 50% opacity + "No artwork available"
label. `art_or_placeholder` returns the art widget or the placeholder. CSS classes:
`np-panel`, `np-art`, `np-placeholder`, `np-tag-row`, `np-link` (Task 10 styles them).

- [ ] **Step 3: Swap container in player.rs**

Wrap the existing `marquee_frame` (`player.rs:335`) region in a `gtk4::Stack` (or a
box whose child is swapped): child A = today's marquee (collapsed), child B = the
Task-2 panel (expanded). The small visualizer widget gets `set_vexpand(true)` /
larger size-request when expanded. `resizable(false)` stays; on toggle call
`window.set_default_size(-1, -1)` + `queue_resize` so collapse returns to the
compact natural size (verify interactively — GTK may need a `set_default_size`
re-kick). Subscribe the panel to `notify_now_playing` so it refreshes on track change.

- [ ] **Step 4: `w` toggle + mode button + persistence**

- `w` in the player `handle_key` match (grep the `handle_key` builder; add lowercase
  `w` — GTK also binds uppercase, follow the existing dual-bind idiom).
- Mode button in the vol_row beside `btn_eq`/`btn_info` (`player.rs:408-458`):
  `Button` with `mode-btn` class, `mode-btn-active` when expanded, tooltip
  `"Now-playing panel (w)"`.
- Toggle flips `config.window.player_expanded`, swaps the Stack child, updates the
  button class, and persists via the window's existing save-on-close (or immediate
  `config.save()` if neighbors do — copy the adjacent idiom).

- [ ] **Step 5: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: clean, suite green (config tests +2), 0 warnings.
```bash
git add frontends/gtk/window/now_playing.rs frontends/gtk/window/mod.rs \
        frontends/gtk/window/player.rs src/config.rs
git commit -m "feat(gtk): A1 expandable now-playing panel (w) with art + tags + wiki links"
```

---

## Task 7: GTK A6 — standalone art window

Singleton, resizable, cover-only, follows every track change; `k` key + art click
open/focus it. While focused, main shortcuts still work (delegate through the
shared key handler, like the shortcuts window).

**Files:**
- Create: `frontends/gtk/window/art_window.rs`
- Modify: `frontends/gtk/window/mod.rs` (`mod art_window;`)
- Modify: `frontends/gtk/window/player.rs` (`k` key; art-click wiring; singleton handle)
- Modify: `player.rs` `sections` array (`:3913`) — add `w` and `k` rows

**Interfaces:**
- Consumes: `NowPlayingInfo` / artwork path (subscription), `LOGO_BYTES`, the shared
  `handle_key` closure.
- Produces: `art_window::open_or_focus(state, ...) ` (singleton — reuses existing
  window if present, else builds; follows every `notify_now_playing`).

- [ ] **Step 1: Build the window (singleton)**

Follow the singleton idiom of another toggled window (grep how the shortcuts/EQ
window stores its handle on `AppState` and reuses it). `art_window.rs` builds a
resizable `ApplicationWindow` showing the cover (`Picture`, scaled to fit),
placeholder identical to A1's. Subscribe to `notify_now_playing` → update image on
every track change (incl. art→no-art → placeholder).

- [ ] **Step 2: Key delegation while focused**

Route the window's `EventControllerKey` through the shared `handle_key`: `Esc`
handled locally (close), everything else delegated — exact pattern: the shortcuts
window at `player.rs` (grep `EventControllerKey` + `handle_key` there). This makes
z/x/c/v/b/j/i/f work while A6 is focused (manual-test item 5).

- [ ] **Step 3: Open triggers**

`k` in player `handle_key` (dual-bind) → `open_or_focus`. A1 art click (Task 6
callback) → `open_or_focus`. Repeat presses focus the existing window (singleton).

- [ ] **Step 4: Shortcuts dialog rows (GTK, 1 of 3 files)**

In `player.rs` `sections` (`:3913`), add to the appropriate section:
```rust
("w", "Toggle now-playing panel"),
("k", "Open album-art window"),
```
> Mac's two files are updated in Task 13 (3-file rule).

- [ ] **Step 5: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: clean, suite green, 0 warnings.
```bash
git add frontends/gtk/window/art_window.rs frontends/gtk/window/mod.rs frontends/gtk/window/player.rs
git commit -m "feat(gtk): A6 singleton album-art window (k) with key delegation"
```

---

## Task 8: GTK A2 — inline ML thumbnails

The `artwork_path` column renders a small thumbnail instead of the "View" text
link. 36k rows: never load full images per row — lazy, cached, background.

**Files:**
- Modify: `frontends/gtk/window/media_library.rs` (artwork_path column cell; grep
  `artwork_path`, `"View"`)
- Modify: `frontends/gtk/window/ml_columns.rs` if the cell text/render lives there
- Consumes: `thumb_path_for` (Task 4)

**Design:** No core test (generation is GTK). Behavior verified in manual pass
(item 6: no jank scrolling a large view; click still opens viewer).

- [ ] **Step 1: Thumbnail cell**

In the artwork column's cell factory: for a row with an artwork path, compute
`thumb_path_for(path, N)` (N ≈ 32–48px). If the thumb PNG exists, load it
(`gtk4::Picture::for_filename` / `gdk::Texture::from_filename`). If not, show a
blank/placeholder and kick a lazy generation.

- [ ] **Step 2: Lazy generation (gdk-pixbuf, off the render path)**

Generate on first display on a background pattern (mirror the metadata-pass
thread idiom — grep the scan/metadata background pattern): decode + scale via
`gdk_pixbuf::Pixbuf::from_file_at_scale(src, N, N, true)`, save PNG to the
`thumb_path_for` path (`pixbuf.savev(&path, "png", &[])`), then request a redraw
of that cell. Ensure the cache dir exists (`std::fs::create_dir_all` on the parent).
Guard against re-generating (path exists → skip). Bad image → leave placeholder,
no panic.

- [ ] **Step 3: Preserve click → viewer**

The thumbnail keeps the existing click behavior (opens the artwork viewer / A6).
Grep the current "View" cell's gesture and reattach to the image.

- [ ] **Step 4: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: clean, suite green, 0 warnings.
```bash
git add frontends/gtk/window/media_library.rs frontends/gtk/window/ml_columns.rs
git commit -m "feat(gtk): inline ML artwork thumbnails with lazy cached generation"
```

---

## Task 9: GTK A5 + D14 — set-art refinements

Confirm APIC `CoverFront`; add "also write folder image" checkbox on embed
(writes `cover.<ext>`).

**Files:**
- Modify: `src/id3_editor.rs` (`write_tag_fields` :447 — confirm PictureType)
- Modify: `frontends/gtk/window/id3.rs` (art-browse row — add checkbox)

- [ ] **Step 1: Confirm APIC picture type (likely no-op)**

Read `write_tag_fields` (`id3_editor.rs:447+`) where it embeds artwork. Confirm the
`id3::frame::Picture` uses `PictureType::CoverFront`. If already correct, note it in
the commit and skip. If not, fix + add a test:
```rust
#[test]
fn embedded_artwork_is_cover_front() {
    // embed an image via write_tag_fields, reopen, assert the APIC picture_type
    // == PictureType::CoverFront
}
```

- [ ] **Step 2: "Also write folder image" checkbox**

In the GTK art-browse row (`id3.rs`, grep the artwork_path entry/browse button),
add an unchecked-by-default `CheckButton` "Also write folder image". On save/embed,
when checked and an artwork source path is set, copy/write `cover.<original ext>`
beside the audio file (ext from the chosen image). Plain checkbox — no config
persistence (user decision: keep simple). Reuse `f4e2a46`'s cover.* precedence so
the written file shows immediately.

- [ ] **Step 3: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: clean, suite green, 0 warnings.
```bash
git add src/id3_editor.rs frontends/gtk/window/id3.rs
git commit -m "feat(gtk): also-write-folder-image on embed; confirm APIC CoverFront"
```

---

## Task 10: Skin CSS for panel + art window

New widget surfaces get skin selectors + `render_gtk_css_covers_*` tests.

**Files:**
- Modify: `src/skin.rs` (`render_gtk_css`; tests near :1433-1507)

- [ ] **Step 1: Write failing coverage tests**

```rust
// src/skin.rs tests, alongside render_gtk_css_covers_marquee_panel (:1433)
#[test]
fn render_gtk_css_covers_now_playing_panel() {
    let css = render_gtk_css(&Skin::default());
    assert!(css.contains(".np-panel"));
    assert!(css.contains(".np-art"));
    assert!(css.contains(".np-placeholder"));
    assert!(css.contains(".np-link"));
}
#[test]
fn render_gtk_css_covers_art_window() {
    let css = render_gtk_css(&Skin::default());
    assert!(css.contains(".art-window"));
}
```
> Match the exact CSS class names used in Tasks 6/7. If they differ, reconcile —
> the classes the widgets add MUST equal what these tests assert.

- [ ] **Step 2: Run → fail**

Run: `cargo test -p sparkamp --lib render_gtk_css_covers_now_playing_panel render_gtk_css_covers_art_window`
Expected: FAIL (selectors absent).

- [ ] **Step 3: Add selectors**

In `render_gtk_css`, add rules for `.np-panel`, `.np-art`, `.np-placeholder`,
`.np-tag-row`, `.np-link`, `.art-window` using the skin's existing colour tokens
(follow the `.np-frame`/marquee rules already there). `.np-placeholder` → 50%
opacity. Hand-built list rows need `.ml-col-view` for selection colours if any.

- [ ] **Step 4: Run → pass; commit**

Run: `cargo test -p sparkamp --lib render_gtk_css_covers`
Expected: PASS.
```bash
git add src/skin.rs
git commit -m "feat(skin): CSS for now-playing panel and album-art window"
```

---

## Task 11: TUI now-playing text section

Reuse `NowPlayingInfo` for a data-as-text section (no art) — capability note.

**Files:**
- Modify: `frontends/tui/` (locate the now-playing / status render at execution)

- [ ] **Step 1: Locate + render**

Find the TUI's now-playing area (grep the TUI crate for the marquee/title render).
Build `NowPlayingInfo` for the current track and render a text block: populated
tags, tech line, play count / last played. No art (capability note for the
checklist). Keyboard walk of the touched screen (manual, item 8).

- [ ] **Step 2: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: clean, suite green, 0 warnings.
```bash
git add frontends/tui/
git commit -m "feat(tui): now-playing data text section from NowPlayingInfo"
```

---

## Task 12: FFI — now-playing + artwork set/clear + ML art path (blind mac prep)

**Files:**
- Create: `src/ffi/now_playing.rs`; Modify `src/ffi/mod.rs`
- Modify: `src/ffi/id3.rs` (artwork set/clear on tag ctx)
- Modify: `src/ffi/media_library.rs` (art path in `SparkampLibTrack` if not crossing)
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` (mirror ALL new symbols)

**Interfaces (repr(C), header-mirrored byte-for-byte):**
- `sparkamp_now_playing_info(ctx) -> SparkampNowPlaying` (repr(C): counts/pointers
  for tags, tech line C-string, artwork path C-string, play_count i64 + has_count
  flag, last_played C-string, artist/album wiki URL C-strings). Polled on the mac
  track-change notification (no new callback bridge — `currentIndex` publishes).
- `sparkamp_tag_set_artwork(ctx, path: *const c_char)` and
  `sparkamp_tag_clear_artwork(ctx)` — operate on the tag ctx's
  `fields.artwork_path` (empty string ⇒ clear, mirroring GTK entry semantics);
  save flows through the existing `sparkamp_tag_save`.
- `SparkampLibTrack`: add `artwork_path`/thumb path C-string if not already crossing
  (`from_lib_track` at :92 currently only crosses `has_art`).

- [ ] **Step 1: FFI roundtrip tests (Rust side)**

```rust
// src/ffi/id3.rs tests (or ffi tests module)
#[test]
fn ffi_set_then_clear_artwork_roundtrips() {
    // open a tag ctx on a temp mp3; sparkamp_tag_set_artwork(ctx, img_path);
    // sparkamp_tag_save; reopen → APIC present.
    // sparkamp_tag_clear_artwork(ctx); save; reopen → APIC gone.
}
```
> Follow the existing FFI tag-ctx test pattern (grep `sparkamp_tag_save` tests).

- [ ] **Step 2: Implement + mirror header**

Implement the three symbols + struct field. Update `sparkamp_bridge.h` with the
exact C signatures/struct layout byte-for-byte (`#[repr(C)]` alignment — follow the
`SparkampLibTrack` pattern from phase 1). Free-string ownership: follow the existing
FFI string-return convention (grep how other `-> *mut c_char` returns are freed).

- [ ] **Step 3: Gate + commit (with checklist append)**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Append mac verification items to `docs/mac-pass-checklist.md` (dated Phase 2
section) IN THIS COMMIT.
```bash
git add src/ffi/ frontends/SparkampMac/SparkampCore/sparkamp_bridge.h docs/mac-pass-checklist.md
git commit -m "feat(ffi): now-playing info + artwork set/clear + ML art path for mac"
```

---

## Task 13: mac — A1 panel + A6 window + ML art column + D14 + shortcuts (BLIND)

Read whole files before editing. Mechanically simple changes. No compiler here.

**Files:**
- Modify: `frontends/SparkampMac/Sources/PlayerWindow.swift` (A1: marquee swap +
  viz stretch, persisted via the model's settings channel; poll
  `sparkamp_now_playing_info` on the existing track-change notification)
- Modify: `frontends/SparkampMac/Sources/ArtworkWindow.swift` (ALREADY EXISTS —
  read first; extend/replace to A6 spec: follow-track + singleton + placeholder;
  do NOT duplicate)
- Modify: `frontends/SparkampMac/Sources/MediaLibraryWindow.swift` (add art column;
  thumbnail via `thumb_path_for` path over FFI, NSImage generation)
- Modify: `frontends/SparkampMac/Sources/Id3EditorWindow.swift` (D14: browse / embed
  / clear embedded art via the Task-12 FFI)
- Modify: `frontends/SparkampMac/Sources/KeyboardShortcutsView.swift` (`sections`
  :22 — add w + k; 2 of 3 files)
- Modify: `frontends/SparkampMac/Sources/SparkampModel+Keys.swift` (+ `SparkampModel.swift`)
  — handle lowercase `w` (panel) + `k` (art window); 3 of 3 files
- Modify: `docs/mac-pass-checklist.md` (all A1/A6/A2/D14 + w/k items)

- [ ] **Step 1: Read the mac files fully** (PlayerWindow, ArtworkWindow,
  MediaLibraryWindow, Id3EditorWindow, KeyboardShortcutsView, SparkampModel+Keys).
  Note the existing track-change publish (`currentIndex`) and settings channel.

- [ ] **Step 2: A1 panel** in PlayerWindow (marquee swap + viz stretch), polling
  `sparkamp_now_playing_info` on track change; persist expanded state via the model.

- [ ] **Step 3: A6** — extend ArtworkWindow to singleton + follow-track + placeholder.

- [ ] **Step 4: A2** — ML art column with NSImage thumbnail from the shared cache path.

- [ ] **Step 5: D14** — Id3 editor browse/embed/clear via FFI.

- [ ] **Step 6: Shortcuts** — w + k in KeyboardShortcutsView `sections` and the
  key handler (lowercase only).

- [ ] **Step 7: Checklist + commit** (Swift does not compile here — gate is the Rust
  suite from Task 12; mac correctness is the user's Xcode pass).
```bash
git add frontends/SparkampMac/ docs/mac-pass-checklist.md
git commit -m "feat(mac): A1 panel, A6 window, ML art column, D14 art edit, w/k shortcuts (blind)"
```

---

## Task 14: Phase close-out

- [ ] **Step 1: Full gate** — `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`,
  0 warnings, quote both `test result:` lines. Confirm count ≥ floor + new tests.
- [ ] **Step 2: Whole-branch final review** on the most capable available model
  (`scripts/review-package BASE HEAD`, BASE = pre-Task-1 commit). ONE fix subagent
  for findings, then re-review. (Phase 0/1 final reviews each caught shipping bugs
  the task gates missed — do NOT skip.)
- [ ] **Step 3: Spec known-limitations** — append any accepted residuals to
  `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` (e.g. TUI no
  art; per-frontend thumb gen; layout eyeball items).
- [ ] **Step 4: Ledger** — write per-task lines + "PHASE 2 COMPLETE" to
  `.superpowers/sdd/progress.md`; copy the durable summary into a docs note (the
  ledger is gitignored + overwritten).
- [ ] **Step 5: User interactive pass list** — deliver the phase doc's Manual test
  plan (items 1-9 + mac checklist) as the close-out message. Do NOT push.

## Self-review (done at write time)

- Spec coverage: A1 (T6), A6 (T7), A2 (T4+T8+T13), A5/D14 (T9+T12+T13), core seams
  (T1 snapshot, T3 NowPlayingInfo, T5 subscription), wiki links (T2), thumbs (T4),
  mac parity (T12/T13), TUI (T11), skin (T10), shortcuts 3-file (T7 GTK + T13 mac).
  All design-doc sections mapped.
- Placeholders: core Tasks 1–4 carry complete code + tests. UI Tasks 5–13 are
  anchor+contract+test-spec (GTK window/mac have no unit-test infra — established
  phase-0/1 pattern; implementers work from patterns, gate on build+suite+manual).
  Named seams flagged PLACEHOLDER (`read_tag_fields_for`, `tech_line_for`) with
  explicit "resolve to the real seam, do not guess" notes.
- Type consistency: `PlaySnapshot`, `NowPlayingInfo`, `thumb_path_for`,
  `wiki_search_url`, `player_expanded`, CSS classes (`.np-*`, `.art-window`) used
  identically across producing/consuming tasks.
