# Disc UX Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the five phase-2 disc features (auto-refresh, drag-to-drive, CD-TEXT, burn-progress overlay, data-disc browsing) plus the UX-audit pre-work (file split, Send-to consistency fixes).

**Architecture:** Core-first. New pure core: media fingerprint, CD-TEXT v07t builder + disc-metadata defaults, structured `BurnProgress` with a streaming tool runner, read-only disc mount + file listing. GTK consumes via the existing per-drive `BurnQueues`/holder seams. The 9.5k-line `media_library.rs` is mechanically split into page sections FIRST so later tasks land in small files.

**Tech Stack:** Rust (edition 2024), GTK4 (gtk4-rs), GStreamer, cdrskin/libburn (CD-TEXT via Sony v07t sheet), udisks2 over zbus, Ratatui (TUI), Swift (blind).

**Spec:** `docs/superpowers/specs/2026-07-16-disc-ux-phase2-design.md` (incl. the P0/P1 pre-work section with gaps G1–G4).

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'` — host builds fail. Zero warnings, zero failures before any completion claim.
- NEVER `git push` without a fresh explicit user instruction.
- Drive-contention rule: no fresh drive probes from UI handlers; all disc reads guarded by `detect::set_exclusive_read` where they touch the device; menus read cached `current_drives`/`current_devices` only.
- RefCell borrows short-lived — never across a UI call, `.await`, or `select_row` (three crashes on this branch came from violating this).
- Async pattern for anything blocking: `glib::spawn_future_local` + `gio::spawn_blocking(...).await`; only Send data crosses; SQLite lookups happen before the spawn.
- `gtk_safe()` on user-visible strings carrying metadata/errors.
- FFI header `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` is hand-maintained; every new/changed `sparkamp_*` symbol added there manually. Swift is BLIND — flag for a Mac pass; never claim it builds.
- Interactive GUI verification is done by the human at checkpoints — implementer gates are build + suite; say so in reports, never claim GUI behavior verified.
- Commit style: conventional prefix, body WHY + verification line, trailer `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

---

### Task 1 (P0): Split media_library.rs into page sections

**Files:**
- Modify: `frontends/gtk/window/media_library.rs` (9,477 lines → scaffolding only)
- Create: `frontends/gtk/window/ml_files.rs`, `ml_playlists.rs`, `ml_devices.rs`, `ml_discs.rs`

**Interfaces:**
- Consumes: the repo's established include!-section pattern — see `frontends/gtk/window/mod.rs:80-122` ("every file is a plain byte slice" stitched by `include!`).
- Produces: identical program. Later tasks edit the new small files.

**Method (this is the whole trick):** `media_library.rs` is essentially one giant `fn open_media_library_window(...)`. Rust's `include!` works INSIDE a function body — the split is *sequential* `include!("ml_<page>.rs");` lines replacing contiguous statement blocks, exactly the mechanism `window/mod.rs` already uses at module level. No signatures change, no code is rewritten — blocks move byte-for-byte.

- [ ] **Step 1: Map section boundaries.** Grep the page markers (`// ── Page:` comments and the audited block starts): devices view (~line 800–2660), discs page (~2661–2890 build + `refresh_discs`/sidebar-wiring ~8180–8560), files page (~2890–4480), playlists manage+editor (~4480–8180). Boundaries must sit BETWEEN complete statements. Adjust to the real file — line numbers drift; the section comments are the anchors.
- [ ] **Step 2: Move blocks.** Cut each contiguous block into its file verbatim; leave `include!("ml_devices.rs");` etc. at the cut point. Shared `let` bindings used across pages (the Rcs: `current_drives`, `burn_queues`, holders, `state`, sidebar widgets) stay in `media_library.rs` BEFORE the includes. If a block is not contiguous (e.g. discs page build vs its poll), use two includes (`ml_discs.rs`, keep order) rather than reordering code.
- [ ] **Step 3: Build + full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -3 && cargo test 2>&1 | grep "test result"'`
Expected: zero warnings, 967+ pass. `git diff --stat` should show only moves (huge -/+ on media_library.rs, new files) — no logic edits.
- [ ] **Step 4: Sanity line counts** — `wc -l frontends/gtk/window/media_library.rs frontends/gtk/window/ml_*.rs`; no file over ~3,000 lines. If one still is, split it again at a page-internal marker.
- [ ] **Step 5: Commit**

```bash
git add frontends/gtk/window/
git commit -m "refactor(gtk): split media_library.rs into page include-sections

9.5k lines was past maintainable; phase-2 work lands more into it.
Byte-for-byte block moves into ml_files/ml_playlists/ml_devices/
ml_discs stitched by include! inside the window fn — the same
mechanism window/mod.rs already uses. Zero behavior change; full
suite green."
```

---

### Task 2 (P1+B): Send-to consistency + shared queue helper + drag-to-drive

**Files:**
- Modify: `frontends/gtk/window/util.rs` (new helper), `ml_files.rs`, `ml_playlists.rs`, `ml_devices.rs`, `ml_discs.rs` (post-split locations of the four send-drive bodies + the editor button row + the disc sidebar rows), `frontends/gtk/window/player.rs` (active-playlist send site)

**Interfaces:**
- Consumes: `BurnQueues`, `add_files`, `AddOutcome` (core), `burn_refresh_holder`, `duration_probe::probe_duration_full`, `build_send_to_menu`/`SendToActions` (util.rs), `send_playlist_run` (editor's whole-playlist→device sender), `show_unreadable_dialog`.
- Produces: `queue_paths_to_drive(...)` in util.rs — the ONE async probe-queue-report path all five call sites (4 menus + drag) use. Task 10/11 mirror the wording.

- [ ] **Step 1: The shared helper** (util.rs; replaces the four near-identical async bodies):

```rust
/// The one Send-to ▸ Disc Drive path: metadata is supplied by the caller
/// (SQLite lookups must happen before any spawn), unknown durations are
/// probed off-thread, readable files queue onto the drive's burn list,
/// unreadable ones are skipped and listed, and an open burn panel is
/// live-refreshed. `status` receives interim ("Reading files…") and final
/// (AddOutcome::status_message) text — every view routes it to its quiet
/// status label (G3: no success modals anywhere).
pub(super) fn queue_paths_to_drive(
    drive_id: String,
    drive_label: String,
    paths: Vec<std::path::PathBuf>,
    metas: std::collections::HashMap<std::path::PathBuf, (String, Option<u32>, u64)>,
    burn_queues: std::rc::Rc<std::cell::RefCell<crate::disc::burnlist::BurnQueues>>,
    burn_refresh_holder: std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<dyn Fn()>>>>,
    status: std::rc::Rc<dyn Fn(String)>,
    win_wk: glib::WeakRef<gtk4::Window>,
) {
    if paths.is_empty() {
        status("Select tracks first".to_string()); // G4
        return;
    }
    status("Reading files…".to_string());
    glib::spawn_future_local(async move {
        let probe_metas: Vec<(std::path::PathBuf, Option<u32>)> = paths
            .iter()
            .map(|p| (p.clone(), metas.get(p).and_then(|m| m.1)))
            .collect();
        let probed: Vec<(std::path::PathBuf, Option<u32>)> =
            gio::spawn_blocking(move || {
                probe_metas
                    .into_iter()
                    .map(|(p, known)| {
                        let secs = known.or_else(|| {
                            crate::duration_probe::probe_duration_full(&p)
                                .map(|d| d.as_secs() as u32)
                        });
                        (p, secs)
                    })
                    .collect()
            })
            .await
            .unwrap_or_default();
        let out;
        let total;
        {
            let mut queues = burn_queues.borrow_mut();
            let list = queues.queue(&drive_id);
            out = crate::disc::burnlist::add_files(
                list,
                &paths,
                |p| metas.get(p).cloned().unwrap_or_else(|| {
                    (p.display().to_string(), None, 0)
                }),
                |p| probed.iter().find(|(pp, _)| pp == p).and_then(|(_, s)| *s),
            );
            total = list.len();
        } // queues borrow drops before any UI call
        if let Some(refresh) = burn_refresh_holder.borrow().as_ref() {
            refresh();
        }
        status(out.status_message(&drive_label, total));
        if let (Some(body), Some(win)) = (out.failed_message(), win_wk.upgrade()) {
            show_unreadable_dialog(&win, &body);
        }
    });
}
```

- [ ] **Step 2: Rewire the four send-drive actions** (files, editor, device view, active playlist) to build `(paths, metas)` from their **live selection at dispatch time** (G1 — no more `ml_selected_tracks` stale stash for the button path; the files actions read `multi_sel`'s selected rows directly, the same way `add_selected` already does), then call the helper. Each view's `status` closure writes its status label through `gtk_safe`.
- [ ] **Step 3: Status labels (G3):** the files view and active playlist already have labels. Add a small `.status-label` `Label` to the editor page's button-row area and to the device view (mirror the files view's `files_status` construction), and route their `status` closures there. Success stays quiet everywhere; only unreadable-file failures dialog.
- [ ] **Step 4: Editor button (G2):** replace the editor's `btn_send_dev` ("Send to…", whole-playlist→device popover) with a `MenuButton` "Send to ▾" configured EXACTLY like the files view's (`set_create_popup_func`, `insert_action_group("ed", …)` on the button — both lessons from this branch). Its menu = `build_send_to_menu(ed.* actions)` **plus** one extra submenu appended after: "Entire playlist to device" with one item per device invoking a new `ed.send-playlist-device` (String device-id target) action whose body is the existing `send_playlist_run` call (whole playlist, files + .m3u8 — semantics unchanged, moved verbatim from the old popover buttons). Delete the old popover block incl. its `connect_closed(unparent)`.
- [ ] **Step 5: Drag-to-drive (New #5):** in the disc sidebar row builder (`ml_discs.rs`, rows named `disc:<id>`), attach a `gtk4::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY)` per drive row. On drop: extract paths from the `FileList`, build metas via the library (same lookup the files action uses; filename fallback), call `queue_paths_to_drive` with that row's drive id/label and the discs status line as `status`. No capacity gate at drop (spec).
- [ ] **Step 6: Build + full suite** (same command/expectation as Task 1).
- [ ] **Step 7: Commit**

```bash
git commit -am "feat(gtk): one Send-to-drive path — live selection, quiet status, drag-to-drive

queue_paths_to_drive replaces four duplicated async bodies; every entry
reads the live selection at dispatch (the button acted on the last
right-clicked rows); success feedback is a quiet status line in every
view (editor + device view gain labels); empty selection says so; the
editor's whole-playlist device send lives inside the standard Send to ▾
menu; disc sidebar rows accept file drops."
```

---

### Task 3 (A): Disc-swap auto-refresh

**Files:**
- Modify: `src/disc/detect.rs` (fingerprint fn + tests), `frontends/gtk/window/ml_discs.rs` (poll wiring)

**Interfaces:**
- Produces: `pub fn media_fingerprint(d: &OpticalDrive) -> u64` — used by the GTK poll; TUI (Task 10) may reuse.

- [ ] **Step 1: Failing test** (detect.rs tests module):

```rust
    #[test]
    fn media_fingerprint_tracks_meaningful_changes() {
        let mut d = OpticalDrive {
            id: "/dev/sr0".into(), label: "T".into(),
            media: MediaInfo::none(), toc: None, mount_path: None,
        };
        let empty = media_fingerprint(&d);
        d.media.present = true;
        d.media.kind = MediaKind::CdRw;
        let blank = media_fingerprint(&d);
        assert_ne!(empty, blank, "media arriving must change the fingerprint");
        let same = media_fingerprint(&d);
        assert_eq!(blank, same, "unchanged media must be stable");
        d.media.is_blank = true;
        assert_ne!(media_fingerprint(&d), blank, "blank flag change must show");
        d.media.capacity_bytes = 700_000_000;
        let with_cap = media_fingerprint(&d);
        d.media.capacity_bytes = 4_700_000_000;
        assert_ne!(media_fingerprint(&d), with_cap, "capacity change must show");
    }
```

- [ ] **Step 2: RED** — `distrobox enter dev-box -- cargo test --lib media_fingerprint` fails (not found).
- [ ] **Step 3: Implement** (detect.rs, near the MediaInfo helpers):

```rust
/// Hash of the load-state a user can see: media kind/flags, TOC track
/// count, capacity. The GTK poll compares per-drive fingerprints across
/// ticks and refreshes an open detail view when the SHOWN drive's changes
/// (disc swapped/ejected/inserted) — unchanged drives are never disturbed.
pub fn media_fingerprint(d: &OpticalDrive) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.media.present.hash(&mut h);
    d.media.is_audio_cd.hash(&mut h);
    d.media.is_blank.hash(&mut h);
    d.media.rewritable.hash(&mut h);
    (d.media.kind as u8).hash(&mut h);
    d.media.capacity_bytes.hash(&mut h);
    d.media.free_bytes.hash(&mut h);
    d.toc.as_ref().map(|t| t.tracks.len()).unwrap_or(0).hash(&mut h);
    h.finish()
}
```

(If `MediaKind` isn't `Copy`/castable, hash `format!("{:?}", d.media.kind)` instead — check the enum.)
- [ ] **Step 4: GREEN**, then wire the poll: in `refresh_discs`'s post-detect closure (ml_discs.rs — where `detail_update` is computed), keep a `Rc<RefCell<HashMap<String, u64>>>` of previous fingerprints; when the shown drive's fingerprint differs from last tick, call `populate_detail(&drive)` (which already refreshes the burn panel) even if the existing `detail_update` equality check missed it; store new fingerprints for all drives. Use the snapshot-first borrow discipline (no borrow across `populate_detail`).
- [ ] **Step 5: Build + full suite; commit**

```bash
git commit -am "feat(gtk): auto-refresh the open drive view when its disc changes

Per-drive media fingerprints compared across poll ticks; the shown
drive repopulates on change (swap/eject/insert), others untouched."
```

---

### Task 4 (C-core): CD-TEXT — v07t sheet, defaults, per-drive metadata

**Files:**
- Create: `src/disc/cdtext.rs` · Modify: `src/disc/mod.rs` (mod line), `src/disc/burnlist.rs` (meta fields), `src/disc/burn.rs` (audio args + run_job)

**Interfaces:**
- Produces: `DiscMeta { artist: String, album: String }`, `default_disc_meta(&[BurnItem]) -> DiscMeta`, `build_v07t(&DiscMeta, &[BurnItem]) -> String`; `BurnList.meta_override: Option<DiscMeta>` + `BurnList::effective_meta(&self) -> DiscMeta`; `burn::run_job` writes the sheet and passes `input_sheet_v07t=` for audio burns. Task 5 (GTK) and Task 10 (TUI) bind the fields; Task 11 flags the mac drutil gap.

- [ ] **Step 1: Failing tests** (new file's test module):

```rust
    fn item(display: &str) -> BurnItem {
        BurnItem { path: format!("/m/{display}.mp3").into(),
                   display: display.into(), duration_secs: Some(60), bytes: 1 }
    }

    #[test]
    fn defaults_common_artist_else_various() {
        let same = [item("Foo - One"), item("Foo - Two")];
        assert_eq!(default_disc_meta(&same).artist, "Foo");
        let mixed = [item("Foo - One"), item("Bar - Two")];
        assert_eq!(default_disc_meta(&mixed).artist, "Various Artists");
        let untagged = [item("justafilename")];
        assert_eq!(default_disc_meta(&untagged).artist, "Various Artists");
        assert!(default_disc_meta(&same).album.starts_with("Sparkamp Disc 2"));
    }

    #[test]
    fn v07t_sheet_carries_album_and_tracks() {
        let meta = DiscMeta { artist: "Foo".into(), album: "My Disc".into() };
        let items = [item("Foo - One"), item("justafilename")];
        let sheet = build_v07t(&meta, &items);
        assert!(sheet.contains("Album Title = My Disc"), "{sheet}");
        assert!(sheet.contains("Performer = Foo"), "{sheet}");
        assert!(sheet.contains("Track 01 = One"), "{sheet}");
        // No " - " separator: whole display becomes the title.
        assert!(sheet.contains("Track 02 = justafilename"), "{sheet}");
    }
```

- [ ] **Step 2: RED**, then implement `src/disc/cdtext.rs`:

```rust
//! CD-TEXT for audio burns, written as a Sony v07t definition sheet that
//! cdrskin consumes via `input_sheet_v07t=<path>` (checked against
//! cdrskin 1.5.8 --help on the dev box). Titles come from the queue's
//! display lines ("Artist - Title", or the whole string when untagged),
//! matching the display logic everywhere else in the app.

use crate::disc::burnlist::BurnItem;

#[derive(Debug, Clone, PartialEq)]
pub struct DiscMeta {
    pub artist: String,
    pub album: String,
}

/// Split one queue display line into (performer, title).
fn split_display(display: &str, disc_artist: &str) -> (String, String) {
    match display.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (disc_artist.to_string(), display.trim().to_string()),
    }
}

/// Defaults: artist = the common track artist when every tagged track
/// agrees, else "Various Artists"; album = "Sparkamp Disc YYYY-MM-DD".
pub fn default_disc_meta(items: &[BurnItem]) -> DiscMeta {
    let mut artists = items.iter().filter_map(|i| {
        i.display.split_once(" - ").map(|(a, _)| a.trim().to_string())
    });
    let artist = match artists.next() {
        Some(first)
            if artists.all(|a| a == first)
                && items.iter().all(|i| i.display.contains(" - ")) =>
        {
            first
        }
        _ => "Various Artists".to_string(),
    };
    let today = chrono_free_today(); // see below — no new crate
    DiscMeta { artist, album: format!("Sparkamp Disc {today}") }
}

/// YYYY-MM-DD from the system clock without adding a date crate: seconds
/// since epoch → civil date (Howard Hinnant's algorithm).
fn chrono_free_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Sony v07t CD-TEXT definition sheet (one line per field; cdrskin's
/// `input_sheet_v07t=`). Only the fields hardware players read: album
/// title/performer + per-track title/performer.
pub fn build_v07t(meta: &DiscMeta, items: &[BurnItem]) -> String {
    let mut s = String::new();
    s.push_str("Input Sheet Version = 0.7T\n");
    s.push_str(&format!("Album Title = {}\n", meta.album));
    s.push_str(&format!("Performer = {}\n", meta.artist));
    for (i, item) in items.iter().enumerate() {
        let (performer, title) = split_display(&item.display, &meta.artist);
        s.push_str(&format!("Track {:02} = {}\n", i + 1, title));
        s.push_str(&format!("Performer {:02} = {}\n", i + 1, performer));
    }
    s
}
```

Register `pub mod cdtext;` in `src/disc/mod.rs`. **Verify the exact v07t field names** by generating one from cdrskin on the dev box (`cdrskin dev=/dev/sr0 cdtext_to_v07t=- --cdtext_dummy` needs a disc; alternatively check libburn's `doc/cdtext.txt` in the vendored sources or cdrskin's texinfo). If the real field syntax differs (e.g. `Track 01` vs `Title 01`), fix builder + tests to the documented format — the tests then pin the REAL format.
- [ ] **Step 3: BurnList fields** (burnlist.rs):

```rust
    /// Audio-burn disc metadata the user typed over the defaults; `None`
    /// means recompute defaults from the current items. Cleared with the
    /// queue after a successful burn.
    pub meta_override: Option<crate::disc::cdtext::DiscMeta>,
```

added to `BurnList` (derives stay; update `Default`), plus:

```rust
    pub fn effective_meta(&self) -> crate::disc::cdtext::DiscMeta {
        self.meta_override
            .clone()
            .unwrap_or_else(|| crate::disc::cdtext::default_disc_meta(&self.items))
    }
```

Unit test: override wins; None recomputes as items change.
- [ ] **Step 4: Burn plumbing** (burn.rs): `run_job` gains `disc_meta: Option<&DiscMeta>` (audio mode only; data ignores). In the audio arm, after WAV prep: write `build_v07t` to `<staged>/cdtext.v07t`, and `cdrskin_audio_args` gains an optional sheet path → inserts `input_sheet_v07t=<path>` before `-dao`. Builder unit test asserts the arg placement. drutil arm: ignore + doc-comment the mac gap. Update ALL `run_job` callers (GTK disc.rs `start_burn` passes `Some(&list.effective_meta())` for audio and clears `meta_override` with the queue on success; TUI passes its own — Task 10; live tests pass None or a fixed meta).
- [ ] **Step 5: Build + full suite (all callers compile); commit**

```bash
git commit -am "feat(disc): CD-TEXT on audio burns via a generated v07t sheet

Album/track titles+performers from the queue's display lines; defaults
are the common artist (else Various Artists) and 'Sparkamp Disc <date>';
per-drive override lives with the burn list and clears after a
successful burn. cdrskin consumes the sheet via input_sheet_v07t; the
drutil (mac) arm has no CD-TEXT path and is flagged."
```

---

### Task 5 (C-GTK): Editable disc artist/album in the burn panel

**Files:**
- Modify: `frontends/gtk/window/disc.rs` (burn panel)

**Interfaces:**
- Consumes: `BurnList.meta_override` / `effective_meta` (Task 4).

- [ ] **Step 1:** In `build_burn_panel`, add a two-`Entry` row ("Disc artist", "Disc album"), visible only when the shown media supports an audio burn (same condition as `btn_audio`). In `refresh_cb`: if `meta_override.is_none()`, set both entries' text to `effective_meta()` values (guard against feedback loops: only `set_text` when the text actually differs). On `connect_changed` (user edit): write `meta_override = Some(DiscMeta { artist: <artist entry>, album: <album entry> })` — short borrow, no rerender from inside the handler. `start_burn` (audio) passes `Some(&effective_meta())` into `run_job` (already wired in Task 4) and the success path's queue-clear also sets `meta_override = None`.
- [ ] **Step 2:** Build + full suite; note for the human pass: defaults live-update as the queue changes until first edit.
- [ ] **Step 3: Commit** — `feat(gtk): editable disc artist/album on the burn panel`.

---

### Task 6 (D-core): Structured BurnProgress + streaming tool runner

**Files:**
- Modify: `src/disc/burn.rs`

**Interfaces:**
- Produces: `pub struct BurnProgress { pub label: String, pub fraction: Option<f32> }`; `run_job(..., progress: impl FnMut(BurnProgress))` (replaces the `&str` phase callback); `parse_cdrskin_progress(line: &str) -> Option<f32>`; `run_tool_streaming(program, args, on_line: impl FnMut(&str) + Send) -> Result<(), String>`. Tasks 7/10/11 consume; all current `phase(...)` callers updated in this task.

- [ ] **Step 1: Failing tests:**

```rust
    #[test]
    fn cdrskin_progress_lines_parse() {
        assert_eq!(parse_cdrskin_progress("Track 01:   12 of   34 MB written"),
                   Some(12.0 / 34.0));
        assert_eq!(parse_cdrskin_progress("Track 12:  340 of  340 MB written"),
                   Some(1.0));
        assert_eq!(parse_cdrskin_progress("Thank you for using cdrskin"), None);
        assert_eq!(parse_cdrskin_progress("Track 01: 0 of 0 MB written"), None);
    }
```

(Capture a real line from a live burn log if the format differs — the test pins the real format; cdrskin's progress goes to stdout as `Track NN:  <x> of <y> MB written` with `-v`; ensure `cdrskin_audio_args`/data args include `-v` so the lines exist.)
- [ ] **Step 2: RED → implement** the parser (split on "of", strip "MB written", parse the two numbers, `None` on zero denominator), and `run_tool_streaming`: same child-spawn/watchdog/cancel skeleton as `run_tool_with_timeout` but stdout = `Stdio::piped()`, a reader thread tees every line to the log file AND to `on_line`; stderr still to the log. Existing `run_tool` becomes a thin wrapper (`on_line = |_| {}`) so erase/data paths are untouched.
- [ ] **Step 3: run_job progress:** replace `phase: impl FnMut(&str)` with `progress: impl FnMut(BurnProgress)`. Emissions: Erasing → `fraction: None`; Preparing i/N → `Some(i as f32 / n as f32)` upgraded with within-track position via `rip::run_pipeline_observed` (position secs / item.duration_secs) where duration known; Burning (audio + data on Linux) → stream via `run_tool_streaming`, fraction from `parse_cdrskin_progress` (forwarded through a channel or the callback — mind Send: the progress closure lives on the caller's thread; run_job already runs on a worker thread, so forward parsed fractions through the same `tx` the GTK worker uses). Keep the label texts identical to today's phases (TUI/mac string-match them until Task 10/11).
- [ ] **Step 4:** Update every `run_job` caller's callback shape (GTK worker `BurnMsg::Phase(String)` → `BurnMsg::Progress(BurnProgress)`; TUI equivalent; live tests print label). Build + full suite.
- [ ] **Step 5: Commit** — `feat(disc): structured burn progress with streamed cdrskin percent`.

---

### Task 7 (D-GTK): Burn progress overlay on the disc view

**Files:**
- Modify: `frontends/gtk/window/disc.rs`, `frontends/gtk/window/ml_discs.rs`

**Interfaces:**
- Consumes: `BurnProgress` (Task 6), the burn poller in `build_burn_panel`.

- [ ] **Step 1:** Wrap the disc detail content in a `gtk4::Overlay`. Overlay child (initially hidden): a centered card — phase `Label` + `ProgressBar`. Burn state lives keyed by drive id in a `Rc<RefCell<HashMap<String, BurnProgress>>>` shared between the burn poller and the detail navigation: the poller updates the entry each `BurnMsg::Progress` (determinate → `set_fraction`, indeterminate → `pulse()` on the existing 200 ms tick); `populate_disc_detail` shows the overlay iff the shown drive has an active entry (this is what makes navigate-away-and-back re-show a live burn). Entry removed (overlay hidden) on Done/Failed/Cancelled — the result text still lands in the status line as today. Cancel button stays available (place it on the overlay card too, wired to the existing cancel handler).
- [ ] **Step 2:** Erase phase: `fraction: None` → pulsing bar with "Erasing…" (the field-observed "stuck on Erasing" now visibly animates).
- [ ] **Step 3:** Build + full suite. Note interactive checks for the human: overlay shows during burn, survives navigation, pulses on erase, fills on burn %, clears on finish.
- [ ] **Step 4: Commit** — `feat(gtk): burn progress overlay with live fraction on the disc view`.

---

### Task 8 (E-core): Read-only disc mount + file listing

**Files:**
- Create: `src/disc/mount.rs` · Modify: `src/disc/mod.rs`

**Interfaces:**
- Produces: `ensure_mounted(drive: &OpticalDrive) -> Result<PathBuf, String>`, `list_disc_files(mount: &Path) -> Vec<DiscFile>` with `pub struct DiscFile { pub path: PathBuf, pub display: String, pub duration_secs: Option<u32>, pub bytes: u64 }`. Task 9 consumes.

- [ ] **Step 1:** `ensure_mounted`: resolve the drive's block device (`drive.id`, e.g. `/dev/sr0`) to a udisks2 object; read its `Filesystem.MountPoints` (reuse `devices::detect::decode_mountpoints`); if non-empty return the first; else call `Filesystem.Mount(options: {})` over zbus (read-only comes free — kernel mounts iso9660 ro) and return the path. Follow `src/devices/detect.rs`'s existing zbus connection/proxy pattern exactly — no new connection machinery. Guard the whole call with `detect::set_exclusive_read` semantics NOT being violated: mounting reads the disc, so callers must invoke it only from the same guarded contexts the poll uses (document on the fn; Task 9 wires it correctly).
- [ ] **Step 2:** `list_disc_files`: recursive walk (depth-capped at 5), audio extensions only (mp3/flac/ogg/opus/m4a/wav — grep the library scanner's extension list and reuse THAT constant if one exists), each file → tag read via the same path `devices::browse::read_device_track` uses (display "Artist - Title" fallback filename, duration if the tag/header has it, bytes from metadata). Pure given a directory → unit-test against a temp dir with a fake layout (extension filter, depth cap, display fallback; no real audio needed if `read_device_track` degrades gracefully — else create minimal fixtures the way that module's tests do).
- [ ] **Step 3:** Live test `live_disc_mount_and_list` (`#[ignore]`): with the burned data CD-RW in the drive — mounts, finds the 3 MP3s + skips playlist.m3u8, correct byte sizes.
- [ ] **Step 4:** Build + full suite; commit — `feat(disc): read-only data-disc mount + audio file listing`.

---

### Task 9 (E-GTK): Data-disc browse + play + add-to-library

**Files:**
- Modify: `frontends/gtk/window/ml_discs.rs`, `frontends/gtk/window/disc.rs`

**Interfaces:**
- Consumes: `ensure_mounted`/`list_disc_files` (Task 8), the device track view's list construction as the visual/interaction template, `add_files_to_library` (existing import path), `queue_paths_to_drive` (Task 2) for Send-to.

- [ ] **Step 1:** In `populate_disc_detail`: when media is present, NOT an audio CD, and not blank (a data disc), show a file list instead of the audio track list: off-thread (`spawn_future_local` + `spawn_blocking`, exclusive-read guard around the mount+walk) `ensure_mounted` → `list_disc_files` → populate a ColumnView modeled on the device track view (`ml_devices.rs`'s `dev_col_view` — same columns/factory approach, simplified: #, Title, Length, Size). Loading state: "Reading disc…" row/status; failure → status line with the error.
- [ ] **Step 2:** Interactions, all mirroring the device view: double-click/Enter plays the file (plain file path playback — the mount makes them ordinary files); right-click context menu = the standard Send-to submenu (actions on a `disc-files` group; send-drive via `queue_paths_to_drive` — a data disc's files CAN queue onto the other drive) plus **"Add to Library"**: copies selected files into the library music folder via the same staging `add_files_to_library` import the rip flow uses, then triggers the library rescan/refresh hooks (`notify_playlist_changed`-family — reuse whatever the rip import fires).
- [ ] **Step 3:** An "Add All to Library" button in the data-disc header row for the whole disc.
- [ ] **Step 4:** Build + full suite. Interactive notes for the human: browse, play, add-to-library on the burned CD-RW; contention: playback from disc while polling stays clean (poll already skips while a disc file plays only for cdda — mounted-file playback is a normal file read, safe).
- [ ] **Step 5: Commit** — `feat(gtk): browse, play, and import data-disc files from the disc view`.

---

### Task 10: TUI parity

**Files:**
- Modify: `frontends/tui/media_library/burn.rs`, `frontends/tui/media_library/detection.rs` (or wherever the TUI polls), `frontends/tui/ui/mod.rs`, tests in `frontends/tui/tests/`

**Interfaces:**
- Consumes: `BurnProgress`, `DiscMeta`/`effective_meta`, `media_fingerprint`.

- [ ] **Step 1:** Burn overlay renders `BurnProgress`: determinate → a text bar (`[####----] 47%`), indeterminate → a spinner char cycle; label above. (The `run_job` callback shape changed in Task 6 — the TUI's channel already updated there; this step is presentation.)
- [ ] **Step 2:** CD-TEXT fields: the burn setup overlay gains two editable lines (artist/album) prefilled from `effective_meta`, writing `meta_override` on edit; audio burn passes it (already wired in Task 6's caller update — verify).
- [ ] **Step 3:** Auto-refresh: the TUI disc poll compares `media_fingerprint` per drive and rebuilds the shown drive's entries on change.
- [ ] **Step 4:** Data-disc browsing + drag-to-drive: NOT applicable (no surface) — note in the commit body.
- [ ] **Step 5:** Tests following `frontends/tui/tests/burn.rs` patterns: progress line rendering (determinate + indeterminate), meta fields default/override. Build + full suite; commit — `feat(tui): burn progress bar, disc metadata fields, media auto-refresh`.

---

### Task 11: Mac blind mirror

**Files:**
- Modify: `frontends/SparkampMac/` (burn views, disc views, model), `src/ffi/disc.rs` + `sparkamp_bridge.h` (progress + meta + disc-files payloads as needed)

**Interfaces:**
- Consumes: everything above through the FFI. BLIND — report DONE_WITH_CONCERNS with a Mac-pass checklist appended to the task report.

- [ ] **Step 1:** FFI: burn job start gains disc-meta (artist/album) in its JSON; burn poll JSON gains `fraction: number|null`; new `sparkamp_disc_mount_list(ctx, drive_json) -> files_json` for data-disc browsing. Header updated by hand.
- [ ] **Step 2:** Swift: progress bar bound to the poll fraction (determinate/indeterminate); disc artist/album TextFields on the burn panel (defaults from a new FFI or computed Swift-side from the queue — pick whichever mirrors core `default_disc_meta` with less duplication and note it); data-disc file list view + add-to-library (import via the existing Swift import path); drag-to-drive onto the sidebar drive rows; fingerprint-based auto-refresh if the mac poll doesn't already repopulate.
- [ ] **Step 3:** CD-TEXT itself is flagged: drutil has no CD-TEXT input — the mac burn stays untitled; document in the report + a `// MAC GAP` comment.
- [ ] **Step 4:** Commit with the BLIND flag — `feat(mac): phase-2 disc UX mirror (BLIND — needs Mac xcodebuild + manual pass)`.

---

### Task 12: Gates + docs

- [ ] **Step 1:** Full gate: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | grep -c warning; cargo test 2>&1 | grep "test result"'` → `0`, all green.
- [ ] **Step 2:** Live where media allows: `live_hw_burn_audio` with CD-TEXT meta + `cdtext_to_v07t` readback assert; `live_disc_mount_and_list` on the data disc. (Needs the human to load media — coordinate.)
- [ ] **Step 3:** Update `docs/superpowers/plans/2026-06-23-optical-disc-support.md` (phase-2 landed note) and this plan's checkboxes; note the mac-pass backlog.
- [ ] **Step 4: Commit** — `docs: disc UX phase 2 landed; mac pass backlog updated`.
