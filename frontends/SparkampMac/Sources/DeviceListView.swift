import SwiftUI

// MARK: - Capacity bar
//
// One component used by the sidebar, overview, and (later) detail so the
// fill color is always consistent: yellow under 15% free, red under 5%, else
// the skin accent.

struct CapacityBar: View {
    /// Free fraction in 0…1.
    let freeFraction: Double
    var accent: Color
    var track: Color
    var height: CGFloat = 6

    private var usedFraction: Double { (1 - freeFraction).clamped(to: 0...1) }
    private var fillColor: Color {
        if freeFraction < 0.05 { return .red }
        if freeFraction < 0.15 { return .yellow }
        return accent
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                RoundedRectangle(cornerRadius: height / 2).fill(track)
                RoundedRectangle(cornerRadius: height / 2)
                    .fill(fillColor)
                    .frame(width: max(0, geo.size.width * usedFraction))
            }
        }
        .frame(height: height)
    }
}

/// "12.3 GB free of 64 GB" — shared capacity caption.
func deviceCapacityText(_ device: Device) -> String {
    guard device.totalBytes > 0 else { return "Capacity unavailable" }
    let f = ByteCountFormatter()
    f.countStyle = .file
    let free = f.string(fromByteCount: Int64(device.freeBytes))
    let total = f.string(fromByteCount: Int64(device.totalBytes))
    return "\(free) free of \(total)"
}

// MARK: - Overview (grid of device cards)

struct DeviceOverview: View {
    let devices: [Device]
    let counts: [String: DeviceCounts]
    let theme: SkinTheme
    let vars: SkinVars
    let onSelect: (Device) -> Void

    private let columns = [GridItem(.adaptive(minimum: 240), spacing: 16)]

    var body: some View {
        ScrollView {
            if devices.isEmpty {
                emptyState
            } else {
                LazyVGrid(columns: columns, spacing: 16) {
                    ForEach(devices) { dev in
                        card(dev)
                    }
                }
                .padding(16)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.background)
    }

    @ViewBuilder
    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "externaldrive")
                .font(.system(size: 36))
                .foregroundStyle(theme.playlistDurationText)
            Text("No devices connected")
                .font(vars.bodyFont.weight(.semibold))
                .foregroundStyle(theme.playlistText)
            Text("Connect a USB drive or SD card to sync music.")
                .font(vars.bodyFont)
                .foregroundStyle(theme.playlistDurationText)
        }
        .frame(maxWidth: .infinity, minHeight: 240)
        .padding(40)
    }

    @ViewBuilder
    private func card(_ dev: Device) -> some View {
        let unsupported = dev.backend == .unsupported
        Button { onSelect(dev) } label: {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    Image(systemName: unsupported
                          ? (dev.fsType == "ios" ? "iphone" : "camera")
                          : "externaldrive.fill")
                        .foregroundStyle(theme.vars.highlight)
                    Text(dev.label.isEmpty ? "Untitled" : dev.label)
                        .font(vars.bodyFont.weight(.semibold))
                        .foregroundStyle(theme.playlistText)
                        .lineLimit(1)
                    Spacer()
                    if dev.readOnly && !unsupported {
                        Text("read-only")
                            .font(.system(size: 10))
                            .foregroundStyle(theme.playlistDurationText)
                    }
                }

                if unsupported {
                    Text(dev.fsType == "ios"
                         ? "iPhone / iPad — music sync isn't supported"
                         : "PTP camera — photo transfer only")
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                        .fixedSize(horizontal: false, vertical: true)
                } else {
                    CapacityBar(freeFraction: dev.freeFraction,
                                accent: theme.vars.highlight,
                                track: theme.windowBorder.opacity(0.4))

                    Text(deviceCapacityText(dev))
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)

                    Text(countsLine(dev))
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                }
            }
            .padding(12)
            .background(
                RoundedRectangle(cornerRadius: 8).fill(theme.playlistCurrentBg.opacity(0.5))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8).stroke(theme.windowBorder, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }

    private func countsLine(_ dev: Device) -> String {
        guard dev.fsVisible else { return "No readable storage" }
        guard let c = counts[dev.id] else { return "Counting…" }
        let songs = c.songs == 1 ? "1 song" : "\(c.songs) songs"
        let pls = c.playlists == 1 ? "1 playlist" : "\(c.playlists) playlists"
        return "\(songs) · \(pls)"
    }
}
