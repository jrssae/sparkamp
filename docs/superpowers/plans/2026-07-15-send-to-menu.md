# "Send to" Menu + Per-Drive Burn Queues Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One consistent "Send to" surface (Active Playlist / Saved Playlist ▸ / Disc Drive ▸ / Removable Device ▸) across all views and frontends, with per-drive burn queues and probe-on-add duration reading — plus the RefCell crash fix that live testing surfaced.

**Architecture:** Core gains `BurnQueues` (drive-id-keyed `BurnList` map) and a pure `add_files` helper with injected metadata/probe functions. GTK gets one `send_to_spec` (pure, unit-tested 0/1/N visibility) + `build_send_to_menu` builder consumed by every view; sends to drives probe off-thread, sends to devices reuse the existing `copy_files_run`. TUI keys its queue by the selected drive. Mac mirrors blind via a new duration-probe FFI.

**Tech Stack:** Rust (edition 2024), GTK4 (gtk4-rs), Ratatui, Swift (blind), GStreamer (`duration_probe`), hand-maintained C FFI header.

**Spec:** `docs/superpowers/specs/2026-07-15-send-to-menu-design.md`

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'` — host builds fail.
- Zero warnings, zero failures before any completion claim.
- NEVER `git push` without a fresh explicit user instruction.
- Drive-contention rule: no fresh drive probes from UI event handlers — menu builders read the cached `current_drives` / `current_devices` state only.
- All user-visible GTK strings through `gtk_safe()` when they carry metadata/error text.
- `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` is hand-maintained — every new/changed `sparkamp_*` symbol must be added there manually.
- Swift changes are BLIND from this box — compile-checked only on a Mac; flag them in the commit body.
- Playlist/ML mutations must fire the matching refresh hooks (`notify_playlist_changed` etc.) — reuse existing handlers, don't invent new mutation paths.
- Commit style: conventional prefix, body explains WHY + verification line, `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` trailer.

---

### Task 1: Fix the RefCell double-borrow crash in the burn panel

**Files:**
- Modify: `frontends/gtk/window/disc.rs:461-469`

**Interfaces:**
- Consumes: nothing new.
- Produces: crash-free `rerender` closure; later tasks touch the same lines when re-keying the queue, so this lands first as its own commit.

Root cause (confirmed from the live backtrace, 2026-07-15): the burn-status
poller closure (`disc.rs:686`) calls `rerender` (`disc.rs:461`), whose
`if let Some(d) = shown_drive.borrow().clone()` keeps the `Ref` temporary
alive while `refresh_cb(&d)` runs; `refresh_cb` immediately does
`shown_drive.borrow_mut()` (`disc.rs:385`) → "RefCell already borrowed"
panic → SIGABRT.

- [ ] **Step 1: Apply the fix** — bind the clone to a local so the `Ref` drops before `refresh_cb` runs:

```rust
    let rerender = {
        let refresh_cb = refresh_cb.clone();
        let shown_drive = shown_drive.clone();
        move || {
            // Bind first: the borrow() Ref must drop before refresh_cb
            // re-borrows shown_drive mutably (live crash 2026-07-15).
            let drive = shown_drive.borrow().clone();
            if let Some(d) = drive {
                refresh_cb(&d);
            }
        }
    };
```

- [ ] **Step 2: Build**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -3'`
Expected: `Finished` with zero warnings.

- [ ] **Step 3: Manual verify** — run the GTK app, open ML → drive detail, queue two files (right-click → Add to Burn List), press the queue's Remove button, start a burn. No panic; the queue rows and meters redraw.

Run: `distrobox enter dev-box -- ./target/debug/sparkamp`

- [ ] **Step 4: Commit**

```bash
git add frontends/gtk/window/disc.rs
git commit -m "fix(gtk): drop shown_drive borrow before burn-panel rerender

The burn poller's rerender held the shown_drive Ref across refresh_cb,
which re-borrows it mutably — RefCell panic + abort, hit live during
the first hardware burn session. Bind the clone to a local first.

Verified: cargo build clean; GTK burn panel queue ops + burn no longer
crash."
```

---

### Task 2: Core `BurnQueues` — per-drive burn lists

**Files:**
- Modify: `src/disc/burnlist.rs`

**Interfaces:**
- Consumes: existing `BurnList` / `BurnItem`.
- Produces: `BurnQueues::queue(&mut self, drive_id: &str) -> &mut BurnList`, `BurnQueues::get(&self, drive_id: &str) -> Option<&BurnList>`, `BurnQueues::remove_gone(&mut self, live: &[&str])` — used by Tasks 5, 7, 9.

- [ ] **Step 1: Write the failing tests** (append inside `mod tests` in `src/disc/burnlist.rs`):

```rust
    #[test]
    fn queues_are_isolated_per_drive() {
        let mut q = BurnQueues::default();
        q.queue("/dev/sr0").add(item("a.mp3", Some(1), 1));
        q.queue("/dev/sr1").add(item("b.mp3", Some(2), 2));
        assert_eq!(q.get("/dev/sr0").unwrap().len(), 1);
        assert_eq!(q.get("/dev/sr1").unwrap().len(), 1);
        assert_eq!(q.get("/dev/sr0").unwrap().items[0].display, "a.mp3");
        assert!(q.get("/dev/sr2").is_none());
    }

    #[test]
    fn remove_gone_prunes_unplugged_drives() {
        let mut q = BurnQueues::default();
        q.queue("/dev/sr0").add(item("a.mp3", Some(1), 1));
        q.queue("/dev/sr1").add(item("b.mp3", Some(2), 2));
        q.remove_gone(&["/dev/sr1"]);
        assert!(q.get("/dev/sr0").is_none());
        assert_eq!(q.get("/dev/sr1").unwrap().len(), 1);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter dev-box -- cargo test --lib burnlist 2>&1 | tail -5`
Expected: FAIL — `BurnQueues` not found.

- [ ] **Step 3: Implement** (after the `BurnList` impl in `src/disc/burnlist.rs`):

```rust
/// Per-drive burn queues — each burner owns an independent list, so
/// "Send to Disc Drive → B" queues onto B only.
#[derive(Debug, Clone, Default)]
pub struct BurnQueues {
    queues: std::collections::HashMap<String, BurnList>,
}

impl BurnQueues {
    /// The queue for a drive, created empty on first use.
    pub fn queue(&mut self, drive_id: &str) -> &mut BurnList {
        self.queues.entry(drive_id.to_string()).or_default()
    }

    pub fn get(&self, drive_id: &str) -> Option<&BurnList> {
        self.queues.get(drive_id)
    }

    /// Drop queues whose drive is no longer attached.
    pub fn remove_gone(&mut self, live: &[&str]) {
        self.queues.retain(|id, _| live.contains(&id.as_str()));
    }
}
```

- [ ] **Step 4: Run tests**

Run: `distrobox enter dev-box -- cargo test --lib burnlist 2>&1 | tail -5`
Expected: PASS (all burnlist tests).

- [ ] **Step 5: Commit**

```bash
git add src/disc/burnlist.rs
git commit -m "feat(disc): per-drive burn queues (BurnQueues)

Each burner owns an independent list so multi-drive sends are
unambiguous. Unit-tested isolation + pruning."
```

---

### Task 3: Core `add_files` with probe-on-add + `AddOutcome`

**Files:**
- Modify: `src/disc/burnlist.rs`

**Interfaces:**
- Consumes: `BurnList::add`, `duration_probe::probe_duration` (production probe, injected by callers).
- Produces: `add_files(list, paths, meta, probe) -> AddOutcome`;
  `AddOutcome { added: usize, duplicate: usize, failed: Vec<PathBuf> }`,
  `AddOutcome::status_message(&self, drive_label: &str, total: usize) -> String`,
  `AddOutcome::failed_message(&self) -> Option<String>` — used by Tasks 4, 5, 7, 9.
  `meta: Fn(&Path) -> (String, Option<u32>, u64)` = (display line, known duration secs, size bytes).
  `probe: Fn(&Path) -> Option<u32>` = duration probe, `None` ⇒ unreadable.

- [ ] **Step 1: Write the failing tests** (append inside `mod tests`):

```rust
    #[test]
    fn add_files_probes_unknown_durations_and_skips_unreadable() {
        use std::path::Path;
        let mut bl = BurnList::default();
        let paths: Vec<PathBuf> =
            ["/m/known.mp3", "/m/probed.mp3", "/m/bad.mp3", "/m/known.mp3"]
                .iter().map(PathBuf::from).collect();
        let meta = |p: &Path| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let secs = (name == "known.mp3").then_some(120);
            (name, secs, 1_000u64)
        };
        let probe = |p: &Path| match p.file_name().unwrap().to_str().unwrap() {
            "probed.mp3" => Some(240),
            _ => None, // bad.mp3 is unreadable; known.mp3 never probed
        };
        let out = add_files(&mut bl, &paths, meta, probe);
        assert_eq!(out.added, 2);
        assert_eq!(out.duplicate, 1); // second known.mp3
        assert_eq!(out.failed, vec![PathBuf::from("/m/bad.mp3")]);
        assert_eq!(bl.len(), 2);
        assert_eq!(bl.total_secs(), 360); // 120 known + 240 probed
        assert!(!bl.has_unknown_durations()); // nothing unknown ever enters
    }

    #[test]
    fn add_outcome_messages() {
        let out = AddOutcome { added: 2, duplicate: 1, failed: vec![PathBuf::from("/m/x.mp3")] };
        let msg = out.status_message("Slimtype DS8A5SH", 5);
        assert!(msg.contains("Queued 2"), "{msg}");
        assert!(msg.contains("Slimtype DS8A5SH"), "{msg}");
        assert!(msg.contains("5 on the list"), "{msg}");
        assert!(msg.contains("1 already queued"), "{msg}");
        let fail = out.failed_message().unwrap();
        assert!(fail.contains("could not be read"), "{fail}");
        assert!(fail.contains("/m/x.mp3"), "{fail}");
        let clean = AddOutcome { added: 1, duplicate: 0, failed: vec![] };
        assert!(clean.failed_message().is_none());
        assert!(!clean.status_message("D", 1).contains("already queued"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter dev-box -- cargo test --lib burnlist 2>&1 | tail -5`
Expected: FAIL — `add_files` / `AddOutcome` not found.

- [ ] **Step 3: Implement** (in `src/disc/burnlist.rs`, after `BurnQueues`):

```rust
use std::path::Path;

/// Result of one batch add: what queued, what was already there, and what
/// could not be read (and therefore was NOT added — an unknown duration
/// would defeat the over-capacity gate).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AddOutcome {
    pub added: usize,
    pub duplicate: usize,
    pub failed: Vec<PathBuf>,
}

impl AddOutcome {
    /// One status line, shared wording across frontends.
    pub fn status_message(&self, drive_label: &str, total: usize) -> String {
        let mut s = format!(
            "Queued {} for burning on {drive_label} ({total} on the list)",
            self.added
        );
        if self.duplicate > 0 {
            s.push_str(&format!(" — {} already queued", self.duplicate));
        }
        s
    }

    /// Multi-line error body listing every skipped file; `None` when all
    /// files were readable.
    pub fn failed_message(&self) -> Option<String> {
        if self.failed.is_empty() {
            return None;
        }
        let mut s =
            String::from("These files could not be read and were not added:\n");
        for p in &self.failed {
            s.push_str(&format!("\n{}", p.display()));
        }
        Some(s)
    }
}

/// Queue a batch. `meta` supplies (display, known duration, bytes) from the
/// caller's library; when the duration is unknown, `probe` reads the file
/// (production: `duration_probe::probe_duration`). Probe failure ⇒ the file
/// is skipped and reported, never queued with an unknown length.
pub fn add_files(
    list: &mut BurnList,
    paths: &[PathBuf],
    meta: impl Fn(&Path) -> (String, Option<u32>, u64),
    probe: impl Fn(&Path) -> Option<u32>,
) -> AddOutcome {
    let mut out = AddOutcome::default();
    for path in paths {
        let (display, known_secs, bytes) = meta(path);
        let secs = match known_secs.or_else(|| probe(path)) {
            Some(s) => s,
            None => {
                out.failed.push(path.clone());
                continue;
            }
        };
        let added = list.add(BurnItem {
            path: path.clone(),
            display,
            duration_secs: Some(secs),
            bytes,
        });
        if added {
            out.added += 1;
        } else {
            out.duplicate += 1;
        }
    }
    out
}
```

- [ ] **Step 4: Run tests**

Run: `distrobox enter dev-box -- cargo test --lib burnlist 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/disc/burnlist.rs
git commit -m "feat(disc): probe-on-add batch queueing with failure reporting

Files with unknown duration are probed at add time; unreadable files
are skipped and listed instead of entering the queue with an unknown
length (which undercounted the over-capacity gate). Shared outcome
wording for all frontends."
```

---

### Task 4: FFI duration-probe helper for mac

**Files:**
- Modify: `src/ffi/disc.rs`
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`

**Interfaces:**
- Consumes: `duration_probe::probe_duration`.
- Produces: `sparkamp_disc_probe_durations(ctx, paths_json) -> char*` returning `[{"path":"…","secs":123|null}]` — consumed by Task 10's Swift. (Mac keeps its burn list Swift-side; only the probe crosses the FFI.)

- [ ] **Step 1: Find the existing FFI JSON/string helpers** — open `src/ffi/disc.rs`, note the `to_c_string`-style helper the other `sparkamp_disc_*` functions use for their return values, and the existing serde imports. Follow that exact pattern.

- [ ] **Step 2: Implement** (append in `src/ffi/disc.rs`, matching the file's existing `#[unsafe(no_mangle)]` style):

```rust
/// Probe durations for a JSON array of absolute paths. Returns a JSON
/// array [{"path":"…","secs":123|null}] — null ⇒ unreadable; the caller
/// must skip that file (never queue unknown lengths). Runs GStreamer
/// discovery per file: call from a background queue, not the UI thread.
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_disc_probe_durations(
    _ctx: *mut SparkampCtx,
    paths_json: *const std::os::raw::c_char,
) -> *mut std::os::raw::c_char {
    let paths: Vec<String> = unsafe { std::ffi::CStr::from_ptr(paths_json) }
        .to_str()
        .ok()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let results: Vec<serde_json::Value> = paths
        .iter()
        .map(|p| {
            let secs = crate::duration_probe::probe_duration(
                std::path::Path::new(p),
            )
            .map(|d| d.as_secs() as u32);
            serde_json::json!({ "path": p, "secs": secs })
        })
        .collect();
    to_c_string(&serde_json::Value::Array(results).to_string())
}
```

(If the file's string-return helper has a different name than `to_c_string`, use that name — copy whatever `sparkamp_disc_burn_job_poll` uses.)

- [ ] **Step 3: Header** (append in `sparkamp_bridge.h` near the other disc functions, ~line 760):

```c
/** Probe durations for a JSON array of absolute paths. Returns a JSON
    array [{"path":"…","secs":123|null}] — null ⇒ unreadable, skip the
    file. Runs GStreamer discovery per file: call off the main thread.
    Free with sparkamp_string_free. */
char *sparkamp_disc_probe_durations(SparkampCtx *ctx, const char *paths_json);
```

(Match the file's actual free-function name — check how other `char*` returns document it.)

- [ ] **Step 4: Build + test**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -3 && cargo test --lib ffi 2>&1 | tail -3'`
Expected: clean build, existing FFI tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/ffi/disc.rs frontends/SparkampMac/SparkampCore/sparkamp_bridge.h
git commit -m "feat(ffi): duration-probe batch call for mac probe-on-add

Mac keeps its burn list Swift-side; only the probe crosses the FFI.
Header updated by hand as usual. Swift adoption is blind — needs a Mac
xcodebuild pass."
```

---

### Task 5: GTK — re-key the burn panel to per-drive queues

**Files:**
- Modify: `frontends/gtk/window/media_library.rs:347` (queue state), `:2590` (panel call), `:2696-2745` (add action — becomes Task 7's send action, here it just re-keys)
- Modify: `frontends/gtk/window/disc.rs:274-380` (panel signature + refresh), all `burn_list.borrow` sites inside the panel (queue buttons `:470-520`, `start_burn` `:536+`)

**Interfaces:**
- Consumes: `BurnQueues` (Task 2).
- Produces: `build_burn_panel(state, burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>>, refresh_discs_holder, win) -> BurnUi`; the panel reads/writes ONLY `queues.queue(&drive.id)` for the currently shown drive. Task 7 queues into the same `Rc`.

- [ ] **Step 1: Swap the shared state** (`media_library.rs:347`):

```rust
    let burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>> =
        Rc::new(RefCell::new(Default::default()));
```

Rename every use of the old `burn_list` Rc in `media_library.rs` to `burn_queues` (the `:2590` panel call and the `:2696` add action; the add action interim body becomes `burn_queues.borrow_mut().queue(<target drive id>)` — Task 7 replaces it wholesale, so here just make it compile against the shown drive the disc detail selected, using the same drive id the panel receives).

- [ ] **Step 2: Re-key the panel** (`disc.rs`): change the parameter to `burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>>`; inside `refresh_cb`, the queue buttons, and `start_burn`, replace `burn_list.borrow()` / `borrow_mut()` with the shown drive's list:

```rust
            // refresh_cb: render the SHOWN drive's queue only.
            let mut queues = burn_queues.borrow_mut();
            let list = queues.queue(&drive.id);
```

```rust
            // queue buttons (remove/up/down/clear): resolve the drive first,
            // then operate on its list. Example (remove):
            let drive_id = shown_drive.borrow().as_ref().map(|d| d.id.clone());
            if let (Some(id), Some(i)) = (drive_id, selected_idx()) {
                burn_queues.borrow_mut().queue(&id).remove(i);
                rerender();
            }
```

```rust
            // start_burn: snapshot the shown drive's items.
            let items = burn_queues.borrow_mut().queue(&drive.id).items.clone();
            if items.is_empty() { return; }
```

(Keep every borrow SHORT-LIVED — resolve + drop before calling anything that redraws; that is what Task 1 fixed.)

- [ ] **Step 3: Prune on poll** — in `media_library.rs`, where the disc poll refreshes `current_drives` (search `current_drives.borrow_mut()` inside the poll tick), prune dead queues:

```rust
            {
                let drives = current_drives.borrow();
                let live: Vec<&str> = drives.iter().map(|d| d.id.as_str()).collect();
                burn_queues.borrow_mut().remove_gone(&live);
            }
```

- [ ] **Step 4: Build + full test**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -3 && cargo test 2>&1 | grep "test result"'`
Expected: clean, all pass.

- [ ] **Step 5: Manual verify** — run the app with both drives attached: queue files onto sr0's panel, open sr1's detail → its queue is empty; unplug sr1 → its queue disappears from state (re-plug shows empty).

- [ ] **Step 6: Commit**

```bash
git add frontends/gtk/window/media_library.rs frontends/gtk/window/disc.rs
git commit -m "feat(gtk): burn panel binds to per-drive queues

Each drive detail now shows only its own burn list; queues of
unplugged drives are pruned on the poll tick. Verified live with two
attached burners."
```

---

### Task 6: GTK — `send_to_spec` (pure) + `build_send_to_menu`

**Files:**
- Modify: `frontends/gtk/window/util.rs` (next to `build_add_to_playlist_submenu`, `:533`)

**Interfaces:**
- Consumes: `build_add_to_playlist_submenu(state, new_action, append_action)` (existing, util.rs:538).
- Produces:
  - `pub(super) enum SendEntry { ActivePlaylist, SavedPlaylist, DriveDirect(String, String), DriveMenu(Vec<(String, String)>), DeviceDirect(String, String), DeviceMenu(Vec<(String, String)>) }` (`(id, label)` pairs)
  - `pub(super) fn send_to_spec(drives: &[(String, String)], devices: &[(String, String)]) -> Vec<SendEntry>`
  - `pub(super) fn build_send_to_menu(state, actions: &SendToActions) -> gio::Menu` where `pub(super) struct SendToActions<'a> { pub active: &'a str, pub new_playlist: &'a str, pub saved_playlist: &'a str, pub drive: &'a str, pub device: &'a str, pub drives: Vec<(String, String)>, pub devices: Vec<(String, String)> }` (action names carry the consumer's group prefix, e.g. `"ml.send-active"`; `drive`/`device` actions take a string target = the id).
  Used by Tasks 7 and 8.

- [ ] **Step 1: Write the failing tests** (in util.rs's test module, or create one following the file's existing pattern):

```rust
    #[test]
    fn send_to_spec_visibility_matrix() {
        let d1 = vec![("sr0".to_string(), "Drive A".to_string())];
        let d2 = vec![
            ("sr0".to_string(), "Drive A".to_string()),
            ("sr1".to_string(), "Drive B".to_string()),
        ];
        // 0 drives, 0 devices: playlist entries only.
        let spec = send_to_spec(&[], &[]);
        assert_eq!(spec, vec![SendEntry::ActivePlaylist, SendEntry::SavedPlaylist]);
        // 1 drive: direct item, no submenu.
        let spec = send_to_spec(&d1, &[]);
        assert!(spec.contains(&SendEntry::DriveDirect("sr0".into(), "Drive A".into())));
        // 2 drives: submenu with both.
        let spec = send_to_spec(&d2, &[]);
        assert!(spec.contains(&SendEntry::DriveMenu(d2.clone())));
        // devices mirror the same rule.
        let dev = vec![("usb1".to_string(), "Stick".to_string())];
        let spec = send_to_spec(&[], &dev);
        assert!(spec.contains(&SendEntry::DeviceDirect("usb1".into(), "Stick".into())));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter dev-box -- cargo test --lib send_to_spec 2>&1 | tail -4`
Expected: FAIL — not found.

- [ ] **Step 3: Implement spec fn**:

```rust
/// What the "Send to" menu shows, as data — pure so the 0/1/N visibility
/// rules are unit-testable without GTK.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SendEntry {
    ActivePlaylist,
    SavedPlaylist,
    /// One drive attached: direct item (id, label), no submenu.
    DriveDirect(String, String),
    /// Multiple drives: submenu, one item per (id, label).
    DriveMenu(Vec<(String, String)>),
    DeviceDirect(String, String),
    DeviceMenu(Vec<(String, String)>),
}

pub(super) fn send_to_spec(
    drives: &[(String, String)],
    devices: &[(String, String)],
) -> Vec<SendEntry> {
    let mut out = vec![SendEntry::ActivePlaylist, SendEntry::SavedPlaylist];
    match drives {
        [] => {}
        [(id, label)] => out.push(SendEntry::DriveDirect(id.clone(), label.clone())),
        many => out.push(SendEntry::DriveMenu(many.to_vec())),
    }
    match devices {
        [] => {}
        [(id, label)] => out.push(SendEntry::DeviceDirect(id.clone(), label.clone())),
        many => out.push(SendEntry::DeviceMenu(many.to_vec())),
    }
    out
}
```

- [ ] **Step 4: Implement the GTK builder** (below the spec fn):

```rust
/// Action names for one consumer of the Send-to menu; each consumer
/// registers its own action group and passes prefixed names here.
pub(super) struct SendToActions<'a> {
    pub active: &'a str,         // e.g. "ml.send-active" (no target)
    pub new_playlist: &'a str,   // e.g. "ml.add-to-new" (no target)
    pub saved_playlist: &'a str, // e.g. "ml.add-to-saved" (i64 playlist id)
    pub drive: &'a str,          // e.g. "ml.send-drive" (String drive id)
    pub device: &'a str,         // e.g. "ml.send-device" (String device id)
    pub drives: Vec<(String, String)>,
    pub devices: Vec<(String, String)>,
}

/// Build the full "Send to" menu: Active Playlist / Saved Playlist ▸ /
/// Disc Drive [▸] / Removable Device [▸]. Drive + device lists must come
/// from the cached poll state — never probe from a menu handler.
pub(super) fn build_send_to_menu(
    state: &std::rc::Rc<std::cell::RefCell<AppState>>,
    actions: &SendToActions<'_>,
) -> gio::Menu {
    let menu = gio::Menu::new();
    for entry in send_to_spec(&actions.drives, &actions.devices) {
        match entry {
            SendEntry::ActivePlaylist => {
                menu.append_item(&gio::MenuItem::new(
                    Some("Active Playlist"),
                    Some(actions.active),
                ));
            }
            SendEntry::SavedPlaylist => {
                let sub = build_add_to_playlist_submenu(
                    state,
                    actions.new_playlist,
                    actions.saved_playlist,
                );
                menu.append_submenu(Some("Saved Playlist"), &sub);
            }
            SendEntry::DriveDirect(id, _label) => {
                let item = gio::MenuItem::new(Some("Disc Drive"), None);
                item.set_action_and_target_value(
                    Some(actions.drive),
                    Some(&id.to_variant()),
                );
                menu.append_item(&item);
            }
            SendEntry::DriveMenu(drives) => {
                let sub = gio::Menu::new();
                for (id, label) in drives {
                    let item = gio::MenuItem::new(Some(&label), None);
                    item.set_action_and_target_value(
                        Some(actions.drive),
                        Some(&id.to_variant()),
                    );
                    sub.append_item(&item);
                }
                menu.append_submenu(Some("Disc Drive"), &sub);
            }
            SendEntry::DeviceDirect(id, _label) => {
                let item = gio::MenuItem::new(Some("Removable Device"), None);
                item.set_action_and_target_value(
                    Some(actions.device),
                    Some(&id.to_variant()),
                );
                menu.append_item(&item);
            }
            SendEntry::DeviceMenu(devices) => {
                let sub = gio::Menu::new();
                for (id, label) in devices {
                    let item = gio::MenuItem::new(Some(&label), None);
                    item.set_action_and_target_value(
                        Some(actions.device),
                        Some(&id.to_variant()),
                    );
                    sub.append_item(&item);
                }
                menu.append_submenu(Some("Removable Device"), &sub);
            }
        }
    }
    menu
}
```

- [ ] **Step 5: Run tests + build**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -3 && cargo test --lib send_to_spec 2>&1 | tail -4'`
Expected: clean build (an `#[allow(dead_code)]` may be needed until Task 7 consumes it — remove it in Task 7), tests PASS.

- [ ] **Step 6: Commit**

```bash
git add frontends/gtk/window/util.rs
git commit -m "feat(gtk): shared Send-to menu builder with 0/1/N visibility

Pure spec fn unit-tests the hidden/direct/submenu rules; the GTK
builder consumes it. Drive/device lists read cached poll state only
(drive-contention rule)."
```

---

### Task 7: GTK — files view adopts "Send to" (button + context menu, send actions)

**Files:**
- Modify: `frontends/gtk/window/media_library.rs` — context menu `:3250-3290`, button row `:3563`, add-to-burn action `:2696-2745` (replaced), device copy runner `:1806` (reused), drive/device caches `:32`, `:342`
- Modify: `frontends/gtk/window/util.rs` (one new dialog helper)

**Interfaces:**
- Consumes: `build_send_to_menu` + `SendToActions` (Task 6), `burn_queues` (Task 5), `add_files`/`AddOutcome` (Task 3), `duration_probe::probe_duration`, `copy_files_run: Rc<dyn Fn(Device, Vec<PathBuf>)>` (existing, `:1806`), `current_drives` (`:342`), `current_devices` (`:32`).
- Produces: actions `ml.send-active`, `ml.send-drive` (String target), `ml.send-device` (String target); `show_unreadable_dialog(win, body)` helper in util.rs (used again by Task 8). Existing `ml.add-to-new` / `ml.add-to-saved` actions are reused as-is.

- [ ] **Step 1: Dialog helper** (util.rs, near the existing AlertDialog helper):

```rust
/// Modal listing files that could not be read (and were not queued).
pub(super) fn show_unreadable_dialog(win: &gtk4::Window, body: &str) {
    let dlg = gtk4::AlertDialog::builder()
        .message("Some files could not be read")
        .detail(gtk_safe(body))
        .modal(true)
        .build();
    dlg.show(Some(win));
}
```

- [ ] **Step 2: `ml.send-drive` action** — replace the whole `add-to-burn` action block (`:2696-2745`) with a targeted action. Meta lookup happens on the main thread (SQLite is not Send); probing runs in `spawn_blocking`; results land in an idle callback:

```rust
        // Send to Disc Drive: probe-on-add, then queue onto THAT drive.
        {
            let state_burn = state.clone();
            let tracks_src = ml_selected_tracks.clone();
            let burn_queues = burn_queues.clone();
            let current_drives = current_drives.clone();
            let status = files_status_holder.clone();
            let win_wk = ml_win.downgrade();
            let action = gio::SimpleAction::new(
                "send-drive",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(drive_id) =
                    target.and_then(|v| v.get::<String>()) else { return };
                let drive_label = current_drives
                    .borrow()
                    .iter()
                    .find(|d| d.id == drive_id)
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| drive_id.clone());
                let paths: Vec<_> = tracks_src.borrow().clone();
                if paths.is_empty() {
                    return;
                }
                // Metadata from the library NOW (SQLite is not Send).
                let metas: std::collections::HashMap<_, _> = {
                    let s = state_burn.borrow();
                    paths.iter().map(|path| {
                        let row = s.media_lib.as_ref().and_then(|l| {
                            l.track_by_path(&path.display().to_string()).ok()
                        });
                        let display = row.as_ref()
                            .map(|t| match (&t.artist, &t.title) {
                                (Some(a), Some(ti)) if !a.is_empty() =>
                                    format!("{a} - {ti}"),
                                (_, Some(ti)) => ti.clone(),
                                _ => t.filename.clone(),
                            })
                            .unwrap_or_else(|| path.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string()));
                        let secs = row.as_ref()
                            .and_then(|t| t.length_secs).map(|s| s as u32);
                        let bytes = std::fs::metadata(path)
                            .map(|m| m.len()).unwrap_or(0);
                        (path.clone(), (display, secs, bytes))
                    }).collect()
                };
                if let Some(lbl) = status.borrow().as_ref() {
                    lbl.set_text("Reading files…");
                }
                let burn_queues = burn_queues.clone();
                let status = status.clone();
                let win_wk = win_wk.clone();
                // Probe off-thread (GStreamer discovery can take seconds),
                // then queue on the main loop — the codebase's established
                // spawn_future_local + spawn_blocking(...).await pattern
                // (e.g. media_library.rs:1072). Only Send data (paths,
                // metas) crosses into spawn_blocking; the Rcs stay in the
                // local future.
                glib::spawn_future_local(async move {
                    let probe_metas: Vec<(std::path::PathBuf, Option<u32>)> =
                        paths.iter()
                            .map(|p| (p.clone(), metas.get(p).and_then(|m| m.1)))
                            .collect();
                    let probed: Vec<(std::path::PathBuf, Option<u32>)> =
                        gio::spawn_blocking(move || {
                            probe_metas
                                .into_iter()
                                .map(|(p, known)| {
                                    let secs = known.or_else(|| {
                                        crate::duration_probe::probe_duration(&p)
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
                            |p| probed.iter()
                                .find(|(pp, _)| pp == p)
                                .and_then(|(_, s)| *s),
                        );
                        total = list.len();
                    } // queues borrow drops before any UI call
                    if let Some(lbl) = status.borrow().as_ref() {
                        lbl.set_text(&gtk_safe(
                            &out.status_message(&drive_label, total),
                        ));
                    }
                    if let (Some(body), Some(win)) =
                        (out.failed_message(), win_wk.upgrade())
                    {
                        show_unreadable_dialog(&win, &body);
                    }
                });
            });
            ml_actions.add_action(&action);
        }
```

(Anchor names — `ml_selected_tracks`, `files_status_holder`, `ml_actions`, `ml_win` — are the ones the replaced `add-to-burn` block and its neighbors already use; keep whatever the surrounding code calls them. `add_files`'s meta closure gets pre-probed values via the `probed` lookup so the probe itself never runs on the main thread.)

- [ ] **Step 3: `ml.send-device` action** — same action-with-string-target pattern; body resolves the device and hands off to the existing runner:

```rust
        {
            let current_devices = current_devices.clone();
            let tracks_src = ml_selected_tracks.clone();
            let copy_files_run = copy_files_run.clone();
            let action = gio::SimpleAction::new(
                "send-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) =
                    target.and_then(|v| v.get::<String>()) else { return };
                let dev = current_devices
                    .borrow()
                    .iter()
                    .find(|d| d.id == dev_id)
                    .cloned();
                let paths: Vec<_> = tracks_src.borrow().clone();
                if let (Some(dev), false) = (dev, paths.is_empty()) {
                    copy_files_run(dev, paths);
                }
            });
            ml_actions.add_action(&action);
        }
```

(Check `Device`'s id field name — if it isn't `id`, use the actual unique field (mount path or serial) consistently here and in the menu-building step. `copy_files_run` already reports progress in the files status line.)

- [ ] **Step 4: `ml.send-active` action** — register a plain `SimpleAction::new("send-active", None)` whose body is the existing "Add to Playlist" button handler's logic (`:3563`'s click handler — move it into the action, have both call one closure).

- [ ] **Step 5: Context menu** (`:3250-3290`): delete the `"Add to Burn List"` item and the `menu.append_submenu(Some("Add to Playlist"), …)` lines; append instead:

```rust
                        let send = build_send_to_menu(
                            &state_for_gest,
                            &SendToActions {
                                active: "ml.send-active",
                                new_playlist: "ml.add-to-new",
                                saved_playlist: "ml.add-to-saved",
                                drive: "ml.send-drive",
                                device: "ml.send-device",
                                drives: current_drives.borrow().iter()
                                    .map(|d| (d.id.clone(), d.label.clone()))
                                    .collect(),
                                devices: current_devices.borrow().iter()
                                    .map(|d| (d.id.clone(), d.label.clone()))
                                    .collect(),
                            },
                        );
                        menu.append_submenu(Some("Send to"), &send);
```

(The gesture closure needs `current_drives` + `current_devices` clones added to its capture list. `Device`'s label field: use whatever the device overview rows display.)

- [ ] **Step 6: Button row** (`:3563`): replace the `Button::with_label("▶ Add to Playlist")` with a `MenuButton` whose model is the same builder output, built fresh on click so drive/device lists are current:

```rust
        let btn_send_to = gtk4::MenuButton::builder()
            .label("Send to ▾")
            .build();
        btn_send_to.add_css_class("pl-btn");
        {
            let state_menu = state.clone();
            let current_drives = current_drives.clone();
            let current_devices = current_devices.clone();
            let btn = btn_send_to.clone();
            btn_send_to.connect_activate(move |_| {
                // Rebuild on open: drives/devices may have come or gone.
                let menu = build_send_to_menu(
                    &state_menu,
                    &SendToActions {
                        active: "ml.send-active",
                        new_playlist: "ml.add-to-new",
                        saved_playlist: "ml.add-to-saved",
                        drive: "ml.send-drive",
                        device: "ml.send-device",
                        drives: current_drives.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        devices: current_devices.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    },
                );
                btn.set_menu_model(Some(&menu));
            });
        }
```

(If `connect_activate` doesn't fire before the popover opens on this GTK4 version, set the model from the poll tick instead — rebuild whenever `current_drives`/`current_devices` change. Verify behavior live in Step 8.)

- [ ] **Step 7: Build + full test**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -3 && cargo test 2>&1 | grep "test result"'`
Expected: clean, all pass.

- [ ] **Step 8: Manual verify** — both drives attached + a USB stick: files view right-click → Send to ▸ shows Active Playlist / Saved Playlist ▸ / Disc Drive ▸ (two entries) / Removable Device; send 3 files to Drive A → "Reading files…" then "Queued 3 for burning on <label>"; send an unscanned file → still queues with probed duration; send a non-audio file (e.g. touch a `.mp3` that is empty) → dialog lists it, not queued; Send to ▸ Removable Device copies with progress.

- [ ] **Step 9: Commit**

```bash
git add frontends/gtk/window/media_library.rs frontends/gtk/window/util.rs
git commit -m "feat(gtk): files view Send-to menu with probe-on-add and device copy

Replaces Add to Burn List + the Add to Playlist button. Duration
probing runs off-thread; unreadable files are skipped and listed;
device sends reuse the existing copy runner. Verified live with two
drives + a USB device."
```

---

### Task 8: GTK — remaining consumers (active playlist, playlist editor, device detail)

**Files:**
- Modify: `frontends/gtk/window/player.rs:1940-1990` (active playlist context menu)
- Modify: `frontends/gtk/window/media_library.rs:4650-4800` (playlist editor popover + its Add to Playlist button)
- Modify: `frontends/gtk/window/media_library.rs` device detail view (search `DeviceDetailView`-equivalent section: the view built around `copy_files_run` / `sync_run_holder`, `:6872` region)

**Interfaces:**
- Consumes: `build_send_to_menu`, `SendToActions`, `show_unreadable_dialog` (Tasks 6-7), the send actions pattern from Task 7.
- Produces: per-view action groups `pl.send-*` (player), `ed.send-*` (editor), `dev.send-*` (device view) with the same five action names; every view's selection feeds the same probe/queue/copy flow.

- [ ] **Step 1: Active playlist** (player.rs:1951): replace `menu.append_submenu(Some("Add to Playlist"), &submenu)` with the Send-to submenu (actions `pl.send-active` is meaningless here — the tracks are already in the active playlist; OMIT `ActivePlaylist` for this consumer by passing the existing `pl.*` actions and filtering):

Add a variant knob to the builder call: pass `active: ""` and skip empty-named actions in `build_send_to_menu`:

```rust
            SendEntry::ActivePlaylist => {
                if !actions.active.is_empty() {
                    menu.append_item(&gio::MenuItem::new(
                        Some("Active Playlist"),
                        Some(actions.active),
                    ));
                }
            }
```

Then in player.rs:

```rust
            let send = build_send_to_menu(
                &state_menu_pl,
                &SendToActions {
                    active: "", // tracks are already in the active playlist
                    new_playlist: "pl.add-to-new",
                    saved_playlist: "pl.add-to-saved",
                    drive: "pl.send-drive",
                    device: "pl.send-device",
                    drives: /* current_drives clone, as Task 7 */,
                    devices: /* current_devices clone, as Task 7 */,
                },
            );
            menu.append_submenu(Some("Send to"), &send);
```

Register `pl.send-drive` / `pl.send-device` on the player window's action group with the SAME bodies as Task 7 Steps 2-3, sourcing paths from the playlist selection (the same selection the existing `pl.add-to-saved` action reads). The player window doesn't have `current_drives`/`current_devices` — pass those two `Rc`s into `build_player_context` (or whatever fn owns this menu; follow how `state_menu_pl` got there).

- [ ] **Step 2: Playlist editor** (media_library.rs:4650-4800): in the pick popover, replace the "Add to Playlist" `add_btn` with a "Send to…" button that pops the same menu (`ed.*` actions, selection = `pick_idxs()` mapped to paths); register `ed.send-*` actions once on the editor's action group with Task 7's bodies.

- [ ] **Step 3: Device detail view** (media_library.rs `:6872` region): the device file list's context menu (or row popover — match how the view exposes per-file actions today) gains the same "Send to" submenu with `dev.*` actions. Device-to-device send: pass the CURRENT device's id in the `devices` vec too — sending to the same device is a no-op copy (skip-if-present) and not worth special-casing.

- [ ] **Step 4: Build + full test**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -3 && cargo test 2>&1 | grep "test result"'`
Expected: clean, all pass.

- [ ] **Step 5: Manual verify** — from each of the three views: Send to ▸ Disc Drive queues onto the chosen drive (check the drive detail), Saved Playlist ▸ New Playlist… still works, device send copies. Active playlist's menu shows NO "Active Playlist" entry.

- [ ] **Step 6: Commit**

```bash
git add frontends/gtk/window/player.rs frontends/gtk/window/media_library.rs frontends/gtk/window/util.rs
git commit -m "feat(gtk): Send-to menu in active playlist, editor, and device views

Same builder + action bodies everywhere; active playlist omits the
self-referential Active Playlist entry. Verified live from all three
views."
```

---

### Task 9: TUI — per-drive queue + probe-on-add

**Files:**
- Modify: `frontends/tui/media_library/burn.rs` (all `self.burn_list` sites: `:12`, `:28`, `:47`, `:99-120`, `:165`)
- Modify: the struct that declares `burn_list` (grep `burn_list:` in `frontends/tui/media_library/`) and the drive-selection state it already keeps
- Test: `frontends/tui/tests/` (follow the existing TUI test layout)

**Interfaces:**
- Consumes: `BurnQueues`, `add_files` (Tasks 2-3), `duration_probe::probe_duration`, the TUI's existing selected-drive id.
- Produces: `fn selected_burn_list(&mut self) -> &mut BurnList` internal helper; `b` queues onto the SELECTED drive's list with probe-on-add + failure reporting in the status line.

- [ ] **Step 1: Re-key state** — replace `burn_list: BurnList` with `burn_queues: crate::disc::burnlist::BurnQueues` in the ML struct; add:

```rust
    /// The burn queue of the drive the disc view has selected.
    fn selected_burn_list(&mut self) -> &mut crate::disc::burnlist::BurnList {
        let id = self.selected_drive_id().unwrap_or_default();
        self.burn_queues.queue(&id)
    }
```

(`selected_drive_id()` — use whatever field the TUI disc view stores its selected drive in; grep `selected_drive` in `frontends/tui/media_library/`. The TUI disc view is already per-drive, so no picker prompt is needed: `b` targets the shown drive — capability parity with GTK's per-drive panels.)

- [ ] **Step 2: Probe-on-add** — rewrite `add_selected_ml_track_to_burn_list` (`burn.rs:12`) around `add_files` with the real probe:

```rust
    pub(super) fn add_selected_ml_track_to_burn_list(&mut self) {
        let Some(track) = self.selected_ml_track() else { return };
        let path = std::path::PathBuf::from(&track.path);
        let display = /* keep the existing display-line construction */;
        let known = track.length_secs.map(|s| s as u32);
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let label = self.selected_drive_label().unwrap_or_default();
        let list = self.selected_burn_list();
        let out = crate::disc::burnlist::add_files(
            list,
            &[path],
            |_| (display.clone(), known, bytes),
            |p| crate::duration_probe::probe_duration(p)
                .map(|d| d.as_secs() as u32),
        );
        let total = list.len();
        self.status = out
            .failed_message()
            .unwrap_or_else(|| out.status_message(&label, total));
    }
```

(Keep the existing fn's selected-track lookup and display construction — only the add path changes. The single-file probe blocks the TUI briefly; acceptable in a terminal flow, note it in the fn doc. Adapt names — `selected_ml_track`, `self.status` — to what `burn.rs:12-40` actually uses.)

- [ ] **Step 3: Re-key remaining sites** — every other `self.burn_list.` in `burn.rs` (`:47`, `:99-120`, `:165`) becomes `self.selected_burn_list().` (or a one-shot local `let list = self.selected_burn_list();` where multiple calls follow).

- [ ] **Step 4: Test** — add a TUI test following the existing pattern in `frontends/tui/tests/` asserting: queueing on drive A then switching the selection to drive B shows an empty burn overlay, and switching back shows A's item. (Use the existing TUI test harness's fake-drive setup — grep `burn` in `frontends/tui/tests/` for the current burn-overlay tests and extend them.)

- [ ] **Step 5: Build + full test**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | tail -3 && cargo test 2>&1 | grep "test result"'`
Expected: clean, all pass.

- [ ] **Step 6: Commit**

```bash
git add frontends/tui src/
git commit -m "feat(tui): per-drive burn queues + probe-on-add

The b key queues onto the selected drive's own list; unknown durations
are probed at add time and unreadable files reported, never queued."
```

---

### Task 10: Mac — blind Swift adoption

**Files:**
- Modify: `frontends/SparkampMac/` — the burn-list model object (grep `burnList` in the Swift sources), `DiscDriveView.swift`, `MLPlaylistEditor.swift`, `DeviceDetailView.swift`, the files-list view (grep the "Add to Burn List" string)

**Interfaces:**
- Consumes: `sparkamp_disc_probe_durations` (Task 4).
- Produces: Swift `burnQueues: [String: [BurnRow]]` keyed by drive id; a `SendToMenu` SwiftUI menu builder mirroring the GTK structure. ALL BLIND — needs Mac xcodebuild + manual pass.

- [ ] **Step 1: Per-drive dict** — replace the single Swift burn-list array with `@Published var burnQueues: [String: [BurnRow]] = [:]`; every read becomes `burnQueues[driveId, default: []]`, writes go through a `queue(for:)` accessor. Burn panel binds to the shown drive's entry.

- [ ] **Step 2: Probe on add** — before appending rows, call the FFI probe off-main:

```swift
    func probeDurations(paths: [String],
                        completion: @escaping ([String: UInt32?]) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let json = try? JSONEncoder().encode(paths)
            let arg = String(data: json ?? Data("[]".utf8), encoding: .utf8)!
            guard let raw = sparkamp_disc_probe_durations(self.ctx, arg) else {
                DispatchQueue.main.async { completion([:]) }
                return
            }
            defer { sparkamp_string_free(raw) }
            struct Probe: Decodable { let path: String; let secs: UInt32? }
            let probes = (try? JSONDecoder().decode(
                [Probe].self, from: Data(String(cString: raw).utf8))) ?? []
            let map = Dictionary(uniqueKeysWithValues:
                probes.map { ($0.path, $0.secs) })
            DispatchQueue.main.async { completion(map) }
        }
    }
```

Unreadable (`secs == nil` and no library duration) → collect, show one alert listing the paths, skip those rows. (Match the real free-function name from the header.)

- [ ] **Step 3: SendTo menu** — one SwiftUI `SendToMenu` view (Menu { … }) with Active Playlist / Saved Playlist ▸ (New Playlist… first — reuse the existing add-to-playlist submenu source) / Disc Drive [▸ when >1] / Removable Device [▸ when >1], hidden when absent — same 0/1/N rules as `send_to_spec`. Adopt in the four views; remove the old "Add to Burn List" item and "Add to Playlist" button equivalents.

- [ ] **Step 4: Build check is NOT possible here.** Do not claim success. Commit with the blind flag:

```bash
git add frontends/SparkampMac
git commit -m "feat(mac): Send-to menu + per-drive burn queues (BLIND)

Mirrors the GTK Send-to structure; probe-on-add via the new
sparkamp_disc_probe_durations FFI. Written blind on the Linux box —
REQUIRES a Mac xcodebuild + manual pass before release."
```

---

### Task 11: Gates + docs

**Files:**
- Modify: `docs/superpowers/plans/2026-06-23-optical-disc-support.md` (hardware-test section)
- Modify: this plan's checkboxes

**Interfaces:** none — verification and bookkeeping.

- [ ] **Step 1: Full gate**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | grep -c warning; cargo test 2>&1 | grep "test result"'`
Expected: `0` warnings, all suites pass.

- [ ] **Step 2: Live hardware re-check** — with the CD-RW loaded: `cargo test --lib live_hw_erase -- --ignored --nocapture` (erase whatever the matrix left), then queue + burn from the GTK UI once end-to-end on the per-drive queue.

- [ ] **Step 3: Update the optical plan doc** — under its Phase 5/6 hardware-test lists, note: 2026-07-15 Slimtype DS8A5SH pass (audio burn 82 s / erase 30 s / data burn 59 s, md5-verified mount), the minfo-merge fix, the RefCell crash fix, and the Send-to redesign pointer to this plan + spec.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/plans/
git commit -m "docs: record first successful burn hardware pass + Send-to follow-up"
```
