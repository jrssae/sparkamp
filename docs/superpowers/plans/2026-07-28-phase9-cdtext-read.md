# Phase 9 — CD-TEXT read parity (TUI + macOS) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Read `docs/superpowers/plans/2026-07-19-opus-handoff.md` FIRST (process,
> environment, key ledger, pitfalls), then `2026-07-19-phase9-cdtext.md`
> (the design-level plan this expands). Disc subsystem: reuse the proven
> `cdtext_to_v07t` read path — do NOT invent a reader.

**Goal:** When gnudb has no match for an audio CD, read CD-TEXT off the disc
and show its album / artist / per-track titles in the disc view — on the TUI
and macOS frontends, matching what GTK already does.

**Architecture:** The core read (`src/disc/cdtext.rs::read_cdtext`), the v07t
parser, and `CdText::to_xmcd` already exist and are used by GTK. This phase
(a) adds a macOS acquisition path (`drutil cdtext`) beside the Linux one,
(b) exposes a guarded FFI read so the blind macOS frontend can call it, and
(c) wires the same read→cache→overlay flow into the TUI. No new UI: the disc
views just show better names. GTK is already complete and gets NO code
changes here.

**Tech Stack:** Rust core (`std::process::Command` for `cdrskin`/`drutil`),
existing FFI (`src/ffi/disc.rs`, C header mirror), Ratatui/crossterm (TUI),
SwiftUI (macOS, blind — no Swift compiler in this environment).

## Global Constraints

- **Build/test ONLY inside distrobox `dev-box`:**
  `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`.
  Host builds fail (no gstreamer/gtk). NEVER gate on `cargo build --lib` —
  GTK compiles only in the bin target; use full `cargo build`.
- **Zero warnings, zero test failures** before any task is "done". Known
  flaky test `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow`
  is a parallel race — confirm it passes single-threaded; it is NOT a
  regression.
- **Precedence rule (LOCKED, matches Winamp):** gnudb wins entirely when it
  has an entry for the disc; CD-TEXT is used ONLY when gnudb is a total miss
  (whole-entry `.or()`, NOT per-field gap-fill). No "prefer CD-TEXT" toggle.
  This is the exact rule GTK already implements at
  `frontends/gtk/window/media_library.rs:9039` (`disc_tags.get(id).or_else(|| disc_cdtext.get(id))`).
- **Drive-contention:** every CD-TEXT read spins the disc and MUST be wrapped
  in `crate::disc::detect::begin_exclusive_read()` / `end_exclusive_read()`
  (respect the P2 refcount `0356e3f`). Read at PROBE/first-show time only,
  cache per disc-id, never re-read on view refresh. Never block the UI thread
  — run the read on a background thread/job.
- **New `src/` modules** need `mod x;` in BOTH `src/lib.rs` AND `src/main.rs`
  (not applicable here — no new modules — but keep in mind).
- **GTK/UI strings** via `gtk_safe()`; sanitize NULs on any CD-TEXT text that
  reaches a UI label. RefCell borrows never held across UI callbacks.
- **macOS is BLIND:** no Swift compiler here. Read whole files, use real
  property names, mirror every FFI symbol byte-for-byte in
  `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, and append
  verification items to `docs/mac-pass-checklist.md` in the SAME commit.
- **Commits** end with:
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
- **NO push** without a fresh explicit instruction from the user.

**Base commit (pre-Task-1):** `d28d034` on branch `album-art-improvements`.

---

## File Structure

- `src/disc/cdtext.rs` — MODIFY. Add `parse_drutil_cdtext` (pure) and a
  macOS `read_cdtext` arm (drutil). The Linux `read_cdtext`, `parse_v07t_readback`,
  `CdText`, and `to_xmcd` already exist and are unchanged.
- `src/ffi/disc.rs` — MODIFY. Add `sparkamp_disc_read_cdtext(ctx, drive_json)`.
- `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` — MODIFY. Mirror the
  new FFI symbol.
- `frontends/tui/media_library/mod.rs` (App struct) + `gnudb.rs` +
  `detection.rs` + `tags.rs` — MODIFY. CD-TEXT cache, background read, tick
  drain, overlay fallback.
- `frontends/SparkampMac/Sources/…` (disc view + model) — MODIFY, BLIND.
  Call the FFI on first show of an unknown audio disc; cache; overlay.
- `docs/mac-pass-checklist.md` — MODIFY. Phase-9 verification section.
- `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` —
  MODIFY. Phase-9 known-limitations.
- This plan file — the deferred macOS burn-side appendix lives at the bottom.

---

### Task 1: Core — macOS CD-TEXT acquisition (`drutil`) + parser

**Files:**
- Modify: `src/disc/cdtext.rs`
- Test: `src/disc/cdtext.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: existing `CdText`, `parse_v07t_readback`, `CdText::to_xmcd`,
  `CdText::is_empty`.
- Produces:
  - `pub fn parse_drutil_cdtext(text: &str) -> CdText` — pure parser for
    `drutil cdtext` output.
  - `#[cfg(target_os = "macos")] pub fn read_cdtext(drive_id: &str) -> Option<CdText>`
    — shells `drutil -drive <drive_id> cdtext`, parses with
    `parse_drutil_cdtext`, returns `None` when empty/failed. Same signature
    as the existing Linux `read_cdtext` so callers are OS-agnostic.

> **BLIND-FORMAT WARNING (read before writing the parser):** `drutil cdtext`'s
> exact stdout format is NOT documented publicly and could not be captured in
> this environment. The parser below is a best-effort over the plausible
> `KEY "value"` token form. A macOS worker MUST capture a real
> `drutil -drive 1 cdtext` dump from a CD-TEXT-bearing disc and adjust
> `parse_drutil_cdtext` + its fixture to match the real output before trusting
> it. Keep the parser tolerant (ignore unrecognized lines; never panic).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/disc/cdtext.rs`:

```rust
#[test]
fn drutil_cdtext_parses_album_artist_and_titles() {
    // PROVISIONAL fixture — verify/replace against a real `drutil cdtext`
    // dump on macOS (see the BLIND-FORMAT WARNING in the plan). The parser
    // treats the first TITLE/PERFORMER pair as disc-level and each
    // subsequent TITLE as track 1, 2, … in order.
    let dump = "\
CD-Text, Block 0 (English):
  TITLE \"Greatest Hits\"
  PERFORMER \"The Band\"
  Track 1:
    TITLE \"First Song\"
  Track 2:
    TITLE \"Second Song\"
";
    let cd = parse_drutil_cdtext(dump);
    assert_eq!(cd.album.as_deref(), Some("Greatest Hits"));
    assert_eq!(cd.artist.as_deref(), Some("The Band"));
    assert_eq!(cd.track_titles.len(), 2);
    assert_eq!(cd.track_titles[0], (1, "First Song".into()));
    assert_eq!(cd.track_titles[1], (2, "Second Song".into()));

    // Round-trips into the same gnudb-style overlay entry as the v07t path.
    let x = cd.to_xmcd("deadbeef");
    assert_eq!(x.album, "Greatest Hits");
    assert_eq!(x.track_titles[1], "Second Song");

    // No CD-TEXT → empty (caller treats as a miss).
    assert!(parse_drutil_cdtext("No CD-Text on this disc.\n").is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib disc::cdtext::tests::drutil_cdtext_parses -- --exact'`
Expected: FAIL — `cannot find function parse_drutil_cdtext`.

- [ ] **Step 3: Write the parser**

Add to `src/disc/cdtext.rs` (below `parse_v07t_readback`):

```rust
/// Parse `drutil cdtext` output into a [`CdText`]. Tolerant, quote-aware:
/// the FIRST `TITLE`/`PERFORMER` pair is disc-level; each later `TITLE`
/// (under a `Track N:` heading) becomes that track's title in order.
///
/// NOTE: `drutil cdtext`'s exact format is undocumented; this handles the
/// plausible `KEY "value"` token form. A macOS worker must confirm the real
/// output shape and adjust this parser + its test fixture (see plan).
pub fn parse_drutil_cdtext(text: &str) -> CdText {
    let mut out = CdText::default();
    let mut cur_track: Option<u32> = None;
    let mut next_seq: u32 = 0; // fallback track counter when no explicit "Track N"
    for line in text.lines() {
        let t = line.trim();
        // "Track 3:" heading sets the track the following TITLE belongs to.
        if let Some(rest) = t.strip_prefix("Track ") {
            if let Some(nums) = rest.split([':', ' ']).next() {
                if let Ok(n) = nums.trim().parse::<u32>() {
                    cur_track = Some(n);
                    continue;
                }
            }
        }
        let Some(val) = quoted_value(t) else { continue };
        if val.is_empty() {
            continue;
        }
        if let Some(key) = t.split_whitespace().next() {
            match key {
                "TITLE" | "Title" => {
                    if out.album.is_none() && cur_track.is_none() {
                        out.album = Some(val);
                    } else {
                        let n = cur_track.take().unwrap_or_else(|| {
                            next_seq += 1;
                            next_seq
                        });
                        if n as usize > next_seq as usize {
                            next_seq = n;
                        }
                        out.track_titles.push((n, val));
                    }
                }
                "PERFORMER" | "Performer" => {
                    if out.artist.is_none() && cur_track.is_none() {
                        out.artist = Some(val);
                    }
                    // per-track performers are ignored (title-only overlay,
                    // same as the v07t readback path).
                }
                _ => {}
            }
        }
    }
    out
}

/// Extract the first double-quoted substring from a line, if any.
fn quoted_value(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib disc::cdtext::tests::drutil_cdtext_parses -- --exact'`
Expected: PASS.

- [ ] **Step 5: Add the macOS read arm**

Add to `src/disc/cdtext.rs`, directly below the existing
`#[cfg(target_os = "linux")] pub fn read_cdtext`:

```rust
/// Read CD-TEXT off the loaded disc on macOS via `drutil -drive <id> cdtext`.
/// `drive_id` is the drutil enumeration index (`OpticalDrive::id`), the same
/// value the mac burn/rip paths pass. `None` when the disc has no CD-TEXT or
/// drutil fails. READS THE DISC — the caller MUST hold the exclusive-read
/// guard (drive-contention rule).
#[cfg(target_os = "macos")]
pub fn read_cdtext(drive_id: &str) -> Option<CdText> {
    let out = std::process::Command::new("drutil")
        .args(["-drive", drive_id, "cdtext"])
        .output()
        .ok()?;
    let cd = parse_drutil_cdtext(&String::from_utf8_lossy(&out.stdout));
    (!cd.is_empty()).then_some(cd)
}
```

- [ ] **Step 6: Verify the whole crate builds clean on both cfgs**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test --lib disc::cdtext'`
Expected: builds with zero warnings; all `disc::cdtext` tests pass. (The
macOS arm won't be exercised on Linux — that's fine; it is compile-checked by
the mac worker's build.)

- [ ] **Step 7: Commit**

```bash
git add src/disc/cdtext.rs
git commit -m "feat(disc): drutil CD-TEXT parser + macOS read_cdtext arm"
```

---

### Task 2: FFI — `sparkamp_disc_read_cdtext`

**Files:**
- Modify: `src/ffi/disc.rs`
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`
- Test: `src/ffi/disc.rs` (inline test for the empty/None path)

**Interfaces:**
- Consumes: `crate::disc::cdtext::read_cdtext`, `CdText::to_xmcd`,
  `crate::disc::discid::freedb_discid`, the existing `json_in` /
  `OpticalDrive` / exclusive-read guard helpers.
- Produces: `sparkamp_disc_read_cdtext(ctx, drive_json) -> *mut c_char`
  returning an `XmcdEntry` JSON (the same shape gnudb entries use) on
  success, or `null` when the disc carries no CD-TEXT / read failed / input
  is bad. Free with `sparkamp_free_string`.

> Acquisition happens core-side. On Linux this is cdrskin/v07t; on macOS it
> is drutil (Task 1). If the drutil parser proves unreliable in live mac
> testing, the mac worker's documented fallback is to acquire CD-TEXT via the
> DiscRecording framework in Swift and build the overlay entry there — see the
> "macOS read fallback" note in Task 4. Either way the precedence/merge stays
> in the frontend (`gnudb.or(cdtext)`), matching GTK.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/ffi/disc.rs` (create one if absent, mirroring
other `src/ffi/*.rs` test modules):

```rust
#[test]
fn read_cdtext_null_on_bad_drive_json() {
    // Bad JSON in → null out, no panic.
    let bad = std::ffi::CString::new("not json").unwrap();
    let p = unsafe { sparkamp_disc_read_cdtext(std::ptr::null_mut(), bad.as_ptr()) };
    assert!(p.is_null());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib ffi::disc::tests::read_cdtext_null -- --exact'`
Expected: FAIL — `cannot find function sparkamp_disc_read_cdtext`.

- [ ] **Step 3: Write the FFI function**

Add to `src/ffi/disc.rs` (follow the existing `sparkamp_disc_track_entries`
style — takes `OpticalDrive` JSON):

```rust
/// Read CD-TEXT off the drive's loaded audio disc and return it as an
/// `XmcdEntry` JSON (same shape as a gnudb match), so the frontend can
/// overlay it exactly like a database entry when gnudb has no match. Takes
/// the `OpticalDrive` JSON from `sparkamp_disc_list_drives`. Returns `null`
/// when the disc has no CD-TEXT, the read fails, or the input is bad. Holds
/// the exclusive-read guard for the duration of the read (drive contention).
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_read_cdtext(
    _ctx: *mut SparkampCtx,
    drive_json: *const c_char,
) -> *mut c_char {
    let Some(drive): Option<OpticalDrive> = json_in(drive_json) else {
        return std::ptr::null_mut();
    };
    let Some(toc) = drive.toc.as_ref() else {
        return std::ptr::null_mut();
    };
    let discid = crate::disc::discid::freedb_discid(toc);
    crate::disc::detect::begin_exclusive_read();
    let cd = crate::disc::cdtext::read_cdtext(&drive.id);
    crate::disc::detect::end_exclusive_read();
    match cd {
        Some(cd) => json_out(&cd.to_xmcd(&discid)),
        None => std::ptr::null_mut(),
    }
}
```

> Verify against `src/disc/detect.rs`: `OpticalDrive` has `id: String` and
> `toc: Option<DiscToc>`, and `freedb_discid` takes `&DiscToc`. Adjust field
> access if the real struct differs (read the struct before writing).

- [ ] **Step 4: Run test to verify it passes**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib ffi::disc::tests::read_cdtext_null -- --exact'`
Expected: PASS.

- [ ] **Step 5: Mirror the symbol in the C header**

In `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, next to
`sparkamp_disc_track_entries`, add (match the file's exact style/return type
convention):

```c
// Reads CD-TEXT off the drive's audio disc; returns an XmcdEntry JSON string
// (same shape as a gnudb match) or NULL when there is no CD-TEXT / read fails.
// Free with sparkamp_free_string.
char *sparkamp_disc_read_cdtext(void *ctx, const char *drive_json);
```

- [ ] **Step 6: Full build + test**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: zero warnings, all pass (confirm the disc flaky test single-threaded
if it trips).

- [ ] **Step 7: Commit**

```bash
git add src/ffi/disc.rs frontends/SparkampMac/SparkampCore/sparkamp_bridge.h
git commit -m "feat(ffi): sparkamp_disc_read_cdtext (guarded CD-TEXT read → XmcdEntry)"
```

---

### Task 3: TUI — CD-TEXT read, cache, and overlay fallback

**Files:**
- Modify: `frontends/tui/media_library/mod.rs` (App fields + tick drain)
- Modify: `frontends/tui/media_library/gnudb.rs` (background read spawner)
- Modify: `frontends/tui/media_library/detection.rs` (auto-trigger on show)
- Modify: `frontends/tui/media_library/tags.rs` (overlay fallback)
- Test: `frontends/tui/media_library/tags.rs` (overlay precedence test)

**Interfaces:**
- Consumes: `crate::disc::cdtext::read_cdtext`, `CdText::to_xmcd`,
  `crate::disc::detect::{begin_exclusive_read, end_exclusive_read}`,
  existing `self.disc_tags: HashMap<String, XmcdEntry>`,
  `self.selected_disc_identity() -> Option<(DiscToc, String)>`,
  the existing `disc_lookup` background pattern in `gnudb.rs`.
- Produces: `disc_cdtext: HashMap<String, XmcdEntry>` cache,
  `disc_cdtext_tried: HashSet<String>`, `disc_cdtext_read: Option<Receiver<(String, XmcdEntry)>>`,
  `spawn_disc_cdtext_read(&mut self)`, tick-loop drain, and a modified
  `apply_disc_tags_to_entries` that falls back to `disc_cdtext`.

> **Read first** (before writing): open `frontends/tui/media_library/gnudb.rs`
> (`spawn_disc_lookup` around line 20 — the mpsc + `std::thread::spawn` + tick
> drain pattern) and `mod.rs` where `disc_lookup` is declared and drained.
> Mirror that pattern exactly; use a SEPARATE channel field so a CD-TEXT read
> can never collide with an in-flight gnudb lookup.

- [ ] **Step 1: Add the cache + channel fields to the App struct**

In the App struct (`frontends/tui/media_library/mod.rs` or wherever
`disc_tags` and `disc_lookup` are declared — grep for them), add:

```rust
/// CD-TEXT read off unknown audio discs, keyed by freedb disc-id. Consulted
/// only when `disc_tags` has no gnudb/user entry (Winamp precedence).
pub(crate) disc_cdtext: std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>,
/// Disc-ids we've already attempted a CD-TEXT read for (one attempt each).
pub(crate) disc_cdtext_tried: std::collections::HashSet<String>,
/// In-flight background CD-TEXT read result, drained in the tick loop.
pub(crate) disc_cdtext_read: Option<std::sync::mpsc::Receiver<(String, crate::disc::xmcd::XmcdEntry)>>,
```

Initialize all three in the App constructor(s) (`Default`/`new`) next to
`disc_tags` / `disc_lookup`: `HashMap::new()`, `HashSet::new()`, `None`.

- [ ] **Step 2: Write the failing overlay-precedence test**

Add to the `tests` module covering `tags.rs` (grep for existing
`apply_disc_tags_to_entries` tests; create a test fn beside them). If the
disc-view test scaffolding differs, adapt — the assertion that matters:
CD-TEXT fills titles only when `disc_tags` misses, and gnudb wins when both
exist.

```rust
#[test]
fn cdtext_overlays_only_when_gnudb_absent() {
    let mut app = App::new_for_test(); // use the existing test constructor
    // Simulate a loaded audio disc with a known disc-id + two "Track N" entries.
    let discid = test_load_two_track_audio_disc(&mut app); // helper: sets mode + selected drive

    // CD-TEXT present, gnudb absent → CD-TEXT titles overlay.
    app.disc_cdtext.insert(discid.clone(), xmcd_with_titles(&["From CDTEXT 1", "From CDTEXT 2"]));
    app.apply_disc_tags_to_entries();
    assert_eq!(nth_entry_title(&app, 0), "From CDTEXT 1");

    // gnudb now present → gnudb wins, CD-TEXT ignored.
    app.disc_tags.insert(discid.clone(), xmcd_with_titles(&["From gnudb 1", "From gnudb 2"]));
    app.apply_disc_tags_to_entries();
    assert_eq!(nth_entry_title(&app, 0), "From gnudb 1");
}
```

> If no `App::new_for_test` / disc-view test harness exists in the TUI crate,
> keep the test minimal: construct the two `XmcdEntry` values and assert the
> precedence helper directly (see Step 4) rather than driving the full App.
> Do NOT invent heavy scaffolding — a focused unit test on the `.or()` lookup
> is sufficient.

- [ ] **Step 3: Run test to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --bin sparkamp cdtext_overlays_only -- --exact'`
Expected: FAIL (compile error or assertion — the fallback isn't wired yet).

- [ ] **Step 4: Wire the overlay fallback**

In `frontends/tui/media_library/tags.rs`, change `apply_disc_tags_to_entries`
so it consults `disc_cdtext` when `disc_tags` misses (whole-entry `.or()`):

```rust
pub(crate) fn apply_disc_tags_to_entries(&mut self) {
    let Some((_, discid)) = self.selected_disc_identity() else {
        return;
    };
    // gnudb/user entry wins; CD-TEXT fills a total miss (Winamp precedence).
    let Some(entry) = self
        .disc_tags
        .get(&discid)
        .or_else(|| self.disc_cdtext.get(&discid))
        .cloned()
    else {
        return;
    };
    let titles = entry.track_titles;
    if let Mode::MediaLibrary(s) = &mut self.mode {
        for e in &mut s.disc_entries {
            let i = e.number as usize - 1;
            if let Some(t) = titles.get(i) {
                if !t.is_empty() {
                    e.title = t.clone();
                }
            }
        }
    }
}
```

> Also update `add_disc_entries` (detection.rs:115-119) and the rip tag lookup
> (`rip.rs:140`) to use the same `disc_tags.get(id).or_else(|| disc_cdtext.get(id))`
> so ripped filenames/tags inherit CD-TEXT names (design manual-test #5). Keep
> the `.cloned()` shape each site already uses.

- [ ] **Step 5: Run test to verify it passes**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --bin sparkamp cdtext_overlays_only -- --exact'`
Expected: PASS.

- [ ] **Step 6: Add the background read spawner**

In `frontends/tui/media_library/gnudb.rs` (beside `spawn_disc_lookup`), add:

```rust
/// Read CD-TEXT off the currently selected unknown audio disc on a
/// background thread (it spins the drive). One attempt per disc-id; result
/// arrives through `disc_cdtext_read` in the tick loop. No-op when a read is
/// already in flight, the disc is already tried, or gnudb already has it.
pub(crate) fn spawn_disc_cdtext_read(&mut self) {
    let Some((toc, discid)) = self.selected_disc_identity() else {
        return;
    };
    if self.disc_tags.contains_key(&discid)
        || self.disc_cdtext_tried.contains(&discid)
        || self.disc_cdtext_read.is_some()
    {
        return;
    }
    let Some(drive_id) = self.selected_disc_drive_id() else {
        return;
    };
    self.disc_cdtext_tried.insert(discid.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    self.disc_cdtext_read = Some(rx);
    let _ = &toc; // discid already computed; toc not needed on the thread
    std::thread::spawn(move || {
        crate::disc::detect::begin_exclusive_read();
        let cd = crate::disc::cdtext::read_cdtext(&drive_id);
        crate::disc::detect::end_exclusive_read();
        if let Some(cd) = cd {
            // Receiver dropped = user closed the library; ignore send error.
            let _ = tx.send((discid.clone(), cd.to_xmcd(&discid)));
        }
    });
}
```

> `selected_disc_drive_id()` — if no such helper exists, derive the drive id
> the same way the gnudb/rip paths do (grep how they get the selected
> `OpticalDrive`; it exposes `.id`). Add a tiny `pub(super) fn
> selected_disc_drive_id(&self) -> Option<String>` if it keeps the spawner
> clean.

- [ ] **Step 7: Auto-trigger on showing an unknown audio disc**

In `frontends/tui/media_library/detection.rs::reload_ml_disc_entries` (right
after `self.apply_disc_tags_to_entries();`), kick a CD-TEXT read when the
shown disc is an unknown audio disc:

```rust
self.apply_disc_tags_to_entries();
// Unknown audio disc with no gnudb/user entry yet: read its CD-TEXT once,
// in the background, then re-overlay when it lands (tick loop).
self.spawn_disc_cdtext_read();
```

`spawn_disc_cdtext_read` self-gates (already-tried / gnudb-present / audio
check via `selected_disc_identity`), so this is safe to call unconditionally
here. If `selected_disc_identity()` returns `Some` only for audio discs,
that's the audio gate; otherwise add an `is_audio_cd` check inside the
spawner.

- [ ] **Step 8: Drain the result in the tick loop**

Where `disc_lookup` is drained each tick (grep `handle_disc_lookup` /
`disc_lookup.as_ref()` / `try_recv` in `mod.rs`), add a sibling drain:

```rust
if let Some(rx) = &self.disc_cdtext_read {
    if let Ok((discid, entry)) = rx.try_recv() {
        self.disc_cdtext.insert(discid, entry);
        self.disc_cdtext_read = None;
        self.apply_disc_tags_to_entries(); // re-overlay with the new names
    } else if rx.try_recv().is_err() {
        // Disconnected without a value (no CD-TEXT / thread done): clear the
        // slot so a later disc can read. Use the disconnected-vs-empty
        // distinction the existing disc_lookup drain uses.
    }
}
```

> Match the EXACT try_recv/disconnect handling the existing `disc_lookup`
> drain uses (Empty = keep waiting, Disconnected = clear slot). Do not
> busy-clear on `Empty`. Read the existing drain and mirror it.

- [ ] **Step 9: Full build + test**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: zero warnings, all pass.

- [ ] **Step 10: Commit**

```bash
git add frontends/tui/media_library/
git commit -m "feat(tui): read CD-TEXT on unknown audio discs, overlay when gnudb misses"
```

---

### Task 4: macOS — call the FFI and overlay (BLIND)

**Files:**
- Modify: `frontends/SparkampMac/Sources/…` — the disc view + model that
  already render gnudb disc tags (grep the Swift sources for the disc/tag
  overlay: `disc`, `xmcd`, `trackTitles`, `sparkamp_disc_track_entries`,
  `sparkamp_disc_id`).
- Modify: `docs/mac-pass-checklist.md` — Phase-9 section (same commit).

**Interfaces:**
- Consumes: `sparkamp_disc_read_cdtext(ctx, drive_json)` (Task 2), the
  existing Swift disc-tag cache + overlay, `sparkamp_free_string`.

> **This task is BLIND — no Swift compiler here.** Read the whole disc-view
> Swift file(s) before editing; use real property names; do not guess symbol
> spellings. The mac worker runs Xcode + real hardware to verify.

- [ ] **Step 1: Locate the mac disc overlay path**

Find where the mac disc view fetches gnudb tags and overlays track titles /
the "Artist — Album" header (mirror of GTK `media_library.rs:9031-9108`).
Note the existing per-disc tag cache keyed by disc-id and the "unknown audio
disc" branch.

- [ ] **Step 2: Add a CD-TEXT cache + one-shot read**

Mirror GTK's `disc_cdtext` + `disc_cdtext_tried` logic in the model:
- A `[String: XmcdEntry]`-equivalent cache keyed by disc-id.
- A `Set<String>` of tried disc-ids.
- On first show of an audio disc with no gnudb/user entry and not yet tried:
  mark tried; on a background queue call
  `sparkamp_disc_read_cdtext(ctx, driveJSON)`; if non-NULL, decode the
  XmcdEntry JSON, `sparkamp_free_string` the pointer, cache it, and trigger a
  re-render on the main queue. Reuse the existing disc-view reload trigger
  (the same mechanism the gnudb match uses).

- [ ] **Step 3: Overlay with Winamp precedence**

In the render path, choose the entry as `gnudbEntry ?? cdtextEntry` (whole
entry; gnudb wins) — identical to GTK's `.or_else`. Overlay per-track titles
(skip empty) and set the "Artist — Album (year)" header. Sanitize/guard NUL
as the existing code does.

- [ ] **Step 4: macOS read fallback (document + implement if drutil parse fails live)**

If, in live testing, `drutil cdtext` output does not parse (the core
`parse_drutil_cdtext` returns empty on real dumps), switch acquisition to the
DiscRecording framework:
- Use `DRDevice` for the drive and read the disc's CD-TEXT via the
  `DRCDTextBlock` / `DRDeviceMediaInfoKey` structured API (title + performer
  per track + disc), which returns real strings rather than a raw dump.
- Build the same overlay entry Swift-side (album/artist/titles), and keep the
  gnudb-wins precedence. This avoids the FFI read entirely for mac; the FFI
  symbol still exists for parity and can be left unused on mac if the
  framework path is chosen. Record whichever path was used in the checklist.

- [ ] **Step 5: Append Phase-9 verification items to `docs/mac-pass-checklist.md`**

Add a "Phase 9 — CD-TEXT read" section covering:
1. CD-TEXT disc absent from gnudb → real album/artist/track titles show.
2. gnudb-known disc → gnudb names unchanged (CD-TEXT does not override).
3. Neither → "Track N" fallback.
4. Burn in one window + probe another → no drive fight, no error dialogs
   (exclusive-read guard).
5. Ripped filenames/tags inherit CD-TEXT names.
6. Which acquisition path was used (drutil parse vs DiscRecording framework)
   and the real `drutil cdtext` dump captured (paste it so the core parser
   fixture can be corrected).

- [ ] **Step 6: Commit (BLIND — Rust suite unchanged)**

```bash
git add frontends/SparkampMac/ docs/mac-pass-checklist.md
git commit -m "feat(mac): read CD-TEXT on unknown audio discs (blind), overlay when gnudb misses"
```

---

### Task 5: Close-out — live test, docs, deferred appendix

**Files:**
- Modify: `src/disc/cdtext.rs` (ignored `live_cdtext_read` test)
- Modify: `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md`
  (Phase-9 known-limitations)
- Modify: `.superpowers/sdd/progress.md`, roadmap memory (ledger)

- [ ] **Step 1: Add an ignored live test (Linux)**

In `src/disc/cdtext.rs` tests, mirror the existing `live_*` disc tests:

```rust
#[test]
#[ignore = "requires a real disc with CD-TEXT in the drive; human-run"]
#[cfg(target_os = "linux")]
fn live_cdtext_read() {
    // Set SPARKAMP_TEST_DRIVE to the optical device (e.g. /dev/sr0).
    let dev = std::env::var("SPARKAMP_TEST_DRIVE").unwrap_or_else(|_| "/dev/sr0".into());
    let cd = read_cdtext(&dev);
    println!("CD-TEXT read: {cd:?}");
    assert!(cd.is_some(), "no CD-TEXT read from {dev}");
}
```

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib live_cdtext_read -- --ignored --nocapture'`
(only when a CD-TEXT disc is loaded).

- [ ] **Step 2: Record known-limitations in the spec**

Add a Phase-9 section noting: whole-entry precedence (no per-field gap-fill,
no toggle — Winamp behavior, supersedes the design doc's `merge_disc_metadata`
gap-fill proposal); macOS burn-side CD-TEXT NOT implemented (drutil CLI has no
v07t input; needs the DiscRecording framework — see this plan's deferred
appendix); `drutil cdtext` read-format verified on real hardware during the
mac pass; CD-TEXT text is length-capped/NUL-sanitized on display.

- [ ] **Step 3: Full gate + ledger**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: zero warnings, all pass (confirm the disc flaky test single-threaded).
Update `.superpowers/sdd/progress.md` and the roadmap memory with Phase-9
completion and the next roadmap item (Phase 10 F11/F12 settings cluster).

- [ ] **Step 4: Commit**

```bash
git add src/disc/cdtext.rs docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md
git commit -m "test(disc): ignored live_cdtext_read + phase-9 known-limitations"
```

---

## Appendix — DEFERRED: macOS burn-side CD-TEXT (DiscRecording framework)

**Not built in this phase** (user decision 2026-07-28: read-side only now; a
macOS-run Claude implements/tests the burn side on real hardware later). This
section is the implementation brief for that future work.

**Problem:** Linux writes CD-TEXT when burning audio discs via `cdrskin
input_sheet_v07t=<sheet>` (`src/disc/burn.rs:771-787`, sheet built by
`build_v07t`). The macOS burn path shells `drutil -audio` (`drutil_audio_args`,
`burn.rs:173`), and drutil's CLI has **no documented CD-TEXT/v07t input** — so
mac burns audio without CD-TEXT.

**Fix (framework, not CLI):** Replace the mac audio-burn CLI call with Apple's
DiscRecording framework, which supports CD-TEXT write (Red Book, ISO Latin-1
Music CD variant):

1. **Link the framework:** add `DiscRecording.framework` to the mac target in
   `project.pbxproj` (framework build phase + fileRef, mirroring how other
   frameworks are linked).
2. **Build the burn from tracks:** for each staged Red Book WAV, create a
   `DRTrack` (audio, 44.1 kHz / 16-bit / stereo). Assemble them into a
   `DRTrackArray` for a single audio session.
3. **Attach CD-TEXT:** build a `DRCDTextBlock` (language 0 / English). Set the
   disc-level `DRCDTextTitleKey` / `DRCDTextPerformerKey` from the same
   `DiscMeta` the v07t sheet uses (album title, artist name), and per-track
   `DRCDTextTitleKey` / `DRCDTextPerformerKey` from each burn item's display
   line (reuse `build_v07t`'s split logic: "Artist - Title" → performer/title,
   whole string when untagged). Assign the block via the burn's
   `DRCDTextKey` / the session's CD-TEXT property.
4. **Burn:** run a `DRBurn` on the `DRDevice` with the track array + CD-TEXT.
   Wire progress to the existing mac burn-progress UI.
5. **Data source:** the exact same metadata that `build_v07t(meta, items)`
   consumes — do NOT duplicate the split/sanitize rules; either call a small
   FFI that returns the parsed (title, performer) pairs, or mirror
   `cdtext.rs::split_display` byte-for-byte in Swift with a drift note.
6. **Verify on hardware:** burn a disc, then read it back with `drutil cdtext`
   (or on Linux `cdrskin cdtext_to_v07t=-`) and confirm album/artist/track
   titles survived — the mirror of the Linux live burn+readback test
   (`burn.rs:911`).
7. **Fallback / gap:** if the framework route is not taken, mac audio burns
   remain CD-TEXT-less (documented limitation). `cdrskin` via Homebrew is a
   non-default alternative but not Apple-native.

**Checklist:** append a "Phase 9 — mac burn CD-TEXT" section to
`docs/mac-pass-checklist.md` when this is implemented, covering the burn +
readback round-trip and character-encoding edge cases (accented / non-ASCII
titles).

---

## Self-Review

- **Spec coverage:** design-doc read path → Tasks 1-4; precedence (locked
  whole-entry) → Global Constraints + Tasks 3/4 (GTK already done);
  drive-contention guard → Tasks 2/3; probe-time-only + cache → Tasks 3/4
  one-shot `_tried`; automated parser tests → Task 1; overlay precedence test
  → Task 3; live test → Task 5; manual test plan → mac checklist (Task 4) +
  TUI covered by Task 3 wiring. Design-doc's `merge_disc_metadata` gap-fill
  fn is intentionally NOT built (user chose Winamp whole-entry; recorded in
  Task 5 known-limitations). Open Question #1 (prefer-CD-TEXT toggle) resolved
  NO.
- **Placeholder scan:** every code step carries real code; the one genuine
  unknown (`drutil cdtext` format) is explicitly flagged with a
  verify-on-hardware step rather than hidden.
- **Type consistency:** `read_cdtext(&str) -> Option<CdText>` identical
  across Linux/macOS arms; `to_xmcd(&str) -> XmcdEntry` reused; FFI returns
  `XmcdEntry` JSON consumed by the same overlay code gnudb entries use;
  `disc_cdtext`/`disc_tags` both `HashMap<String, XmcdEntry>` so `.or_else`
  type-checks.
- **GTK:** already implements the full flow at the locked precedence — no GTK
  task; verified against `media_library.rs:9031-9108`.
