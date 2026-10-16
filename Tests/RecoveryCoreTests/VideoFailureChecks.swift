import Foundation
@testable import RecoveryCore

func runVideoFailureChecks(_ root: URL) async throws {
    func executable(_ name: String, _ body: String) throws -> URL {
        let file = root.appending(path: name)
        try Data(("#!/bin/sh\n" + body + "\n").utf8).write(to: file)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: file.path)
        return file
    }
    let decoder = try executable("failed-decoder", "echo 'bad compressed picture' >&2\nexit 1")
    let probe = try executable("fixture-probe", #"printf '%s' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":64,"height":64}]}'"#)
    let tools = VideoTools(engine: decoder, ffmpeg: decoder, ffprobe: probe)
    let session = try VideoSession(destination: root)
    let input = root.appending(path: "unused-fixture.mov")
    let result = try await VideoValidator.validate(input, tools: tools, session: session, prefix: "missing-hashes", receive: { _ in })
    try check(!result.passed && result.diagnostics.contains("bad compressed picture"), "Early decoder failure lost diagnostics")
    try check(!FileManager.default.fileExists(atPath: session.directory.appending(path: "missing-hashes-track-0.framehash").path), "Fixture unexpectedly wrote hashes")
    print("PASS Decoder exit before framehash creation is a failed result, not a session abort")

    let oversizedProbe = try executable("oversized-probe", #"printf '%s' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":20000,"height":64}]}'"#)
    let rejected = try await VideoValidator.validate(input, tools: VideoTools(engine: decoder, ffmpeg: decoder, ffprobe: oversizedProbe),
        session: session, prefix: "invalid-media", receive: { _ in })
    try check(!rejected.passed, "Unsupported dimensions passed validation")
    print("PASS Unsupported media settings remain an individual failed validation")

    let moved = root.appending(path: "moved-failure-session")
    try FileManager.default.moveItem(at: session.directory, to: moved)
    do {
        _ = try await VideoValidator.validate(input, tools: tools, session: session, prefix: "missing-directory", receive: { _ in })
        throw CheckFailure(description: "Output I/O failure was swallowed")
    } catch is VideoError { }
    print("PASS Output session failure still aborts validation")
}
