import Foundation
import RecoveryCore

struct CheckFailure: Error, CustomStringConvertible { let description: String }
func check(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw CheckFailure(description: message) }
}
func rejects(_ body: () throws -> Void) throws {
    do { try body() } catch { return }
    throw CheckFailure(description: "Expected an error")
}
func event(_ json: String) throws -> EngineEvent {
    try JSONDecoder().decode(EngineEvent.self, from: Data(json.utf8))
}

let checks: [(String, () throws -> Void)] = [
    ("RAF header never implies RAW decoding", {
        let value = try event(#"{"event":"candidate","candidate":{"offset":1024,"length":100000,"format":"raf","camera":"X-T5"}}"#)
        try check(value.candidate?.statusLabel == "구조 확인 · RAW 미검증", "RAF status overclaims validation")
    }),
    ("Missing completion fails", {
        var validator = StreamValidator()
        try validator.accept(event(#"{"event":"progress","scanned":5,"total":10}"#))
        try rejects { try validator.finish() }
    }),
    ("Malformed candidate fails", {
        var validator = StreamValidator()
        try rejects { try validator.accept(event(#"{"event":"candidate"}"#)) }
    }),
    ("Count mismatch fails", {
        var validator = StreamValidator()
        try rejects { try validator.accept(event(#"{"event":"complete","count":2,"directory":null}"#)) }
    }),
    ("Empty scan completes", {
        var validator = StreamValidator()
        try validator.accept(event(#"{"event":"complete","count":0,"directory":null}"#))
        try validator.finish()
    }),
    ("Missing JPEG cannot pass validation", {
        try check(MediaInspector.validate(url: URL(fileURLWithPath: "/nonexistent/recovery-card-test.jpg"), format: .jpeg) == .failed, "Invalid JPEG was not rejected")
    }),
    ("RAW remains unverified", {
        try check(MediaInspector.validate(url: URL(fileURLWithPath: "/nonexistent/test.raf"), format: .raf) == .rawUnverified, "RAW must not claim decoder support")
    }),
    ("Event after completion fails", {
        var validator = StreamValidator()
        try validator.accept(event(#"{"event":"complete","count":0,"directory":null}"#))
        try rejects { try validator.accept(event(#"{"event":"progress","scanned":1,"total":1}"#)) }
    })
]

var failed = 0
for (name, run) in checks {
    do { try run(); print("PASS \(name)") }
    catch { failed += 1; print("FAIL \(name): \(error)") }
}
print("\(checks.count-failed)/\(checks.count) checks passed")
if failed > 0 { exit(1) }

actor CollectedEvents {
    var values: [EngineEvent] = []
    func append(_ event: EngineEvent) { values.append(event) }
}

if CommandLine.arguments.contains("--integration") {
    do {
        let root = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
        let fixtures = root.appending(path: "fixtures/generated")
        let original = fixtures.appending(path: "original.jpg")
        try check(MediaInspector.validate(url: original, format: .jpeg) == .jpegDecoded, "Real JPEG did not decode")
        let engine = try EngineClient.locateEngine()
        let collected = CollectedEvents()
        try await EngineClient.run(executable: engine, arguments: ["scan", fixtures.appending(path: "demo-card.img").path]) {
            await collected.append($0)
        }
        let results = await collected.values
        let photos = results.compactMap(\.candidate)
        try check(photos.count == 2 && results.last?.event == .complete, "Swift bridge lost scan events")
        guard let source = results.first?.source else { throw CheckFailure(description: "No source fingerprint") }
        let temporary = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let exported = CollectedEvents()
        let request = try JSONEncoder().encode(RecoveryRequest(source: source, candidates: photos))
        try await EngineClient.run(executable: engine, arguments: ["recover", fixtures.appending(path: "demo-card.img").path, temporary.path], input: request) {
            await exported.append($0)
        }
        let saved = await exported.values.filter { $0.event == .saved }
        try check(saved.count == 2, "Swift bridge did not recover two files")
        for item in saved {
            guard let path = item.path, let candidate = item.candidate else { throw CheckFailure(description: "Missing export data") }
            let expected = fixtures.appending(path: candidate.format == .jpeg ? "original.jpg" : "synthetic.raf")
            let actualBytes = try Data(contentsOf: URL(fileURLWithPath: path))
            let expectedBytes = try Data(contentsOf: expected)
            try check(actualBytes == expectedBytes, "Swift round-trip changed file bytes")
        }
        print("PASS Swift → Rust scan/export round-trip + real JPEG decode")

        do {
            try await EngineClient.run(executable: URL(fileURLWithPath: "/usr/bin/true"), arguments: []) { _ in }
            throw CheckFailure(description: "Missing completion was accepted by process bridge")
        } catch is EngineError { print("PASS Process without completion is rejected") }

        let clock = ContinuousClock()
        let started = clock.now
        let task = Task {
            try await EngineClient.run(executable: URL(fileURLWithPath: "/bin/sleep"), arguments: ["30"]) { _ in }
        }
        try await Task.sleep(for: .milliseconds(100))
        task.cancel()
        do { try await task.value; throw CheckFailure(description: "Cancellation was accepted as success") }
        catch is CancellationError { }
        try check(started.duration(to: clock.now) < .seconds(5), "Process cancellation was too slow")
        print("PASS Running child terminates promptly on cancellation")
    } catch {
        print("FAIL Integration: \(error)")
        exit(1)
    }
}

if CommandLine.arguments.contains("--broken-pipe") {
    do {
        try await EngineClient.run(executable: URL(fileURLWithPath: "/usr/bin/false"), arguments: [], input: Data(repeating: 0, count: 5*1024*1024)) { _ in }
        print("FAIL Early child exit was accepted"); exit(1)
    } catch { print("PASS Early exit during large stdin is a caught error") }
}

if CommandLine.arguments.contains("--bad-json") {
    let clock = ContinuousClock()
    let started = clock.now
    do {
        try await EngineClient.run(executable: URL(fileURLWithPath: "/usr/bin/python3"), arguments: ["-c", "import time; print('invalid json', flush=True); time.sleep(3)"]) { _ in }
        print("FAIL Malformed protocol was accepted"); exit(1)
    } catch {
        guard started.duration(to: clock.now) < .seconds(2) else {
            print("FAIL Protocol error waited for child instead of terminating it"); exit(1)
        }
        print("PASS Malformed protocol terminates child promptly")
    }
}
