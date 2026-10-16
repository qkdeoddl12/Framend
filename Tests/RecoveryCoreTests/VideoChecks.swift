import Foundation
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
        do {
            _ = try await VideoRecovery.repair(input: input, reference: input, destination: directory)
            throw CheckFailure(description: "Same input/reference accepted")
        } catch is VideoError { }
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
