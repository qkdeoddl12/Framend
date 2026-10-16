import Foundation

public struct VideoExtent: Codable, Equatable, Sendable {
    public let offset: UInt64
    public let length: UInt64
}

public struct VideoCandidate: Codable, Identifiable, Equatable, Sendable {
    public let id: UInt64
    public let name: String
    public let length: UInt64
    public let extents: [VideoExtent]
    public let deleted: Bool
    public let recoverable: Bool
    public let reason: String?
    public let allocated_clusters: UInt64
    public var statusLabel: String {
        if !recoverable { return reason ?? "조각 순서 확인 불가" }
        return deleted ? "삭제 흔적 · 디코딩 필요" : "저장 순서 확인 · 디코딩 필요"
    }
}

public struct VideoScan: Codable, Sendable {
    public let source: SourceFingerprint
    public let candidates: [VideoCandidate]
    public let warnings: [String]
}

public struct NativeSalvageReport: Codable, Sendable {
    public let codec: String
    public let fps_num: UInt32
    public let fps_den: UInt32
    public let frames: UInt64
    public let extents: [VideoExtent]
    public let skipped_bytes: UInt64
    public let input_sha256: String
    public let reference_sha256: String
    public let warnings: [String]
    public let partial: Bool
    public let frame_count_basis: String
    public let timing_basis: String
    public let segments: UInt64
}

public struct VideoTrackValidation: Codable, Sendable {
    public let index: Int
    public let type: String
    public let codec: String
    public let decodedFrames: Int
    public let decodedSamples: Int64
    public let declaredDuration: String?
    public let decodedStart: Double?
    public let decodedEnd: Double?
}

public struct VideoValidation: Codable, Sendable {
    public let status: String
    public let tracks: [VideoTrackValidation]
    public let diagnostics: String
    public var passed: Bool {
        status == "decoded" && tracks.contains { $0.type == "video" && $0.decodedFrames > 0 }
            && tracks.allSatisfy { $0.decodedFrames > 0 }
    }
    public init(status: String, tracks: [VideoTrackValidation], diagnostics: String) {
        self.status = status; self.tracks = tracks; self.diagnostics = diagnostics
    }
}

public struct VideoResult: Codable, Sendable, Identifiable {
    public let file: String
    public let sha256: String
    public let method: String
    public let validation: VideoValidation
    public let warnings: [String]
    public var id: String { file }
    public var label: String {
        if method == "native_nal_salvage" {
            return validation.passed ? "영상 트랙 부분 복구 · 디코딩 통과" : "부분 복구 · 검사 오류"
        }
        return validation.passed ? "전체 디코딩 통과 · 원본 완전성 미상" : "부분 복구 · 검사 오류"
    }
}

public struct VideoOutcome: Sendable {
    public let directory: URL
    public let results: [VideoResult]
}

public struct VideoUpdate: Sendable {
    public let stage: String
    public let fraction: Double?
    public let directory: URL?
    public init(_ stage: String, fraction: Double? = nil, directory: URL? = nil) { self.stage = stage; self.fraction = fraction; self.directory = directory }
}

struct VideoCLIEvent: Decodable {
    let event: String
    let scanned: UInt64?
    let total: UInt64?
    let sha256: String?
    let report: NativeSalvageReport?
}

public enum VideoError: LocalizedError {
    case failed(String)
    public var errorDescription: String? { if case .failed(let message) = self { message } else { nil } }
}
