import SwiftUI
import Observation
import RecoveryCore
import UniformTypeIdentifiers

@MainActor @Observable
final class RecoveryModel {
    enum Phase { case idle, ready, scanning, scanned, exporting, done, cancelled, failed }
    var phase = Phase.idle
    var imageURL: URL?
    var source: SourceFingerprint?
    var candidates: [Candidate] = []
    var selection: Set<UInt64> = []
    var progress = 0.0
    var processedBytes: UInt64 = 0
    var totalBytes: UInt64 = 0
    var status = "카드 이미지에서 사진을 찾아보세요."
    var errorMessage: String?
    var outputURL: URL?
    var validationLabels: [UInt64: String] = [:]
    var filter = "전체"
    private var scanComplete = false
    private var operation: Task<Void, Never>?
    var isBusy: Bool { phase == .scanning || phase == .exporting }
    var canExport: Bool { scanComplete && !isBusy && !selection.isEmpty }
    var visibleCandidates: [Candidate] {
        candidates.filter { filter == "전체" || $0.format.rawValue.uppercased() == filter }
    }
    var selectedCandidate: Candidate? {
        guard selection.count == 1, let id = selection.first else { return nil }
        return candidates.first { $0.id == id }
    }

    func chooseImage() {
        Task {
            let panel = NSOpenPanel()
            panel.title = "SD카드의 디스크 이미지 선택"
            panel.message = "SSD에 보관한 평면 이미지(.img, .dd, .raw)를 선택하세요. 압축 DMG와 실제 장치는 지원하지 않습니다."
            panel.canChooseDirectories = false
            panel.allowsMultipleSelection = false
            guard await panel.begin() == .OK, let url = panel.url else { return }
            loadImage(url)
        }
    }

    func loadImage(_ url: URL) {
        guard !isBusy else { return }
        imageURL = url.resolvingSymlinksInPath()
        candidates = []; selection = []; source = nil; outputURL = nil; validationLabels = [:]
        scanComplete = false; progress = 0; processedBytes = 0; totalBytes = 0
        phase = .ready
        status = "이미지를 선택했습니다. 파일 찾기를 시작하세요."
    }

    func scan() {
        guard let imageURL, !isBusy else { return }
        candidates = []; selection = []; validationLabels = [:]; outputURL = nil
        source = nil; progress = 0; scanComplete = false
        phase = .scanning; status = "JPEG·RAF 파일 구조를 찾고 있습니다…"
        operation = Task {
            do {
                let engine = try EngineClient.locateEngine()
                try await EngineClient.run(executable: engine, arguments: ["scan", imageURL.path]) { [weak self] event in
                    await self?.receiveScan(event)
                }
                try Task.checkCancellation()
                scanComplete = source != nil
                phase = .scanned
                status = candidates.isEmpty ? "연속 저장된 JPEG·RAF 후보를 찾지 못했습니다." : "\(candidates.count)개 후보를 찾았습니다. 저장할 파일을 선택하세요."
            } catch { handle(error) }
            operation = nil
        }
    }

    private func receiveScan(_ event: EngineEvent) {
        switch event.event {
        case .started:
            source = event.source; totalBytes = event.source?.size ?? 0
        case .candidate:
            if let candidate = event.candidate { candidates.append(candidate) }
        case .progress: updateProgress(event)
        case .complete: progress = 1
        case .saved: break
        }
    }

    func chooseDestination() {
        guard canExport else { return }
        Task {
            let panel = NSOpenPanel()
            panel.title = "복구 파일을 저장할 폴더"
            panel.message = "원본 SD카드가 아닌 Mac 또는 별도 드라이브를 선택하세요. 새 복구 폴더를 만듭니다."
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.canCreateDirectories = true
            guard await panel.begin() == .OK, let destination = panel.url else { return }
            export(to: destination)
        }
    }

    private func export(to destination: URL) {
        guard let imageURL, let source, canExport else { return }
        let selected = candidates.filter { selection.contains($0.id) }
        phase = .exporting; progress = 0; status = "선택한 파일을 새 폴더에 저장합니다…"
        operation = Task {
            do {
                let input = try JSONEncoder().encode(RecoveryRequest(source: source, candidates: selected))
                let saved = SavedFiles()
                try await EngineClient.run(executable: EngineClient.locateEngine(), arguments: ["recover", imageURL.path, destination.path], input: input) { [weak self] event in
                    if event.event == .saved, let candidate = event.candidate, let path = event.path {
                        await saved.append(candidate: candidate, url: URL(fileURLWithPath: path))
                    }
                    await self?.receiveExport(event)
                }
                status = "저장한 JPEG의 픽셀 데이터를 검사합니다…"
                var report: [ValidationRecord] = []
                let files = await saved.files
                for (index, file) in files.enumerated() {
                    try Task.checkCancellation()
                    let result = await Task.detached(priority: .utility) {
                        autoreleasepool { MediaInspector.validate(url: file.url, format: file.candidate.format) }
                    }.value
                    try Task.checkCancellation()
                    validationLabels[file.candidate.id] = result.label
                    report.append(ValidationRecord(file: file.url.lastPathComponent, result: result))
                    status = "이미지 검사 \(index+1)/\(files.count) · \(result.label)"
                }
                if let outputURL {
                    let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
                    try encoder.encode(report).write(to: outputURL.appending(path: "validation.json"), options: .withoutOverwriting)
                }
                phase = .done; progress = 1
                status = "\(selected.count)개 저장 완료 · RAF는 RAW 전체 디코딩 미검증입니다."
            } catch { handle(error) }
            operation = nil
        }
    }

    private func receiveExport(_ event: EngineEvent) {
        if event.event == .progress { updateProgress(event) }
        if event.event == .complete, let directory = event.directory { outputURL = URL(fileURLWithPath: directory) }
    }

    private func updateProgress(_ event: EngineEvent) {
        processedBytes = event.scanned ?? 0
        totalBytes = event.total ?? 0
        progress = totalBytes == 0 ? 0 : Double(processedBytes) / Double(totalBytes)
    }

    func cancel() { status = "중단하고 있습니다…"; operation?.cancel() }

    private func handle(_ error: Error) {
        if error is CancellationError || operation?.isCancelled == true {
            phase = .cancelled
            status = "작업을 중단했습니다. 저장 도중이었다면 INCOMPLETE 표시가 있는 폴더를 확인하세요."
        } else {
            phase = .failed; status = "작업을 완료하지 못했습니다."
            errorMessage = error.localizedDescription
        }
    }

    func showOutput() { if let outputURL { NSWorkspace.shared.open(outputURL) } }
}

private actor SavedFiles {
    struct Entry: Sendable { let candidate: Candidate; let url: URL }
    var files: [Entry] = []
    func append(candidate: Candidate, url: URL) { files.append(Entry(candidate: candidate, url: url)) }
}

private struct ValidationRecord: Encodable { let file: String; let result: MediaValidation }
