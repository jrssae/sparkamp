# Phase 9 follow-on — Disc metadata source badge + CD-TEXT editor seeding

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.
> This EXTENDS Phase 9 (CD-TEXT read parity). Read
> `docs/superpowers/plans/2026-07-28-phase9-cdtext-read.md` and
> `2026-07-19-opus-handoff.md` first. Steps use checkbox (`- [ ]`) syntax.

**Goal:** (1) Show a small text pill on the disc view indicating WHERE the
displayed track metadata came from — `gnudb`, `edited`, or `CD-TEXT` — so the
source is unambiguous. (2) Seed the disc tag editor from CD-TEXT when the disc
has no gnudb/user entry, so a CD-TEXT-only disc can be edited → saved →
submitted to gnudb in one pass (today artist/album come up blank).

**Architecture:** Precedence is whole-entry (a disc's whole displayed set comes
from ONE source), so a SINGLE disc-level badge is correct — not per-row. A pure
core fn maps the three per-disc cache states (official gnudb match present /
user tag set present / CD-TEXT present) to a `DiscMetaSource`. Each frontend
computes it from the caches it already holds (`disc_official`/`disc_tags`/
`disc_cdtext` on GTK+TUI; `discOfficial`/`discTagSets`/`discCdtext` on mac) and
renders the pill next to the "Artist — Album" header. The editor-seed change is
a one-line `.or_else(cdtext)` at each frontend's tag-editor prefill.

**Tech Stack:** Rust core; GTK4 (skinnable CSS pill); Ratatui (text tag);
SwiftUI (blind — no Swift compiler here).

## Global Constraints

- Build/test ONLY inside distrobox `dev-box`:
  `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`.
  Host builds fail. Never gate on `cargo build --lib` (GTK/TUI are bin
  targets). Run cargo commands in the FOREGROUND. Zero warnings + zero
  failures before any task is done. Known parallel-race flakes
  (`disc::burn::...run_tool_watchdog_kills_a_wedged_child`,
  `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow`) —
  re-run the gate single-threaded (`-- --test-threads=1`) if one trips.
- **Source precedence (LOCKED, matches the Winamp whole-entry rule):**
  official gnudb match present → `gnudb`; else a user tag set present (no
  official) → `edited`; else CD-TEXT present → `CD-TEXT`; else none (no pill).
- **Badge style:** TEXT PILL (user decision) — the source word in a small
  rounded label (GTK/mac), a bracketed text tag on TUI (`[CD-TEXT]`).
- **Editor seeding:** when the disc has neither an official nor a user tag
  set, the tag editor's artist/album/year/genre/titles seed from the CD-TEXT
  entry (`disc_cdtext`). gnudb/user entries still win when present.
  CD-TEXT is STILL never auto-submitted — the user's edit+save is the
  promotion step (unchanged).
- **macOS is BLIND:** read whole Swift files, real property names, mirror any
  FFI byte-for-byte, and append verification items to
  `docs/mac-pass-checklist.md` (phase-9 section) in the SAME commit.
- New submodule `src/disc/source.rs` needs `pub mod source;` in
  `src/disc/mod.rs` (it is a child of the `disc` module tree — does NOT need
  entries in `src/lib.rs`/`src/main.rs`).
- Commits end: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
  Do NOT push.

**Base commit (pre-Task-1):** current `album-art-improvements` HEAD (`1cafcf4`).

---

## File Structure

- `src/disc/source.rs` — NEW. `DiscMetaSource` enum + `resolve` + `badge`.
- `src/disc/mod.rs` — MODIFY. `pub mod source;`.
- `frontends/gtk/window/media_library.rs` — MODIFY. Compute + render the pill
  in the disc header block.
- `frontends/gtk/window/disc.rs` — MODIFY. Seed the tag editor from CD-TEXT.
- `frontends/gtk/window/skin.rs` — MODIFY. `.disc-source-pill` CSS.
- `frontends/tui/ui/media_library.rs` — MODIFY. Render the text tag in the
  disc header.
- `frontends/tui/media_library/tags.rs` — MODIFY. Seed `open_disc_tag_editor`
  from CD-TEXT.
- `frontends/SparkampMac/Sources/DiscDriveView.swift` + `SparkampModel+Discs.swift`
  — MODIFY, BLIND. Pill + editor seed.
- `docs/mac-pass-checklist.md` — MODIFY. Phase-9 badge/seed verification.

---

### Task 1: Core — `DiscMetaSource`

**Files:**
- Create: `src/disc/source.rs`
- Modify: `src/disc/mod.rs` (add `pub mod source;` beside the other `pub mod`s)
- Test: `src/disc/source.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `pub enum DiscMetaSource { Gnudb, Edited, CdText, None }` (derive
    `Debug, Clone, Copy, PartialEq, Eq`).
  - `pub fn resolve(has_official: bool, has_user_tags: bool, has_cdtext: bool) -> DiscMetaSource`
  - `pub fn badge(self) -> Option<&'static str>` on the enum.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_and_badge_follow_whole_entry_precedence() {
        // official gnudb match → gnudb, regardless of cdtext.
        assert_eq!(DiscMetaSource::resolve(true, true, true), DiscMetaSource::Gnudb);
        assert_eq!(DiscMetaSource::resolve(true, false, false), DiscMetaSource::Gnudb);
        // user tag set, no official → edited (even if cdtext also present).
        assert_eq!(DiscMetaSource::resolve(false, true, true), DiscMetaSource::Edited);
        // only cdtext → CD-TEXT.
        assert_eq!(DiscMetaSource::resolve(false, false, true), DiscMetaSource::CdText);
        // nothing → None, no pill.
        assert_eq!(DiscMetaSource::resolve(false, false, false), DiscMetaSource::None);

        assert_eq!(DiscMetaSource::Gnudb.badge(), Some("gnudb"));
        assert_eq!(DiscMetaSource::Edited.badge(), Some("edited"));
        assert_eq!(DiscMetaSource::CdText.badge(), Some("CD-TEXT"));
        assert_eq!(DiscMetaSource::None.badge(), None);
    }
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib disc::source'`
Expected: FAIL — module/type not found.

- [ ] **Step 3: Implement**

`src/disc/source.rs`:

```rust
//! Which source produced the disc metadata currently shown in the disc view.
//! Precedence is whole-entry (the whole displayed track set comes from one
//! source), so this is a single disc-level classification, not per-track.

/// The origin of the disc's displayed album/artist/track names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscMetaSource {
    /// An official gnudb match (possibly user-tweaked, but gnudb-derived).
    Gnudb,
    /// A user-created/edited tag set with no official gnudb match behind it.
    Edited,
    /// CD-TEXT read off the disc (gnudb had no match).
    CdText,
    /// No metadata — the "Track N" fallback; no badge shown.
    None,
}

impl DiscMetaSource {
    /// Classify from what each per-disc cache holds. `has_official` = an
    /// untouched gnudb match is on file; `has_user_tags` = a displayed/edited
    /// tag set exists; `has_cdtext` = CD-TEXT was read. gnudb/user win over
    /// CD-TEXT (whole-entry precedence).
    pub fn resolve(has_official: bool, has_user_tags: bool, has_cdtext: bool) -> Self {
        if has_official {
            DiscMetaSource::Gnudb
        } else if has_user_tags {
            DiscMetaSource::Edited
        } else if has_cdtext {
            DiscMetaSource::CdText
        } else {
            DiscMetaSource::None
        }
    }

    /// Short pill text, or `None` when there is nothing to badge.
    pub fn badge(self) -> Option<&'static str> {
        match self {
            DiscMetaSource::Gnudb => Some("gnudb"),
            DiscMetaSource::Edited => Some("edited"),
            DiscMetaSource::CdText => Some("CD-TEXT"),
            DiscMetaSource::None => None,
        }
    }
}
```

Add `pub mod source;` to `src/disc/mod.rs` beside the other `pub mod` lines.

- [ ] **Step 4: Run it, verify pass**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib disc::source'`
Expected: PASS.

- [ ] **Step 5: Full build**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: zero warnings. (If the enum trips dead-code before frontends
consume it, that's fine — frontend tasks land in the same branch; but the
`#[cfg(test)]` use keeps it referenced. If a warning appears, add
`#[allow(dead_code)]` on the enum with a comment that frontends consume it in
Tasks 2-4, matching repo precedent, and remove it when unused.)

- [ ] **Step 6: Commit**

```bash
git add src/disc/source.rs src/disc/mod.rs
git commit -m "feat(disc): DiscMetaSource classifier (gnudb/edited/CD-TEXT)"
```

---

### Task 2: GTK — source pill + CD-TEXT editor seeding

**Files:**
- Modify: `frontends/gtk/window/media_library.rs` (disc header render block)
- Modify: `frontends/gtk/window/disc.rs` (tag-editor prefill)
- Modify: `frontends/gtk/window/skin.rs` (`.disc-source-pill` CSS + test)

**Interfaces:**
- Consumes: `crate::disc::source::DiscMetaSource`, the existing
  `disc_official`, `disc_tags`, `disc_cdtext` `Rc<RefCell<HashMap<..>>>`
  caches (all in scope in the disc render block, ~`media_library.rs:8971-9108`).

> **Read first:** the disc header render block in `media_library.rs` (the
> `if let Some(id) = &discid { ... }` around lines 9033-9108) where `header`
> (the "Artist — Album (year)" string) is built and `disc_tag_lbl` is set; and
> the tag-editor prefill in `disc.rs` (the edit dialog around lines 323-594
> that fills `artist_entry`/`album_entry`/title rows from a stored entry).

- [ ] **Step 1: Add a source-pill label to the disc header row**

Near where `disc_tag_lbl` is built (`media_library.rs:2791-2798`), add a
sibling `Label` `disc_source_pill` with css class `disc-source-pill`,
`set_visible(false)` initially, appended to `disc_header_text` right after
`disc_tag_lbl`. Clone it into the render closure alongside `tag_lbl` (~8971).

- [ ] **Step 2: Compute + set the pill in the render block**

In the disc render block, after the `header`/`entry` resolution (~9044-9108),
compute the source from the three caches for the current `id`:

```rust
let source = crate::disc::source::DiscMetaSource::resolve(
    disc_official.borrow().contains_key(id),
    disc_tags.borrow().get(id).is_some(),
    disc_cdtext.borrow().get(id).is_some(),
);
match source.badge() {
    Some(text) => {
        source_pill.set_text(text);
        source_pill.set_visible(true);
    }
    None => source_pill.set_visible(false),
}
```

Place it so it does not hold a `borrow()` across a UI call (each
`.borrow()...` is released at the statement). Ensure the pill is hidden on the
non-audio / no-disc branches (mirror how `tag_lbl.set_visible(false)` is
handled).

- [ ] **Step 3: Seed the tag editor from CD-TEXT**

In `disc.rs`, where the edit dialog prefills its fields from the stored tag
set (`disc_tags.borrow().get(&discid)`), change the source entry to fall back
to CD-TEXT when there is no stored/user entry:
`disc_tags.borrow().get(&discid).cloned().or_else(|| disc_cdtext.borrow().get(&discid).cloned())`.
This requires threading the `disc_cdtext` cache into the edit-UI function the
same way Task-5 of the read plan threaded it into `connect_rip_ui` (add a
param, pass `disc_cdtext.clone()` at the call site in `media_library.rs`). Read
the edit fn's signature + call site first and mirror the rip plumbing exactly.
Do NOT hold two RefCell borrows across a call — bind the gnudb lookup to a
local, then `.or_else` the cdtext lookup (a small pure helper like
`select_rip_tags` is fine if it keeps it clean).

- [ ] **Step 4: Skin the pill (TDD in skin.rs)**

Add a `.disc-source-pill` rule to `render_gtk_css` (small rounded label:
subtle background using an existing skin var, `text`/`text_dim` foreground,
small font, padding + border-radius — mirror an existing pill/badge class if
one exists, e.g. the queue badge or `.ml-section-header`). Add a TDD test in
`skin.rs` asserting the rendered CSS `contains(".disc-source-pill")`, using the
real `SkinVars` idiom already used by the other skin tests.

- [ ] **Step 5: Full build + test**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: zero warnings; all pass (the new skin test included).

- [ ] **Step 6: Commit**

```bash
git add frontends/gtk/window/media_library.rs frontends/gtk/window/disc.rs frontends/gtk/window/skin.rs
git commit -m "feat(gtk): disc metadata source pill + seed tag editor from CD-TEXT"
```

---

### Task 3: TUI — source tag + CD-TEXT editor seeding

**Files:**
- Modify: `frontends/tui/ui/media_library.rs` (disc header render)
- Modify: `frontends/tui/media_library/tags.rs` (`open_disc_tag_editor`)
- Test: `frontends/tui/tests/discs.rs` (editor-seed precedence)

**Interfaces:**
- Consumes: `crate::disc::source::DiscMetaSource`, `self.disc_official`
  (verify the TUI App has an official-match cache; if it has none, pass
  `false` for `has_official` and the tag set alone distinguishes edited vs
  gnudb — see note), `self.disc_tags`, `self.disc_cdtext`.

> **Read first:** confirm whether the TUI App tracks a gnudb "official" cache
> (grep `disc_official` in `frontends/tui/`). If it does NOT, then a present
> `disc_tags` entry can be either a fetched gnudb match or a user edit —
> `resolve(false, has_user_tags, has_cdtext)` would label both `edited`. If
> that is the case, prefer a `has_official` derived from wherever the TUI
> records a fetched gnudb match; if there is genuinely no such signal, pass
> `has_official = self.disc_official?.contains(id)` when available, else fall
> back to labeling a `disc_tags` hit as `gnudb` (the common case) and record
> the limitation. Ask if unsure rather than guessing.

- [ ] **Step 1: Render the source tag in the disc header**

In the disc view's header area of `ui/media_library.rs` (where the disc's
Artist/Album lines are drawn), append the source tag when
`DiscMetaSource::resolve(...).badge()` is `Some` — e.g. render `[CD-TEXT]` /
`[gnudb]` / `[edited]` at the end of the header line (dim style). No pill
graphics in a terminal — a bracketed tag is the convention.

- [ ] **Step 2: Write the failing editor-seed test**

In `frontends/tui/tests/discs.rs` (mirror `cdtext_overlays_only_when_gnudb_absent`):
load a CD-TEXT-only audio disc (populate `disc_cdtext[id]` with artist/album/
titles, `disc_tags` empty), call `open_disc_tag_editor()`, and assert the
resulting `DiscTagEditState` has `artist`/`album` seeded FROM CD-TEXT (not
blank) and the titles seeded from CD-TEXT. Add a second assertion: with a
gnudb/user `disc_tags` entry present, the editor seeds from THAT (gnudb wins).

- [ ] **Step 3: Run it, verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --bin sparkamp <testfn>'`
Expected: FAIL — artist/album come up blank today.

- [ ] **Step 4: Seed `open_disc_tag_editor` from CD-TEXT**

In `tags.rs::open_disc_tag_editor`, change the `stored` binding from
`self.disc_tags.get(&discid).cloned()` to
`self.disc_tags.get(&discid).cloned().or_else(|| self.disc_cdtext.get(&discid).cloned())`.
The existing title-fallback to `disc_entries` stays (titles already CD-TEXT-
overlaid); this additionally seeds artist/album/year/genre from CD-TEXT.

- [ ] **Step 5: Run it, verify pass; then full build + test**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: zero warnings; all pass.

- [ ] **Step 6: Commit**

```bash
git add frontends/tui/
git commit -m "feat(tui): disc metadata source tag + seed tag editor from CD-TEXT"
```

---

### Task 4: macOS — source pill + CD-TEXT editor seeding (BLIND)

**Files:**
- Modify: `frontends/SparkampMac/Sources/DiscDriveView.swift` (header pill)
- Modify: `frontends/SparkampMac/Sources/SparkampModel+Discs.swift` (source
  computation + editor seed)
- Modify: `docs/mac-pass-checklist.md` (phase-9 section, SAME commit)

**Interfaces:**
- Consumes: `discOfficial`, `discTagSets`, `discCdtext` (all exist —
  `SparkampModel+Discs.swift:80/73/97`), and the existing `discOverlayTags`
  helper.

> **BLIND — no Swift compiler here.** Read the whole disc view + model files.
> Use real property names. The Rust suite is unchanged by this task; verify
> `git status` shows only Swift + the checklist.

- [ ] **Step 1: Compute the source in the model**

Add a helper (mirror `resolve`'s logic; Swift-side is fine — no FFI needed
since mac holds all three caches):

```swift
// Which source produced the disc's displayed names — mirrors Rust
// DiscMetaSource::resolve. gnudb/user win over CD-TEXT (whole-entry).
func discMetaSourceBadge(_ id: String) -> String? {
    if discOfficial[id] != nil { return "gnudb" }
    if discTagSets[id] != nil { return "edited" }
    if discCdtext[id] != nil { return "CD-TEXT" }
    return nil
}
```

(Match the exact label strings `"gnudb"`/`"edited"`/`"CD-TEXT"` used by the
Rust `badge()` so the three frontends read identically.)

- [ ] **Step 2: Render the pill in the disc header**

In `DiscDriveView.swift`, next to the "Artist — Album (year)" header (where
`discOverlayTags` feeds the header, ~line 205), show a small rounded text pill
with `discMetaSourceBadge(id)` when non-nil (a `Text` in a capsule background —
mirror any existing small-label styling in the mac UI; use theme colors, don't
hardcode). Hidden when nil.

- [ ] **Step 3: Seed the disc tag editor from CD-TEXT**

Find the disc tag editor's prefill (grep for where it reads `discTagSets[id]`
to fill artist/album/title fields). Change it to seed from `discOverlayTags(id)`
(which already returns `discTagSets[id] ?? cdtext`) so a CD-TEXT-only disc
prefills artist/album/titles. Leave the gnudb SUBMIT path reading
`discTagSets` directly (do NOT let CD-TEXT auto-submit).

- [ ] **Step 4: Append phase-9 checklist items**

Under the existing "Phase 9 — CD-TEXT read" section in
`docs/mac-pass-checklist.md`, add a "source badge + editor seeding"
subsection:
1. gnudb-known disc → header shows `gnudb` pill.
2. gnudb-unknown CD-TEXT disc → header shows `CD-TEXT` pill.
3. Edit + save a CD-TEXT/unknown disc → pill flips to `edited`.
4. No metadata (Track N) → no pill.
5. Open the tag editor on a CD-TEXT-only disc → artist/album/titles PREFILLED
   from CD-TEXT (not blank); Save → Submit becomes available and uploads the
   promoted tags to gnudb.
6. Confirm the three frontends show identical badge text.

- [ ] **Step 5: Blind self-check + commit**

Re-read the changed Swift: the badge labels match Rust exactly; the editor
seed uses `discOverlayTags`; the submit path is untouched; `git status` shows
only Swift + checklist.

```bash
git add frontends/SparkampMac/ docs/mac-pass-checklist.md
git commit -m "feat(mac): disc metadata source pill + seed tag editor from CD-TEXT (blind)"
```

---

### Task 5: Close-out — docs + ledger

**Files:**
- Modify: `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md`
  (phase-9 known-limitations: add the source-badge feature + the "how to
  submit CD-TEXT to gnudb" note — edit+save promotes CD-TEXT into the user
  tag set, then Submit; nothing auto-uploads).
- Modify: `.superpowers/sdd/progress.md` + roadmap memory.

- [ ] **Step 1: Record in the spec** the badge (single disc-level, whole-entry
  source; labels gnudb/edited/CD-TEXT) and the editor-seed behavior + the
  submit path. Note any per-frontend source-signal caveat surfaced in Task 3.

- [ ] **Step 2: Full gate**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: zero warnings, all pass (single-threaded if a flake trips).

- [ ] **Step 3: Commit** + update ledger/roadmap.

---

## Self-Review

- **Spec coverage:** source badge (Tasks 1-4, all 3 frontends), whole-entry
  single-disc-level classification (Task 1 pure fn, table-tested), editor
  seeding from CD-TEXT (Tasks 2-4), submit-process documentation (Task 5), mac
  verification captured in the phase-9 checklist (Task 4) per the user's
  request to test it on macOS later.
- **Placeholder scan:** all code steps carry real code; the one genuine
  unknown (whether the TUI tracks an `official` cache) is flagged with an
  explicit read + ask step, not hidden.
- **Type consistency:** `DiscMetaSource::resolve(bool,bool,bool)` and
  `badge() -> Option<&'static str>` used identically by GTK/TUI; mac mirrors
  the label strings exactly; editor seed is `tags.or(cdtext)` at every
  frontend, matching the read plan's precedence.
