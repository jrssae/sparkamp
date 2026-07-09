# Optical Disc (CD/DVD) Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans to implement task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Winamp-parity optical-disc support across **all three Sparkamp frontends — GTK (`sparkamp --ui`), TUI (`sparkamp`), and SparkampMac**: play audio CDs, identify discs via gnudb.org, override/prefill ID3 tags and submit corrections upstream, rip to tagged MP3, and burn audio CDs and data (MP3) CD/DVDs — with graceful handling of drive/disc failures.

**Architecture:** A shared Rust core owns everything platform-neutral — the freedb/CDDB **disc-ID math**, the **gnudb HTTP client** (query / read / submit), the **tag-override model** (reusing `src/id3_editor.rs` + `src/tags.rs`), and the **GStreamer rip/encode pipeline** (GStreamer already ships on both platforms). A thin **platform device layer** does only what must be native: raw disc/TOC access and burning. Linux uses GStreamer `cdiocddasrc` for read/rip, `cd-info` (libcdio) to probe drive/media, and libburnia CLIs — `xorriso` (data ISO9660/UDF) and `cdrskin` (audio CD + blanking) — for burning. macOS uses the auto-mounted AIFF volume for read and — **as-built deviation, see the Phase 5 note** — Apple's `drutil` CLI (a wrapper over DiscRecording) for burning/blanking instead of linking DiscRecording.framework. The **GTK and TUI frontends call the core directly** (same crate, in-process — exactly as they already call `crate::devices` / `crate::media_library`); **SparkampMac** calls it through the same **JSON-over-FFI** style used by the device-sync work (`src/ffi/`), with long-running rip/burn progress delivered through the existing `sparkamp_tick` + mpsc callback mechanism. All disc *logic* is shared core, so the three frontends differ only in presentation.

**Tech Stack:** Rust core, GStreamer (`cdiocddasrc`, `lamemp3enc`), a **light blocking HTTP client** (`ureq` or `minreq` — gnudb endpoints are plain `http://`, so avoid `reqwest`'s TLS/openssl pull-in that would bloat the Flatpak `cargo vendor` tree), serde_json; Linux: libcdio-utils (`cd-info`), libburnia (`xorriso`, `cdrskin`); macOS: DiscRecording.framework, IOKit/DiskArbitration, CoreAudio.

**Relationship to other plans:** Independent subsystem — separate from `2026-06-23-macos-device-sync-parity.md` (removable block volumes) and the MTP/iOS device work. Reuses the same FFI conventions and the platform-native-device-layer pattern. The "Devices" sidebar gains an **optical drive** entry distinct from removable volumes.

---

## Why hybrid (decision record)

- **GStreamer for read/rip** — one pipeline on both OSes (Linux `cdiocddasrc`; mac `filesrc` over the mounted `.aiff`), and read/scratch errors ride the existing GStreamer bus-error path already surfaced in the UI.
- **libburnia CLIs (xorriso/cdrskin) for Linux burn** — proven, no immature Rust FFI, and burning (the drive-flaky operation) runs in a **killable subprocess**, which is the backbone of the feature-7 error handling.
- **Rejected linking libburn/libisofs** — no maintained Rust bindings, heavy `unsafe`, Linux-only payoff (mac uses DiscRecording), and worse crash isolation on the riskiest op.
- **macOS DiscRecording** — native, handles audio + data + blank + write-once/RW without external tools.

---

## Settings additions (`src/config.rs`, `Config` struct)

All new fields use `#[serde(default)]` + a `Default` impl (CLAUDE.md).

- [x] `gnudb_email: String` — **default CHANGED to blank** (the gnudb howto forbids submitting with an app-wide default; the retired `sparkamp@fastmail.com` value in old configs is treated as unset — `gnudb::is_unset_email`). Lookups work without it via an anonymous `hello`; the **first submission prompts for it in a modal** (mac sheet / TUI overlay — GTK needs the same dialog). Editable in Settings (mac shipped; GTK/TUI Settings fields still open).
- [x] `gnudb_submit_mode_test: bool` — default `true`; mac Settings has the toggle; submissions in test mode are labeled "not published".
- [x] `rip_dest_dir: Option<PathBuf>` — implemented; dialog defaults config → first watched folder → ~/Music, remembers the choice.
- [x] `rip_mp3_quality: u8` — **preset id, not raw VBR:** 0 = V0, 1 = V2 (default), 2 = 320 CBR (matches the dropdown; `Mp3Quality::from_config`).
- [x] `burn_verify: bool` — default `true`; keeps drutil's post-burn verification (off adds `-noverify`). mac Settings toggle shipped; Linux tools have no switch (Opus follow-up: readback check).

---

## Shared data model (`src/disc/mod.rs`, new module)

```rust
/// One track's position on the disc. `start_frame` is the **CDDB-absolute**
/// frame (75 frames = 1 s), i.e. LBA **+ 150** (the 2-second lead-in pregap).
/// libcdio/GStreamer report the post-pregap LBA, so the detector MUST add 150
/// when populating this — get this wrong and every freedb disc-ID is wrong and
/// gnudb never matches. `leadout_frame` is likewise absolute.
pub struct TocTrack { pub number: u8, pub start_frame: u32, pub is_audio: bool }
/// Full table of contents for the loaded disc.
pub struct DiscToc { pub tracks: Vec<TocTrack>, pub leadout_frame: u32 }
/// What kind of media is in the drive and what we can do with it.
pub struct MediaInfo {
    pub present: bool, pub is_audio_cd: bool, pub is_blank: bool,
    pub rewritable: bool, pub kind: MediaKind, // CdR, CdRw, DvdR, DvdRw, DvdRam, Unknown
    pub free_bytes: u64, pub capacity_bytes: u64,
}
/// One physical optical drive. Every drive present is listed in the sidebar,
/// exactly like each external device — never collapsed to a single "the drive".
pub struct OpticalDrive {
    /// Stable per-drive id used for sidebar identity + subprocess targeting:
    /// Linux device node (e.g. `/dev/sr0`); macOS IOKit/BSD name.
    pub id: String,
    /// Human label from the drive (vendor + model, e.g. "PIONEER BD-RW BDR-XD07").
    pub label: String,
    pub media: MediaInfo,
    /// TOC when an audio/data disc is loaded; `None` when blank or empty.
    pub toc: Option<DiscToc>,
}
```

`DiscToc`, `MediaInfo`, and `OpticalDrive` derive `Serialize, Deserialize` (JSON-over-FFI to Swift).

---

## Phase 1 — Detection + audio-CD playback

**Files:** Create `src/disc/detect.rs` (cfg-split: Linux `cd-info`/GStreamer; a mac-neutral entry that Swift feeds), `src/disc/toc.rs`; modify GTK `frontends/gtk/window.rs` (a "Disc Drives" sidebar group + per-drive detail/track list), TUI `frontends/tui/ui/media_library.rs` + `frontends/tui/keys.rs` (see TUI parity section), `src/ffi/disc.rs` (drive enumeration + playlist URIs). Test: `src/disc/toc.rs`.

- [x] **Enumerate every optical drive.** Linux: `/sys/block/sr*` + `cd-info` parse (written, dev-box verify pending). macOS — **design deviation (agreed):** the core self-detects instead of Swift-IOKit-feeds: `drutil list`/`status` enumerate drives + media, and the mounted volume's `.TOC.plist` (via `plutil -convert xml1`; its Start Block values are already CDDB-absolute) supplies the TOC. Keeps Swift thin AND makes the TUI's disc section work on macOS. All output parsers are platform-neutral `&str` fns, unit-tested on every OS. Live-verified against a MATSHITA DVD-RAM UJ8C2 + 8-track audio CD. Polled ~10 s (subprocess-backed) on the mac tick; TUI detects on tab entry / `r`.
- [x] **"Disc Drives" sidebar group.** macOS: sidebar group (one row per drive, label + media state) + `DiscDriveView` detail (track table, Add Disc / Scan / Eject, banners for no-disc/blank/data). TUI: third ML tab "Discs" (drive rows + track list). **GTK: done** — Disc Drives sidebar group above Devices (one row per drive, label + media-state line, chevron collapse), clickable header → overview card grid (media summary + detail line, empty state), per-drive detail (audio track list, Add Selected / Add All, double-click a track to add it, banners for no-disc/blank/data), 10 s off-thread poll with in-place row diffing + unplug fallback to overview, and optical `iso9660`/`udf` volumes excluded from the Devices list.
- [x] **Add-to-playlist URIs.** Per-track `DiscTrackEntry { number, path, title, duration_secs }`: macOS the mounted AIFF path (matched by leading track number, localization-proof); Linux `cdda://N?device=/dev/srX` — `Track::uri()` passes `cdda://` through and the engine strips `?device=`, stashing the node for the `source-setup` handler. Entries carry "Track N" + TOC duration; added via the new `sparkamp_playlist_add_entry` (no tag scan / probe).
- [x] **UI: add individual tracks or whole disc** — mac: double-click/context menu/Add Selected/Add All/Add Disc; TUI: Enter = track, `a` = whole disc.
- [x] **Tests:** TOC plist parse, drutil list/status parse + media mapping, cd-info parse (+150 pregap), durations, cdda URI construction, FFI round-trip — 14 unit tests + an `--ignored` live probe (`cargo test --lib live_list_drives -- --ignored --nocapture`). Manual play test on the real drive: user-run.
- **Out of scope (v1):** mixed-mode / CD-Extra discs (audio + data session) — handle the **audio tracks only** (play/rip/identify); ignore the data track. No multi-drive picker — every drive is its own sidebar row.
- [ ] **Commit** `feat(disc): optical-drive detection + audio-CD playback`.

## Phase 2 — gnudb identification + tag override

**Files:** Create `src/disc/discid.rs`, `src/disc/gnudb.rs`, `src/disc/xmcd.rs`; modify `src/ffi/disc.rs`. Test: all three new files.

- [ ] **Task 2.1 — freedb disc-ID (pure, TDD).** Implement:

```rust
/// freedb/CDDB disc ID: 8-hex from the TOC. Assumes `start_frame`/`leadout_frame`
/// are already CDDB-absolute (LBA + 150 pregap — see `TocTrack`). The detector,
/// not this function, is responsible for adding the pregap.
pub fn freedb_discid(toc: &DiscToc) -> String {
    fn digit_sum(mut secs: u32) -> u32 { let mut s = 0; while secs > 0 { s += secs % 10; secs /= 10; } s }
    let n: u32 = toc.tracks.iter().map(|t| digit_sum(t.start_frame / 75)).sum();
    let first = toc.tracks.first().map(|t| t.start_frame / 75).unwrap_or(0);
    let last = toc.leadout_frame / 75;
    let total = last.saturating_sub(first);
    format!("{:08x}", ((n % 0xff) << 24) | (total << 8) | toc.tracks.len() as u32)
}
```

  - [x] Failing test first (todo!() stub, 4 tests verified failing), then implemented: hand-computed 3-track vector, the real 8-track test disc (`6f067d08`), empty-TOC edge, and the `cddb query` arg builder. **Algorithm confirmed by gnudb itself**: the server echoes the disc ID it computes from our offsets/nsecs — it matches ours exactly.

- [x] **Task 2.2 — gnudb HTTP client** (`src/disc/gnudb.rs`, via `minreq` — tiny, TLS-free, vendored). `hello_param` splits the email at the last `@` (unit-tested); URLs carry `proto=6`; response parsing covers 200 / 210 / 211 / 202-as-empty / 403; network failures are typed `Offline` vs `Protocol`. Live-verified against gnudb.gnudb.org in both directions (query → 202 for this unlisted disc; read of a missing entry → 401 surfaced as a protocol error). Config gained `disc.gnudb_email` (default sparkamp@fastmail.com, editable in mac Settings) + `gnudb_submit_mode_test`.
  - **Handshake:** the CDDB `hello` is **four `+`-separated fields — `username+hostname+clientname+version`** — so split the configured email at `@`: `sparkamp@fastmail.com` → `hello=sparkamp+fastmail.com+Sparkamp+<pkg_version>`. Do **not** put the whole address in one field. `clientname` must be descriptive ("Sparkamp"); `version` from `env!("CARGO_PKG_VERSION")`. Always send `proto=6` (UTF-8). URL-encode: spaces→`+`, other specials→`%XX`.
  - GET `http://gnudb.gnudb.org/~cddb/cddb.cgi?cmd=cddb+query+<discid>+<ntrks>+<off1>+…+<offn>+<nsecs>&hello=sparkamp+fastmail.com+Sparkamp+<version>&proto=6`. Handle response codes **200** (exact), **211** (inexact list), **202** (none), **403** (corrupt). `read` → `cmd=cddb+read+<category>+<discid>` (same hello+proto). Timeouts + offline → typed error, surfaced as "couldn't reach gnudb".
  - Add a helper `fn hello_param(email: &str) -> String` that splits on the last `@` (fallback: whole string as username, `localhost` as host if no `@`); unit-test it.
- [x] **Task 2.3 — xmcd parse/build** (`src/disc/xmcd.rs`). Parses DISCID/DTITLE/DYEAR/DGENRE/TTITLEn/EXTD/EXTTn with wrapped-line continuation and the self-titled no-separator case; `build` emits the submission format incl. the offsets/length comment header. Round-trip tested.
- [x] **Task 2.4 — tag override UI** (mac + TUI; GTK = Linux box). Overrides are per-disc tag sets keyed by freedb id, editable **with or without a match**, overlaid onto the track titles and consumed by rip (P3) / submission (P4). macOS: Identify button (auto-applies a single exact match; sheet picker otherwise) + Edit Tags sheet (disc fields + per-track titles), artist—album shown in the drive header. TUI: `m` identify (background thread + tick-drained channel — the UI never blocks on the network), match overlay (↑↓/Enter/Esc), `e` tag editor overlay (rows = disc fields + titles; Enter edits, Esc saves).
- [x] **Commit** `feat(disc): gnudb query/read/parse + per-track tag override`.

## Phase 3 — Rip to MP3

**Files:** Create `src/disc/rip.rs`; modify `src/ffi/disc.rs` (async rip + progress via `sparkamp_tick`), Settings UI. Test: `src/disc/rip.rs` (pipeline-string builder + path/tag mapping; the actual GStreamer run is a manual/integration check).

- [x] **Rip pipeline (GStreamer, shared)** — `src/disc/rip.rs`: per-track `parse::launch` pipeline (mac `filesrc <aiff> ! decodebin`, Linux `cdiocddasrc track=N device=…`; shared `audioconvert ! lamemp3enc ! filesink`), post-encode tags via `id3_editor::write_tag_fields` (one tag path, incl. sampler "Artist / Title" split into artist + album_artist). Stall watchdog on **pipeline position**, not bus traffic (a healthy encode posts no messages for minutes at optical speed). **Live-verified**: real CD track → 3 MB tagged MP3 in 65 s, ID3 asserted (`cargo test --lib live_rip -- --ignored`). Quality presets 0=V0/1=V2 default/2=320 CBR (`rip_mp3_quality`; deviation from the plan's raw-VBR u8, matches the dropdown).
- [x] **Destination + naming** — `dest_path`: sanitized `Artist/Album/NN - Title.mp3`; dialog defaults `rip_dest_dir` → first watched folder → ~/Music, choice remembered (config setters via FFI on mac, direct on TUI).
- [x] **Auto-import** — finished files go through `add_files_to_library` on both frontends.
- [x] **Progress + cancel** — mac: per-track progress bar in the drive view + Cancel (stops after the current track), rip loop on a worker queue. TUI: rip overlay (`g`; Space/a select, q quality, d dest, Enter) with progress in the hint bar and `c` to cancel; background thread + tick-drained channel.
- [x] **Commit** `feat(disc): rip audio CD to tagged MP3 with auto-import`.

## Phase 4 — Submit to gnudb

**Files:** Modify `src/disc/gnudb.rs`, `src/ffi/disc.rs`, Settings UI.

- [x] **Category selection.** Fixed set in `gnudb::CATEGORIES`; `suggest_category` maps free-text genre (rock/metal/punk→rock, etc., default `misc`), unit-tested. mac: category Picker in the submit sheet, prefilled. TUI: category overlay (`u`), preselected.
- [x] **Build xmcd** — `xmcd::build` (Phase 2.3) + `validate_for_submit` (non-empty artist/album, every track genuinely titled — "Track N" placeholders rejected with the offending track numbers), DISCID derived from the TOC, revision = matched entry + 1 (parse now captures `# Revision:`) or 0 for a new disc. The untouched match is kept per disc (`discOfficial` / `disc_official`) as the baseline.
- [x] **POST submit.cgi** with Category/Discid/User-Email/Submit-Mode/Charset/X-Cddbd-Note headers via minreq; 200 parsed as acceptance, anything else surfaced verbatim (500/501 covered by tests). Test mode honored from `gnudb_submit_mode_test` (mac Settings toggle added); test-mode results are labeled "not published".
- [x] **UI (deviating per user request):** the mac drive view's redundant Scan button is **replaced** by "Submit to gnudb", shown only when the disc is unmatched (always) or the tags differ from the official match (`discSubmittable`). TUI: `u` in the Discs tab.
- [ ] **Register the app** — one-time human action: email `info@gnudb.org` announcing client "Sparkamp" + contact (`sparkamp@fastmail.com`). Not a code step — still pending.
- [x] **Commit** `feat(disc): submit disc metadata to gnudb`.

## Phase 5 — Burn audio CD

**Files:** Create `src/disc/burn.rs` (Linux subprocess orchestration), `src/disc/burnlist.rs` (the dedicated Burn list model); modify `src/ffi/disc.rs`, all three frontends (GTK/TUI/mac Burn list UI). macOS burning is implemented Swift-side over DiscRecording, driven by the same Burn list.

> **Blind-implementation note (2026-07-07, Fable):** Phases 5–6 were implemented
> WITHOUT blank media on hand. Everything not needing a blank disc is unit- or
> live-tested below ("Internal tests"); everything needing one is enumerated for
> the follow-up worker ("Hardware tests — Opus") with expected results.
> **Design deviation from this plan, on purpose:** the macOS burn path shells out
> to Apple's own `drutil burn` / `drutil erase` (a CLI over DiscRecording)
> instead of linking DiscRecording.framework from Swift. Rationale: one
> subprocess-orchestration code path in core for both OSes (drutil ↔ cdrskin/
> xorriso), pure-function command builders that ARE unit-testable blind, and no
> risk of misusing a large ObjC API that cannot be exercised without media. If
> hardware testing shows drutil's progress/error reporting is too coarse, moving
> to DiscRecording later is contained (swap `burn.rs`'s mac arms; the FFI and
> UIs don't change).

- [x] **Burn list model** (`src/disc/burnlist.rs`) — dedicated queue, separate from the active playlist: add (dedup)/remove/move-up/down, running audio seconds vs a media capacity, data bytes total. Unit-tested.
- [x] **Confirm before erasing.** Both frontends prompt before any burn onto a non-blank rewritable disc; write-once non-blank media is refused outright. Never auto-blank.
- [x] **Prepare audio.** `burn::prepare_wav` — GStreamer `decodebin ! audioconvert ! audioresample ! capsfilter 44.1 kHz/S16LE/stereo ! wavenc` per track into a temp dir. **Live-tested without media** (see Internal tests: rip output → WAV, header verified).
- [x] **Linux burn (written blind, compile-checked on the dev box later).** `cdrskin dev=<node> blank=as_needed -audio -pad -dao <wavs…>`; erase = `cdrskin dev=<node> blank=fast`. Killable subprocess; non-zero exit → typed error with stderr tail.
- [x] **macOS burn (deviation above).** Audio: stage WAVs into a temp folder → `drutil burn -audio -noverify -drive <index> <folder>`; erase = `drutil erase quick -drive <index>`. Same subprocess runner as Linux.
- [ ] **Commit** `feat(disc): burn audio CD from a dedicated burn list`.

**Internal tests (no blank media — implemented and passing now):**
- `cargo test --lib burnlist` — queue ops, dedup, reorder bounds, audio-seconds and byte totals, over-capacity detection.
- `cargo test --lib disc::burn` — command builders byte-for-byte (`cdrskin` audio/erase, `xorriso` data, `drutil` audio/data/erase), WAV staging name order (`01.wav…`), capacity math (blank CD-R 359 999 blocks ≈ 79:59), erase-decision matrix (blank → no erase; RW+content → erase-after-confirm; write-once+content → refuse).
- `cargo test --lib live_prepare_wav -- --ignored --nocapture` — real transcode of any library file to Red Book WAV; asserts RIFF header: PCM, 2 ch, 44 100 Hz, 16-bit.
- `cargo build` zero warnings, `xcodebuild` succeeds; TUI burn overlay opens with no media and shows the "insert a blank disc" state.

**Hardware tests (Opus, blank media required):**
1. **Audio CD-R:** add 3+ library tracks to the burn list (ML Files → right-click → Add to Burn List / TUI `b`), insert blank CD-R, drive view → Burn Audio CD → expect prepare progress per track, then burn phase, success status; disc plays in the Sparkamp Discs tab (TOC track count matches) and in Music.app. Verify gap/order correctness.
2. **Over-capacity:** queue >80 min, expect the burn button blocked with the over-capacity message BEFORE any disc write.
3. **CD-RW with content:** burn once, then burn a different list → expect the erase confirmation (cancel leaves the disc untouched; confirm erases and burns).
4. **Write-once with content:** insert the burned CD-R again → audio burn must be refused with a clear message, no drutil/cdrskin invocation.
5. **Cancel mid-burn:** cancel during the burn phase → subprocess killed, status reports cancellation, disc reported as likely unusable (expected for write-once).
6. **drutil output audit:** capture `drutil burn` stdout/stderr from a real burn; if percent lines exist, wire them into the progress parser (`parse_drutil_burn_progress` stub notes the format to look for); if not, keep the indeterminate spinner.

## Phase 6 — Burn data (MP3) disc, write-once + rewritable

**Files:** Modify `src/disc/burn.rs`, `src/ffi/disc.rs`, all three frontends (GTK, TUI, mac).

- [x] **Data burn list** — the same burn-list model in "data" mode: any files (MP3s from the library via the same add paths); shows total bytes vs the media's free bytes.
- [x] **Linux burn (written blind, dev-box verify later).** `xorriso -outdev <node> -blank as_needed -joliet on -map <staged dir> / -commit`. Files are staged (symlinked/copied) into one temp dir so the disc root is flat and predictable. Rewritable → `-blank as_needed`; write-once with content → refused in v1 (multisession append is listed as an Opus follow-up below rather than shipped untested).
- [x] **macOS burn (deviation above).** Stage into a temp folder → `drutil burn -drive <index> <folder>` (drutil produces an ISO9660/Joliet layout via DiscRecording).
- [x] **MP3-CD companion playlist.** Every data burn writes `playlist.m3u8` (or `.m3u`, following the app-wide playlist-format setting) at the disc root listing the files in burn order — unit-tested.
- [x] **Verify toggle.** `disc.burn_verify` (default ON) keeps drutil's post-burn verification; off adds `-noverify`. Exposed in mac Settings → Media Library → Discs. cdrskin/xorriso have no equivalent switch — a Linux readback check is an Opus follow-up.
- [ ] **Commit** `feat(disc): burn data MP3 discs (write-once + rewritable)`.

**Internal tests (no blank media — implemented and passing now):** covered by the `disc::burn` builder tests above (xorriso/drutil data args, staging layout, byte totals, refuse/erase matrix for data mode).

**Hardware tests (Opus, blank media required):**
1. **Data CD-R:** queue a dozen MP3s → Burn Data → after eject/reinsert, the volume mounts with exactly those files at the root; they play from the Devices/Finder path.
2. **Data DVD-RAM / CD-RW rewrite:** burn, then burn a different set → erase confirmation → new content only.
3. **Over-capacity:** queue > free bytes → blocked pre-burn.
4. **Write-once append (deferred feature):** current build must REFUSE a second data burn to a non-blank CD-R with a clear message. If append is wanted, implement `xorriso -dev` (not `-outdev`) growisofs-style appends + `drutil` equivalent, then test: two sequential burns → both sessions' files visible after remount.
5. **Cancel mid-burn** and **drutil output audit** as in Phase 5.

## Phase 7 — Graceful failure handling (woven through, hardened here)

**Files:** `src/disc/*`, `src/ffi/disc.rs`, all three frontends (GTK, TUI, mac).

- [ ] **Drive disconnect** — the detection poll notices the device vanished → invalidate the loaded-disc session, hide/disable disc actions, show a banner ("Drive disconnected — reconnect and reload"). Any in-flight subprocess op errors out (child dies with the device); the app never wedges. On mac, DiskArbitration disappearance drives the same. *Already partially in place: mac nav falls back when a drive disappears; TUI shows "No optical drives found" after `r`.*
- [ ] **Disc read error / scratch** — `cdiocddasrc`/decode read failure surfaces as a GStreamer bus error (existing handling); ripping offers **retry / skip track / abort**, and reports which tracks failed. Playback of a bad track marks it broken like any unreadable file. *Already partially in place: a failed rip track is counted and reported; retry/skip UI is the remaining piece.*
- [ ] **Blank/append/capacity errors** — over-capacity is blocked pre-burn (shipped in Phases 5–6); a burn failure is parsed from the tool's exit/stderr tail and shown (shipped); verify the messages read sensibly against real failures.
- [ ] **Timeouts** — rip already has the 30 s position watchdog; add a coarse wall-clock watchdog to the burn/erase subprocess runner (suggested: kill + report after 30 min without exit).
- [ ] **No-drive / unsupported-media** — clean messaging when no optical drive exists or the media can't serve the request (shipped: non-blank write-once refusals, "insert a blank disc" states; verify wording in the flesh).
- [ ] **Commit** `feat(disc): graceful drive/disc/burn error handling`.

**Internal tests (runnable without media, for Opus to add while hardening):**
- Unit-test the stderr-tail error extraction and (if drutil emits one) the burn progress parser against captured fixtures.
- Unit-test the subprocess watchdog with a `sleep`-style child (kill fires, error surfaces).
- Simulated disconnect: TUI/mac behavior when `list_drives` goes empty mid-session (drop the drive from the fixture path) — nav resets, no panic.

**Hardware tests (Opus):**
1. Pull the USB drive mid-rip → rip errors out with the stall/read error, app stays responsive, banner appears on the next poll.
2. Pull the USB drive mid-burn → subprocess dies, error surfaced, no wedge.
3. Scratched/dirty disc rip → failed tracks counted, others complete.
4. Eject from Finder while the Discs tab is open → rows update to "No disc" within a poll.

---

## FFI surface (`src/ffi/disc.rs`) — JSON-over-FFI, mac parity

Mirrors the device-sync FFI conventions (`#[unsafe(no_mangle)] pub extern "C"` — Rust 2024, JSON `*mut c_char` freed with `sparkamp_free_string`, long ops via `sparkamp_tick` + mpsc + a dirty/progress counter). Swift enumerates the optical device via IOKit and feeds TOC/track/media info in; core owns discid, gnudb, tags, and the rip pipeline; Swift owns DiscRecording burning.

**Header is hand-maintained.** cbindgen was removed (it can't parse Rust 2024 `#[unsafe(no_mangle)]`), so these `sparkamp_disc_*` signatures must be added **by hand** to `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` — there is no regeneration step. Match the existing declaration style in that header.

- `sparkamp_disc_list_drives(ctx) -> json` (array of `OpticalDrive` — every drive present)
- `sparkamp_disc_track_uris(ctx, drive_id, toc_json) -> json` (playlist URIs scoped to the drive)
- `sparkamp_gnudb_query(ctx, toc_json) -> json` / `sparkamp_gnudb_read(ctx, category, discid) -> json`
- `sparkamp_gnudb_submit(ctx, xmcd_json, category, mode) -> json`
- `sparkamp_disc_rip(ctx, drive_id, tracks_json, dest, quality) -> job_id` + progress polled in tick
- `sparkamp_disc_prepare_audio(ctx, burnlist_json) -> temp_paths_json`, then `sparkamp_burn_audio(ctx, drive_id, temp_paths_json) -> job_id` / `sparkamp_burn_data(ctx, drive_id, files_json) -> job_id` (Linux via core subprocess; mac burns via DiscRecording driven from Swift with the same Burn list). Every drive-scoped call takes the `drive_id` from `OpticalDrive`.
- `sparkamp_disc_fs_error(...)` events surfaced through the error callback

---

## TUI parity (Ratatui)

The TUI (`sparkamp`, no `--ui`) is a first-class Rust frontend that calls the shared core **directly, in-process** (no FFI) — the same way it already drives `crate::media_library` and `crate::devices`. So every disc *capability* is identical; only the presentation degrades to a terminal. **Existing TUI modules to extend, not reinvent:** `frontends/tui/ui/media_library.rs` (add the "Disc Drives" section + per-drive view), `frontends/tui/ui/overlays.rs` (all modals/dialogs below), `frontends/tui/id3.rs` + `frontends/tui/ui/id3.rs` (per-track tag override editor — already exists), `frontends/tui/keys.rs` (keybindings + the `i` help overlay), `frontends/tui/media_library.rs` (state).

Terminal-appropriate UX for each feature (do these as a TUI task in every phase — mirror the phase's core work into these modules):

- [ ] **Detect/list drives (Phase 1).** A "Disc Drives" section in the media-library pane listing **one row per drive** (label + media state), identical model to the external-device list. Enter opens the per-drive view.
- [ ] **Play (Phase 1).** In the per-drive audio-disc view, a track table; keys to add the selected track or the whole disc to the active playlist (reuse the existing playlist-append path).
- [ ] **Identify + override (Phase 2).** A "Match on gnudb" action → results overlay (exact/inexact list) → apply; then the **existing TUI ID3 editor** edits per-track title/artist/album/year/genre, editable even with no match. A disc-level overlay holds album/artist/year/genre/category.
- [ ] **Rip (Phase 3).** A rip overlay: pick tracks, destination (defaults to first watched folder, prompts first time), quality; a **text progress bar** per track + overall via the tick/mpsc counter; cancel key.
- [ ] **Submit (Phase 4).** Category selector overlay (fixed set, default `misc`) + confirm; result/error shown in an overlay. Honors `gnudb_submit_mode_test`.
- [ ] **Burn audio + data (Phases 5–6).** A Burn-list overlay: add/remove/reorder, running capacity meter, disc-type detection, **confirm-before-erase** on a non-empty RW disc; text progress bar during burn; cancel kills the subprocess.
- [ ] **Errors (Phase 7).** Drive-disconnect / read-scratch / burn-failure surfaced as overlay messages from the same typed core errors; no thumbnails needed (disc flows have no artwork diff — that was device-sync only).

**Terminal limitations (acceptable):** no image thumbnails (N/A here), progress as text bars, everything modal via `overlays.rs`. No capability is dropped — only visuals simplify.

## Default audio-CD player integration (auto-open on insert)

**Goal:** inserting an audio CD launches (or foregrounds) Sparkamp with the
Media Library window open and navigated to the drive that received the disc —
i.e. Sparkamp can be the system's audio-CD handler.

**Shared in-app behavior (macOS shipped 2026-07-07; GTK to implement):**
- Setting `disc.auto_show_inserted_audio_cd` (default **on**): while the app
  runs, the disc poll watches for a drive transitioning to "audio CD loaded";
  on that transition it opens the Media Library window (if closed) and
  navigates to that drive's detail view.
- The insertion check runs on every poll from app start — NOT only while the
  ML window is open (macOS moved the disc poll out of the ML-visible gate for
  this; ~10 s cadence, background queue).
- Cold-launch caveat, accepted by design: the first poll after launch treats
  an already-loaded audio CD as "inserted" — so a launch triggered by the OS
  handler navigates correctly, and a manual launch with a CD already in the
  drive also jumps to it. Users who dislike that turn the setting off.

**macOS — registering as the handler (user action, one time):** the OS-side
launcher is the digihub service: System Settings → **CDs & DVDs** (the pane
appears while an optical drive is attached) → "When you insert a music CD" →
**Open other application…** → Sparkamp.app. Sparkamp's Settings → Media
Library → Discs has an "Open CDs & DVDs Settings…" button plus the
instructions. (The underlying preference is the `com.apple.digihub` domain,
key `com.apple.digihub.cd.music.appeared`; Sparkamp deliberately does NOT
write it programmatically — the action codes are undocumented and silently
rewriting system handler prefs is hostile. The Settings pane is the supported
path.)

**GTK/Linux — registering as the handler (Opus):**
- Advertise the x-content type in the desktop entry: add
  `x-content/audio-cdda;` to a `MimeType=` line in
  `packaging/dev.sparkamp.Sparkamp.desktop` (this is what GNOME's Settings →
  Removable Media lists under "CD audio"; the user picks Sparkamp there).
- Handle the launch: GNOME activates the handler with the disc's location
  (gio passes the `cdda://` mount/URI as an argument, or activates via
  D-Bus). Accept and ignore an unrecognized `cdda://…`/device argument
  gracefully, then rely on the same shared behavior: startup disc poll sees
  the audio CD → open the ML window → navigate to that drive.
- The in-app transition watcher covers insertions while running (GNOME may
  also autostart the handler only on user click depending on the "Removable
  Media" prompt setting — both paths end at the same navigation).
- Flatpak note: x-content association works from inside the sandbox via the
  exported .desktop; drive polling already requires the optical device
  permissions from Phase 1.

---

## GTK implementation notes (read before starting — decisions made during the macOS/TUI build)

Everything below was built and user-approved on macOS/TUI during 2026-07-06/07;
GTK must mirror the *behavior* (presentation is GTK's own). Reference
implementations: `frontends/SparkampMac/Sources/DiscDriveView.swift`,
`MediaLibraryWindow.swift` (discsSection), `SparkampModel+Discs.swift`, and
`frontends/tui/media_library.rs`. The core is shared — GTK calls `crate::disc`
directly, in-process, like the TUI does.

**Navigation & views (Phase 1 additions):**
- **Disc Drives overview page**: the sidebar "Disc Drives" group header is
  clickable and opens a card grid (one card per drive) in the style of the
  Devices overview — card = disc icon + drive label + media state ("Audio CD
  (8 tracks)" / "Blank CD-R" / "Data disc" / "No disc") + a detail line
  (audio: "MM:SS of audio" from the TOC; blank: "700 MB writable"; data:
  free-of-total; empty: an insert hint). Clicking a card opens the drive
  detail. Empty state: "No disc drives connected".
- **Unplug fallback**: if the drive being viewed disappears, navigate to the
  discs overview (not the files view) — parity with device disappearance.
- **Iconography**: a disc glyph with the media format badged on it — CD, DVD,
  CD-R, CD-RW, DVD-R, DVD-RW, DVD-RAM. Pressed discs report no writable kind:
  badge CD vs DVD by capacity (>1 GB = DVD). Empty tray = bare drive glyph,
  no badge. Sidebar rows stay icon-free (user preference) — plain label +
  media-state line.
- **Devices/Discs separation**: optical media must NOT appear in the Devices
  (removable volume) list — a mounted audio CD or data disc belongs to Disc
  Drives only. (mac filters by DiskArbitration media kind + the `.TOC.plist`
  marker; GTK/udisks needs the equivalent optical exclusion.) Known deferred
  question: DVD-RAM could arguably be exempted to act as a random-access
  sync device; refused for now.

**Playlist integration (Phase 1/2 behaviors):**
- Disc adds go through `sparkamp_playlist_add_entry`-equivalent semantics:
  title/artist/album + exact TOC duration, NO tag scan or duration probe.
  Playlist rows read "Artist - Title" like every other entry.
- The xmcd **sampler convention**: a track title containing " / " splits into
  per-track artist + title (disc artist becomes album_artist for rip tagging;
  the split title names ripped files). One shared rule across add, edit
  propagation, and rip — see `discEntryMeta` (Swift) / `add_disc_entries`
  (TUI) / `tag_fields_for_track` (core).
- Adds honor the replace/append add-behavior setting and autoplay-on-add
  (start the first new track only when the playlist was replaced or empty —
  never interrupt playback). Same semantics as the ML double-click path.

**Tag persistence + sync (built after the plan was written):**
- Per-disc tags persist in `~/.config/sparkamp/disc_tags.toml`
  (`crate::disc::tagstore`): the user's tag set AND the untouched gnudb match
  ("official") per freedb id. GTK must restore on drive view (names survive
  app restarts) and write through on match receipt / editor save.
- **Editing disc tags updates already-added active-playlist rows
  immediately** (path-keyed; `sparkamp_playlist_update_entry_meta`
  equivalent) — not just future adds.
- General expectation that also applies to the file ID3 editor: saves update
  every matching playlist row (canonical path match, duplicates included)
  AND the ML DB row at once; and playlist rows must repaint on metadata-only
  changes even when nothing is playing (a mac NSTableView diff bug hid this —
  make sure GTK's list rebinds on content change, not just row count).

**gnudb flow details:**
- Identify: single exact match auto-applies; multiple matches open a picker;
  zero matches → honest status pointing at the tag editor. Lookups run in
  the background and never block the UI; results survive leaving the view —
  a match list arriving while the view is closed is parked and re-presented
  on the next visit, tied to the drive it was for.
- Submit: the action is visible only when the disc is unknown to gnudb
  (always) or the user's tags differ from the official baseline (compare
  against tagstore's `official`). Category picker from the fixed 11-set,
  prefilled by `gnudb::suggest_category`. Validation before network:
  artist+album non-empty, every track genuinely titled ("Track N"
  placeholders rejected with the track numbers listed). Revision = official
  + 1 for updates, 0 for new. Blank email → capture modal first (see
  Settings section). Test-mode results labeled "(test mode — not
  published)".

**Rip flow details (beyond the Phase 3 bullets):**
- Rip dialog: tracks preselected from the current selection (else all),
  destination + quality remembered. **Unwatched-destination policy is
  warn-only** (user decision): a destination outside every watched folder
  shows a warning ("files will rip here but won't appear in the library"),
  rips anyway, and the completion status reports honestly ("not in library
  (destination isn't a watched folder)" / "only N added"). Never auto-add
  watch folders.
- Import stamps `last_scanned` (core fix) — freshly ripped rows must NOT
  show the "not yet scanned" clock indicator.

**Burn flow details (beyond the Phase 5/6 bullets):**
- Burn list is fed from the ML files view (context-menu "Add to Burn List"
  on GTK, like mac; TUI uses `b`). Dedup by path; reorder supported.
- The burn UI lives on the drive detail view whenever non-audio media is
  loaded (blank OR rewritable-with-content OR data disc): queue list with
  remove, audio-minutes and data-bytes capacity meters that BLOCK the burn
  when over, Burn Audio CD / Burn Data Disc actions, erase-confirmation
  dialog for RW-with-content, refusal message for write-once-with-content.
- Progress phases shown to the user: Starting… → Erasing… (RW) →
  "Preparing i/N · <track>" (audio prepare, per track) → "Burning… (this
  takes a while)" → status line result (success with counts, or the burn
  tool's stderr tail). Cancel works between prepare tracks and kills the
  burn/erase subprocess. Burning runs entirely off the UI thread and
  survives the window/view being closed and reopened.
- Verification has NO distinct phase yet (it happens inside the tool during
  "Burning…"); a verify failure surfaces as a burn failure. The Opus drutil
  stdout audit may enable real percent + a "Verifying…" phase; GTK/Linux
  has no tool-level verify — candidate readback check is an open follow-up.
- Data burns write the companion playlist (`playlist.m3u8`/`.m3u` per the
  app-wide format setting) at the disc root, files staged flat with name
  dedup ("song (2).mp3").

**Threading conventions:** every disc subprocess/network/GStreamer call off
the UI thread (`gio::spawn_blocking` on GTK); detection is subprocess-backed,
so poll lazily (mac: ~10 s while the window is open; TUI: on tab entry +
explicit rescan) — never per-frame.

---

## Verification

- **Rust (this Linux dev box):** disc-ID vectors, CDDB arg builder, xmcd round-trip, gnudb response-code handling (mock HTTP), rip path/tag mapping, media-capability parse — `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`, zero warnings/failures (CLAUDE.md gate). Burning/read against a real drive is manual.
- **TUI manual (Linux):** run `sparkamp`, exercise the full matrix (list drives → play → identify → override tags → rip → submit-test → burn audio → burn data → disconnect mid-op) entirely from the terminal.
- **Linux manual:** external DVD-RAM drive — play a track + whole disc; identify a known CD via gnudb; override tags; rip a track to tagged MP3 (auto-imported); burn an audio CD-R; burn a data MP3 CD-R and a DVD-RAM (write-once + rewrite); pull the USB mid-op to confirm graceful failure.
- **macOS (engineer on a Mac):** same matrix via DiscRecording + the AIFF mount; `xcodebuild` + manual.

## Self-review notes (parity vs Winamp / the 7 asks)

1 play tracks/disc → Phase 1. 2 gnudb match → Phase 2 (discid + query/read). 3 override tags + submit → Phase 2.4 + Phase 4. 4 rip to MP3 with prefilled tags → Phase 3. 5 burn audio CD → Phase 5. 6 burn data disc, write-once + RW → Phase 6. 7 graceful drive/disc errors → Phase 7 (woven throughout; subprocess isolation is the key enabler). **All three frontends covered:** shared core + **GTK and TUI direct in-process calls** + SparkampMac via JSON-over-FFI, with burning native per platform (libburnia CLIs on Linux, DiscRecording on macOS). The TUI reaches full capability parity (terminal-simplified presentation only) — see the TUI parity section.
