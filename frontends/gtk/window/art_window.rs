//! A6 standalone album-art window — singleton, resizable, cover only.
//!
//! Opened via `k` or by clicking the A1 panel's art. Both triggers are wired
//! through a deferred `art_open` slot in player.rs rather than calling
//! [`open_or_focus`] directly, because this window's key controller needs
//! `handle_key` for delegation, and `handle_key` is built *after* the A1
//! panel (chicken-and-egg — see the comment on `art_open` in player.rs).
//!
//! Follows every track change via the same `subscribe_now_playing` seam as
//! the A1 panel (T5/T6): the update closure only ever touches its own
//! widgets, never `AppState`, so it is safe to run from inside the
//! subscriber fan-out (which callers invoke with no `AppState` borrow held).

use gtk4::prelude::*;
use gtk4::{
    gdk, glib, Align, Box as GtkBox, ContentFit, EventControllerKey, Image, Label, Orientation,
    Picture,
};
use std::cell::RefCell;
use std::rc::Rc;

use super::AppState;
use crate::now_playing::NowPlayingInfo;

/// Open the album-art window: build it on first call, or bring the existing
/// one forward on every call after.
///
/// This is "open or focus", not a toggle — repeated `k` presses / art clicks
/// never hide the window, only `Esc` or the window's own close button do
/// (wired below). The window is a true singleton: it is built once and kept
/// alive (hidden, never destroyed) for the app's lifetime, so its
/// `now_playing` subscription is registered exactly once.
pub(super) fn open_or_focus(
    state: Rc<RefCell<AppState>>,
    handle_key: Rc<dyn Fn(gdk::Key) -> glib::Propagation>,
    parent: Option<&gtk4::Window>,
) {
    // Singleton fast path — mirrors the ML/EQ window idiom (player.rs
    // ml_window handling): present() while the borrow is still live is safe
    // here because it never re-enters AppState.
    {
        let s = state.borrow();
        if let Some(ref w) = s.art_window {
            w.present();
            return;
        }
    }

    let win = gtk4::Window::builder()
        .title("Album Art — Sparkamp")
        .default_width(360)
        .default_height(360)
        .resizable(true)
        .build();
    win.add_css_class("art-window");
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }
    // Hide, don't destroy, on the window-manager close button. This window
    // is a singleton kept alive for the app's lifetime — destroying it would
    // mean re-subscribing (and leaking the old subscriber closure) on the
    // next open.
    win.set_hide_on_close(true);

    // Art fills the whole window; swapped wholesale (Picture <-> placeholder)
    // on every track change, same technique as the A1 panel's `populate()`.
    let slot = GtkBox::new(Orientation::Vertical, 0);
    slot.set_hexpand(true);
    slot.set_vexpand(true);
    win.set_child(Some(&slot));

    // Esc hides the window locally; everything else delegates to the shared
    // handler so main-window shortcuts (z/x/c/v/b/j/i/f/…) keep working while
    // this window has focus — exact pattern as the keyboard-shortcuts window
    // (player.rs, shortcuts_win's EventControllerKey).
    {
        let key_ctrl = EventControllerKey::new();
        let handler = handle_key.clone();
        let win_wk = win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(w) = win_wk.upgrade() {
                    w.hide();
                }
                return glib::Propagation::Stop;
            }
            handler(key)
        });
        win.add_controller(key_ctrl);
    }

    // Follow every track change (including art -> no-art -> placeholder).
    let update: Rc<dyn Fn(&NowPlayingInfo)> = {
        let slot = slot.clone();
        Rc::new(move |info: &NowPlayingInfo| populate(&slot, info))
    };

    // Seed immediately from whatever is already playing — the subscriber
    // fan-out alone only fires on the *next* track change (see
    // AppState::current_now_playing's doc comment), which would leave a
    // freshly opened window blank until the user changes tracks.
    let initial = state.borrow().current_now_playing();
    match initial {
        Some(ref info) => update(info),
        None => slot.append(&placeholder_widget()),
    }
    // Register the subscription under its own short borrow, taken after the
    // seed borrow above has already been dropped — never hold a borrow
    // across a call that might itself need one (see the borrow-discipline
    // note at the top of player.rs).
    state.borrow_mut().subscribe_now_playing(update);

    win.present();
    state.borrow_mut().art_window = Some(win);
}

/// Clear and refill `slot` from `info` — mirrors the A1 panel's `populate()`.
fn populate(slot: &GtkBox, info: &NowPlayingInfo) {
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
    slot.append(&art_or_placeholder(info));
}

/// The art widget for `info`: a `Picture` scaled to fit the current window
/// size when artwork exists, otherwise the placeholder below.
///
/// Not the A1 panel's `art_or_placeholder` (that one is fixed to a 200x200
/// slot, by design, so re-populating never nudges the panel's layout) — this
/// window is resizable and cover-only, so the image should fill whatever
/// size the user resizes it to.
fn art_or_placeholder(info: &NowPlayingInfo) -> gtk4::Widget {
    match info.artwork_path.as_ref() {
        Some(path) => {
            let pic = Picture::new();
            pic.set_can_shrink(true);
            pic.set_content_fit(ContentFit::Contain);
            pic.set_hexpand(true);
            pic.set_vexpand(true);
            pic.set_filename(Some(path));
            pic.add_css_class("np-art");
            pic.upcast()
        }
        None => placeholder_widget(),
    }
}

/// 50%-opacity logo + "No artwork available" — visually identical to the A1
/// panel's placeholder (same embedded `LOGO_BYTES`, same opacity, same
/// text), just centered in the full window instead of a fixed 200x200 slot.
fn placeholder_widget() -> gtk4::Widget {
    let wrap = GtkBox::new(Orientation::Vertical, 6);
    wrap.set_hexpand(true);
    wrap.set_vexpand(true);
    wrap.set_valign(Align::Center);
    wrap.set_halign(Align::Center);
    wrap.add_css_class("np-placeholder");

    let logo_px = 140;
    let img = Image::new();
    img.set_pixel_size(logo_px);
    img.set_opacity(0.5);
    if let Some(pb) = super::load_logo_pixbuf(logo_px) {
        img.set_from_pixbuf(Some(&pb));
    }
    wrap.append(&img);

    let lbl = Label::new(Some("No artwork available"));
    lbl.set_opacity(0.5);
    wrap.append(&lbl);

    wrap.upcast()
}
