import SwiftUI
import RecoveryCore

struct VideoRecoveryView: View {
    @Bindable var model: VideoRecoveryModel
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                VStack(alignment: .leading, spacing: 5) {
                    Text("Framend · 영상 복구").font(.title2.bold())
                    Text("파일 조각과 프레임 구조를 분석하고, 결과를 끝까지 검사합니다.").foregroundStyle(.secondary)
                }
                Spacer()
                Text("실험 기능").font(.caption).padding(8).background(.quaternary, in: .capsule)
            }
            Picker("복구 방식", selection: $model.mode) {
                ForEach(VideoRecoveryModel.Mode.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented).disabled(model.busy)
            .onChange(of: model.mode) { model.resetMode() }
            GroupBox {
                VStack(alignment: .leading, spacing: 10) {
                    fileRow(model.mode == .repair ? "손상 영상" : "카드 복제본", url: model.input, kind: "input")
                    if model.mode == .repair { fileRow("정상 참고 영상", url: model.reference, kind: "reference") }
                    fileRow("저장 폴더", url: model.destination, kind: "destination")
                    Text(model.mode == .repair ? "같은 카메라·코덱·해상도·프레임 속도의 정상 영상이 필요합니다. 영상 트랙만 부분 복구하며 음성과 원래 재생 시간은 복원하지 않습니다." : "exFAT 파일 목록에 남은 MOV·MP4의 저장 순서를 확인합니다. 파일 목록이 사라진 경우에는 후보가 없을 수 있습니다.")
                        .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }.padding(6)
            }
            if let message = model.toolMessage { Label(message, systemImage: "exclamationmark.triangle").foregroundStyle(.orange) }
            if let outcome = model.outcome {
                List(outcome.results) { result in
                    VStack(alignment: .leading, spacing: 6) {
                        Text(result.file).font(.headline)
                        Text(result.label).foregroundStyle(result.validation.passed ? .green : .orange)
                        ForEach(result.validation.tracks, id: \.index) { track in
                            Text("\(track.type) · \(track.codec) · \(track.decodedFrames) 프레임" + (track.decodedSamples > 0 ? " · \(track.decodedSamples) 음성 샘플" : ""))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        ForEach(result.warnings, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
                    }.padding(.vertical, 5)
                }
            } else if let scan = model.scan {
                if !scan.warnings.isEmpty { Text(scan.warnings.joined(separator: "\n")).font(.caption).foregroundStyle(.orange).lineLimit(3) }
                if scan.candidates.isEmpty {
                    ContentUnavailableView("파일 목록에서 영상 후보를 찾지 못했습니다", systemImage: "film", description: Text("이 결과는 카드에 영상 데이터가 전혀 없다는 뜻은 아닙니다."))
                } else {
                    List(scan.candidates) { candidate in
                        HStack {
                            Toggle("선택", isOn: Binding(get: { model.selected.contains(candidate.id) }, set: { value in
                                if value { model.selected.insert(candidate.id) } else { model.selected.remove(candidate.id) }
                            })).labelsHidden().disabled(!candidate.recoverable || model.busy)
                            VStack(alignment: .leading, spacing: 4) {
                                Text(candidate.name).font(.headline)
                                Text(candidate.statusLabel).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text(ByteCountFormatter.string(fromByteCount: Int64(clamping: candidate.length), countStyle: .file)).monospacedDigit()
                            Text("\(candidate.extents.count)개 조각").foregroundStyle(.secondary)
                        }.padding(.vertical, 3)
                    }
                }
            } else {
                ContentUnavailableView(model.mode == .repair ? "프레임에 남은 장면을 찾습니다" : "카드의 파일 조각 순서를 확인합니다", systemImage: "film.stack", description: Text("원본은 읽기 전용으로 열고, 결과는 새 작업 폴더에 저장합니다."))
            }
            Divider()
            HStack(spacing: 14) {
                if model.busy {
                    if let fraction = model.fraction { ProgressView(value: fraction).frame(width: 100) }
                    else { ProgressView().controlSize(.small) }
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text(model.stage).font(.callout)
                    if model.busy, let started = model.started { Text(started, style: .relative).font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
                }
                Spacer()
                if model.outputDirectory != nil { Button("결과 폴더") { model.showOutput() } }
                if model.busy { Button("중단", role: .destructive) { model.cancel() } }
                else {
                    if model.mode == .filesystem { Button("영상 찾기") { model.find() }.disabled(model.input == nil || model.toolMessage != nil) }
                    Button(model.mode == .repair ? "영상 부분 복구" : "선택 영상 복구") { model.recover() }.buttonStyle(.borderedProminent).disabled(!model.canRecover)
                }
            }
        }
        .padding(24)
        .alert("작업을 확인해 주세요", isPresented: Binding(get: { model.error != nil }, set: { if !$0 { model.error = nil } })) {
            Button("확인") { model.error = nil }
        } message: { Text(model.error ?? "") }
    }
    private func fileRow(_ title: String, url: URL?, kind: String) -> some View {
        HStack {
            Text(title).frame(width: 105, alignment: .leading).foregroundStyle(.secondary)
            Text(url?.path ?? "선택하지 않음").lineLimit(1).truncationMode(.middle)
            Spacer()
            Button("선택") { model.choose(kind) }.disabled(model.busy)
        }
    }
}
