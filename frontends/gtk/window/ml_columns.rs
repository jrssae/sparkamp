use super::*;

/// Defines all columns that can appear in both the Media Library window
/// and the ID3 tag editor.  `id3_editable` fields are shown as text entries
/// in the ID3 editor; `read_only` fields are shown as non-editable labels.
pub(super) type MlColumnDef = sparkamp::ml_columns::ColumnDef;
/// The one column table, defined in core so the TUI reads the same one.
/// Re-exported under the old name so this frontend's ~20 reference sites are
/// unchanged.
pub(super) use sparkamp::ml_columns::ALL as ALL_COLUMNS;

/// Re-apply the shared media-library column config (visibility, widths, order)
/// to a ColumnView's named columns. `fixed_leading` is how many pinned columns
/// precede the named ones (the files view has 0, the editor 2 = status +
/// position, the device view 1 = playlist-order). Used so the files view, the
/// playlist editor, and the device view all reflect the same column settings.
pub(super) fn apply_ml_columns_to(
    col_view: &ColumnView,
    named: &[(String, ColumnViewColumn)],
    state: &Rc<RefCell<AppState>>,
    fixed_leading: u32,
) {
    let (visible_ids, widths, order): (
        Vec<String>,
        std::collections::HashMap<String, i32>,
        Vec<String>,
    ) = {
        let s = state.borrow();
        (
            s.config.media_library.visible_columns.clone(),
            s.config.media_library.ml_file_col_widths.clone(),
            s.config.media_library.ml_file_col_order.clone(),
        )
    };
    for (id, col) in named {
        col.set_visible(visible_ids.contains(id));
        if let Some(&w) = widths.get(id) {
            if w > 0 {
                col.set_fixed_width(w);
            }
        }
    }
    if !order.is_empty() {
        for (_, col) in named {
            col_view.remove_column(col);
        }
        let mut pos = fixed_leading;
        for col_id in &order {
            if let Some((_, col)) = named.iter().find(|(id, _)| id == col_id) {
                col_view.insert_column(pos, col);
                pos += 1;
            }
        }
        for (id, col) in named {
            if !order.contains(id) {
                col_view.insert_column(pos, col);
                pos += 1;
            }
        }
    }
}

/// Human file size: whole KB under 1 MB, one-decimal MB above.

/// Text shown for a `LibTrack` in a given media-library column. Shared by the
/// device track view so it mirrors the files view's columns.
///
/// `artist_as_album_artist` is the F12.2 display fallback
/// (`config.media_library.artist_as_album_artist`); passed through to
/// `play_stats::effective_album_artist` for the "album_artist" column. A4
/// (phase 11 album gallery) MUST also route its grouping through that same
/// helper.
/// Text shown for a `LibTrack` in a given media-library column. Shared by the
/// device track view so it mirrors the files view's columns.
pub(super) fn ml_cell_text(t: &sparkamp::media_library::LibTrack, id: &str, artist_as_album_artist: bool) -> String {
    sparkamp::ml_columns::value(t, id, artist_as_album_artist)
}

pub(super) fn ml_sort_key(t: &sparkamp::media_library::LibTrack, col: &str) -> String {
    match col {
        "num" => t.sort_keys.num.clone(),
        "title" => t.sort_keys.title.clone(),
        "artist" => t.sort_keys.artist.clone(),
        "album" => t.sort_keys.album.clone(),
        "duration" => t.sort_keys.duration.clone(),
        "filename" => t.sort_keys.filename.clone(),
        "year" => t.sort_keys.year.clone(),
        "genre" => t.sort_keys.genre.clone(),
        "bitrate" => t.sort_keys.bitrate.clone(),
        "channels" => format!("{:02}", t.channels.unwrap_or(0)),
        "sample_rate" => format!("{:010}", t.sample_rate.unwrap_or(0)),
        "file_size" => format!("{:010}", t.file_size.unwrap_or(0)),
        "added_at" => t.added_at.clone().unwrap_or_default(),
        "file_mtime" => t.file_mtime.clone().unwrap_or_default(),
        "bitrate_mode" => t.bitrate_mode.as_deref().unwrap_or("").to_lowercase(),
        "path" => t.path.to_lowercase(),
        "play_count" => format!("{:010}", t.play_count),
        "last_played" => t.last_played.clone().unwrap_or_default(),
        "last_scanned" => t.last_scanned.clone().unwrap_or_default(),
        "comment" => t.sort_keys.comment.clone(),
        "album_artist" => t.sort_keys.album_artist.clone(),
        "disc_num" => format!("{:010}", t.disc_num.unwrap_or(0)),
        "disc_total" => format!("{:010}", t.disc_total.unwrap_or(0)),
        "composer" => t.sort_keys.composer.clone(),
        "original_artist" => t.original_artist.as_deref().unwrap_or("").to_lowercase(),
        "copyright" => t.copyright.as_deref().unwrap_or("").to_lowercase(),
        "url" => t.url.as_deref().unwrap_or("").to_lowercase(),
        "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_lowercase(),
        "bpm" => t.bpm.as_deref().unwrap_or("").to_lowercase(),
        "lyric" => t.lyric.as_deref().unwrap_or("").to_lowercase(),
        "artwork_path" => t.artwork_path.as_deref().unwrap_or("").to_lowercase(),
        // Unanalyzed tracks ("0" prefix) sort before every real gain ("1"
        // prefix) regardless of direction — same convention as the other
        // "no data yet" columns above, which key off `unwrap_or_default()`
        // landing before real text. A plain `{:012.4}` zero-pad on the raw
        // (possibly negative) dB value would NOT sort correctly here: Rust's
        // sign-aware zero-padding keeps the `-` in front and pads after it,
        // so e.g. -12.5 ("-000012.5000") sorts AFTER -6.2 ("-000006.2000")
        // lexically — backwards. Shifting by +1000 first keeps every
        // realistic gain positive, where fixed-width zero-padding sorts
        // correctly.
        "rg_gain" => t
            .rg_track_gain
            .map(|g| format!("1{:012.4}", g + 1000.0))
            .unwrap_or_else(|| "0".to_string()),
        _ => String::new(),
    }
}


// ---------------------------------------------------------------------------
// The artwork column's cell — shared by every view that renders ALL_COLUMNS
// ---------------------------------------------------------------------------

/// Edge length of the cached cover thumbnails the artwork column paints.
pub(super) const ML_ARTWORK_THUMB_PX: i32 = 40;

/// Builds and binds the artwork cell for a `ColumnView` over library tracks.
///
/// One per view, because the two caches it holds are per-view: which buttons
/// already carry a click handler, and which thumbnails are mid-generation.
///
/// This exists because the three views that render `ALL_COLUMNS` had drifted
/// into three different artwork cells. Files painted a real thumbnail; the
/// playlist editor and the device view each showed a "View" text button, and
/// all three carried a comment saying they mirrored the files view — true
/// when written, and untrue from the moment Files gained thumbnails
/// (2026-08-10). Sharing the cell is the only way that stays fixed.
pub(super) struct ArtworkCells {
    /// Buttons whose click handler is already wired for their current row.
    handlers: Rc<RefCell<std::collections::HashMap<glib::Object, glib::SignalHandlerId>>>,
    /// Source images with a thumbnail generation already running, so a
    /// re-bind while scrolling doesn't spawn the same decode twice.
    inflight: Rc<RefCell<std::collections::HashSet<PathBuf>>>,
}

impl ArtworkCells {
    pub(super) fn new() -> Self {
        ArtworkCells {
            handlers: Rc::new(RefCell::new(std::collections::HashMap::new())),
            inflight: Rc::new(RefCell::new(std::collections::HashSet::new())),
        }
    }

    /// The cell widget: a flat button wrapping the thumbnail image. Blank
    /// until [`Self::bind`] finds or generates the cached PNG.
    pub(super) fn setup(&self) -> Button {
        let img = Image::builder().pixel_size(ML_ARTWORK_THUMB_PX).build();
        Button::builder()
            .child(&img)
            .margin_start(6)
            .margin_end(6)
            .margin_top(3)
            .margin_bottom(3)
            .hexpand(true)
            .vexpand(true)
            .halign(Align::Fill)
            .valign(Align::Fill)
            .build()
    }

    /// Paint `art_path` into this row's cell and wire its click.
    ///
    /// `current_art` reads the artwork path off a `ListItem` as it stands
    /// *now*; the views differ in what their store holds (a `LibTrack` in
    /// Files and the device view, an `EditorEntry` in the playlist editor),
    /// so each supplies its own reader. It is called after the off-thread
    /// decode finishes, to confirm the recycled cell still shows the same
    /// track before painting — without it, scrolling fast paints thumbnails
    /// onto whatever row happens to occupy the cell by then.
    pub(super) fn bind(
        &self,
        li: &gtk4::ListItem,
        art_path: Option<&str>,
        current_art: impl Fn(&gtk4::ListItem) -> Option<String> + 'static,
    ) {
        let Some(btn) = li.child().and_then(|c| c.downcast::<Button>().ok()) else {
            return;
        };
        let Some(art_path) = art_path else {
            btn.set_visible(false);
            return;
        };
        btn.set_visible(true);
        btn.set_sensitive(true);

        // Reconnect on every bind rather than once per button. `ColumnView`
        // recycles cells, so a handler that captured the first row's path
        // would keep opening that image for every later row in the same cell.
        let key = btn.clone().upcast::<glib::Object>();
        if let Some(old) = self.handlers.borrow_mut().remove(&key) {
            btn.disconnect(old);
        }
        let art_for_click = art_path.to_string();
        let id = btn.connect_clicked(move |_| open_image_viewer(&art_for_click));
        self.handlers.borrow_mut().insert(key, id);

        let Some(img) = btn.child().and_then(|c| c.downcast::<Image>().ok()) else {
            return;
        };
        let src = PathBuf::from(art_path);
        let Some(thumb) = sparkamp::now_playing::thumb_path_for(&src, ML_ARTWORK_THUMB_PX as u32)
        else {
            return;
        };
        if thumb.exists() {
            img.set_from_file(Some(&thumb));
            return;
        }
        // Not cached yet: blank the cell so a recycled thumbnail isn't left
        // showing, then generate off the main thread.
        img.clear();
        if !self.inflight.borrow_mut().insert(src.clone()) {
            return; // already generating for this source
        }
        let inflight = self.inflight.clone();
        let img_wk = img.downgrade();
        let li_wk = li.downgrade();
        glib::spawn_future_local(async move {
            let src_blk = src.clone();
            let thumb_blk = thumb.clone();
            let ok = gio::spawn_blocking(move || -> Result<(), ()> {
                if let Some(parent) = thumb_blk.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(
                    &src_blk,
                    ML_ARTWORK_THUMB_PX,
                    ML_ARTWORK_THUMB_PX,
                    true,
                )
                .map_err(|_| ())?;
                pixbuf.savev(&thumb_blk, "png", &[]).map_err(|_| ())
            })
            .await;
            // Done either way — stop treating this source as in flight.
            inflight.borrow_mut().remove(&src);
            if !matches!(ok, Ok(Ok(()))) {
                return; // decode or encode failed; leave the cell blank
            }
            let (Some(li), Some(img)) = (li_wk.upgrade(), img_wk.upgrade()) else {
                return;
            };
            if current_art(&li).as_deref() == src.to_str() {
                img.set_from_file(Some(&thumb));
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Visualizer draw helpers (module-level so both build() and open_waveform_fullscreen can use them)
// ---------------------------------------------------------------------------

