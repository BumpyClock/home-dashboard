# Photo transition and cache writer validation, 2026-10-27

Five matched pairs on the USB-attached PF1007L reduced the median longest
render-start gap during a slide from 143.0 ms to 66.0 ms. Prepared textures
removed allocation/upload work from every measured candidate transition.
Steady animation cadence remains about 34 ms. This is not a 60 FPS result
or a measurement of physical panel scanout.

## Changes under test

Two stable image slots retain the outgoing and incoming photo across slide
and fade transitions. The patched FemtoVG renderer prepares the next photo
outside animation and reuses its pixel-buffer identity across image elements.
The current photo must have pixels before preparation is queued. Preparation
waits one render so it does not precede the current photo's first frame.

Prepared textures have a 16 MiB estimated RGBA-byte limit and LRU eviction.
This excludes retained RGB buffers, visible element textures, the pending
image, and driver overhead. Preparation is best effort for the next sequential
photo. Reverse or rapid navigation can still upload during animation.

Full-screen disk persistence uses one writer. Its admission limits include
the active write and queued writes, at most eight buffers and 24 MiB of RGB
pixels. Decode workers can claim their next job after UI acknowledgement.
Duplicate or excess cache writes are skipped without dropping display work.
Queued optional writes may be lost on process exit. Source-version checks,
atomic replacement, JPEG quality 90, and the 128 MiB disk limit remain.

## Matched hardware captures

- Baseline executable SHA-256 is
  `f163b66a39439855212cb97573507d24732b3250526d9d2e1b9e779f06f70e24`.
  This is the prior implementation with trace hooks.
- Candidate executable SHA-256 is
  `00d5c7b2f9aed460a32cb85ecec61c8570e9d387b933d57cae452ebe498c18f5`.
- Five alternating baseline/candidate pairs used the same 66-photo library,
  warm private disk cache, copied settings, portrait 800×1280 display, and
  normal GPU policy. Rotation was disabled. Screensaver was forced on.
- Each process ran 31 seconds and captured 28 seconds, including one automatic
  500 ms slide. Start/end temperature readings spanned 41.31–43.25 °C.
- Both builds used bounded tracing. Neither used a rendering notifier or
  the optional GPU fence probe. Every capture had a complete summary with
  zero dropped events, truncated fields, or recorded operation failures.

Values below are medians across five runs, with run ranges in parentheses.

| Measurement | Baseline | Candidate |
|---|---:|---:|
| Longest render-start gap within transition | 143.0 ms (138.1–153.8) | 66.0 ms (65.3–69.0) |
| Render frames started within transition | 8 (8–9) | 15 (15–15) |
| Per-run median render-start gap | 34.09 ms (33.99–34.39) | 33.99 ms (33.98–34.04) |
| Allocation/upload wall time within transition | 266.1 ms (262.4–276.9) | 0 ms |
| Transition frames with texture upload work | 2 | 0 |
| First full photo assigned to UI after trace start | 1.120 s (1.115–1.154) | 1.142 s (1.126–1.211) |
| Peak sampled process RSS | 68.0 MiB (67.3–69.1) | 68.3 MiB (67.8–68.5) |
| Process CPU time between resource samples | 2.62 s (2.60–2.65) | 2.66 s (2.64–2.71) |

The median longest gap improved by 54%. The first-photo median increased
23 ms and the ranges overlap. No startup improvement is claimed. RSS was
sampled once per second and excludes GPU allocations. CPU time uses process
user/system ticks at 100 ticks per second. Allocation/upload timings are
wall durations of instrumented calls, not GPU execution durations.

The analyzer pairs transition start/end events, selects render starts inside
those bounds, and compares consecutive starts. It does not discard long
intervals with a cutoff or include idle time between slides. Results and
capture hashes are preserved in [results.json](performance-2026-10-27/results.json).

## GPU scheduling diagnostic

A separate candidate capture confirmed `EGL_KHR_fence_sync` support. All
31 submitted fences were still pending at each zero-timeout poll before
page-flip wait, after that wait, and after swap. The diagnostic adds an
explicit flush, but steady cadence remained about 34 ms. This does not
support adding that flush in normal operation. It also does not establish
exact GPU completion time or justify removing the page-flip wait.

`FRAME_GPU_FENCE_TRACE=1` remains opt-in and requires an active bounded
trace. Ordinary operation and ordinary traces preserve synchronization.

## Cold cache and consecutive slides

A separate 28-second run began with an empty private disk cache. All five
full-screen writes completed successfully. Maximum reserved writer memory
was 9,216,000 bytes, below the 24 MiB admission limit. The associated decode
jobs ended 0.476–1.373 seconds before their writer completions. This confirms
that persistence no longer occupies those workers until the write finishes.
No queue admission was dropped in this run. Saturation behavior is covered
by the blocked-writer unit test, not demonstrated by this workload.

A 108-second run completed five consecutive slides with zero texture-upload
calls inside all five transitions. Their longest render-start gaps were
64.6–75.1 ms. Six background preparations and two LRU evictions were recorded.
Four newly needed full-screen files were persisted successfully. Peak sampled
process RSS was 89.3 MiB and the last sample was 70.6 MiB. This short run
exercises eviction but does not establish long-term memory stability.

Both runs had zero dropped trace events and recorded photo-operation
failures. Their logs did contain a weather DNS lookup failure; cached weather
was retained. The tests do not establish network connectivity.

## Verification and artifacts

The application suite passed 109 tests. Three renderer handoff tests,
frame-trace tests, and ten patch-preparation tests passed. Software-rendered
pixel checks cover PhotoCard and screensaver fades in both slot directions,
including the night overlay. The final ARM release build passed.

Private raw captures, receipts, logs, exact binaries, source SHA-256 inventory,
and harnesses are under `.claude/optimize-20261027/`. The alternating-run
harness is `run-ab.sh`; `analyze.py` produces the table's source data.
The `diagnostic/` directory contains the ordinary and fence-enabled captures.
These are local, gitignored artifacts. No claim of hardware pixel equivalence
is made from the software-renderer tests.

Source edits appeared in the shared checkout after this binary was built.
The measurements above apply only to executable `00d5c7b2`, not every current
working-tree edit. `source-sha256.json` identifies the measured source snapshot;
`post-build-source-edits.json` records the later changes. Those changes were
preserved and were not incorporated into the measured artifact.

## Deployment

The validated executable was installed on the USB-attached PF1007L as
`/opt/photoframe-proto/releases/00d5c7b2` on October 27 at 15:52 PDT. The
`current` link was switched atomically. Startup verification confirmed the
running process executable's full SHA-256 and an active service with zero
restarts. A follow-up check confirmed the same process remained active and
trace, metrics, and GPU-fence diagnostic environment variables were absent.
Its receipt is `.claude/optimize-20261027/deployment-health.txt`. Rollback
release `48b37fba` was retained. This was a device deploy; no Git commit or
remote push was performed.
