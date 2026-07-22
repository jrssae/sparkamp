//! GTK4 main window — widget layout, callbacks, and application logic.
#![allow(deprecated)]
//!
//! ## Architecture
//!
//! All mutable runtime state is held in an [`AppState`] value that is wrapped
//! in `Rc<RefCell<AppState>>`.  GTK4 runs on a single thread, so `Rc` (rather
//! than `Arc`) is the right primitive: it is cheaper and there is no risk of
//! data races.  Each callback that needs to read or write state receives its
//! own `Rc::clone`, which is cheap (just an integer increment).
//!
//! ### Borrow discipline
//! `RefCell` enforces single-writer / multiple-reader rules at runtime.  To
//! prevent a panic, every borrow is kept as short as possible:
//! - Immutable borrows (`.borrow()`) are dropped before any mutable borrow.
//! - Mutable borrows (`.borrow_mut()`) are dropped before calling any GTK
//!   method that might re-enter a callback (e.g. `queue_draw()`).
//!
//! ## GUI features
//! - Now-playing title and artist labels
//! - Seek bar with drag-detection (prevents the tick loop from fighting user)
//! - Animated visualizer (bars / waveform, toggled with `a`; waveform fullscreen with `f`)
//! - Transport buttons: ⏮ ▶ ⏸ ⏹ ⏭
//! - Volume slider (0 – 100 %)
//! - Live search / jump overlay (`j` key)
//! - Native file-chooser for adding tracks (`n` key)
//! - `Delete` key removes the highlighted playlist row
//! - Winamp keyboard bindings: z x c v b a q

use anyhow::Result;
use glib::ControlFlow;
use gtk4::prelude::*;
// Suppress deprecated warnings for GTK4 APIs that are still widely used
// but have modern replacements (ComboBoxText, ColorButton, ListStore, TreeView, etc.)
// TODO: Migrate to modern APIs (DropDown, ListStore, TreeView, etc.) when feasible
#[allow(deprecated)]
use gtk4::{
    gdk, gdk_pixbuf, gio, glib, Adjustment, Align, Application, ApplicationWindow, Box as GtkBox,
    Button, CellRendererText, CheckButton, ColorButton, ColumnView, ColumnViewColumn,
    ContentFit, CustomSorter, DragSource, DrawingArea, DropDown, DropTarget, Entry,
    EventControllerKey, GestureClick, Grid, Image, Label, ListBox, ListBoxRow, ListStore,
    MultiSelection, Notebook, Orientation, Paned, Picture, PolicyType, Scale, ScrolledWindow,
    Separator, SignalListItemFactory, SortListModel, SpinButton, Stack, StackTransitionType,
    TreeView, TreeViewColumn,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use crate::{
    config::{Config, VisualizerMode, WaveformStyle},
    duration_cache::DurationCache,
    duration_probe,
    engine::{BusEvent, Player, PlayerState},
    model::{fmt_duration, Playlist, Track},
    shuffle::ShuffleState,
};
// Device sync/plan/apply logic lives in core (`crate::devices::plan`); the
// thin `device_*`/`apply_*` functions below forward to it. These two types are
// produced/consumed by that logic and the frontend, so they are shared from
// core rather than redefined here.
use crate::devices::plan::{PlaylistSyncItem, TagConflictItem};

// Disc (optical media) UI: rip dialog/worker + drive-view helpers. A child
// module so it can use this file's private AppState/gtk_safe; new disc UI
// (submit, burn) goes there, not here.
mod disc;
use disc::{disc_overview_detail_line, selected_disc_discid};

// A1 expandable now-playing panel (art + tags + wiki links). A child module
// (not include!d) so its widget-building code stays out of player.rs's
// already-large body; player.rs calls it as `now_playing::build_panel(...)`.
mod now_playing;

// A6 standalone album-art window (`k` key / A1 art click). A child module
// for the same reason as now_playing above; player.rs calls it as
// `art_window::open_or_focus(...)`.
mod art_window;
mod mpris;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Physical file split (2026-07-11)
// ---------------------------------------------------------------------------
// window.rs reached ~21k lines, unworkable for review or for smaller models.
// The sections below are include!d verbatim: every file is a plain byte slice
// of the old window.rs, so the compiler sees the exact same single module and
// nothing needed visibility or import surgery. This split was produced on a
// machine that cannot compile the (Linux-only) GTK frontend, so include! was
// chosen because byte-identity is provable offline. Converting these to real
// `mod` submodules (pub(super) items + per-file imports) is a follow-up to do
// ON the Linux box, one file at a time, where the compiler can arbitrate.

// AppState + scan state and the AppState impl (core-side logic, no widgets)
include!("state.rs");

// small shared UI helpers: icons, gtk_safe, sanitizers, dialogs, notify_* hooks
include!("util.rs");

// build(): the main player window (transport, playlist pane, viz, key handling)
include!("player.rs");

// ID3 editor window, field customizer, column customizer, gnudb email prompt
include!("id3.rs");

// the Settings window (all tabs)
include!("settings.rs");

// the Equalizer window
include!("eq.rs");

// the Deduplicate Music window + its scan worker
include!("dedupe.rs");

// Media Library / ID3 column definitions, cell text, sort keys
include!("ml_columns.rs");

// visualizer draw helpers, fullscreen waveform window, image viewer
include!("viz.rs");

// device-sync UI helpers: MTP enumeration, plans, conflict prompts
include!("devices.rs");

// open_media_library_window(): files/playlists/devices/discs pages
include!("media_library.rs");
include!("queue_manager.rs");

// unit tests (#[cfg(test)] mod tests)
include!("tests.rs");
