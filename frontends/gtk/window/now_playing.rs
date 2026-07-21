//! A1 expandable now-playing panel — album art on the left, and on the right
//! an auto-cycling carousel that rotates through data groups (Tags →
//! Technical → Stats → Links) a few seconds apart, with page dots below.
//! Child module of [`super`] (window.rs): swapped in for the marquee frame
//! when the panel is expanded (`w` key / mode button), built in `player.rs`.
//!
//! `build_panel` constructs the widget tree once and starts one cycle timer;
//! the returned update closure is registered with
//! `AppState::subscribe_now_playing` so every track change rebuilds the
//! carousel's pages in place (no reparenting of the panel itself).

use gtk4::prelude::*;
use gtk4::{
    gdk, gdk_pixbuf, glib, Align, Box as GtkBox, GestureClick, Image, Label, LinkButton,
    Orientation, Picture, PolicyType, ScrolledWindow, Stack, StackTransitionType,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::now_playing::NowPlayingInfo;

/// Art / placeholder square side length. Fixed so re-population (which
/// swaps the art widget) never nudges the panel's overall size.
const ART_SIZE: i32 = 100;

/// How long each carousel page stays up before advancing.
const CAROUSEL_INTERVAL: Duration = Duration::from_secs(6);

/// How often the cycle timer checks whether it is time to advance. A poll
/// (rather than a fixed advance interval) lets a dot click push the next
/// advance out without tearing down and rebuilding the timer source.
const CAROUSEL_POLL: Duration = Duration::from_secs(1);

/// How many tag rows fit on one carousel page before spilling onto the next,
/// so a metadata-rich file (comment, composer, …) gets extra pages/dots
/// instead of a single scrolling wall of text.
const ROWS_PER_TAG_PAGE: usize = 4;

/// Mutable carousel state shared between the update closure (which rebuilds
/// pages on every track change) and the cycle timer (which advances them).
/// All access is on the GTK main thread, so borrows are always short and
/// non-overlapping — never held across a GTK call that could re-enter.
struct Carousel {
    stack: Stack,
    dots: GtkBox,
    index: usize,
    pages: usize,
    /// When the timer should next auto-advance. A dot click pushes this out
    /// (reset + doubled dwell) so a manual pick lingers.
    next_advance: Instant,
}

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

    // Right: the carousel. A crossfading Stack of data-group pages (fixed to
    // the art height so it stays compact — each page scrolls internally if it
    // overruns) above a centered row of page dots.
    let stack = Stack::builder()
        .transition_type(StackTransitionType::Crossfade)
        .hexpand(true)
        .vexpand(true)
        .build();
    stack.set_vhomogeneous(false);
    stack.set_size_request(-1, ART_SIZE);

    let dots = GtkBox::new(Orientation::Horizontal, 4);
    dots.set_halign(Align::Center);
    dots.add_css_class("np-dots");

    let right = GtkBox::new(Orientation::Vertical, 2);
    right.set_hexpand(true);
    right.set_vexpand(true);
    right.append(&stack);
    right.append(&dots);

    root.append(&art_slot);
    root.append(&right);

    let carousel = Rc::new(RefCell::new(Carousel {
        stack,
        dots,
        index: 0,
        pages: 0,
        next_advance: Instant::now() + CAROUSEL_INTERVAL,
    }));

    let update: Rc<dyn Fn(&NowPlayingInfo)> = {
        let art_slot = art_slot.clone();
        let carousel = carousel.clone();
        Rc::new(move |info: &NowPlayingInfo| populate(&art_slot, &carousel, info))
    };

    if let Some(info) = info {
        update(info);
    }

    // One cycle timer for the panel's lifetime (the panel lives in np_stack
    // and is never destroyed). Polls once a second and advances when the
    // dwell has elapsed; a no-op while fewer than two pages exist.
    {
        let carousel = carousel.clone();
        glib::timeout_add_local(CAROUSEL_POLL, move || {
            tick(&carousel);
            glib::ControlFlow::Continue
        });
    }

    (root.upcast::<gtk4::Widget>(), update)
}

/// Auto-advance the carousel once its dwell has elapsed. Borrow is dropped
/// before the GTK calls that read/update the widgets — those never re-enter
/// the RefCell.
fn tick(carousel: &Rc<RefCell<Carousel>>) {
    let (stack, dots, next) = {
        let mut c = carousel.borrow_mut();
        if c.pages < 2 || Instant::now() < c.next_advance {
            return;
        }
        c.index = (c.index + 1) % c.pages;
        c.next_advance = Instant::now() + CAROUSEL_INTERVAL;
        (c.stack.clone(), c.dots.clone(), c.index)
    };
    stack.set_visible_child_name(&next.to_string());
    set_active_dot(&dots, next);
}

/// Jump directly to page `to` (clicked dot). Same borrow discipline as
/// `advance`: read what's needed, drop the borrow, then touch the widgets.
fn jump(carousel: &Rc<RefCell<Carousel>>, to: usize) {
    let (stack, dots) = {
        let mut c = carousel.borrow_mut();
        if to >= c.pages {
            return;
        }
        c.index = to;
        // A manual pick resets the dwell and doubles it, so the chosen page
        // lingers instead of flipping away a moment later.
        c.next_advance = Instant::now() + CAROUSEL_INTERVAL * 2;
        (c.stack.clone(), c.dots.clone())
    };
    stack.set_visible_child_name(&to.to_string());
    set_active_dot(&dots, to);
}

/// Rebuild the art and the carousel pages from `info`. Pages are rebuilt
/// wholesale (once per track change, not per frame); only groups with content
/// get a page, so an empty-tag file simply shows fewer pages.
fn populate(art_slot: &GtkBox, carousel: &Rc<RefCell<Carousel>>, info: &NowPlayingInfo) {
    while let Some(child) = art_slot.first_child() {
        art_slot.remove(&child);
    }
    art_slot.append(&art_or_placeholder(info));

    // Build the page widgets for whichever groups have data. Tags spill onto
    // extra pages (ROWS_PER_TAG_PAGE each) so a metadata-rich file shows all
    // its fields across multiple dots rather than one scrolling page.
    let mut pages: Vec<gtk4::Widget> = Vec::new();

    for chunk in info.tags.chunks(ROWS_PER_TAG_PAGE) {
        let col = GtkBox::new(Orientation::Vertical, 4);
        for (label, value) in chunk {
            col.append(&tag_row(label, value));
        }
        pages.push(page_scroller(&col));
    }
    // Technical tab: discrete format/bitrate/sample-rate/channels rows (label/
    // value, like the tags). The length is omitted (the seek bar shows it).
    if !info.technical.is_empty() {
        let col = GtkBox::new(Orientation::Vertical, 4);
        for (label, value) in &info.technical {
            col.append(&tag_row(label, value));
        }
        pages.push(page_scroller(&col));
    }
    // Stats tab: play count, last played (as of this play's start), last scanned.
    if info.play_count.is_some() || info.last_played.is_some() || info.last_scanned.is_some() {
        let col = GtkBox::new(Orientation::Vertical, 4);
        if let Some(count) = info.play_count {
            col.append(&tag_row("Play count", &count.to_string()));
        }
        if let Some(ref last) = info.last_played {
            col.append(&tag_row("Last played", &super::format_last_played(last)));
        }
        if let Some(ref scanned) = info.last_scanned {
            col.append(&tag_row("Last scanned", &super::format_last_played(scanned)));
        }
        pages.push(page_scroller(&col));
    }
    if info.artist_wiki_url.is_some() || info.album_wiki_url.is_some() {
        let col = GtkBox::new(Orientation::Vertical, 4);
        if let Some(ref url) = info.artist_wiki_url {
            col.append(&wiki_row("Artist on Wikipedia", url));
        }
        if let Some(ref url) = info.album_wiki_url {
            col.append(&wiki_row("Album on Wikipedia", url));
        }
        pages.push(page_scroller(&col));
    }

    let mut c = carousel.borrow_mut();
    while let Some(child) = c.stack.first_child() {
        c.stack.remove(&child);
    }
    while let Some(child) = c.dots.first_child() {
        c.dots.remove(&child);
    }

    for (i, page) in pages.iter().enumerate() {
        c.stack.add_named(page, Some(&i.to_string()));

        // One dot per page; the current page's is filled. Clicking a dot
        // jumps straight to that page (and lingers there — see `jump`).
        let dot = Label::new(Some(if i == 0 { "●" } else { "○" }));
        dot.add_css_class("np-dot");
        let click = GestureClick::new();
        let carousel_click = carousel.clone();
        click.connect_released(move |_, _, _, _| jump(&carousel_click, i));
        dot.add_controller(click);
        c.dots.append(&dot);
    }

    c.pages = pages.len();
    c.index = 0;
    c.next_advance = Instant::now() + CAROUSEL_INTERVAL;
    if c.pages > 0 {
        c.stack.set_visible_child_name("0");
    }
    // Only worth showing dots when there is more than one page to cycle.
    c.dots.set_visible(c.pages > 1);
}

/// Wrap a page's content column in a compact vertical scroller so a page
/// scrolls inside the fixed panel height rather than stretching it (a safety
/// net — tag pages are chunked to fit, but a long wrapped value can still
/// overrun).
fn page_scroller(col: &GtkBox) -> gtk4::Widget {
    ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .hexpand(true)
        .vexpand(true)
        .child(col)
        .build()
        .upcast()
}

/// Fill the dot at `active`, hollow the rest.
fn set_active_dot(dots: &GtkBox, active: usize) {
    let mut i = 0;
    let mut child = dots.first_child();
    while let Some(w) = child {
        let next = w.next_sibling();
        if let Some(lbl) = w.downcast_ref::<Label>() {
            lbl.set_text(if i == active { "●" } else { "○" });
        }
        i += 1;
        child = next;
    }
}

/// The art widget for `info`: a `Picture` loaded from `artwork_path` when
/// present, otherwise the app logo at 50% opacity + "No artwork available".
/// Exposed for T7 (A6 art window) to reuse the identical placeholder.
pub(super) fn art_or_placeholder(info: &NowPlayingInfo) -> gtk4::Widget {
    match info.artwork_path.as_ref() {
        // Load the cover pre-scaled into a fixed-size texture. A plain
        // `Picture::set_filename` keeps the file's full intrinsic size as its
        // natural size (height_request is only a MINIMUM), so a large cover
        // blows past the 100x100 slot. Scaling to ART_SIZE up front caps the
        // texture — and therefore the Picture's natural size — at the slot.
        // (Trade-off: the panel thumbnail is a still frame; the A6 window
        // still shows the full/animated image via set_filename.)
        Some(path) => match gdk_pixbuf::Pixbuf::from_file_at_scale(path, ART_SIZE, ART_SIZE, true) {
            Ok(pb) => {
                let texture = gdk::Texture::for_pixbuf(&pb);
                let pic = Picture::for_paintable(&texture);
                pic.set_can_shrink(true);
                pic.set_content_fit(gtk4::ContentFit::Contain);
                pic.set_valign(Align::Start);
                pic.set_halign(Align::Start);
                pic.add_css_class("np-art");
                pic.upcast()
            }
            Err(_) => placeholder_widget(),
        },
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

    // Wrap + center so the caption stays inside the fixed 100x100 slot instead
    // of overflowing it on one line.
    let lbl = Label::new(Some("No artwork available"));
    lbl.set_opacity(0.5);
    lbl.set_wrap(true);
    lbl.set_justify(gtk4::Justification::Center);
    lbl.set_halign(Align::Center);
    lbl.set_max_width_chars(10);
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
