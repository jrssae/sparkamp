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
use gtk4::glib::prelude::*;
use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use super::AppState;

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

/// Keeps the MPRIS bus-name ownership + object registration alive for the app's
/// lifetime. GLib owns the registered closures, but we park the ids (and the
/// connection) here both to hold the option to unown/unregister on quit and to
/// give P3-T5 a handle to the live connection for the Player interface.
/// `#[allow(dead_code)]` — the fields exist to own lifetimes, not to be read.
#[allow(dead_code)]
pub(super) struct MprisGuard {
    owner: gio::OwnerId,
    /// The session-bus connection, filled once the name is acquired. P3-T5's
    /// Player registration + signal emission hang off this.
    pub(super) conn: Rc<RefCell<Option<gio::DBusConnection>>>,
    /// Root-object registration id, filled on bus-acquired.
    root_reg: Rc<RefCell<Option<gio::RegistrationId>>>,
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

    let owner = gio::bus_own_name(
        gio::BusType::Session,
        "org.mpris.MediaPlayer2.sparkamp",
        gio::BusNameOwnerFlags::NONE,
        // bus-acquired: register the root object + stash the connection.
        {
            let app = app.clone();
            let window = window.clone();
            let conn_slot = conn_slot.clone();
            let reg_slot = reg_slot.clone();
            move |conn, _name| {
                register_root(&conn, &app, &window, &reg_slot);
                *conn_slot.borrow_mut() = Some(conn);
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
