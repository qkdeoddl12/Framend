import Darwin
import Foundation
import RecoveryCore

private struct VideoProcessCheckFailure: Error, CustomStringConvertible {
    let description: String
}

private func requireVideoProcess(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw VideoProcessCheckFailure(description: message) }
}

private actor ProcessLines {
    var values: [String] = []
    func append(_ line: String) { values.append(line) }
}

private enum ExpectedProcessFailure {
    case timeout, stdoutLimit, stderrLimit, missingExecutable
}

private func expectProcessFailure(_ expected: ExpectedProcessFailure, _ body: () async throws -> ToolResult) async throws {
    do { _ = try await body() }
    catch let error as ToolProcessError {
        switch (expected, error) {
        case (.timeout, .timedOut),
             (.stdoutLimit, .outputLimit(stream: "stdout", bytes: _)),
             (.stderrLimit, .outputLimit(stream: "stderr", bytes: _)),
             (.missingExecutable, .system(operation: _, code: ENOENT)): return
        default: throw VideoProcessCheckFailure(description: "Unexpected process failure: \(error)")
        }
    }
    throw VideoProcessCheckFailure(description: "Expected the runner to throw")
}

private func fixturePID(_ file: URL) async throws -> pid_t {
    for _ in 0..<200 {
        if let text = try? String(contentsOf: file, encoding: .utf8),
           let pid = Int32(text.trimmingCharacters(in: .whitespacesAndNewlines)) { return pid }
        try await Task.sleep(for: .milliseconds(10))
    }
    throw VideoProcessCheckFailure(description: "Child did not publish its PID")
}

private func requireProcessGone(_ pid: pid_t) async throws {
    for _ in 0..<300 {
        if kill(pid, 0) == -1 && errno == ESRCH { return }
        try await Task.sleep(for: .milliseconds(10))
    }
    throw VideoProcessCheckFailure(description: "Descendant \(pid) survived process cleanup")
}

public func runVideoProcessChecks() async throws {
    let python = URL(fileURLWithPath: "/usr/bin/python3")
    try requireVideoProcess(FileManager.default.isExecutableFile(atPath: python.path), "Python fixture runtime is missing")
    let temporary = FileManager.default.temporaryDirectory.appending(path: "video process 한글 \(UUID().uuidString)")
    try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: temporary) }

    let lines = ProcessLines()
    let output = try await ToolProcess.run(executable: python, arguments: ["-c", "import os; os.write(1,b'first\\nsecond'); os.write(2,b'diagnostic\\n')"]) {
        await lines.append($0)
    }
    try requireVideoProcess(output.status == 0, "Successful child returned a failure status")
    try requireVideoProcess(output.stdout == Data("first\nsecond".utf8), "stdout changed")
    try requireVideoProcess(output.stderr == Data("diagnostic\n".utf8), "stderr changed")
    let received = await lines.values
    try requireVideoProcess(received == ["first", "second"], "Progress did not preserve complete lines and trailing text")
    print("PASS ToolProcess captures stdout/stderr and complete progress lines")

    let largeLines = ProcessLines()
    let largeJSON = try await ToolProcess.run(executable: python, arguments: ["-c", "import os; os.write(1,b'x'*98304)"], outputLimit: 128 * 1024) {
        await largeLines.append($0)
    }
    let completeLines = await largeLines.values
    try requireVideoProcess(largeJSON.stdout.count == 98_304 && completeLines.count == 1 && completeLines[0].utf8.count == 98_304,
                            "A complete line within the configured stdout limit was rejected or split")
    print("PASS ToolProcess permits complete long lines within the configured output limit")

    let bad = try await ToolProcess.run(executable: python, arguments: ["-c", "import sys; sys.stderr.write('bad input'); sys.exit(7)"])
    try requireVideoProcess(bad.status == 7 && bad.stderr == Data("bad input".utf8), "Nonzero exit did not return status and diagnostic")
    let early = try await ToolProcess.run(executable: python, arguments: ["-c", "import os; os.close(0); os._exit(9)"], input: Data(repeating: 42, count: 8 * 1024 * 1024))
    try requireVideoProcess(early.status == 9, "Large input/early exit lost the child status")
    let echoed = try await ToolProcess.run(executable: python, arguments: ["-c", "import sys; data=sys.stdin.buffer.read(); print(len(data))"], input: Data(repeating: 42, count: 2 * 1024 * 1024))
    try requireVideoProcess(echoed.stdout == Data("2097152\n".utf8), "Nonblocking stdin dropped input bytes")
    print("PASS ToolProcess returns bad exits and safely handles large stdin")

    let clock = ContinuousClock()
    var started = clock.now
    try await expectProcessFailure(.timeout) {
        try await ToolProcess.run(executable: python, arguments: ["-c", "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(30)"], timeout: 0.15)
    }
    try requireVideoProcess(started.duration(to: clock.now) < .seconds(3), "Timeout did not bound a SIGTERM-resistant child")
    print("PASS ToolProcess timeout escalates to SIGKILL promptly")

    let childPIDFile = temporary.appending(path: "cancel-child.pid")
    let forkScript = """
    import os,signal,sys,time
    pid=os.fork()
    if pid==0:
        signal.signal(signal.SIGTERM,signal.SIG_IGN)
        with open(sys.argv[1],'w') as f: f.write(str(os.getpid()))
        time.sleep(30)
    else:
        time.sleep(30)
    """
    let cancellation = Task {
        try await ToolProcess.run(executable: python, arguments: ["-c", forkScript, childPIDFile.path])
    }
    let descendant = try await fixturePID(childPIDFile)
    var descendantNeedsCleanup = true
    defer { if descendantNeedsCleanup { _ = kill(descendant, SIGKILL) } }
    started = clock.now
    cancellation.cancel()
    do { _ = try await cancellation.value; throw VideoProcessCheckFailure(description: "Cancellation returned success") }
    catch is CancellationError { }
    try await requireProcessGone(descendant)
    descendantNeedsCleanup = false
    try requireVideoProcess(started.duration(to: clock.now) < .seconds(3), "Cancellation cleanup was too slow")
    print("PASS ToolProcess cancellation kills forked descendants")

    let orphanPIDFile = temporary.appending(path: "orphan-child.pid")
    let orphanScript = """
    import os,signal,sys,time
    pid=os.fork()
    if pid==0:
        signal.signal(signal.SIGTERM,signal.SIG_IGN)
        with open(sys.argv[1],'w') as f: f.write(str(os.getpid()))
        time.sleep(30)
    else:
        while not os.path.exists(sys.argv[1]): time.sleep(0.005)
        os._exit(0)
    """
    started = clock.now
    let orphan = try await ToolProcess.run(executable: python, arguments: ["-c", orphanScript, orphanPIDFile.path], timeout: 3)
    let orphanPID = try await fixturePID(orphanPIDFile)
    var orphanNeedsCleanup = true
    defer { if orphanNeedsCleanup { _ = kill(orphanPID, SIGKILL) } }
    try requireVideoProcess(orphan.status == 0, "Exited parent status changed during descendant cleanup")
    try await requireProcessGone(orphanPID)
    orphanNeedsCleanup = false
    try requireVideoProcess(started.duration(to: clock.now) < .seconds(3), "Descendant-held pipes blocked completion")
    print("PASS ToolProcess reaps parent and terminates pipe-holding descendants")

    started = clock.now
    try await expectProcessFailure(.stdoutLimit) {
        try await ToolProcess.run(executable: python, arguments: ["-c", "import os,time; os.write(1,b'x'*131072); time.sleep(30)"], outputLimit: 4096)
    }
    try requireVideoProcess(started.duration(to: clock.now) < .seconds(3), "Newline-free output bypassed the byte limit")
    try await expectProcessFailure(.stderrLimit) {
        try await ToolProcess.run(executable: python, arguments: ["-c", "import os; os.write(2,b'x'*1048576)"])
    }
    print("PASS ToolProcess bounds stdout without newlines and stderr")

    // Apple's /usr/bin/python3 launcher locates its runtime using the executable basename.
    let link = temporary.appending(path: "python3")
    try FileManager.default.createSymbolicLink(at: link, withDestinationURL: python)
    try Data("working directory marker".utf8).write(to: temporary.appending(path: "marker.txt"))
    let path = try await ToolProcess.run(executable: link, arguments: ["-c", "import sys; print(open('marker.txt').read()); print(sys.argv[1])", "한글 argument ; $literal"], directory: temporary)
    try requireVideoProcess(path.status == 0, "Path fixture failed (\(path.status)): \(String(decoding: path.stderr, as: UTF8.self))")
    let actual = String(decoding: path.stdout, as: UTF8.self).split(separator: "\n").map(String.init)
    try requireVideoProcess(actual == ["working directory marker", "한글 argument ; $literal"], "Unicode/spaces working directory or literal arguments were changed")
    try await expectProcessFailure(.missingExecutable) {
        try await ToolProcess.run(executable: temporary.appending(path: "missing tool"), arguments: [])
    }
    print("PASS ToolProcess preserves paths/arguments and rejects a missing executable")
}
