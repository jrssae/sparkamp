# GTK Human Interface Guidelines improvements — design

Date: 2026-08-24
Branch: `gtk-hig-improvements`
Status: approved, ready for implementation planning

## Summary

An audit of the GTK frontend against the GNOME Human Interface Guidelines
found 64 points of divergence. Most of them are deliberate: Sparkamp is a
Winamp replica, and conformance is not the goal. This design picks the ten
that improve the app for real users without eroding the reason it exists —
accessibility, discoverability, packaging metadata, and one data-loss trap
in the terminal frontend.

The audit itself is not reproduced here. What follows is what we decided to
change, why, and what we deliberately left alone.

## Scope

GTK/Linux, plus a contained TUI shortcut fix. macOS is untouched; every
per-item parity gap is logged in `docs/mac-pass-checklist.md` for a later
session on a Mac, where Xcode can validate the result.

### Non-goals

Explicitly out of scope, having been considered and declined:

- Header bars, and folding the app's eleven top-level windows into one.
- Replacing the skin CSS system with libadwaita defaults.
- Making the main player window resizable.
- Migrating the two remaining `GtkTreeView`s (see "Deferred: item 11").

These represent roughly twenty of the sixty-four audit findings. Each one
would delete something the app is for.

## Decisions

Recorded 2026-08-24, in the order they were made:

1. **GTK/Linux only.** macOS keeps current behavior; gaps get logged, not
   implemented blind.
2. **Font units: `pt`, with re-baselined defaults.** Not `em` — see item 7.
3. **Toasts: libadwaita `AdwToastOverlay`**, with Adw's reach kept narrow and
   re-decided per item.
4. **Empty states: `AdwStatusPage`** — the second and last Adw widget.
5. **Notifications: unfocused-only, plus a Settings toggle.**
6. **Shortcut aliases: Ctrl+F, F1, Ctrl+?** added; **Ctrl+, replaces Ctrl+.**
   (not an alias — the old binding goes away).
7. **Accessibility: full sweep**, including list and table semantics.
8. **Mnemonics: playlist menu bar and Settings tabs** only.
9. **Screenshots: captured by the user**, wired up here.
10. **Item 11 (TreeView migration): deferred** to its own branch.
11. **TUI: `/` conflict fixed, Ctrl+F and F1 added.** No other TUI changes.

## Item 1 — accessible names

There are zero accessibility API calls in the 37,557 lines of
`frontends/gtk/`. Every icon-only control announces as "button" or as
nothing at all.

**Scope:** every `Button::from_icon_name` (transport ×5 at
`player.rs:966-970`, mode buttons at `player.rs:816-837`, `np_toggle` at
`player.rs:661`, and the per-view action buttons); the visualizer and
album-art `DrawingArea`s; the unlabelled `Scale`s (seek, volume, and the ten
EQ bands, which announce as bare numbers today); and row/cell semantics on
the four `ColumnView`s — `files.rs:257`, `playlists.rs:443`,
`devices_page.rs:476`, `disc_data.rs:95`.

Names use `update_property(&[gtk4::accessible::Property::Label(..)])`
(verified against the vendored gtk4 0.9 API, `vendor/gtk4/src/accessible.rs:110`).
Value-bearing
controls also set `ValueText` so the seek bar announces "1:23 of 4:05"
rather than "73".

**Known limitation.** The active playlist is a `GtkTreeView`
(`playlist_window.rs:272`), deprecated since GTK 4.10, whose accessible
implementation does not plumb cell-level names and roles the way
`ColumnView`'s factory-built rows do. It gets a widget-level name, row
count, and selection announcements; cell-level semantics are not reachable
without the migration deferred as item 11. This is the single largest gap
the branch knowingly leaves open, and it is the argument for doing item 11
later.

This item lands **last**, because it annotates widgets that items 10 and 12
introduce.

## Item 4 — metainfo completeness

`packaging/dev.sparkamp.Sparkamp.metainfo.xml` has no screenshots, no
developer, and no branding. GNOME Software and Flathub will not list an app
without screenshots, so this is the item that actually unblocks distribution.

Added:

- `<screenshots>` — five entries with captions (main player, playlist,
  media library, album gallery, settings), pointing at
  `https://raw.githubusercontent.com/jrssae/sparkamp/main/docs/screenshots/*.png`.
- `<developer id="dev.sparkamp">` with `<name>`.
- `<branding>` colors, sampled from the app icon.
- `<requires><display_length>` and `<recommends><control>` so the store
  reports form-factor support honestly. Given the fixed 384px-wide player,
  `pointer` and `keyboard` are recommended and no touch claim is made.

**Dependency:** the PNGs do not exist. A new `docs/screenshots/` directory
is created with a `README.md` naming the five expected files and the
capture conventions (default Dark skin, no personal music metadata
visible). The user captures them during the interactive pass. The metainfo
references them by name from the start, so the block is complete the moment
the files land.

## Item 5 — third-party trademark

"Winamp" appears in the metainfo `<summary>`, the `.desktop` `Comment`, and
the `.desktop` `Keywords`. It is a third-party trademark in shipped
metadata: Flathub review flags it, and it carries legal exposure for no
functional gain.

- Summary and Comment: "Winamp-style audio player for Linux" becomes
  "Classic-style audio player".
- `Keywords`: `winamp` removed; `music;audio;player;mp3;` retained.
- The `.desktop` Comment also drops "for Linux" — HIG writing style says
  not to name the platform back at the user.

Prose in `README.md` that describes the app as *inspired by* Winamp is
descriptive use and stays. This item changes shipped metadata only.

## Item 7 — font sizes scale with the system

`src/skin.rs` writes `font-size: {n}px` in roughly twenty-five CSS rules.
GTK does not scale `px`, so GNOME's large-text accessibility setting has no
effect on any Sparkamp text.

**Mechanism: `pt`, not `em`.** `em` resolves against the theme font — on
GNOME, Cantarell 11 (~14.7px) — so emitting `1.0em` for a 12px skin
variable would render about 22% larger than today, and the rendered size
would vary with whatever font the user's distribution sets. That breaks the
skin guide's promise that one `.css` file "drives Sparkamp's appearance
identically on Linux and macOS". `pt` is converted through `gtk-xft-dpi`,
which is exactly what the text-scaling factor multiplies, so it scales
correctly while staying deterministic.

`render_gtk_css` emits `{n * 0.75}pt` wherever it emits `{n}px` today
(px x 0.75 at the CSS 96dpi reference). The public 14-variable skin format
is **byte-identical**: `parse_px` still reads `--sp-font-size: 12px`, and a
custom skin's declared numbers render exactly as they do now.

**Re-baselined defaults.** With sizes finally scaling, the built-in
defaults are worth revisiting — the existing 12px base renders at 9.75pt,
which reads small on a modern display and sits well under GNOME's native
Cantarell 11.

Decided 2026-08-24: land on native 11pt. Integer px values are used so the
templates stay pleasant to hand-edit; 11.25pt is within 2% of native 11pt,
which is imperceptible, and the large size falls exactly on 30pt.

| Variable | Now | New | As pt |
|---|---|---|---|
| `--sp-font-size` | 12px | 15px | 11.25pt |
| `--sp-font-size-marquee` | 14px | 18px | 13.5pt |
| `--sp-font-size-large` | 32px | 40px | 30pt |

A ~22% increase. The two built-in skins shift deliberately; every custom
skin keeps its own declared sizes, because `load_skin` (`skin.rs:404`)
gives a user file priority over the built-in.

**Four places carry these numbers and must stay in sync:**
`SkinVars::dark_defaults` (`skin.rs:172-175`) and `light_defaults`
(`skin.rs:194-197`), which are what the built-in skins actually render, and
`skin_templates/dark.css:27-29` and `light.css:24-26`, which are what
**Download skin…** exports. A test pins all four together.

Because the player window is `resizable(false)` and carries no hard
max-width, GTK sizes it to its natural content width, so larger text grows
the window rather than clipping. This is worth confirming during the
interactive pass at both default and large-text settings.

`src/skin_templates/skin-guide.md` gains a short section explaining that
font sizes are declared in px but rendered relative to the desktop's text
size.

## Item 8 — standard shortcut aliases

All additive except one. Ctrl-combinations go in the modifier-aware
wrappers, since `keys.rs`'s `handle_key` is modifier-blind by design.

| Binding | Action | Note |
|---|---|---|
| Ctrl+F | Jump / search | Alias for `j`. Also wired inside the Media Library window, where `j` does nothing today |
| F1 | Keyboard shortcuts | Alias for `i`. Unbound today. Sparkamp has no help manual, so the shortcuts window is the honest target |
| Ctrl+? | Keyboard shortcuts | Alias for `i`. Arrives as Ctrl+Shift+slash |
| Ctrl+, | Settings | **Replaces Ctrl+.**, which is removed |

Ctrl+, is a behavior change, not an addition — it goes in the release
notes. It also happens to align GTK with the macOS frontend's existing
Cmd+, so it reduces cross-platform divergence rather than adding to it.

`shortcut_sections()` at `player.rs:9` is updated; the
`shortcut_dialog_lists_every_phase6_key` drift guard must stay green.

**Not done:** Ctrl+Q and Esc keep their current bindings. Both were flagged
in the audit as genuinely wrong (Ctrl+Q enqueues instead of quitting; Esc
quits the app outright) but they were not selected for this branch.

## Item 9 — track-change notification

The GTK frontend sends no desktop notifications. MPRIS already publishes
metadata, so GNOME Shell shows Sparkamp in its media section; what is
missing is the transient banner on track change.

One more `subscribe_now_playing` subscriber (`state.rs:831`) builds a
`gio::Notification` with the track title as heading, artist as body, and
the cached album art as icon, sent via `app.send_notification`.

**Fires only when no Sparkamp window is active** — checked across the open
windows via `is_active()`. A banner over the player you are already looking
at is the reason people disable music notifications.

`config.playback.notify_track_change: bool`, default `true`, with
`#[serde(default)]` per house rule, plus a checkbox on the Settings
Behavior tab.

No new Flatpak permission: `--talk-name=org.freedesktop.portal.Desktop` is
already granted.

## Item 10 — toasts for non-fatal feedback

Seventeen `AlertDialog` call sites stop the user for things that do not
warrant stopping. The codebase already has the right instinct — `util.rs`
documents a house rule, "G3: no success modals anywhere", routing success
to a quiet status label — but failures still go modal.

**libadwaita `AdwToastOverlay`.** The dependency is added under the
existing `cfg(target_os = "linux")` block, so macOS is unaffected;
`org.gnome.Platform 47` already ships Adw, so there is no runtime change.

The skin CSS loads at `STYLE_PROVIDER_PRIORITY_APPLICATION`
(`player.rs:218`), which outranks the theme stylesheet — that is how it
overrides stock Adwaita today — so Adw's stylesheet loses on every selector
the skin covers. Residual drift is limited to widgets the skin does not
style, and is an interactive-pass check item.

`adw::init()` is called once in `frontends/gtk/mod.rs` before
`window::build`. The application stays a `gtk4::Application`; Adw widgets
work without `AdwApplication` provided `init()` has run.

Each window root is wrapped in an `AdwToastOverlay`, with a `util.rs`
helper alongside the existing `show_alert_parented` and
`show_playlist_save_error`. Demoted to toasts: playlist save failures,
unreadable-file reports, artwork decode failures, and similar recoverable
errors. Destructive confirmations and genuinely fatal errors stay modal.

**Adw's reach stays narrow**: `ToastOverlay` here, `StatusPage` in item 12,
nothing else without a fresh decision.

## Item 12 — empty states

Blank panes today. `AdwStatusPage` — the second and last Adw widget —
behind a small `util.rs` helper so copy and icon are the only per-site
variation.

| View | Copy |
|---|---|
| Active playlist, no tracks | "No tracks in the playlist" / "Press `n` to add files, or drag music here" |
| Media Library Files, no folders indexed | "No music folders" / points at Settings, with an Add Folder action button |
| No search results — Files, Albums, Playlists, Devices, Discs | "No results for <query>" — one helper, five call sites |
| Albums / Playlists / Devices empty | Per-view heading and description |

The active-playlist state doubles as onboarding: it is the first thing a
new user sees.

**Risk:** `AdwStatusPage` carries Adw's own typography and color and does
not read the skin variables, so it will look like a stock-GNOME panel
inside a skinned window. This is the most visible drift the branch
introduces, and it is the specific thing to judge during the interactive
pass. If it reads badly, the fallback is a hand-built skin-styled box
behind the same helper signature — a contained change.

## Item 13 — mnemonics

Zero mnemonics in the tree: `use_underline` and `with_mnemonic` appear
nowhere. The Winamp-style playlist menu bar is mouse-only.

- `playlist_window.rs:1054-1167` — `_Add`, `_Select`, `S_ort`, `_List`,
  via `use_underline(true)` on the `MenuButton` labels built by
  `menu_button()` at line 161.
- `settings.rs` — the notebook tab labels.

No visual change except an underline while Alt is held. Access keys are
deconflicted within each container.

## Item 14 — TUI shortcut consistency

**Framing:** this is terminal convention and internal consistency, not HIG.
The HIG governs GTK desktop apps, not terminal apps, and the TUI already
speaks the correct idiom for its platform — `j`/`k` navigation, `/` for
search, `q`, `[`/`]`, `o` for an ops popup. Importing GNOME shortcuts into
it would make it less conformant to its own platform, not more.

Two of the four GTK aliases are impossible in a terminal and are not
attempted: Ctrl+? *is* `0x7F`, the DEL byte, indistinguishable from
Backspace without the kitty keyboard protocol; Ctrl+, has no ASCII
encoding at all. Both would misfire on most terminals.

| Change | Where | Why |
|---|---|---|
| `/` playlist-clear moves into the `o` ops popup as **Remove All** | `keys.rs:499`, `ui/overlays.rs` | `/` currently **clears the entire playlist** in the playlist view while meaning **search** in the Media Library (`media_library/mod.rs:331`). Same app, same key, one destructive. Mirrors GTK's `List ▾ ▸ Remove All` |
| `/` and Ctrl+F open jump/search in the playlist view | `keys.rs` | Extends the pattern the Media Library already has, rather than importing one |
| F1 opens the help overlay, alongside `i` | `keys.rs` | No F-keys bound today. `i` still works if a multiplexer intercepts F1 |
| Help overlay text updated | `ui/overlays.rs` | The TUI keeps its own shortcut list, separate from GTK's `shortcut_sections()` |

The `/` fix is the highest-value change in this item: it removes a
data-loss trap bound to the key every terminal user reflexively presses to
search.

## Deferred: item 11 — TreeView migration

Not in this branch. Recorded here because item 1 depends on the outcome.

The audit's "TreeView x33" was a count of mentions, not instances. There
are **two** actual `TreeView`s left:

| Site | References | Character |
|---|---|---|
| `dedupe.rs:88` | self-contained | One file, one column set, no drag-and-drop |
| `playlist_window.rs:272` | **92 across 3 files**, 47 of them in `dnd.rs` (1,359 lines) | The active playlist |

Four of six list surfaces are already `ColumnView`, so the migration
pattern exists in-tree; this is the tail, not the bulk.

It is deferred because the playlist's multi-select drag-reorder was hard
won — selection preserved across GTK's transient collapse, drop position
honoring before/after halves, all dragged rows reordered — and a port
rewrites that against a different selection model. That deserves its own
branch with its own test coverage, not a ride-along with ten unrelated
items.

The cost of deferring is stated in item 1: cell-level accessibility on the
app's most-used list stays out of reach until it happens.

## Cross-cutting concerns

### Dependency and vendoring

`libadwaita` 0.7 (the release paired with gtk4 0.9) joins `Cargo.toml` under `[target.'cfg(target_os = "linux")'.dependencies]`.

The Flatpak builds offline from a vendored tree: 302 crates, 476 MB, with
`packaging/cargo-sources.json` generated alongside. Both must be
regenerated, the same procedure followed for `notify` in phase 8. This is a
real work item, not a footnote, and it gates items 10 and 12.

### Testing

Per `CLAUDE.md`: `cargo build && cargo test`, zero warnings, run inside the
distrobox `dev-box` — never on the host.

New coverage:

- `skin.rs`: emitted CSS carries `pt`, conversion is exactly `x 0.75`, and
  no `px` font size survives; a test pinning the re-baselined defaults
  across all four sync points — both `defaults()` functions and both
  `.css` templates — so an edit to one that misses the others fails.
- `shortcut_sections()` drift guard extended to the four new aliases, and
  asserting Ctrl+. is gone.
- `Config` round-trip for `notify_track_change`, including absent-field
  default.
- TUI: `/` no longer clears the playlist; `/` and Ctrl+F enter search mode;
  F1 opens help; Remove All reachable from the ops popup.

Toast, `StatusPage`, and accessibility work is GTK callback surface, which
`CLAUDE.md` exempts from unit testing. It is covered by the interactive
pass instead.

### Order of work

Dependency first, accessibility last, because item 1 annotates widgets that
items 10 and 12 create.

1. libadwaita + vendor regeneration
2. Item 7 — skin.rs px to pt, re-baselined defaults
3. Items 5 and 4 — packaging metadata
4. Items 8 and 13 — shortcuts and mnemonics
5. Item 14 — TUI
6. Item 9 — notification
7. Item 10 — toasts
8. Item 12 — empty states
9. Item 1 — accessibility sweep

### Version

No version bump in this branch. Per house rule the release version is
confirmed with the user before any bump, and a metainfo `<release>` entry
plus `scripts/pre-release-check.sh` follow at release time, not here.

## Risks

1. **Adw stylesheet drift** on widgets the skin does not cover. Judged
   during the interactive pass. Reversible for toasts (hand-rolled pill
   behind the same helper); reversible for empty states (skin-styled box
   behind the same helper).
2. **`AdwStatusPage` will not read the skin.** The most visible drift the
   branch introduces. See item 12.
3. **Active-playlist accessibility is capped** by `GtkTreeView`. Documented
   limitation, not a defect to fix here.
4. **Built-in skins get ~22% larger text** by design, via the re-baseline
   to native 11pt. Custom skins do not move, since a user file wins over
   the built-in. The most user-visible change in the branch: it belongs in
   the release notes, and the player window must be checked for reflow at
   both default and large-text settings.
5. **Ctrl+. is removed.** A behavior change for existing users; release
   notes must say so.
6. **Vendor regeneration** is the step most likely to consume unplanned
   time, and it blocks two items.

## macOS parity gaps deferred

Logged in `docs/mac-pass-checklist.md` for a session on a Mac:

| Item | Gap |
|---|---|
| 1 | No `accessibilityLabel` sweep on the SwiftUI frontend |
| 7 | macOS reads the same skin variables but applies them independently; whether it honors Dynamic Type is unverified |
| 8 | Cmd+, already matches; F1 and Cmd+F aliases absent |
| 9 | No `UNUserNotificationCenter` track-change notification |
| 10 | No toast layer; errors are alerts |
| 12 | No empty states (`ContentUnavailableView` is the natural fit) |
| 13 | Not applicable — macOS menus carry their own key equivalents |

Items 4, 5, 11, and 14 are Linux-only or not applicable.

## Open for review

1. Screenshot filenames and captions, once `docs/screenshots/README.md`
   names them.

The font re-baseline, previously open here, was settled on 2026-08-24 —
see item 7.
