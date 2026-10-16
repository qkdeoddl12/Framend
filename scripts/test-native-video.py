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
                 '-f', codec, '-i', elementary, '-map', '0:v:0', '-c:v', 'copy', '-n', output])
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
    (root / 'summary.json').write_text(json.dumps(results, indent=2))
    print('FIXTURE_DIRECTORY=' + str(root), flush=True)


if __name__ == '__main__':
    main()
