import Foundation
import CryptoKit
import RecoveryCore

func runVideoChecks() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: "Framend-검사-\(UUID())")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: root) }
    let source = root.appending(path: "input.mov")
    try Data([1, 2, 3]).write(to: source)
    let link = root.appending(path: "linked.mov")
    try FileManager.default.createSymbolicLink(at: link, withDestinationURL: source)
    try rejects { _ = try VideoInput(link) }
    let snapshot = try VideoInput(source)
    try Data([4, 5, 6, 7]).write(to: source)
    try rejects { try snapshot.unchanged() }
    print("PASS Video input rejects symlinks and detects source replacement")

    let first = try VideoSession(destination: root)
    let second = try VideoSession(destination: root)
    try check(first.directory != second.directory, "Sessions reused output directory")
    try first.writeJSON(["state": "test"], named: "report.json")
    try rejects { try first.writeJSON(["state": "overwrite"], named: "report.json") }
    try rejects { try first.writeJSON(["state": "escape"], named: "../escape.json") }
    try check(FileManager.default.fileExists(atPath: first.directory.appending(path: "INCOMPLETE.txt").path), "Session lacks incomplete marker")
    print("PASS Video sessions preserve existing outputs and reject path escape")
    let replaced = try VideoSession(destination: root)
    let moved = root.appending(path: "moved-session")
    try FileManager.default.moveItem(at: replaced.directory, to: moved)
    try FileManager.default.createDirectory(at: replaced.directory, withIntermediateDirectories: false)
    try rejects { try replaced.writeJSON(["unexpected": "write"], named: "wrong-directory.json") }
    print("PASS Video session detects replaced output directory")


    let failed = VideoValidation(status: "failed", tracks: [], diagnostics: "decode failed")
    try check(!failed.passed, "Failed validation became success")
    let empty = VideoValidation(status: "decoded", tracks: [], diagnostics: "")
    try check(!empty.passed, "Zero video frames became success")
    print("PASS Video validation cannot claim success for empty or failed output")
    try await runSegmentChecks(root)
    try await runVideoFailureChecks(root)
}

func runSegmentChecks(_ root: URL) async throws {
    let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    let segment: [String: Any] = ["id": 1, "output_offset": 0, "output_length": 3, "frames": 3,
        "extent_start": 0, "extent_count": 1, "sha256": hash, "end_reason": "range_end"]
    func report(_ segments: [[String: Any]], frames: UInt64 = 3) throws -> NativeSalvageReport {
        let value: [String: Any] = ["codec": "h264", "fps_num": 30, "fps_den": 1, "frames": frames,
            "extents": [["offset": 1, "length": 3]], "skipped_bytes": 2, "input_sha256": hash,
            "reference_sha256": hash, "warnings": [], "partial": true,
            "frame_count_basis": "structural_access_unit_starts", "timing_basis": "estimated_reference_average",
            "segments": segments.count, "segment_map": segments]
        return try JSONDecoder().decode(NativeSalvageReport.self, from: JSONSerialization.data(withJSONObject: value))
    }
    let valid = try report([segment])
    try valid.validateSegments(outputSize: 3, inputSize: 5)
    for (key, value) in [("output_offset", 1 as Any), ("output_length", 0), ("output_length", UInt64.max),
                         ("extent_start", 1), ("extent_count", 2), ("id", 2), ("frames", UInt64.max),
                         ("sha256", "../bad"), ("end_reason", "invented")] {
        var changed = segment; changed[key] = value
        let invalid = try report([changed])
        try rejects { try invalid.validateSegments(outputSize: 3, inputSize: 5) }
    }
    let duplicate = try report([segment, segment], frames: 6)
    try rejects { try duplicate.validateSegments(outputSize: 6, inputSize: 5) }
    try rejects { try valid.validateSegments(outputSize: 4, inputSize: 5) }
    try rejects { try valid.validateSegments(outputSize: 3, inputSize: 2) }
    print("PASS Segment map rejects gaps, overlap, overflow and invalid provenance")
    let inputURL = root.appending(path: "segments.es")
    try Data("abc".utf8).write(to: inputURL)
    let input = try VideoInput(inputURL)
    let session = try VideoSession(destination: root)
    let copied = try session.copySegment(from: input, segment: valid.segment_map[0], named: "copied.es")
    let copiedBytes = try Data(contentsOf: copied)
    try check(copiedBytes == Data("abc".utf8), "Segment bytes changed")
    try rejects { _ = try session.copySegment(from: input, segment: valid.segment_map[0], named: "copied.es") }
    var wrongHash = segment; wrongHash["sha256"] = String(repeating: "0", count: 64)
    let badHash = try report([wrongHash]).segment_map[0]
    try rejects { _ = try session.copySegment(from: input, segment: badHash, named: "bad-hash.es") }
    let cancelled = Task {
        while !Task.isCancelled { await Task.yield() }
        return try session.copySegment(from: input, segment: valid.segment_map[0], named: "cancelled.es")
    }
    cancelled.cancel()
    do { _ = try await cancelled.value; throw CheckFailure(description: "Cancelled copy succeeded") }
    catch is CancellationError { }
    try check(!FileManager.default.fileExists(atPath: session.directory.appending(path: "cancelled.es").path), "Cancelled copy created output")
    try Data("def".utf8).write(to: inputURL)
    try rejects { _ = try session.copySegment(from: input, segment: valid.segment_map[0], named: "changed.es") }
    let payload = Data((0..<(1_048_576 + 13)).map { UInt8($0 % 251) })
    let largeURL = root.appending(path: "large-segment.es")
    try (Data([9, 8, 7]) + payload + Data([6, 5])).write(to: largeURL)
    var subrange = segment
    subrange["output_offset"] = 3; subrange["output_length"] = payload.count
    subrange["sha256"] = SHA256.hash(data: payload).map { String(format: "%02x", $0) }.joined()
    let largeInput = try VideoInput(largeURL)
    let largeSegment = try report([subrange]).segment_map[0]
    let largeCopy = try session.copySegment(from: largeInput, segment: largeSegment, named: "large-copy.es")
    let copiedPayload = try Data(contentsOf: largeCopy)
    try check(copiedPayload == payload, "Nonzero-offset copy failed across buffer boundary")
    try rejects { _ = try session.copySegment(from: input, segment: largeSegment, named: "out-of-bounds.es") }
    print("PASS Segment copy checks hash, exclusive output, cancellation and source replacement")
}

func runVideoIntegration(_ directory: URL) async throws {
    for codec in ["h264", "hevc"] {
        let input = directory.appending(path: "\(codec)-missing-moov.mp4")
        let reference = directory.appending(path: "\(codec)-reference.mp4")
        let before = try VideoInput(input).sha256()
        let outcome = try await VideoRecovery.repair(input: input, reference: reference, destination: directory)
        guard let result = outcome.results.first else { throw CheckFailure(description: "No repair result") }
        try check(result.validation.passed, "Full decode failed: \(result.validation.diagnostics)")
        try check(result.validation.tracks.first?.decodedFrames == 30, "Expected 30 decoded frames")
        try check(result.label.contains("부분 복구"), "Native salvage mislabeled complete")
        try check(!FileManager.default.fileExists(atPath: outcome.directory.appending(path: "INCOMPLETE.txt").path), "Successful operation still incomplete")
        try check(FileManager.default.fileExists(atPath: outcome.directory.appending(path: "native-report.json").path), "Missing provenance")
        let after = try VideoInput(input).sha256()
        try check(before == after, "Input changed")
        print("PASS Swift \(codec) native recovery, mux, full decode, evidence and partial status")
        let reordered = try await VideoRecovery.repair(input: directory.appending(path: "\(codec)-bframes.mp4"),
            reference: directory.appending(path: "\(codec)-bframes-original.mp4"), destination: directory)
        try check(reordered.results.first?.validation.passed == false && reordered.results.first?.validation.diagnostics.contains("Non-monotonic DTS") == true,
            "Unresolved B-frame timing was incorrectly accepted")
        try check(FileManager.default.fileExists(atPath: reordered.directory.appending(path: "INCOMPLETE.txt").path), "B-frame timing failure lost incomplete marker")
        print("PASS \(codec) unresolved B-frame timing is reported as failed, with raw data retained")
        do {
            _ = try await VideoRecovery.repair(input: input, reference: input, destination: directory)
            throw CheckFailure(description: "Same input/reference accepted")
        } catch is VideoError { }
        let tamper = SegmentTamper(replacement: try Data(contentsOf: directory.appending(path: "\(codec)-mixed-segment-2.es")))
        do {
            _ = try await VideoRecovery.repair(input: input, reference: reference, destination: directory) { update in
                await tamper.receive(update)
            }
            throw CheckFailure(description: "Replaced intermediate segment was accepted as original evidence")
        } catch is VideoError { }
        let replaced = await tamper.didReplace
        try check(replaced, "Tamper test did not reach replacement window")
        print("PASS \(codec) replaced intermediate segment is rejected")
        let outputTamper = SegmentTamper(replacement: try Data(contentsOf: reference),
            stage: "segment-0001: video", filename: "recovered-video.mp4")
        do {
            _ = try await VideoRecovery.repair(input: input, reference: reference, destination: directory) { update in
                await outputTamper.receive(update)
            }
            throw CheckFailure(description: "Replaced output video was accepted with original segment provenance")
        } catch is VideoError { }
        let outputReplaced = await outputTamper.didReplace
        try check(outputReplaced, "Output tamper did not reach validation window")
        print("PASS \(codec) replaced output video is rejected")
        for caseName in ["mixed", "isolated-damage", "gap-truncated"] {
            let mixed = directory.appending(path: "\(codec)-\(caseName).mp4")
            let beforeMixed = try VideoInput(mixed).sha256()
            let separated = try await VideoRecovery.repair(input: mixed, reference: reference, destination: directory)
            try check(separated.results.count == 2, "Disconnected video runs were joined")
            let second = separated.results[1]
            try check(second.validation.passed && second.validation.tracks.first?.decodedFrames == 30, "Later valid run was lost")
            if caseName == "mixed" { try check(separated.results[0].validation.passed, "First valid run failed") }
            if caseName == "isolated-damage" {
                try check(!separated.results[0].validation.passed, "Corrupt run was marked successful")
                try check(FileManager.default.fileExists(atPath: separated.directory.appending(path: "INCOMPLETE.txt").path), "Failed run lost incomplete marker")
            }
            for index in 1...2 {
                let prefix = String(format: "segment-%04d", index)
                try check(FileManager.default.fileExists(atPath: separated.directory.appending(path: "\(prefix)-result.json").path), "Segment evidence missing")
            }
            let afterMixed = try VideoInput(mixed).sha256()
            try check(beforeMixed == afterMixed, "Mixed source changed")
            print("PASS Swift \(codec) \(caseName): two independent runs, later 30 frames retained")
            print("SEGMENT_OUTPUT=\(separated.directory.path)")
        }
    }
}

private actor SegmentTamper {
    let replacement: Data
    let stage: String
    let filename: String
    var directory: URL?
    var didReplace = false
    init(replacement: Data, stage: String = "별도 영상", filename: String = "segment-0001.es") {
        self.replacement = replacement; self.stage = stage; self.filename = filename
    }
    func receive(_ update: VideoUpdate) {
        if let directory = update.directory { self.directory = directory }
        guard !didReplace, update.stage.contains(stage), let directory else { return }
        do {
            let part = directory.appending(path: filename)
            try FileManager.default.moveItem(at: part, to: directory.appending(path: "original-" + filename))
            try replacement.write(to: part)
            didReplace = true
        } catch { print("Tamper setup failed: \(error)") }
    }
}

func runRealExfatIntegration(image: URL, destination: URL) async throws {
    let scan = try await VideoRecovery.scan(image: image)
    let candidates = scan.candidates.filter(\.recoverable)
    try check(candidates.count == 2, "Expected two known X-T5 videos in this verification image")
    let outcome = try await VideoRecovery.extract(image: image, scan: scan, candidates: candidates, destination: destination) { update in
        if update.fraction == nil { print(update.stage) }
    }
    try check(outcome.results.count == 2 && outcome.results.allSatisfy { $0.validation.passed }, "Real X-T5 decode failed")
    let frames = outcome.results.flatMap(\.validation.tracks).filter { $0.type == "video" }.map(\.decodedFrames).sorted()
    try check(frames == [480, 960], "Real X-T5 frame counts changed")
    let audio = outcome.results.flatMap(\.validation.tracks).filter { $0.type == "audio" }
    try check(audio.count == 1 && audio[0].decodedSamples > 0, "Real X-T5 audio did not decode")
    print("PASS real exFAT extraction: 960 + 480 video frames and audio decoded")
    print("REAL_EXFAT_OUTPUT=\(outcome.directory.path)")
}
