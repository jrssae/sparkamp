import SwiftUI

// Scrolling marquee — scrolls left when text overflows the available width.
// Uses a Task + async sleep loop rather than a Timer so it cancels cleanly.
struct MarqueeView: View {
    let text: String

    @EnvironmentObject var themeManager: ThemeManager

    @State private var textWidth:      CGFloat = 0
    @State private var containerWidth: CGFloat = 0
    @State private var offset:         CGFloat = 0

    private let speed:         CGFloat = 40   // pixels / second
    private let pauseDuration: Double  = 2.0  // seconds at each end

    var body: some View {
        let t = themeManager.currentTheme
        let vars = themeManager.currentVars
        TimelineView(.animation) { _ in
            GeometryReader { geo in
                let overflows = textWidth > geo.size.width
                ZStack(alignment: .leading) {
                    Text(text)
                        .font(vars.marqueeFont)
                        .foregroundStyle(t.titleText)
                        .lineLimit(1)
                        .fixedSize()
                        .offset(x: overflows ? offset : 0)
                        .background(
                            GeometryReader { inner in
                                Color.clear
                                    .onAppear { textWidth = inner.size.width }
                                    .onChange(of: text) {
                                        textWidth = inner.size.width
                                        offset = 0
                                    }
                            }
                        )
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .clipped()
                .onAppear { containerWidth = geo.size.width }
                .onChange(of: geo.size.width) { _, w in containerWidth = w }
            }
        }
        .onChange(of: text) { offset = 0 }
        .task(id: text) {
            guard !text.isEmpty else { return }
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 100_000_000)
                guard textWidth > containerWidth else {
                    try? await Task.sleep(nanoseconds: 500_000_000)
                    continue
                }
                // Pause at start.
                try? await Task.sleep(nanoseconds: UInt64(pauseDuration * 1_000_000_000))
                // Scroll to end.
                let travel = textWidth - containerWidth
                let scrollDuration = Double(travel) / Double(speed)
                let steps = max(Int(scrollDuration * 60), 1)
                for step in 1...steps {
                    if Task.isCancelled { break }
                    withAnimation(.linear(duration: 1.0 / 60.0)) {
                        offset = -CGFloat(step) / CGFloat(steps) * travel
                    }
                    try? await Task.sleep(nanoseconds: 16_666_667)
                }
                // Pause at end.
                try? await Task.sleep(nanoseconds: UInt64(pauseDuration * 1_000_000_000))
                // Jump back instantly.
                withAnimation(.none) { offset = 0 }
            }
        }
    }
}
