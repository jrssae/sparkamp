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
//! holds `gio::File`s and `dnd.rs` filters them through `.path()`, which is
//! `None` for a `cdda://` URI, so a disc drag carried in a `FileList` would
//! arrive empty. Strings carry both. The provider still offers `FileList` too,
//! so dragging library tracks out to a file manager keeps working.

use super::*;

/// Content type for a Sparkamp drag: URIs, one per line.
///
/// Newline-separated because `gdk::ContentProvider::for_value` takes a single
/// `Value` and a `Vec<String>` has no `ToValue`. URIs cannot contain a raw
/// newline, so the join is unambiguous.
///
/// Unused for now: `attach_uri_drag` negotiates the content type from the
/// `Value`'s GType rather than this string, so nothing reads it until a drop
/// target (the next task) registers it explicitly. Remove the allow there.
#[allow(dead_code)]
pub(super) const SPARKAMP_URIS_MIME: &str = "application/x-sparkamp-uris";

/// Make `widget` draggable, publishing whatever `uris` returns at drag time.
///
/// `uris` is called on every drag, not once at setup, so it reads the
/// selection as it is when the drag starts.
///
/// The first caller lands in a later task (attaching this to the album
/// gallery, disc views and device views); nothing calls it yet.
#[allow(dead_code)]
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
/// Exercised by this module's tests but not yet called from production code;
/// the first caller (the active playlist's drop target) lands in the next
/// task. Remove this allow there.
#[allow(dead_code)]
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
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
    }
    Vec::new()
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
}
