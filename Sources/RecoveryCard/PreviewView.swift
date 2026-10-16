import SwiftUI
import RecoveryCore

struct PreviewView: View {
    let imageURL: URL?
    let candidate: Candidate?
    @State private var thumbnail: CGImage?
    @State private var loading = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("미리보기").font(.headline)
            ZStack {
                RoundedRectangle(cornerRadius: 10).fill(.quaternary)
                if let thumbnail {
                    Image(decorative: thumbnail, scale: 1).resizable().scaledToFit().padding(8)
                } else if loading { ProgressView() }
                else {
                    Text(candidate == nil ? "파일 하나를 선택하세요" : "미리보기를 만들 수 없습니다")
                        .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center).padding()
                }
            }.frame(minHeight: 160, maxHeight: 240)
            if let candidate {
                LabeledContent("형식", value: candidate.format.rawValue.uppercased())
                LabeledContent("크기", value: ContentView.bytes(candidate.length))
                if let camera = candidate.camera { LabeledContent("카메라", value: camera) }
                if let width = candidate.width, let height = candidate.height {
                    LabeledContent("해상도", value: "\(width) × \(height)")
                }
                Text(candidate.format == .raf ? "RAF에 포함된 JPEG 미리보기입니다. RAW 원본의 정상 여부는 아직 검증하지 않았습니다." : "미리보기만으로 원본의 무결성을 판단할 수 없습니다. 저장 후 JPEG 디코딩 검사를 진행합니다.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .font(.callout)
        .padding(20)
        .task(id: candidate) {
            thumbnail = nil
            loading = false
            guard let candidate, let imageURL else { return }
            loading = true
            let result = await Task.detached(priority: .utility) {
                try? MediaInspector.thumbnail(imageURL: imageURL, candidate: candidate)
            }.value
            guard !Task.isCancelled else { return }
            thumbnail = result
            loading = false
        }
    }
}
