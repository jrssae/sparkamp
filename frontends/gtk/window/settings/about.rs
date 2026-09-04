//! The About tab: version, engine, licence and links.
//!
//! Split out of `open_settings_window`, which was one 2,775-line function
//! holding every tab inline. The body is unchanged; what it used to close
//! over arrives as arguments.

use super::*;

pub(super) fn build(notebook: &Notebook) {
        let outer = GtkBox::new(Orientation::Vertical, 16);
        outer.set_margin_top(24);
        outer.set_margin_bottom(24);
        outer.set_margin_start(24);
        outer.set_margin_end(24);

        // Header: title + version + description.
        let header = GtkBox::new(Orientation::Vertical, 4);

        let title = Label::new(Some("Sparkamp"));
        title.set_halign(Align::Start);
        title.add_css_class("about-title");
        header.append(&title);

        let version = Label::new(Some(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
        version.set_halign(Align::Start);
        version.add_css_class("about-subtle");
        header.append(&version);

        let desc = Label::new(Some(
            "A compact, fast, open-source Winamp-style music player with the \
             backend built in Rust and support for UI in GNOME desktop with \
             GTK4 & macOS with Swift.",
        ));
        desc.set_halign(Align::Start);
        desc.set_xalign(0.0);
        desc.set_wrap(true);
        desc.set_max_width_chars(60);
        desc.add_css_class("about-subtle");
        header.append(&desc);

        outer.append(&header);
        outer.append(&gtk4::Separator::new(Orientation::Horizontal));

        // Engine.
        let engine_box = GtkBox::new(Orientation::Vertical, 4);
        let engine_h = Label::new(Some("Engine"));
        engine_h.set_halign(Align::Start);
        engine_h.add_css_class("about-section");
        let engine_b = Label::new(Some("GStreamer: playbin, equalizer-10bands, volume"));
        engine_b.set_halign(Align::Start);
        engine_b.add_css_class("about-subtle");
        engine_box.append(&engine_h);
        engine_box.append(&engine_b);
        outer.append(&engine_box);

        // License.
        let license_box = GtkBox::new(Orientation::Vertical, 4);
        let license_h = Label::new(Some("License"));
        license_h.set_halign(Align::Start);
        license_h.add_css_class("about-section");
        let license_link = gtk4::LinkButton::with_label(
            "https://www.gnu.org/licenses/agpl-3.0.html",
            "GNU Affero General Public License v3 (AGPL-3.0)",
        );
        license_link.set_halign(Align::Start);
        // Sections 15 and 16 of the AGPL already say this and nobody reads
        // them. Saying it in plain words costs a line and belongs in front of
        // someone before they point the app at their music.
        let warranty = Label::new(Some(
            "Sparkamp is made in good faith and comes with no warranty. If it \
             loses data or breaks something, that risk is yours. Sections 15 \
             and 16 of the licence say this in legal terms.",
        ));
        warranty.set_halign(Align::Start);
        warranty.set_xalign(0.0);
        warranty.set_wrap(true);
        warranty.set_max_width_chars(60);
        warranty.add_css_class("about-subtle");
        license_box.append(&license_h);
        license_box.append(&license_link);
        license_box.append(&warranty);
        outer.append(&license_box);

        // GitHub.
        let gh_box = GtkBox::new(Orientation::Vertical, 4);
        let gh_h = Label::new(Some("Get the latest"));
        gh_h.set_halign(Align::Start);
        gh_h.add_css_class("about-section");
        let gh_b = Label::new(Some(
            "Source code, releases, and issue tracking are hosted on GitHub. \
             Clone the repository or grab the latest build there.",
        ));
        gh_b.set_halign(Align::Start);
        gh_b.set_xalign(0.0);
        gh_b.set_wrap(true);
        gh_b.set_max_width_chars(60);
        gh_b.add_css_class("about-subtle");
        let gh_link = gtk4::LinkButton::with_label(
            "https://github.com/jrssae/sparkamp",
            "github.com/jrssae/sparkamp",
        );
        gh_link.set_halign(Align::Start);
        gh_box.append(&gh_h);
        gh_box.append(&gh_b);
        gh_box.append(&gh_link);
        outer.append(&gh_box);

        // Nominative use of another product's name is fine; saying so plainly
        // is what lowers the risk. A trademark symbol would not, because those
        // are used by the owner of a mark, so printing one here would read as
        // Sparkamp claiming it.
        let trademark = Label::new(Some(
            "Winamp is a trademark of its respective owner. Sparkamp is an \
             independent project, not affiliated with or endorsed by them.",
        ));
        trademark.set_halign(Align::Start);
        trademark.set_xalign(0.0);
        trademark.set_wrap(true);
        trademark.set_max_width_chars(60);
        trademark.add_css_class("about-subtle");
        outer.append(&trademark);

        // Reachable from inside the app rather than only from a store listing
        // or the repository.
        let privacy_link = gtk4::LinkButton::with_label(
            "https://github.com/jrssae/sparkamp/blob/main/PRIVACY.md",
            "Privacy Policy",
        );
        privacy_link.set_halign(Align::Start);
        outer.append(&privacy_link);

        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .child(&outer)
            .build();

        let tab_lbl = Label::with_mnemonic(SETTINGS_TAB_LABELS[4]);
        notebook.append_page(&scroll, Some(&tab_lbl));
        // Move About to leftmost position.
        notebook.reorder_child(&scroll, Some(0));
}
