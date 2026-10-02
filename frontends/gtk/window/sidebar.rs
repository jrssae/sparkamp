//! The Media Library's left-hand navigation list.
//!
//! Child module of [`super`] (window.rs), extracted from
//! `open_media_library_window` by plan step 3. It owns the `ListBox`, the
//! `DropTarget` that accepts playlist and file drags onto its rows, the five
//! static rows (Files, Albums, Playlists, Disc Drives, Devices) and the three
//! expand/collapse chevrons.
//!
//! What it deliberately does *not* own is routing. Selecting a row is handled
//! by three separate `connect_row_selected` handlers registered by the Files,
//! Discs and Devices sections respectively: GTK fires all three on every
//! selection and each ignores the widget names it does not recognise. That is
//! why routing travels with its page in later steps rather than landing here.
//!
//! The sub-row vectors and expanded flags are returned rather than kept
//! private because the pages own the rows themselves — the disc poll inserts
//! `disc:<id>` rows, the device poll appends `dev:<id>` rows, and the playlist
//! editor renames `pl:<id>` rows. This struct hands them the handles to do it.

use gtk4::prelude::*;
use gtk4::{
    gdk, gio, glib, Align, Box as GtkBox, DropTarget, GestureClick, Label, ListBox, ListBoxRow,
    Orientation, PolicyType, ScrolledWindow,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// `attach_pl_row_drag` makes a `pl:` row draggable; `notify_playlist_changed`
// tells the rest of the app a saved playlist gained tracks. Both are private
// to the parent module, which a child may still use.
use super::{attach_pl_row_drag, gtk_safe, notify_playlist_changed, AppState, MlHost};

/// Left inset of a header chevron, matching the 10px text inset the
/// chevron-less rows (Files, Albums) use, so every row starts on one line.
const CHEVRON_MARGIN: i32 = 10;

/// Fixed width of the chevron's slot. Requested rather than left to the glyph
/// so the text column does not shift with the font the skin picks.
const CHEVRON_GLYPH_W: i32 = 12;

/// Left inset for rows that have no chevron (Files, Albums), placing their
/// text on the same column as a header's. Without it the nav reads as ragged.
const ROW_TEXT_INSET: i32 = CHEVRON_MARGIN + CHEVRON_GLYPH_W + 4;

/// Left inset for sub-rows (`pl:`, `disc:`, `dev:`), one step deeper than the
/// header they hang under so the nesting is legible. Shared rather than
/// repeated: sub-rows are created in six places — here for the initial
/// playlists, and in media_library.rs for the playlist rebuilds and the disc
/// and device polls — and they drifted into looking flat once the chevrons
/// moved and pushed every header's text right.
pub(super) const SUB_ROW_INSET: i32 = ROW_TEXT_INSET + 16;

/// Width of the chevron's click zone, measured from the row's left edge.
/// A click inside it toggles the section; anything right of it falls through
/// to row selection, which is what navigates. Covers the 10px inset plus the
/// glyph and its trailing margin, with a little slack for a fat finger.
const CHEVRON_HIT_WIDTH: f64 = 26.0;

/// The sidebar and every handle into it the rest of the window needs.
pub(super) struct Sidebar {
    /// The nav list itself. Pages append their own sub-rows to it and read
    /// `widget_name()` off the selected row to route.
    pub list: ListBox,
    /// The `ScrolledWindow` wrapping [`Sidebar::list`] — what goes in the pane.
    pub scroll: ScrolledWindow,
    /// Late-bound playlist-send runner, filled by the device view. The drop
    /// handler calls it when a playlist is dragged onto a `dev:` row; left at
    /// `None` the drop silently does nothing.
    pub send_playlist_holder:
        Rc<RefCell<Option<Rc<dyn Fn(sparkamp::devices::Device, i64, String)>>>>,
    pub playlists_expanded: Rc<Cell<bool>>,
    pub pl_sub_rows: Rc<RefCell<Vec<ListBoxRow>>>,
    pub discs_expanded: Rc<Cell<bool>>,
    pub disc_sub_rows: Rc<RefCell<Vec<ListBoxRow>>>,
    /// Spinner in the "Disc Drives" header, shown until the first drive poll
    /// finishes. A child of the header, but started and stopped by the disc
    /// page, so it is handed back.
    pub disc_detect_spinner: gtk4::Spinner,
    pub devices_expanded: Rc<Cell<bool>>,
    pub dev_sub_rows: Rc<RefCell<Vec<ListBoxRow>>>,
}

/// Build the sidebar. Rows are appended in nav order, which is the order they
/// appear in: Files, Albums, Playlists, Disc Drives, Devices.
pub(super) fn build(host: &MlHost) -> Sidebar {
    let sidebar = ListBox::new();
    sidebar.set_selection_mode(gtk4::SelectionMode::Single);
    sidebar.add_css_class("ml-sidebar");
    sidebar.set_vexpand(true);

    // Latest detected devices — now a parameter (shared with player.rs's
    // active playlist Send-to menu), kept current by the poll below.

    // Sidebar DropTarget — accept FileList drags from the active playlist,
    // ML files view, or ML editor and append paths to the saved playlist
    // whose `pl:<id>` row is under the drop coordinate.  Drops landing on
    // the Files/Playlists header rows fall through to no-op.
    // Deferred handle to the playlist-send runner (defined later, in the
    // device-view section). Lets the sidebar drop handler send a playlist
    // dragged onto a device row.
    let send_playlist_holder: Rc<
        RefCell<Option<Rc<dyn Fn(sparkamp::devices::Device, i64, String)>>>,
    > = Rc::new(RefCell::new(None));
    // copy_files_holder is now a parameter (shared with player.rs's active
    // playlist Send-to menu) — see the fn signature above.
    {
        let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        dt.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
        let sidebar_for_drop = sidebar.clone();
        let state_for_drop   = host.state.clone();
        let current_devices_drop = host.current_devices.clone();
        let send_holder_drop = send_playlist_holder.clone();
        let copy_holder_drop = host.copy_files_holder.clone();
        dt.connect_drop(move |_, value, _x, y| {
            // Locate the sidebar row under the drop coordinate.
            let mut hit: Option<ListBoxRow> = None;
            let mut i = 0i32;
            while let Some(r) = sidebar_for_drop.row_at_index(i) {
                if let Some(b) = r.compute_bounds(&sidebar_for_drop) {
                    if y as f32 >= b.y() && y as f32 <= b.y() + b.height() {
                        hit = Some(r);
                        break;
                    }
                }
                i += 1;
            }
            let Some(row) = hit else { return false };
            let name = row.widget_name().to_string();

            // Resolve the drag payload. A playlist row drags a `pl:<id>`
            // String. Track drags ship a FileList — but when the drop target
            // also advertises STRING (it does, for `pl:`), GTK may instead
            // deliver the FileList as a text/uri-list String. Handle both so a
            // drag from the active playlist works regardless of which format
            // gets negotiated.
            enum Payload {
                Playlist(i64),
                Files(Vec<std::path::PathBuf>),
            }
            let payload = if let Ok(s) = value.get::<String>() {
                if let Some(pid) = s.strip_prefix("pl:").and_then(|n| n.trim().parse::<i64>().ok())
                {
                    Payload::Playlist(pid)
                } else {
                    // A newline-separated uri-list or path-list.
                    let paths: Vec<std::path::PathBuf> = s
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty() && !l.starts_with('#'))
                        .map(|l| {
                            if l.starts_with("file://") {
                                gio::File::for_uri(l)
                                    .path()
                                    .unwrap_or_else(|| std::path::PathBuf::from(l))
                            } else {
                                std::path::PathBuf::from(l)
                            }
                        })
                        .collect();
                    if paths.is_empty() {
                        return false;
                    }
                    Payload::Files(paths)
                }
            } else if let Ok(file_list) = value.get::<gdk::FileList>() {
                let paths: Vec<std::path::PathBuf> =
                    file_list.files().iter().filter_map(|f| f.path()).collect();
                if paths.is_empty() {
                    return false;
                }
                Payload::Files(paths)
            } else {
                return false;
            };

            match payload {
                // Playlist dropped onto a device row → send files + .m3u8.
                Payload::Playlist(pid) => {
                    let Some(backend) = name.strip_prefix("dev:") else {
                        return false;
                    };
                    let Some(dev) = current_devices_drop
                        .borrow()
                        .iter()
                        .find(|d| d.backend_id == backend)
                        .cloned()
                    else {
                        return false;
                    };
                    let plname = state_for_drop
                        .borrow()
                        .media_lib
                        .as_ref()
                        .and_then(|l| l.playlist_by_id(pid).ok())
                        .map(|p| p.name)
                        .unwrap_or_default();
                    if let Some(send) = send_holder_drop.borrow().as_ref() {
                        send(dev, pid, plname);
                        return true;
                    }
                    false
                }
                Payload::Files(srcs) => {
                    // Onto a device row → copy the files (async, with progress).
                    if let Some(backend) = name.strip_prefix("dev:") {
                        let Some(dev) = current_devices_drop
                            .borrow()
                            .iter()
                            .find(|d| d.backend_id == backend)
                            .cloned()
                        else {
                            return false;
                        };
                        if let Some(copy) = copy_holder_drop.borrow().as_ref() {
                            copy(dev, srcs);
                            return true;
                        }
                        return false;
                    }
                    // Onto a saved-playlist row → append the files to it.
                    let Some(pid) =
                        name.strip_prefix("pl:").and_then(|n| n.parse::<i64>().ok())
                    else {
                        return false;
                    };
                    let path_strs: Vec<String> =
                        srcs.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                    if let Some(lib) = state_for_drop.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.append_paths_to_playlist(pid, &path_strs) {
                            // `{e:#}`, not `{e}`: anyhow's Display prints only
                            // the outermost context, so a plain `{e}` reports
                            // "write playlist <path>" and hides the cause that
                            // actually explains it (a read-only mount, a
                            // permission denial). The alternate form walks the
                            // whole chain.
                            eprintln!("append_paths_to_playlist {pid}: {e:#}");
                            return false;
                        }
                    }
                    notify_playlist_changed(pid);
                    true
                }
            }
        });
        sidebar.add_controller(dt);
    }

    let sidebar_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&sidebar)
        .build();

    // ── "Files" row ───────────────────────────────────────────────────────
    {
        let lbl = Label::builder()
            .label("Files")
            .halign(Align::Start)
            .xalign(0.0)
            .margin_start(ROW_TEXT_INSET)
            .margin_end(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let row = ListBoxRow::new();
        row.set_widget_name("files");
        row.set_child(Some(&lbl));
        sidebar.append(&row);
    }

    // ── "Albums" row (Phase 11 A5: album gallery) ──────────────────────────
    {
        let lbl = Label::builder()
            .label("Albums")
            .halign(Align::Start)
            .xalign(0.0)
            .margin_start(ROW_TEXT_INSET)
            .margin_end(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let row = ListBoxRow::new();
        row.set_widget_name("albums");
        row.set_child(Some(&lbl));
        sidebar.append(&row);
    }

    // ── Source filters under Files and under Albums ───────────────────────
    // Only once a server is set up; rebuilt when the server list changes
    // (see `AppState::source_rows_callback`).
    refresh_source_rows(&sidebar, &host.state.borrow().config.servers);
    {
        let list = sidebar.downgrade();
        let state = Rc::downgrade(&host.state);
        host.state.borrow_mut().source_rows_callback = Some(Rc::new(move || {
            let (Some(list), Some(state)) = (list.upgrade(), state.upgrade()) else { return };
            let servers = state.borrow().config.servers.clone();
            refresh_source_rows(&list, &servers);
        }));
    }

    // ── "Playlists" header row (with expand/collapse chevron) ─────────────
    let playlists_expanded = Rc::new(Cell::new(
        host.state.borrow().config.window.ml_playlists_expanded
    ));

    // Track sub-rows so we can show/hide them on toggle
    let pl_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let pl_header_box = GtkBox::new(Orientation::Horizontal, 0);

        let pl_lbl = Label::builder()
            .label("Playlists")
            .halign(Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .margin_top(7)
            .margin_bottom(7)
            .build();

        // Chevron label — "▾" expanded, "▸" collapsed. Leads the text rather
        // than trailing it: a right-aligned chevron is the first thing a long
        // playlist name pushes out of view in a narrow sidebar.
        let chevron_lbl = Label::builder()
            .label(if playlists_expanded.get() { "▾" } else { "▸" })
            .width_request(CHEVRON_GLYPH_W)
            .margin_start(CHEVRON_MARGIN)
            .margin_end(4)
            .build();

        pl_header_box.append(&chevron_lbl);
        pl_header_box.append(&pl_lbl);

        let row_playlists = ListBoxRow::new();
        row_playlists.set_widget_name("playlists");
        row_playlists.set_child(Some(&pl_header_box));
        sidebar.append(&row_playlists);

        // Chevron click toggles expansion (separate from navigation)
        let gesture = GestureClick::new();
        let expanded_rc = playlists_expanded.clone();
        let sub_rows_rc  = pl_sub_rows.clone();
        let chev = chevron_lbl.clone();
        let state_toggle = host.state.clone();
        gesture.connect_released(move |_g, _n, x, _y| {
            if x > CHEVRON_HIT_WIDTH {
                return; // right of the chevron = navigation, handled elsewhere
            }
            let new_val = !expanded_rc.get();
            expanded_rc.set(new_val);
            // Mirror into the config immediately rather than only when the
            // Media Library window closes. Quitting the app with this window
            // still open runs the main window's close handler, which saves a
            // clone of the config and never consults this cell — so a collapse
            // made and never followed by an ML close was silently discarded.
            // Keeping the config current makes every save path correct.
            state_toggle.borrow_mut().config.window.ml_playlists_expanded = new_val;
            chev.set_text(if new_val { "▾" } else { "▸" });
            for r in sub_rows_rc.borrow().iter() {
                r.set_visible(new_val);
            }
        });
        row_playlists.add_controller(gesture);
    }

    // Populate initial playlist sub-rows
    {
        // Playlist files and every server's playlists, by name.
        let playlists_initial = host.state
            .borrow()
            .media_lib
            .as_ref()
            .and_then(|lib| lib.listed_playlists().ok())
            .unwrap_or_default();
        let icons = playlist_icons(&host.state.borrow());
        let expanded = playlists_expanded.get();
        for pl in &playlists_initial {
            let lbl = Label::builder()
                .label(gtk_safe(&pl.name))
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(SUB_ROW_INSET)
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
            let row = ListBoxRow::new();
            row.set_widget_name(&format!("pl:{}", pl.id));
            row.set_child(Some(&pl_row_child(lbl, &pl.source, None, icons.as_ref())));
            row.set_visible(expanded);
            attach_pl_row_drag(&row, pl.id);
            sidebar.append(&row);
            pl_sub_rows.borrow_mut().push(row);
        }
    }

    // ── "Disc Drives" header row (optical drives via sparkamp::disc) ─────────
    // Sits just above Devices. Disc sub-rows are inserted between this header
    // and the Devices header; device rows keep appending to the sidebar end, so
    // the two groups stay separate. Phase 1: detection + audio-CD playback.
    let discs_expanded = Rc::new(Cell::new(true));
    let disc_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));
    // Spinner shown in the sidebar header while that first poll runs; stopped
    // and hidden by refresh_discs once detection completes.
    let disc_detect_spinner = gtk4::Spinner::new();
    // Sits immediately after the "Disc Drives" label (not far-right, where a wide
    // sidebar would push it off-screen). An unsized spinner in a header slot can
    // render 0×0, so give it an explicit size and center it vertically.
    disc_detect_spinner.set_margin_start(6);
    disc_detect_spinner.set_size_request(16, 16);
    disc_detect_spinner.set_valign(Align::Center);
    disc_detect_spinner.start();
    {
        let hdr = GtkBox::new(Orientation::Horizontal, 0);
        // Chevron leads, then the label at its text width (no hexpand) so the
        // spinner can follow it directly; a hexpanding spacer absorbs the rest.
        let lbl = Label::builder()
            .label("Disc Drives")
            .halign(Align::Start)
            .xalign(0.0)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let spacer = Label::new(None);
        spacer.set_hexpand(true);
        let chev = Label::builder()
            .label(if discs_expanded.get() { "▾" } else { "▸" })
            .width_request(CHEVRON_GLYPH_W)
            .margin_start(CHEVRON_MARGIN)
            .margin_end(4)
            .build();
        hdr.append(&chev);
        hdr.append(&lbl);
        hdr.append(&disc_detect_spinner);
        hdr.append(&spacer);
        let row = ListBoxRow::new();
        row.set_widget_name("discs");
        row.set_child(Some(&hdr));
        sidebar.append(&row);

        let gesture = GestureClick::new();
        let exp = discs_expanded.clone();
        let subs = disc_sub_rows.clone();
        let chev2 = chev.clone();
        gesture.connect_released(move |_g, _n, x, _y| {
            if x > CHEVRON_HIT_WIDTH {
                return; // right of the chevron = navigation, handled elsewhere
            }
            let v = !exp.get();
            exp.set(v);
            chev2.set_text(if v { "▾" } else { "▸" });
            for r in subs.borrow().iter() {
                r.set_visible(v);
            }
        });
        row.add_controller(gesture);
    }

    // ── "Devices" header row (external USB/SD storage via udisks2) ────────
    // Mirrors the Playlists header: an expand/collapse chevron, with device
    // sub-rows populated live by the poll below.
    let devices_expanded = Rc::new(Cell::new(true));
    let dev_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));
    // `current_devices` is declared earlier (before the sidebar DropTarget).
    {
        let hdr = GtkBox::new(Orientation::Horizontal, 0);
        let lbl = Label::builder()
            .label("Devices")
            .halign(Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let chev = Label::builder()
            .label(if devices_expanded.get() { "▾" } else { "▸" })
            .width_request(CHEVRON_GLYPH_W)
            .margin_start(CHEVRON_MARGIN)
            .margin_end(4)
            .build();
        hdr.append(&chev);
        hdr.append(&lbl);
        let row = ListBoxRow::new();
        row.set_widget_name("devices");
        row.set_child(Some(&hdr));
        sidebar.append(&row);

        let gesture = GestureClick::new();
        let exp = devices_expanded.clone();
        let subs = dev_sub_rows.clone();
        let chev2 = chev.clone();
        gesture.connect_released(move |_g, _n, x, _y| {
            if x > CHEVRON_HIT_WIDTH {
                return; // right of the chevron = navigation, handled elsewhere
            }
            let v = !exp.get();
            exp.set(v);
            chev2.set_text(if v { "▾" } else { "▸" });
            for r in subs.borrow().iter() {
                r.set_visible(v);
            }
        });
        row.add_controller(gesture);
    }
    Sidebar {
        list: sidebar,
        scroll: sidebar_scroll,
        send_playlist_holder,
        playlists_expanded,
        pl_sub_rows,
        discs_expanded,
        disc_sub_rows,
        disc_detect_spinner,
        devices_expanded,
        dev_sub_rows,
    }
}

/// The source filters offered under Files and under Albums, as `(label,
/// key)`: All, Local, each enabled server by name, Local changes, Needs
/// attention. Empty without an enabled server, when every song is local. The
/// key goes into the row's name, `src:<page>:<key>`.
pub(super) fn source_filter_options(servers: &[sparkamp::config::ServerConfig]) -> Vec<(String, String)> {
    let enabled: Vec<&sparkamp::config::ServerConfig> = servers.iter().filter(|s| s.enabled).collect();
    if enabled.is_empty() {
        return Vec::new();
    }
    let mut options = vec![("All".to_string(), "all".to_string()), ("Local".to_string(), "local".to_string())];
    for s in enabled {
        options.push((s.name.clone(), format!("server:{}", s.id)));
    }
    options.push(("Local changes".to_string(), "local_changes".to_string()));
    options.push(("Needs attention".to_string(), "needs_attention".to_string()));
    options
}

/// The page (`"files"` or `"albums"`) and filter a `src:` row stands for.
pub(super) fn parse_source_row(name: &str) -> Option<(&str, sparkamp::media_library::servers::SourceFilter)> {
    use sparkamp::media_library::servers::SourceFilter;
    let rest = name.strip_prefix("src:")?;
    let (page, key) = rest.split_once(':')?;
    let filter = match key {
        "all" => SourceFilter::All,
        "local" => SourceFilter::Local,
        "local_changes" => SourceFilter::LocalChanges,
        "needs_attention" => SourceFilter::NeedsAttention,
        _ => SourceFilter::Server(key.strip_prefix("server:")?.to_string()),
    };
    Some((page, filter))
}

/// Put the source-filter rows under the Files and Albums rows, replacing any
/// already there; none without an enabled server.
pub(super) fn refresh_source_rows(list: &ListBox, servers: &[sparkamp::config::ServerConfig]) {
    let mut i = 0;
    while let Some(row) = list.row_at_index(i) {
        if row.widget_name().starts_with("src:") {
            list.remove(&row);
        } else {
            i += 1;
        }
    }
    let options = source_filter_options(servers);
    for page in ["files", "albums"] {
        let mut at = None;
        let mut i = 0;
        while let Some(row) = list.row_at_index(i) {
            if row.widget_name() == page {
                at = Some(i);
                break;
            }
            i += 1;
        }
        let Some(at) = at else { continue };
        for (k, (label, key)) in options.iter().enumerate() {
            let lbl = Label::builder()
                .label(gtk_safe(label))
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(SUB_ROW_INSET)
                .margin_end(8)
                .margin_top(3)
                .margin_bottom(3)
                .build();
            lbl.add_css_class("dim-label");
            let row = ListBoxRow::new();
            row.set_widget_name(&format!("src:{page}:{key}"));
            row.set_child(Some(&lbl));
            list.insert(&row, at + 1 + k as i32);
        }
    }
}

/// The source icons for playlist rows, or `None` before any server is set
/// up, when every playlist is a file here and rows keep their plain name.
pub(super) fn playlist_icons(state: &AppState) -> Option<std::collections::HashMap<&'static str, gdk::Texture>> {
    state.config.servers.iter().any(|s| s.enabled).then(super::files::source_icons)
}

/// A playlist row's content. Without icons, the name label alone, as rows
/// always were. With them, the source icon (a file here, or a server) in
/// front of the name; given a `server_name`, a server playlist also shows
/// it and a lock after the name, as the Playlists page does.
pub(super) fn pl_row_child(
    name: Label,
    source: &sparkamp::media_library::PlaylistSource,
    server_name: Option<&str>,
    icons: Option<&std::collections::HashMap<&'static str, gdk::Texture>>,
) -> gtk4::Widget {
    use sparkamp::media_library::PlaylistSource;
    let Some(icons) = icons else { return name.upcast() };
    let row = GtkBox::new(Orientation::Horizontal, 5);
    row.set_margin_start(name.margin_start());
    name.set_margin_start(0);
    name.add_css_class("pl-row-name");
    let (key, tip) = match source {
        PlaylistSource::Local => ("local", "A playlist file on this computer".to_string()),
        PlaylistSource::Server(_) => (
            "server",
            match server_name {
                Some(n) => format!("On {n}, read-only in Sparkamp"),
                None => "On a server, read-only in Sparkamp".to_string(),
            },
        ),
    };
    if let Some(texture) = icons.get(key) {
        let img = gtk4::Image::from_paintable(Some(texture));
        img.set_pixel_size(14);
        img.set_tooltip_text(Some(&gtk_safe(&tip)));
        row.append(&img);
    }
    row.append(&name);
    if let (PlaylistSource::Server(_), Some(n)) = (source, server_name) {
        let server = Label::new(Some(&gtk_safe(n)));
        server.add_css_class("dim-label");
        server.add_css_class("pl-row-server");
        row.append(&server);
        let lock = Label::new(Some("🔒"));
        lock.set_tooltip_text(Some("Read-only in Sparkamp: playlist changes are not sent to servers yet"));
        lock.set_margin_end(8);
        row.append(&lock);
    }
    row.upcast()
}

/// A playlist row's name label, whether the row holds the label alone or
/// the label beside its source icon.
pub(super) fn pl_row_label(row: &ListBoxRow) -> Option<Label> {
    let child = row.child()?;
    if let Ok(label) = child.clone().downcast::<Label>() {
        return Some(label);
    }
    let mut part = child.first_child();
    while let Some(w) = part {
        if w.has_css_class("pl-row-name") {
            return w.downcast::<Label>().ok();
        }
        part = w.next_sibling();
    }
    None
}

/// What a playlist search matches against: the name, and for a server
/// playlist the server's name, lowercased.
pub(super) fn pl_row_search_text(row: &ListBoxRow) -> String {
    let mut text = pl_row_label(row).map(|l| l.label().to_string()).unwrap_or_default();
    if let Some(child) = row.child() {
        let mut part = child.first_child();
        while let Some(w) = part {
            if w.has_css_class("pl-row-server") {
                if let Ok(l) = w.clone().downcast::<Label>() {
                    text.push(' ');
                    text.push_str(&l.label());
                }
            }
            part = w.next_sibling();
        }
    }
    text.to_lowercase()
}

#[cfg(test)]
mod source_row_tests {
    use super::*;
    use sparkamp::media_library::servers::SourceFilter;

    fn server(id: &str, name: &str, enabled: bool) -> sparkamp::config::ServerConfig {
        sparkamp::config::ServerConfig { id: id.into(), name: name.into(), enabled, ..Default::default() }
    }

    #[test]
    fn no_filters_until_a_server_is_set_up() {
        assert!(source_filter_options(&[]).is_empty());
        assert!(source_filter_options(&[server("x", "Off", false)]).is_empty());
    }

    #[test]
    fn each_enabled_server_gets_a_filter_by_name_and_every_row_reads_back() {
        let options = source_filter_options(&[server("a1", "Oscar", true), server("b2", "Off", false)]);
        let labels: Vec<&str> = options.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["All", "Local", "Oscar", "Local changes", "Needs attention"]);
        let read: Vec<_> = options
            .iter()
            .map(|(_, key)| parse_source_row(&format!("src:albums:{key}")).unwrap())
            .collect();
        assert_eq!(read[2], ("albums", SourceFilter::Server("a1".into())));
        assert_eq!(read[4], ("albums", SourceFilter::NeedsAttention));
        assert_eq!(parse_source_row("files"), None);
        assert_eq!(parse_source_row("pl:3"), None);
    }
}
