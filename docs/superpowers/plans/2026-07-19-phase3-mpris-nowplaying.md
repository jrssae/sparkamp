# Phase 3 — F6 MPRIS + mac Now Playing (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. Depends on phase 2's
> now-playing-changed core seam — do not build a second notification path.

**Goal:** OS-level media integration. Linux: MPRIS2 D-Bus service (media
keys, GNOME quick-settings, playerctl). mac: MPNowPlayingInfoCenter +
MPRemoteCommandCenter (media keys, Control Center, AirPods taps). Album art
in the OS widget comes free via the artwork path from phases 1-2.

## Architecture

- **Core:** phase 2's now-playing event (metadata + artwork path + playback
  state) plus a position query already served by the engine
  (`engine.rs` position/duration — verify the existing seek/position API).
  Add whatever thin core accessor MPRIS needs (e.g. `position_usecs()`),
  nothing D-Bus-specific in core.
- **Linux frontend:** implement in the GTK frontend process using
  **gio/glib D-Bus** (`gio::DBusConnection` — gtk4-rs re-exports gio; NO
  new crate dependency; zbus rejected to avoid a second async runtime).
  New module `frontends/gtk/window/mpris.rs` (or `frontends/gtk/mpris.rs`
  if it needs no window types): owns the bus name
  `org.mpris.MediaPlayer2.sparkamp`, exports `org.mpris.MediaPlayer2` +
  `org.mpris.MediaPlayer2.Player` interfaces via XML node info +
  method-call dispatch (gio's register_object pattern).
- Metadata map: `mpris:trackid` (path-derived object path), `mpris:length`
  (usecs), `mpris:artUrl` (`file://` + artwork_path when present),
  `xesam:title/artist/album/albumArtist/genre/trackNumber/contentCreated`.
  Empty fields omitted. Builder = pure fn → unit-testable (no bus needed).
- Commands → existing controller entry points (the same fns `handle_key`
  transport arms call): Play, Pause, PlayPause, Stop, Next, Previous,
  Seek(offset), SetPosition, plus properties PlaybackStatus, Position,
  CanPlay/CanPause/CanGoNext/CanGoPrevious/CanSeek (true), CanControl.
  Dispatch on the GTK main loop (glib::MainContext::invoke) — never touch
  AppState off-thread.
- `PropertiesChanged` signals on track change + play state change + seek
  (Seeked signal). Emit from the phase-2 subscription.
- **mac:** Swift-side only (BLIND): feed `MPNowPlayingInfoCenter.default()`
  (title/artist/album/artwork via NSImage from artworkPath/length/position/
  rate) on the model's published track/state changes;
  `MPRemoteCommandCenter` handlers → existing FFI transport calls
  (`sparkamp_play/pause/stop/next/prev`, seek — verify exact names in
  `src/ffi/playback.rs` + bridge.h; they exist for the transport buttons).
  Likely zero new FFI; if position pull is missing, add one accessor.
- TUI: out of surface (no session bus assumption) — capability note, skip.

## Tasks (boundaries for expansion)

1. Core: position accessor + any event-payload gaps (unit tests).
2. Metadata-map builder (pure; tests: full map, empty-field omission,
   artUrl only when art exists, length conversion).
3. gio D-Bus service skeleton: name ownership, root interface (Identity
   "Sparkamp", DesktopEntry — match the installed .desktop name from
   packaging/), Raise → present window, Quit → close.
4. Player interface: properties + methods wired to controller; Seeked +
   PropertiesChanged emission.
5. mac Now Playing + RemoteCommand (blind, checklist).
6. Gate + docs.

## Automated tests

- Metadata builder unit tests (above).
- Command dispatch: fake the method-call handler layer — table test that
  each MPRIS method name maps to the right controller call (extract the
  name→action match into a pure fn to make this testable without a bus).
- Property snapshot fn (PlaybackStatus string for each PlayerState;
  Position conversion).
- D-Bus itself: not unit-tested (needs a session bus) — covered manually;
  say so in the plan rather than faking it.

## Manual test plan

1. `playerctl -l` shows sparkamp; `playerctl metadata` full + correct;
   `playerctl play-pause/next/previous/stop/position 30` all act.
2. Keyboard media keys work with the window unfocused.
3. GNOME quick-settings widget: art, title, transport buttons live.
4. Art updates on track change; no-art track → widget without stale image.
5. Two players open (e.g. + a browser) — sparkamp coexists, no name clash.
6. Seek from the widget reflects in the app seek bar (Seeked signal).
mac checklist: Control Center card (art/title/position), media keys,
AirPods tap next/prev, scrub from Control Center.

## Performance / pitfalls

- Position property: MPRIS consumers poll — answer from the engine without
  taking RefCell borrows across the D-Bus callback re-entry (grab, copy,
  drop). Emit Seeked ONLY on real seeks (not every tick).
- Do not emit PropertiesChanged per second for Position — spec says
  consumers poll Position; only track/status changes signal.
- Name ownership failure (another instance) → log + degrade silently.
- gio object registration lifetimes: keep registration ids alive in the
  module struct; dropping them unexports silently.

## Open questions

1. DesktopEntry value — confirm the .desktop base name in packaging/
   (needed for the widget to show the right icon).
2. LoopStatus/Shuffle MPRIS properties: wire to repeat/shuffle now or omit
   (CanControl minimal)? Propose: wire them — cheap, playerctl-visible.
