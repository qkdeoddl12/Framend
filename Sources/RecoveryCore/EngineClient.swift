import Foundation
import Darwin

public enum EngineClient {
    public static func locateEngine() throws -> URL {
        let manager = FileManager.default
        let candidates = [
            Bundle.main.bundleURL.appending(path: "Contents/Helpers/recovery-engine"),
            URL(fileURLWithPath: manager.currentDirectoryPath).appending(path: "engine/target/release/recovery-engine"),
            URL(fileURLWithPath: manager.currentDirectoryPath).appending(path: "engine/target/debug/recovery-engine")
        ]
        guard let url = candidates.first(where: { manager.isExecutableFile(atPath: $0.path) }) else { throw EngineError.missingEngine }
        return url
    }

    public static func run(executable: URL, arguments: [String], input: Data? = nil,
                           receive: @escaping @Sendable (EngineEvent) async -> Void) async throws {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        let output = Pipe()
        let errors = Pipe()
        let standardInput = Pipe()
        process.standardOutput = output
        process.standardError = errors
        process.standardInput = standardInput
        // An engine that exits early must produce a write error, not kill the app.
        guard fcntl(standardInput.fileHandleForWriting.fileDescriptor, F_SETNOSIGPIPE, 1) != -1 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        let termination = AsyncStream<Int32> { continuation in
            process.terminationHandler = { process in
                continuation.yield(process.terminationStatus)
                continuation.finish()
            }
        }
        try Task.checkCancellation()
        try process.run()
        defer {
            if process.isRunning { process.terminate() }
            try? output.fileHandleForReading.close()
            try? errors.fileHandleForReading.close()
            try? standardInput.fileHandleForWriting.close()
        }
        try await withTaskCancellationHandler {
            try Task.checkCancellation()
            async let errorText = collectErrors(errors.fileHandleForReading)
            // Pipe writes can block for a large selection; keep them off the UI actor.
            let writer = Task.detached {
                defer { try? standardInput.fileHandleForWriting.close() }
                if let input { try standardInput.fileHandleForWriting.write(contentsOf: input) }
            }
            do {
                var validator = StreamValidator()
                var completion: EngineEvent?
                for try await line in output.fileHandleForReading.bytes.lines {
                    try Task.checkCancellation()
                    guard line.utf8.count <= 64 * 1024 else { throw EngineError.protocolError }
                    let event = try JSONDecoder().decode(EngineEvent.self, from: Data(line.utf8))
                    try validator.accept(event)
                    if event.event == .complete { completion = event }
                    else { await receive(event) }
                }
                var status: Int32 = -1
                for await result in termination { status = result }
                let message = try await errorText
                try Task.checkCancellation()
                guard status == 0 else { throw EngineError.failed(message.isEmpty ? "복구 엔진이 오류로 종료되었습니다 (\(status))." : message) }
                try await writer.value
                try validator.finish()
                if let completion { await receive(completion) }
            } catch {
                // Stop the child before async-let cleanup waits for stderr to close.
                if process.isRunning { process.terminate() }
                _ = await writer.result
                throw error
            }
        } onCancel: {
            if process.isRunning { process.terminate() }
        }
    }

    private static func collectErrors(_ handle: FileHandle) async throws -> String {
        var result = ""
        for try await line in handle.bytes.lines {
            if result.utf8.count < 16 * 1024 { result += line.prefix(2048) + "\n" }
        }
        return result.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
