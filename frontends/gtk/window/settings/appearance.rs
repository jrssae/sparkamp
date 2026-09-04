//! The Appearance tab: skin choice, colours and fonts.
//!
//! Split out of `open_settings_window`, which was one 2,775-line function
//! holding every tab inline. The body is unchanged; what it used to close
//! over arrives as arguments.

use super::*;

pub(super) fn build(notebook: &Notebook, state: &Rc<RefCell<AppState>>, css_provider: &Rc<gtk4::CssProvider>, text_rgba: &Rc<RefCell<gdk::RGBA>>, accent_rgba: &Rc<RefCell<Option<gdk::RGBA>>>, rebuild_playlist: &Rc<dyn Fn()>, win: &gtk4::Window) {
    let state = state.clone();
    let css_provider = css_provider.clone();
    let text_rgba = text_rgba.clone();
    let accent_rgba = accent_rgba.clone();
    let rebuild_playlist = rebuild_playlist.clone();
    let win = win.clone();
        use gtk4::{Box as GtkBox, Button, DropDown, Grid, Label, ListBox, ListBoxRow,
                   Orientation, PolicyType, ScrolledWindow, SelectionMode, Separator,
                   FileDialog, FileFilter};

        let root = GtkBox::new(Orientation::Vertical, 10);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_margin_start(16);
        root.set_margin_end(16);

        // Header
        let header = Label::new(Some("Skin"));
        header.set_halign(Align::Start);
        header.add_css_class("heading");
        root.append(&header);

        // Scrollable list of skins
        let listbox = ListBox::new();
        listbox.set_selection_mode(SelectionMode::Single);
        listbox.add_css_class("rich-list");
        // Hand-built GtkListBox: needs .ml-col-view for the skin's
        // selection/hover colours (house rule) — without it the selected
        // skin row keeps GTK's default accent (phase-1 user-pass finding).
        listbox.add_css_class("ml-col-view");

        let scrolled = ScrolledWindow::new();
        scrolled.set_policy(PolicyType::Never, PolicyType::Automatic);
        scrolled.set_min_content_height(200);
        scrolled.set_child(Some(&listbox));
        root.append(&scrolled);

        // Suppress the row_selected handler while we programmatically
        // re-select the active row after rebuild. GtkNotebook tab switches
        // can also fire spurious row_selected events on re-show; we only
        // want user clicks to apply a skin.
        let suppress_sel: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        // Populate rows
        let rebuild_list = {
            let listbox = listbox.clone();
            let state_rc = state.clone();
            let suppress = suppress_sel.clone();
            Rc::new(move || {
                suppress.set(true);
                while let Some(row) = listbox.first_child() {
                    listbox.remove(&row);
                }
                let hidden = state_rc.borrow().config.appearance.hidden_skins.clone();
                let entries = crate::skin::list_skins(&hidden);
                let active = state_rc.borrow().config.appearance.active_skin.clone();
                let mut active_row: Option<ListBoxRow> = None;

                for entry in entries {
                    let row = ListBoxRow::new();
                    let hbox = GtkBox::new(Orientation::Horizontal, 8);
                    hbox.set_margin_top(4);
                    hbox.set_margin_bottom(4);
                    hbox.set_margin_start(8);
                    hbox.set_margin_end(8);

                    let name_lbl = Label::new(Some(&entry.display_name));
                    name_lbl.set_halign(Align::Start);
                    name_lbl.set_hexpand(true);
                    hbox.append(&name_lbl);

                    if entry.is_builtin {
                        let tag = Label::new(Some("(built-in)"));
                        tag.add_css_class("dim-label");
                        hbox.append(&tag);
                    }

                    if entry.name == active {
                        let mark = Label::new(Some("● Active"));
                        mark.add_css_class("dim-label");
                        hbox.append(&mark);
                    }

                    row.set_child(Some(&hbox));
                    row.set_widget_name(&entry.name);
                    listbox.append(&row);
                    if entry.name == active {
                        active_row = Some(row);
                    }
                }
                if let Some(r) = active_row {
                    listbox.select_row(Some(&r));
                }
                suppress.set(false);
            })
        };
        rebuild_list();

        // Activating a row (user click / Enter) applies the skin live.
        // Uses `row-activated`, NOT `row-selected`: `select_row()` (from the
        // rebuild) and GtkNotebook tab re-show both emit spurious
        // `row-selected` events, and only a real user gesture emits
        // `row-activated`. The old `row-selected` + suppress-flag guard only
        // covered the rebuild window, so a tab switch's spurious selection
        // slipped through and jumped the active skin down a row.
        {
            let state_rc = state.clone();
            let provider = css_provider.clone();
            let text_rgba = text_rgba.clone();
            let accent_rgba = accent_rgba.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let rebuild = rebuild_list.clone();
            listbox.connect_row_activated(move |_, row| {
                let name = row.widget_name().to_string();
                if name.is_empty() { return; }
                // User-clicked a row while the skin was already active
                // (e.g., re-click to re-apply) — nothing to do.
                if state_rc.borrow().config.appearance.active_skin == name {
                    return;
                }
                let Some(skin) = crate::skin::load_skin(&name) else { return };
                let css = crate::skin::render_gtk_css(&skin.vars);
                provider.load_from_data(&css);
                super::util::apply_color_scheme(skin.vars.background.luminance() < 0.5);
                *text_rgba.borrow_mut() = gdk::RGBA::new(
                    skin.vars.text_color.r as f32 / 255.0,
                    skin.vars.text_color.g as f32 / 255.0,
                    skin.vars.text_color.b as f32 / 255.0,
                    1.0,
                );
                // Playlist TreeView stores fg color per-row via RGBA column;
                // update the shared accent from the new skin's highlight so
                // the playing row re-renders in the new skin's accent rather
                // than the color captured at startup.
                *accent_rgba.borrow_mut() = Some(gdk::RGBA::new(
                    skin.vars.highlight.r as f32 / 255.0,
                    skin.vars.highlight.g as f32 / 255.0,
                    skin.vars.highlight.b as f32 / 255.0,
                    1.0,
                ));
                state_rc.borrow_mut().config.appearance.active_skin = name;
                // Refresh all playlist rows so the new text / accent colors
                // propagate — CSS alone doesn't reach the deprecated cell
                // renderer's foreground-rgba column.
                rebuild_pl();
                rebuild();
            });
        }

        // Row of action buttons
        let btn_row = GtkBox::new(Orientation::Horizontal, 8);
        let btn_add = Button::with_label("Add skin…");
        let btn_remove = Button::with_label("Remove");
        let btn_download = Button::with_label("Download skin…");
        btn_row.append(&btn_add);
        btn_row.append(&btn_remove);
        btn_row.append(&btn_download);
        root.append(&btn_row);

        // Wire Add
        {
            let state_rc = state.clone();
            let rebuild = rebuild_list.clone();
            let listbox = listbox.clone();
            let win_ref = win.clone();
            btn_add.connect_clicked(move |_| {
                let dialog = FileDialog::new();
                dialog.set_title("Add Sparkamp skin");
                let filter = FileFilter::new();
                filter.add_suffix("css");
                filter.set_name(Some("Sparkamp skin (*.css)"));
                let filters = gio::ListStore::new::<FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));

                let state_rc = state_rc.clone();
                let rebuild = rebuild.clone();
                let listbox = listbox.clone();
                let win_alert = win_ref.clone();
                dialog.open(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    match crate::skin::add_user_skin(&path) {
                        Ok(entry) => {
                            state_rc.borrow_mut().config.appearance.active_skin =
                                entry.name.clone();
                            state_rc.borrow_mut().config.appearance.hidden_skins
                                .retain(|n| !n.eq_ignore_ascii_case(&entry.name));
                            rebuild();
                            if let Some(row) = find_row_by_name(&listbox, &entry.name) {
                                listbox.select_row(Some(&row));
                            }
                        }
                        Err(e) => {
                            // Non-fatal: the file chooser can be reopened to retry.
                            show_toast(&win_alert, &format!("Could not add skin: {e}"));
                        }
                    }
                });
            });
        }

        // Wire Remove (disabled for built-ins)
        {
            let state_rc = state.clone();
            let rebuild = rebuild_list.clone();
            let listbox = listbox.clone();
            btn_remove.connect_clicked(move |_| {
                let Some(row) = listbox.selected_row() else { return };
                let name = row.widget_name().to_string();
                if name == "dark" || name == "light" || name.is_empty() {
                    return;
                }
                {
                    let mut s = state_rc.borrow_mut();
                    if !s.config.appearance.hidden_skins.iter().any(|h| h.eq_ignore_ascii_case(&name)) {
                        s.config.appearance.hidden_skins.push(name.clone());
                    }
                    if s.config.appearance.active_skin == name {
                        s.config.appearance.active_skin = "dark".to_string();
                    }
                }
                rebuild();
            });
        }

        // Update Remove-disabled state reactively on selection changes.
        {
            let btn_remove = btn_remove.clone();
            listbox.connect_row_selected(move |_, row| {
                let name = row.map(|r| r.widget_name().to_string()).unwrap_or_default();
                let is_builtin = name == "dark" || name == "light" || name.is_empty();
                btn_remove.set_sensitive(!is_builtin);
            });
        }

        // Wire Download (Export template CSS…)
        {
            let listbox = listbox.clone();
            let win_ref = win.clone();
            btn_download.connect_clicked(move |_| {
                let Some(row) = listbox.selected_row() else { return };
                let name = row.widget_name().to_string();
                let Some(skin) = crate::skin::load_skin(&name) else { return };

                let dialog = FileDialog::new();
                dialog.set_title("Save Sparkamp skin");
                dialog.set_initial_name(Some(&format!("{name}.css")));

                let skin_copy = skin.clone();
                dialog.save(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let css = match &skin_copy.source {
                        crate::skin::SkinSource::BuiltIn => match skin_copy.name.as_str() {
                            "dark" => crate::skin::DARK_TEMPLATE_CSS.to_string(),
                            "light" => crate::skin::LIGHT_TEMPLATE_CSS.to_string(),
                            _ => crate::skin::DARK_TEMPLATE_CSS.to_string(),
                        },
                        crate::skin::SkinSource::UserFile(p) => {
                            std::fs::read_to_string(p).unwrap_or_default()
                        }
                    };
                    let _ = std::fs::write(&path, css);
                });
            });
        }

        // Separator
        let sep = gtk4::Separator::new(Orientation::Horizontal);
        sep.set_margin_top(8);
        sep.set_margin_bottom(8);
        root.append(&sep);

        // Documentation header + button
        let doc_header = Label::new(Some("Documentation"));
        doc_header.set_halign(Align::Start);
        doc_header.add_css_class("heading");
        root.append(&doc_header);

        let btn_guide = Button::with_label("Export how-to guide…");
        root.append(&btn_guide);
        {
            let win_ref = win.clone();
            btn_guide.connect_clicked(move |_| {
                let dialog = FileDialog::new();
                dialog.set_title("Save Sparkamp skin guide");
                dialog.set_initial_name(Some("sparkamp-skin-guide.md"));
                dialog.save(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let _ = std::fs::write(&path, crate::skin::SKIN_GUIDE_MD);
                });
            });
        }

        // ── Graphics ──────────────────────────────────────────────────────
        //
        // Which display backend and GSK renderer Sparkamp runs on. This is a
        // diagnostic surface first: "what am I actually using?" is otherwise
        // unanswerable from inside the app, and it is the first question worth
        // asking when the window renders wrongly or not at all.
        //
        // Both dropdowns only take effect on the next launch — GDK reads
        // GDK_BACKEND and GSK_RENDERER once, during init, long before any of
        // this exists. `sparkamp --backend=…` / `--renderer=…` override them
        // for a single run, which is the way back from a choice that leaves no
        // window to change the setting in.
        let gfx_sep = Separator::new(Orientation::Horizontal);
        gfx_sep.set_margin_top(8);
        gfx_sep.set_margin_bottom(8);
        root.append(&gfx_sep);

        let gfx_header = Label::new(Some("Graphics"));
        gfx_header.set_halign(Align::Start);
        gfx_header.add_css_class("heading");
        root.append(&gfx_header);

        let gfx_grid = Grid::new();
        gfx_grid.set_row_spacing(8);
        gfx_grid.set_column_spacing(12);

        let lbl_current = Label::new(Some("Current"));
        lbl_current.set_halign(Align::Start);
        gfx_grid.attach(&lbl_current, 0, 0, 1, 1);

        // Filled in on map: a window has no renderer until it is realized.
        let val_current = Label::new(Some("…"));
        val_current.set_halign(Align::Start);
        val_current.set_selectable(true);
        val_current.add_css_class("dim-label");
        gfx_grid.attach(&val_current, 1, 0, 1, 1);

        let lbl_backend = Label::new(Some("Display backend"));
        lbl_backend.set_halign(Align::Start);
        lbl_backend.set_tooltip_text(Some(
            "Automatic checks Wayland in a throwaway helper process at startup \
             and falls back to X11 if this compositor crashes GTK. Pick Wayland \
             or X11 to skip that check and always use one.",
        ));
        gfx_grid.attach(&lbl_backend, 0, 1, 1, 1);

        let dd_backend = DropDown::from_strings(&["Automatic", "Wayland", "X11"]);
        {
            let current = state.borrow().config.appearance.display_backend;
            dd_backend.set_selected(match current {
                crate::config::DisplayBackend::Auto => 0,
                crate::config::DisplayBackend::Wayland => 1,
                crate::config::DisplayBackend::X11 => 2,
            });
        }
        {
            let state_rc = state.clone();
            dd_backend.connect_selected_notify(move |d| {
                let choice = match d.selected() {
                    1 => crate::config::DisplayBackend::Wayland,
                    2 => crate::config::DisplayBackend::X11,
                    _ => crate::config::DisplayBackend::Auto,
                };
                state_rc.borrow_mut().config.appearance.display_backend = choice;
            });
        }
        gfx_grid.attach(&dd_backend, 1, 1, 1, 1);

        let lbl_renderer = Label::new(Some("Renderer"));
        lbl_renderer.set_halign(Align::Start);
        lbl_renderer.set_tooltip_text(Some(
            "Automatic lets GTK choose. Cairo is software rendering — slower, \
             but it works where the GPU drivers do not.",
        ));
        gfx_grid.attach(&lbl_renderer, 0, 2, 1, 1);

        let dd_renderer = DropDown::from_strings(&[
            "Automatic",
            "gl",
            "vulkan",
            "cairo (software)",
        ]);
        {
            let current = state.borrow().config.appearance.gsk_renderer;
            dd_renderer.set_selected(match current {
                crate::config::RendererChoice::Auto => 0,
                crate::config::RendererChoice::Gl => 1,
                crate::config::RendererChoice::Vulkan => 2,
                crate::config::RendererChoice::Cairo => 3,
            });
        }
        {
            let state_rc = state.clone();
            dd_renderer.connect_selected_notify(move |d| {
                let choice = match d.selected() {
                    1 => crate::config::RendererChoice::Gl,
                    2 => crate::config::RendererChoice::Vulkan,
                    3 => crate::config::RendererChoice::Cairo,
                    _ => crate::config::RendererChoice::Auto,
                };
                state_rc.borrow_mut().config.appearance.gsk_renderer = choice;
            });
        }
        gfx_grid.attach(&dd_renderer, 1, 2, 1, 1);
        root.append(&gfx_grid);

        let gfx_hint = Label::new(Some("Both take effect the next time Sparkamp starts."));
        gfx_hint.set_halign(Align::Start);
        gfx_hint.set_wrap(true);
        gfx_hint.add_css_class("dim-label");
        root.append(&gfx_hint);

        // Why the read-out above may disagree with the dropdowns: a command-line
        // override, or an automatic fallback. Nothing is shown in the ordinary
        // case, so a line here always means something happened.
        for note in crate::display_backend::status_notes(&crate::display_backend::status()) {
            let lbl = Label::new(Some(&note));
            lbl.set_halign(Align::Start);
            lbl.set_wrap(true);
            lbl.add_css_class("dim-label");
            root.append(&lbl);
        }

        // The renderer exists only once the window has been realized, so the
        // read-out is filled in on map rather than at construction time.
        {
            let val = val_current.clone();
            win.connect_map(move |w| {
                let backend = gtk4::prelude::WidgetExt::display(w)
                    .type_()
                    .name()
                    .to_string();
                let renderer = w
                    .renderer()
                    .map(|r| r.type_().name().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                val.set_text(&format!(
                    "{} · {}",
                    crate::display_backend::backend_display_name(&backend),
                    crate::display_backend::renderer_display_name(&renderer),
                ));
            });
        }

        let tab_lbl = Label::with_mnemonic(SETTINGS_TAB_LABELS[0]);
        notebook.append_page(&settings_scroll_page(&root), Some(&tab_lbl));
}
