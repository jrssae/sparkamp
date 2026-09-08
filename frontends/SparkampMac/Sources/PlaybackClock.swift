import Foundation

/// Elapsed and total time for the playing track, on its own publisher.
///
/// Split out of `SparkampModel` because these two values change 10 times a
/// second during playback while the model carries 105 published properties.
/// Anything holding the model as an `@EnvironmentObject` observes all of them,
/// so a window showing no clock at all still re-rendered on every tick: the tag
/// editor rebuilt 24 field rows and a lyrics `TextEditor` ten times a second
/// for values it never displayed.
///
/// Only the seek bar, the time display, the Touch Bar slider and the macOS Now
/// Playing card care. They observe this; nothing else does.
final class PlaybackClock: ObservableObject {
    /// Seconds elapsed.
    @Published var position: Double = 0
    /// Seconds total, or -1 when the duration is not known yet.
    @Published var duration: Double = -1
}
