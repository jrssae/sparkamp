import Foundation

// MARK: - Shared FFI marshaling

/// The two things every call across the Rust boundary needs: taking ownership
/// of a returned C string, and the snake_case JSON round trip the core speaks.
///
/// `DiscService` and `DeviceService` had each grown their own private copy of
/// all of this — `takeString` was character-for-character the same function in
/// both, and each carried its own `JSONDecoder`/`JSONEncoder` pair configured
/// identically. Nothing was wrong with either copy; the problem is that the
/// ownership contract (copy, then free, exactly once) was being restated rather
/// than referenced, and a third service would have restated it again.
///
/// A 2026-08-11 audit of all 45 `String(cString:)` sites in the frontend found
/// **no leaks** — every owned pointer is freed and the one that is not
/// (`sparkamp_audio_extension`) returns a static the core explicitly documents
/// as must-not-free. So this exists to keep that record clean going forward,
/// not to repair it. New FFI call sites should reach for `takeString` rather
/// than hand-rolling the pair.
enum SparkampFFI {

    /// The core emits and expects snake_case; these are the only two coders
    /// configured for it, shared so a third service cannot pick a different
    /// strategy and silently fail to decode.
    static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }()

    /// Take ownership of a C string returned by the FFI: copy it into a Swift
    /// `String` and free the original.
    ///
    /// Only for pointers the core says the caller owns — the header spells this
    /// out per function ("Free with `sparkamp_free_string`"). Static pointers
    /// such as `sparkamp_audio_extension`'s must not come through here.
    static func takeString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
        guard let ptr = ptr else { return nil }
        defer { sparkamp_free_string(ptr) }
        return String(cString: ptr)
    }

    /// Encode a payload to the snake_case JSON the Rust side expects.
    static func encodeJSON<T: Encodable>(_ value: T) -> String? {
        guard let data = try? encoder.encode(value) else { return nil }
        return String(data: data, encoding: .utf8)
    }

    /// Decode a snake_case JSON string returned by the core.
    static func decodeJSON<T: Decodable>(_ s: String?) -> T? {
        guard let s = s, let data = s.data(using: .utf8) else { return nil }
        return try? decoder.decode(T.self, from: data)
    }
}
