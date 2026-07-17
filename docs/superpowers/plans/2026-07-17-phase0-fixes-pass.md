# Phase 0 — Fixes Pass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land every phase-0 fix from the Winamp-parity roadmap: B1+B2+B7 (ID3 extended-field wiring), B3 (bind `u`, truthful shortcuts dialog), B4 (title casing), B5 (APIC mime), D8 (mac playlist autoscroll), D10 (mac EQ labels), D13 (GTK genre free-text typeahead), D16 (verify-discs toggle), D17 (granite beat settings). B6 is already resolved (CLAUDE.md line 77 has the correct skins path) — verify and move on.

**Architecture:** Core-first. The six ID3 fields the GTK editor drops (composer, original_artist, copyright, url, encoded_by, lyric) move into `TagFields` in `src/id3_editor.rs`, which fixes GTK (B1), mac (most of B2), and the TUI in one stroke. Frames outside `TagFields` (mac Customize offers TEXT, TIT3, TPUB, TKEY, TMOO, TLAN, TSRC) get a generic FFI passthrough that finally uses the dead `write_extra_frame` machinery (B7). Everything else is a localized frontend fix.

**Tech Stack:** Rust (id3 crate, GTK4 via gtk4-rs), SwiftUI/AppKit (written blind — cannot compile on this box).

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Never gate on `cargo build --lib` — GTK code only compiles in the bin target.
- Zero warnings, zero failures before any "done" claim. Baseline: 1015 tests; the count must not drop.
- Branch: `album-art-improvements`. NEVER `git push` without a fresh explicit user instruction.
- Comments: plain English, explain why not what (CLAUDE.md).
- Swift changes cannot be compiled here — append a verification item to `docs/mac-pass-checklist.md` (follow its existing format) in the same commit as each Swift change.
- User-facing casing is "Sparkamp".
- Config fields use `#[serde(default)]` + `Default` impl (existing fields here already do).
- Commit style: conventional prefix, body = why + verification line, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: Extend `TagFields` with the six dropped fields (B1 core)

**Files:**
- Modify: `src/id3_editor.rs` (struct ~line 240, `field_pairs` ~264, `read_tag_fields` ~311, `read_extra_frames` DEFAULT_IDS ~374, `write_tag_fields` ~412, tests module at end of file)
- Modify: `frontends/tui/mod.rs:963-983` (`id3_field_value_mut`)
- Modify: `frontends/tui/ui/id3.rs:58` (comment says "12 (label, value) pairs")

**Interfaces:**
- Produces: `TagFields { composer, original_artist, copyright, url, encoded_by, lyric: String }` — Tasks 2 and 3 read/write these exact field names. Frame mapping (must match the scanner in `src/tags.rs:114-119`): TCOM, TOPE, TCOP, WXXX, TENC, USLT.

- [ ] **Step 1: Read the existing tests module** at the bottom of `src/id3_editor.rs` (a roundtrip test asserting composer lives near line 792). Reuse its temp-file setup style in the next step.

- [ ] **Step 2: Write the failing test** (in the existing `#[cfg(test)]` module):

```rust
#[test]
fn extended_fields_roundtrip() {
    // The six fields the GTK editor used to drop (B1) must survive a
    // write/read cycle, including the two non-text frames (WXXX, USLT).
    let path = std::env::temp_dir().join("sparkamp_ext_fields_test.mp3");
    std::fs::write(&path, b"").unwrap();

    let fields = TagFields {
        title: "T".into(),
        composer: "A Composer".into(),
        original_artist: "Orig Artist".into(),
        copyright: "(c) 2026".into(),
        url: "https://example.com/a".into(),
        encoded_by: "Sparkamp".into(),
        lyric: "la la\nla".into(),
        ..TagFields::default()
    };
    write_tag_fields(&path, &fields).unwrap();

    let back = read_tag_fields(&path);
    assert_eq!(back.composer, "A Composer");
    assert_eq!(back.original_artist, "Orig Artist");
    assert_eq!(back.copyright, "(c) 2026");
    assert_eq!(back.url, "https://example.com/a");
    assert_eq!(back.encoded_by, "Sparkamp");
    assert_eq!(back.lyric, "la la\nla");

    // Clearing a field must remove its frame.
    let mut cleared = back.clone();
    cleared.lyric = String::new();
    cleared.url = String::new();
    write_tag_fields(&path, &cleared).unwrap();
    let back2 = read_tag_fields(&path);
    assert_eq!(back2.lyric, "");
    assert_eq!(back2.url, "");

    std::fs::remove_file(&path).ok();
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test extended_fields_roundtrip'`
Expected: compile error — `TagFields` has no field `composer`.

- [ ] **Step 4: Implement.** Add to the `TagFields` struct (after `comment`, before `artwork_path`):

```rust
    pub composer: String,        // TCOM
    pub original_artist: String, // TOPE
    pub copyright: String,       // TCOP
    pub url: String,             // WXXX — a link frame, not a text frame
    pub encoded_by: String,      // TENC
    pub lyric: String,           // USLT — unsynchronised lyrics content
```

In `read_tag_fields`, before the final struct literal:

```rust
    let get_extended = |frame_id: &str| -> String {
        tag.get(frame_id)
            .and_then(|f| f.content().text())
            .unwrap_or("")
            .to_string()
    };
    // WXXX carries ExtendedLink content — pull the link out explicitly
    // rather than relying on Content::text() covering link frames.
    let url = tag
        .get("WXXX")
        .map(|f| match f.content() {
            id3::Content::ExtendedLink(e) => e.link.clone(),
            c => c.text().unwrap_or("").to_string(),
        })
        .unwrap_or_default();
```

and in the struct literal:

```rust
        composer: get_extended("TCOM"),
        original_artist: get_extended("TOPE"),
        copyright: get_extended("TCOP"),
        url,
        encoded_by: get_extended("TENC"),
        lyric: tag.lyrics().next().map(|l| l.text.clone()).unwrap_or_default(),
```

In `write_tag_fields`, after `set_text!("TBPM", &fields.bpm);`:

```rust
    set_text!("TCOM", &fields.composer);
    set_text!("TOPE", &fields.original_artist);
    set_text!("TCOP", &fields.copyright);
    set_text!("TENC", &fields.encoded_by);

    // WXXX is a link frame — set_text would serialize it as a malformed
    // text frame, so build the ExtendedLink content explicitly.
    tag.remove("WXXX");
    if !fields.url.is_empty() {
        tag.add_frame(id3::Frame::with_content(
            "WXXX",
            id3::Content::ExtendedLink(id3::frame::ExtendedLink {
                description: String::new(),
                link: fields.url.clone(),
            }),
        ));
    }

    // USLT likewise carries Lyrics content rather than plain text.
    tag.remove("USLT");
    if !fields.lyric.is_empty() {
        tag.add_frame(id3::frame::Lyrics {
            lang: "eng".to_string(),
            description: String::new(),
            text: fields.lyric.clone(),
        });
    }
```

Extend `read_extra_frames` DEFAULT_IDS so the Customize panel stops offering frames the main editor now owns:

```rust
    const DEFAULT_IDS: &[&str] = &[
        "TIT2", "TPE1", "TALB", "TPE2", "TCON", "TDRC", "TRCK", "TPOS", "TBPM", "COMM",
        "TCOM", "TOPE", "TCOP", "WXXX", "TENC", "USLT",
    ];
```

Append to `field_pairs()` (after Comment) and update its doc comment:

```rust
            ("Composer", self.composer.clone()),
            ("Original Artist", self.original_artist.clone()),
            ("Copyright", self.copyright.clone()),
            ("URL", self.url.clone()),
            ("Encoded By", self.encoded_by.clone()),
            ("Lyric", self.lyric.clone()),
```

- [ ] **Step 5: Fix the TUI index map.** `frontends/tui/mod.rs` `id3_field_value_mut` routes edits by `field_pairs` index; without new arms every new field would silently edit `comment` via the `_` arm. Replace the tail and update the doc comment (0-11 unchanged, then):

```rust
        10 => &mut fields.bpm,
        11 => &mut fields.comment,
        12 => &mut fields.composer,
        13 => &mut fields.original_artist,
        14 => &mut fields.copyright,
        15 => &mut fields.url,
        16 => &mut fields.encoded_by,
        _ => &mut fields.lyric,
```

Update the `frontends/tui/ui/id3.rs:58` comment ("12 (label, value) pairs" → 18).

- [ ] **Step 6: Run the full suite**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: builds warning-free; `extended_fields_roundtrip` passes; no other test regresses.

- [ ] **Step 7: Commit**

```bash
git add src/id3_editor.rs frontends/tui/mod.rs frontends/tui/ui/id3.rs
git commit -m "fix(id3): carry composer/orig-artist/copyright/url/encoded-by/lyric in TagFields"
```

---

### Task 2: GTK editor writes the six fields (B1 UI)

**Files:**
- Modify: `frontends/gtk/window/id3.rs` (`get_id3_field_value` lines 1-46, `do_save` TagFields literal lines 1058-1113)

**Interfaces:**
- Consumes: Task 1's `TagFields` fields (exact names above).

- [ ] **Step 1: Read fields from the tag, not the DB.** In `get_id3_field_value`, the six arms currently read `track_meta` (the ML DB row — stale for files not in the library). Point them at the freshly-read tag instead:

```rust
        "composer" => fields.composer.clone(),
        "original_artist" => fields.original_artist.clone(),
        "copyright" => fields.copyright.clone(),
        "url" => fields.url.clone(),
        "encoded_by" => fields.encoded_by.clone(),
        "lyric" => fields.lyric.clone(),
```

(The `track_meta` parameter becomes unused — remove it and its two call sites' argument, or keep it only if another field still needs it; after this change none does, so remove it to stay warning-free.)

- [ ] **Step 2: Collect the six entries on save.** In `do_save`'s `TagFields` literal add (before `artwork_path`):

```rust
                composer: entries
                    .get("composer")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                original_artist: entries
                    .get("original_artist")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                copyright: entries
                    .get("copyright")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                url: entries
                    .get("url")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                encoded_by: entries
                    .get("encoded_by")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                lyric: entries
                    .get("lyric")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
```

CAUTION: a field the user has hidden via the ID3 column config has no entry in the map, and `""` makes `write_tag_fields` REMOVE that frame — saving with Composer hidden would silently strip composers. Guard the six new fields by falling back to the value read from disk: capture `let fields_snapshot = fields.clone();` in `do_save`'s closure set-up, and use `.unwrap_or_else(|| fields_snapshot.composer.clone())` (per field) instead of `.unwrap_or_default()`. Leave the twelve original fields exactly as they are in this task — changing their (long-standing) hidden-field behavior is out of scope.

- [ ] **Step 3: Build + full suite**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: green, zero warnings.

- [ ] **Step 4: Commit**

```bash
git add frontends/gtk/window/id3.rs
git commit -m "fix(gtk): save the six extended ID3 fields instead of dropping them (B1)"
```

---

### Task 3: FFI passthrough for extended + arbitrary frames (B2 + B7)

**Files:**
- Modify: `src/ffi/id3.rs` (struct ~13, `sparkamp_tag_get` ~53, `sparkamp_tag_set` ~79, `sparkamp_tag_save` ~151; add tests module)

**Interfaces:**
- Consumes: Task 1's `TagFields` fields; `crate::id3_editor::write_extra_frame(path, frame_id, value)`.
- Produces: no new FFI symbols — `sparkamp_bridge.h` unchanged. Existing `sparkamp_tag_get/set` now honor TCOM/TOPE/TCOP/WXXX/TENC/USLT plus any other `T*` frame (mac Customize: TEXT, TIT3, TPUB, TKEY, TMOO, TLAN, TSRC).

- [ ] **Step 1: Write the failing test** (new `#[cfg(test)]` module at the bottom of `src/ffi/id3.rs`):

```rust
#[cfg(test)]
mod tests {
    use std::ffi::CString;

    // Round-trip a TagFields-backed frame and a passthrough frame through
    // the raw FFI surface the mac editor uses (B2/B7).
    #[test]
    fn ffi_extended_and_passthrough_roundtrip() {
        let path = std::env::temp_dir().join("sparkamp_ffi_tag_test.mp3");
        std::fs::write(&path, b"").unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();

        unsafe {
            let ctx = super::sparkamp_tag_open(c_path.as_ptr());
            assert!(!ctx.is_null());
            let set = |ctx, id: &str, v: &str| {
                let id = CString::new(id).unwrap();
                let v = CString::new(v).unwrap();
                super::sparkamp_tag_set(ctx, id.as_ptr(), v.as_ptr());
            };
            set(ctx, "TCOM", "A Composer");
            set(ctx, "TPUB", "A Publisher"); // not in TagFields — passthrough
            assert_eq!(super::sparkamp_tag_save(ctx), 0);
            super::sparkamp_tag_close(ctx);

            let ctx2 = super::sparkamp_tag_open(c_path.as_ptr());
            let get = |ctx, id: &str| -> String {
                let id = CString::new(id).unwrap();
                let p = super::sparkamp_tag_get(ctx, id.as_ptr());
                let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
                super::sparkamp_free_string(p);
                s
            };
            assert_eq!(get(ctx2, "TCOM"), "A Composer");
            assert_eq!(get(ctx2, "TPUB"), "A Publisher");
            super::sparkamp_tag_close(ctx2);
        }
        std::fs::remove_file(&path).ok();
    }
}
```

(If `sparkamp_free_string` lives in another ffi module, call it via `crate::ffi::` path — check `src/ffi/mod.rs`.)

- [ ] **Step 2: Run to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test ffi_extended_and_passthrough_roundtrip'`
Expected: FAIL — TPUB (and TCOM) come back empty.

- [ ] **Step 3: Implement.** Add to `SparkampTagCtx`:

```rust
    /// Values set via sparkamp_tag_set for frames outside TagFields —
    /// written with write_extra_frame on save. This is what finally uses
    /// the extra-frame write path (B7) for the mac Customize fields (B2).
    pending_extra: Vec<(String, String)>,
```

(initialize `pending_extra: Vec::new()` in `sparkamp_tag_open`).

`sparkamp_tag_get` — add TagFields arms and a passthrough fallback:

```rust
        "TCOM" => &tag.fields.composer,
        "TOPE" => &tag.fields.original_artist,
        "TCOP" => &tag.fields.copyright,
        "WXXX" => &tag.fields.url,
        "TENC" => &tag.fields.encoded_by,
        "USLT" => &tag.fields.lyric,
        other => {
            // Pending writes win over what was read from disk.
            let v = tag
                .pending_extra
                .iter()
                .rev()
                .find(|(id, _)| id == other)
                .map(|(_, v)| v.as_str())
                .or_else(|| {
                    tag.extra_frames
                        .iter()
                        .find(|f| f.id == other)
                        .map(|f| f.value.as_str())
                })
                .unwrap_or("");
            return CString::new(v).unwrap_or_default().into_raw();
        }
```

`sparkamp_tag_set` — mirror arms plus a generic text-frame arm:

```rust
        "TCOM" => tag.fields.composer = val,
        "TOPE" => tag.fields.original_artist = val,
        "TCOP" => tag.fields.copyright = val,
        "WXXX" => tag.fields.url = val,
        "TENC" => tag.fields.encoded_by = val,
        "USLT" => tag.fields.lyric = val,
        other if other.starts_with('T') => {
            tag.pending_extra.retain(|(id, _)| id != other);
            tag.pending_extra.push((other.to_string(), val));
        }
        _ => {}
```

`sparkamp_tag_save` — write pending frames after the main fields:

```rust
    match crate::id3_editor::write_tag_fields(path, &tag.fields) {
        Ok(_) => {}
        Err(_) => return -2,
    }
    for (id, value) in &tag.pending_extra {
        // write_extra_frame re-reads and rewrites the tag per frame; the
        // Customize panel tops out at a handful of frames, so that's fine.
        if crate::id3_editor::write_extra_frame(path, id, value).is_err() {
            return -2;
        }
    }
    0
```

If `ExtraFrame`'s `#[allow(dead_code)]` becomes unnecessary once this lands, remove the attribute.

- [ ] **Step 4: Run full suite**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: green, zero warnings.

- [ ] **Step 5: Append to `docs/mac-pass-checklist.md`** (existing format): verify the mac ID3 editor saves Composer/Copyright/Encoded-by and Customize-only frames (Publisher, Key, Mood, …) and they survive reopen.

- [ ] **Step 6: Commit**

```bash
git add src/ffi/id3.rs docs/mac-pass-checklist.md
git commit -m "fix(ffi): tag get/set honors extended TagFields and passes through T-frames (B2/B7)"
```

---

### Task 4: Correct APIC mime for GIF/WebP (B5)

**Files:**
- Modify: `src/id3_editor.rs:496-508` (artwork embed in `write_tag_fields`), test in same file

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn artwork_mime_matches_extension() {
    // Embedding a .gif/.webp must not claim image/jpeg (B5) — players
    // decode by the declared mime and render garbage otherwise.
    let art = std::env::temp_dir().join("sparkamp_mime_test.GIF");
    std::fs::write(&art, b"GIF89a fake").unwrap();
    let song = std::env::temp_dir().join("sparkamp_mime_test.mp3");
    std::fs::write(&song, b"").unwrap();

    let fields = TagFields {
        artwork_path: art.to_string_lossy().into_owned(),
        ..TagFields::default()
    };
    write_tag_fields(&song, &fields).unwrap();

    let tag = id3::Tag::read_from_path(&song).unwrap();
    let pic = tag.pictures().next().unwrap();
    assert_eq!(pic.mime_type, "image/gif");

    std::fs::remove_file(&art).ok();
    std::fs::remove_file(&song).ok();
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test artwork_mime_matches_extension'`
Expected: FAIL — mime is `image/jpeg`.

- [ ] **Step 3: Implement.** Replace the `let mime = if … ".png" …` block with an extension match (case-insensitive — note the test uses an uppercase extension):

```rust
                let mime = match art_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .as_deref()
                {
                    Some("png") => "image/png",
                    Some("gif") => "image/gif",
                    Some("webp") => "image/webp",
                    // jpg/jpeg and anything unrecognized — keep the old
                    // default so behavior only changes where it was wrong.
                    _ => "image/jpeg",
                };
```

- [ ] **Step 4: Run full suite** — same command, expect green + zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/id3_editor.rs
git commit -m "fix(id3): declare correct APIC mime for gif/webp artwork (B5)"
```

---

### Task 5: Bind `u` to the EQ + truthful shortcuts dialog (B3)

**Files:**
- Modify: `frontends/gtk/window/player.rs` (`handle_key` clones ~3470-3500 and match arms ~3768-3785; shortcuts `sections` array 3846-3882)

- [ ] **Step 1: Bind `u`.** `btn_eq` (built at ~line 408) already carries the "(u)" tooltip and its click handler (wired later, ~4241) toggles the singleton EQ window — route the key through the button exactly like `i` routes through `kbd_btn_info`. Add with the other clones before `Rc::new(move |key…`:

```rust
        let kbd_btn_eq = btn_eq.clone();
```

and a match arm next to the info arm:

```rust
                // ── Equalizer toggle (u) — same path as the EQ button so
                // the singleton/active-CSS logic stays in one place ────────
                gdk::Key::u | gdk::Key::U => {
                    kbd_btn_eq.emit_clicked();
                    glib::Propagation::Stop
                }
```

(`btn_eq.connect_clicked` is wired after `handle_key` is built but before any key event can fire — same deferred pattern the fullscreen opener uses, so this is safe.)

- [ ] **Step 2: Fix the dialog claims** in the `sections` array:
  - `("↑ k / ↓ l",  "Browse up / down")` → `("↑ ↓", "Browse up / down")` — k/l are not bound anywhere.
  - `("u", "Open EQ (TUI only — use EQ button in GUI)")` → `("u", "Toggle equalizer window")`.
  - `("q / Esc", "Quit")` → `("q", "Quit")` — Esc closes/hides child windows, it does not quit.

- [ ] **Step 3: Build + full suite** — distrobox command, expect green + zero warnings.

- [ ] **Step 4: Commit**

```bash
git add frontends/gtk/window/player.rs
git commit -m "fix(gtk): bind u to the equalizer and correct shortcut-dialog claims (B3)"
```

---

### Task 6: Title casing "SparkAmp" → "Sparkamp" (B4)

**Files:**
- Modify: `frontends/gtk/window/eq.rs:14`, `frontends/gtk/window/settings.rs:11`, `frontends/gtk/window/player.rs:145` and `:585`, plus the comments at `frontends/gtk/window/util.rs:338` and `frontends/gtk/window/state.rs:676`

- [ ] **Step 1: Replace every occurrence.** `grep -rn "SparkAmp" src/ frontends/` and change each to "Sparkamp" (window titles are user-facing; the two comments are fixed for consistency).

- [ ] **Step 2: Verify zero remain:** `grep -rn "SparkAmp" src/ frontends/` → no output.

- [ ] **Step 3: Build + full suite** — expect green.

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "fix(gtk): correct Sparkamp casing in window titles (B4)"
```

---

### Task 7: GTK genre = free-text entry with predefined typeahead (D13)

**Files:**
- Modify: `frontends/gtk/window/util.rs:154-207` (replace `make_genre_combo`)
- Modify: `frontends/gtk/window/id3.rs:834-847` and `:866-877` (both call sites)

- [ ] **Step 1: Replace `make_genre_combo` with `make_genre_entry`:**

```rust
#[allow(deprecated)] // EntryCompletion/ListStore — no GTK4 replacement yet
fn make_genre_entry(initial_value: &str) -> gtk4::Entry {
    // Free-text entry with typeahead over the predefined ID3v1 list only —
    // matches the mac editor (D13): suggestions come from the list, but
    // any typed value is accepted and saved verbatim.
    let entry = Entry::new();
    entry.set_text(initial_value);

    let mut genres: Vec<&str> = crate::id3_editor::ID3V1_GENRES.to_vec();
    genres.sort_unstable_by_key(|g| g.to_ascii_lowercase());
    let store = gtk4::ListStore::new(&[glib::types::Type::STRING]);
    for g in &genres {
        store.set(&store.append(), &[(0, g)]);
    }
    let completion = gtk4::EntryCompletion::new();
    completion.set_model(Some(&store));
    completion.set_text_column(0);
    completion.set_popup_completion(true);
    completion.set_minimum_key_length(1);
    entry.set_completion(Some(&completion));
    entry
}
```

- [ ] **Step 2: Update both call sites** in `id3.rs` (left- and right-column loops are near-identical; the grid column differs — 1 vs 3):

```rust
        if *id == "genre" {
            let entry = make_genre_entry(&value);
            entry.set_hexpand(true);
            grid.attach(&entry, 1, row as i32, 1, 1);
            left_entries.push((id.to_string(), entry));
        } else {
```

The old "hidden carrier entry" comment and dropdown die with `make_genre_combo` — the entry itself is now registered, so Save reads it directly.

- [ ] **Step 3: Build + full suite** — expect green, zero warnings (confirm no dangling `make_genre_combo` reference).

- [ ] **Step 4: Commit**

```bash
git add frontends/gtk/window/util.rs frontends/gtk/window/id3.rs
git commit -m "fix(gtk): genre becomes free-text with predefined-only typeahead (D13)"
```

---

### Task 8: "Verify discs after burning" toggle in GTK Settings (D16)

**Files:**
- Modify: `frontends/gtk/window/settings.rs` (disc rows live near the gnudb email at ~368 and `chk_autocd` at ~405)

- [ ] **Step 1: Read the `chk_autocd` block** (~395-425) to get the exact container variable and config-save idiom used there.

- [ ] **Step 2: Add the toggle** directly after it, same container, mirroring that idiom:

```rust
        let chk_verify = CheckButton::with_label("Verify discs after burning");
        chk_verify.set_active(state.borrow().config.disc.burn_verify);
        chk_verify.connect_toggled({
            let state = state.clone();
            move |c| {
                let mut s = state.borrow_mut();
                s.config.disc.burn_verify = c.is_active();
                let _ = s.config.save();
            }
        });
```

(append to the same parent box the `chk_autocd` block appends to — reuse whatever helper/label wrapper that block uses so the rows look identical; mac already has this toggle via `sparkamp_set_burn_verify`, so no FFI work.)

- [ ] **Step 3: Build + full suite** — expect green.

- [ ] **Step 4: Commit**

```bash
git add frontends/gtk/window/settings.rs
git commit -m "feat(gtk): expose burn_verify as a Settings toggle (D16)"
```

---

### Task 9: Granite beat sensitivity + brightness in GTK Settings (D17)

**Files:**
- Modify: `frontends/gtk/window/settings.rs` (Granite rows: speed Scale ~832, feedback Scale ~897 — the pattern to copy)

- [ ] **Step 1: Read the speed-scale block** (~820-895) — note its label/row construction AND whether it applies the value live to the running visualizer (a `player.` / renderer call beside the config write). Copy both behaviors exactly. Also `grep -n "beat_sensitivity" frontends/gtk/` — if the renderer reads config each frame, the config write alone suffices; if it caches, mirror the speed scale's live-apply call.

- [ ] **Step 2: Add the sensitivity scale** after the feedback row, same construction as the speed row (range matches the FFI clamp in `src/ffi/granite.rs:233` — 1.05..=3.0):

```rust
        let sens_adj = gtk4::Adjustment::new(
            state.borrow().config.visualizer.granite.beat_sensitivity as f64,
            1.05, 3.0, 0.05, 0.1, 0.0,
        );
        let scale_gr_sens = Scale::new(Orientation::Horizontal, Some(&sens_adj));
        scale_gr_sens.set_hexpand(true);
        sens_adj.connect_value_changed({
            let state = state.clone();
            move |a| {
                let mut s = state.borrow_mut();
                s.config.visualizer.granite.beat_sensitivity = a.value() as f32;
                let _ = s.config.save();
            }
        });
```

with a "Beat sensitivity" label row matching the neighbors.

- [ ] **Step 3: Add the brightness toggle** below it:

```rust
        let chk_gr_bright = CheckButton::with_label("Brighten colors on beats");
        chk_gr_bright.set_active(state.borrow().config.visualizer.granite.beat_brightness);
        chk_gr_bright.connect_toggled({
            let state = state.clone();
            move |c| {
                let mut s = state.borrow_mut();
                s.config.visualizer.granite.beat_brightness = c.is_active();
                let _ = s.config.save();
            }
        });
```

- [ ] **Step 4: Build + full suite** — expect green, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add frontends/gtk/window/settings.rs
git commit -m "feat(gtk): granite beat sensitivity and brightness settings (D17)"
```

---

### Task 10: mac playlist auto-scrolls to current track (D8) — BLIND Swift

**Files:**
- Modify: `frontends/SparkampMac/Sources/PlaylistView.swift` (an NSViewRepresentable table; `parent.model.currentIndex` already drives `isCurrent` at lines 223/283)
- Modify: `docs/mac-pass-checklist.md`

- [ ] **Step 1: Read `PlaylistView.swift`** to find the coordinator and the `updateNSView` path that runs on model changes.

- [ ] **Step 2: Add scroll-on-change.** In the coordinator add `var lastScrolledIndex: Int = -1`, and where `updateNSView` refreshes rows:

```swift
        // Auto-scroll to the current track on track change (D8) — mirrors
        // the GTK frontend's scroll_to_row_if_needed. Tracks the last index
        // so user scrolling isn't fought while the same track plays.
        let cur = parent.model.currentIndex
        if cur >= 0, cur != context.coordinator.lastScrolledIndex,
           cur < tableView.numberOfRows {
            tableView.scrollRowToVisible(cur)
            context.coordinator.lastScrolledIndex = cur
        }
```

(Adapt names to the actual coordinator/table variables found in Step 1 — this cannot be compiled here.)

- [ ] **Step 3: Append to `docs/mac-pass-checklist.md`:** playlist scrolls to the playing row on every track change (auto-advance, z/b, double-click), and does not yank the view while the same track keeps playing.

- [ ] **Step 4: Build + full suite** (Rust unaffected — run anyway to keep the gate honest). Expected: green.

- [ ] **Step 5: Commit**

```bash
git add frontends/SparkampMac/Sources/PlaylistView.swift docs/mac-pass-checklist.md
git commit -m "fix(mac): auto-scroll playlist to current track on change (D8)"
```

---

### Task 11: Remove mac EQ frequency labels (D10) — BLIND Swift

**Files:**
- Modify: `frontends/SparkampMac/Sources/EqualizerWindow.swift` (`BandSliderColumn`, ~lines 195-235)
- Modify: `docs/mac-pass-checklist.md`

- [ ] **Step 1: Delete the label.** In `BandSliderColumn` remove: the `@State private var labelText` property, the `// Frequency label` `Text(labelText)…` view block, and the `.onAppear { … sparkamp_eq_band_label … }` modifier. Keep the slider and its frames untouched. Do NOT remove the `sparkamp_eq_band_label` FFI symbol — the TUI still uses band labels.

- [ ] **Step 2: Append to `docs/mac-pass-checklist.md`:** EQ window shows 10 unlabeled sliders (matches GTK), column spacing intact.

- [ ] **Step 3: Build + full suite** — expected green (Rust unaffected).

- [ ] **Step 4: Commit**

```bash
git add frontends/SparkampMac/Sources/EqualizerWindow.swift docs/mac-pass-checklist.md
git commit -m "fix(mac): drop EQ frequency labels to match GTK (D10)"
```

---

### Task 12: Phase gate — B6 verification + final sweep

**Files:**
- Verify only (no expected changes): `CLAUDE.md`

- [ ] **Step 1: B6 check:** `grep -n "local/share" CLAUDE.md` → expect no output (line 77 already documents `~/.config/sparkamp/skins/`). If something new appears, fix it to the config path; otherwise B6 is a recorded no-op.

- [ ] **Step 2: Full gate:**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: zero warnings, zero failures, test count ≥ 1015 + the 3 new tests.

- [ ] **Step 3: Report to the user** for interactive GTK verification: u toggles EQ; shortcuts dialog text; genre typeahead accepts free text; Settings shows verify-discs + granite beat rows; ID3 editor round-trips composer/lyric/url; a GIF cover embeds with `image/gif`. Mac items wait on `docs/mac-pass-checklist.md`.
