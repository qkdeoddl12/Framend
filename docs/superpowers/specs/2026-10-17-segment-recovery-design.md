# Independent video segment recovery

The user authorized continuing the unfinished recovery engine. The next milestone
is retaining useful independent video runs without joining across unknown bytes.
This is a prerequisite for honest fragment reconstruction, not arbitrary fragment
reordering or an AI model. Existing Rust/Swift code and local FFmpeg remain.

## Behavior

The native scanner already drops dependent pictures after unknown bytes and resumes
at a viable IDR. It currently concatenates those runs into one elementary stream.
Keep that stream as an intermediate evidence artifact, but add a segment map with
exact output byte ranges, SHA-256, structural frame counts, source extent indices,
and the reason each run ended. Do not merge source extents across a run boundary.
Each run begins with the reference parameter sets followed by original payloads.

The app validates that the map covers the entire intermediate file with no gaps,
overlap, zero-length entries, or count inconsistencies. It copies each range using
bounded reads into a new exclusive file in the pinned output session, checks its
SHA-256, muxes it separately, and fully decodes it. No automatic cross-gap joining.
File numbering reflects physical scan order, not established recording chronology.

If a muxer or decoder exits unsuccessfully, record its exit and diagnostics and
preserve the elementary stream; continue other runs without assuming the cause
was media corruption. Cancellation, source change, invalid tool/engine protocol,
process launch or resource-limit failures, insufficient free space, and the app's
own copy/report I/O failures stop the operation. Keep INCOMPLETE if any run fails. A
successful run remains labelled partial, video-only, with estimated timing.
Every result links to one segment ID and has a per-segment evidence JSON. Legacy
single-run output remains recovered-video.mp4; multiple runs use numbered names.

## Limits and invariants

- Read originals only; bulk artifacts on the chosen external destination.
- At most 256 segments. Hitting the limit fails explicitly, never claims a full scan.
- Existing parser and resource bounds still apply. No new dependency.
- Budget three input sizes plus parameter-set overhead and existing free-space margin.
- Do not claim same-source provenance within a contiguous run, exact timing, audio,
  semantic reference compatibility, or complete recovery from decode success.
- Keep CLI video-salvage compatible; its aggregate ES is not a recommended final movie.
- UI explains separate output for disconnected runs and surfaces per-run errors.

## Evidence required

Rust fixtures: unknown gaps, adjacent container ranges, picture mismatch, single
run, exact segment hashes and extent partitions, limit exhaustion, cancellation.
Swift checks: malformed maps, wrong hash/range, exclusive streaming copy, source
replacement and cancellation, failure status, continued processing after one bad run.
Generated H.264 and HEVC clips with distinct pictures and matching settings: interleave
whole media runs with junk, verify two separate outputs against each source's pixel
hashes; truncate one run and verify the other remains useful. Also mix a structurally
plausible but undecodable run with a valid run and verify isolation.

Builder choices: deterministic filenames, JSON field names and helper boundaries
follow current conventions; retain the existing feature checkout; no publication.
