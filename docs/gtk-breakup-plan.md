# GTK breakup plan — `media_library.rs` and `player.rs`

Branch: `code-breakup` · Written 2026-08-05 against `9ab2322`

Extends [refactor-plan.md](refactor-plan.md), whose Phase 2 named this work
and never started it. That document's goal still stands: **no source file
over ~800 lines, no function over ~300.** This one supplies the architecture
its Phase 2b/2c left as one-liners.

---

## 1. What we are actually up against

> **Resolved 2026-08-10, steps 1–7.** `media_library.rs` is now 461 lines and
> `open_media_library_window` is 199. The section below is kept as written
> because it is the argument the whole plan rests on, and because the
> measurements in it are what every later step was chosen against.

`frontends/gtk/window/media_library.rs` is 11,927 lines. That number
undersells it. The file is **one function**: `open_media_library_window`
spans lines 62–11814, or 11,752 lines, 574 KB, roughly 143k tokens. There is
no signature boundary anywhere inside it to read or reason against.

It is one function because everything inside captures everything else:

| Measure | Count |
|---|---|
| `Rc::new(RefCell::new(…))` locals | 51 |
| `Rc<dyn Fn…>` callbacks | 84 |
| `connect_*` signal closures | 194 |
| `.clone()` calls (capture churn) | 1,446 |

That capture web — not the line count — is the thing that has to be
dismantled. Splitting the *file* without splitting the *function* is what
the last attempt did, and it is why we are here again.

### The constraint that shapes everything

**GTK cannot be compiled, or even `cargo check`ed, on the macOS dev machine.**
`src/main.rs:58` gates the frontend behind `#[cfg(target_os = "linux")]` with
no feature flag. `refactor-plan.md` hit this exact wall in June and parked
Phase 2 for it.

The stopgap that followed (task #45) is documented at `window/mod.rs:94–104`
and is worth quoting, because it constrains what "a module" currently means
here:

> window.rs reached ~21k lines, unworkable for review or for smaller models.
> The sections below are include!d verbatim: every file is a plain byte slice
> of the old window.rs … Converting these to real `mod` submodules
> (pub(super) items + per-file imports) is a follow-up to do ON the Linux
> box, one file at a time, where the compiler can arbitrate.

So `state.rs`, `util.rs`, `player.rs`, `media_library.rs` and friends are
**not modules**. They are `include!()` byte slices sharing one namespace.
Only `disc`, `watch`, `now_playing`, `art_window` and `mpris` are real `mod`s.

> **Resolved 2026-08-11 by step 8** (`d328c1c`, `6b7ee5c`). Every one of them
> is a real `mod` now, and `window/mod.rs` contains no `include!`. The rest of
> this section is kept as the problem statement the plan was written against.

Two consequences:

1. A physical re-split into more `include!` files is provable offline
   (byte-identity) but buys us nothing architecturally — the namespace stays
   flat and the function stays whole.
2. The real work — extracting functions, choosing parameters, fixing
   visibility — **cannot be validated on this machine**. Doing it blind here
   would repeat the phase-12 lesson at ten times the scale.

### Unblock this first — DONE 2026-08-05

`.github/workflows/gtk-check.yml` runs `cargo check --all-targets` on
`ubuntu-latest` with `libgtk-4-dev`, on every branch. `refactor-plan.md:11`
suggested this in June; the repo now has working Actions, so it was a small
job. **143 s cold, 58 s warm** — a usable per-extraction loop.

Verified by A/B/A rather than assumed: clean tree green, a deliberate
`let _x: i32 = "not an integer"` at `media_library.rs:11930` red with
`error[E0308]`, then green again once reverted. So the gate genuinely
type-checks GTK source at depth, and `--all-targets` failed the bin *and* the
test target, confirming test code is covered too.

Two caveats. `RUSTFLAGS: -D warnings` passed first try, which confirms the
Linux tree is warning-free — but a warning was never actually tested against
the gate, so that half is assumed until the first extraction strands an
import. And a note for reading errors: paths are reported through the
`include!` indirection as `src/../frontends/gtk/window/…`, not repo-relative.

Nothing about this covers runtime behaviour — see [§5](#5-smoke-tests).

---

## 2. Your hypothesis, checked

You guessed each left-hand nav item gets its own file. That is close to
right, and the stack confirms the boundaries — `add_named` gives five
top-level destinations (six pages; Playlists nests two):

| Page | Sidebar item |
|---|---|
| `files` | Files |
| `albums` | Albums |
| `playlists` → `pl-manage`, `pl-edit` | Playlists (nested sub-stack) |
| `discs` | Disc Drives |
| `devices` | Devices |

Two things you could not have known complicate it.

**The file is not organised by feature.** It is organised widgets-first,
wiring-last. Devices' widgets sit at 598–2963 but its polling and behaviour
at 9408–9820. Discs' widgets at 2964–3978, its wiring at 9821–11813:

| Region | Lines | Size |
|---|---|---|
| sidebar + setup | 98–597 | 500 |
| devices — widgets | 598–2963 | 2,366 |
| discs — widgets | 2964–3978 | 1,015 |
| stack assembly | 3979–4011 | 33 |
| **files** | 4012–6141 | 2,130 |
| **albums** | 6142–6257 | 116 |
| **playlists** | 6258–9407 | 3,150 |
| devices — poll/wiring | 9408–9820 | 413 |
| discs — wiring | 9821–11813 | 1,993 |

Cutting naively at nav boundaries gives Devices and Discs two disjoint halves
each. Reuniting them means *moving* code, and in a function this closure-dense
statement order is semantically load-bearing — a closure can only capture
locals declared above it. Moves must be compiler-checked, one at a time.

**The nav split alone does not shrink anything enough.** Playlists at 3,150
and Devices at 2,779 are each still four times the 800-line target. Every
page needs an internal second cut as well.

---

## 3. Architecture

### 3.1 The seam already exists — use it

The file contains **24 `*_holder` cells**: `refresh_discs_holder`,
`col_view_holder`, `files_status_holder`, `rebuild_track_list_holder`,
`eject_holder`, `send_holder`, `sync_holder`, and eighteen more. Each is an
`Rc<RefCell<Option<Rc<dyn Fn…>>>>` — declared empty, filled once the thing it
refers to exists.

That is a late-binding indirection, and it is precisely what breaks the
"closure A needs widget B, which is built by closure A" cycles that force
everything into one scope. The codebase invented it organically under
pressure. The refactor should adopt it deliberately as *the* cross-page
contract rather than treating it as incidental.

### 3.2 `MlCtx` — one context instead of thirty captures

`open_media_library_window` already takes 10 parameters, 8 of them shared
`Rc` state. Every extracted page would need most of them plus a shifting
subset of the 51 local cells. Threading those positionally is how a refactor
turns into a 15-argument function nobody can call.

Introduce one context struct, built once at the top and passed by reference:

```rust
/// Everything a Media Library page needs from its host. Built once in
/// `open_media_library_window`, borrowed by every page builder.
pub(super) struct MlCtx {
    // From the caller (player.rs) — shared so the active playlist's
    // Send-to menu sees the same drives, devices and burn queues.
    pub state: Rc<RefCell<AppState>>,
    pub rebuild_playlist: Rc<dyn Fn()>,
    pub set_track: Rc<dyn Fn(&str)>,
    pub current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>>,
    pub current_devices: Rc<RefCell<Vec<crate::devices::Device>>>,
    pub burn_queues: Rc<RefCell<crate::disc::burnlist::BurnQueues>>,
    pub copy_files: CopyFilesHolder,
    pub burn_refresh: RefreshHolder,

    // Shared chrome the pages attach themselves to.
    pub win: gtk4::Window,
    pub stack: Stack,
    pub sidebar: ListBox,

    // Cross-page late binds — the holder pattern, promoted to the contract.
    pub reload_files: RefreshHolder,
    pub refresh_discs: RefreshHolder,
    pub reload_devices: RefreshHolder,
    pub reload_playlists: RefreshHolder,
}

/// `Rc<RefCell<Option<Rc<dyn Fn()>>>>`, the existing holder shape, named.
pub(super) type RefreshHolder = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
```

Each page then has one shape:

```rust
pub(super) fn build(ctx: &MlCtx) -> gtk4::Box
```

State private to a page (`pl_edit_query`, `drag_selection`, `disc_fingerprints`,
`rip_preselect`, …) stays inside that page's module and never reaches `MlCtx`.
Line numbers say most of the 51 cells are already page-local: 3989–4074 are
Files-only, 6275–6605 Playlists-only, 9953–10730 Discs-only. Only the sidebar
row vectors and the four reload holders are genuinely shared.

**The test for whether a cell belongs in `MlCtx`: is it touched by more than
one stack page?** If not, it moves with its page. Expect `MlCtx` to end at
12–16 fields, not 51.

### 3.3 Target layout

```
window/media_library/
  mod.rs         ~350   MlCtx, open_media_library_window: build ctx,
                        assemble sidebar + stack, wire holders, return win
  sidebar.rs     ~450   the ListBox, its rows, expand/collapse, routing
  files.rs       ~900   Files page: columns, search, status bar
  files_menu.rs  ~600   Files row context menu + Send-to
  albums.rs      ~150   thin shim over the existing album_gallery
  playlists/
    mod.rs       ~250   pl_sub_stack, load-by-id, sidebar sub-rows
    manage.rs    ~800   pl-manage: new / rename / delete
    editor.rs    ~900   pl-edit: table, add, remove, revert, save-as
    editor_menu.rs ~700 editor row context menu
  devices/
    mod.rs       ~200
    overview.rs  ~700   cards, counts, poll
    detail.rs    ~900   file list, columns, status bar
    actions.rs   ~800   copy / sync / scan / eject
  discs/
    mod.rs       ~200
    audio.rs     ~700   audio-CD track list + add
    data.rs      ~700   data-disc browser
    gnudb.rs     ~700   identify + tag override
    rip.rs       ~700   rip dialog + worker
```

Every file lands under the 800-line goal but two, both close. The existing
real `mod disc` (1,815 lines, drive-view helpers + rip worker) is the natural
home for the `discs/` group — extend it rather than creating a rival.

**What steps 5 and 6 actually built is flatter than this**, and the count is
higher: `disc_page`/`disc_data`/`disc_gnudb` and
`devices_page`/`devices_poll`/`devices_menu`/`devices_playlists`/
`devices_actions`/`devices_columns`, all siblings of `window/`. Flat because
`use super::…` reaches the window module's private items directly, where a
nested `devices/detail.rs` would spell every one of them `super::super`. Six
device files rather than four because the cuts were chosen by measured seam
width rather than by this sketch's guesses — see the two "what step N
actually did" sections in [§4](#4-sequence). Read the sketch as the shape of
the answer, not the file list.

### 3.4 Alternatives considered

**One file per nav item, no internal split.** Your original instinct.
Rejected: leaves Playlists at 3,150 and Devices at 2,779 lines. Halves the
problem and stops.

**Keep `include!`, just add more slices.** The only option that is fully
provable offline. Rejected as an end state — it leaves one namespace and one
11.7k-line function, so it fixes the file listing and nothing a maintainer or
a model actually trips over. Worth keeping as a *fallback* if step 0 (CI
check) proves harder than expected.

**A trait per page (`trait MlPage { fn build(&self, ctx: &MlCtx) }`).**
Rejected: adds a vtable and a lifetime puzzle for zero benefit. The pages are
built once, never swapped, never iterated polymorphically. Free functions
returning widgets are the honest shape.

**Push state into `AppState`.** Rejected: `AppState` is the *application's*
model, shared with player.rs and the TUI-facing core. Media-Library window
chrome does not belong there, and widening it would couple the two windows
harder than they already are.

---

## 4. Sequence

Every step is one commit. Never combine a move with a behaviour change.

| # | Step | Lines | Risk | Gate | Manual test |
|---|---|---|---|---|---|
| 0 | ~~CI `cargo check` on ubuntu + libgtk-4-dev~~ **DONE `1f19f00`** | ~100 | none | ✅ A/B/A verified | — |
| 1 | ~~`MlCtx` + `RefreshHolder` introduced~~ **DONE `6cbc556`** | ~150 | low | CI | **A** |
| 2 | ~~**`albums.rs`** — mechanism pilot~~ **DONE `a80004e`** | 116 | very low | CI | **A + B** |
| 3 | ~~`sidebar.rs`~~ **DONE `cbd4bcd`** | 500 | medium | CI | **A + C** (batchable with 4) |
| 4 | ~~`files.rs` + `files_menu.rs`~~ **DONE `2d64bb8`, `7f0ceaa`** | 2,130 | medium | CI | **A + D + X** |
| 5 | ~~Discs — hoist, then extract~~ **DONE `eb4c994` (5a), `cd88e67` `2a64c0f` (5b)** | 2,499 | **high** | CI ✅ | **A + E + X** ✅ 2026-08-11 (in step 8's sweep) |
| 6 | ~~`devices/` — reunite the widgets with the wiring, then extract~~ **DONE `9caffd7` `cfd3cb3` (6a/6b), `08b08ad` `e3eb626` (6c/6d)** | 3,456 | **high** | CI ✅ | **A + F** ✅ 2026-08-10 |
| 7 | ~~`playlists/`~~ **DONE `d572f0f` (7a), `0167817` (7b), `c58ed1c` (7c)** | 3,139 | high | CI ✅ | **A + G + X** ✅ 2026-08-11 |
| 8 | ~~Convert the remaining `include!`s in `window/mod.rs` to real `mod`s~~ **DONE `d328c1c` (11 slices), `6b7ee5c` (the last 4)** | 17,386 | medium | CI ✅ | **49/49 ✅ 2026-08-11** (29 automated, 20 by hand) |
| 9 | ~~`player.rs` — `playlist_window.rs` + `dnd.rs` + `keys.rs`~~ **DONE** | 2,405 | medium | CI ✅ | **A + G** — owed |

**All ten steps are complete.** `window/mod.rs` holds no `include!`; it is 250
lines of `mod` and `use` declarations plus the module docs. Step 8's sweep ran
all 49 checks — 29 automated, 20 by hand — and the E group in it discharges
step 5's owed round, which had never been run. Step 9 owes a manual round of
its own; see its close-out below for what to exercise.

**19 of the 20 manual checks passed.** The one failure was X39 (Rescan
Metadata's count), and the round turned up four more defects outside the
checklist entirely. None of the five came from the extraction; all five are
fixed. See "What the manual round found" in the step 8 close-out below.

Test groups are defined in [§5](#5-smoke-tests). Steps 5 and 6 run their group
twice — once after the hoist commit, once after the extract.

**Step 2 is the pilot for a reason.** Albums is 116 contiguous lines that
already delegate to `album_gallery.rs`. It exercises the entire mechanism —
`MlCtx`, a real `mod`, `pub(super)` visibility, the stack hookup — at a size
where a mistake costs minutes. Do not skip it for something more impressive,
and do not batch its test with a later step: the whole point is finding a
wrong mechanism before six more steps are built on it.

**Steps 5 and 6 were the dangerous ones** because they move code across
thousands of lines of intervening statements, and closure capture depends on
declaration order. Split each into separate commits: hoist first, *within*
the existing function, and confirm green; only then extract the reunited
block; only then cut it up. Both are now done — and step 6 added a step in
front of the hoist that turned out to matter more than the hoist itself. See
the two sections below.

### What step 5 actually did — 2026-08-09

The hoist (5a) moved the *widgets down* to the wiring rather than the wiring
up to the widgets, which is the opposite of the sketch above and is the
cheaper direction: hoisting the wiring would have landed it above `stack`,
the Files column holders, the playlist editor's `track_list` and four
device-wiring closures it captures. Moving the widgets down crossed one
reference — the stack hookup, which came along. That is the shape to copy for
step 6, and its cost is on the record: the audio-CD row menu stopped
dispatching, the hoist was reverted (`b844e67`), fixed (`8d0614b`) and
reapplied (`eb4c994`).

The extraction (5b) landed **flat siblings**, not the nested `discs/` group
in [§3.3](#33-target-layout):

| File | Lines | What |
|---|---|---|
| `disc_page.rs` | 1,556 | overview cards, drive detail, 2 s poll, audio-CD wiring |
| `disc_data.rs` | 851 | the data-disc file browser (Task 9) |
| `disc_gnudb.rs` | 372 | identify + manual tag override |
| `disc.rs` | 1,815 | unchanged — disc logic + widget helpers the page calls |

Flat because `use super::…` reaches the window module's private items
directly; a nested `disc/page.rs` would spell every one of them
`super::super`. `disc` stays what it was and re-exports nothing.

`MlCtx` did not grow. The page takes `&Sidebar` as a second argument for the
three cells `sidebar.rs` built for it — its sub-rows, the chevron state, the
header spinner. By §3.2's test those are touched by one page only, so they
are a module-to-module handoff rather than shared window state. Use the same
shape in steps 6 and 7 instead of widening `MlCtx` toward the 51-cell sprawl
the test exists to prevent.

**`disc_page.rs` is still 1,556 against the ~800 target.** The seam that
remains is the widgets-first/wiring-last one, ~30 widgets wide, and narrowing
it means reordering statements a closure's capture depends on — a different
job from moving code. Two narrow cuts existed and both were taken (the data
browser, 4 names in / 5 out; gnudb, 8 in / 0 out). Expect the same residue in
step 6 and do not force a third cut through a wide seam to hit the number.

### What step 6 actually did — 2026-08-10

Four commits, not two. The extra one at the front is the reason the rest went
so much more cheaply than step 5.

**6a (`9caffd7`) — clear the way.** Before moving anything, the three names
outside the Devices block that referred into it were checked. Only three
existed across ~3,400 lines: `dev_page` (the `stack.add_named` call, which
travels with the widget), `copy_files_run` (an `MlCtx` field) and
`send_playlist_run` (captured by the playlist editor). The latter two already
had a holder sitting next to them — `copy_files_holder` on `MlHost`,
`send_playlist_holder` on `Sidebar` — and two call sites were simply
bypassing it. Routing them through it made the block a closed set and cost
one field on `MlCtx`.

**Do this measurement first in step 7.** It is cheap, it is what decides
whether the move is possible at all, and it turns "high risk" into a
mechanical edit. The two directions are not symmetric: moving the wiring *up*
would have stranded it above `stack`, above `ctx` and above the editor, while
moving the widgets *down* crossed nothing once 6a landed.

**6b (`cfd3cb3`) — the hoist.** 5,331 lines out, 5,331 in, file length
unchanged. Two orderings changed and both were checked rather than assumed:
Devices became the last page added to the stack (`Stack` resolves by name, so
not observable), and its `connect_row_selected` handler moved from second to
third of three. That last one is only safe because all three handlers
dispatch on disjoint `widget_name` prefixes with no catch-all — worth
re-checking in step 7, since Playlists' handler is one of them.

**6c (`08b08ad`) — the lift.** `devices_page::build(&ctx, &sb)`, the same
signature step 5 settled on. `media_library.rs` 7,077 → 3,628.

**6d (`e3eb626`) — five cuts**, all verified verbatim against 6c:

| File | Lines | Seam (in/out) |
|---|---|---|
| `devices_playlists.rs` | 617 | 17 / 1 |
| `devices_menu.rs` | 569 | 18 / 0 |
| `devices_poll.rs` | 497 | 12 / 4 |
| `devices_actions.rs` | 433 | 13 / 0 |
| `devices_columns.rs` | 356 | 6 / 2 |
| `devices_page.rs` | 1,535 | — |

**Measure candidate cuts, then choose.** The poll block reads 12 names; when
it was measured together with the selection handler that follows it, it read
40 and looked untouchable. The cut existed only because the two were measured
separately. A short script that reports, for a line range, which names it
defines that are read after it and which it reads that are defined before it,
is what made every decision in this step — including the decision in 6a.

`devices_page.rs` is still 1,535, next to `disc_page.rs`'s 1,556, and for the
same reason §"What step 5 actually did" gives. The selection handler alone
reads 33 widgets. That seam was left rather than forced.

**Manual test round: passed 2026-08-10** (groups A and F, on hardware, with a
USB stick and a rewritable disc). Five defects came out of it, and it is worth
recording that **none of them were caused by the extraction** — the move was
byte-verified at every step and behaved identically throughout. What the round
found was pre-existing:

| Fix | What it was |
|---|---|
| `8412ad0` | active-playlist Send-to ▸ Device showed no progress in the ML |
| `2fc2d64` | editor Send-to ▾ silently no-op'd with no rows selected |
| `4a2610a` | data-disc `playlist.m3u8` carried no `#EXTINF` |
| `60de50e`, `1e3195b` | a mounted data disc lost its media typing, so both burn buttons went dead with no explanation — including on the disc Sparkamp had just burned |

The lesson to carry into step 7 is about the test round, not the refactor: a
smoke-test group is worth running even when the diff is provably a move,
because it is the only time anyone exercises these paths end to end. Four of
the five had nothing to do with Devices.

Group X was not run as such; several of its items were covered incidentally by
the Send-to fixes above. Remaining data-disc issues are known and deliberately
deferred as lower priority than the remaining breakup steps.

### What step 7 actually did — 2026-08-10

**No hoist.** Measured first, as step 6 recommends, the Playlists block was
already contiguous and declared **nothing** the rest of the window read back —
its widgets and its wiring had never been separated by another page's code.
That measurement is now three-for-three at deciding the shape of a step before
any code moves.

What it did find was one thing in the way, and not the kind step 6 saw: a
single `connect_row_selected` handler routed Files, Albums *and* Playlists and
merely happened to sit here. `d572f0f` gave Files and Albums their own, which
is what `sidebar.rs` had documented all along, so the lift stayed a pure move.
`albums::build` stopped returning its `show_gallery_overview` closure at the
same time — its only two callers were the routing blocks that moved next to it.

| File | Lines | Seam (in/out) |
|---|---|---|
| `playlists_columns.rs` | 1,024 | 17 / 4 |
| `playlists_manage.rs` | 635 | 14 / 5 |
| `playlists_menu.rs` | 430 | 14 / 0 |
| `playlists.rs` | 1,471 | — |

One non-move edit was needed: `EditorEntry` was declared *inside* `build()`,
and the row menu reads the same wrapper back out of the store, so it moved to
module scope as `pub(super)`.

**The headline, and the reason this plan existed:**

```
media_library.rs            11,927 → 461 lines
open_media_library_window   11,752 → 199 lines
```

That function is now under `refactor-plan.md`'s 300-line goal, and the file is
what its name always implied: build the window and its chrome, assemble
`MlCtx`, call six page builders, save state on close. Both alias blocks shrank
with it — of `MlHost`'s eight fields only `state` is still aliased, and of the
sidebar's, only the list and its scroller.

**Manual test round: passed 2026-08-11** (groups A, G and X, on hardware).
It produced six fixes, and — as in step 6 — **none of them were caused by the
extraction**. The moves were byte-verified and behaved identically throughout;
what the round found was six pre-existing defects in paths nobody had
exercised end to end:

| Fix | What it was |
|---|---|
| `2b5910e` | three different artwork cells across the three views that share `ALL_COLUMNS`; Files had thumbnails, the editor and device views a "View" text button. Fixing it also removed a recycled-cell bug that opened the wrong image |
| `940899c` | Save was gated on the playlist living in Sparkamp's own directory, so it was insensitive for all 44 of this library's — with nothing saying why. Plus: no dirty indicator existed, Revert found its playlist by scanning the sidebar, and the manage page had no search |
| `eee4122` | five of the editor's mutation paths wrote the `.m3u8` immediately and two did not, so Revert could only undo half of an edit session |
| `f553463` | navigating to Playlists set the chevron's expanded flag without expanding anything, and `close_request` persisted it |
| `45e07d5` | the Files table was built twice on every window open — 474 ms of the 2.4 s |
| `d7e00db` | the Media Library window was rebuilt and **leaked** on every reopen: ~126 MB and a fresh pair of 2 s pollers per cycle |

The last two are worth separating from the rest. They were not visible as
bugs — the app worked — and neither would ever have been found by reading a
diff, because neither was in one. They were found by *measuring* the thing the
user complained about ("2–3 seconds", "toggling multiple times"), which is a
different discipline from reviewing a refactor and produced the two largest
wins of the step: open time 2.37 s → 1.42 s → 0.26 s on reopen, and a leak
that reached 1.6 GB in eight cycles.

**Take a measurement before believing a cause.** Step 6's lesson was to
measure seams before moving code; step 7 adds the runtime twin. Every
hypothesis this round that was reasoned rather than measured — including two
of mine about which code was at fault — was wrong.

### What step 8 actually did — 2026-08-11

All 15 `include!` slices became real `mod`s in two commits. The recipe was the
same for every file and took three lines of Python to apply:

1. `use super::*;` at the head of the file. A child module can see its
   ancestors' private items, so this reproduces exactly what the slice saw
   when it was spliced into `window/mod.rs`.
2. `pub(super)` on every top-level item, so the parent can re-export it.
3. `mod x; use x::*;` in `mod.rs`.

Step 3 is the part worth explaining. The re-export keeps the window module's
namespace identical to what it was, so the 232 names the page modules from
steps 2–7 already pull in through `use super::{…}` still resolve, and **not
one call site had to change**. The alternative — `pub(super)` in the child and
`super::x::foo` at every reader — would have rewritten every one of those
imports and each of their use sites, for the same compiled output.

What the compiler found that the recipe did not, all of it in the last four
files:

| Found | Why it only surfaced now |
|---|---|
| Struct **fields** (`MlColumnDef`, `MtpRaw`, `MtpMeta`, `PlaylistSendPlan`, `MlHost`, `MlCtx`, `AppState`×37, `ScanState`, `RgJobState`) | field privacy is per-struct, and `pub(super)` on the struct says nothing about them |
| `impl AppState`'s 22 bare methods | same reason |
| `thread_local!` statics in util.rs | `pub(super)` has to go *inside* the macro body |
| `use crate::skin::…` at the foot of state.rs | it was serving player.rs — invisible while both were one module |
| build()'s 38-line doc comment stranded at the end of state.rs | the 2026-07-11 byte cut fell between the doc and its `fn`; a real `mod` boundary made it a parse error |

That last one is the most useful thing the step turned up. The doc block had
been 4.5k lines away from `pub fn build` for a month, rendering on nothing.
`include!` cannot tell you that; a module boundary can.

**Cost:** 17,386 lines of code changed module, essentially all of it by
script; the hand-written part is the five rows in the table above plus the
comment rewrites. No behaviour touched. `window/mod.rs` is 244 lines —
declarations and module docs, nothing else. Tests: 825 pass.

One pre-existing failure was confirmed *not* to be ours by running the suite
on `9ccf5b3` first: `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow`
panics with "must start clear" under the parallel runner and passes alone —
a global refcount in `src/disc/detect.rs` shared with another test. Untouched;
it predates this step and is outside the plan.

### Step 8's test round — 2026-08-11

29 of the 49 checks were automated over AT-SPI and all 29 pass. The harness is
`scratchpad/step8_all.py`: three launches, one for the normal case, one with
the chevron collapsed, one restarting with the ML left open. It backs up and
restores `config.toml`, and never clicks Eject, Rip, Burn, Copy, Save or
Delete.

Covered: window start and restore, all five pages rendering distinct content,
search filter and restore, the status bar tracking selection, the playlist
sub-row route into the editor, the data-disc browser (a disc happened to be in
`sr0`), the attached USB device and its playlist chips, and the three step-7
regressions — reopen time, RSS across five open/close cycles, and chevron
persistence.

The other 20 are listed in the artifact handed to the user, grouped by what
blocks them: 8 need a real pointer, 4 need an audio CD, 3 write to or eject
hardware, and 5 are the cross-page holders, all of which start from a
right-click menu.

**What the round actually caught, none of it from the extraction:**

| Fix | What it was |
|---|---|
| `9e2c884` | Returning to Files from an album drill-down kept the album filter. My regression from `45e07d5`: the handler asked "was a filter set" when it needed "is this table showing one". |
| `c3fb2c7` | The watch drain refreshed the UI on every tick that saw an event; a bulk ingest made that ~95% main-loop occupancy. |
| `f367324` | **The freeze.** The drain applied an unbounded batch of events per tick, each a tag read off disk, synchronously on the GTK main thread. Now bounded to 100 ms of work per tick. |
| `487931e` | `&s[..30]` on a lyric split a multi-byte char. Inside a bind callback, which cannot unwind, so it aborted the process. |
| `ee80091` | `/mnt` vs `/var/mnt` stored the same file twice; 8,417 duplicate rows and climbing. This was the ingest driving the freeze. |

**Automation has a ceiling, and knowing where it is has value.** Three checks
were skipped on a wrong guess and later proved genuinely unreachable: gallery
tiles expose only `listitem.scroll-to`, ColumnView headers are a label with no
action, and `generate_keyboard_event` hangs under Wayland. Two more "failures"
were bugs in the probe, not the app — the sidebar was identified as "the
biggest list", which is the Files table whenever the chevron is collapsed, and
the device track view as "the biggest table", which is its playlist chips.
Both now match on content instead of size.

**And the freeze was found by measurement, not by reading.** Two hypotheses of
mine were wrong first — a probe-flood that turned out to be 34 flat threads at
18 ms round-trips, and a rebuild storm whose fix did not stop the hang. What
settled it was sampling `/proc` from a parent process while a child drove the
UI, so the record survived the app going unresponsive.

### What the manual round found — 2026-08-11

19 of the 20 passed. The one failure was X39; the other four defects were
found off the checklist entirely, which is the argument for a human round
rather than a longer script.

| Fix | What it was |
|---|---|
| `8f0f254` | **The one that mattered.** `is_read_only` answered "can I write this?" by opening the file for writing, and Linux emits `IN_CLOSE_WRITE` when that descriptor closes, written to or not. So the Files status column generated watch events for the rows it was inspecting; the watcher called them modifications and rebuilt the view; the rebuild rebound the rows, which probed more files. A closed loop that reset scroll and selection every 15-20 seconds with nothing touching the disk — fatal for building a multi-file selection. Now `access(W_OK)`, which is silent to inotify. Selection also survives a rebuild now, for the case a real file change triggers one. |
| `ef76652` | Syncing a device froze the window until every file had copied. The planning half was already off-thread; the applying half never was. Now a worker with its own SQLite connection, driving the detail view's progress bar. |
| `86292e0` | Ctrl+A did not select all in the playlist window (Ctrl+I inverted, so the gap was conspicuous); tracks added by path showed a blank duration the library already knew; and **X39** — cancelling a rescan only set the worker's flag, leaving `ml_scan` populated, so the stale totals stayed on screen and the next Rescan was silently refused. |
| `229a659` | Scrolling queued one status probe per distinct row swept over, and the app worked through that backlog at ~24% of a core for 20 s after scrolling stopped. Probes now wait 150 ms and re-check the row is still there. |

Also fixed during the round but properly a library bug, not a UI one:
`ee80091`, one file stored twice under `/mnt` and `/var/mnt` — 8,417 duplicate
rows, still growing, and the ingest driving all of the above. Its own plan is
at `2026-08-11-path-canonicalization.md`.

**Nothing here came from the module extraction.** Four predate the branch;
one (`9e2c884`) was mine, from the perf fix in step 7's round rather than from
step 8's conversion. That is the useful result: 17,386 lines moved between
modules, and every defect the round surfaced was already there.

One gap was found and deliberately not filled: **copy from a device to the
computer** ("Copy to Library ▸ &lt;folder&gt;") is in the June design spec
`a00041a` and was never built. Not a regression, and out of scope here.

### Handing off a test round

Manual testing needs the Linux box, so it batches to whenever the user is at
one — but the agent must not silently accumulate untested steps.

**When a step with a manual test lands green in CI, stop and hand the user
the exact checks for that step — the group letters, expanded into the actual
numbered items, not a pointer to this file.** Then wait. Do not start the
next step on the assumption the last one was fine; a holder that was never
wired compiles perfectly and fails silently, and stacking a second extraction
on top of it makes the bisect twice as expensive.

The one exception is the 3+4 pair, which may be handed off together — both are
low-risk, and Files exercises the sidebar anyway.

---

## 5. Smoke tests

Runtime verification cannot happen in CI: `cargo check` proves the code
compiles, not that the UI works. It is specifically blind to the three things
this refactor endangers — signals that no longer fire, `RefCell` borrows that
now overlap, and holders that were never populated. **A `None` holder is not
an error. Every call site silently does nothing.** There are 24 of them.

Each group is a couple of minutes. Run group A every time; add the group(s)
named for the step.

### A — always (any step)

1. `sparkamp` starts, no GTK-CRITICAL or borrow panic on stderr. (The GUI is
   the default invocation. There is no `--ui` flag — clap rejects it; `--tui`
   is the only mode flag. This document said `--ui` until 2026-08-09.)
2. Open the Media Library. All five sidebar entries are present: Files,
   Albums, Playlists, Disc Drives, Devices. (Five entries, six stack pages —
   Playlists nests `pl-manage` and `pl-edit`.)
3. Click each in turn — each shows its own page, none blank, no stderr noise.
4. Close and reopen the window from the toolbar button; it restores at the
   same size, still populated.
5. **Restart the app with the ML window left open at quit.** This is the
   second, easily-forgotten construction path (player.rs's session-restore
   call site, distinct from the toolbar button) — it must come back populated,
   not blank.

### B — Albums (step 2)

5. Gallery renders cover thumbnails, not placeholder squares.
6. Change the sort; order actually changes.
7. Change the zoom; tile size actually changes.
8. Click an album → its tracks open.

### C — Sidebar (step 3)

9. Playlists chevron expands and collapses; sub-rows appear and disappear.
10. Selecting a playlist sub-row opens that playlist in the editor.
11. Disc Drives and Devices headers list currently-attached hardware — and
    nothing when none is attached.

### D — Files (step 4)

12. Search filters rows; clearing it restores the full list.
13. Click a column header — sorts; click again — reverses.
14. Right-click a row: menu appears with the full item list, correct order.
15. Status bar shows the right counts, and updates with the selection.
16. Double-click a row → the track plays.

### E — Discs (step 5) — needs an audio CD and a data disc

17. Insert an audio CD → the track list populates without navigating away.
18. Right-click tracks → Enqueue, and → Replace Current Playlist.
19. Identify (gnudb) returns matches and the tag override sheet applies.
20. Rip Track(s) opens with only the selected tracks checked.
21. Insert a data disc → the file browser lists files with sizes.
22. Eject → the list clears promptly and the sidebar row disappears.

### F — Devices (step 6) — needs a USB device

23. Attach → it appears in the sidebar and in the overview cards within ~2 s.
24. Open it → its files list with full columns.
25. Copy files to it → the progress bar advances and completes.
26. Device playlist chips: create, rename, delete.
27. Eject → spinner, then the row disappears; no stale detail page.

### G — Playlists (step 7)

28. New playlist → appears in both the sidebar and the manage list.
29. Rename → both places update.
30. Editor: add files, remove a row, reorder.
31. **Revert** discards edits; the dirty indicator clears.
32. **Save** persists — reopen the playlist and the change is there.
33. Right-click a row → Remove from Playlist. The track survives in Files.
34. Delete the playlist → gone from the sidebar, and the file is gone from
    disk, but its *tracks* are still in the library (Deletion Rule).

### X — cross-page holders (steps 4–8)

The ones CI cannot see. Each spans two pages, so each is a holder an
extraction can leave unwired.

35. Files → select tracks → Send to ▸ Disc Drive → **the burn panel on that
    drive updates live**, without navigating to it first (`burn_refresh_holder`).
36. Eject a drive holding a burn queue → **the sidebar row disappears and the
    queue is dropped**, no leftover panel (`refresh_discs_holder`).
37. Save a playlist in the editor → **the sidebar sub-row label updates**.
38. Rescan Metadata in Files → **the status count updates** without a manual
    reload (`files_status_holder`).
39. Add a track to the active playlist from any ML surface → **the main
    player window's playlist reflects it immediately** (`rebuild_playlist`).

### If something fails

Report which numbered check, and what happened instead. A silent no-op points
at an unwired holder — grep the extracted module for the holder's name and
confirm something still calls `.replace(Some(...))` on it. A borrow panic
points at two `.borrow_mut()` calls whose relative order the move changed.

---

## 6. `player.rs` — worth doing, but later

> **Done 2026-08-11 as step 9.** All three cuts landed and `player.rs` is
> 3,426 lines. What follows is the survey the step was planned from; the
> close-out after it records what actually happened, including where the
> boundaries below turned out to be drawn in the wrong place.

5,754 lines, and again one function: `build()` at line 65 runs to the end.

| Region | Lines | Size |
|---|---|---|
| setup + state | 65–272 | 208 |
| now-playing row | 273–921 | 649 |
| playlist window | 922–1802 | 881 |
| playlist menu bar | 1803–1956 | 154 |
| drag + drop | 1957–3153 | 1,197 |
| misc handlers | 3154–4099 | 946 |
| draw + key handling | 4100–5753 | 1,654 |

**Verdict: yes, but after the Media Library, and it is a smaller job than the
line count suggests.** At ~66k tokens `player.rs` still fits in a context
window with room to work; `media_library.rs` at ~143k does not. That gap is
the whole argument for the ordering — one file is uncomfortable, the other is
genuinely unworkable.

The cuts are cleaner here because the regions are contiguous — no
widgets-first/wiring-last split to undo:

1. **`playlist_window.rs`** (922–1956, ~1,035 lines). A separate GTK window
   with its own header, button bar, TreeView and menu bar. The cleanest
   extraction in the file and the obvious first move.
2. **`dnd.rs`** (1957–3153, ~1,197). DragSource plus the external-file
   DropTarget. Self-contained behaviour over a widget it does not own.
3. **`keys.rs`** (the `EventControllerKey` closure inside 4100–5753). One
   large `match` on keyval. Extracts to `keys::install(&win, &ctx)`.
4. **`shortcut_sections()`** at line 7 — already a standalone fn, and already
   flagged as one of three hand-maintained shortcut tables that drifted this
   week. Moving it to core and having GTK, TUI and mac all render from it
   kills a whole bug class. **Do this one independently of the breakup** — it
   is small, it is core-side, and it is therefore compilable and testable on
   the Mac, unlike everything else in this document.

`build()` needs the same `MlCtx` treatment under a different name (`PlayerCtx`),
so doing the Media Library first means the pattern is proven before it is
applied to the window that everything else hangs off.

### What step 9 actually did — 2026-08-11

| | Before | After |
|---|---|---|
| `player.rs` | 5,831 | **3,426** |
| `build()` | 5,708 | **3,201** |
| new modules | — | `playlist_window.rs` 1,092 · `dnd.rs` 1,241 · `keys.rs` 435 |

`PlayerCtx` is 43 fields — the widgets, callbacks, holders and channels that
more than one of the three new modules reads. It is assembled once, after the
playlist window exists and before the first module needs it.

**The order in the survey above is wrong, and measuring is what showed it.**
Before touching anything, every fn-scope binding in `build()` was indexed
against every line that reads it, and each candidate region scored on two
numbers: how many bindings flow *in*, and how many declared inside it are
still read *after* it. The second number is the one that decides difficulty,
and it inverted the plan's ordering:

| Cut | Flows in | Escapes | Shape |
|---|---|---|---|
| `dnd.rs` | 35 | **0** | `install(&ctx)` — pure sink |
| `keys.rs` | 28 | **1** | `build(&ctx, …) -> Rc<dyn Fn(Key) -> Propagation>` |
| `playlist_window.rs` | 10 | **21** | `build(Deps) -> PlaylistWin` |

So drag-and-drop went first, not the playlist window. It reads a wide slice of
the window and writes nothing back that anything later reads, which makes it
the one cut that cannot break a caller — the right place to prove the
mechanism, exactly as Albums was in step 2.

Two boundaries in the survey were also drawn in the wrong place:

- **The playlist window has to take the menu bar with it** (the survey listed
  them as separate regions). Split, the menu bar's four `menu_button` calls
  and its Select/Sort handlers kept reaching back into the window's widgets.
  Together they are one 995-line unit that escapes 21 bindings instead of 23.
- **`keys.rs` is 378 lines, not the ~1,654 the survey implies.** That figure
  was "draw + key handling" lumped together; the `handle_key` `match` alone is
  a fifth of it. The visualizer draw code beside it is a separate job.

Everything that escapes comes back through `PlaylistWin`, which `build`
destructures into locals of the same names. That is what kept the cut honest:
not one line below the call site changed.

**Every moved body was diffed against the original and is byte-identical** —
the three modules re-alias what they need under the original names at the top
of the function, so the moved code reads exactly as it did in `build`. A
reviewer can check the claim mechanically rather than reading 2,400 lines.

**The plan's goal is still not met.** `player.rs` is the largest file in the
repo and `build()` is still one 3,201-line function — under the 800/300 bar
only by a factor of four. What is left splits along visible seams:

| Region | Lines | Escapes cleanly? |
|---|---|---|
| tick loop | ~640 | reads the whole window; needs `PlayerCtx` |
| jump window | ~450 | its own window, like the playlist one |
| add-file dialogs | ~260 | three handlers over one `FileFilter` |
| visualizer draw | ~100 | already calls module-level helpers |

Those four would take `build()` to roughly 800. They are not part of step 9
and have not been scored the way the three above were.

---

## 7. `sparkamp_bridge.h` — from discipline to tooling

Not part of the GTK breakup, but the same class of problem: a structure that
is currently correct only because someone keeps remembering to keep it so.

### Where it stands

`frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` is 1,115 hand-written
lines declaring **305 functions** against 305 `extern "C"` exports in
`src/ffi/`. Audited 2026-08-05: every exported symbol is declared, no
declaration is orphaned, and five sampled signatures matched their Rust
definitions exactly. Nothing is broken today.

The risk is what happens when it does break. A mismatched *name* fails at
link time, loudly. A mismatched *signature* — a swapped `int32_t`/`int64_t`,
a missing `const`, a `char*` where the Rust side returns a struct pointer —
compiles clean on both sides and corrupts the stack at runtime. There is no
compiler on either side of this boundary that can see both halves.

Every new FFI function needs two mirrored edits by hand. `refactor-plan.md`
already records this as a manual step ("Parity Task 4: hand-edit both bridge
headers"), and this session added five more getters the same way.

### The path

**Step A — a CI drift check (cheap, immediate).** A script that extracts
exported symbol names from `src/ffi/*.rs` and declared names from the header,
diffs them, and fails the build on any asymmetry. This is exactly the audit
run by hand above, so it is maybe 30 lines of shell and it can land today —
it needs no Rust changes and no macOS toolchain. It catches missing and
orphaned declarations, which is the common failure. It does **not** catch
signature drift.

**Step B — cbindgen generates the header.** Add `cbindgen` as a build
dependency with a `cbindgen.toml`, and have it emit the header from the Rust
source, so the signature is derived rather than restated. Then the two halves
cannot disagree, because there is only one half.

Two things make this less than a drop-in:

- The current header carries substantial hand-written documentation — the
  ownership contracts especially ("Free with `sparkamp_free_string`"), which
  are the part a caller most needs and the part cbindgen cannot invent. These
  must migrate to `///` doc comments on the Rust functions, which cbindgen
  will then carry through. That is the bulk of the work and it is a genuine
  improvement: the contract ends up next to the code that implements it.
- The opaque types (`SparkampCtx`, `SparkampNowPlaying`, `SparkampTag`) and
  the JSON-string conventions need `cbindgen.toml` rules so the generated
  header keeps the shape Swift already imports.

**Step C — commit the generated header and verify it in CI.** Keep the file
checked in, since the Xcode build reads it directly and should not depend on
a Rust build step. CI regenerates and fails if the committed copy differs.
That gives generation's safety without adding a build-order dependency
between Cargo and Xcode.

### Sequencing against the GTK work

Independent — different files, different language, no overlap. Step A is a
good warm-up for the CI muscle that step 0 of the GTK plan needs anyway, and
both are ubuntu-runner shell jobs. Step B is a focused day's work best done
when no large Swift change is in flight, since it rewrites the header
wholesale and would conflict with anything touching it.

---

## 8. Verification, every commit

1. CI `cargo check` on ubuntu — zero warnings.
2. `cargo test` on macOS — core + TUI unaffected by GTK moves (baseline
   720 bin / 622 lib; `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow`
   is a known parallel flake, green under `--test-threads=1`).
3. `git diff --stat` — a move should be ~1:1 adds to deletes. A large
   imbalance means something was rewritten, not moved.
4. Steps 3–7: manual pass on Linux before the next step starts.
