import Foundation

public enum VideoRecovery {
    public static func availability() -> String? {
        do { _ = try VideoTools(); return nil } catch { return error.localizedDescription }
    }
    public static func scan(image: URL, receive: @escaping @Sendable (VideoUpdate) async -> Void = { _ in }) async throws -> VideoScan {
        let source = try VideoInput(image); let tools = try VideoTools()
        await receive(VideoUpdate("exFAT 파일 목록과 조각 순서를 분석하고 있습니다"))
        let result = try await ToolProcess.run(executable: tools.engine, arguments: ["video-scan", image.path], timeout: 1800, outputLimit: 32 * 1024 * 1024)
        try checked(result); try source.unchanged()
        return try JSONDecoder().decode(VideoScan.self, from: result.stdout)
    }
    public static func repair(input: URL, reference: URL, destination: URL,
                              receive: @escaping @Sendable (VideoUpdate) async -> Void = { _ in }) async throws -> VideoOutcome {
        let source = try VideoInput(input); let template = try VideoInput(reference)
        guard !source.sameFile(as: template) else { throw VideoError.failed("손상 영상과 정상 참고 영상은 서로 다른 파일을 선택하세요.") }
        let tools = try VideoTools(); let session = try VideoSession(destination: destination)
        await receive(VideoUpdate("작업 폴더 준비", directory: session.directory))
        do {
            let (needed, overflow) = source.size.multipliedReportingOverflow(by: 2)
            guard !overflow else { throw VideoError.failed("입력 크기가 너무 큽니다.") }
            try session.checkSpace(needed)
            try await session.writeJSON(tools.evidence(), named: "tools.json")
            let referenceValidation = try await VideoValidator.validate(reference, tools: tools, session: session, prefix: "reference", receive: receive)
            try session.writeJSON(referenceValidation, named: "reference-validation.json")
            guard referenceValidation.passed else { throw VideoError.failed("참고 영상의 전체 디코딩이 실패했습니다. 정상 영상을 선택하세요.") }
            let elementary = try session.path("salvaged.es")
            await receive(VideoUpdate("영상의 NAL 구조와 프레임 연결을 분석하고 있습니다", fraction: 0))
            let operation = try await ToolProcess.run(executable: tools.engine,
                arguments: ["video-salvage", input.path, reference.path, elementary.path], timeout: 3600,
                receive: progress("프레임 구조 분석", receive))
            try session.write(operation.stderr, named: "native-salvage.log")
            try checked(operation)
            guard let report = try completion(operation.stdout).report, ["h264", "hevc"].contains(report.codec),
                  report.partial, report.frames > 0, report.fps_num > 0, report.fps_den > 0 else {
                throw VideoError.failed("엔진에서 유효한 프레임 복구 결과를 받지 못했습니다.")
            }
            try source.unchanged(); try template.unchanged()
            try session.writeJSON(report, named: "native-report.json")
            let output = try session.path("recovered-video.mp4")
            await receive(VideoUpdate("복구한 프레임을 영상 파일로 저장하고 있습니다"))
            let mux = try await ToolProcess.run(executable: tools.ffmpeg,
                arguments: ["-nostdin", "-v", "error", "-fflags", "+genpts", "-r", "\(report.fps_num)/\(report.fps_den)", "-f", report.codec,
                    "-i", elementary.path, "-map", "0:v:0", "-c:v", "copy", "-n", output.path], timeout: 3600, outputLimit: 512 * 1024)
            try session.write(mux.stderr, named: "mux.log"); try checked(mux)
            let validation = try await VideoValidator.validate(output, tools: tools, session: session, prefix: "recovered", receive: receive)
            let warnings = report.warnings + ["영상 트랙만 부분 복구했습니다. 음성과 원래 타임스탬프는 복원하지 않았습니다.", "재생 속도는 참고 영상의 평균 프레임 속도를 사용합니다. 원본 완전성과 장면 순서는 보장하지 않습니다."]
            let result = VideoResult(file: output.lastPathComponent, sha256: try VideoInput(output).sha256(), method: "native_nal_salvage", validation: validation, warnings: warnings)
            try source.unchanged(); try template.unchanged()
            try session.writeJSON([result], named: "results.json")
            if validation.passed { try session.finish() }
            return VideoOutcome(directory: session.directory, results: [result])
        } catch { session.failed(error); throw error }
    }
    public static func extract(image: URL, scan: VideoScan, candidates: [VideoCandidate], destination: URL,
                               receive: @escaping @Sendable (VideoUpdate) async -> Void = { _ in }) async throws -> VideoOutcome {
        guard !candidates.isEmpty, candidates.allSatisfy({ $0.recoverable && scan.candidates.contains($0) }),
              Set(candidates.map(\.id)).count == candidates.count else { throw VideoError.failed("조각 순서를 확인한 파일을 선택하세요.") }
        let source = try VideoInput(image); let tools = try VideoTools(); let session = try VideoSession(destination: destination)
        await receive(VideoUpdate("작업 폴더 준비", directory: session.directory))
        do {
            var needed: UInt64 = 0
            for candidate in candidates {
                let (sum, overflow) = needed.addingReportingOverflow(candidate.length)
                guard !overflow else { throw VideoError.failed("출력 크기가 너무 큽니다.") }; needed = sum
            }
            try session.checkSpace(needed)
            try session.writeJSON(scan, named: "exfat-scan.json")
            try await session.writeJSON(tools.evidence(), named: "tools.json")
            struct Request: Encodable { let source: SourceFingerprint; let candidate: VideoCandidate }
            var results: [VideoResult] = []
            for (index, candidate) in candidates.enumerated() {
                try Task.checkCancellation(); try source.unchanged()
                let prefix = "file-\(candidate.id)"
                let output = try session.path("\(prefix).mov")
                await receive(VideoUpdate("\(index + 1)/\(candidates.count): \(candidate.name) 조각 복원", fraction: 0))
                let request = try JSONEncoder().encode(Request(source: scan.source, candidate: candidate))
                let operation = try await ToolProcess.run(executable: tools.engine,
                    arguments: ["video-extract", image.path, output.path], input: request, timeout: 3600,
                    receive: progress("\(candidate.name) 조각 복원", receive))
                try session.write(operation.stderr, named: "\(prefix)-extract.log"); try checked(operation)
                guard let hash = try completion(operation.stdout).sha256, hash.count == 64,
                      hash == (try VideoInput(output).sha256()) else { throw VideoError.failed("복원 파일의 해시가 일치하지 않습니다.") }
                let validation = try await VideoValidator.validate(output, tools: tools, session: session, prefix: prefix, receive: receive)
                let result = VideoResult(file: output.lastPathComponent, sha256: hash, method: "exfat_extent_recovery", validation: validation,
                    warnings: candidate.deleted ? ["삭제된 파일의 남은 메타데이터를 사용했습니다. 과거 원본과의 동일성은 확정할 수 없습니다."] : [])
                results.append(result)
                try session.writeJSON(result, named: "\(prefix)-result.json")
            }
            try source.unchanged(); try session.writeJSON(results, named: "results.json")
            if results.allSatisfy({ $0.validation.passed }) { try session.finish() }
            return VideoOutcome(directory: session.directory, results: results)
        } catch { session.failed(error); throw error }
    }
    private static func checked(_ result: ToolResult) throws {
        guard result.status == 0 else {
            throw VideoError.failed("작업 실패 (\(result.status)): " + String(decoding: result.stderr.suffix(8192), as: UTF8.self))
        }
    }
    private static func completion(_ data: Data) throws -> VideoCLIEvent {
        let events = try data.split(separator: 10).map { try JSONDecoder().decode(VideoCLIEvent.self, from: Data($0)) }
        guard let event = events.last, event.event == "complete", events.filter({ $0.event == "complete" }).count == 1 else {
            throw VideoError.failed("엔진이 완료 기록을 남기지 않았습니다.")
        }
        return event
    }
    private static func progress(_ stage: String, _ receive: @escaping @Sendable (VideoUpdate) async -> Void) -> @Sendable (String) async -> Void {
        { line in
            if let event = try? JSONDecoder().decode(VideoCLIEvent.self, from: Data(line.utf8)), event.event == "progress",
               let scanned = event.scanned, let total = event.total, total > 0, scanned <= total {
                await receive(VideoUpdate(stage, fraction: Double(scanned) / Double(total)))
            }
        }
    }
}
