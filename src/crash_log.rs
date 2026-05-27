//! Crash + warning log to `~/.config/sparkamp/crash.log`.
//!
//! Sparkamp can crash silently when a panic happens deep inside a GTK or
//! GStreamer callback (e.g. during a large drag-and-drop import or rapid
//! pipeline state transitions) because there is no terminal attached when
//! the app is launched from a desktop shortcut.  This module installs:
//!
//! 1. A Rust panic hook that writes the panic message + backtrace to
//!    `~/.config/sparkamp/crash.log` and also forwards to stderr.
//! 2. A GLib log handler that captures `CRITICAL`/`WARNING`/`ERROR`
//!    messages from GTK, GLib, GIO and GStreamer (which are otherwise
//!    routed to GLib's default `g_log` handler and may abort the process
//!    via `G_DEBUG=fatal-criticals`).
//!
//! The hook is idempotent — calling `install()` more than once is a no-op.
//! `RUST_BACKTRACE=1` is forced unless the env var is already set, so
//! crash entries always include a stack trace.

use gstreamer::glib;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static INSTALLED: OnceLock<()> = OnceLock::new();

/// Path to the crash log file (`~/.config/sparkamp/crash.log`).  The
/// parent directory is created on demand by [`append_line`].
pub fn log_path() -> PathBuf {
    LOG_PATH
        .get_or_init(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("sparkamp")
                .join("crash.log")
        })
        .clone()
}

/// Append one line to the crash log (creating the file + parent dir if
/// missing).  Errors are silently swallowed — losing a log line is
/// preferable to crashing the panic handler.
fn append_line(line: &str) {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Rough ISO-ish stamp without pulling in chrono — the value of the
    // log entry is the message + backtrace, not millisecond precision.
    format!("[unix:{secs}]")
}

/// Install the panic + GLib log capture.  Safe to call multiple times.
pub fn install() {
    if INSTALLED.set(()).is_err() {
        return;
    }
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        // SAFETY: env mutation must happen before any thread reads
        // RUST_BACKTRACE.  install() runs at the top of main(), before
        // gstreamer::init() spawns helper threads.
        unsafe { std::env::set_var("RUST_BACKTRACE", "1"); }
    }

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&'static str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic payload>");
        let bt = std::backtrace::Backtrace::force_capture();
        append_line(&format!(
            "{ts} PANIC at {loc}: {msg}\n{bt}",
            ts = timestamp(),
            loc = location,
            msg = payload,
            bt = bt,
        ));
        // Still print to stderr in case the user launched from a terminal.
        prev(info);
    }));

    install_glib_handlers();
}

fn install_glib_handlers() {
    // Capture Error / Critical / Warning across the domains that
    // typically produce the assertions that precede a crash.
    let levels = glib::LogLevels::LEVEL_ERROR
        | glib::LogLevels::LEVEL_CRITICAL
        | glib::LogLevels::LEVEL_WARNING;
    let domains: &[Option<&str>] = &[
        None,
        Some("Gtk"),
        Some("GLib"),
        Some("GLib-GObject"),
        Some("Gio"),
        Some("Gdk"),
        Some("GStreamer"),
    ];
    for domain in domains {
        let dom = domain.map(|d| d.to_string());
        glib::log_set_handler(
            dom.as_deref(),
            levels,
            false,
            false,
            move |captured_domain, captured_level, message| {
                let domain_str = captured_domain.unwrap_or("<no-domain>");
                let level_str = format!("{:?}", captured_level);
                append_line(&format!(
                    "{ts} GLIB-{level} [{domain}]: {msg}",
                    ts = timestamp(),
                    level = level_str,
                    domain = domain_str,
                    msg = message,
                ));
            },
        );
    }
}
