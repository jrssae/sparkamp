//! The Behavior tab: playback, ReplayGain and playlist handling.
//!
//! Split out of `open_settings_window`, which was one 2,775-line function
//! holding every tab inline. The body is unchanged; what it used to close
//! over arrives as arguments.

use super::*;

pub(super) fn build(notebook: &Notebook, state: &Rc<RefCell<AppState>>, win: &gtk4::Window) {
    let state = state.clone();
    let win = win.clone();
        use crate::config::PlaylistAddBehavior;

        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        let lbl = Label::new(Some("Autoplay on add"));
        lbl.set_halign(Align::Start);
        grid.attach(&lbl, 0, 0, 1, 1);

        let chk = CheckButton::new();
        chk.set_active(state.borrow().config.behavior.autoplay_on_add);
        {
            let state_rc = state.clone();
            chk.connect_toggled(move |c| {
                state_rc.borrow_mut().config.behavior.autoplay_on_add = c.is_active();
            });
        }
        grid.attach(&chk, 1, 0, 1, 1);

        // Row 1: what adding files to the active playlist does by default.
        //
        // Named for the action, not for one caller. The old label
        // ("Media library → playlist") described where it was first used, but
        // the setting has never been Media-Library-specific: it governs every
        // add that has not been told otherwise — drag-and-drop from any view
        // or from a file manager, files passed on the command line, and files
        // opened through the desktop when Sparkamp is the default player.
        let lbl_add = Label::new(Some("Default add file action"));
        lbl_add.set_halign(Align::Start);
        lbl_add.set_tooltip_text(Some(
            "Applies to every way files reach the playlist: drag-and-drop, the \
             Media Library, the command line, and files opened from the desktop.",
        ));
        grid.attach(&lbl_add, 0, 1, 1, 1);

        let dd_add = DropDown::from_strings(&["Append to current", "Replace current"]);
        {
            let behavior = state.borrow().config.behavior.playlist_add_behavior.clone();
            dd_add.set_selected(match behavior {
                PlaylistAddBehavior::Append => 0,
                PlaylistAddBehavior::Replace => 1,
            });
        }
        {
            let state_rc = state.clone();
            dd_add.connect_selected_notify(move |d| {
                let behavior = match d.selected() {
                    1 => PlaylistAddBehavior::Replace,
                    _ => PlaylistAddBehavior::Append,
                };
                state_rc.borrow_mut().config.behavior.playlist_add_behavior = behavior;
            });
        }
        grid.attach(&dd_add, 1, 1, 1, 1);

        // Row 2: gnudb email — used for the CDDB/gnudb handshake on disc
        // identify and (later) submission. Stored locally only.
        let lbl_email = Label::new(Some("gnudb email"));
        lbl_email.set_halign(Align::Start);
        lbl_email.set_tooltip_text(Some(
            "Your email for the gnudb/CDDB handshake — needed to identify and \
             submit disc metadata. Stored locally and used only to talk to gnudb.",
        ));
        grid.attach(&lbl_email, 0, 2, 1, 1);

        let email_entry = gtk4::Entry::new();
        email_entry.set_hexpand(true);
        email_entry.set_placeholder_text(Some("you@example.com"));
        email_entry.set_text(&gtk_safe(&state.borrow().config.disc.gnudb_email));
        {
            let state_rc = state.clone();
            email_entry.connect_changed(move |e| {
                let mut s = state_rc.borrow_mut();
                s.config.disc.gnudb_email = e.text().to_string();
                let _ = s.config.save();
            });
        }
        grid.attach(&email_entry, 1, 2, 1, 1);

        // Auto-open on audio-CD insert (mirrors the macOS Settings toggle;
        // the app-level insertion watcher reads this live).
        let lbl_autocd = Label::builder()
            .label("Audio CD inserted")
            .halign(Align::Start)
            .build();
        lbl_autocd.set_tooltip_text(Some(
            "When an audio CD is inserted, open the Media Library on that \
             drive's view. Also covers a CD already in the drive at launch, \
             so setting Sparkamp as the system's CD handler lands on the disc.",
        ));
        grid.attach(&lbl_autocd, 0, 3, 1, 1);
        let chk_autocd = CheckButton::with_label("Open the Media Library");
        chk_autocd.set_active(state.borrow().config.disc.auto_show_inserted_audio_cd);
        {
            let state_rc = state.clone();
            chk_autocd.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.disc.auto_show_inserted_audio_cd = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_autocd, 1, 3, 1, 1);

        // gnudb test mode (mirrors the macOS Settings toggle).
        let lbl_gnudb_test = Label::builder()
            .label("gnudb submissions")
            .halign(Align::Start)
            .build();
        lbl_gnudb_test.set_tooltip_text(Some(
            "gnudb validates test submissions without publishing them. \
             Turn off once a real submission is confirmed working.",
        ));
        grid.attach(&lbl_gnudb_test, 0, 4, 1, 1);
        let chk_gnudb_test = CheckButton::with_label("Submit in test mode");
        chk_gnudb_test.set_active(state.borrow().config.disc.gnudb_submit_mode_test);
        {
            let state_rc = state.clone();
            chk_gnudb_test.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.disc.gnudb_submit_mode_test = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_gnudb_test, 1, 4, 1, 1);

        // OS handler registration shortcut (mirrors the macOS "Open CDs &
        // DVDs Settings…" button): GNOME's "CD audio" handler choice lives
        // in Settings → Removable Media. We never write that preference
        // ourselves — the user points it at Sparkamp once there.
        let lbl_handler = Label::builder()
            .label("System CD handler")
            .halign(Align::Start)
            .build();
        lbl_handler.set_tooltip_text(Some(
            "To have GNOME launch Sparkamp automatically on insert, pick it \
             under \"CD audio\" in Settings → Removable Media.",
        ));
        grid.attach(&lbl_handler, 0, 5, 1, 1);
        let btn_handler = Button::with_label("Open Removable Media Settings…");
        btn_handler.add_css_class("pl-btn");
        {
            let win_alert = win.clone();
            btn_handler.connect_clicked(move |_| {
                let launched = std::process::Command::new("gnome-control-center")
                    .arg("removable-media")
                    .spawn()
                    .is_ok();
                if !launched {
                    // Non-fatal: the user can open GNOME Settings by hand instead.
                    show_toast(
                        &win_alert,
                        "Couldn't open GNOME Settings — open Removable Media \
                         settings yourself and pick Sparkamp under \"CD audio\".",
                    );
                }
            });
        }
        grid.attach(&btn_handler, 1, 5, 1, 1);

        // Verify discs after burning (mirrors the macOS Settings toggle via
        // sparkamp_set_burn_verify): re-reads the burned disc and compares
        // it against the source audio before reporting a successful burn.
        let lbl_verify = Label::builder()
            .label("Disc burning")
            .halign(Align::Start)
            .build();
        lbl_verify.set_tooltip_text(Some(
            "After burning, re-read the disc and compare it against the \
             source audio before reporting success. Slower, but catches \
             bad burns.",
        ));
        grid.attach(&lbl_verify, 0, 6, 1, 1);
        let chk_verify = CheckButton::with_label("Verify discs after burning");
        chk_verify.set_active(state.borrow().config.disc.burn_verify);
        {
            let state_rc = state.clone();
            chk_verify.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.disc.burn_verify = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_verify, 1, 6, 1, 1);

        // ── ReplayGain (phase 4) — no dedicated "Playback" tab exists, so
        // these live here alongside the other playback-adjacent toggles.
        // Each row applies live to the engine on change (never just the
        // config file), per `AppState::apply_replaygain` /
        // `set_rg_fallback_db`.
        let sep_rg = gtk4::Separator::new(Orientation::Horizontal);
        sep_rg.set_margin_top(8);
        sep_rg.set_margin_bottom(4);
        grid.attach(&sep_rg, 0, 7, 2, 1);

        let hdr_rg = Label::new(Some("ReplayGain"));
        hdr_rg.set_halign(Align::Start);
        hdr_rg.add_css_class("heading");
        grid.attach(&hdr_rg, 0, 8, 2, 1);

        // Master enable — reshapes the rgvolume/rglimiter chain immediately.
        let lbl_rg_enable = Label::new(Some("Use ReplayGain"));
        lbl_rg_enable.set_halign(Align::Start);
        grid.attach(&lbl_rg_enable, 0, 9, 1, 1);
        let chk_rg_enable = CheckButton::new();
        chk_rg_enable.set_active(state.borrow().config.playback.replaygain.enabled);
        {
            let state_rc = state.clone();
            chk_rg_enable.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.playback.replaygain.enabled = c.is_active();
                let _ = s.config.save();
                s.apply_replaygain();
            });
        }
        grid.attach(&chk_rg_enable, 1, 9, 1, 1);

        // Source: Track / Album / Automatic (index order matches RgSource).
        let lbl_rg_source = Label::new(Some("ReplayGain source"));
        lbl_rg_source.set_halign(Align::Start);
        grid.attach(&lbl_rg_source, 0, 10, 1, 1);
        let dd_rg_source = DropDown::from_strings(&["Track", "Album", "Automatic"]);
        {
            use crate::config::RgSource;
            let cur = state.borrow().config.playback.replaygain.source;
            dd_rg_source.set_selected(match cur {
                RgSource::Track => 0,
                RgSource::Album => 1,
                RgSource::Automatic => 2,
            });
        }
        {
            use crate::config::RgSource;
            let state_rc = state.clone();
            dd_rg_source.connect_selected_notify(move |d| {
                let source = match d.selected() {
                    0 => RgSource::Track,
                    1 => RgSource::Album,
                    _ => RgSource::Automatic,
                };
                let mut s = state_rc.borrow_mut();
                s.config.playback.replaygain.source = source;
                let _ = s.config.save();
                s.apply_replaygain();
            });
        }
        grid.attach(&dd_rg_source, 1, 10, 1, 1);

        // Clipping protection — inserts rglimiter after rgvolume.
        let lbl_rg_clip = Label::new(Some("Clipping protection"));
        lbl_rg_clip.set_halign(Align::Start);
        grid.attach(&lbl_rg_clip, 0, 11, 1, 1);
        let chk_rg_clip = CheckButton::new();
        chk_rg_clip.set_active(state.borrow().config.playback.replaygain.clip_protection);
        {
            let state_rc = state.clone();
            chk_rg_clip.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.playback.replaygain.clip_protection = c.is_active();
                let _ = s.config.save();
                s.apply_replaygain();
            });
        }
        grid.attach(&chk_rg_clip, 1, 11, 1, 1);

        // Fallback gain for files carrying no ReplayGain info. Live slider —
        // `set_rg_fallback_db` nudges rgvolume's property directly, no
        // pipeline rebuild (and it already updates the config field itself).
        let lbl_rg_fallback = Label::new(Some("Fallback gain (no RG info)"));
        lbl_rg_fallback.set_halign(Align::Start);
        grid.attach(&lbl_rg_fallback, 0, 12, 1, 1);
        let rg_fallback_adj = Adjustment::new(
            state.borrow().config.playback.replaygain.fallback_db as f64,
            -12.0, 0.0, 0.5, 1.0, 0.0,
        );
        let scale_rg_fallback = Scale::new(Orientation::Horizontal, Some(&rg_fallback_adj));
        scale_rg_fallback.set_hexpand(true);
        scale_rg_fallback.set_digits(1);
        scale_rg_fallback.set_draw_value(true);
        // A bare "-3" spoken alone doesn't say what unit it's in, unlike the
        // other three sliders below — this is the only one with a physical
        // unit, so it gets ValueText the same way the seek bar does.
        scale_rg_fallback.update_property(&[
            gtk4::accessible::Property::Label("ReplayGain fallback gain"),
            gtk4::accessible::Property::ValueText(&format!("{:.1} dB", rg_fallback_adj.value())),
        ]);
        {
            let state_rc = state.clone();
            let scale_rg_fallback = scale_rg_fallback.clone();
            rg_fallback_adj.connect_value_changed(move |a| {
                let db = a.value().clamp(-12.0, 0.0);
                let mut s = state_rc.borrow_mut();
                s.set_rg_fallback_db(db);
                let _ = s.config.save();
                scale_rg_fallback
                    .update_property(&[gtk4::accessible::Property::ValueText(&format!("{db:.1} dB"))]);
            });
        }
        grid.attach(&scale_rg_fallback, 1, 12, 1, 1);

        // ── Play count threshold (Phase 10, F11) — same "no dedicated
        // Playback tab" situation as ReplayGain above, so it lives here too.
        // Feeds `config.playback.play_stats`, consumed by
        // `play_stats::play_counted_at` in the player tick loop.
        let sep_ps = gtk4::Separator::new(Orientation::Horizontal);
        sep_ps.set_margin_top(8);
        sep_ps.set_margin_bottom(4);
        grid.attach(&sep_ps, 0, 13, 2, 1);

        let hdr_ps = Label::new(Some("Play Count"));
        hdr_ps.set_halign(Align::Start);
        hdr_ps.add_css_class("heading");
        grid.attach(&hdr_ps, 0, 14, 2, 1);

        let lbl_ps_enable = Label::new(Some("Count plays"));
        lbl_ps_enable.set_halign(Align::Start);
        grid.attach(&lbl_ps_enable, 0, 15, 1, 1);
        let chk_play_stats = CheckButton::new();
        chk_play_stats.set_active(state.borrow().config.playback.play_stats.enabled);
        {
            let state_rc = state.clone();
            chk_play_stats.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.playback.play_stats.enabled = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_play_stats, 1, 15, 1, 1);

        // Mode: N seconds vs N% of track. The two CheckButtons share a
        // group (mutually exclusive, radio-style); each row also carries
        // that mode's SpinButton so its value stays visible next to it.
        use crate::config::PlayStatsMode;
        let cur_mode = state.borrow().config.playback.play_stats.mode;

        let lbl_ps_seconds = Label::new(Some("After N seconds"));
        lbl_ps_seconds.set_halign(Align::Start);
        grid.attach(&lbl_ps_seconds, 0, 16, 1, 1);
        let row_ps_seconds = GtkBox::new(Orientation::Horizontal, 6);
        let radio_ps_seconds = CheckButton::new();
        radio_ps_seconds.set_active(cur_mode == PlayStatsMode::Seconds);
        let spin_ps_seconds = SpinButton::with_range(1.0, 3600.0, 1.0);
        spin_ps_seconds.set_value(state.borrow().config.playback.play_stats.seconds as f64);
        row_ps_seconds.append(&radio_ps_seconds);
        row_ps_seconds.append(&spin_ps_seconds);
        grid.attach(&row_ps_seconds, 1, 16, 1, 1);

        let lbl_ps_percent = Label::new(Some("After N% of track"));
        lbl_ps_percent.set_halign(Align::Start);
        grid.attach(&lbl_ps_percent, 0, 17, 1, 1);
        let row_ps_percent = GtkBox::new(Orientation::Horizontal, 6);
        let radio_ps_percent = CheckButton::new();
        radio_ps_percent.set_group(Some(&radio_ps_seconds));
        radio_ps_percent.set_active(cur_mode == PlayStatsMode::Percent);
        let spin_ps_percent = SpinButton::with_range(1.0, 100.0, 1.0);
        spin_ps_percent.set_value(state.borrow().config.playback.play_stats.percent as f64);
        row_ps_percent.append(&radio_ps_percent);
        row_ps_percent.append(&spin_ps_percent);
        grid.attach(&row_ps_percent, 1, 17, 1, 1);

        {
            let state_rc = state.clone();
            radio_ps_seconds.connect_toggled(move |c| {
                if c.is_active() {
                    let mut s = state_rc.borrow_mut();
                    s.config.playback.play_stats.mode = PlayStatsMode::Seconds;
                    let _ = s.config.save();
                }
            });
        }
        {
            let state_rc = state.clone();
            radio_ps_percent.connect_toggled(move |c| {
                if c.is_active() {
                    let mut s = state_rc.borrow_mut();
                    s.config.playback.play_stats.mode = PlayStatsMode::Percent;
                    let _ = s.config.save();
                }
            });
        }
        {
            let state_rc = state.clone();
            spin_ps_seconds.connect_value_changed(move |s| {
                let secs = s.value() as u32;
                let mut st = state_rc.borrow_mut();
                st.config.playback.play_stats.seconds = secs;
                let _ = st.config.save();
            });
        }
        {
            let state_rc = state.clone();
            spin_ps_percent.connect_value_changed(move |s| {
                let pct = s.value() as u8;
                let mut st = state_rc.borrow_mut();
                st.config.playback.play_stats.percent = pct;
                let _ = st.config.save();
            });
        }

        // ── Stop with fadeout (Shift+V) — how long the ramp to silence
        // takes. Same home as ReplayGain and Play Count above: playback
        // settings live on this tab because there is no Playback tab.
        let sep_fade = gtk4::Separator::new(Orientation::Horizontal);
        sep_fade.set_margin_top(8);
        sep_fade.set_margin_bottom(4);
        grid.attach(&sep_fade, 0, 18, 2, 1);

        let hdr_fade = Label::new(Some("Stop With Fadeout"));
        hdr_fade.set_halign(Align::Start);
        hdr_fade.add_css_class("heading");
        grid.attach(&hdr_fade, 0, 19, 2, 1);

        let lbl_fade = Label::new(Some("Fade length (seconds)"));
        lbl_fade.set_halign(Align::Start);
        grid.attach(&lbl_fade, 0, 20, 1, 1);
        let spin_fade = SpinButton::with_range(
            *crate::config::FADEOUT_SECS_RANGE.start() as f64,
            *crate::config::FADEOUT_SECS_RANGE.end() as f64,
            1.0,
        );
        spin_fade.set_value(state.borrow().config.playback.fadeout_secs as f64);
        spin_fade.set_tooltip_text(Some("Shift+V stops playback over this many seconds"));
        {
            let state_rc = state.clone();
            spin_fade.connect_value_changed(move |s| {
                let secs = s.value() as u32;
                let mut st = state_rc.borrow_mut();
                st.config.playback.fadeout_secs = secs;
                let _ = st.config.save();
            });
        }
        grid.attach(&spin_fade, 1, 20, 1, 1);

        // ── Playlists — the preferred format for new saves. This used to be a
        // whole notebook tab of its own ("Filetypes") holding this single
        // dropdown; it reads better next to the other file-handling settings,
        // and the macOS frontend groups it the same way.
        {
            use crate::config::PlaylistFormat;

            let sep_fmt = gtk4::Separator::new(Orientation::Horizontal);
            sep_fmt.set_margin_top(8);
            sep_fmt.set_margin_bottom(4);
            grid.attach(&sep_fmt, 0, 21, 2, 1);

            let hdr_fmt = Label::new(Some("Playlists"));
            hdr_fmt.set_halign(Align::Start);
            hdr_fmt.add_css_class("heading");
            grid.attach(&hdr_fmt, 0, 22, 2, 1);

            let lbl_fmt = Label::new(Some("Playlist format"));
            lbl_fmt.set_halign(Align::Start);
            grid.attach(&lbl_fmt, 0, 23, 1, 1);

            let dd_fmt = DropDown::from_strings(&["m3u8", "m3u"]);
            dd_fmt.set_selected(match state.borrow().config.media_library.playlist_format {
                PlaylistFormat::M3u8 => 0,
                PlaylistFormat::M3u => 1,
            });
            {
                let state_rc = state.clone();
                dd_fmt.connect_selected_notify(move |d| {
                    let fmt = if d.selected() == 1 {
                        PlaylistFormat::M3u
                    } else {
                        PlaylistFormat::M3u8
                    };
                    state_rc.borrow_mut().config.media_library.playlist_format = fmt;
                });
            }
            grid.attach(&dd_fmt, 1, 23, 1, 1);

            let hint = Label::new(Some(
                "New playlists, Save As, and device exports use this format. \
                 Existing playlists keep their own.",
            ));
            hint.set_halign(Align::Start);
            hint.set_wrap(true);
            hint.add_css_class("status-label");
            grid.attach(&hint, 0, 24, 2, 1);
        }

        let tab_lbl = Label::with_mnemonic(SETTINGS_TAB_LABELS[1]);
        notebook.append_page(&settings_scroll_page(&grid), Some(&tab_lbl));
}
