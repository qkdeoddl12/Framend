#!/usr/bin/env python3
"""Portable desktop-worker integration using small, generated media and exFAT data."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import wave
import zipfile
import zlib


def digest(data):
    return hashlib.sha256(data).hexdigest()


def png():
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b'\0\x10\x80\xff\xff')) + chunk(b'IEND', b'')


def wav():
    data = io.BytesIO()
    with wave.open(data, 'wb') as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(8000)
        stream.writeframes(b'\0\0' * 800)
    return data.getvalue()


def exfat(files):
    image = bytearray(1024 * 1024)
    def put(fmt, offset, value):
        struct.pack_into('<' + fmt, image, offset, value)
    def cluster(number):
        return 40 * 512 + (number - 2) * 512
    def allocate(number):
        image[cluster(3) + (number - 2) // 8] |= 1 << ((number - 2) % 8)
    def checksum(data, bits, skip):
        total = 0
        for index, byte in enumerate(data):
            if index not in skip:
                total = (((total >> 1) | ((total & 1) << (bits - 1))) + byte) & ((1 << bits) - 1)
        return total
    image[:11] = b'\xeb\x76\x90EXFAT   '
    for fmt, offset, value in [('Q',72,2048),('I',80,24),('I',84,16),('I',88,40),('I',92,2008),('I',96,2),('H',104,0x100),('H',510,0xaa55)]:
        put(fmt, offset, value)
    image[108], image[110], image[112] = 9, 1, 255
    for index in range(1, 9):
        put('I', (index + 1) * 512 - 4, 0xaa550000)
    check = checksum(image[:11 * 512], 32, {106, 107, 112})
    for offset in range(11 * 512, 12 * 512, 4):
        put('I', offset, check)
    image[12 * 512:24 * 512] = image[:12 * 512]
    for index in range(4):
        put('I', 24 * 512 + index * 4, 0xfffffff8 if index == 0 else 0xffffffff)
    image[cluster(2)] = 0x81
    put('I', cluster(2) + 20, 3)
    put('Q', cluster(2) + 24, (2008 + 7) // 8)
    allocate(2)
    allocate(3)
    directory, start = cluster(2) + 32, 8
    for name, contents in files.items():
        encoded = name.encode('utf-16le')
        assert len(encoded) <= 30
        entry = bytearray(96)
        entry[0], entry[1], entry[32], entry[33], entry[35], entry[64] = 0x85, 2, 0xc0, 3, len(encoded) // 2, 0xc1
        struct.pack_into('<H', entry, 4, 0x20)
        struct.pack_into('<Q', entry, 40, len(contents))
        struct.pack_into('<I', entry, 52, start)
        struct.pack_into('<Q', entry, 56, len(contents))
        entry[66:66 + len(encoded)] = encoded
        struct.pack_into('<H', entry, 2, checksum(entry, 16, {2, 3}))
        image[directory:directory + 96] = entry
        image[cluster(start):cluster(start) + len(contents)] = contents
        count = (len(contents) + 511) // 512
        for number in range(start, start + count):
            allocate(number)
        directory += 96
        start += count
    return bytes(image)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', type=Path, required=True)
    parser.add_argument('--output-root', type=Path, required=True)
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix='desktop-jobs-', dir=args.output_root.resolve()))
    def run(request):
        result = subprocess.run([str(args.engine.resolve()), 'desktop-job'], input=json.dumps(request), text=True, capture_output=True, timeout=120)
        assert result.returncode == 0, result.stderr
        events = [json.loads(line) for line in result.stdout.splitlines()]
        assert events[0]['event'] == 'started' and events[-1]['event'] == 'complete'
        return events
    archives = io.BytesIO()
    with zipfile.ZipFile(archives, 'w') as archive:
        archive.writestr('hello.txt', 'portable archive')
    files = {'사진.MOV': png(), 'tone.wav': wav(), 'report.txt': b'General document recovery\n', 'archive.zip': archives.getvalue()}
    cases = [('carve', b'\0' * 31 + png() + b'\0' * 19 + wav(), [png(), wav()]), ('filesystem', exfat(files), list(files.values()))]
    summary = []
    for method, image, expected in cases:
        source = root / f'{method}-한글 card.img'
        source.write_bytes(image)
        scan = run({'operation':'scan', 'source':str(source), 'method':method})
        entries = [e['entry'] for e in scan if e['event'] == 'candidate']
        assert len(entries) == len(expected) and all(e['recoverable'] for e in entries)
        result = run({'operation':'recover','source':str(source),'output':str(root),'selections':[e['selection'] for e in entries]})
        saved = [e['result'] for e in result if e['event'] == 'saved']
        assert sorted(item['sha256'] for item in saved) == sorted(digest(data) for data in expected)
        for item in saved:
            assert digest(Path(item['path']).read_bytes()) == item['sha256']
            assert item['inspection'] in ('decoded', 'structure_only', 'unverified')
        assert source.read_bytes() == image
        assert result[-1]['failed'] == 0
        assert not (Path(result[-1]['directory']) / 'INCOMPLETE.txt').exists()
        if method == 'filesystem':
            assert next(i for i in saved if i['name'] == 'archive.zip')['inspection'] == 'unverified'
            assert next(i for i in saved if i['name'] == 'report.txt')['inspection'] == 'unverified'
            assert next(i for i in saved if i['name'] == '사진.MOV')['inspection'] in ('decoded', 'structure_only')
        summary.append({'method':method,'count':len(saved),'results':saved})
    (root / 'verification.json').write_text(json.dumps(summary, indent=2), encoding='utf-8')
    print(f'PASS: 2 carved files and 4 exFAT files, exact hashes, unchanged inputs, honest inspection. Evidence: {root}')


if __name__ == '__main__':
    main()
