import SwiftUI
import AppKit
import RecoveryCore

@MainActor @Observable
final class VideoRecoveryModel {
    enum Mode: String, CaseIterable { case filesystem = "카드 이미지에서 영상 찾기", repair = "손상 영상 수리" }
    var mode: Mode = .filesystem
    var input: URL?
    var reference: URL?
    var destination: URL?
    var scan: VideoScan?
    var selected = Set<UInt64>()
    var outcome: VideoOutcome?
    var outputDirectory: URL?
    var stage = "카드 이미지 또는 손상 영상을 선택하세요"
    var fraction: Double?
    var busy = false
    var error: String?
    var started: Date?
    var toolMessage = VideoRecovery.availability()
    private var task: Task<Void, Never>?

    func choose(_ kind: String) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = kind != "destination"; panel.canChooseDirectories = kind == "destination"
        panel.allowsMultipleSelection = false
        panel.message = kind == "destination" ? "외장 SSD의 저장 폴더를 선택하세요" : kind == "reference" ? "같은 카메라·해상도·코덱·프레임 속도로 촬영한 정상 영상" : "원본을 보존한 디스크 이미지 또는 영상 파일"
        panel.begin { [weak self] result in
            guard result == .OK, let url = panel.url, let self else { return }
            if kind == "destination" { self.destination = url.resolvingSymlinksInPath() }
            else if kind == "reference" { self.reference = url }
            else { self.input = url; self.scan = nil; self.selected = []; self.outcome = nil }
        }
    }
    func resetMode() { scan = nil; selected = []; outcome = nil; input = nil; stage = "입력 파일을 선택하세요" }
    func find() {
        guard let input else { return }
        start {
            let scan = try await VideoRecovery.scan(image: input, receive: self.receiver())
            self.scan = scan
            self.selected = Set(scan.candidates.filter(\.recoverable).map(\.id))
            self.stage = "영상 후보 \(scan.candidates.count)개 · 저장 순서 확인 \(self.selected.count)개"
        }
    }
    func recover() {
        guard let input, let destination else { return }
        let reference = reference; let scan = scan
        let candidates = scan?.candidates.filter { selected.contains($0.id) } ?? []
        let mode = mode
        start {
            let receive = self.receiver()
            let outcome: VideoOutcome
            if mode == .repair, let reference {
                outcome = try await VideoRecovery.repair(input: input, reference: reference, destination: destination, receive: receive)
            } else if let scan {
                outcome = try await VideoRecovery.extract(image: input, scan: scan, candidates: candidates, destination: destination, receive: receive)
            } else { throw VideoError.failed("입력과 복구 후보를 확인하세요.") }
            self.outcome = outcome; self.outputDirectory = outcome.directory
            self.stage = outcome.results.allSatisfy { $0.validation.passed } ? "검사 완료 · 각 파일의 복구 범위를 확인하세요" : "검사 오류 · 결과와 진단 기록을 확인하세요"
        }
    }
    private func receiver() -> @Sendable (VideoUpdate) async -> Void {
        { [weak self] update in await self?.update(update) }
    }
    private func update(_ update: VideoUpdate) {
        stage = update.stage; fraction = update.fraction
        if let directory = update.directory { outputDirectory = directory }
    }
    private func start(_ operation: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }
        busy = true; error = nil; outcome = nil; outputDirectory = nil; fraction = nil; started = Date()
        task = Task {
            defer { busy = false; fraction = nil; task = nil }
            do { try await operation() }
            catch is CancellationError { stage = "중단됨 · 작업 폴더의 미완료 결과를 보존했습니다" }
            catch { self.error = error.localizedDescription; stage = "작업 실패 · 진단 기록을 확인하세요" }
        }
    }
    func cancel() { stage = "중단 요청 · 프로세스 정리 중"; task?.cancel() }
    var canRecover: Bool {
        !busy && toolMessage == nil && input != nil && destination != nil &&
            (mode == .repair ? reference != nil : !selected.isEmpty)
    }
    func showOutput() { if let outputDirectory { NSWorkspace.shared.open(outputDirectory) } }
}
