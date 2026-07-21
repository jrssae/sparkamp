//! MPRIS2 D-Bus media integration (Linux) via `gio` — no extra crate, no
//! second async runtime. This module owns the well-known bus name
//! `org.mpris.MediaPlayer2.sparkamp` and exports the **root**
//! `org.mpris.MediaPlayer2` interface (Identity / DesktopEntry / Raise / Quit).
//! The `org.mpris.MediaPlayer2.Player` interface + `PropertiesChanged`/`Seeked`
//! signals are added in a later task off the same [`gio::DBusConnection`].
//!
//! D-Bus needs a live session bus, so this is verified manually
//! (`playerctl` / `busctl`) rather than in the unit suite; the pure
//! metadata/command mappers it will consume live in `src/mpris_meta.rs` and
//! ARE unit-tested.

use gtk4::gio;
use gtk4::glib;
use gtk4::glib::prelude::*;
use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use super::AppState;
use crate::engine::PlayerState;
use crate::mpris_meta::{
    build_metadata, mpris_command_action, playback_status_str, repeat_to_loop_status, MetaValue,
    MprisAction, MprisMeta,
};

/// Root `org.mpris.MediaPlayer2` introspection. The Player interface XML lives
/// separately and is registered in P3-T5.
const ROOT_XML: &str = r#"<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="Identity" type="s" access="read"/>
    <property name="DesktopEntry" type="s" access="read"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
</node>"#;

/// `org.mpris.MediaPlayer2.Player` introspection.
const PLAYER_XML: &str = r#"<node>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek"><arg name="Offset" type="x" direction="in"/></method>
    <method name="SetPosition">
      <arg name="TrackId" type="o" direction="in"/>
      <arg name="Position" type="x" direction="in"/>
    </method>
    <signal name="Seeked"><arg name="Position" type="x"/></signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="LoopStatus" type="s" access="readwrite"/>
    <property name="Rate" type="d" access="readwrite"/>
    <property name="Shuffle" type="b" access="readwrite"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>"#;

const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

/// Cached MPRIS Player state, compared each poll to decide which
/// `PropertiesChanged` to emit, and read by the property handlers so a
/// Position/Metadata poll never does disk I/O.
struct PlayerCache {
    /// Current track path — a change triggers a Metadata rebuild.
    path: Option<String>,
    status: String,
    loop_status: String,
    shuffle: bool,
    volume: f64,
    metadata: glib::Variant,
    /// `mpris:length` baked into the cached `metadata`. GStreamer resolves the
    /// duration ~50–300ms AFTER play starts, so the track-change rebuild often
    /// captures 0; when this is still <= 0 but the engine now reports a real
    /// length, the poll rebuilds Metadata so the widget scrubber gets a total.
    meta_length: i64,
}

/// Parks the MPRIS bus-name owner id, object registration ids, and the live
/// connection on AppState. GLib owns the registered closures for the process
/// lifetime, and `OwnerId`/`RegistrationId` have no `Drop`, so this is not
/// load-bearing for keeping the service exported — it exists to hold the
/// connection (the Player interface / signal emission hang off it) and to keep
/// the option of an explicit unown/unregister on quit.
/// `#[allow(dead_code)]` — most fields are held, not read.
#[allow(dead_code)]
pub(super) struct MprisGuard {
    owner: gio::OwnerId,
    /// The session-bus connection, filled once the name is acquired. P3-T5's
    /// Player registration + signal emission hang off this.
    pub(super) conn: Rc<RefCell<Option<gio::DBusConnection>>>,
    /// Root + Player object registration ids, filled on bus-acquired.
    root_reg: Rc<RefCell<Option<gio::RegistrationId>>>,
    player_reg: Rc<RefCell<Option<gio::RegistrationId>>>,
}

/// Stand up the MPRIS service. Safe to call once during window build; on any
/// failure (no session bus, name already owned by another instance) it logs
/// and leaves media integration disabled — never panics.
pub(super) fn init(
    app: &gtk4::Application,
    window: &gtk4::ApplicationWindow,
    state: Rc<RefCell<AppState>>,
) {
    let conn_slot: Rc<RefCell<Option<gio::DBusConnection>>> = Rc::new(RefCell::new(None));
    let reg_slot: Rc<RefCell<Option<gio::RegistrationId>>> = Rc::new(RefCell::new(None));
    let player_reg_slot: Rc<RefCell<Option<gio::RegistrationId>>> = Rc::new(RefCell::new(None));
    let cache = Rc::new(RefCell::new(PlayerCache {
        path: None,
        status: "Stopped".to_string(),
        loop_status: "None".to_string(),
        shuffle: false,
        volume: state.borrow().config.playback.volume,
        metadata: glib::VariantDict::new(None).end(),
        meta_length: 0,
    }));

    let owner = gio::bus_own_name(
        gio::BusType::Session,
        "org.mpris.MediaPlayer2.sparkamp",
        gio::BusNameOwnerFlags::NONE,
        // bus-acquired: register the root + Player objects, stash the
        // connection, and start the change-poll.
        {
            let app = app.clone();
            let window = window.clone();
            let state = state.clone();
            let conn_slot = conn_slot.clone();
            let reg_slot = reg_slot.clone();
            let player_reg_slot = player_reg_slot.clone();
            let cache = cache.clone();
            move |conn, _name| {
                register_root(&conn, &app, &window, &reg_slot);
                register_player(&conn, &state, &cache, &player_reg_slot);
                *conn_slot.borrow_mut() = Some(conn.clone());
                start_poll(conn, state.clone(), cache.clone());
            }
        },
        // name-acquired: nothing extra to do.
        |_conn, _name| {},
        // name-lost: another instance owns the name, or no bus — degrade.
        |_conn, name| {
            eprintln!(
                "MPRIS: could not own bus name '{name}' \
                 (another instance, or no session bus); media integration disabled"
            );
        },
    );

    state.borrow_mut().mpris_guard = Some(MprisGuard {
        owner,
        conn: conn_slot,
        root_reg: reg_slot,
        player_reg: player_reg_slot,
    });
}

/// Register the root `org.mpris.MediaPlayer2` object on `conn`.
fn register_root(
    conn: &gio::DBusConnection,
    app: &gtk4::Application,
    window: &gtk4::ApplicationWindow,
    reg_slot: &Rc<RefCell<Option<gio::RegistrationId>>>,
) {
    let node = match gio::DBusNodeInfo::for_xml(ROOT_XML) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("MPRIS: invalid root introspection XML: {e}");
            return;
        }
    };
    let Some(iface) = node.lookup_interface("org.mpris.MediaPlayer2") else {
        eprintln!("MPRIS: root interface missing from introspection XML");
        return;
    };

    let reg = conn
        .register_object("/org/mpris/MediaPlayer2", &iface)
        .method_call({
            let app = app.clone();
            let window = window.clone();
            move |_conn, _sender, _path, _iface, method, _params, invocation| {
                // Raise/Quit touch only GTK objects — no AppState borrow.
                match method {
                    "Raise" => window.present(),
                    "Quit" => app.quit(),
                    _ => {}
                }
                invocation.return_value(None);
            }
        })
        .property(|_conn, _sender, _path, _iface, prop| match prop {
            "Identity" => "Sparkamp".to_variant(),
            "DesktopEntry" => "dev.sparkamp.Sparkamp".to_variant(),
            "CanQuit" => true.to_variant(),
            "CanRaise" => true.to_variant(),
            "HasTrackList" => false.to_variant(),
            "SupportedUriSchemes" => Vec::<String>::new().to_variant(),
            "SupportedMimeTypes" => Vec::<String>::new().to_variant(),
            // Unknown property — should never be queried; return an empty
            // string rather than panicking.
            _ => String::new().to_variant(),
        })
        .build();

    match reg {
        Ok(id) => *reg_slot.borrow_mut() = Some(id),
        Err(e) => eprintln!("MPRIS: failed to register root object: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Player interface (org.mpris.MediaPlayer2.Player)
// ---------------------------------------------------------------------------

/// Register the Player object: transport methods + properties (incl. writable
/// LoopStatus / Shuffle / Volume). All handlers run on the GTK main loop
/// (gio dispatches D-Bus there), so AppState borrows are safe as long as they
/// are dropped before any callback that itself borrows state.
fn register_player(
    conn: &gio::DBusConnection,
    state: &Rc<RefCell<AppState>>,
    cache: &Rc<RefCell<PlayerCache>>,
    reg_slot: &Rc<RefCell<Option<gio::RegistrationId>>>,
) {
    let node = match gio::DBusNodeInfo::for_xml(PLAYER_XML) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("MPRIS: invalid Player introspection XML: {e}");
            return;
        }
    };
    let Some(iface) = node.lookup_interface(PLAYER_IFACE) else {
        eprintln!("MPRIS: Player interface missing from introspection XML");
        return;
    };

    let reg = conn
        .register_object(OBJECT_PATH, &iface)
        .method_call({
            let state = state.clone();
            let conn = conn.clone();
            move |_c, _sender, _path, _iface, method, params, invocation| {
                dispatch_method(&state, &conn, method, &params);
                invocation.return_value(None);
            }
        })
        .property({
            let state = state.clone();
            let cache = cache.clone();
            move |_c, _sender, _path, _iface, prop| get_player_property(&state, &cache, prop)
        })
        .set_property({
            let state = state.clone();
            move |_c, _sender, _path, _iface, prop, value| {
                set_player_property(&state, prop, &value)
            }
        })
        .build();

    match reg {
        Ok(id) => *reg_slot.borrow_mut() = Some(id),
        Err(e) => eprintln!("MPRIS: failed to register Player object: {e}"),
    }
}

/// Dispatch an MPRIS Player method to the controller. Borrows are kept short;
/// callbacks that re-borrow state (play_and_update) run after the borrow drops.
fn dispatch_method(
    state: &Rc<RefCell<AppState>>,
    conn: &gio::DBusConnection,
    method: &str,
    params: &glib::Variant,
) {
    let Some(action) = mpris_command_action(method) else {
        return;
    };
    match action {
        MprisAction::Play => {
            let (is_stopped, cb) = {
                let s = state.borrow();
                (
                    matches!(s.player.state(), PlayerState::Stopped),
                    s.play_and_update_callback.clone(),
                )
            };
            if is_stopped {
                if let Some(cb) = cb {
                    cb();
                }
            } else {
                let _ = state.borrow_mut().player.play();
            }
        }
        MprisAction::Pause => {
            let playing = matches!(state.borrow().player.state(), PlayerState::Playing);
            if playing {
                let _ = state.borrow_mut().player.toggle_pause();
            }
        }
        MprisAction::PlayPause => {
            let (is_stopped, cb) = {
                let s = state.borrow();
                (
                    matches!(s.player.state(), PlayerState::Stopped),
                    s.play_and_update_callback.clone(),
                )
            };
            if is_stopped {
                if let Some(cb) = cb {
                    cb();
                }
            } else {
                let _ = state.borrow_mut().player.toggle_pause();
            }
        }
        MprisAction::Stop => {
            let _ = state.borrow_mut().player.stop();
        }
        MprisAction::Next => {
            // The 100ms tick loop's now-playing choke point + marquee render
            // pick up the track change (same as the GTK Next button path).
            let _ = state.borrow_mut().play_next();
        }
        MprisAction::Previous => {
            let _ = state.borrow_mut().play_prev();
        }
        MprisAction::Seek(_) => {
            // Player.Seek(x): relative µs offset.
            let offset = params.child_value(0).get::<i64>().unwrap_or(0);
            let target = {
                let mut s = state.borrow_mut();
                let cur = s.player.position_usecs();
                let target = (cur + offset).max(0);
                let _ = s.player.seek(Duration::from_micros(target as u64));
                target
            };
            emit_seeked(conn, target);
        }
        MprisAction::SetPosition(_) => {
            // Player.SetPosition(o, x): absolute µs (arg index 1).
            let pos = params.child_value(1).get::<i64>().unwrap_or(0).max(0);
            {
                let mut s = state.borrow_mut();
                let _ = s.player.seek(Duration::from_micros(pos as u64));
            }
            emit_seeked(conn, pos);
        }
        MprisAction::Raise | MprisAction::Quit => {} // root interface handles these
    }
}

/// Read a Player property into a `glib::Variant`. Metadata comes from the cache
/// (rebuilt only on track change) so a Position/Metadata poll never does I/O.
fn get_player_property(
    state: &Rc<RefCell<AppState>>,
    cache: &Rc<RefCell<PlayerCache>>,
    prop: &str,
) -> glib::Variant {
    match prop {
        "PlaybackStatus" => playback_status_str(state.borrow().player.state()).to_variant(),
        "LoopStatus" => {
            repeat_to_loop_status(state.borrow().config.playback.repeat_mode).to_variant()
        }
        "Shuffle" => state.borrow().shuffle_state.enabled.to_variant(),
        "Metadata" => cache.borrow().metadata.clone(),
        "Position" => state.borrow().player.position_usecs().to_variant(),
        "Volume" => state.borrow().config.playback.volume.to_variant(),
        "Rate" | "MinimumRate" | "MaximumRate" => 1.0f64.to_variant(),
        "CanGoNext" | "CanGoPrevious" | "CanPlay" | "CanPause" | "CanSeek" | "CanControl" => {
            true.to_variant()
        }
        _ => String::new().to_variant(),
    }
}

/// Write a settable Player property. Returns true on success. NOTE: this
/// updates the engine/config directly; the GTK repeat/shuffle/volume widgets
/// do not re-render from a D-Bus set (accepted limitation — behavior is
/// correct, only the on-screen control lags until the user touches it).
fn set_player_property(state: &Rc<RefCell<AppState>>, prop: &str, value: &glib::Variant) -> bool {
    match prop {
        "LoopStatus" => {
            if let Some(s) = value.get::<String>() {
                if let Some(mode) = crate::mpris_meta::loop_status_to_repeat(&s) {
                    state.borrow_mut().config.playback.repeat_mode = mode;
                    // Persist — a D-Bus-only change would otherwise be lost on
                    // restart (the GTK toggles save; this path must too).
                    let _ = state.borrow().config.save();
                    return true;
                }
            }
            false
        }
        "Shuffle" => {
            if let Some(on) = value.get::<bool>() {
                {
                    let mut s = state.borrow_mut();
                    s.shuffle_state.enabled = on;
                    s.shuffle_state.reset();
                    s.config.playback.shuffle_enabled = on;
                }
                let _ = state.borrow().config.save();
                return true;
            }
            false
        }
        "Volume" => {
            if let Some(v) = value.get::<f64>() {
                let v = v.clamp(0.0, 1.0);
                {
                    let mut s = state.borrow_mut();
                    s.player.set_volume(v);
                    s.config.playback.volume = v;
                }
                let _ = state.borrow().config.save();
                return true;
            }
            false
        }
        "Rate" => true, // accepted, ignored (only 1.0 supported)
        _ => false,
    }
}

/// Poll (every 500 ms) for status / loop / shuffle / track changes and emit a
/// single `PropertiesChanged` with whatever changed. MPRIS consumers poll
/// Position themselves, so it is deliberately NOT signalled.
fn start_poll(
    conn: gio::DBusConnection,
    state: Rc<RefCell<AppState>>,
    cache: Rc<RefCell<PlayerCache>>,
) {
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let (path, status, loop_status, shuffle, volume, length) = {
            let s = state.borrow();
            (
                s.playlist.current().map(|t| t.path.to_string_lossy().into_owned()),
                playback_status_str(s.player.state()).to_string(),
                repeat_to_loop_status(s.config.playback.repeat_mode).to_string(),
                s.shuffle_state.enabled,
                s.config.playback.volume,
                s.player.length_usecs(),
            )
        };

        let mut changed: Vec<(&'static str, glib::Variant)> = Vec::new();
        {
            let c = cache.borrow();
            if status != c.status {
                changed.push(("PlaybackStatus", status.to_variant()));
            }
            if loop_status != c.loop_status {
                changed.push(("LoopStatus", loop_status.to_variant()));
            }
            if shuffle != c.shuffle {
                changed.push(("Shuffle", shuffle.to_variant()));
            }
            if (volume - c.volume).abs() > f64::EPSILON {
                changed.push(("Volume", volume.to_variant()));
            }
        }
        // Rebuild Metadata on a track change, OR when the duration finally
        // resolves for the current track (the track-change rebuild often runs
        // before GStreamer knows the length, which would otherwise leave
        // mpris:length absent for the whole track).
        let track_changed = cache.borrow().path != path;
        let length_resolved = cache.borrow().meta_length <= 0 && length > 0;
        if track_changed || length_resolved {
            let (meta, meta_length) = build_current_meta(&state);
            {
                let mut c = cache.borrow_mut();
                c.metadata = meta.clone();
                c.meta_length = meta_length;
            }
            changed.push(("Metadata", meta));
        }

        {
            let mut c = cache.borrow_mut();
            c.status = status;
            c.loop_status = loop_status;
            c.shuffle = shuffle;
            c.volume = volume;
            c.path = path;
        }

        if !changed.is_empty() {
            emit_props_changed(&conn, &changed);
        }
        glib::ControlFlow::Continue
    });
}

/// Build the `a{sv}` Metadata variant for the current track (empty dict + 0
/// length when nothing is playing). Returns the `mpris:length` (µs) it baked in
/// so the poll can tell when a later duration resolution needs a rebuild. Reads
/// tags off disk — called only on track change / length resolution.
fn build_current_meta(state: &Rc<RefCell<AppState>>) -> (glib::Variant, i64) {
    let path = match state.borrow().playlist.current().map(|t| t.path.clone()) {
        Some(p) => p,
        None => return (glib::VariantDict::new(None).end(), 0),
    };
    let fields = crate::id3_editor::read_tag_fields(&path);
    let length = state.borrow().player.length_usecs();
    let art = state
        .borrow()
        .current_now_playing()
        .and_then(|i| i.artwork_path)
        .map(|p| p.to_string_lossy().into_owned());

    let meta = MprisMeta {
        path: path.to_string_lossy().into_owned(),
        length_usecs: length,
        art_path: art,
        title: fields.title,
        artist: fields.artist,
        album: fields.album,
        album_artist: fields.album_artist,
        genre: fields.genre,
        track_number: fields.track_number.parse::<i64>().ok(),
    };
    (meta_to_variant(&build_metadata(&meta)), length)
}

/// Convert the pure builder's typed pairs into an `a{sv}` variant.
fn meta_to_variant(pairs: &[(&'static str, MetaValue)]) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    for (key, val) in pairs {
        let var = match val {
            MetaValue::Str(s) => s.to_variant(),
            MetaValue::StrList(l) => l.to_variant(),
            MetaValue::I64(n) => {
                // xesam:trackNumber is "i" (int32); mpris:length is "x" (int64).
                if *key == "xesam:trackNumber" {
                    (*n as i32).to_variant()
                } else {
                    n.to_variant()
                }
            }
            MetaValue::ObjPath(p) => glib::variant::ObjectPath::try_from(p.as_str())
                .map(|op| op.to_variant())
                .unwrap_or_else(|_| p.to_variant()),
            MetaValue::ArtUrl(u) => u.to_variant(),
        };
        dict.insert_value(key, &var);
    }
    dict.end()
}

/// Emit `org.freedesktop.DBus.Properties.PropertiesChanged` for the Player
/// interface with the given changed properties.
fn emit_props_changed(conn: &gio::DBusConnection, changed: &[(&'static str, glib::Variant)]) {
    let dict = glib::VariantDict::new(None);
    for (key, var) in changed {
        dict.insert_value(key, var);
    }
    let params = glib::Variant::tuple_from_iter([
        PLAYER_IFACE.to_variant(),
        dict.end(),
        Vec::<String>::new().to_variant(),
    ]);
    let _ = conn.emit_signal(
        None,
        OBJECT_PATH,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        Some(&params),
    );
}

/// Emit the Player `Seeked(x)` signal after a real seek.
fn emit_seeked(conn: &gio::DBusConnection, position_usecs: i64) {
    let params = glib::Variant::tuple_from_iter([position_usecs.to_variant()]);
    let _ = conn.emit_signal(None, OBJECT_PATH, PLAYER_IFACE, "Seeked", Some(&params));
}
