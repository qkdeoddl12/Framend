import Foundation

public struct SourceFingerprint: Codable, Equatable, Sendable {
    public let device: UInt64
    public let inode: UInt64
    public let size: UInt64
    public let modified_seconds: Int64
    public let modified_nanos: Int64
    public let changed_seconds: Int64
    public let changed_nanos: Int64
}

public struct Candidate: Codable, Equatable, Identifiable, Sendable {
    public enum Format: String, Codable, Sendable { case jpeg, raf }
    public let offset: UInt64
    public let length: UInt64
    public let format: Format
    public let width: UInt32?
    public let height: UInt32?
    public let camera: String?
    public var id: UInt64 { offset }
    public var filename: String { "\(String(offset, radix: 16)).\(format == .jpeg ? "jpg" : "raf")" }
    public var statusLabel: String { format == .raf ? "구조 확인 · RAW 미검증" : "구조 확인 · 픽셀 미검증" }
}

public struct RecoveryRequest: Encodable, Sendable {
    public let source: SourceFingerprint
    public let candidates: [Candidate]
    public init(source: SourceFingerprint, candidates: [Candidate]) { self.source = source; self.candidates = candidates }
}

public struct EngineEvent: Decodable, Sendable {
    public enum Kind: String, Decodable, Sendable { case started, progress, candidate, saved, complete }
    public let event: Kind
    public let source: SourceFingerprint?
    public let candidate: Candidate?
    public let scanned: UInt64?
    public let total: UInt64?
    public let path: String?
    public let sha256: String?
    public let count: Int?
    public let directory: String?
}

public struct StreamValidator: Sendable {
    private var resultCount = 0
    private var completed = false
    public init() {}
    public mutating func accept(_ event: EngineEvent) throws {
        guard !completed else { throw EngineError.protocolError }
        switch event.event {
        case .started:
            guard event.source != nil else { throw EngineError.protocolError }
        case .progress:
            guard let scanned = event.scanned, let total = event.total, scanned <= total else { throw EngineError.protocolError }
        case .candidate, .saved:
            guard let candidate = event.candidate, candidate.length > 0,
                  candidate.offset <= UInt64.max - candidate.length else { throw EngineError.protocolError }
            if event.event == .saved {
                guard let path = event.path, path.hasPrefix("/"), let hash = event.sha256,
                      hash.count == 64, hash.allSatisfy(\.isHexDigit) else { throw EngineError.protocolError }
            }
            resultCount += 1
        case .complete:
            guard event.count == resultCount else { throw EngineError.protocolError }
            completed = true
        }
    }
    public func finish() throws {
        guard completed else { throw EngineError.incomplete }
    }
}

public enum EngineError: LocalizedError, Sendable {
    case protocolError, incomplete, missingEngine, failed(String)
    public var errorDescription: String? {
        switch self {
        case .protocolError: "복구 엔진의 응답이 올바르지 않습니다."
        case .incomplete: "작업이 끝나기 전에 엔진이 종료되었습니다. 다시 시도하세요."
        case .missingEngine: "복구 엔진을 찾을 수 없습니다. scripts/build-app.sh로 앱을 다시 빌드하세요."
        case .failed(let message): message
        }
    }
}
