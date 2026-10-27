# Telemetry diagnosis, 2026-10-27

The strongest next target is GPU texture reuse and preparation before photo
transitions. The captured transition stalls in texture allocation/upload even
though its next photo's pixels were assigned to the UI 18.595 seconds earlier.
Cache persistence also occupies decode workers after their UI results are ready.

This is diagnosis of historical captures, not a measured improvement or a new
deployment. The subsequent stable-slot, texture-prewarm, and cache-writer
changes are not represented in these captures.

## Evidence and limits

- USB PF1007L, ARM release `f163b66a39439855212cb97573507d24732b3250526d9d2e1b9e779f06f70e24`.
- One 35-second run with a private cold cache and 66 photos, plus one roughly
  4-second restart using that cache. Both start in the screensaver. The restart
  was a shutdown check, not a matched benchmark.
- 1,267 and 498 events respectively. Both summaries match event counts, with
  zero drops and zero truncated fields. No recorded `ok=false`, `success=false`,
  or UI rejection occurred in the cold run. No stage spans were incomplete.
- The cold run contains one complete transition. It does not establish
  repeatability, grid behavior, tracing overhead, or sustained thermal behavior.
- Event durations are CPU-side wall durations, not CPU execution or GPU time.
  Worker durations may overlap. Nested phases must not be summed with parents.
- The saved source hashes identify the historical captured build, not the
  current app/UI/instrumentation/patch files. Source references below describe
  that captured implementation. Slint source inspected is pinned to 1.18.1.
  Current changes have host regression coverage, not new device validation.

The raw captures remain unchanged at
`.claude/trace-20261027/photos/{trace,shutdown}.jsonl`. SHA-256 values are
`1bf6aff172413691e70de835a3cb27ef7983ab261efb9a06514608912f1a35c4`
and `be8bc24fec1e119bd5170985773cfb08775bebe50c663cf927c6699b4457c78f`.
Reproduce the additional grouping with
`python3 .claude/trace-20261027/diagnosis/analyze.py`; derived output is
`diagnosis/details.json`. These artifacts are local and gitignored.

## 1. Prepare and retain textures before a transition

Measured transition 89 starts at 20.889565 s and ends at 21.395464 s.
Its destination, photo index 1, was assigned by job 6 at 2.294418 s.

| Render ID | Render wall time | Texture allocation | Texture upload | Upload payload |
| --- | ---: | ---: | ---: | ---: |
| 93 | 139.393 ms | 75.636 ms | 51.723 ms | 800 x 1280 RGB, 3,072,000 bytes |
| 94 | 133.183 ms | 72.964 ms | 52.379 ms | 800 x 1280 RGB, 3,072,000 bytes |

Those two frames spend 252.702 ms inside allocation/upload calls, approximately
half the 505.899 ms transition window. Their render-start intervals are 140.187
and 133.807 ms. This is direct evidence of expensive texture work on the
animation path. It is not evidence that removing it would halve total latency.

Source establishes a likely mechanism. [Photo publication](../src/photos.rs#L1446)
uses `slint::Image::from_rgb8`. In pinned Slint `graphics/image.rs:833`, that
constructor sets `ImageCacheKey::Invalid`. `ImageCacheKey::new` returns `None`
for it. FemtoVG `itemrenderer.rs:1232` then uses each image element's graphics
cache and falls back to creating a texture without the shared texture cache.
The [screensaver](../ui/screensaver.slint#L67) has separate previous/current
image elements whose sources change during transition.

Inference: moving the outgoing photo between image elements can upload it
again, while the incoming photo uploads on first actual draw. The trace lacks
texture-to-photo identity, so it does not prove which photo each upload holds.

Recommended experiment: preserve the outgoing image element/texture while
rotating two stable image slots, then prepare the incoming texture during idle
time. Compare that with a byte-bounded renderer cache shared by pixel-buffer
identity. A renderer preparation path is needed if clipping or invisibility
skips drawing; merely adding a hidden Image is not proof of upload. Bound
retained textures by bytes and release them on rendition replacement.

Acceptance: repeat transitions with no full-screen texture allocation/upload
inside their active intervals; verify unchanged pixels, bounded memory, and
the resulting cadence. Add stable rendition/texture IDs and cache hit/miss
reasons to distinguish reuse from repeated upload.

## 2. Investigate presentation cadence after eliminating upload spikes

Frames 96 through 100 are the five later frames within the same transition.
Median render wall time is 33.453 ms. Median scene work is 2.069 ms, flush
2.654 ms, and previous-page-flip wait 27.464 ms. Summed wait time is 82.3% of
their summed render wall time. Those medians are not additive.

The four render-start intervals between these frames range from 33.862 to
34.156 ms. DRM sequence numbers advance by two per flip, with kernel-clock
flip deltas near 34.0 ms. This transition settles around 29 frames/second.
The preceding frame 95 also waits 52.528 ms for a previous flip.

Measured limitation: the UI thread waits in the backend. Unresolved cause:
GPU completion, buffer availability, or presentation scheduling/phase. A long
wait does not prove the wait itself is unnecessary. The current hooks provide
no GPU completion timestamp. Do not remove synchronization based on this trace.

Recommended experiment: repeat with textures already resident, then relate
command submission, nonblocking GPU completion signals if supported, and DRM
flip sequences. Examine whether the current render/wait/swap/submit ordering
misses a refresh opportunity. Optimize that ordering only after separating
driver/GPU work from scheduling.

## 3. Release decode workers before disk-cache persistence

Across five foreground full-screen jobs, summed worker wall duration is
7.466 s. Cache writes take 3.240 s and acquisition of the serialized full-cache
write lock takes another 0.720 s. Together that is 53.0% of those summed job
durations. These are overlapping worker lifetimes, not 3.960 seconds of
avoidable user-visible delay or CPU execution.

Per-write JPEG encoding ranges from 448 to 511 ms. Fsync ranges from 52 to
505 ms. Job 21 assigns its image at 2.576594 s but waits 673.401 ms for the
write lock and does not finish until 3.778746 s. Job 5 similarly assigns its
image at 1.499193 s and remains occupied until 2.121971 s.

Source confirms that the [worker saves the pending full rendition](../src/photos.rs#L1486)
before releasing its claim. [The store lock](../src/photos/thumb_store.rs#L123)
serializes the write and pruning. Deferred UI publication already protects the
current image from waiting for encoding, but it does not free that worker to
prepare another requested image.

Recommended experiment: transfer cache persistence to one writer with a bounded
queue, preserving source-version checks and deduplication. Decode workers can
then service current/next images promptly. Limit queued pixel-buffer bytes and
define backpressure before adding the writer. Measure requested-image latency
under rapid navigation and cold cache creation, not just cache throughput.

## Secondary findings

- Background generation performs 66 thumbnail jobs from 3.778789 to 20.123037 s.
  Summed job wall time is 32.003 s, including 20.056 s in original decode.
  It ends 0.767 s before the transition, so it cannot explain that transition's
  initial upload stalls. Lower-priority or single-worker warming is worth a
  later cold-start/input-responsiveness experiment, not the first animation fix.
- One full rendition has a 578.086 ms resize, compared with a 25.260 ms median
  across the five full renditions. It decodes a 2560 x 1920 landscape image at
  full DCT scale for an 800 x 1280 output. Investigate that geometry separately
  if cold loading remains slow. It finished long before the captured transition.
- All four foreground renditions on restart hit the full disk cache, with a
  median lookup/read/decode duration of 239.700 ms. No original decode or
  full-cache rewrite appears for those jobs. This establishes cache use, not a
  matched speedup estimate.
- Process RSS stays at 73,436 KiB, about 71.7 MiB, from the 22-second sample
  through the 34-second sample. CPU ticks advance by only 0.01 CPU seconds in
  that interval. There is no evidence here to prioritize idle-loop work or
  process-memory growth. This short capture cannot rule out GPU memory leaks.

Prioritize texture lifetime/preparation, then presentation attribution and
cache-writer separation. Validate each independently with repeated matched
runs before claiming improvement.
