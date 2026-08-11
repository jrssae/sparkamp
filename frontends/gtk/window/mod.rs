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
    ContentFit, DragSource, DrawingArea, DropDown, DropTarget, Entry,
    EventControllerKey, GestureClick, Grid, GridView, Image, Label, ListBox, ListBoxRow,
    ListStore, MultiSelection, NoSelection, Notebook, Orientation, Paned, Picture, PolicyType,
    Scale, ScrolledWindow,
    Separator, SignalListItemFactory, SpinButton, Stack, StackTransitionType,
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
// Skin CSS. Lived at the foot of state.rs while every file was one flat
// module; it belongs here now that they are real `mod`s, because player.rs is
// what reads it.
use crate::skin::{self, render_gtk_css, SkinVars};

// Disc (optical media) UI: rip dialog/worker + drive-view helpers. A child
// module so it can use the window module's private AppState/gtk_safe; new
// disc UI (submit, burn) goes there, not here.
mod disc;

// Live folder-watcher lifecycle (Phase 8 Task 10): rebuild/start/stop the
// `notify` watcher, drain its event channel, and the startup-rescan trigger.
// A child module for the same reason as `disc` above — keeps this glue out
// of player.rs/settings.rs/media_library.rs, which call it as `watch::...`.
mod watch;

// A1 expandable now-playing panel (art + tags + wiki links). A child module
// so its widget-building code stays out of player.rs's already-large body;
// player.rs calls it as `now_playing::build_panel(...)`.
mod now_playing;

// A6 standalone album-art window (`k` key / A1 art click). A child module
// for the same reason as now_playing above; player.rs calls it as
// `art_window::open_or_focus(...)`.
mod art_window;
mod mpris;

// Media Library "Albums" page (plan step 2, the breakup's first extraction).
// It takes `&MlCtx` and reaches back for `build_album_gallery`.
mod albums;

// The Media Library's left-hand nav list (plan step 3). Owns the ListBox, its
// DropTarget, the five static rows and the chevrons; routing stays with each
// page, which registers its own row-selected handler.
mod sidebar;

// The Media Library "Files" page (plan step 4): the library track table, its
// search row, status bar and row context menu. The Albums drill-down renders
// through it too.
mod files;

// The Files page's row context menu and Send-to actions (plan step 4), split
// from files.rs so neither half sits far over the 800-line goal.
mod files_menu;

// The Media Library "Disc Drives" page (plan step 5): overview cards, the
// drive detail view, the data-disc browser and the 2 s drive poll. A sibling
// of `mod disc` rather than a child of it — `disc` is the disc *logic* and
// widget helpers this page calls into, and the flat shape is what steps 2–4
// already proved (`use super::…` reaches the window's items directly).
mod disc_page;

// The data-disc file browser inside that page (plan step 5, second cut),
// split from disc_page.rs so neither half sits far over the 800-line goal.
mod disc_data;

// gnudb identify + the manual tag-override editor (plan step 5, third cut).
// Declares nothing the rest of the page reads back, so it lifted cleanly.
mod disc_gnudb;

// The Media Library "Devices" page (plan step 6): overview cards, the device
// detail view, device-playlist management and the 2 s udisks2 poll. Flat, like
// the disc pages. Not to be confused with `mod devices` below — that is the
// *logic* (detection, mounts, copy/sync helpers) this page drives; core device
// support proper lives in `crate::devices`.
mod devices_page;

// Scan / Eject / Sync (plan step 6, fourth cut) — the three device-wide
// buttons, shared with the overview cards' per-row Eject and Sync.
mod devices_actions;

// Device detection (plan step 6, third cut): the 2 s udisks2 poll, the
// overview cards, and the sidebar sub-rows they keep live.
mod devices_poll;

// The device track view's row context menu and Send-to actions (plan step 6,
// second cut) — what files_menu.rs is to the Files page.
mod devices_menu;

// Device playlists (plan step 6, first cut): sending a library playlist to a
// device, and New / Rename / Duplicate / Delete on the ones already there.
mod devices_playlists;

// The device track view's columns (plan step 6, fifth cut). Driven by the same
// shared ALL_COLUMNS table as the Files view, plus two device-only columns.
mod devices_columns;

// The Media Library "Playlists" page (plan step 7): the saved-playlist
// manager and the track editor, as two sub-pages of one stack page.
mod playlists;

// The playlist editor's row context menu (plan step 7) — what files_menu.rs
// is to the Files page.
mod playlists_menu;

// The saved-playlist manager and the load-a-playlist seam (plan step 7).
mod playlists_manage;

// The playlist editor's columns, cells and row gestures (plan step 7).
mod playlists_columns;

// ---------------------------------------------------------------------------
// The original window.rs, one module per section (2026-07-11, 2026-08-11)
// ---------------------------------------------------------------------------
// window.rs reached ~21k lines, unworkable for review or for smaller models.
// It was first cut into the files below as `include!` byte slices, because
// that split was produced on a machine that cannot compile the (Linux-only)
// GTK frontend and byte-identity is provable offline. Plan step 8 finished
// the job on the Linux box: each is now a real `mod`, with `use super::*;` at
// its head and `pub(super)` on the items its neighbours read.
//
// Each `mod` is paired with a `use <name>::*;` so the window module's own
// namespace is unchanged — every `super::foo` in the page modules above still
// resolves, and no call site had to move. Narrowing those globs to named
// imports is a separate job, and a mechanical one.

// AppState + scan state and the AppState impl (core-side logic, no widgets)
mod state;
use state::*;

// small shared UI helpers: icons, gtk_safe, sanitizers, dialogs, notify_* hooks
mod util;
use util::*;

// build(): the main player window (transport, playlist pane, viz, key handling)
mod player;
// `pub use` rather than `use`: `build` is this module's entry point, called
// from frontends/gtk/mod.rs.
pub use player::*;

// ID3 editor window, field customizer, column customizer, gnudb email prompt
mod id3;
use id3::*;

// the Settings window (all tabs)
mod settings;
use settings::*;

// the Equalizer window
mod eq;
use eq::*;

// the Deduplicate Music window + its scan worker
mod dedupe;
use dedupe::*;

// Media Library / ID3 column definitions, cell text, sort keys
mod ml_columns;
use ml_columns::*;

// visualizer draw helpers, fullscreen waveform window, image viewer
mod viz;
use viz::*;

// device-sync UI helpers: MTP enumeration, plans, conflict prompts
mod devices;
use devices::*;

// open_media_library_window(): files/playlists/devices/discs pages
mod media_library;
use media_library::*;

// the play-queue panel embedded in the Jump/Queue window
mod queue_manager;
use queue_manager::*;

// Phase 11 A4: album gallery grid (build_album_gallery) — cover thumbnails,
// zoom + sort controls, recycled GridView cells.
mod album_gallery;
use album_gallery::*;

// Phase 12 F15: read-only lyrics viewer + View/Search decision entry point
// (view_or_search_lyrics) shared by every track-row surface.
mod lyrics;
use lyrics::*;

// unit tests — a real child module (plan step 8); `use super::*` reaches the
// window's private items exactly as the inline `mod tests` block used to.
#[cfg(test)]
mod tests;
