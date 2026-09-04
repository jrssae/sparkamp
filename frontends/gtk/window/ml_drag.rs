//! One drag source for every Media Library view.
//!
//! Before this, only the Files table and the playlist editor were draggable;
//! the album gallery, the disc views and the device views were not. Each new
//! source needs the same three things — collect what is selected, publish it,
//! and let the active playlist's drop target read it back — so they share one
//! helper rather than five near-copies.
//!
//! ## Why URIs and not `gdk::FileList`
//!
//! A CD track on Linux is `cdda://5?device=/dev/sr0`, not a file. `FileList`
//! holds `gio::File`s, and handing that string to `gio::File::for_path` does
//! not fail — GLib treats the scheme-looking text as a *relative* path,
//! prepends the current working directory, and collapses the `//` down to
//! `/`, silently mangling it into a bogus path like
//! `<cwd>/cdda:/5?device=/dev/sr0` instead of rejecting it. `.path()` then
//! returns `Some` of that garbage rather than `None`, so a disc drag carried
//! in a `FileList` would arrive looking valid and fail later. Strings carry
//! both correctly. The provider still offers `FileList` too, so dragging
//! library tracks out to a file manager keeps working.

use super::*;

/// Make `widget` draggable, publishing whatever `uris` returns at drag time.
///
/// `uris` is called on every drag, not once at setup, so it reads the
/// selection as it is when the drag starts.
pub(super) fn attach_uri_drag<W, F>(widget: &W, uris: F)
where
    W: IsA<gtk4::Widget>,
    F: Fn() -> Vec<String> + 'static,
{
    let ds = gtk4::DragSource::new();
    ds.set_actions(gdk::DragAction::COPY);
    ds.connect_prepare(move |_, _, _| {
        let list = uris();
        if list.is_empty() {
            // No selection: refuse the drag rather than starting an empty one.
            return None;
        }
        let joined = list.join("\n");
        let text_provider = gdk::ContentProvider::for_value(&joined.to_value());

        // Offer a FileList as well, for the paths that are real files, so a
        // drag out to a file manager still works. Skipped entirely when the
        // selection is all pseudo-URIs (a CD), since an empty FileList would
        // advertise a type that yields nothing.
        let files: Vec<gio::File> = list
            .iter()
            .filter(|u| !u.contains("://"))
            .map(gio::File::for_path)
            .collect();
        if files.is_empty() {
            return Some(text_provider);
        }
        let file_provider =
            gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value());
        Some(gdk::ContentProvider::new_union(&[
            text_provider,
            file_provider,
        ]))
    });
    widget.as_ref().add_controller(ds);
}

/// Read a dropped value back into URIs, accepting either content type.
///
/// Returns an empty Vec for a value that is neither, which callers treat as
/// "not for us" and decline.
///
/// Exercised by this module's tests and by the active playlist's drop target.
pub(super) fn uris_from_value(value: &glib::Value) -> Vec<String> {
    if let Ok(joined) = value.get::<String>() {
        return joined
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
    }
    if let Ok(fl) = value.get::<gdk::FileList>() {
        return fl
            .files()
            .iter()
            .filter_map(|f| f.path())
            // Known limitation, not an oversight: `to_string_lossy` mangles a
            // non-UTF-8 file name into `\u{FFFD}` replacement characters, so
            // that one dropped path no longer equals its `Track::path` and a
            // cross-window reorder drop falls back to treating it as a new
            // file instead of recognising the move. A byte-faithful fix would
            // need this function (and `is_playable_uri`, and every
            // `PathBuf::from(uri)` call site downstream) to carry `OsString`
            // instead of `String` — but `String` is also what the sibling
            // text-payload branch above requires, since a CD track's
            // `cdda://` pseudo-URI has to share this same `Vec<_>` and travel
            // over one `glib::Value` of `G_TYPE_STRING` (see this module's
            // top doc comment). Widening the type is a real restructure of
            // that shared design, not a one-line fix, so it's left
            // documented rather than done here. Non-UTF-8 filenames are rare
            // in practice.
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
    }
    Vec::new()
}

/// Expand a dropped `pl:<id>` payload into the track paths of that playlist.
///
/// A playlist row's own drag source (`util::attach_pl_row_drag`) publishes a
/// single `pl:<id>` string, not track URIs — that payload is also what a
/// device drop target reads to sync the whole playlist. When the *playlist*
/// is the drop target instead, `pl:<id>` needs to mean "add everything this
/// playlist holds", so it is resolved here and expanded before the normal
/// `is_playable_uri` / `dispatch_add` path runs, exactly as if the caller had
/// dragged out every one of its tracks individually.
///
/// Anything that isn't exactly one `pl:<id>` entry — a normal file/URI drop,
/// or (defensively) a multi-entry payload — passes through untouched. A
/// `pl:<id>` whose playlist no longer exists also passes through unchanged;
/// it fails `is_playable_uri` downstream and the drop is declined, same as
/// before this function existed.
pub(super) fn expand_playlist_drop(
    lib: Option<&sparkamp::media_library::MediaLibrary>,
    uris: Vec<String>,
) -> Vec<String> {
    let [only] = uris.as_slice() else {
        return uris;
    };
    let Some(id) = only
        .strip_prefix("pl:")
        .and_then(|n| n.trim().parse::<i64>().ok())
    else {
        return uris;
    };
    let Some(lib) = lib else { return uris };
    let Ok(all) = lib.all_playlists() else {
        return uris;
    };
    let Some(pl) = all.into_iter().find(|p| p.id == id) else {
        return uris;
    };
    lib.load_playlist_tracks(&pl)
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.path)
        .collect()
}

/// Whether `uri` looks like something the playlist can actually hold.
///
/// An absolute filesystem path, or a pseudo-URI whose scheme the engine
/// understands (`cdda://` for a CD track — see `parse_cdda_uri`). Everything
/// a Sparkamp drag produces passes; dropped prose does not.
///
/// Needed because the drop target accepts a bare `glib::Type::STRING` and
/// GTK negotiates by GType, not by mime — `text/plain` from any application
/// deserializes to a string as well, so the type alone cannot tell a track
/// list from a paragraph.
pub(super) fn is_playable_uri(uri: &str) -> bool {
    uri.starts_with('/') || uri.starts_with("cdda://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_joined_payload_splits_back_into_its_uris() {
        let v = "/m/a.mp3\ncdda://5?device=/dev/sr0\n/m/b.mp3".to_string().to_value();
        assert_eq!(
            uris_from_value(&v),
            vec![
                "/m/a.mp3".to_string(),
                "cdda://5?device=/dev/sr0".to_string(),
                "/m/b.mp3".to_string()
            ]
        );
    }

    #[test]
    fn blank_lines_in_a_payload_are_dropped() {
        let v = "/m/a.mp3\n\n\n/m/b.mp3\n".to_string().to_value();
        assert_eq!(
            uris_from_value(&v),
            vec!["/m/a.mp3".to_string(), "/m/b.mp3".to_string()]
        );
    }

    #[test]
    fn a_value_of_neither_type_yields_nothing() {
        let v = 42i32.to_value();
        assert!(uris_from_value(&v).is_empty());
    }

    #[test]
    fn an_absolute_path_is_playable() {
        assert!(is_playable_uri("/m/a.mp3"));
    }

    #[test]
    fn a_cdda_uri_is_playable() {
        assert!(is_playable_uri("cdda://5?device=/dev/sr0"));
    }

    #[test]
    fn a_bare_word_is_not_playable() {
        assert!(!is_playable_uri("hello"));
    }

    #[test]
    fn a_relative_path_is_not_playable() {
        assert!(!is_playable_uri("music/a.mp3"));
    }

    #[test]
    fn an_https_url_is_not_playable() {
        assert!(!is_playable_uri("https://example.com/a.mp3"));
    }

    #[test]
    fn a_playlist_payload_expands_to_its_track_paths() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let lib = sparkamp::media_library::MediaLibrary::open_at(db_file.path()).unwrap();
        let list_dir = tempfile::tempdir().unwrap();
        let list_path = list_dir.path().join("road_trip.m3u8");
        let id = lib
            .save_playlist_tracks_to_path(
                &list_path,
                &["/music/a.mp3".to_string(), "/music/b.mp3".to_string()],
            )
            .unwrap();

        let expanded = expand_playlist_drop(Some(&lib), vec![format!("pl:{id}")]);
        assert_eq!(
            expanded,
            vec!["/music/a.mp3".to_string(), "/music/b.mp3".to_string()]
        );
    }

    #[test]
    fn a_payload_that_is_not_pl_id_passes_through_untouched() {
        let uris = vec!["/music/a.mp3".to_string()];
        assert_eq!(expand_playlist_drop(None, uris.clone()), uris);

        let cdda = vec!["cdda://5?device=/dev/sr0".to_string()];
        assert_eq!(expand_playlist_drop(None, cdda.clone()), cdda);
    }

    #[test]
    fn a_multi_entry_payload_is_never_mistaken_for_a_playlist_id() {
        // Defensive: no current drag source publishes `pl:<id>` alongside
        // other entries, but if one ever did, it must not be expanded.
        let uris = vec!["pl:1".to_string(), "/music/a.mp3".to_string()];
        assert_eq!(expand_playlist_drop(None, uris.clone()), uris);
    }
}
