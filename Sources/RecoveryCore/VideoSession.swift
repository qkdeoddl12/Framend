import Foundation
import CryptoKit
import Darwin

public final class VideoInput: @unchecked Sendable {
    public let url: URL
    private let handle: FileHandle
    private let identity: Identity
    private struct Identity: Equatable {
        let device: dev_t; let inode: ino_t; let size: off_t
        let modified: timespec; let changed: timespec
        static func == (a: Self, b: Self) -> Bool {
            a.device == b.device && a.inode == b.inode && a.size == b.size &&
            a.modified.tv_sec == b.modified.tv_sec && a.modified.tv_nsec == b.modified.tv_nsec &&
            a.changed.tv_sec == b.changed.tv_sec && a.changed.tv_nsec == b.changed.tv_nsec
        }
    }
    public var size: UInt64 { UInt64(identity.size) }
    public init(_ url: URL) throws {
        let fd = open(url.path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
        guard fd >= 0 else { throw VideoError.failed("입력 파일을 읽을 수 없습니다: \(url.lastPathComponent)") }
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_mode & S_IFMT == S_IFREG, info.st_size > 0 else {
            try? handle.close(); throw VideoError.failed("비어 있지 않은 일반 파일을 선택하세요. 링크·장치는 지원하지 않습니다.")
        }
        self.handle = handle; self.url = url
        identity = Identity(device: info.st_dev, inode: info.st_ino, size: info.st_size, modified: info.st_mtimespec, changed: info.st_ctimespec)
    }
    public func unchanged() throws {
        let current = try VideoInput(url)
        var info = stat()
        guard fstat(handle.fileDescriptor, &info) == 0 else { throw VideoError.failed("입력 파일 연결이 끊어졌습니다.") }
        let held = Identity(device: info.st_dev, inode: info.st_ino, size: info.st_size, modified: info.st_mtimespec, changed: info.st_ctimespec)
        guard identity == current.identity && identity == held else { throw VideoError.failed("입력 파일이 변경되어 작업을 중단했습니다.") }
    }
    func sameFile(as other: VideoInput) -> Bool { identity.device == other.identity.device && identity.inode == other.identity.inode }
    public func sha256() throws -> String {
        var hash = SHA256(); var position: UInt64 = 0
        var buffer = [UInt8](repeating: 0, count: 1_048_576)
        while position < size {
            try Task.checkCancellation()
            let count = pread(handle.fileDescriptor, &buffer, min(buffer.count, Int(size-position)), off_t(position))
            guard count > 0 else { throw VideoError.failed("파일 해시 검사 중 읽기가 실패했습니다.") }
            hash.update(data: Data(buffer.prefix(count))); position += UInt64(count)
        }
        try unchanged()
        return hash.finalize().map { String(format: "%02x", $0) }.joined()
    }
}

public struct VideoSession: Sendable {
    public let directory: URL
    private let held: DirectoryHandle
    private final class DirectoryHandle: @unchecked Sendable {
        let handle: FileHandle
        let device: dev_t
        let inode: ino_t
        init(_ url: URL) throws {
            let fd = open(url.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard fd >= 0 else { throw VideoError.failed("작업 폴더를 열지 못했습니다.") }
            handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
            var info = stat()
            guard fstat(fd, &info) == 0 else { throw VideoError.failed("작업 폴더 식별정보를 읽지 못했습니다.") }
            device = info.st_dev; inode = info.st_ino
        }
    }
    public init(destination: URL) throws {
        var info = stat()
        guard lstat(destination.path, &info) == 0, info.st_mode & S_IFMT == S_IFDIR else {
            throw VideoError.failed("링크가 아닌 저장 폴더를 선택하세요.")
        }
        directory = destination.resolvingSymlinksInPath().appending(path: "Framend-\(UUID().uuidString)")
        guard mkdir(directory.path, 0o700) == 0 else { throw VideoError.failed("새 저장 폴더를 만들지 못했습니다.") }
        held = try DirectoryHandle(directory)
        try write(Data("Incomplete operation. Outputs may be partial or unverified.\n".utf8), named: "INCOMPLETE.txt")
    }
    func checkSpace(_ needed: UInt64) throws {
        _ = try path("space-check")
        var info = statvfs()
        guard fstatvfs(held.handle.fileDescriptor, &info) == 0 else { throw VideoError.failed("저장장치 연결을 확인하세요.") }
        let (free, overflow) = UInt64(info.f_bavail).multipliedReportingOverflow(by: UInt64(info.f_frsize))
        guard !overflow, free >= needed, free - needed >= 64 * 1024 * 1024 else {
            throw VideoError.failed("저장 공간이 부족합니다. 예상 출력과 여유 공간 64MiB가 필요합니다.")
        }
    }
    func path(_ name: String) throws -> URL {
        guard !name.isEmpty, name != ".", name != "..", !name.contains("/"), !name.contains("\0") else {
            throw VideoError.failed("잘못된 결과 파일 이름입니다.")
        }
        var info = stat()
        guard lstat(directory.path, &info) == 0, info.st_mode & S_IFMT == S_IFDIR,
              info.st_dev == held.device, info.st_ino == held.inode else {
            throw VideoError.failed("작업 폴더가 사라지거나 변경되었습니다.")
        }
        return directory.appending(path: name)
    }
    func write(_ data: Data, named name: String) throws {
        _ = try path(name)
        let fd = openat(held.handle.fileDescriptor, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw VideoError.failed("기존 결과를 덮어쓰지 않습니다: \(name)") }
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        try handle.write(contentsOf: data); try handle.synchronize(); try handle.close()
    }
    public func writeJSON<T: Encodable>(_ value: T, named name: String) throws {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try write(encoder.encode(value), named: name)
    }
    func finish() throws {
        _ = try path("INCOMPLETE.txt")
        guard unlinkat(held.handle.fileDescriptor, "INCOMPLETE.txt", 0) == 0 else { throw VideoError.failed("완료 상태를 저장하지 못했습니다.") }
    }
    func failed(_ error: Error) {
        try? writeJSON(["status": error is CancellationError ? "cancelled" : "failed", "reason": error.localizedDescription], named: "failure.json")
    }
}
