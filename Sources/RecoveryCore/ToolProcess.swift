import Darwin
import Foundation

public struct ToolResult: Sendable {
    public let status: Int32
    public let stdout: Data
    public let stderr: Data
}

public enum ToolProcessError: Error, LocalizedError, Sendable {
    case invalidConfiguration
    case timedOut
    case outputLimit(stream: String, bytes: Int)
    case system(operation: String, code: Int32)
    case pipesDidNotClose
    case terminationFailed

    public var errorDescription: String? {
        switch self {
        case .invalidConfiguration: "Invalid process path, timeout, or output limit."
        case .timedOut: "The video tool exceeded its time limit."
        case let .outputLimit(stream, bytes): "The video tool exceeded the \(stream) limit (\(bytes) bytes)."
        case let .system(operation, code): "\(operation): \(String(cString: strerror(code)))"
        case .pipesDidNotClose: "The video tool left an output pipe open after termination."
        case .terminationFailed: "The video tool did not terminate after SIGKILL."
        }
    }
}

private final class ProcessCancellation: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    var isCancelled: Bool { lock.withLock { value } }
    func cancel() { lock.withLock { value = true } }
}

public enum ToolProcess {
    public static func run(executable: URL, arguments: [String], input: Data? = nil,
                           directory: URL? = nil, timeout: TimeInterval = 3600,
                           outputLimit: Int = 16 * 1024 * 1024,
                           receive: @escaping @Sendable (String) async -> Void = { _ in }) async throws -> ToolResult {
        try Task.checkCancellation()
        guard executable.isFileURL, directory?.isFileURL != false,
              timeout.isFinite, timeout > 0, outputLimit > 0,
              !([executable.path] + arguments + [directory?.path ?? ""]).contains(where: { $0.utf8.contains(0) }) else {
            throw ToolProcessError.invalidConfiguration
        }
        let cancellation = ProcessCancellation()
        let progress = AsyncStream<String>.makeStream()
        // Blocking syscalls stay on a dispatch worker, including when called by the UI actor.
        let worker = Task.detached {
            try await withCheckedThrowingContinuation { (completion: CheckedContinuation<ToolResult, any Error>) in
                DispatchQueue.global(qos: .userInitiated).async {
                    let result = Result {
                        try execute(executable: executable, arguments: arguments, input: input,
                                    directory: directory, timeout: timeout, outputLimit: outputLimit,
                                    cancellation: cancellation, progress: progress.continuation)
                    }
                    progress.continuation.finish()
                    completion.resume(with: result)
                }
            }
        }
        return try await withTaskCancellationHandler {
            for await line in progress.stream { await receive(line) }
            // The stream ends immediately on cancellation; still await cleanup and child reaping.
            let result = try await worker.value
            try Task.checkCancellation()
            return result
        } onCancel: {
            cancellation.cancel()
        }
    }

    private static func execute(executable: URL, arguments: [String], input: Data?,
                                directory: URL?, timeout: TimeInterval, outputLimit: Int,
                                cancellation: ProcessCancellation,
                                progress: AsyncStream<String>.Continuation) throws -> ToolResult {
        if cancellation.isCancelled { throw CancellationError() }
        var descriptors: [Int32] = []
        defer { for fd in descriptors where fd >= 0 { close(fd) } }
        for _ in 0..<3 {
            var pair: [Int32] = [-1, -1]
            guard pipe(&pair) == 0 else { throw systemError("pipe") }
            descriptors.append(contentsOf: pair)
        }
        // Keep pipe descriptors away from stdio so spawn's dup/close actions cannot alias.
        for index in descriptors.indices {
            if descriptors[index] < 3 {
                let replacement = fcntl(descriptors[index], F_DUPFD_CLOEXEC, 3)
                guard replacement >= 0 else { throw systemError("duplicate pipe") }
                close(descriptors[index])
                descriptors[index] = replacement
            }
            guard fcntl(descriptors[index], F_SETFD, FD_CLOEXEC) != -1,
                  fcntl(descriptors[index], F_SETNOSIGPIPE, 1) != -1 else { throw systemError("configure pipe") }
        }
        for index in [1, 2, 4] {
            let flags = fcntl(descriptors[index], F_GETFL)
            guard flags >= 0, fcntl(descriptors[index], F_SETFL, flags | O_NONBLOCK) != -1 else {
                throw systemError("nonblocking pipe")
            }
        }
        let pid = try spawn(executable: executable, arguments: arguments, directory: directory, pipes: descriptors)
        var cleanupPerformed = false
        defer { if !cleanupPerformed { _ = try? terminateAndReap(pid) } }
        for index in [0, 3, 5] { close(descriptors[index]); descriptors[index] = -1 }

        var stdout = Data()
        var stderr = Data()
        var pendingLine = Data()
        var inputOffset = 0
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        func closeInput() {
            if descriptors[1] >= 0 { close(descriptors[1]); descriptors[1] = -1 }
        }
        if input?.isEmpty != false { closeInput() }
        while true {
            if cancellation.isCancelled { throw CancellationError() }
            if ProcessInfo.processInfo.systemUptime >= deadline { throw ToolProcessError.timedOut }
            try drain(fd: &descriptors[2], bytes: &stdout, limit: outputLimit, stream: "stdout",
                      pendingLine: &pendingLine, progress: progress)
            var unusedLine = Data()
            try drain(fd: &descriptors[4], bytes: &stderr, limit: 512 * 1024, stream: "stderr",
                      pendingLine: &unusedLine, progress: nil)
            if let input, descriptors[1] >= 0 {
                let written = input.withUnsafeBytes { raw in
                    Darwin.write(descriptors[1], raw.baseAddress!.advanced(by: inputOffset), min(16 * 1024, input.count - inputOffset))
                }
                if written > 0 {
                    inputOffset += written
                    if inputOffset == input.count { closeInput() }
                } else if written < 0 && errno == EPIPE {
                    closeInput()
                } else if written < 0 && errno != EAGAIN && errno != EINTR {
                    throw systemError("write stdin")
                }
            }
            if try hasExited(pid) { break }
            try pollPipes(descriptors)
        }
        closeInput()
        // Keep the direct child waitable until the group is killed, preventing PID reuse races.
        cleanupPerformed = true
        let status = try terminateAndReap(pid)
        let drainDeadline = ProcessInfo.processInfo.systemUptime + 0.25
        while descriptors[2] >= 0 || descriptors[4] >= 0 {
            if cancellation.isCancelled { throw CancellationError() }
            try drain(fd: &descriptors[2], bytes: &stdout, limit: outputLimit, stream: "stdout",
                      pendingLine: &pendingLine, progress: progress)
            var unusedLine = Data()
            try drain(fd: &descriptors[4], bytes: &stderr, limit: 512 * 1024, stream: "stderr",
                      pendingLine: &unusedLine, progress: nil)
            if descriptors[2] < 0 && descriptors[4] < 0 { break }
            guard ProcessInfo.processInfo.systemUptime < drainDeadline else { throw ToolProcessError.pipesDidNotClose }
            try pollPipes(descriptors)
        }
        if !pendingLine.isEmpty { progress.yield(String(decoding: pendingLine, as: UTF8.self)) }
        return ToolResult(status: status, stdout: stdout, stderr: stderr)
    }

    private static func spawn(executable: URL, arguments: [String], directory: URL?, pipes: [Int32]) throws -> pid_t {
        var actions: posix_spawn_file_actions_t?
        try check(posix_spawn_file_actions_init(&actions), "initialize spawn actions")
        defer { posix_spawn_file_actions_destroy(&actions) }
        var attributes: posix_spawnattr_t?
        try check(posix_spawnattr_init(&attributes), "initialize spawn attributes")
        defer { posix_spawnattr_destroy(&attributes) }
        try check(posix_spawnattr_setpgroup(&attributes, 0), "set child process group")
        var signals = sigset_t()
        sigemptyset(&signals)
        try check(posix_spawnattr_setsigmask(&attributes, &signals), "set child signal mask")
        sigaddset(&signals, SIGPIPE)
        sigaddset(&signals, SIGTERM)
        sigaddset(&signals, SIGINT)
        try check(posix_spawnattr_setsigdefault(&attributes, &signals), "reset child signal handlers")
        let flags = POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_CLOEXEC_DEFAULT
        try check(posix_spawnattr_setflags(&attributes, Int16(flags)), "set spawn flags")
        for (source, destination) in [(pipes[0], STDIN_FILENO), (pipes[3], STDOUT_FILENO), (pipes[5], STDERR_FILENO)] {
            try check(posix_spawn_file_actions_adddup2(&actions, source, destination), "redirect child pipe")
        }
        for fd in pipes { try check(posix_spawn_file_actions_addclose(&actions, fd), "close child pipe") }
        if let directory {
            let code = directory.path.withCString { path in
                posix_spawn_file_actions_addchdir_np(&actions, path)
            }
            try check(code, "set working directory")
        }
        let argv = ([executable.path] + arguments).map { strdup($0) } + [nil]
        let environment = ProcessInfo.processInfo.environment.map { strdup("\($0.key)=\($0.value)") } + [nil]
        defer {
            for pointer in argv { free(pointer) }
            for pointer in environment { free(pointer) }
        }
        guard argv.dropLast().allSatisfy({ $0 != nil }), environment.dropLast().allSatisfy({ $0 != nil }) else {
            throw ToolProcessError.system(operation: "allocate process arguments", code: ENOMEM)
        }
        var pid: pid_t = 0
        let code = argv.withUnsafeBufferPointer { arguments in
            environment.withUnsafeBufferPointer { environment in
                executable.path.withCString { path in
                    posix_spawn(&pid, path, &actions, &attributes,
                                UnsafeMutablePointer(mutating: arguments.baseAddress!),
                                UnsafeMutablePointer(mutating: environment.baseAddress!))
                }
            }
        }
        try check(code, "launch \(executable.lastPathComponent)")
        return pid
    }

    private static func drain(fd: inout Int32, bytes: inout Data, limit: Int, stream: String,
                              pendingLine: inout Data, progress: AsyncStream<String>.Continuation?) throws {
        guard fd >= 0 else { return }
        var buffer = [UInt8](repeating: 0, count: 8192)
        // Bound each turn so a continuously writing child cannot starve cancellation or stderr.
        for _ in 0..<8 {
            let count = Darwin.read(fd, &buffer, buffer.count)
            if count == 0 { close(fd); fd = -1; return }
            if count < 0 {
                if errno == EAGAIN || errno == EINTR { return }
                throw systemError("read \(stream)")
            }
            guard count <= limit - bytes.count else { throw ToolProcessError.outputLimit(stream: stream, bytes: limit) }
            bytes.append(contentsOf: buffer.prefix(count))
            if let progress {
                // The aggregate stdout limit also bounds each complete line and queued progress.
                pendingLine.append(contentsOf: buffer.prefix(count))
                while let newline = pendingLine.firstIndex(of: 10) {
                    var line = pendingLine[..<newline]
                    if line.last == 13 { line = line.dropLast() }
                    progress.yield(String(decoding: line, as: UTF8.self))
                    pendingLine.removeSubrange(...newline)
                }
            }
        }
    }

    private static func pollPipes(_ descriptors: [Int32]) throws {
        var polling = [pollfd(fd: descriptors[1], events: Int16(POLLOUT), revents: 0),
                       pollfd(fd: descriptors[2], events: Int16(POLLIN), revents: 0),
                       pollfd(fd: descriptors[4], events: Int16(POLLIN), revents: 0)]
        let result = poll(&polling, nfds_t(polling.count), 20)
        if result < 0 && errno != EINTR { throw systemError("poll child pipes") }
    }

    private static func hasExited(_ pid: pid_t) throws -> Bool {
        var information = siginfo_t()
        if waitid(P_PID, id_t(pid), &information, WEXITED | WNOHANG | WNOWAIT) == -1 {
            if errno == EINTR { return false }
            throw systemError("observe child exit")
        }
        return information.si_pid == pid
    }

    private static func terminateAndReap(_ pid: pid_t) throws -> Int32 {
        kill(-pid, SIGTERM)
        let grace = ProcessInfo.processInfo.systemUptime + 0.2
        while ProcessInfo.processInfo.systemUptime < grace { usleep(10_000) }
        kill(-pid, SIGKILL)
        let deadline = ProcessInfo.processInfo.systemUptime + 2
        while ProcessInfo.processInfo.systemUptime < deadline {
            var status: Int32 = 0
            let result = waitpid(pid, &status, WNOHANG)
            if result == pid { return status & 0x7f == 0 ? (status >> 8) & 0xff : 128 + (status & 0x7f) }
            if result == -1 && errno != EINTR { throw systemError("reap child") }
            usleep(10_000)
        }
        // A kernel-stalled child can outlast SIGKILL; reap its eventual exit without blocking a thread.
        let reaper = DispatchSource.makeProcessSource(identifier: pid, eventMask: .exit, queue: .global(qos: .utility))
        reaper.setEventHandler {
            var status: Int32 = 0
            _ = waitpid(pid, &status, WNOHANG)
            reaper.setEventHandler(handler: nil)
            reaper.cancel()
        }
        reaper.resume()
        throw ToolProcessError.terminationFailed
    }

    private static func systemError(_ operation: String) -> ToolProcessError {
        .system(operation: operation, code: errno)
    }

    private static func check(_ code: Int32, _ operation: String) throws {
        if code != 0 { throw ToolProcessError.system(operation: operation, code: code) }
    }
}
