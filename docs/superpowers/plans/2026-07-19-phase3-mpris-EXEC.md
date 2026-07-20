# Phase 3 — F6 MPRIS + mac Now Playing (execution plan)

Expanded from `2026-07-19-phase3-mpris-nowplaying.md`. Read the handoff +
that design doc first. Branch `album-art-improvements`. Base at start:
`7ae6ef6` (phase-2 pushed). Suite floor: 451 lib + 655 bin, 0 warnings.

## Resolved decisions (user, 2026-07-20)
- **DesktopEntry** = `dev.sparkamp.Sparkamp` (from `packaging/dev.sparkamp.Sparkamp.desktop`; MPRIS DesktopEntry value drops the `.desktop`).
- **Bus name** = `org.mpris.MediaPlayer2.sparkamp`. Identity = "Sparkamp".
- **LoopStatus + Shuffle**: WIRE both, read AND write (playerctl loop/shuffle → Sparkamp repeat/shuffle). LoopStatus map: `None`↔RepeatMode::Off, `Track`↔Song, `Playlist`↔Playlist.
- Linux D-Bus via **gio** (`gtk4::gio`, already imported in `frontends/gtk/window/mod.rs:38`) — NO new crate, no zbus.
- TUI: out of surface (no session-bus assumption) — skip, capability note.

## Reused seams (verified)
- Now-playing event: phase-2 `AppState::subscribe_now_playing` / `notify_now_playing` / `current_now_playing` (frontends/gtk/window/state.rs). Emit PropertiesChanged from a subscriber. `NowPlayingInfo` (src/now_playing.rs): tags, tech_line, artwork_path: Option<PathBuf>, play_count, last_played, wiki urls. NOTE: NowPlayingInfo has NO title/artist/album as separate fields — they're inside `tags` (curated pairs) OR re-read. For MPRIS metadata we need discrete artist/title/album/genre/trackNumber → build from `id3_editor::read_tag_fields(path)` (as build_now_playing_info does) OR extend the payload. DECISION at T2: add discrete fields to a small MPRIS metadata source rather than parsing the display tags.
- Engine: `Player::position() -> Option<Duration>`, `duration() -> Option<Duration>`, `seek(Duration)` (src/engine.rs:641/654/664). `PlayerState { Stopped, Playing, Paused }` (engine.rs:57).
- Transport: engine `play()/toggle_pause()/stop()` (engine.rs:553/564/588); `AppState::play_next()/play_prev()` (state.rs). Repeat: `config.playback.repeat_mode` (RepeatMode::Off/Song/Playlist). Shuffle: `shuffle_state` + `toggle_shuffle`.
- Current track path: `AppState.playlist.current().path`.
- FFI transport (mac): `sparkamp_play/pause/stop/seek(fraction)/get_state/toggle_shuffle` (src/ffi/playback.rs). Verify next/prev + a position pull exist; add one accessor if missing.

## Global rules
- Build/test ONLY in distrobox (`distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`). 0 warnings. GTK code only in bin target. New src/ modules → mod in BOTH lib.rs + main.rs.
- Borrow discipline: never hold AppState borrow across a D-Bus callback re-entry — grab/copy/drop. Dispatch commands on the GTK main loop (`glib::MainContext::default().invoke` or the callback already runs there).
- No push without fresh explicit ask.

## Tasks

### P3-T1 — Core position/state accessors (pure, tested)
Thin core accessors MPRIS needs, no D-Bus in core:
- `Player::position_usecs(&self) -> i64` (position().map(|d| d.as_micros() as i64).unwrap_or(0)) + `length_usecs`. Add to engine.rs.
- Confirm PlaybackStatus source (PlayerState) reachable. If a helper `mpris_playback_status(&PlayerState) -> &'static str` ("Playing"/"Paused"/"Stopped") is pure, put it in the mpris module (T4), not core.
- Tests: position_usecs conversion (Some/None→0).
Files: src/engine.rs (+tests). Small.

### P3-T2 — MPRIS metadata builder (pure fn, tested)
New `src/mpris_meta.rs` (core, UI-agnostic, no gio): a pure builder producing an ordered list of (key, typed-value) pairs for the MPRIS Metadata map from discrete track fields + length + artwork path. Input struct `MprisMeta { trackid_path: String, length_usecs: i64, art_path: Option<String>, title/artist/album/album_artist/genre: String, track_number: Option<i64> }`. Output a `Vec<(&'static str, MetaValue)>` where MetaValue is a small enum (Str/StrList/I64/ObjPath/ArtUrl) the gio layer converts to `glib::Variant`. Keys: mpris:trackid (object path from a sanitized path hash), mpris:length (i64 usecs), mpris:artUrl (`file://` + art_path, ONLY when art present), xesam:title/artist(list)/album/albumArtist(list)/genre(list)/trackNumber. OMIT empty fields.
Tests: full map; empty-field omission; artUrl only when art_path Some; length passthrough; trackid is a valid object path (starts `/`, ascii). `mod mpris_meta;` in lib.rs + main.rs.

### P3-T3 — Command/property pure mappers (tested)
In `src/mpris_meta.rs` (or a sibling): pure fns that make the bus layer table-testable without a session bus:
- `mpris_command_action(method: &str) -> Option<MprisAction>` where MprisAction enum = Play/Pause/PlayPause/Stop/Next/Previous/Seek(i64)/SetPosition(i64)/Raise/Quit. (Seek/SetPosition carry parsed args; for the name→action table test, a variant w/o arg or a separate arg-parse fn is fine.)
- `playback_status_str(&PlayerState) -> &'static str`.
- `loop_status_to_repeat(&str) -> Option<RepeatMode>` + `repeat_to_loop_status(RepeatMode) -> &'static str` (None/Track/Playlist).
Tests: every method name maps; unknown→None; loop-status round-trip; status strings.

### P3-T4 — gio D-Bus service skeleton (GTK)
New `frontends/gtk/mpris.rs` (or window/mpris.rs). Own bus name `org.mpris.MediaPlayer2.sparkamp` via `gio::bus_own_name`. Register the root `org.mpris.MediaPlayer2` interface via `gio::DBusNodeInfo::for_xml(INTROSPECTION_XML)` + `connection.register_object` with a method-call closure. Root props: Identity="Sparkamp", DesktopEntry="dev.sparkamp.Sparkamp", CanQuit=true, CanRaise=true, HasTrackList=false, SupportedUriSchemes/MimeTypes=empty arrays. Methods: Raise→present the main window; Quit→close app. KEEP registration ids + the owner-id alive in a struct stored on AppState (dropping unexports). Name-ownership failure → log + degrade (no panic). Wire `mpris::init(app, window, state)` from window build.
Files: new mpris module + mod wiring + a field on AppState for the guard. Gate = build (D-Bus itself is manual-tested; say so).

### P3-T5 — Player interface + signals (GTK)
Add `org.mpris.MediaPlayer2.Player` to the introspection + register. Properties (read): PlaybackStatus (playback_status_str), LoopStatus (repeat_to_loop_status), Shuffle (bool), Metadata (build via mpris_meta from current track), Position (position_usecs), Rate/MinRate/MaxRate=1.0, Volume, CanPlay/CanPause/CanGoNext/CanGoPrevious/CanSeek/CanControl=true. Writable: LoopStatus (loop_status_to_repeat → set repeat, update button), Shuffle (→ toggle to match), Volume. Methods → controller entry points (same fns handle_key transport arms call), dispatched on the main loop, short borrows: Play/Pause/PlayPause/Stop/Next/Previous/Seek(offset usecs → engine.seek relative)/SetPosition(trackid,usecs → seek absolute). Emit `PropertiesChanged` on: track change + play-state change + repeat/shuffle change (hook the phase-2 now-playing subscriber + the transport/repeat/shuffle handlers). Emit `Seeked` ONLY on real user seeks (not per tick). Do NOT signal Position per second (consumers poll).
Borrow-safety: copy needed data under a short borrow, drop, then reply/emit.
Files: mpris.rs (+ hooks in player.rs for repeat/shuffle/seek emission). Gate = build; manual test plan = the doc's playerctl list.

### P3-T6 — mac Now Playing + RemoteCommand (BLIND)
Swift only. Feed `MPNowPlayingInfoCenter.default().nowPlayingInfo` (title/artist/album, artwork via NSImage from artworkPath, duration, elapsed position, playbackRate) on the model's track/state changes (reuse the phase-2 `nowPlaying` published info + currentIndex/isPlaying). `MPRemoteCommandCenter` handlers → existing FFI transport (sparkamp_play/pause/stop/next/prev/seek; verify names in bridge.h; add a position-pull FFI accessor ONLY if missing). Register once at launch. Update on the same publishes T13 hooks. mac-pass-checklist Phase-3 section. Gate = Rust suite unchanged.

### P3-T7 — Phase close-out
Full gate; final whole-branch review (most-capable model) over the phase-3 diff; ONE fix subagent for findings + re-review; spec known-limitations; ledger; mac checklist; user manual-test list (playerctl walk + mac Control Center). Do NOT push.
