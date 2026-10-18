#!/usr/bin/env python3
"""CLI integration: exact recovery, source integrity, invalid requests, clean protocol."""
import hashlib
import json
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
engine = root / "engine/target/release/recovery-engine"
fixture = root / "fixtures/generated"
image = fixture / "demo-card.img"
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
before = sha(image)

def run(*args, payload=None):
    return subprocess.run([str(engine), *map(str, args)], input=payload, text=True, capture_output=True, check=True)

events = [json.loads(line) for line in run("scan", image).stdout.splitlines()]
assert events[0]["event"] == "started"
assert events[-1]["event"] == "complete" and events[-1]["count"] == 2
candidates = [event["candidate"] for event in events if event["event"] == "candidate"]
assert [c["format"] for c in candidates] == ["jpeg", "raf"]
request = {"source": events[0]["source"], "candidates": candidates}
with tempfile.TemporaryDirectory(prefix="recovery-smoke-") as output:
    output = pathlib.Path(output).resolve()
    result = [json.loads(line) for line in run("recover", image, output, payload=json.dumps(request)).stdout.splitlines()]
    saved = [e for e in result if e["event"] == "saved"]
    assert len(saved) == 2
    for event, original in zip(saved, ["original.jpg", "synthetic.raf"]):
        recovered = pathlib.Path(event["path"])
        assert sha(recovered) == sha(fixture / original) == event["sha256"]
    directory = pathlib.Path(result[-1]["directory"])
    assert (directory / "manifest.json").is_file()
    assert not (directory / "INCOMPLETE.txt").exists()
    manifest = json.loads((directory / "manifest.json").read_text())
    assert manifest["status"] == "extracted_not_decoded"
assert sha(image) == before
print("PASS CLI: two exact SHA-256 matches, unchanged source, complete manifest, JSON stream")
