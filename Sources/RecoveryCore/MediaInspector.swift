import Foundation
import ImageIO
import CoreGraphics

public enum MediaValidation: String, Codable, Sendable {
    case jpegDecoded, rawUnverified, failed, unchecked
    public var label: String {
        switch self {
        case .jpegDecoded: "JPEG 디코딩 확인"
        case .rawUnverified: "RAW 전체 디코딩 미검증"
        case .failed: "이미지 디코딩 실패"
        case .unchecked: "검증 제한 초과 · 미검증"
        }
    }
}

public enum MediaInspector {
    public static func validate(url: URL, format: Candidate.Format) -> MediaValidation {
        guard format == .jpeg else { return .rawUnverified }
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              CGImageSourceGetStatus(source) == .statusComplete,
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int,
              width > 0, height > 0 else { return .failed }
        guard width <= 20000, height <= 20000, width * height <= 80_000_000 else { return .unchecked }
        let options = [kCGImageSourceShouldCacheImmediately: true] as CFDictionary
        guard let image = CGImageSourceCreateImageAtIndex(source, 0, options),
              CGImageSourceGetStatusAtIndex(source, 0) == .statusComplete,
              let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                                      bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return .failed }
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return .jpegDecoded
    }

    public static func thumbnail(imageURL: URL, candidate: Candidate) throws -> CGImage? {
        let values = try imageURL.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
        guard values.isRegularFile == true, values.isSymbolicLink != true else { return nil }
        let handle = try FileHandle(forReadingFrom: imageURL)
        defer { try? handle.close() }
        var start = candidate.offset
        var length = candidate.length
        if candidate.format == .raf {
            try handle.seek(toOffset: start)
            guard let header = try handle.read(upToCount: 108), header.count == 108 else { return nil }
            let offset = header[84..<88].reduce(UInt64(0)) { $0 << 8 | UInt64($1) }
            let size = header[88..<92].reduce(UInt64(0)) { $0 << 8 | UInt64($1) }
            guard offset <= length, size <= length - offset, start <= UInt64.max - offset else { return nil }
            start += offset
            length = size
        }
        guard length > 0, length <= 64 * 1024 * 1024 else { return nil }
        try handle.seek(toOffset: start)
        guard let data = try handle.read(upToCount: Int(length)), data.count == Int(length),
              let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceThumbnailMaxPixelSize: 1200,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }
}
