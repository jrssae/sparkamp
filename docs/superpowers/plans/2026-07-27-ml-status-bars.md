# Media Library Status Bars (GTK + mac) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Give the four Media Library views (Files, Playlists, Devices, Discs) the same bottom status bar the active playlist has — `N tracks · MM:SS total · MM:SS selected` — on GTK and macOS, so all list views feel consistent. (The active-playlist 1px spacing tweaks are already done in commit `<spacing>`; this plan is only the ML status bars.)

**Architecture:** Reuse the existing core formatter `crate::playlist_status::playlist_status_line(count, total_secs, selected_secs: Option<u64>)`. On GTK, a single shared helper `ml_status_bar(selection) -> (Label, Rc<dyn Fn()>)` builds the label and a refresh closure from any `MultiSelection` over `BoxedAnyObject<LibTrack>`, wired to selection + model changes; the four views each append the label to their page container. On macOS, a shared `MLStatusBar` view (or a reused `formatStatus` + `Text`) renders the same line under each ML table.

**Tech Stack:** Rust core (`src/playlist_status.rs`, done), GTK4 (`frontends/gtk/window/media_library.rs`), macOS SwiftUI (`frontends/SparkampMac/Sources/`).

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. GTK compiles only in the bin target; never gate on `cargo build --lib`.
- Zero warnings, zero failures before any "done". Grep with `grep -E "warning:|error\[|error:"`. KNOWN pre-existing flaky test `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow` (parallel-only global-refcount race) is NOT a finding; confirm with `cargo test -- --test-threads=1` if only it fails.
- The status line MUST be produced by `crate::playlist_status::playlist_status_line` (GTK) / a byte-for-byte mirror `PlaylistView.formatStatus` already in the mac code (macOS) — identical format to the active playlist: `N tracks · MM:SS total · MM:SS selected`, singular "1 track", `M:SS` under an hour / `H:MM:SS` at/above, selected clause only when ≥1 selected.
- GTK strings reaching a widget pass through `gtk_safe()` where they carry metadata (the status line is app-generated numbers/text — no metadata — so no gtk_safe needed, but keep the rule in mind).
- macOS is BLIND here (no Swift compiler): read whole files before editing; mechanically simple changes; verification items appended to `docs/mac-pass-checklist.md` in the same commit.
- Comments: why, not what. Conventional commits, `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` trailer. Branch `album-art-improvements`; do NOT push.

## Anchors (re-verify — media_library.rs is ~10k lines, `include!`d into mod.rs; line numbers drift)

- `frontends/gtk/window/media_library.rs`: a `Stack` with named pages appended at: `files_vbox` → "files" (~5364), `dev_page` → "devices" (~3537), `disc_page` → "discs" (~3538), `pl_vbox` → "playlists" (~8278). Each page is a vertical `GtkBox` you append the status label to (append at the bottom of that page's box).
- Per-view `MultiSelection` over `gio::ListStore` of `glib::BoxedAnyObject` holding `crate::media_library::LibTrack`:
  - Files view: `multi_sel` / `col_view` (~3573), inside `files_vbox`.
  - Device view: `dev_selection` (~835), `dev_col_view` (~836); page `dev_page`/`dev_detail`.
  - Disc view: `disc_files_selection` (~2807); page `disc_page`/`disc_detail` (the data-disc file list, may be hidden until a data disc is present).
  - Playlists view: the selected playlist's track preview list — locate its selection/store (search `preview`, `pl_edit`, the ColumnView inside `pl_vbox`/`edit_vbox` ~8272).
- `LibTrack.length_secs`: `Option<i64>` (see `t.length_secs.map(|s| s as u32)` ~2519, `t.length_secs.is_none()` ~1156) — cast `s.max(0) as u64` for the sum.
- macOS ML views: `MLFilesTable.swift`, `MLPlaylistEditor.swift`, `DeviceDetailView.swift`, `DiscDriveView.swift`. The shared formatter `formatStatus(count:totalSecs:selectedSecs:)` already exists as a `static` on `PlaylistView` (phase 7) — reuse or lift it to a shared helper.

---

## Task 1: GTK — shared `ml_status_bar` helper + Files view

**Files:** Modify `frontends/gtk/window/media_library.rs`. Build-gated + manual.

**Interfaces:** Produces a module-scope `fn ml_status_bar(selection: &MultiSelection) -> (Label, std::rc::Rc<dyn Fn()>)` consumed by Tasks 1–2.

- [ ] **Step 1: Write the helper.** Add near the top of `media_library.rs` (module scope, before `open_media_library_window`):

```rust
/// Bottom status bar for a Media Library list view: `N tracks · MM:SS total ·
/// MM:SS selected`, matching the active playlist. Works over any MultiSelection
/// whose items are BoxedAnyObject<LibTrack>. Returns the Label (append it to the
/// view's page box) and a refresh closure (already wired to selection + model
/// changes; also call it once after the store is first populated).
fn ml_status_bar(selection: &MultiSelection) -> (Label, std::rc::Rc<dyn Fn()>) {
    let label = Label::builder()
        .halign(Align::Start)
        .css_classes(["status-label"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .margin_start(8)
        .margin_end(8)
        .margin_top(2)
        .margin_bottom(5)
        .build();
    let refresh: std::rc::Rc<dyn Fn()> = {
        let label = label.clone();
        let selection = selection.clone();
        std::rc::Rc::new(move || {
            let n = selection.n_items();
            let (mut count, mut total, mut sel_n, mut sel_secs) = (0usize, 0u64, 0usize, 0u64);
            for i in 0..n {
                let Some(obj) = selection.item(i) else { continue };
                let Ok(bx) = obj.downcast::<glib::BoxedAnyObject>() else { continue };
                let t = bx.borrow::<crate::media_library::LibTrack>();
                let secs = t.length_secs.unwrap_or(0).max(0) as u64;
                count += 1;
                total += secs;
                if selection.is_selected(i) {
                    sel_n += 1;
                    sel_secs += secs;
                }
            }
            let sel = if sel_n > 0 { Some(sel_secs) } else { None };
            label.set_text(&crate::playlist_status::playlist_status_line(count, total, sel));
        })
    };
    selection.connect_selection_changed({
        let r = refresh.clone();
        move |_, _, _| r()
    });
    selection.connect_items_changed({
        let r = refresh.clone();
        move |_, _, _, _| r()
    });
    refresh();
    (label, refresh)
}
```

> Verify against gtk4 0.9: `MultiSelection` implements `SelectionModel` + `ListModel`, so `n_items()`, `item(i)`, `is_selected(i)`, `connect_selection_changed(|model, position, n_items|)`, and `connect_items_changed(|model, pos, removed, added|)` are all available. Confirm `LibTrack.length_secs`'s exact integer type and adjust the cast. If `status-label` CSS gives unwanted padding, tune the margins so it matches the active-playlist bar visually.

- [ ] **Step 2: Wire the Files view.** Find the Files view's `multi_sel` (~3573) and its page box `files_vbox` (~5364). After both exist, call the helper and append the label to the BOTTOM of `files_vbox`:

```rust
    let (files_status_bar, files_status_refresh) = ml_status_bar(&multi_sel);
    files_vbox.append(&files_status_bar);
```

Then call `files_status_refresh()` at the point the Files store finishes (re)loading (search where the files store is populated / the search filter re-runs — the Files list already updates a count somewhere; add the refresh call there, or rely on the `items_changed` wiring if the store is mutated in place). Keep a clone if multiple populate sites need it.

- [ ] **Step 3: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings. Files view shows `N tracks · MM:SS total` at the bottom; selecting rows adds `· MM:SS selected`; filtering/search updates the count live.

- [ ] **Step 4: Commit.**

```bash
git add frontends/gtk/window/media_library.rs
git commit -m "feat(gtk): shared ML status-bar helper + Files view status line"
```

---

## Task 2: GTK — Playlists, Devices, Discs views

**Files:** Modify `frontends/gtk/window/media_library.rs`. Build-gated + manual.

**Interfaces:** Consumes `ml_status_bar` (Task 1).

- [ ] **Step 1: Device view.** Locate `dev_selection` (~835) and the device page box `dev_page` / `dev_detail` container. Append:

```rust
    let (dev_status_bar, dev_status_refresh) = ml_status_bar(&dev_selection);
    dev_detail.append(&dev_status_bar);
```

Call `dev_status_refresh()` where the device track list is (re)loaded (search where `dev_all_tracks` / the device store is filled). The device view already has a transient `dev_status`/`dev_hint` action-status label — this NEW count·total bar is separate; place it so both read cleanly (put the count bar at the very bottom, below the action row, OR just above it — match the active-playlist feel; pick the bottom of the page).

- [ ] **Step 2: Disc view.** Locate `disc_files_selection` (~2807) and the disc page container `disc_detail`. The disc file list is only shown for data discs (`disc_files_scroll.set_visible(is_data_disc)` ~9042). Append the status bar and tie its visibility to the same `is_data_disc` condition so it only shows when there is a file list:

```rust
    let (disc_status_bar, disc_status_refresh) = ml_status_bar(&disc_files_selection);
    disc_detail.append(&disc_status_bar);
    // hide with the file list (audio CDs / empty tray show no file list)
    disc_status_bar.set_visible(false);
```

Set `disc_status_bar.set_visible(is_data_disc)` alongside the existing `disc_files_scroll.set_visible(is_data_disc)` call(s) (~9042 and the reset ~8877), and call `disc_status_refresh()` when the disc file store loads (~3057 area).

- [ ] **Step 3: Playlists view.** Locate the selected-playlist track preview/edit list inside `pl_vbox` (~8278) / `edit_vbox` (~8272) — its `MultiSelection`/ColumnView (search the ColumnView built for the playlist's tracks). Append its status bar to that view's box and call refresh when a playlist is opened/previewed (its track store loads). If the Playlists tab has TWO sub-views (manage list of playlists vs. edit a playlist's tracks), attach the bar to the one that shows TRACKS (the edit/preview view), not the list-of-playlists.

- [ ] **Step 4: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings. Devices / Discs (data disc) / Playlists-tracks each show the count·total·selected bar; it updates on selection and on list (re)load; the disc bar hides for audio CDs / empty tray.

- [ ] **Step 5: Commit.**

```bash
git add frontends/gtk/window/media_library.rs
git commit -m "feat(gtk): status line on ML Devices, Discs, and Playlists views"
```

---

## Task 3: macOS (blind) — status bars on the four ML views

**Files:** Modify `MLFilesTable.swift`, `MLPlaylistEditor.swift`, `DeviceDetailView.swift`, `DiscDriveView.swift`, and a shared helper location; `docs/mac-pass-checklist.md`. Read each file fully first.

**Interfaces:** Reuse the phase-7 `formatStatus(count:totalSecs:selectedSecs:)` (currently `static` on `PlaylistView`) — lift it to a free helper or a small `MLStatusBar` SwiftUI view so all views share ONE formatter identical to core.

- [ ] **Step 1: Read the four files fully** + find `PlaylistView.formatStatus` (phase 7). Decide the shared home: either make `formatStatus` a free top-level func (e.g. in a small `PlaylistStatus.swift`) or an `MLStatusBar` `View` that takes count/total/selected and renders `Text(formatStatus(...))`. Keep the exact format (mirrors `src/playlist_status.rs`).

- [ ] **Step 2: Each view** — compute count = its row model's count, total = sum of the rows' durations (the mac ML row types carry a duration; grep each file for the duration field, e.g. `duration`/`length`), selected = `nil` when the view's selection set is empty else the sum over selected rows. Render the shared status bar at the BOTTOM of each view (`MLFilesTable`, `MLPlaylistEditor`'s track list, `DeviceDetailView`'s file list, `DiscDriveView`'s disc-files list). Use each view's REAL model/selection property names — do not invent. Where a view has no selection concept, pass `selectedSecs: nil`.

- [ ] **Step 3: Checklist.** Append a dated section to `docs/mac-pass-checklist.md`: each of the four ML views shows `N tracks · MM:SS total` at the bottom, adds `· MM:SS selected` when ≥1 row selected, updates on selection + list reload; format matches the active playlist.

- [ ] **Step 4: Build (Rust only — Swift blind) + commit.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings (Rust unchanged).

```bash
git add frontends/SparkampMac/ docs/mac-pass-checklist.md
git commit -m "feat(mac): status line on the four Media Library views (blind)"
```

---

## Task 4: Gate + close-out

- [ ] **Step 1: Full gate.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test 2>&1 | grep -E "warning:|error|test result:"'`
Expected: two `test result:` lines, 0 failed, no `warning:`/`error` (single-thread if only the known disc flake trips).

- [ ] **Step 2: Record.** Add a one-line note to the phase-7 known-limitations (or a short new section) in `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md`: ML views now carry the shared count·total·selected status bar (GTK + mac); mac blind-verified via checklist.

- [ ] **Step 3: Commit.**

```bash
git add docs/
git commit -m "docs: record ML status-bar consistency pass"
```

## Manual test plan (GTK interactive + mac checklist)

1. Files view: bottom bar shows count·total; search/filter updates it; selecting rows shows `· selected`; deselect hides it.
2. Devices view: same, for the shown device's files.
3. Discs view: shows for a DATA disc's file list; hidden for audio CD / empty tray; updates on disc load.
4. Playlists view: shows for the opened playlist's tracks.
5. All four read identically to the active-playlist bar (same format, same look).
6. mac: the four ML views per `docs/mac-pass-checklist.md`.

## Self-review notes

- Shared helper (GTK `ml_status_bar`, mac shared `formatStatus`/`MLStatusBar`) prevents format drift across views — the whole point of the request.
- Perf: total is O(view rows) per refresh; Files can be ~36k rows — acceptable on selection/reload (one linear pass, no per-row widget work), but if a profile shows jank, cache total on model-change and only re-sum selection on selection-change.
- Anchors drift in the 10k-line file — each task re-greps its view's selection/container before editing.
