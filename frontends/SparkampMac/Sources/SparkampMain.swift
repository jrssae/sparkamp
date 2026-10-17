import SwiftUI

/// The process entry point.
///
/// On its own so the Swift test bundle can compile every other source file:
/// `@main` on `SparkampMacApp` made that file an entry point, and a test
/// bundle that included it would try to be an app. The tests leave this file
/// out, and nothing else needs it.
@main
enum SparkampMain {
    static func main() {
        SparkampMacApp.main()
    }
}
