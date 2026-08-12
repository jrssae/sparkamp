import SwiftUI
import AppKit

// Split out of SettingsWindow.swift on 2026-08-12: the file was 1,087
// lines of five unrelated panes. They were already siblings of
// `SettingsView` rather than nested in it, so each moved whole — the
// only edit is `private struct` -> `struct`, because file-private would
// now hide the pane from the host that instantiates it.

// MARK: - Behavior pane

/// GTK's Behavior tab: how playback, adding files, ReplayGain, play counts,
/// fadeout and discs behave. The disc and gnudb settings live here rather than
/// under Media Library because that is where GTK keeps them.
struct BehaviorPane: View {
    @EnvironmentObject var model: SparkampModel

    @State private var autoplayOnAdd: Bool = false
    @State private var addBehavior: Int    = 0    // 0=Append, 1=Replace
    @State private var playlistFormat: Int = 0    // 0=m3u8, 1=m3u

    // Discs / gnudb — GTK groups these under Behavior too.
    @State private var gnudbEmail: String = ""
    @State private var gnudbSubmitTest: Bool = true
    @State private var burnVerify: Bool = true
    @State private var autoShowInsertedCd: Bool = true

    // ReplayGain (playback normalization).
    @State private var rgEnabled: Bool     = true
    @State private var rgSource: Int       = 2    // 0=Track, 1=Album, 2=Automatic
    @State private var rgClip: Bool        = true
    @State private var rgFallback: Double  = 0.0

    // Play-count threshold (Phase 10, F11).
    @State private var playStatsEnabled: Bool = true
    @State private var playStatsMode: Int     = 0    // 0=Seconds, 1=Percent
    @State private var playStatsSeconds: Int  = 20
    @State private var playStatsPercent: Int  = 50
    @State private var fadeoutSeconds: Int    = 3

    var body: some View {
        Form {
            Section("Playlists") {
                Picker("Playlist format", selection: $playlistFormat) {
                    Text("m3u8 (UTF-8)").tag(0)
                    Text("m3u").tag(1)
                }
                .onChange(of: playlistFormat) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_playlist_format(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }
                Text("New playlists, Save As, and device exports use this format. Existing playlists keep their own.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Playback") {
                Toggle("Autoplay on add", isOn: $autoplayOnAdd)
                    .onChange(of: autoplayOnAdd) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_autoplay_on_add(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }

                Picker("Media library → playlist", selection: $addBehavior) {
                    Text("Append to playlist").tag(0)
                    Text("Replace playlist").tag(1)
                }
                .pickerStyle(.radioGroup)
                .onChange(of: addBehavior) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_playlist_add_behavior(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }
            }

            Section("ReplayGain") {
                Toggle("Use ReplayGain volume normalization", isOn: $rgEnabled)
                    .onChange(of: rgEnabled) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rg_enabled(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }

                Picker("ReplayGain source", selection: $rgSource) {
                    Text("Track").tag(0)
                    Text("Album").tag(1)
                    Text("Automatic").tag(2)
                }
                .onChange(of: rgSource) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_rg_source(ctx, Int32(newValue))
                    sparkamp_save_config(ctx)
                }
                .disabled(!rgEnabled)

                Toggle("Clipping protection", isOn: $rgClip)
                    .onChange(of: rgClip) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_rg_clip_protection(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }
                    .disabled(!rgEnabled)

                Stepper(
                    "Fallback gain (no RG info): \(rgFallback, specifier: "%.1f") dB",
                    value: $rgFallback, in: -15...15, step: 0.5
                )
                .onChange(of: rgFallback) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_rg_fallback_db(ctx, Float(newValue))
                    sparkamp_save_config(ctx)
                }
                .disabled(!rgEnabled)

                Text("Applied to tracks that have no ReplayGain value. Automatic uses album gain unless shuffle is on.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Play Count") {
                Toggle("Count plays", isOn: $playStatsEnabled)
                    .onChange(of: playStatsEnabled) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_play_stats_enabled(ctx, newValue)
                        sparkamp_save_config(ctx)
                    }

                Picker("Count after", selection: $playStatsMode) {
                    Text("N seconds").tag(0)
                    Text("N% of track").tag(1)
                }
                .onChange(of: playStatsMode) { _, newValue in
                    guard let ctx = model.ctx else { return }
                    sparkamp_set_play_stats_mode(ctx, UInt32(newValue))
                    sparkamp_save_config(ctx)
                }
                .disabled(!playStatsEnabled)

                Stepper("After \(playStatsSeconds) seconds", value: $playStatsSeconds, in: 1...3600)
                    .onChange(of: playStatsSeconds) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_play_stats_seconds(ctx, UInt32(newValue))
                        sparkamp_save_config(ctx)
                    }
                    .disabled(!playStatsEnabled || playStatsMode != 0)

                Stepper("After \(playStatsPercent)% of track", value: $playStatsPercent, in: 1...100)
                    .onChange(of: playStatsPercent) { _, newValue in
                        guard let ctx = model.ctx else { return }
                        sparkamp_set_play_stats_percent(ctx, UInt32(newValue))
                        sparkamp_save_config(ctx)
                    }
                    .disabled(!playStatsEnabled || playStatsMode != 1)

                Text("A track's play count and last-played date update in the Media Library once playback passes this point. Tracks shorter than the threshold count near the end.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Stop With Fadeout") {
                Stepper("Fade length (seconds): \(fadeoutSeconds)",
                        value: $fadeoutSeconds, in: 1...10)
                    .onChange(of: fadeoutSeconds) { _, newValue in
                        model.setFadeoutSeconds(newValue)
                    }

                Text("Shift+V ramps playback down to silence over this long and then stops. Plain Stop (v) is still immediate.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            // ── Discs ─────────────────────────────────────────────────────
            // Grouped here, not under Media Library, to match GTK's Behavior
            // tab.
            Section("gnudb submissions") {
                TextField("you@example.com", text: $gnudbEmail)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    .onSubmit { saveGnudbEmail() }
                Text("Your address, sent with gnudb disc submissions (gnudb requires a personal one — Sparkamp asks on the first submission if this is blank). Lookups work without it.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Toggle("Submit in test mode", isOn: $gnudbSubmitTest)
                    .onChange(of: gnudbSubmitTest) { _, v in
                        if let ctx = model.ctx { sparkamp_set_gnudb_submit_test(ctx, v) }
                    }
                Text("gnudb validates test submissions without publishing them. Turn off once a real submission is confirmed working.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Disc burning") {
                Toggle("Verify discs after burning", isOn: $burnVerify)
                    .onChange(of: burnVerify) { _, v in
                        if let ctx = model.ctx { sparkamp_set_burn_verify(ctx, v) }
                    }
                Text("Reads the disc back after a burn to catch bad media. Slower; turn off to trade safety for speed.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section("Audio CD inserted") {
                Toggle("Open the Media Library", isOn: $autoShowInsertedCd)
                    .onChange(of: autoShowInsertedCd) { _, v in
                        if let ctx = model.ctx { sparkamp_set_auto_show_inserted_cd(ctx, v) }
                    }
                Text("Shows the Media Library at the drive that received the disc. To have macOS launch Sparkamp automatically on insert, set it as the handler in System Settings.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("Open CDs & DVDs Settings…") { openCdDvdSettings() }
                    .buttonStyle(.bordered)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            guard let ctx = model.ctx else { return }
            autoplayOnAdd  = sparkamp_get_autoplay_on_add(ctx)
            let p = sparkamp_get_gnudb_email(ctx)
            gnudbEmail = p.map { String(cString: $0) } ?? ""
            sparkamp_free_string(p)
            gnudbSubmitTest    = sparkamp_get_gnudb_submit_test(ctx)
            burnVerify         = sparkamp_get_burn_verify(ctx)
            autoShowInsertedCd = sparkamp_get_auto_show_inserted_cd(ctx)
            addBehavior    = Int(sparkamp_get_playlist_add_behavior(ctx))
            playlistFormat = Int(sparkamp_get_playlist_format(ctx))
            rgEnabled      = sparkamp_get_rg_enabled(ctx)
            rgSource       = Int(sparkamp_get_rg_source(ctx))
            rgClip         = sparkamp_get_rg_clip_protection(ctx)
            rgFallback     = Double(sparkamp_get_rg_fallback_db(ctx))
            playStatsEnabled = sparkamp_get_play_stats_enabled(ctx)
            playStatsMode    = Int(sparkamp_get_play_stats_mode(ctx))
            playStatsSeconds = Int(sparkamp_get_play_stats_seconds(ctx))
            playStatsPercent = Int(sparkamp_get_play_stats_percent(ctx))
            fadeoutSeconds   = Int(sparkamp_get_fadeout_secs(ctx))
        }
        // The email field commits on Return; catch the case where the pane
        // closes with an uncommitted edit still in it.
        .onDisappear { saveGnudbEmail() }
    }

    private func saveGnudbEmail() {
        guard let ctx = model.ctx else { return }
        gnudbEmail.withCString { sparkamp_set_gnudb_email(ctx, $0) }
    }

    /// Open the macOS "CDs & DVDs" pane, where the "When you insert a music CD"
    /// action lives. We never write that preference programmatically (it's in
    /// Apple's protected `com.apple.digihub` domain) — the user points it at
    /// Sparkamp.app once here.
    private func openCdDvdSettings() {
        let pane = URL(fileURLWithPath: "/System/Library/PreferencePanes/DigiHubDiscs.prefPane")
        if FileManager.default.fileExists(atPath: pane.path) {
            NSWorkspace.shared.open(pane)
        } else if let url = URL(string: "x-apple.systempreferences:com.apple.preferences.DigiHubDiscs") {
            NSWorkspace.shared.open(url)
        }
    }
}
