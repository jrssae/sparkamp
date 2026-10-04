//! The Servers block of the Media Library tab: the configured Navidrome /
//! OpenSubsonic servers, each with its state, a progress bar while its
//! catalog downloads, and a Test that reports every address on its own.
//! "Add Server…" opens a dialog, the way "Add Folder…" above it opens a
//! picker.
//!
//! The state shown is read from `AppState` (`server_status`,
//! `server_progress`), which the main tick keeps current, so closing this
//! window and opening it again shows where a download has got to.

use super::*;
use sparkamp::config::ServerConfig;

/// The parts of one server row that change while the window is open.
struct RowParts {
    id: String,
    name: String,
    state_lbl: gtk4::Label,
    bar: gtk4::ProgressBar,
}

/// Lay the Servers block out on `grid` from `row` down (three rows).
pub(super) fn attach(grid: &gtk4::Grid, row: i32, state: &Rc<RefCell<AppState>>, win: &gtk4::Window) {
    let lbl = gtk4::Label::new(Some("Servers:"));
    lbl.set_halign(gtk4::Align::Start);
    let btn_add = gtk4::Button::with_label("Add Server…");
    let btn_remove = gtk4::Button::with_label("Remove");
    btn_remove.set_sensitive(false);

    let list = gtk4::ListBox::new();
    list.add_css_class("playlist");
    list.set_selection_mode(gtk4::SelectionMode::Single);

    let status = gtk4::Label::new(None);
    status.set_halign(gtk4::Align::Start);
    status.add_css_class("dim-label");

    grid.attach(&lbl, 0, row, 2, 1);
    grid.attach(&btn_add, 2, row, 1, 1);
    grid.attach(&btn_remove, 3, row, 1, 1);
    grid.attach(&list, 0, row + 1, 4, 1);
    grid.attach(&status, 0, row + 2, 4, 1);

    let parts: Rc<RefCell<Vec<RowParts>>> = Rc::new(RefCell::new(Vec::new()));

    let rebuild: Rc<dyn Fn()> = {
        let state = state.clone();
        let list = list.clone();
        let status = status.clone();
        let btn_remove = btn_remove.clone();
        let parts = parts.clone();
        Rc::new(move || {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let servers = state.borrow().config.servers.clone();
            let mut new_parts = Vec::new();
            for server in &servers {
                let row_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
                row_box.set_margin_top(4);
                row_box.set_margin_bottom(4);

                let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                head.append(&gtk4::Image::from_icon_name("network-server-symbolic"));
                let names = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
                let name = gtk4::Label::new(Some(&gtk_safe(&server.name)));
                name.set_halign(gtk4::Align::Start);
                name.add_css_class("heading");
                let urls: Vec<&str> =
                    [server.lan_url.as_deref(), server.remote_url.as_deref()].into_iter().flatten().collect();
                let url_lbl = gtk4::Label::new(Some(&gtk_safe(&urls.join("  ·  "))));
                url_lbl.set_halign(gtk4::Align::Start);
                url_lbl.add_css_class("dim-label");
                names.append(&name);
                names.append(&url_lbl);
                names.set_hexpand(true);
                head.append(&names);
                let btn_test = gtk4::Button::with_label("Test");
                head.append(&btn_test);
                row_box.append(&head);

                let state_lbl = gtk4::Label::new(None);
                state_lbl.set_halign(gtk4::Align::Start);
                state_lbl.add_css_class("dim-label");
                row_box.append(&state_lbl);
                let bar = gtk4::ProgressBar::new();
                bar.set_visible(false);
                row_box.append(&bar);
                let results = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
                row_box.append(&results);

                {
                    let state = state.clone();
                    let cfg = server.clone();
                    let results = results.clone();
                    btn_test.connect_clicked(move |_| {
                        let password = state.borrow().secrets.get(&cfg.id);
                        match password {
                            Some(pw) => run_test(cfg.clone(), pw, &results),
                            None => show_message(&results, "No password stored for this server."),
                        }
                    });
                }

                let row = gtk4::ListBoxRow::new();
                row.set_child(Some(&row_box));
                list.append(&row);
                new_parts.push(RowParts { id: server.id.clone(), name: server.name.clone(), state_lbl, bar });
            }
            *parts.borrow_mut() = new_parts;
            btn_remove.set_sensitive(!servers.is_empty());
            status.set_text(if servers.is_empty() {
                "No servers — click \"Add Server…\" to add a Navidrome or other Subsonic server"
            } else {
                ""
            });
            refresh(&parts.borrow(), &state.borrow());
        })
    };
    rebuild();

    // Keep each row's state and progress current while the window is open.
    {
        let state = state.clone();
        let parts = parts.clone();
        let list_weak = list.downgrade();
        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            if list_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            refresh(&parts.borrow(), &state.borrow());
            glib::ControlFlow::Continue
        });
    }

    {
        let state = state.clone();
        let win = win.clone();
        let rebuild = rebuild.clone();
        btn_add.connect_clicked(move |_| open_add_dialog(&win, &state, rebuild.clone()));
    }

    {
        let state = state.clone();
        let win = win.downgrade();
        let list = list.clone();
        let rebuild = rebuild.clone();
        btn_remove.connect_clicked(move |_| {
            let Some(index) = list.selected_row().map(|r| r.index()) else { return };
            let Some(server) = state.borrow().config.servers.get(index as usize).cloned() else { return };
            let dialog = gtk4::AlertDialog::builder()
                .message(format!("Remove {}?", server.name))
                .detail("Its cached catalog and stored password are removed. Your files are not touched.")
                .buttons(vec!["Cancel".to_string(), "Remove".to_string()])
                .cancel_button(0)
                .default_button(0)
                .modal(true)
                .build();
            let state = state.clone();
            let rebuild = rebuild.clone();
            dialog.choose(win.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                if result != Ok(1) {
                    return;
                }
                {
                    let mut s = state.borrow_mut();
                    let _ = s.secrets.delete(&server.id);
                    if let Some(lib) = s.media_lib.as_ref() {
                        let _ = lib.forget_server(&server.id);
                    }
                    s.config.servers.retain(|x| x.id != server.id);
                    let _ = s.config.save();
                    s.restart_servers();
                }
                rebuild();
                refresh_library(&state);
            });
        });
    }
}

/// Bring each row up to date: a bar and a count while its catalog
/// downloads, otherwise its status line.
fn refresh(parts: &[RowParts], s: &AppState) {
    for p in parts {
        match s.server_progress.iter().find(|(id, _)| id == &p.id).map(|(_, pr)| *pr) {
            Some(pr) => {
                p.bar.set_visible(true);
                match pr.total.filter(|t| *t > 0) {
                    Some(total) => p.bar.set_fraction((pr.fetched as f64 / total as f64).min(1.0)),
                    None => p.bar.pulse(),
                }
                let line = sparkamp::servers::status::progress_line(&p.name, &pr);
                p.state_lbl.set_text(line.strip_prefix(&format!("{}: ", p.name)).unwrap_or(&line));
            }
            None => {
                p.bar.set_visible(false);
                let prefix = format!("{}: ", p.name);
                let line = s.server_status.iter().find(|l| l.starts_with(&prefix));
                p.state_lbl.set_text(line.map(|l| &l[prefix.len()..]).unwrap_or(""));
            }
        }
    }
}

/// Ask the Media Library window, if open, to list the files again.
fn refresh_library(state: &Rc<RefCell<AppState>>) {
    let callback = state.borrow().rebuild_ml_callback.clone();
    if let Some(callback) = callback {
        callback();
    }
    // The sidebar's source filters follow the server list, and a removed
    // server's playlists went with its catalog.
    let rows = state.borrow().source_rows_callback.clone();
    if let Some(rows) = rows {
        rows();
    }
    super::super::notify_playlist_nav_refresh();
}

/// Replace `out`'s contents with one line of text.
fn show_message(out: &gtk4::Box, text: &str) {
    while let Some(child) = out.first_child() {
        out.remove(&child);
    }
    let lbl = gtk4::Label::new(Some(text));
    lbl.set_halign(gtk4::Align::Start);
    lbl.set_wrap(true);
    lbl.add_css_class("dim-label");
    out.append(&lbl);
}

/// Test each address of `cfg` on a background thread and show the answers
/// in `out`: one line per address, marked as answered or not, so it is plain
/// which address said what.
fn run_test(cfg: ServerConfig, password: String, out: &gtk4::Box) {
    show_message(out, "Testing each address…");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<(bool, String, String)>>();
    std::thread::spawn(move || {
        let checks = sparkamp::servers::sync::test_addresses(&cfg, &password, |_| {
            sparkamp::servers::transport::PlatformTransport::default()
        });
        let lines = checks
            .iter()
            .map(|c| {
                let what = match &c.outcome {
                    Ok(report) => report.summary(),
                    Err(why) => why.clone(),
                };
                (c.outcome.is_ok(), format!("{} · {}", c.address.label(), c.url), what)
            })
            .collect();
        let _ = tx.send(lines);
    });
    let out = out.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || match rx.try_recv() {
        Ok(lines) => {
            while let Some(child) = out.first_child() {
                out.remove(&child);
            }
            if lines.is_empty() {
                show_message(&out, "Add a home or remote address to test.");
            }
            for (ok, head, what) in lines {
                let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
                let icon = gtk4::Image::from_icon_name(if ok { "emblem-ok-symbolic" } else { "dialog-error-symbolic" });
                icon.set_valign(gtk4::Align::Start);
                line.append(&icon);
                let text = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
                let head_lbl = gtk4::Label::new(Some(&gtk_safe(&head)));
                head_lbl.set_halign(gtk4::Align::Start);
                head_lbl.add_css_class("heading");
                let what_lbl = gtk4::Label::new(Some(&gtk_safe(&what)));
                what_lbl.set_halign(gtk4::Align::Start);
                what_lbl.set_wrap(true);
                what_lbl.set_xalign(0.0);
                what_lbl.add_css_class("dim-label");
                text.append(&head_lbl);
                text.append(&what_lbl);
                line.append(&text);
                out.append(&line);
            }
            glib::ControlFlow::Break
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
    });
}

/// The Add Server dialog: what the server is called, where it is, and how
/// to sign in. Each field says what it wants rather than showing a sample
/// name. Test tries each address on its own before adding.
fn open_add_dialog(parent: &gtk4::Window, state: &Rc<RefCell<AppState>>, rebuild: Rc<dyn Fn()>) {
    let dialog = gtk4::Window::builder()
        .title("Add Server")
        .modal(true)
        .transient_for(parent)
        .default_width(560)
        .resizable(false)
        .build();

    let grid = gtk4::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);
    grid.set_margin_top(16);
    grid.set_margin_bottom(16);
    grid.set_margin_start(16);
    grid.set_margin_end(16);

    let intro = gtk4::Label::new(Some("A Navidrome or other Subsonic server. Give at least one address."));
    intro.set_halign(gtk4::Align::Start);
    intro.add_css_class("dim-label");
    grid.attach(&intro, 0, 0, 2, 1);

    let field = |row: i32, label: &str, hint: &str| {
        let lbl = gtk4::Label::new(Some(label));
        lbl.set_halign(gtk4::Align::End);
        let entry = gtk4::Entry::new();
        entry.set_placeholder_text(Some(hint));
        entry.set_hexpand(true);
        grid.attach(&lbl, 0, row, 1, 1);
        grid.attach(&entry, 1, row, 1, 1);
        entry
    };
    let name = field(1, "Name", "Nickname for this server");
    let lan = field(2, "Home address", "Address on your home network, http:// or https://");
    let remote = field(3, "Remote address", "Address from anywhere, https:// only (optional)");
    let username = field(4, "Username", "Your account name on the server");
    let password = field(5, "Password", "Your password on the server");
    password.set_visibility(false);
    password.set_input_purpose(gtk4::InputPurpose::Password);

    // No keyring yet on Linux: say so rather than let it surprise anyone.
    let note = gtk4::Label::new(Some(
        "The password is kept for this session only, until keyring support arrives.",
    ));
    note.set_halign(gtk4::Align::Start);
    note.set_wrap(true);
    note.add_css_class("dim-label");

    // Plain HTTP outside the home network: said while the address is typed.
    let http_warning = gtk4::Label::new(None);
    http_warning.set_halign(gtk4::Align::Start);
    http_warning.set_wrap(true);
    http_warning.add_css_class("warning");
    http_warning.set_visible(false);
    let warn_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    warn_box.append(&note);
    warn_box.append(&http_warning);
    grid.attach(&warn_box, 1, 6, 1, 1);
    {
        let http_warning = http_warning.clone();
        lan.connect_changed(move |e| {
            match sparkamp::servers::validate::home_address_warning(&e.text()) {
                Some(w) => {
                    http_warning.set_text(&gtk_safe(&w));
                    http_warning.set_visible(true);
                }
                None => http_warning.set_visible(false),
            }
        });
    }

    let results = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    grid.attach(&results, 0, 7, 2, 1);
    let problem = gtk4::Label::new(None);
    problem.set_halign(gtk4::Align::Start);
    problem.set_wrap(true);
    problem.add_css_class("error");
    grid.attach(&problem, 0, 8, 2, 1);

    let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let btn_test = gtk4::Button::with_label("Test");
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let btn_cancel = gtk4::Button::with_label("Cancel");
    let btn_add = gtk4::Button::with_label("Add Server");
    btn_add.add_css_class("suggested-action");
    buttons.append(&btn_test);
    buttons.append(&spacer);
    buttons.append(&btn_cancel);
    buttons.append(&btn_add);
    grid.attach(&buttons, 0, 9, 2, 1);
    dialog.set_child(Some(&grid));

    let read = {
        let (name, lan, remote, username) = (name.clone(), lan.clone(), remote.clone(), username.clone());
        move || {
            let opt = |e: &gtk4::Entry| {
                let t = e.text().trim().to_string();
                (!t.is_empty()).then_some(t)
            };
            ServerConfig {
                lan_url: opt(&lan),
                remote_url: opt(&remote),
                username: username.text().trim().to_string(),
                ..ServerConfig::new(name.text().trim())
            }
        }
    };

    // Test needs an address and a sign-in; Add also needs a name.
    let update_buttons = {
        let read = read.clone();
        let (password, btn_test, btn_add) = (password.clone(), btn_test.clone(), btn_add.clone());
        Rc::new(move || {
            let cfg = read();
            let can_test = (cfg.lan_url.is_some() || cfg.remote_url.is_some())
                && !cfg.username.is_empty()
                && !password.text().is_empty();
            btn_test.set_sensitive(can_test);
            btn_add.set_sensitive(can_test && !cfg.name.is_empty());
        })
    };
    update_buttons();
    for entry in [&name, &lan, &remote, &username, &password] {
        let update = update_buttons.clone();
        entry.connect_changed(move |_| update());
    }

    {
        let read = read.clone();
        let (password, results) = (password.clone(), results.clone());
        btn_test.connect_clicked(move |_| run_test(read(), password.text().to_string(), &results));
    }
    {
        let dialog = dialog.clone();
        btn_cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let dialog = dialog.clone();
        btn_add.connect_clicked(move |_| {
            let cfg = read();
            let outcome = {
                let mut s = state.borrow_mut();
                sparkamp::servers::validate::validate_new_server(&cfg, &s.config.servers)
                    .and_then(|()| {
                        s.secrets
                            .set(&cfg.id, &password.text())
                            .map_err(|e| format!("Could not store the password: {e}"))
                    })
                    .map(|()| {
                        s.config.servers.push(cfg.clone());
                        let _ = s.config.save();
                        // The first update starts at once and shows its
                        // progress under the server's row.
                        s.restart_servers();
                    })
            };
            match outcome {
                Ok(()) => {
                    dialog.close();
                    rebuild();
                    refresh_library(&state);
                }
                Err(why) => problem.set_text(&why),
            }
        });
    }
    dialog.present();
}
