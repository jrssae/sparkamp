# Optical Disc (CD/DVD) Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans to implement task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Winamp-parity optical-disc support across **all three Sparkamp frontends — GTK (`sparkamp --ui`), TUI (`sparkamp`), and SparkampMac**: play audio CDs, identify discs via gnudb.org, override/prefill ID3 tags and submit corrections upstream, rip to tagged MP3, and burn audio CDs and data (MP3) CD/DVDs — with graceful handling of drive/disc failures.

**Architecture:** A shared Rust core owns everything platform-neutral — the freedb/CDDB **disc-ID math**, the **gnudb HTTP client** (query / read / submit), the **tag-override model** (reusing `src/id3_editor.rs` + `src/tags.rs`), and the **GStreamer rip/encode pipeline** (GStreamer already ships on both platforms). A thin **platform device layer** does only what must be native: raw disc/TOC access and burning. Linux uses GStreamer `cdiocddasrc` for read/rip, `cd-info` (libcdio) to probe drive/media, and libburnia CLIs — `xorriso` (data ISO9660/UDF) and `cdrskin` (audio CD + blanking) — for burning. macOS uses the auto-mounted AIFF volume + CoreAudio for read and **DiscRecording.framework** for all burning/blanking. The **GTK and TUI frontends call the core directly** (same crate, in-process — exactly as they already call `crate::devices` / `crate::media_library`); **SparkampMac** calls it through the same **JSON-over-FFI** style used by the device-sync work (`src/ffi/`), with long-running rip/burn progress delivered through the existing `sparkamp_tick` + mpsc callback mechanism. All disc *logic* is shared core, so the three frontends differ only in presentation.

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

- [ ] `gnudb_email: String` — **default `"sparkamp@fastmail.com"`**, editable in Settings; used as the CDDB `hello` user id and the submission `User-Email` header. A Settings text field exposes it on all three frontends (GTK, TUI, mac).
- [ ] `gnudb_submit_mode_test: bool` — default `true` until a real submission is verified end-to-end, then the UI offers "submit".
- [ ] `rip_dest_dir: Option<PathBuf>` — last chosen rip destination; when unset, the rip dialog **starts at the Media Library's first watched folder** but still prompts before the first rip.
- [ ] `rip_mp3_quality: u8` — LAME VBR quality (default 2 ≈ V2 ~190 kbps); Settings dropdown (V0/V2/320 CBR).

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
- [x] **"Disc Drives" sidebar group.** macOS: sidebar group (one row per drive, label + media state) + `DiscDriveView` detail (track table, Add Disc / Scan / Eject, banners for no-disc/blank/data). TUI: third ML tab "Discs" (drive rows + track list). GTK: Linux box's task.
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

  - [ ] Failing test first with a **known vector** (e.g. a 3-track disc with published discid); run, verify fail; implement; verify pass. Include the `cddb query` argument string builder `discid ntrks off1..offn nsecs` and test it too.

- [ ] **Task 2.2 — gnudb HTTP client.** `query(toc, email) -> Vec<DiscMatch>` and `read(category, discid, email) -> XmcdEntry`.
  - **Handshake:** the CDDB `hello` is **four `+`-separated fields — `username+hostname+clientname+version`** — so split the configured email at `@`: `sparkamp@fastmail.com` → `hello=sparkamp+fastmail.com+Sparkamp+<pkg_version>`. Do **not** put the whole address in one field. `clientname` must be descriptive ("Sparkamp"); `version` from `env!("CARGO_PKG_VERSION")`. Always send `proto=6` (UTF-8). URL-encode: spaces→`+`, other specials→`%XX`.
  - GET `http://gnudb.gnudb.org/~cddb/cddb.cgi?cmd=cddb+query+<discid>+<ntrks>+<off1>+…+<offn>+<nsecs>&hello=sparkamp+fastmail.com+Sparkamp+<version>&proto=6`. Handle response codes **200** (exact), **211** (inexact list), **202** (none), **403** (corrupt). `read` → `cmd=cddb+read+<category>+<discid>` (same hello+proto). Timeouts + offline → typed error, surfaced as "couldn't reach gnudb".
  - Add a helper `fn hello_param(email: &str) -> String` that splits on the last `@` (fallback: whole string as username, `localhost` as host if no `@`); unit-test it.
- [ ] **Task 2.3 — xmcd parse/build.** Parse `DISCID, DTITLE (artist / album), DYEAR, DGENRE, TTITLEn, EXTD, EXTT`. Build the same format for submission. Unit-test round-trip on a captured xmcd sample.
- [ ] **Task 2.4 — tag override UI.** On match, prefill a per-track editor (reuse `id3_editor::TagFields`). **User can edit every field even with no match** (blank template: artist/album/year/genre + per-track title). These overrides drive both rip tagging (Phase 3) and submission (Phase 4).
- [ ] **Commit** `feat(disc): gnudb query/read/parse + per-track tag override`.

## Phase 3 — Rip to MP3

**Files:** Create `src/disc/rip.rs`; modify `src/ffi/disc.rs` (async rip + progress via `sparkamp_tick`), Settings UI. Test: `src/disc/rip.rs` (pipeline-string builder + path/tag mapping; the actual GStreamer run is a manual/integration check).

- [ ] **Rip pipeline (GStreamer, shared).** Per track: source differs by platform — Linux `cdiocddasrc track=N device=<drive_id>` (the selected `OpticalDrive.id`), macOS `filesrc location=<aiff> ! decodebin` — then shared `audioconvert ! lamemp3enc quality=<cfg> ! filesink`. After encode, write tags with `id3_editor::write_tag_fields` from the Phase-2 overrides (title/artist/album/album-artist/year/genre/track#/total). Prefer post-encode tag write over `id3v2mux` so one code path owns tags.
- [ ] **Destination + naming.** Organize `Artist/Album/NN - Title.mp3` under `rip_dest_dir`; dialog **starts at the first watched folder**, prompts before the first rip, remembers the choice.
- [ ] **Auto-import.** After rip, add the files to the Media Library (reuse the existing import/scan path) so they appear immediately.
- [ ] **Progress + cancel.** Per-track and overall progress via the tick/mpsc mechanism (mirrors metadata scanning). Cancel stops after the current track.
- [ ] **Commit** `feat(disc): rip audio CD to tagged MP3 with auto-import`.

## Phase 4 — Submit to gnudb

**Files:** Modify `src/disc/gnudb.rs`, `src/ffi/disc.rs`, Settings UI.

- [ ] **Category selection.** gnudb submissions require one of a **fixed** category set (`blues, classical, country, data, folk, jazz, misc, newage, reggae, rock, soundtrack`) — not free-text ID3 genre. Show a dropdown at submit time, prefilled by a best-effort map from the matched/ID3 genre, **defaulting to `misc`**. Pass the chosen category as the `Category` header + `sparkamp_gnudb_submit` argument.
- [ ] **Build xmcd** from the current (matched or user-overridden) tags via `xmcd.rs` (Phase 2.3). Enforce gnudb validation: non-empty disc artist/title, **every** track titled (reject "Track N" defaults), correct DISCID, revision 0 for new / incremented for update.
- [ ] **POST submit.cgi** with headers `Category, Discid, User-Email: <gnudb_email>, Submit-Mode: <test|submit>, Charset: UTF-8, Content-Length, X-Cddbd-Note`. Default to **test** mode (`gnudb_submit_mode_test`) until a real round-trip is confirmed; handle **200** ok, **500/501** header/validation errors surfaced to the user.
- [ ] **Register the app** — one-time human action: email `info@gnudb.org` announcing client "Sparkamp" + contact (`sparkamp@fastmail.com`). Note in the plan; not a code step.
- [ ] **Commit** `feat(disc): submit disc metadata to gnudb`.

## Phase 5 — Burn audio CD

**Files:** Create `src/disc/burn.rs` (Linux subprocess orchestration), `src/disc/burnlist.rs` (the dedicated Burn list model); modify `src/ffi/disc.rs`, all three frontends (GTK/TUI/mac Burn list UI). macOS burning is implemented Swift-side over DiscRecording, driven by the same Burn list.

- [ ] **Burn list model** — a dedicated queue (Winamp-style), separate from the active playlist: add/remove/reorder library tracks; running total vs 74/80-min CD capacity with an over-capacity warning.
- [ ] **Confirm before erasing.** Blanking a non-empty rewritable disc (CD-RW/DVD-RW/DVD-RAM) destroys its contents — require an explicit confirmation dialog first (mirrors the CLAUDE.md Deletion-Rule spirit for user data). Applies to both audio (this phase) and data (Phase 6) RW burns; never auto-blank a disc that already has content without the prompt.
- [ ] **Prepare audio.** Transcode each burn-list track to Red Book PCM (44.1 kHz/16-bit/stereo WAV) via GStreamer into a temp dir.
- [ ] **Linux burn.** Target the selected drive's node (`OpticalDrive.id`, e.g. `/dev/sr0`). Blank first if rewritable and not empty (`cdrskin dev=<drive_id> blank=fast`), then `cdrskin dev=<drive_id> -audio -pad speed=<n> track01.wav …` (DAO). Parse progress/percent from stderr; map non-zero exit to a typed error. Subprocess is killable for cancel/timeout.
- [ ] **macOS burn.** Swift `DRBurn` + `DRAudioTrack` per WAV/AIFF; DiscRecording handles blanking + progress via `DRBurnStatus` notifications.
- [ ] **Commit** `feat(disc): burn audio CD from a dedicated burn list`.

## Phase 6 — Burn data (MP3) disc, write-once + rewritable

**Files:** Modify `src/disc/burn.rs`, `src/ffi/disc.rs`, all three frontends (GTK, TUI, mac).

- [ ] **Data burn list** — reuse the Burn-list model in "data" mode: arbitrary files/folders of MP3s; show bytes vs media capacity (CD ~700 MB, DVD ~4.7 GB, DVD-RAM per media).
- [ ] **Linux burn.** Target the selected drive's node (`OpticalDrive.id`). `xorriso` builds ISO9660+Joliet+UDF and burns: `xorriso -outdev <drive_id> -blank as_needed -joliet on -add <files> -commit`. **Write-once** (CD-R/DVD-R/DVD+R): multisession append when not closed, else new session. **Rewritable** (CD-RW/DVD-RW/DVD-RAM): `-blank as_needed` (fast blank) then write; DVD-RAM may also be written as a mounted UDF filesystem (random access) — prefer xorriso for uniformity but detect DVD-RAM so blanking isn't forced when appending. Progress/error via stderr parse + exit code.
- [ ] **macOS burn.** Swift `DRBurn` + `DRFolder`/`DRFilesystemTrack` (ISO9660/Joliet/UDF); DiscRecording auto-detects write-once vs RW and handles blanking + append.
- [ ] **Commit** `feat(disc): burn data MP3 discs (write-once + rewritable)`.

## Phase 7 — Graceful failure handling (woven through, hardened here)

**Files:** `src/disc/*`, `src/ffi/disc.rs`, all three frontends (GTK, TUI, mac).

- [ ] **Drive disconnect** — the detection poll notices the device vanished → invalidate the loaded-disc session, hide/disable disc actions, show a banner ("Drive disconnected — reconnect and reload"). Any in-flight subprocess op errors out (child dies with the device); the app never wedges. On mac, DiskArbitration disappearance drives the same.
- [ ] **Disc read error / scratch** — `cdiocddasrc`/decode read failure surfaces as a GStreamer bus error (existing handling); ripping offers **retry / skip track / abort**, and reports which tracks failed. Playback of a bad track marks it broken like any unreadable file.
- [ ] **Blank/append/capacity errors** — over-capacity is blocked pre-burn with a clear message; a burn failure (bad media, buffer underrun, write error) is parsed from the tool's exit/stderr and shown, leaving a partially written disc reported (not a silent hang).
- [ ] **Timeouts** — every subprocess op has a watchdog; exceeding it kills the child and reports a timeout.
- [ ] **No-drive / unsupported-media** — clean messaging when no optical drive exists or the media type can't be used for the requested op (e.g. asking to burn an audio CD to DVD-RAM in a CD-incapable drive).
- [ ] **Commit** `feat(disc): graceful drive/disc/burn error handling`.

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

## Verification

- **Rust (this Linux dev box):** disc-ID vectors, CDDB arg builder, xmcd round-trip, gnudb response-code handling (mock HTTP), rip path/tag mapping, media-capability parse — `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`, zero warnings/failures (CLAUDE.md gate). Burning/read against a real drive is manual.
- **TUI manual (Linux):** run `sparkamp`, exercise the full matrix (list drives → play → identify → override tags → rip → submit-test → burn audio → burn data → disconnect mid-op) entirely from the terminal.
- **Linux manual:** external DVD-RAM drive — play a track + whole disc; identify a known CD via gnudb; override tags; rip a track to tagged MP3 (auto-imported); burn an audio CD-R; burn a data MP3 CD-R and a DVD-RAM (write-once + rewrite); pull the USB mid-op to confirm graceful failure.
- **macOS (engineer on a Mac):** same matrix via DiscRecording + the AIFF mount; `xcodebuild` + manual.

## Self-review notes (parity vs Winamp / the 7 asks)

1 play tracks/disc → Phase 1. 2 gnudb match → Phase 2 (discid + query/read). 3 override tags + submit → Phase 2.4 + Phase 4. 4 rip to MP3 with prefilled tags → Phase 3. 5 burn audio CD → Phase 5. 6 burn data disc, write-once + RW → Phase 6. 7 graceful drive/disc errors → Phase 7 (woven throughout; subprocess isolation is the key enabler). **All three frontends covered:** shared core + **GTK and TUI direct in-process calls** + SparkampMac via JSON-over-FFI, with burning native per platform (libburnia CLIs on Linux, DiscRecording on macOS). The TUI reaches full capability parity (terminal-simplified presentation only) — see the TUI parity section.
