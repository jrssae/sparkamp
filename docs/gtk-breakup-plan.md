# GTK breakup plan — `media_library.rs` and `player.rs`

Branch: `code-breakup` · Written 2026-08-05 against `9ab2322`

Extends [refactor-plan.md](refactor-plan.md), whose Phase 2 named this work
and never started it. That document's goal still stands: **no source file
over ~800 lines, no function over ~300.** This one supplies the architecture
its Phase 2b/2c left as one-liners.

---

## 1. What we are actually up against

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

Two consequences:

1. A physical re-split into more `include!` files is provable offline
   (byte-identity) but buys us nothing architecturally — the namespace stays
   flat and the function stays whole.
2. The real work — extracting functions, choosing parameters, fixing
   visibility — **cannot be validated on this machine**. Doing it blind here
   would repeat the phase-12 lesson at ten times the scale.

### Unblock this first

Before any extraction, add a `cargo check` workflow on `ubuntu-latest` with
`libgtk-4-dev`. `refactor-plan.md:11` already suggested it; the difference
now is that the repo has working, trusted GitHub Actions, so it is a small
job. That gives compiler arbitration in ~2 minutes per push and turns the
whole plan from "blind" into "iterate through CI".

**This is step 0 and nothing else should start before it.** Without it the
plan's entire value — the compiler telling us which captures a given page
actually needs — is unavailable.

---

## 2. Your hypothesis, checked

You guessed each left-hand nav item gets its own file. That is close to
right, and the stack confirms the boundaries — `add_named` gives exactly six
destinations:

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

| # | Step | Lines | Risk | Gate |
|---|---|---|---|---|
| 0 | **CI `cargo check` on ubuntu + libgtk-4-dev** | ~40 | none | workflow green on unmodified tree |
| 1 | `MlCtx` + `RefreshHolder` introduced, nothing extracted yet — pass `&ctx` where the 10 params went | ~150 | low | CI |
| 2 | **`albums.rs`** — mechanism pilot | 116 | very low | CI |
| 3 | `sidebar.rs` | 500 | medium | CI |
| 4 | `files.rs` + `files_menu.rs` | 2,130 | medium | CI |
| 5 | `discs/` — reunite 2964–3978 with 9821–11813 into the existing `mod disc` | 3,008 | **high** | CI |
| 6 | `devices/` — reunite 598–2963 with 9408–9820 | 2,779 | **high** | CI |
| 7 | `playlists/` | 3,150 | high | CI |
| 8 | Convert the remaining `include!`s in `window/mod.rs` to real `mod`s | — | medium | CI |

**Step 2 is the pilot for a reason.** Albums is 116 contiguous lines that
already delegate to `album_gallery.rs`. It exercises the entire mechanism —
`MlCtx`, a real `mod`, `pub(super)` visibility, the stack hookup — at a size
where a mistake costs minutes. Do not skip it for something more impressive.

**Steps 5 and 6 are the dangerous ones** because they move code across ~7,000
lines of intervening statements, and closure capture depends on declaration
order. Split each into two commits: first hoist the late wiring up next to
its widgets *within* the existing function and confirm green; only then
extract the reunited block to its own file.

Runtime verification cannot happen in CI — `cargo check` proves it compiles,
not that the UI still works. Each of steps 3–7 needs a pass on the Linux box:
open the page, exercise its buttons, confirm the cross-page holders still
fire (add from Files → the burn panel updates; eject a drive → the sidebar
row disappears).

---

## 5. `player.rs` — worth doing, but later

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

---

## 6. `sparkamp_bridge.h` — from discipline to tooling

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

## 7. Verification, every commit

1. CI `cargo check` on ubuntu — zero warnings.
2. `cargo test` on macOS — core + TUI unaffected by GTK moves (baseline
   720 bin / 622 lib; `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow`
   is a known parallel flake, green under `--test-threads=1`).
3. `git diff --stat` — a move should be ~1:1 adds to deletes. A large
   imbalance means something was rewritten, not moved.
4. Steps 3–7: manual pass on Linux before the next step starts.
