import SwiftUI
import RecoveryCore

struct ContentView: View {
    @Bindable var model: RecoveryModel
    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 215, ideal: 245, max: 280)
        } detail: {
            VStack(spacing: 0) {
                sourceHeader
                Divider()
                if model.candidates.isEmpty { emptyState }
                else { results }
                Divider()
                footer
            }
            .background(.background)
        }
        .navigationTitle("Recovery Card")
        .toolbar {
            ToolbarItemGroup {
                Button("이미지 열기", systemImage: "folder") { model.chooseImage() }
                    .disabled(model.isBusy)
                Button("파일 찾기", systemImage: "magnifyingglass") { model.scan() }
                    .disabled(model.imageURL == nil || model.isBusy)
                if model.isBusy {
                    Button("중단", systemImage: "stop.circle") { model.cancel() }
                }
                Button("선택 파일 저장", systemImage: "square.and.arrow.down") { model.chooseDestination() }
                    .disabled(!model.canExport)
            }
        }
        .alert("작업을 확인해 주세요", isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })) {
            Button("확인") { model.errorMessage = nil }
        } message: { Text(model.errorMessage ?? "") }
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 28) {
            VStack(alignment: .leading, spacing: 8) {
                Image(systemName: "sdcard.fill").font(.largeTitle).foregroundStyle(.tint)
                    .accessibilityHidden(true)
                Text("Recovery Card").font(.title2.bold())
                Text("잃어버린 순간을, 다시.").font(.subheadline).foregroundStyle(.secondary)
            }
            VStack(alignment: .leading, spacing: 22) {
                step("1", title: "이미지 선택", detail: "카드 복제본을 불러옵니다", active: model.imageURL != nil)
                step("2", title: "파일 찾기", detail: "JPEG · Fujifilm RAF", active: !model.candidates.isEmpty)
                step("3", title: "복구 저장", detail: "별도 폴더에 안전하게", active: model.outputURL != nil)
            }
            Spacer()
            VStack(alignment: .leading, spacing: 10) {
                Label("원본 읽기 전용", systemImage: "lock.shield")
                Label("Mac 안에서 처리", systemImage: "desktopcomputer")
                Text("첫 프리뷰 · 이미지 파일 모드\n실제 SD카드 연결은 후속 지원")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            .font(.callout)
        }
        .padding(24)
    }

    private func step(_ number: String, title: String, detail: String, active: Bool) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Text(active ? "✓" : number)
                .font(.callout.monospacedDigit().bold())
                .frame(width: 28, height: 28)
                .background(active ? Color.accentColor.opacity(0.16) : Color.secondary.opacity(0.1), in: .circle)
                .foregroundStyle(active ? Color.accentColor : .secondary)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var sourceHeader: some View {
        HStack(spacing: 14) {
            Image(systemName: "externaldrive").font(.title2).foregroundStyle(.secondary).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(model.imageURL?.lastPathComponent ?? "디스크 이미지 선택").font(.headline).lineLimit(1)
                Text(model.imageURL?.deletingLastPathComponent().path ?? "Mac이나 외장 SSD에 보관한 .img · .dd · .raw 파일")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            }
            Spacer()
            Text("FUJIFILM X-T5").font(.caption.monospaced()).foregroundStyle(.secondary)
        }
        .padding(24)
    }

    private var emptyState: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: model.isBusy ? "sparkle.magnifyingglass" : "photo.on.rectangle.angled")
                .font(.system(size: 48, weight: .light)).foregroundStyle(.secondary).accessibilityHidden(true)
            Text(model.isBusy ? "사진의 흔적을 찾고 있습니다" : model.phase == .scanned ? "복구 후보가 없습니다" : "사진을 찾는 첫 단계")
                .font(.title2.bold())
            Text(model.isBusy ? "원본 이미지에는 어떤 내용도 쓰지 않습니다." : "SD카드의 디스크 이미지 파일을 열어주세요.\nJPEG와 RAF의 파일 구조를 찾아 복구 후보를 보여드립니다.")
                .multilineTextAlignment(.center).foregroundStyle(.secondary)
            if !model.isBusy {
                Button(model.imageURL == nil ? "이미지 파일 선택" : "파일 찾기 시작") {
                    if model.imageURL == nil { model.chooseImage() } else { model.scan() }
                }.buttonStyle(.borderedProminent).controlSize(.large)
            }
            Spacer()
            Text("이 버전은 연속 저장된 파일을 찾습니다. 단편화된 파일과 영상은 후속 지원합니다.")
                .font(.caption).foregroundStyle(.secondary).padding(.bottom, 24)
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 24)
    }

    private var results: some View {
        VStack(spacing: 0) {
            HStack {
                Text("발견한 파일 \(model.candidates.count)").font(.headline)
                Spacer()
                Picker("파일 종류", selection: $model.filter) {
                    Text("전체").tag("전체"); Text("JPEG").tag("JPEG"); Text("RAF").tag("RAF")
                }.pickerStyle(.segmented).frame(width: 190)
                Button("모두 선택") { model.selection = Set(model.visibleCandidates.map(\.id)) }
                Button("선택 해제") { model.selection = [] }
            }.padding(16)
            HSplitView {
                Table(model.visibleCandidates, selection: $model.selection) {
                    TableColumn("파일") { candidate in
                        Label(candidate.filename, systemImage: candidate.format == .raf ? "camera.aperture" : "photo")
                            .font(.callout.monospacedDigit())
                    }.width(min: 130, ideal: 170)
                    TableColumn("크기") { candidate in Text(Self.bytes(candidate.length)).foregroundStyle(.secondary) }
                        .width(85)
                    TableColumn("검사 상태") { candidate in
                        Text(model.validationLabels[candidate.id] ?? candidate.statusLabel)
                            .font(.caption).foregroundStyle(.secondary)
                    }.width(min: 155, ideal: 210)
                }
                PreviewView(imageURL: model.imageURL, candidate: model.selectedCandidate)
                    .frame(minWidth: 220, idealWidth: 270, maxWidth: 380)
            }
        }
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 10) {
            if model.isBusy { ProgressView(value: model.progress).accessibilityLabel("작업 진행률") }
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(model.status).font(.callout)
                    if model.isBusy { Text("\(Self.bytes(model.processedBytes)) / \(Self.bytes(model.totalBytes))").font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
                    else if !model.selection.isEmpty { Text("\(model.selection.count)개 선택됨 · ⌘ 또는 ⇧ 키로 여러 파일 선택").font(.caption).foregroundStyle(.secondary) }
                }
                Spacer()
                if model.outputURL != nil {
                    Button("저장 폴더 열기", systemImage: "folder") { model.showOutput() }
                }
            }
        }.padding(20)
    }

    static func bytes(_ value: UInt64) -> String { ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .file) }
}
