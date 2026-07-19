# Phase 1 — Metadata Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture real technical metadata (sample rate, bitrate, channels, file size, mtime, added-at, VBR/CBR) in the scanner + schema, surface it as ML columns and an ID3-window tech line on GTK/mac/TUI, add the folder-image artwork fallback, and skin the settings-tab widgets.

**Architecture:** Core-first. A new `src/technical_probe.rs` owns codec probing (Symphonia) and bitrate math; `upsert_track` is the single write seam that fills the new `tracks` columns; the metadata-pass WHERE clause is extended so the existing ~36k rows (all with NULL bitrate/channels today — the capture path never existed) backfill on the next rescan. Display layers only format what the DB provides. F2 changes only `src/tags.rs` + a safety guard in `refresh_artwork`. B8 changes only `src/skin.rs`.

**Tech Stack:** Rust (rusqlite, symphonia — already a dependency, id3), GTK4, SwiftUI/AppKit (blind), Ratatui.

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Never gate on `--lib` — GTK code only compiles in the bin target.
- Zero warnings, zero failures. Floor at plan time: **1021 passed (411 lib + 610 bin)**; quote BOTH result lines in reports.
- Branch `album-art-improvements` (checked out; pushed to origin — pushing again still requires a fresh explicit user instruction).
- Comments: plain English, why not what. User-facing casing "Sparkamp".
- macOS Swift is written blind — flag it, and append verification items to `docs/mac-pass-checklist.md` in the same commit. New/changed FFI structs or symbols are hand-added to `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` (no cbindgen).
- Config fields (none expected this phase) would use `#[serde(default)]` + `Default`.
- RefCell borrows short-lived — never held across a UI call.
- Commit style: conventional prefix, body = why + a verification line, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Keep new files under ~800 lines; carve related chunks out of oversized files only when a task touches them.

## File Structure

- Create: `src/technical_probe.rs` — codec-parameter probe + bitrate math + MP3 VBR/CBR sniff (pure logic, unit-testable, no DB).
- Modify: `src/media_library/mod.rs` (schema `new_cols`, `LibTrack`, row mapper, `ReadOnlyTrackFields`), `src/media_library/scan.rs` (`upsert_track`, metadata-pass WHERE), `src/media_library/queries.rs` (5 SELECT lists, sort map, `refresh_artwork` guard), `src/tags.rs` (folder fallback), `src/skin.rs` (B8 CSS), `frontends/gtk/window/ml_columns.rs` (+ its value match), `frontends/gtk/window/id3.rs` (tech line), TUI id3 screen, mac `MLFilesTable.swift`/`Id3EditorWindow.swift`/model + `src/ffi/media_library.rs`.

---

### Task 1: `technical_probe` module — codec params + bitrate math (F13 core)

**Files:**
- Create: `src/technical_probe.rs`
- Modify: `src/lib.rs` (register `pub mod technical_probe;` beside the other modules — check `src/lib.rs` for the existing `pub mod` list and match its ordering style)

**Interfaces:**
- Produces (contract for Tasks 2, 4, 5):
  - `pub struct TechProbe { pub sample_rate: Option<i64>, pub channels: Option<i64> }`
  - `pub fn probe_technical(path: &Path) -> TechProbe`
  - `pub fn avg_bitrate_kbps(file_size_bytes: u64, length_secs: f64) -> Option<i64>`

- [ ] **Step 1: Write the failing tests** (new file with a `#[cfg(test)]` module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid PCM WAV: 44-byte header + one frame. Symphonia parses
    // this from the header alone — no fixtures needed, fully deterministic.
    fn write_test_wav(path: &std::path::Path, sample_rate: u32, channels: u16) {
        let data_len = (channels as u32) * 2; // one 16-bit frame
        let byte_rate = sample_rate * channels as u32 * 2;
        let block_align = channels * 2;
        let mut buf = Vec::new();
        buf.extend(b"RIFF");
        buf.extend(&(36 + data_len).to_le_bytes());
        buf.extend(b"WAVE");
        buf.extend(b"fmt ");
        buf.extend(&16u32.to_le_bytes());
        buf.extend(&1u16.to_le_bytes()); // PCM
        buf.extend(&channels.to_le_bytes());
        buf.extend(&sample_rate.to_le_bytes());
        buf.extend(&byte_rate.to_le_bytes());
        buf.extend(&block_align.to_le_bytes());
        buf.extend(&16u16.to_le_bytes()); // bits per sample
        buf.extend(b"data");
        buf.extend(&data_len.to_le_bytes());
        buf.extend(std::iter::repeat(0u8).take(data_len as usize));
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn probe_reads_sample_rate_and_channels_from_wav_header() {
        let p = std::env::temp_dir().join("sparkamp_techprobe_test.wav");
        write_test_wav(&p, 44100, 2);
        let t = probe_technical(&p);
        assert_eq!(t.sample_rate, Some(44100));
        assert_eq!(t.channels, Some(2));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn probe_survives_unreadable_file() {
        let t = probe_technical(std::path::Path::new("/nonexistent/x.mp3"));
        assert_eq!(t.sample_rate, None);
        assert_eq!(t.channels, None);
    }

    #[test]
    fn avg_bitrate_math() {
        // 1 MB over 25 s ≈ 320 kbps; degenerate durations yield None.
        assert_eq!(avg_bitrate_kbps(1_000_000, 25.0), Some(320));
        assert_eq!(avg_bitrate_kbps(1_000_000, 0.0), None);
        assert_eq!(avg_bitrate_kbps(0, 25.0), Some(0));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test technical_probe'`
Expected: compile error — module/functions don't exist.

- [ ] **Step 3: Implement:**

```rust
//! Technical audio properties read from codec headers.
//!
//! The scanner never captured sample rate or a reliable bitrate/channel
//! count (the DB columns existed but stayed NULL). This module is the one
//! place that derives them: codec parameters via Symphonia's format probe
//! (header-only — no decode), and average bitrate from file size over
//! duration, which is exact for CBR and the honest average for VBR.

use std::path::Path;

#[derive(Debug, Default, Clone, Copy)]
pub struct TechProbe {
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
}

/// Read sample rate and channel count from the file's codec parameters.
/// Returns an empty probe on any error — scan rows degrade to NULL rather
/// than failing the scan.
pub fn probe_technical(path: &Path) -> TechProbe {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let Ok(file) = std::fs::File::open(path) else {
        return TechProbe::default();
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(probed) = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return TechProbe::default();
    };
    let params = probed.format.tracks().first().map(|t| &t.codec_params);
    TechProbe {
        sample_rate: params.and_then(|p| p.sample_rate).map(|s| s as i64),
        channels: params.and_then(|p| p.channels).map(|c| c.count() as i64),
    }
}

/// Average bitrate in kbps from container size and duration. Exact for
/// CBR; for VBR it is the true average, which is what players display.
pub fn avg_bitrate_kbps(file_size_bytes: u64, length_secs: f64) -> Option<i64> {
    if length_secs <= 0.5 {
        return None;
    }
    Some(((file_size_bytes as f64 * 8.0) / length_secs / 1000.0).round() as i64)
}
```

- [ ] **Step 4: Run focused tests → PASS**, then the full suite once → green, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/technical_probe.rs src/lib.rs
git commit -m "feat(core): technical_probe — codec sample-rate/channels + bitrate math"
```

---

### Task 2: MP3 VBR/CBR sniff (F13, minor item)

**Files:**
- Modify: `src/technical_probe.rs`

**Interfaces:**
- Produces (for Task 3): `pub fn mp3_bitrate_mode(path: &Path) -> Option<&'static str>` returning `Some("VBR")` / `Some("CBR")` / `None` (non-MP3, unreadable, or no marker).

- [ ] **Step 1: Failing tests** (append to the module's tests):

```rust
    // Build a fake MP3: optional ID3v2 header (10-byte header + payload),
    // then bytes that contain (or don't) a Xing/Info marker.
    fn write_fake_mp3(path: &std::path::Path, id3_payload_len: u32, marker: Option<&[u8]>) {
        let mut buf = Vec::new();
        if id3_payload_len > 0 {
            buf.extend(b"ID3");
            buf.extend(&[3u8, 0, 0]); // version 2.3, no flags
            // Syncsafe 28-bit size, 7 bits per byte.
            let s = id3_payload_len;
            buf.extend(&[
                ((s >> 21) & 0x7f) as u8,
                ((s >> 14) & 0x7f) as u8,
                ((s >> 7) & 0x7f) as u8,
                (s & 0x7f) as u8,
            ]);
            buf.extend(std::iter::repeat(0u8).take(id3_payload_len as usize));
        }
        buf.extend(&[0xFF, 0xFB, 0x90, 0x00]); // MPEG1 Layer3 frame sync
        buf.extend(std::iter::repeat(0u8).take(32));
        if let Some(m) = marker {
            buf.extend(m);
        }
        buf.extend(std::iter::repeat(0u8).take(64));
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn xing_marker_means_vbr() {
        let p = std::env::temp_dir().join("sparkamp_vbr_test.mp3");
        write_fake_mp3(&p, 0, Some(b"Xing"));
        assert_eq!(mp3_bitrate_mode(&p), Some("VBR"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn info_marker_means_cbr_and_id3_is_skipped() {
        let p = std::env::temp_dir().join("sparkamp_cbr_test.mp3");
        // 5000-byte ID3 tag: marker sits beyond a naive fixed-window scan,
        // so this fails unless the ID3 header size is actually honored.
        write_fake_mp3(&p, 5000, Some(b"Info"));
        assert_eq!(mp3_bitrate_mode(&p), Some("CBR"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn no_marker_and_non_mp3_yield_none() {
        let p = std::env::temp_dir().join("sparkamp_nomode_test.mp3");
        write_fake_mp3(&p, 0, None);
        assert_eq!(mp3_bitrate_mode(&p), None);
        std::fs::remove_file(&p).ok();
        assert_eq!(mp3_bitrate_mode(std::path::Path::new("/nonexistent.mp3")), None);
        let w = std::env::temp_dir().join("sparkamp_nomode_test.wav");
        write_test_wav(&w, 44100, 2);
        assert_eq!(mp3_bitrate_mode(&w), None);
        std::fs::remove_file(&w).ok();
    }
```

- [ ] **Step 2: Run → FAIL** (function missing).

- [ ] **Step 3: Implement:**

```rust
/// Detect VBR vs CBR for MP3 files by the Xing/Info header convention:
/// LAME and friends write "Xing" into the first frame for VBR files and
/// "Info" for CBR. Absence of both means unknown — display blank rather
/// than guessing.
pub fn mp3_bitrate_mode(path: &Path) -> Option<&'static str> {
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mp3"))
        .unwrap_or(false)
    {
        return None;
    }
    let data = read_prefix(path, 10)?;
    // Skip a leading ID3v2 tag: 10-byte header, syncsafe 28-bit size.
    let audio_start = if data.starts_with(b"ID3") && data.len() >= 10 {
        10 + (((data[6] as u64 & 0x7f) << 21)
            | ((data[7] as u64 & 0x7f) << 14)
            | ((data[8] as u64 & 0x7f) << 7)
            | (data[9] as u64 & 0x7f))
    } else {
        0
    };
    // The Xing/Info block sits inside the first MPEG frame; 4 KiB past the
    // tag comfortably covers every version/channel-mode offset.
    let window = read_range(path, audio_start, 4096)?;
    if window.windows(4).any(|w| w == b"Xing") {
        Some("VBR")
    } else if window.windows(4).any(|w| w == b"Info") {
        Some("CBR")
    } else {
        None
    }
}

fn read_prefix(path: &Path, n: usize) -> Option<Vec<u8>> {
    read_range(path, 0, n)
}

fn read_range(path: &Path, start: u64, n: usize) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf).ok()?;
    buf.truncate(read);
    Some(buf)
}
```

- [ ] **Step 4: Focused tests → PASS; full suite → green.**

- [ ] **Step 5: Commit**

```bash
git add src/technical_probe.rs
git commit -m "feat(core): MP3 VBR/CBR detection via Xing/Info marker"
```

---

### Task 3: Schema columns + scanner capture + backfill trigger (F13 DB seam)

**Files:**
- Modify: `src/media_library/mod.rs:437-452` (`new_cols`), `LibTrack` (~line 43) + the row mapper (the helper near mod.rs:477 that maps rows to `LibTrack` — read it first, every SELECT shares it), `src/media_library/scan.rs:553-646` (`upsert_track`), `scan.rs:476` (metadata-pass WHERE), `src/media_library/queries.rs` — all 5 SELECT column lists (lines ~30, ~50, ~115, ~177, ~332; grep `"comment, album_artist, disc_num"` to find them) and the ORDER BY map (~line 139).
- Test: `src/media_library/tests.rs` (read its existing fixture helpers first; reuse them).

**Interfaces:**
- Consumes: Task 1's `probe_technical`/`avg_bitrate_kbps`, Task 2's `mp3_bitrate_mode`.
- Produces (contract for Tasks 4-6): `tracks` columns and matching `LibTrack` fields, exact names: `sample_rate: Option<i64>`, `file_size: Option<i64>`, `file_mtime: Option<String>` (ISO-8601, same formatter as `last_scanned` — read `update_last_scanned` and reuse its formatting helper), `added_at: Option<String>` (set on first INSERT only, never updated), `bitrate_mode: Option<String>` ("VBR"/"CBR").

- [ ] **Step 1: Failing test** (in `src/media_library/tests.rs`, using its existing in-memory/temp DB + file fixture pattern — mirror a neighboring scan test):

```rust
#[test]
fn upsert_captures_technical_columns_and_preserves_added_at() {
    // Arrange: temp library + a real WAV fixture (reuse/write the 44-byte
    // WAV helper pattern from technical_probe's tests via a local copy or
    // a shared fixture helper if tests.rs already has one).
    // 1) upsert a wav file; assert sample_rate == 44100, channels == 2,
    //    file_size == the fixture's byte length, file_mtime non-NULL,
    //    added_at non-NULL, bitrate non-NULL when length_secs is known.
    // 2) capture added_at, upsert the same path again; assert added_at
    //    is unchanged while last_scanned/file_mtime refresh.
}
```

(Write it as real code against the fixture helpers you find in tests.rs — the assertions above are the required behavior, and the second-upsert `added_at` stability check is mandatory.)

- [ ] **Step 2: Run → FAIL** (no such columns).

- [ ] **Step 3: Implement.**
  - `new_cols` additions (append):

```rust
            ("sample_rate", "INTEGER"),
            ("file_size", "INTEGER"),
            ("file_mtime", "TEXT"),
            ("added_at", "TEXT"),
            ("bitrate_mode", "TEXT"),
```

  - `upsert_track`: before the SQL, gather:

```rust
        let tech = crate::technical_probe::probe_technical(p);
        let fs_meta = std::fs::metadata(p).ok();
        let file_size = fs_meta.as_ref().map(|m| m.len() as i64);
        let file_mtime = fs_meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(format_timestamp); // the same helper update_last_scanned uses
        let bitrate = file_size
            .zip(length_secs)
            .and_then(|(sz, len)| crate::technical_probe::avg_bitrate_kbps(sz as u64, len));
        let channels = tech.channels.or(tags.channels);
        let bitrate_mode = crate::technical_probe::mp3_bitrate_mode(p).map(str::to_string);
        let now = format_now(); // same clock/format as last_scanned
```

    Extend the INSERT column list with `sample_rate, file_size, file_mtime, added_at, bitrate_mode` (+5 placeholders), bind `tech.sample_rate, file_size, file_mtime, now, bitrate_mode`, replace the existing `tags.bitrate`/`tags.channels` bindings with `bitrate`/`channels`, and extend `ON CONFLICT DO UPDATE SET` with every new column EXCEPT `added_at` — first-insert timestamp is the whole point of that column. Adapt `format_timestamp`/`format_now` to whatever the real helper in this file/module is called after reading `update_last_scanned` — reuse, don't duplicate.
  - Backfill trigger — `scan.rs:476`, extend the metadata-pass selection so existing rows re-read once:

```rust
        "SELECT id, path FROM tracks WHERE folder_id = ?1 AND (artist IS NULL OR length_secs IS NULL OR sample_rate IS NULL)"
```

    (All ~36k pre-phase rows have NULL sample_rate, so the next user Rescan backfills the new columns in the normal background metadata pass; after that the mtime smart-skip applies as before.)
  - `LibTrack`: add the five fields (types per Interfaces); extend the shared row mapper and all 5 SELECT lists in queries.rs with the five columns, keeping order consistent with the mapper.
  - ORDER BY map (queries.rs ~139): add arms — `"sample_rate"`, `"file_size"` numeric (`COALESCE(sample_rate,0) {dir}` style, mirror the numeric neighbors), `"added_at"`, `"file_mtime"`, `"bitrate_mode"` text (mirror the text neighbors).

- [ ] **Step 4: Focused test → PASS; full suite → green, zero warnings.**

- [ ] **Step 5: Commit**

```bash
git add src/media_library/ src/technical_probe.rs
git commit -m "feat(ml): capture sample rate, size, mtime, added-at, bitrate mode in the scanner"
```

---

### Task 4: GTK ML columns + value formatting (F13 UI)

**Files:**
- Modify: `frontends/gtk/window/ml_columns.rs` — `ALL_COLUMNS` defs + the value match (the `"bpm" => …, "comment" => …` match over `&LibTrack`; read the `last_played` arm first and copy its date formatting for the two timestamp columns).

**Interfaces:**
- Consumes: Task 3's `LibTrack` fields (exact names above).
- Produces: column ids used verbatim by mac (Task 7) sort keys: `sample_rate`, `file_size`, `added_at`, `file_mtime`, `bitrate_mode`.

- [ ] **Step 1: Add five `MlColumnDef` entries** in the read-only section after `channels`-adjacent defs (match neighbors' style):

```rust
    MlColumnDef { id: "sample_rate", header: "Sample Rate", expand: false, id3_editable: false, default_ml_visible: false, default_id3_visible: false },
    MlColumnDef { id: "file_size", header: "Size", expand: false, id3_editable: false, default_ml_visible: false, default_id3_visible: false },
    MlColumnDef { id: "added_at", header: "Date Added", expand: false, id3_editable: false, default_ml_visible: true, default_id3_visible: false },
    MlColumnDef { id: "file_mtime", header: "File Modified", expand: false, id3_editable: false, default_ml_visible: false, default_id3_visible: false },
    MlColumnDef { id: "bitrate_mode", header: "Mode", expand: false, id3_editable: false, default_ml_visible: false, default_id3_visible: false },
```

(Struct literal layout: match the multi-line style of the existing entries, not this compressed form.)

- [ ] **Step 2: Add value arms:**

```rust
        "sample_rate" => t
            .sample_rate
            .map(|s| format!("{:.1} kHz", s as f64 / 1000.0))
            .unwrap_or_default(),
        "file_size" => t.file_size.map(format_file_size).unwrap_or_default(),
        "added_at" => t
            .added_at
            .as_deref()
            .map(format_datetime_for_column) // exact fn the last_played arm uses
            .unwrap_or_default(),
        "file_mtime" => t
            .file_mtime
            .as_deref()
            .map(format_datetime_for_column)
            .unwrap_or_default(),
        "bitrate_mode" => t.bitrate_mode.clone().unwrap_or_default(),
```

plus a small helper beside the match:

```rust
/// Human file size: whole KB under 1 MB, one-decimal MB above.
fn format_file_size(bytes: i64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}
```

(Adapt `format_datetime_for_column` to whatever the `last_played` arm actually calls — reuse it verbatim.)

- [ ] **Step 3: Full build + suite → green.** Sorting works through the Task-3 ORDER BY arms because GTK column sorting goes through the queries sort map — verify by reading how the existing `bitrate` column id reaches the sort map, and confirm the five new ids flow the same way (if a GTK-side sort-id allowlist exists, extend it).

- [ ] **Step 4: Commit**

```bash
git add frontends/gtk/window/ml_columns.rs
git commit -m "feat(gtk): ML columns for sample rate, size, date added, mtime, bitrate mode"
```

---

### Task 5: ID3-window technical line, GTK + TUI (F3)

**Files:**
- Modify: `src/media_library/mod.rs:163-245` (`ReadOnlyTrackFields` + `read_only_track_fields` + new `tech_summary`), `frontends/gtk/window/id3.rs` (one label under the field grid), the TUI ID3 screen (`frontends/tui/ui/id3.rs` — read it first; add one text line in the same style as its existing header/footer lines).

**Interfaces:**
- Consumes: Task 3's `LibTrack.sample_rate`.
- Produces: `ReadOnlyTrackFields.sample_rate: String` (formatted "44.1 kHz" or empty) and `pub fn tech_summary(ro: &ReadOnlyTrackFields) -> String` — used by GTK, TUI, and (as reference formatting) mac in Task 7.

- [ ] **Step 1: Failing test** (in the tests module of `src/media_library/mod.rs`, or `tests.rs` if that's where display tests live — check first):

```rust
#[test]
fn tech_summary_joins_populated_parts_only() {
    let ro = ReadOnlyTrackFields {
        filetype: "mp3".into(),
        bitrate: "320k".into(),
        sample_rate: "44.1 kHz".into(),
        channels: "stereo".into(),
        duration: "3:45".into(),
        // fill the remaining fields with Default/empty per the struct
        ..Default::default()
    };
    assert_eq!(tech_summary(&ro), "MP3 · 320k · 44.1 kHz · stereo · 3:45");

    let sparse = ReadOnlyTrackFields { duration: "3:45".into(), ..Default::default() };
    assert_eq!(tech_summary(&sparse), "3:45");
}
```

(If `ReadOnlyTrackFields` lacks `Default`, derive it. The filetype is uppercased in the summary — that's the deliberate Winamp-style presentation.)

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement.**
  - Add `pub sample_rate: String` to the struct; populate in `read_only_track_fields`:

```rust
    let sample_rate = track
        .and_then(|t| t.sample_rate)
        .map(|s| format!("{:.1} kHz", s as f64 / 1000.0))
        .unwrap_or_default();
```

  - Add:

```rust
/// One-line technical summary for the ID3 window: uppercase filetype,
/// bitrate, sample rate, channel layout, duration — skipping empty parts.
/// Deliberately NOT shown on the main player window (spec deviation from
/// Winamp): the ID3 window is Sparkamp's home for technical detail.
pub fn tech_summary(ro: &ReadOnlyTrackFields) -> String {
    let ft = ro.filetype.to_uppercase();
    [ft.as_str(), &ro.bitrate, &ro.sample_rate, &ro.channels, &ro.duration]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}
```

  - GTK: in `open_id3_editor_window` after the field grid is added, insert a dimmed label (`css_classes(["info-desc"])`, `halign Start`, margins matching the status label) with `gtk_safe(&tech_summary(&ro))`; keep it above the artwork section.
  - TUI: one line rendering `tech_summary` where the ID3 screen shows the filename/header (match its existing style constants).

- [ ] **Step 4: Focused test → PASS; full suite → green.**

- [ ] **Step 5: Commit**

```bash
git add src/media_library/mod.rs frontends/gtk/window/id3.rs frontends/tui/
git commit -m "feat(id3): technical summary line in the tag window (GTK + TUI)"
```

---

### Task 6: Folder-image artwork fallback + cache-guarded refresh (F2)

**Files:**
- Modify: `src/tags.rs:76-99` (artwork block in `read_track_tags`), `src/media_library/queries.rs:352-370` (`refresh_artwork`).
- Test: `src/tags.rs` tests module (check whether one exists; create if not, following the file's conventions).

**Interfaces:**
- Produces: `TrackTags.artwork_path` may now point OUTSIDE the cache dir (at the user's folder image). Every consumer treats it as an opaque path already, EXCEPT `refresh_artwork`'s delete — which this task guards.

- [ ] **Step 1: Failing tests:**

```rust
#[test]
fn folder_image_fallback_when_no_embedded_art() {
    let dir = std::env::temp_dir().join("sparkamp_folderart_test");
    std::fs::create_dir_all(&dir).unwrap();
    // Tagless audio file + a folder image with non-canonical casing.
    let song = dir.join("song.mp3");
    std::fs::write(&song, b"").unwrap();
    let art = dir.join("Cover.JPG");
    std::fs::write(&art, b"fake").unwrap();

    let tags = read_track_tags(&song);
    assert_eq!(
        tags.artwork_path.as_deref(),
        Some(art.to_string_lossy().as_ref()),
        "case-insensitive folder image should be found"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn no_folder_image_means_no_artwork() {
    let dir = std::env::temp_dir().join("sparkamp_nofolderart_test");
    std::fs::create_dir_all(&dir).unwrap();
    let song = dir.join("song.mp3");
    std::fs::write(&song, b"").unwrap();
    assert!(read_track_tags(&song).artwork_path.is_none());
    std::fs::remove_dir_all(&dir).ok();
}
```

- [ ] **Step 2: Run → FAIL** (fallback missing).

- [ ] **Step 3: Implement.**
  - In `read_track_tags`, after the embedded-APIC `artwork_path` is computed (both the ID3 branch AND the symphonia branch — the symphonia branch currently never sets artwork; give both the fallback by applying it wherever the final `TrackTags` is about to be returned with `artwork_path == None`):

```rust
/// Probe the track's directory for a conventional cover image. Winamp's
/// source order: embedded APIC first, then folder image. Case-insensitive
/// because rips and downloads disagree about casing.
pub(crate) fn folder_image_fallback(track_path: &Path) -> Option<String> {
    const NAMES: &[&str] = &[
        "folder.jpg", "folder.jpeg", "folder.png",
        "cover.jpg", "cover.jpeg", "cover.png",
        "front.jpg", "front.jpeg", "front.png",
    ];
    let dir = track_path.parent()?;
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(usize, std::path::PathBuf)> = None;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if let Some(rank) = NAMES.iter().position(|n| *n == name) {
            // Prefer folder.* over cover.* over front.* (NAMES order).
            if best.as_ref().map(|(r, _)| rank < *r).unwrap_or(true) {
                best = Some((rank, e.path()));
            }
        }
    }
    best.map(|(_, p)| p.to_string_lossy().into_owned())
}
```

    and at each return site: `artwork_path: artwork_path.or_else(|| folder_image_fallback(path))` (adapt to the branch's variable names).
  - `refresh_artwork` guard — replace the unconditional delete:

```rust
        // Only delete cached extractions. artwork_path can now point at the
        // user's own folder image (F2 fallback) — deleting that would be
        // destroying their file, not our cache.
        if let Ok(track) = self.track_by_path(path) {
            if let Some(ref old_art) = track.artwork_path {
                let cache_root = dirs::cache_dir()
                    .unwrap_or_else(std::env::temp_dir)
                    .join("sparkamp");
                if std::path::Path::new(old_art).starts_with(&cache_root) {
                    let _ = std::fs::remove_file(old_art);
                }
            }
        }
```

- [ ] **Step 4: Focused tests → PASS; full suite → green.** (The ID3-editor thumbnail, ML artwork column, and art viewer inherit the fallback automatically — they all read `artwork_path`.)

- [ ] **Step 5: Commit**

```bash
git add src/tags.rs src/media_library/queries.rs
git commit -m "feat(art): folder-image fallback with cache-guarded refresh (F2)"
```

---

### Task 7: mac ML columns + tech line (F13/F3 mac) — BLIND Swift

**Files:**
- Modify: `src/ffi/media_library.rs:424` (`sparkamp_ml_get_tracks` — read the whole function first to learn how rows cross the FFI; extend it with the five new fields the same way existing fields cross), `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` (hand-mirror any struct/signature change), `frontends/SparkampMac/Sources/MLFilesTable.swift` (`MLTrack` struct, column spec list ~line 92, cell `case`s ~317, sort comparators ~377/397), `frontends/SparkampMac/Sources/Id3EditorWindow.swift` (tech line), `docs/mac-pass-checklist.md`.

**Interfaces:**
- Consumes: Task 3's DB fields; Task 4's column ids (`sample_rate`, `file_size`, `added_at`, `file_mtime`, `bitrate_mode`) as sortKey strings; Task 5's `tech_summary` formatting ("· "-joined, uppercase filetype) as the mac tech-line reference.

- [ ] **Step 1: Read `sparkamp_ml_get_tracks` and the existing `MLTrack` Swift struct.** Whatever the row transport is (C struct array, parallel accessor calls, or serialized buffer), extend it with: `sample_rate: Int`, `file_size: Int64`, `added_at: String`, `file_mtime: String`, `bitrate_mode: String` (0/empty = absent), mirroring exactly how `bitrate` crosses today. Update `sparkamp_bridge.h` by hand for any C-visible change.

- [ ] **Step 2: MLFilesTable columns** — five new specs following line 92's pattern (next free `bit` values, `sortKey` strings matching the GTK ids verbatim):

```swift
        .init(id: "col-samplerate", title: "Sample Rate", bit: <next>, width: 90, sortKey: "sample_rate", isSmallMono: true),
        .init(id: "col-filesize",   title: "Size",         bit: <next>, width: 80, sortKey: "file_size",   isSmallMono: true),
        .init(id: "col-added",      title: "Date Added",   bit: <next>, width: 130, sortKey: "added_at",   isSmallMono: true),
        .init(id: "col-mtime",      title: "File Modified", bit: <next>, width: 130, sortKey: "file_mtime", isSmallMono: true),
        .init(id: "col-brmode",     title: "Mode",         bit: <next>, width: 60, sortKey: "bitrate_mode", isSmallMono: true),
```

(`<next>` = the actual next unused bit indices — read the list; these literals must be real numbers in the code.) Add matching cell cases (sample rate rendered `String(format: "%.1f kHz", Double(sr)/1000)` when > 0; size KB/MB matching Task 4's thresholds; timestamps and mode as strings) and `KeyPathComparator` + sortKey mappings for all five.

- [ ] **Step 3: mac ID3 tech line** — in `Id3EditorWindow.swift`, where the editor has the file's ML row available (or can look it up the way the mac ML opens the editor), add one dimmed text line under the field grid: uppercase filetype, bitrate, sample rate, channels, duration joined with " · ", skipping empties — same output as core `tech_summary`.

- [ ] **Step 4: Append to `docs/mac-pass-checklist.md`** (same commit): new section "Phase-1: ML technical columns + ID3 tech line" — five columns appear/sort/format correctly; tech line matches GTK's for the same file; existing columns unaffected.

- [ ] **Step 5: Rust suite** (gate honesty — FFI change compiles in Rust even though Swift is blind): full build + suite → green, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add src/ffi/media_library.rs frontends/SparkampMac/ docs/mac-pass-checklist.md
git commit -m "feat(mac): ML technical columns + ID3 tech line (blind — needs Xcode pass)"
```

---

### Task 8: Settings-widget skinning (B8)

**Files:**
- Modify: `src/skin.rs::render_gtk_css` (~line 544 onward; insert near the seek/vol scale block ~635) + its tests module (~1388, follow the `render_gtk_css_covers_*` pattern).

**Interfaces:**
- Consumes: existing CSS vars in the function (`tbg`, `border`, `hl`, `text`, and the button-background var the transport buttons use — read the surrounding `writeln!`s and reuse the same locals).

- [ ] **Step 1: Failing test:**

```rust
    #[test]
    fn render_gtk_css_covers_generic_settings_widgets() {
        let css = render_gtk_css(&SkinVars::default());
        // Generic (classless) widgets used across the Settings tabs must
        // follow the skin: scales, dropdowns, spinbuttons, popover lists.
        assert!(css.contains("scale trough"));
        assert!(css.contains("scale highlight"));
        assert!(css.contains("scale slider"));
        assert!(css.contains("dropdown > button"));
        assert!(css.contains("spinbutton"));
        assert!(css.contains("popover listview row:selected"));
    }
```

(Match the construction of neighboring tests — if they build `SkinVars` differently than `::default()`, copy that.)

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement** — add near the seek/vol block (element selectors lose to the `.seek-scale`/`.vol-scale` class selectors on specificity, so the dedicated bars keep their chunky style):

```rust
    // Generic settings-surface widgets (B8). Every Scale/DropDown/SpinButton
    // without a dedicated class — the Settings tabs, mostly — follows the
    // skin instead of GTK defaults. The seek/vol class selectors above stay
    // more specific and keep their dedicated styling.
    writeln!(css, "scale trough {{ \
        background: {tbg}; border: 1px solid {border}; \
    }}").unwrap();
    writeln!(css, "scale highlight {{ background: {hl}; }}").unwrap();
    writeln!(css, "scale slider {{ \
        background: {text}; border: 1px solid {border}; \
    }}").unwrap();
    writeln!(css, "dropdown > button, spinbutton {{ \
        background: {tbg}; color: {text}; border: 1px solid {border}; \
    }}").unwrap();
    writeln!(css, "spinbutton text {{ background: {tbg}; color: {text}; }}").unwrap();
    writeln!(css, "popover listview row {{ color: {text}; }}").unwrap();
    writeln!(css, "popover listview row:hover {{ background: {hl_hov}; }}").unwrap();
    writeln!(css, "popover listview row:selected {{ \
        background: {hl_sel}; color: {text}; \
    }}").unwrap();
```

(Use the exact local variable names in scope at the insertion point — `hl_hov`/`hl_sel` exist near the playlist-row rules; if the locals at your insertion point differ, move the block or reference the right ones.)

- [ ] **Step 4: Focused test → PASS; full suite → green, zero warnings.**

- [ ] **Step 5: Commit**

```bash
git add src/skin.rs
git commit -m "fix(gtk): settings-tab scales, dropdowns, spinbuttons follow the skin (B8)"
```

---

### Task 9: Phase gate

- [ ] **Step 1: Full gate:** `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'` — zero warnings, zero failures, count above the 1021 floor.
- [ ] **Step 2: Ledger + report for the user's interactive pass:** Rescan (or add a folder) → sample-rate/size/date-added columns fill and sort; existing rows backfill in the background pass; ID3 window shows the tech line; a tagless file in a folder with `Cover.JPG` shows art in the ID3 thumbnail/ML/art viewer; Settings sliders/dropdowns now follow the skin; mac items on `docs/mac-pass-checklist.md`.
