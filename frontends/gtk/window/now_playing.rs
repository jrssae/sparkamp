//! A1 expandable now-playing panel — art + curated tags + tech line + play
//! stats + Wikipedia links.  Child module of [`super`] (window.rs): swapped
//! in for the marquee frame when the panel is expanded (`w` key / mode
//! button), built in `player.rs`.
//!
//! `build_panel` constructs the widget tree once; the returned update
//! closure is registered with `AppState::subscribe_now_playing` so every
//! track change repopulates the same widgets in place (no rebuild, no
//! reparenting) rather than fighting the Stack's child identity.

use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, GestureClick, Image, Label, LinkButton, Orientation, Picture,
    PolicyType, ScrolledWindow,
};
use std::rc::Rc;

use crate::now_playing::NowPlayingInfo;

/// Art / placeholder square side length. Fixed so re-population (which
/// swaps the art widget) never nudges the panel's overall size.
const ART_SIZE: i32 = 200;

/// Build the panel widget tree and its update closure.
///
/// `info` seeds the panel immediately if a track is already playing when the
/// panel is built (see `AppState::current_now_playing` — the subscriber
/// fan-out alone only fires on the *next* track change, which would leave a
/// mid-playback toggle empty). `on_art_click` fires when the art (or its
/// placeholder) is clicked; T7 wires the real A6-window opener here, so an
/// empty closure is fine until then.
pub(super) fn build_panel(
    info: Option<&NowPlayingInfo>,
    on_art_click: Rc<dyn Fn()>,
) -> (gtk4::Widget, Rc<dyn Fn(&NowPlayingInfo)>) {
    let root = GtkBox::new(Orientation::Horizontal, 10);
    root.add_css_class("np-panel");
    root.set_hexpand(true);
    root.set_vexpand(true);

    // Left: art / placeholder. A fixed-size wrapper box so swapping its
    // child on repopulate never changes the panel's layout.
    let art_slot = GtkBox::new(Orientation::Vertical, 0);
    art_slot.set_size_request(ART_SIZE, ART_SIZE);
    art_slot.set_valign(Align::Start);
    {
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| on_art_click());
        art_slot.add_controller(click);
    }

    // Right: scrollable column of tag / tech / stats / link rows. Track
    // count varies (curated tags are filtered to non-empty already, links
    // are optional), so the column can run taller than the art — scroll
    // rather than clip.
    let tag_col = GtkBox::new(Orientation::Vertical, 4);
    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .hexpand(true)
        .vexpand(true)
        .child(&tag_col)
        .build();

    root.append(&art_slot);
    root.append(&scroller);

    let update: Rc<dyn Fn(&NowPlayingInfo)> = {
        let art_slot = art_slot.clone();
        let tag_col = tag_col.clone();
        Rc::new(move |info: &NowPlayingInfo| populate(&art_slot, &tag_col, info))
    };

    if let Some(info) = info {
        update(info);
    }

    (root.upcast::<gtk4::Widget>(), update)
}

/// Clear and refill `art_slot` + `tag_col` from `info`. Rows are rebuilt
/// wholesale rather than diffed — the row count changes track to track
/// (optional tech line, optional wiki links), and this only runs once per
/// track change, not per frame.
fn populate(art_slot: &GtkBox, tag_col: &GtkBox, info: &NowPlayingInfo) {
    while let Some(child) = art_slot.first_child() {
        art_slot.remove(&child);
    }
    while let Some(child) = tag_col.first_child() {
        tag_col.remove(&child);
    }

    art_slot.append(&art_or_placeholder(info));

    for (label, value) in &info.tags {
        tag_col.append(&tag_row(label, value));
    }
    if !info.tech_line.is_empty() {
        tag_col.append(&text_row(&info.tech_line));
    }
    if let Some(count) = info.play_count {
        tag_col.append(&tag_row("Play count", &count.to_string()));
    }
    if let Some(ref last) = info.last_played {
        tag_col.append(&tag_row("Last played", &super::format_last_played(last)));
    }
    if let Some(ref url) = info.artist_wiki_url {
        tag_col.append(&wiki_row("Artist on Wikipedia", url));
    }
    if let Some(ref url) = info.album_wiki_url {
        tag_col.append(&wiki_row("Album on Wikipedia", url));
    }
}

/// The art widget for `info`: a `Picture` loaded from `artwork_path` when
/// present, otherwise the app logo at 50% opacity + "No artwork available".
/// Exposed for T7 (A6 art window) to reuse the identical placeholder.
pub(super) fn art_or_placeholder(info: &NowPlayingInfo) -> gtk4::Widget {
    match info.artwork_path.as_ref() {
        Some(path) => {
            let pic = Picture::new();
            pic.set_width_request(ART_SIZE);
            pic.set_height_request(ART_SIZE);
            pic.set_can_shrink(true);
            pic.set_content_fit(gtk4::ContentFit::Contain);
            pic.set_filename(Some(path));
            pic.add_css_class("np-art");
            pic.upcast()
        }
        None => placeholder_widget(),
    }
}

/// 50%-opacity logo + "No artwork available" — matches the A6 art-window
/// placeholder (both draw from the same embedded `LOGO_BYTES`).
fn placeholder_widget() -> gtk4::Widget {
    let wrap = GtkBox::new(Orientation::Vertical, 6);
    wrap.set_size_request(ART_SIZE, ART_SIZE);
    wrap.set_valign(Align::Center);
    wrap.set_halign(Align::Center);
    wrap.add_css_class("np-placeholder");

    let logo_px = ART_SIZE - 60;
    let img = Image::new();
    img.set_pixel_size(logo_px);
    img.set_valign(Align::Center);
    img.set_halign(Align::Center);
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

/// A "Label: value" row (curated tags, play count, last played).
fn tag_row(label: &str, value: &str) -> gtk4::Widget {
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.add_css_class("np-tag-row");

    let key = Label::new(Some(&format!("{label}:")));
    key.set_halign(Align::Start);
    key.set_xalign(0.0);
    key.set_width_chars(12);

    let val = Label::new(Some(value));
    val.set_halign(Align::Start);
    val.set_xalign(0.0);
    val.set_hexpand(true);
    val.set_wrap(true);

    row.append(&key);
    row.append(&val);
    row.upcast()
}

/// A single full-width line (the tech-summary string).
fn text_row(text: &str) -> gtk4::Widget {
    let row = GtkBox::new(Orientation::Horizontal, 0);
    row.add_css_class("np-tag-row");

    let lbl = Label::new(Some(text));
    lbl.set_halign(Align::Start);
    lbl.set_xalign(0.0);
    lbl.set_hexpand(true);
    lbl.set_wrap(true);

    row.append(&lbl);
    row.upcast()
}

/// A Wikipedia `LinkButton` row.
fn wiki_row(label: &str, url: &str) -> gtk4::Widget {
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.add_css_class("np-tag-row");

    let link = LinkButton::with_label(url, label);
    link.add_css_class("np-link");
    link.set_halign(Align::Start);

    row.append(&link);
    row.upcast()
}
