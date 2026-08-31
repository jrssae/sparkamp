//! Display-backend and renderer selection for the GTK4 frontend.
//!
//! Some compositors crash GDK's Wayland backend outright — COSMIC 1.7.0 kills
//! GTK 4.16.13 inside `gdk_wayland_display_query_registry` before a window ever
//! appears. A SIGSEGV is not a Rust panic, so nothing in-process can recover
//! from it: by the time GDK has read `GDK_BACKEND` it is already too late.
//!
//! So the decision is made *before* this process touches GDK at all. On an
//! `Auto` setting in a Wayland session, `main` spawns a short-lived child
//! (`sparkamp --probe-display`) that does nothing but open a GDK display and
//! exit. If the child dies on a signal, the parent sets `GDK_BACKEND=x11` and
//! carries on; the parent itself never risks the crash.
//!
//! [`decide_backend`] and [`decide_renderer`] hold the precedence rules and are
//! pure — every environment input is injected, so the whole table is testable
//! without a display.

pub use crate::config::{DisplayBackend, ProbeCache, RendererChoice};

/// The environment inputs that steer the decision, injected for testability.
#[derive(Debug, Default, Clone)]
pub struct SessionEnv {
    pub wayland_display: Option<String>,
    pub gdk_backend: Option<String>,
    pub gsk_renderer: Option<String>,
    pub session_type: Option<String>,
    pub current_desktop: Option<String>,
}

/// What `main` should do about `GDK_BACKEND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendDecision {
    /// Leave the environment alone and let GDK choose.
    Inherit,
    /// Set `GDK_BACKEND` to this value.
    Force(&'static str),
    /// Run the child probe; its outcome decides.
    Probe,
}

/// What `main` should do about `GSK_RENDERER`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererDecision {
    Inherit,
    Force(&'static str),
}

/// Decide the display backend.
///
/// Precedence, highest first: the `--backend` flag, an existing `GDK_BACKEND`
/// in the environment, the saved setting, then the automatic probe. An explicit
/// `Auto` from the command line beats a saved `Wayland`/`X11` — that is the
/// point of the flag, so a setting that broke the window can be bypassed
/// without editing the config file.
pub fn decide_backend(
    cli: Option<DisplayBackend>,
    cfg: DisplayBackend,
    env: &SessionEnv,
    cache: Option<&ProbeCache>,
    session_sig: &str,
) -> BackendDecision {
    // 1. The command line. An explicit `auto` deliberately falls through to
    //    the automatic path below rather than forcing anything.
    let setting = match cli {
        Some(b @ (DisplayBackend::Wayland | DisplayBackend::X11)) => {
            return BackendDecision::Force(backend_value(b));
        }
        Some(DisplayBackend::Auto) => DisplayBackend::Auto,
        // 2. An existing GDK_BACKEND — a flatpak override or the user's shell.
        //    Only a flag overrides it; the saved setting must not.
        None if env.gdk_backend.is_some() => return BackendDecision::Inherit,
        // 3. The saved setting.
        None => cfg,
    };

    match setting {
        DisplayBackend::Wayland => BackendDecision::Force(backend_value(setting)),
        DisplayBackend::X11 => BackendDecision::Force(backend_value(setting)),
        // 4. Automatic. Only a Wayland session can hit the crash, so anywhere
        //    else GDK is left to choose and no child is spawned.
        DisplayBackend::Auto => {
            if env.wayland_display.is_none() {
                return BackendDecision::Inherit;
            }
            match cache {
                Some(c) if c.session == session_sig && c.crashed => {
                    BackendDecision::Force("x11")
                }
                Some(c) if c.session == session_sig => BackendDecision::Inherit,
                // No verdict, or one taken under a different compositor or GTK
                // build — that says nothing about this one.
                _ => BackendDecision::Probe,
            }
        }
    }
}

/// Decide the GSK renderer, on the same precedence ladder as the backend.
pub fn decide_renderer(
    cli: Option<RendererChoice>,
    cfg: RendererChoice,
    env: &SessionEnv,
) -> RendererDecision {
    let choice = match cli {
        Some(RendererChoice::Auto) => return RendererDecision::Inherit,
        Some(explicit) => explicit,
        None if env.gsk_renderer.is_some() => return RendererDecision::Inherit,
        None => cfg,
    };

    match choice {
        RendererChoice::Auto => RendererDecision::Inherit,
        explicit => RendererDecision::Force(renderer_value(explicit)),
    }
}

/// How the display probe ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The child opened a display and exited cleanly — Wayland works here.
    Ok,
    /// The child died on a signal — GDK's Wayland backend is unusable.
    Crashed,
    /// The child could not be run, timed out, or failed for some other
    /// reason. That says nothing about Wayland, so nothing changes.
    Inconclusive,
}

/// Classify a finished probe from its exit status.
///
/// Only death by signal counts as a crash. A non-zero exit is the child
/// reporting a problem of its own (no display to open, GTK init refused), and
/// downgrading every user to X11 because the probe itself is broken would be
/// worse than the bug being probed for.
pub fn classify_probe(success: bool, signal: Option<i32>) -> ProbeOutcome {
    match (success, signal) {
        (true, _) => ProbeOutcome::Ok,
        // Any signal, not just SIGSEGV: SIGABRT (a GTK assertion) and SIGBUS
        // leave the display just as unusable. A probe we kill on timeout never
        // reaches here — the caller returns `Inconclusive` for that itself.
        (false, Some(_)) => ProbeOutcome::Crashed,
        (false, None) => ProbeOutcome::Inconclusive,
    }
}

/// Build the key a probe verdict is remembered under.
///
/// A verdict is only meaningful for the compositor, the GTK build, and the
/// renderer it was taken against, so all three go in the key. Moving to a
/// runtime with a fixed GTK — or switching renderer — invalidates the old
/// "crashes" answer automatically, with no migration step.
///
/// The renderer belongs here because the probe child runs under the same
/// `GSK_RENDERER` the app will use: a crash it saw might have been the
/// renderer's doing rather than Wayland's, and that verdict must not follow
/// the user to a different renderer. `None` means GSK was left to choose.
pub fn session_signature(
    env: &SessionEnv,
    gtk_version: (u32, u32, u32),
    renderer: Option<&str>,
) -> String {
    let (major, minor, micro) = gtk_version;
    format!(
        "{}|{}|gtk{}.{}.{}|gsk:{}",
        env.current_desktop.as_deref().unwrap_or("?"),
        env.session_type.as_deref().unwrap_or("?"),
        major, minor, micro,
        renderer.unwrap_or("default"),
    )
}

/// How long the probe child is given before it is written off.
///
/// Opening a display is sub-second work; anything slower is a hang, not a slow
/// machine. The wait costs nothing on the common path because the child exits
/// long before this.
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Wait for a probe child and classify how it ended, killing it if it hangs.
///
/// A hung child is [`ProbeOutcome::Inconclusive`], never `Crashed`: the signal
/// that ends it came from us, so it says nothing about the compositor.
pub fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> ProbeOutcome {
    use std::os::unix::process::ExitStatusExt;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return classify_probe(status.success(), status.signal());
            }
            Ok(None) => {}
            // The child vanished from under us; nothing was learned.
            Err(_) => return ProbeOutcome::Inconclusive,
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            // Reap it so no zombie is left behind for the session.
            let _ = child.wait();
            return ProbeOutcome::Inconclusive;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

impl SessionEnv {
    /// Read the session from the real process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build from an arbitrary lookup, so the decision table can be exercised
    /// without touching the process environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        SessionEnv {
            wayland_display: get("WAYLAND_DISPLAY"),
            gdk_backend: get("GDK_BACKEND"),
            gsk_renderer: get("GSK_RENDERER"),
            session_type: get("XDG_SESSION_TYPE"),
            current_desktop: get("XDG_CURRENT_DESKTOP"),
        }
    }
}

/// Run a probe command to completion and classify how it ended.
///
/// A command that cannot even be started is [`ProbeOutcome::Inconclusive`] —
/// that is a problem with the probe, not with the compositor, and it must not
/// drag every user onto X11.
pub fn probe_with(mut cmd: std::process::Command) -> ProbeOutcome {
    // The child says everything through its exit status; its output would only
    // interleave with ours, and a crashing GDK is noisy.
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());

    match cmd.spawn() {
        Ok(mut child) => wait_with_timeout(&mut child, PROBE_TIMEOUT),
        Err(_) => ProbeOutcome::Inconclusive,
    }
}

/// Fold a probe outcome into the saved verdict, returning whether it changed.
///
/// An inconclusive probe is deliberately not recorded: nothing was learned, so
/// the next launch should try again rather than inherit a guess.
pub fn record_probe(
    slot: &mut Option<ProbeCache>,
    session_sig: &str,
    outcome: ProbeOutcome,
) -> bool {
    let crashed = match outcome {
        ProbeOutcome::Ok => false,
        ProbeOutcome::Crashed => true,
        ProbeOutcome::Inconclusive => return false,
    };

    let fresh = ProbeCache { session: session_sig.to_string(), crashed };
    if slot.as_ref() == Some(&fresh) {
        return false;
    }
    *slot = Some(fresh);
    true
}

/// Why the running process ended up on the backend and renderer it did.
///
/// Recorded once at startup so Settings → Appearance can explain a read-out
/// that disagrees with the saved setting, instead of silently contradicting it.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayStatus {
    /// Set when `--backend` overrode the saved setting for this run.
    pub backend_flag: Option<DisplayBackend>,
    /// Set when `--renderer` overrode the saved setting for this run.
    pub renderer_flag: Option<RendererChoice>,
    /// The probe verdict, when a probe ran this launch.
    pub probe: Option<ProbeOutcome>,
    /// Whether the automatic path actually fell back to X11.
    pub fell_back_to_x11: bool,
}

static STATUS: std::sync::OnceLock<DisplayStatus> = std::sync::OnceLock::new();

/// The startup decision, for the Settings read-out. All-default before
/// [`configure`] runs, and in the TUI, which never calls it.
pub fn status() -> DisplayStatus {
    STATUS.get().copied().unwrap_or_default()
}

/// The GTK the process is linked against, at runtime rather than compile time.
#[cfg(target_os = "linux")]
fn gtk_runtime_version() -> (u32, u32, u32) {
    (
        gtk4::major_version(),
        gtk4::minor_version(),
        gtk4::micro_version(),
    )
}

/// The probe child: open a display the way the app would, then exit.
///
/// Runs in a process of its own precisely so that a compositor which kills
/// GDK takes this throwaway down instead of the player. It writes nothing and
/// touches no config; the exit status is the entire result.
#[cfg(target_os = "linux")]
pub fn run_probe_child() -> ! {
    // `gtk4::init` opens the default display, which is where COSMIC kills
    // GTK 4.16. If this call returns at all, Wayland is usable here.
    if gtk4::init().is_err() {
        // GTK refused for a reason of its own — not the crash we are looking
        // for, so report it distinctly and let the parent stay on Wayland.
        std::process::exit(2);
    }
    std::process::exit(0);
}

/// Choose the display backend and renderer, and put them into the environment.
///
/// Must be called before anything touches GTK or spawns a thread: it writes
/// `GDK_BACKEND`/`GSK_RENDERER`, and `set_var` is only sound while the process
/// is still single-threaded.
///
/// Returns `true` when `cfg` gained a new probe verdict and should be saved.
#[cfg(target_os = "linux")]
pub fn configure(
    cli_backend: Option<DisplayBackend>,
    cli_renderer: Option<RendererChoice>,
    cfg: &mut crate::config::Config,
) -> bool {
    let env = SessionEnv::from_env();

    let mut st = DisplayStatus {
        backend_flag: cli_backend,
        renderer_flag: cli_renderer,
        ..Default::default()
    };

    // The renderer is settled first: the probe child inherits it, so it is part
    // of what any verdict is about.
    let renderer: Option<String> =
        match decide_renderer(cli_renderer, cfg.appearance.gsk_renderer, &env) {
            RendererDecision::Force(name) => {
                set_env("GSK_RENDERER", name);
                Some(name.to_string())
            }
            // Nothing forced: whatever GSK_RENDERER already held (usually
            // nothing) is what the probe will run under.
            RendererDecision::Inherit => env.gsk_renderer.clone(),
        };

    let session_sig = session_signature(&env, gtk_runtime_version(), renderer.as_deref());

    let mut config_changed = false;
    match decide_backend(
        cli_backend,
        cfg.appearance.display_backend,
        &env,
        cfg.appearance.display_probe.as_ref(),
        &session_sig,
    ) {
        BackendDecision::Inherit => {}
        BackendDecision::Force(name) => {
            set_env("GDK_BACKEND", name);
            // A cached crash verdict is what produced this on the auto path.
            st.fell_back_to_x11 = cli_backend.is_none()
                && cfg.appearance.display_backend == DisplayBackend::Auto
                && name == "x11";
        }
        BackendDecision::Probe => {
            let outcome = probe_with(probe_command());
            st.probe = Some(outcome);
            config_changed =
                record_probe(&mut cfg.appearance.display_probe, &session_sig, outcome);
            if outcome == ProbeOutcome::Crashed {
                eprintln!(
                    "sparkamp: this compositor crashes GTK's Wayland backend; \
                     falling back to X11. Override with --backend=wayland."
                );
                set_env("GDK_BACKEND", "x11");
                st.fell_back_to_x11 = true;
            }
        }
    }

    let _ = STATUS.set(st);
    config_changed
}

/// The command that runs the probe: this same executable, in probe mode.
///
/// `GDK_BACKEND=wayland` is pinned on the child so a crash means Wayland
/// specifically, not "whatever GDK happened to pick".
#[cfg(target_os = "linux")]
fn probe_command() -> std::process::Command {
    let exe = std::env::current_exe().unwrap_or_else(|_| "sparkamp".into());
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--probe-display").env("GDK_BACKEND", "wayland");
    cmd
}

/// Set an environment variable during single-threaded startup.
#[cfg(target_os = "linux")]
fn set_env(key: &str, value: &str) {
    // SAFETY: called only from `configure`, which `main` runs before GStreamer
    // is initialised, before GTK is touched, and before any thread is spawned.
    // No other thread can be reading the environment concurrently.
    unsafe { std::env::set_var(key, value) };
}

/// Turn a `GdkDisplay` subclass name into something worth showing a user.
///
/// Unknown backends fall through unchanged rather than being hidden — a name
/// nobody recognised is still the answer to "what am I running on?".
pub fn backend_display_name(type_name: &str) -> String {
    match type_name {
        "GdkWaylandDisplay" => "Wayland".to_string(),
        "GdkX11Display" => "X11".to_string(),
        "GdkBroadwayDisplay" => "Broadway".to_string(),
        "GdkMacosDisplay" => "macOS".to_string(),
        other => other.to_string(),
    }
}

/// Turn a `GskRenderer` subclass name into the value `GSK_RENDERER` would take,
/// so what the read-out shows and what the dropdown offers are the same words.
pub fn renderer_display_name(type_name: &str) -> String {
    match type_name {
        "GskNglRenderer" => "ngl".to_string(),
        "GskVulkanRenderer" => "vulkan".to_string(),
        "GskGLRenderer" => "gl".to_string(),
        "GskCairoRenderer" => "cairo".to_string(),
        "GskBroadwayRenderer" => "broadway".to_string(),
        other => other.to_string(),
    }
}

/// Lines explaining why the live read-out may disagree with the dropdowns.
///
/// Empty in the ordinary case: a note that always shows is a note nobody reads.
pub fn status_notes(st: &DisplayStatus) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(b) = st.backend_flag {
        notes.push(format!(
            "Backend overridden for this run by --backend={}.",
            backend_value(b)
        ));
    }
    if let Some(r) = st.renderer_flag {
        notes.push(format!(
            "Renderer overridden for this run by --renderer={}.",
            renderer_value(r)
        ));
    }
    if st.fell_back_to_x11 {
        notes.push(
            "This compositor crashes GTK's Wayland backend, so Sparkamp fell back to X11."
                .to_string(),
        );
    }
    notes
}

/// The `GDK_BACKEND` value a setting maps to. `Auto` has none — it is the
/// absence of a choice, resolved by the probe.
pub fn backend_value(b: DisplayBackend) -> &'static str {
    match b {
        DisplayBackend::Auto => "auto",
        DisplayBackend::Wayland => "wayland",
        DisplayBackend::X11 => "x11",
    }
}

/// The `GSK_RENDERER` value a setting maps to.
pub fn renderer_value(r: RendererChoice) -> &'static str {
    match r {
        RendererChoice::Auto => "auto",
        RendererChoice::Vulkan => "vulkan",
        RendererChoice::Gl => "gl",
        RendererChoice::Cairo => "cairo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wayland_session() -> SessionEnv {
        SessionEnv {
            wayland_display: Some("wayland-0".into()),
            session_type: Some("wayland".into()),
            current_desktop: Some("COSMIC".into()),
            ..Default::default()
        }
    }

    fn x11_session() -> SessionEnv {
        SessionEnv {
            session_type: Some("x11".into()),
            current_desktop: Some("GNOME".into()),
            ..Default::default()
        }
    }

    #[test]
    fn cli_flag_beats_everything_including_an_env_override() {
        let mut env = wayland_session();
        env.gdk_backend = Some("wayland".into());
        let cache = ProbeCache { session: "sig".into(), crashed: false };

        assert_eq!(
            decide_backend(Some(DisplayBackend::X11), DisplayBackend::Wayland,
                           &env, Some(&cache), "sig"),
            BackendDecision::Force("x11")
        );
        assert_eq!(
            decide_backend(Some(DisplayBackend::Wayland), DisplayBackend::X11,
                           &env, Some(&cache), "sig"),
            BackendDecision::Force("wayland")
        );
    }

    #[test]
    fn cli_auto_overrides_a_saved_setting_and_returns_to_probing() {
        assert_eq!(
            decide_backend(Some(DisplayBackend::Auto), DisplayBackend::X11,
                           &wayland_session(), None, "sig"),
            BackendDecision::Probe
        );
    }

    #[test]
    fn an_existing_gdk_backend_is_left_alone_when_no_flag_is_given() {
        let mut env = wayland_session();
        env.gdk_backend = Some("x11".into());
        assert_eq!(
            decide_backend(None, DisplayBackend::Auto, &env, None, "sig"),
            BackendDecision::Inherit
        );
    }

    #[test]
    fn a_saved_setting_beats_the_probe_but_not_the_environment() {
        assert_eq!(
            decide_backend(None, DisplayBackend::X11, &wayland_session(), None, "sig"),
            BackendDecision::Force("x11")
        );
        assert_eq!(
            decide_backend(None, DisplayBackend::Wayland, &wayland_session(), None, "sig"),
            BackendDecision::Force("wayland")
        );
    }

    #[test]
    fn auto_outside_a_wayland_session_never_probes() {
        assert_eq!(
            decide_backend(None, DisplayBackend::Auto, &x11_session(), None, "sig"),
            BackendDecision::Inherit
        );
    }

    #[test]
    fn auto_probes_when_no_verdict_is_cached_for_this_session() {
        assert_eq!(
            decide_backend(None, DisplayBackend::Auto, &wayland_session(), None, "sig"),
            BackendDecision::Probe
        );
    }

    #[test]
    fn a_cached_verdict_for_this_session_is_reused_instead_of_probing() {
        let crashed = ProbeCache { session: "sig".into(), crashed: true };
        assert_eq!(
            decide_backend(None, DisplayBackend::Auto, &wayland_session(),
                           Some(&crashed), "sig"),
            BackendDecision::Force("x11")
        );

        let fine = ProbeCache { session: "sig".into(), crashed: false };
        assert_eq!(
            decide_backend(None, DisplayBackend::Auto, &wayland_session(),
                           Some(&fine), "sig"),
            BackendDecision::Inherit
        );
    }

    #[test]
    fn a_verdict_from_a_different_session_is_ignored_and_reprobed() {
        let stale = ProbeCache { session: "gtk-4.16".into(), crashed: true };
        assert_eq!(
            decide_backend(None, DisplayBackend::Auto, &wayland_session(),
                           Some(&stale), "gtk-4.22"),
            BackendDecision::Probe
        );
    }

    #[test]
    fn renderer_flag_beats_the_environment_and_the_saved_setting() {
        let mut env = SessionEnv::default();
        env.gsk_renderer = Some("ngl".into());
        assert_eq!(
            decide_renderer(Some(RendererChoice::Cairo), RendererChoice::Vulkan, &env),
            RendererDecision::Force("cairo")
        );
    }

    #[test]
    fn an_explicit_auto_renderer_flag_hands_the_choice_back_to_gsk() {
        assert_eq!(
            decide_renderer(Some(RendererChoice::Auto), RendererChoice::Vulkan,
                            &SessionEnv::default()),
            RendererDecision::Inherit
        );
    }

    #[test]
    fn an_existing_gsk_renderer_is_left_alone_when_no_flag_is_given() {
        let mut env = SessionEnv::default();
        env.gsk_renderer = Some("cairo".into());
        assert_eq!(
            decide_renderer(None, RendererChoice::Vulkan, &env),
            RendererDecision::Inherit
        );
    }

    #[test]
    fn the_saved_renderer_applies_when_nothing_outranks_it() {
        let env = SessionEnv::default();
        assert_eq!(
            decide_renderer(None, RendererChoice::Gl, &env),
            RendererDecision::Force("gl")
        );
        assert_eq!(
            decide_renderer(None, RendererChoice::Gl, &env),
            RendererDecision::Force("gl")
        );
        assert_eq!(
            decide_renderer(None, RendererChoice::Auto, &env),
            RendererDecision::Inherit
        );
    }

    #[test]
    fn only_death_by_signal_counts_as_a_crashed_probe() {
        assert_eq!(classify_probe(false, Some(11)), ProbeOutcome::Crashed);
        assert_eq!(classify_probe(false, Some(6)), ProbeOutcome::Crashed);
        assert_eq!(classify_probe(false, Some(7)), ProbeOutcome::Crashed);
    }

    #[test]
    fn a_clean_exit_clears_wayland() {
        assert_eq!(classify_probe(true, None), ProbeOutcome::Ok);
    }

    #[test]
    fn a_plain_failure_is_inconclusive_and_does_not_downgrade_anyone() {
        assert_eq!(classify_probe(false, None), ProbeOutcome::Inconclusive);
    }

    #[test]
    fn the_session_signature_covers_the_compositor_and_the_gtk_build() {
        let cosmic = wayland_session();
        let mut gnome = wayland_session();
        gnome.current_desktop = Some("GNOME".into());

        assert_ne!(
            session_signature(&cosmic, (4, 16, 13), None),
            session_signature(&gnome, (4, 16, 13), None),
            "a verdict from one compositor must not be reused on another"
        );
        assert_ne!(
            session_signature(&cosmic, (4, 16, 13), None),
            session_signature(&cosmic, (4, 22, 4), None),
            "a runtime bump must invalidate the old verdict"
        );
        assert_eq!(
            session_signature(&cosmic, (4, 16, 13), None),
            session_signature(&cosmic, (4, 16, 13), None),
            "the same session must produce the same key"
        );
    }

    #[test]
    fn the_session_signature_covers_the_renderer_the_probe_ran_under() {
        // The probe child runs with whatever renderer the app will use, so a
        // crash it saw may have been the renderer's fault, not Wayland's.
        // Changing the renderer must therefore re-ask rather than inherit.
        let env = wayland_session();
        assert_ne!(
            session_signature(&env, (4, 16, 13), None),
            session_signature(&env, (4, 16, 13), Some("vulkan")),
            "a verdict taken under a forced renderer must not apply to the default"
        );
        assert_ne!(
            session_signature(&env, (4, 16, 13), Some("cairo")),
            session_signature(&env, (4, 16, 13), Some("vulkan")),
            "each renderer gets its own verdict"
        );
    }

    fn sh(script: &str) -> std::process::Child {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sh should be spawnable")
    }

    #[test]
    fn a_child_that_exits_cleanly_reads_as_ok() {
        let mut c = sh("exit 0");
        assert_eq!(
            wait_with_timeout(&mut c, std::time::Duration::from_secs(10)),
            ProbeOutcome::Ok
        );
    }

    #[test]
    fn a_child_killed_by_a_segfault_reads_as_crashed() {
        let mut c = sh("kill -SEGV $$");
        assert_eq!(
            wait_with_timeout(&mut c, std::time::Duration::from_secs(10)),
            ProbeOutcome::Crashed
        );
    }

    #[test]
    fn a_child_that_exits_non_zero_reads_as_inconclusive() {
        let mut c = sh("exit 3");
        assert_eq!(
            wait_with_timeout(&mut c, std::time::Duration::from_secs(10)),
            ProbeOutcome::Inconclusive
        );
    }

    #[test]
    fn a_hung_child_is_killed_and_reads_as_inconclusive_not_crashed() {
        let mut c = sh("sleep 30");
        let started = std::time::Instant::now();
        let outcome = wait_with_timeout(&mut c, std::time::Duration::from_millis(200));

        assert_eq!(outcome, ProbeOutcome::Inconclusive);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the wait must give up at the timeout, not wait out the child"
        );
        // The child is reaped, not left behind as a zombie.
        assert!(c.try_wait().is_ok());
    }

    #[test]
    fn the_session_is_read_from_the_expected_variable_names() {
        let env = SessionEnv::from_lookup(|k| match k {
            "WAYLAND_DISPLAY" => Some("wayland-1".into()),
            "GDK_BACKEND" => Some("wayland".into()),
            "GSK_RENDERER" => Some("cairo".into()),
            "XDG_SESSION_TYPE" => Some("wayland".into()),
            "XDG_CURRENT_DESKTOP" => Some("COSMIC".into()),
            _ => None,
        });

        assert_eq!(env.wayland_display.as_deref(), Some("wayland-1"));
        assert_eq!(env.gdk_backend.as_deref(), Some("wayland"));
        assert_eq!(env.gsk_renderer.as_deref(), Some("cairo"));
        assert_eq!(env.session_type.as_deref(), Some("wayland"));
        assert_eq!(env.current_desktop.as_deref(), Some("COSMIC"));
    }

    #[test]
    fn an_empty_environment_reads_as_all_unset() {
        let env = SessionEnv::from_lookup(|_| None);
        assert!(env.wayland_display.is_none());
        assert!(env.gdk_backend.is_none());
    }

    #[test]
    fn a_probe_command_that_segfaults_is_reported_as_crashed() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("kill -SEGV $$");
        assert_eq!(probe_with(cmd), ProbeOutcome::Crashed);
    }

    #[test]
    fn a_probe_command_that_cannot_be_started_is_inconclusive() {
        let cmd = std::process::Command::new("/nonexistent/sparkamp-probe");
        assert_eq!(probe_with(cmd), ProbeOutcome::Inconclusive);
    }

    #[test]
    fn a_probe_command_that_succeeds_clears_wayland() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("exit 0");
        assert_eq!(probe_with(cmd), ProbeOutcome::Ok);
    }

    #[test]
    fn a_crash_verdict_is_recorded_against_this_session() {
        let mut slot = None;
        assert!(record_probe(&mut slot, "sig", ProbeOutcome::Crashed));
        assert_eq!(
            slot,
            Some(ProbeCache { session: "sig".into(), crashed: true })
        );
    }

    #[test]
    fn a_clean_verdict_is_recorded_too_so_the_probe_runs_only_once() {
        let mut slot = None;
        assert!(record_probe(&mut slot, "sig", ProbeOutcome::Ok));
        assert_eq!(
            slot,
            Some(ProbeCache { session: "sig".into(), crashed: false })
        );
    }

    #[test]
    fn an_inconclusive_probe_records_nothing_and_is_retried_next_launch() {
        let mut slot = None;
        assert!(!record_probe(&mut slot, "sig", ProbeOutcome::Inconclusive));
        assert_eq!(slot, None);
    }

    #[test]
    fn an_inconclusive_probe_does_not_wipe_a_verdict_already_held() {
        let existing = ProbeCache { session: "sig".into(), crashed: true };
        let mut slot = Some(existing.clone());
        assert!(!record_probe(&mut slot, "sig", ProbeOutcome::Inconclusive));
        assert_eq!(slot, Some(existing));
    }

    #[test]
    fn re_recording_the_same_verdict_reports_no_change_so_nothing_is_rewritten() {
        let mut slot = Some(ProbeCache { session: "sig".into(), crashed: true });
        assert!(!record_probe(&mut slot, "sig", ProbeOutcome::Crashed));
    }

    #[test]
    fn a_verdict_from_another_session_is_replaced_wholesale() {
        let mut slot = Some(ProbeCache { session: "old".into(), crashed: true });
        assert!(record_probe(&mut slot, "new", ProbeOutcome::Ok));
        assert_eq!(
            slot,
            Some(ProbeCache { session: "new".into(), crashed: false })
        );
    }

    #[test]
    fn gdk_display_classes_read_as_backend_names() {
        assert_eq!(backend_display_name("GdkWaylandDisplay"), "Wayland");
        assert_eq!(backend_display_name("GdkX11Display"), "X11");
        assert_eq!(backend_display_name("GdkBroadwayDisplay"), "Broadway");
    }

    #[test]
    fn an_unrecognised_display_class_is_shown_as_is() {
        assert_eq!(backend_display_name("GdkFutureDisplay"), "GdkFutureDisplay");
    }

    #[test]
    fn gsk_renderer_classes_read_as_the_names_the_dropdown_uses() {
        assert_eq!(renderer_display_name("GskNglRenderer"), "ngl");
        assert_eq!(renderer_display_name("GskVulkanRenderer"), "vulkan");
        assert_eq!(renderer_display_name("GskGLRenderer"), "gl");
        assert_eq!(renderer_display_name("GskCairoRenderer"), "cairo");
    }

    #[test]
    fn an_unrecognised_renderer_class_is_shown_as_is() {
        assert_eq!(renderer_display_name("GskFutureRenderer"), "GskFutureRenderer");
    }

    #[test]
    fn an_ordinary_startup_produces_no_notes() {
        assert!(status_notes(&DisplayStatus::default()).is_empty());
    }

    #[test]
    fn a_backend_flag_is_called_out_so_the_dropdown_is_not_read_as_wrong() {
        let st = DisplayStatus {
            backend_flag: Some(DisplayBackend::X11),
            ..Default::default()
        };
        let notes = status_notes(&st);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("--backend=x11"), "got: {notes:?}");
    }

    #[test]
    fn a_renderer_flag_is_called_out_too() {
        let st = DisplayStatus {
            renderer_flag: Some(RendererChoice::Cairo),
            ..Default::default()
        };
        let notes = status_notes(&st);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("--renderer=cairo"), "got: {notes:?}");
    }

    #[test]
    fn the_fallback_explains_itself() {
        let st = DisplayStatus {
            probe: Some(ProbeOutcome::Crashed),
            fell_back_to_x11: true,
            ..Default::default()
        };
        let notes = status_notes(&st);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("X11"), "got: {notes:?}");
        assert!(notes[0].to_lowercase().contains("wayland"), "got: {notes:?}");
    }

    #[test]
    fn a_flag_and_a_fallback_each_get_their_own_line() {
        let st = DisplayStatus {
            renderer_flag: Some(RendererChoice::Cairo),
            probe: Some(ProbeOutcome::Crashed),
            fell_back_to_x11: true,
            ..Default::default()
        };
        assert_eq!(status_notes(&st).len(), 2);
    }
}
