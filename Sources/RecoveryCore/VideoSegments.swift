import Foundation

extension NativeSalvageReport {
    public func validateSegments(outputSize: UInt64, inputSize: UInt64) throws {
        func invalid() -> VideoError { .failed("엔진의 영상 구간 기록이 유효하지 않습니다.") }
        guard !segment_map.isEmpty, segment_map.count <= 256, segments == UInt64(segment_map.count),
              partial, frames > 0, ["h264", "hevc"].contains(codec), fps_num > 0, fps_den > 0,
              frame_count_basis == "structural_access_unit_starts", timing_basis == "estimated_reference_average" else { throw invalid() }
        var outputCursor: UInt64 = 0; var extentCursor = 0; var frameCount: UInt64 = 0
        for (index, segment) in segment_map.enumerated() {
            guard segment.id == index + 1, segment.output_offset == outputCursor,
                  segment.output_length > 0, outputCursor <= outputSize,
                  segment.output_length <= outputSize - outputCursor,
                  segment.extent_start == extentCursor, segment.extent_count > 0,
                  extentCursor <= extents.count, segment.extent_count <= extents.count - extentCursor,
                  segment.frames > 0, frameCount <= frames, segment.frames <= frames - frameCount,
                  segment.sha256.utf8.count == 64, segment.sha256.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
                  ["unknown_bytes", "picture_mismatch", "range_end"].contains(segment.end_reason) else { throw invalid() }
            outputCursor += segment.output_length; extentCursor += segment.extent_count; frameCount += segment.frames
        }
        guard outputCursor == outputSize, extentCursor == extents.count, frameCount == frames else { throw invalid() }
        var sourceEnd: UInt64 = 0; var retained: UInt64 = 0
        for extent in extents {
            guard extent.length > 0, extent.offset >= sourceEnd, extent.offset <= inputSize,
                  extent.length <= inputSize - extent.offset else { throw invalid() }
            sourceEnd = extent.offset + extent.length; retained += extent.length
        }
        guard skipped_bytes == inputSize - retained else { throw invalid() }
    }
}
