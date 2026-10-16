import Foundation

struct VideoTools: Sendable {
    let engine: URL
    let ffmpeg: URL
    let ffprobe: URL
    init(engine: URL, ffmpeg: URL, ffprobe: URL) {
        self.engine = engine; self.ffmpeg = ffmpeg; self.ffprobe = ffprobe
    }
    init() throws {
        engine = try EngineClient.locateEngine()
        func locate(_ name: String) throws -> URL {
            let paths = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] + (ProcessInfo.processInfo.environment["PATH"] ?? "").split(separator: ":").map(String.init)
            guard let url = paths.map({ URL(filePath: $0).appending(path: name) }).first(where: { FileManager.default.isExecutableFile(atPath: $0.path) }) else {
                throw VideoError.failed("\(name)이 필요합니다. Homebrew의 ffmpeg 패키지를 설치해 주세요.")
            }
            return url
        }
        ffmpeg = try locate("ffmpeg"); ffprobe = try locate("ffprobe")
    }
    func evidence() async throws -> [String: String] {
        let result = try await ToolProcess.run(executable: ffmpeg, arguments: ["-version"], timeout: 10, outputLimit: 128 * 1024)
        return ["engine_sha256": try VideoInput(engine).sha256(), "ffmpeg_sha256": try VideoInput(ffmpeg.resolvingSymlinksInPath()).sha256(), "ffmpeg_version": String(decoding: result.stdout, as: UTF8.self), "app": "Framend native video preview 0.3"]
    }
}

// Force the MOV demuxer and disable external data references on untrusted media.
private let movieInputOptions = ["-max_alloc", "268435456", "-protocol_whitelist", "file,pipe", "-enable_drefs", "0", "-use_absolute_path", "0", "-f", "mov"]

struct VideoValidator {
    struct Probe: Decodable {
        let streams: [Stream]
        struct Stream: Decodable {
            let index: Int
            let codec_type: String?
            let codec_name: String?
            let width: Int?
            let height: Int?
            let channels: Int?
            let sample_rate: String?
            let duration: String?
        }
    }
    static func validate(_ file: URL, tools: VideoTools, session: VideoSession, prefix: String,
                         receive: @escaping @Sendable (VideoUpdate) async -> Void) async throws -> VideoValidation {
        let probe = try await ToolProcess.run(executable: tools.ffprobe,
            arguments: ["-v", "error"] + movieInputOptions + ["-show_streams", "-of", "json", file.path], timeout: 60, outputLimit: 2 * 1024 * 1024)
        try session.write(probe.stderr, named: "\(prefix)-probe.log")
        guard probe.status == 0 else { return VideoValidation(status: "failed", tracks: [], diagnostics: String(decoding: probe.stderr, as: UTF8.self)) }
        let streams = try JSONDecoder().decode(Probe.self, from: probe.stdout).streams
        guard streams.count <= 16 else { return VideoValidation(status: "failed", tracks: [], diagnostics: "검사할 트랙이 너무 많습니다.") }
        let media = streams.filter { $0.codec_type == "video" || $0.codec_type == "audio" }
        guard media.contains(where: { $0.codec_type == "video" }) else { return VideoValidation(status: "failed", tracks: [], diagnostics: "검사 가능한 영상 트랙이 없습니다.") }
        var tracks: [VideoTrackValidation] = []
        var diagnostics = ""; var failed = false
        for stream in media {
            try Task.checkCancellation()
            if stream.codec_type == "video" {
                guard let width = stream.width, let height = stream.height, width > 0, height > 0,
                      width <= 16384, height <= 16384, width * height <= 80_000_000 else {
                    return VideoValidation(status: "failed", tracks: tracks, diagnostics: diagnostics + "영상 크기가 검증 한도를 벗어났습니다.")
                }
            } else {
                guard let channels = stream.channels, channels > 0, channels <= 64,
                      let rate = Int(stream.sample_rate ?? ""), rate > 0, rate <= 384000 else {
                    return VideoValidation(status: "failed", tracks: tracks, diagnostics: diagnostics + "음성 트랙 설정을 확인할 수 없습니다.")
                }
            }
            await receive(VideoUpdate("\(prefix): \(stream.codec_type ?? "") 트랙 \(stream.index) 전체 디코딩"))
            let hashes = try session.path("\(prefix)-track-\(stream.index).framehash")
            let result = try await ToolProcess.run(executable: tools.ffmpeg,
                arguments: ["-nostdin", "-v", "error", "-xerror", "-err_detect", "explode", "-threads", "2"] + movieInputOptions +
                    ["-i", file.path, "-map", "0:\(stream.index)", "-threads", "2", "-fps_mode", "passthrough", "-f", "framehash", "-hash", "sha256", "-n", hashes.path],
                timeout: 3600, outputLimit: 512 * 1024)
            try session.write(result.stderr, named: "\(prefix)-track-\(stream.index).log")
            // A decoder can reject a damaged stream before creating its hash file.
            // Preserve that per-file failure without hiding our own output I/O errors.
            guard result.status == 0 else {
                failed = true
                diagnostics += "track \(stream.index): exit \(result.status)\n" + String(decoding: result.stderr, as: UTF8.self)
                continue
            }
            let measured = try FrameHashes.read(hashes)
            let sampleRate = Double(stream.sample_rate ?? "") ?? 0
            let samples = stream.codec_type == "audio" ? Int64((measured.duration * sampleRate).rounded()) : 0
            tracks.append(VideoTrackValidation(index: stream.index, type: stream.codec_type ?? "unknown", codec: stream.codec_name ?? "unknown",
                decodedFrames: measured.count, decodedSamples: samples, declaredDuration: stream.duration,
                decodedStart: measured.start, decodedEnd: measured.end))
            if measured.count == 0 || !result.stderr.isEmpty {
                failed = true
                diagnostics += "track \(stream.index): exit \(result.status), frames \(measured.count)\n" + String(decoding: result.stderr, as: UTF8.self)
            }
        }
        return VideoValidation(status: failed ? "failed" : "decoded", tracks: tracks, diagnostics: diagnostics)
    }
}

struct FrameHashes {
    var count = 0
    var duration = 0.0
    var start: Double?
    var end: Double?
    static func read(_ url: URL) throws -> Self {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var pending = Data(); var result = Self(); var timebase: Double?
        func line(_ data: Data) throws {
            let text = String(decoding: data, as: UTF8.self)
            if text.hasPrefix("#tb 0:") {
                let values = text.dropFirst(6).trimmingCharacters(in: .whitespaces).split(separator: "/")
                if values.count == 2, let n = Double(values[0]), let d = Double(values[1]), n > 0, d > 0 { timebase = n / d }
            } else if !text.hasPrefix("#"), !text.isEmpty {
                let fields = text.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
                guard fields.count == 6, let tb = timebase, let pts = Double(fields[2]), let duration = Double(fields[3]),
                      pts.isFinite, duration.isFinite, duration >= 0 else { throw VideoError.failed("디코딩 증거의 프레임 행이 잘못되었습니다.") }
                let beginning = pts * tb; let ending = (pts + duration) * tb
                guard beginning.isFinite, ending.isFinite else { throw VideoError.failed("디코딩 시간값이 잘못되었습니다.") }
                result.count += 1; result.duration += duration * tb
                result.start = min(result.start ?? beginning, beginning)
                result.end = max(result.end ?? ending, ending)
            }
        }
        while let chunk = try handle.read(upToCount: 64 * 1024), !chunk.isEmpty {
            try Task.checkCancellation(); pending.append(chunk)
            while let newline = pending.firstIndex(of: 10) {
                try line(Data(pending[..<newline])); pending.removeSubrange(...newline)
            }
            guard pending.count <= 64 * 1024 else { throw VideoError.failed("디코딩 증거 행이 너무 깁니다.") }
        }
        if !pending.isEmpty { try line(pending) }
        guard result.duration.isFinite, result.duration < Double(Int64.max) / 384000 else { throw VideoError.failed("디코딩 길이가 한도를 벗어났습니다.") }
        return result
    }
}
