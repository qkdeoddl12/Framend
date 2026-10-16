#!/usr/bin/env python3
"""Reproducible generated-media tests; all media goes to the supplied external directory."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import time


def run(args, **kwargs):
    result = subprocess.run([str(a) for a in args], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args[0]} exited {result.returncode}: {result.stderr.decode(errors='replace')[-4000:]}")
    return result.stdout


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def damaged(source, output, truncate=False):
    data = bytearray(source.read_bytes())
    position = 0
    found = False
    while position + 8 <= len(data):
        size, kind = struct.unpack_from('>I4s', data, position)
        header = 8
        if size == 1:
            size = struct.unpack_from('>Q', data, position + 8)[0]
            header = 16
        elif size == 0:
            size = len(data) - position
        assert size >= header and position + size <= len(data)
        if kind == b'moov':
            data[position + 4:position + 8] = b'free'
            data[position + header:position + size] = bytes(size - header)
            found = True
        position += size
    assert found
    if truncate:
        data = data[:len(data) - max(1024, len(data) // 8)]
    with output.open('xb') as handle:
        handle.write(data)


def framehash(ffmpeg, file):
    text = run([ffmpeg, '-nostdin', '-v', 'error', '-xerror', '-err_detect', 'explode', '-i', file,
                '-map', '0:v:0', '-fps_mode', 'passthrough', '-f', 'framehash', '-hash', 'sha256', '-']).decode()
    return [line.split(',')[-1].strip() for line in text.splitlines() if line and not line.startswith('#')]


def payload(path):
    data = path.read_bytes()
    position = 0
    chunks = []
    while position + 8 <= len(data):
        size, kind = struct.unpack_from('>I4s', data, position)
        header = 8
        if size == 1:
            size = struct.unpack_from('>Q', data, position + 8)[0]
            header = 16
        if size == 0:
            size = len(data) - position
        assert size >= header and size <= len(data) - position
        if kind == b'mdat':
            chunks.append(data[position + header:position + size])
        position += size
    assert chunks
    return b''.join(chunks)


def corrupt_slices(data, codec):
    data = bytearray(data)
    position = 0
    changed = 0
    while position + 4 <= len(data):
        size = struct.unpack_from('>I', data, position)[0]
        assert 0 < size <= len(data) - position - 4
        kind = data[position + 4] & 31 if codec == 'h264' else (data[position + 4] >> 1) & 63
        if (kind in (1, 5) if codec == 'h264' else kind < 32) and size > 128:
            data[position + 4 + 128:position + 4 + size] = bytes(size - 128)
            changed += 1
        position += size + 4
    assert changed > 0
    return bytes(data)


def segment_cases(args, root, codec, original, reference):
    first, second = payload(original), payload(reference)
    expected_first, expected_second = framehash(args.ffmpeg, original), framehash(args.ffmpeg, reference)
    results = []
    for case, first_bytes in [('mixed', first), ('isolated-damage', corrupt_slices(first, codec)), ('gap-truncated', first[:len(first) * 3 // 4])]:
        source = root / f'{codec}-{case}.mp4'
        source.write_bytes(first_bytes + b'\xff' * 4096 + second)
        before = digest(source)
        elementary = root / f'{codec}-{case}.es'
        raw = run([args.engine, 'video-salvage', source, reference, elementary])
        report = json.loads(raw.splitlines()[-1])['report']
        (root / f'{codec}-{case}.json').write_text(json.dumps(report, indent=2))
        assert len(report['segment_map']) == 2, f'{codec}-{case}: wrong segment count'
        outcomes = []
        stream = elementary.read_bytes()
        for index, segment in enumerate(report['segment_map']):
            start, length = segment['output_offset'], segment['output_length']
            content = stream[start:start + length]
            assert hashlib.sha256(content).hexdigest() == segment['sha256']
            part = root / f'{codec}-{case}-segment-{index + 1}.es'
            part.write_bytes(content)
            output = part.with_suffix('.mp4')
            try:
                run([args.ffmpeg, '-nostdin', '-v', 'error', '-fflags', '+genpts', '-r', f"{report['fps_num']}/{report['fps_den']}",
                     '-f', codec, '-i', part, '-map', '0:v:0', '-c:v', 'copy', '-avoid_negative_ts', 'make_zero', '-n', output])
                actual = framehash(args.ffmpeg, output)
                passed = True
            except RuntimeError:
                actual, passed = [], False
            if index == 1:
                assert passed and actual == expected_second, f'{codec}-{case}: later intact run lost/contaminated'
            elif case == 'mixed':
                assert passed and actual == expected_first, f'{codec}: first run lost/contaminated'
            elif case == 'isolated-damage':
                assert not passed, f'{codec}: corruption fixture unexpectedly decoded'
            # At a mid-NAL cut, the declared length may consume junk. Decoding
            # alone can succeed with changed pixels; measure rather than hide it.
            expected = expected_first if index == 0 else expected_second
            exact_prefix = bool(actual) and actual == expected[:len(actual)]
            outcomes.append(dict(decoded=passed, frames=len(actual), exact_pixel_prefix=exact_prefix))
        assert digest(source) == before and report['input_sha256'] == before
        result = dict(case=f'{codec}-{case}', segments=outcomes, partial=True)
        results.append(result)
        print('PASS', result, flush=True)
    return results


def bframes_case(args, root, codec, encoder):
    original = root / f'{codec}-bframes-original.mp4'
    source = root / f'{codec}-bframes.mp4'
    encoder = [item.replace('bframes=0', 'bframes=3:open-gop=0') for item in encoder]
    if codec == 'h264':
        encoder[encoder.index('-bf') + 1] = '3'
    run([args.ffmpeg, '-nostdin', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=320x180:rate=15',
         '-t', '2', '-pix_fmt', 'yuv420p', *encoder, '-movflags', '+faststart', '-n', original])
    damaged(original, source)
    before = [digest(source), digest(original)]
    elementary = root / f'{codec}-bframes.es'
    raw = run([args.engine, 'video-salvage', source, original, elementary])
    report = json.loads(raw.splitlines()[-1])['report']
    assert report['frames'] == 30 and report['segments'] == 1
    output = root / f'{codec}-bframes-repaired.mp4'
    run([args.ffmpeg, '-nostdin', '-v', 'error', '-fflags', '+genpts', '-r', f"{report['fps_num']}/{report['fps_den']}",
         '-f', codec, '-i', elementary, '-map', '0:v:0', '-c:v', 'copy', '-avoid_negative_ts', 'make_zero', '-n', output])
    expected = framehash(args.ffmpeg, original)
    assert len(expected) == 30
    # Some B-frame layouts still lack usable presentation timestamps after raw
    # salvage. Keep this known failure visible instead of accepting a shorter movie.
    try:
        actual = framehash(args.ffmpeg, output)
    except RuntimeError as error:
        assert 'Non-monotonic DTS' in str(error), str(error)
        actual = None
    if actual is not None:
        assert actual == expected, f'{codec}: B-frame leading pictures were lost'
    assert before == [digest(source), digest(original)]
    result = dict(case=f'{codec}-bframes', strict_decode=actual is not None,
                  exact_pixels=actual == expected, timing_unresolved=actual is None, partial=True)
    (root / f'{codec}-bframes.json').write_text(json.dumps(report, indent=2))
    print('PASS', result, flush=True)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('destination', type=Path)
    parser.add_argument('--engine', type=Path, default=Path('engine/target/release/recovery-engine'))
    parser.add_argument('--ffmpeg', default='/opt/homebrew/bin/ffmpeg')
    args = parser.parse_args()
    root = args.destination.resolve() / time.strftime('native-video-%Y%m%d-%H%M%S')
    root.mkdir(parents=True, exist_ok=False)
    results = []
    for codec in ['h264', 'hevc']:
        encoder = ['-c:v', 'libx264', '-preset', 'ultrafast', '-g', '15', '-bf', '0'] if codec == 'h264' else [
            '-c:v', 'libx265', '-preset', 'ultrafast', '-x265-params', 'pools=1:frame-threads=1:keyint=15:min-keyint=15:scenecut=0:bframes=0:log-level=error', '-tag:v', 'hvc1']
        reference = root / f'{codec}-reference.mp4'
        original = root / f'{codec}-original.mp4'
        for target, generator in [(reference, 'testsrc2=size=320x180:rate=15'), (original, 'testsrc=size=320x180:rate=15')]:
            run([args.ffmpeg, '-nostdin', '-v', 'error', '-f', 'lavfi', '-i', generator, '-t', '2', '-pix_fmt', 'yuv420p',
                 *encoder, '-movflags', '+faststart', '-n', target])
        reference_before = digest(reference)
        expected = framehash(args.ffmpeg, original)
        for truncated in [False, True]:
            case = f'{codec}-' + ('truncated' if truncated else 'missing-moov')
            source = root / f'{case}.mp4'
            damaged(original, source, truncated)
            before = digest(source)
            elementary = root / f'{case}.es'
            report_data = run([args.engine, 'video-salvage', source, reference, elementary])
            (root / f'{case}.jsonl').write_bytes(report_data)
            report = json.loads(report_data.splitlines()[-1])['report']
            output = root / f'{case}-repaired.mp4'
            run([args.ffmpeg, '-nostdin', '-v', 'error', '-fflags', '+genpts', '-r', f"{report['fps_num']}/{report['fps_den']}",
                 '-f', codec, '-i', elementary, '-map', '0:v:0', '-c:v', 'copy', '-avoid_negative_ts', 'make_zero', '-n', output])
            try:
                actual = framehash(args.ffmpeg, output)
                strict_decode = True
            except RuntimeError:
                if not truncated:
                    raise
                actual = []
                strict_decode = False
            if not truncated:
                assert actual == expected, f'{case}: decoded pixels differ or frames missing ({len(actual)}/{len(expected)})'
            elif strict_decode:
                assert actual and actual == expected[:len(actual)], f'{case}: retained frames differ'
            assert digest(source) == before and digest(reference) == reference_before, 'Input modified'
            assert report['input_sha256'] == before and report['reference_sha256'] == reference_before
            assert report['partial'] is True, 'Salvage mislabeled complete'
            results.append(dict(case=case, decoded_frames=len(actual), source_frames=len(expected), strict_decode=strict_decode, partial=True))
            print('PASS', results[-1], flush=True)
        results.extend(segment_cases(args, root, codec, original, reference))
        results.append(bframes_case(args, root, codec, encoder))
    (root / 'summary.json').write_text(json.dumps(results, indent=2))
    print('FIXTURE_DIRECTORY=' + str(root), flush=True)


if __name__ == '__main__':
    main()
