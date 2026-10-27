# October 26 frame performance update

The update reduces repeated full-screen photo processing and resize allocation.
It does not change the transition design or GPU renderer.

## Implementation

- Full-screen images are cached on demand as quality-90 JPEGs under
  `FRAME_THUMB_CACHE/full`. Entries are keyed by source version and both target
  dimensions. The oldest writes are evicted above 128 MiB. The existing grid
  cache remains compatible; an empty `FRAME_THUMB_CACHE` disables both stores.
- Workers post newly rendered images to the UI before encoding and writing
  their full-screen cache entries. Missing, corrupt, or stale entries fall back
  to the source. Original photos are never modified.
- Cover resizing keeps the former Triangle sample coordinates, full-source
  antialias support, accumulation order, and rounding. It writes the retained
  crop directly to the final buffer, using a single float row and filter weights.
  RAM-cache limits and the default two workers remain unchanged.

The resampler's equivalence tests belong in the code because a later filter or
image-library change can alter pixels. The cache format version must change if
stored geometry or encoding changes. Full-screen cache entries are optional;
failures must continue through the original decode path.

## Correctness checks

- All 102 application tests pass on the host.
- All four resampler tests pass natively on the ARMv7 frame. These include 81
  patterned geometry comparisons and independent center, edge, and orientation
  expectations.
- Host comparisons cover five bundled photos, four target sizes, and eight
  rotations/mirrors. All 160 comparisons have zero pixel difference before JPEG
  cache encoding. [Raw comparisons](performance-2026-10-26/host-pixel-comparison.txt).
- JPEG cache round trips on those five photos have mean absolute per-channel
  error of 0.49–1.57 out of 255 and maxima of 13–26. Cached pixels differ from
  the uncached first rendering. [Quality measurements](performance-2026-10-26/jpeg-quality.txt).
- The release ARM build includes the existing pinned FemtoVG fixes and uses the
  existing Cortex-A7/NEON configuration. The installed service configuration,
  user settings, photo library, and thermal policy are preserved.

## Host resize measurements

Five alternating old/new release runs per input, with consumed output checksums
and a counting allocator. The machine was a shared M3 Ultra with 28 logical CPUs.
Results exclude UI delivery, GPU upload, and presentation.

For a decoded 2000×1500 RGB source resized and cropped to 800×1280, median time
was 32.477 ms before and 4.939 ms after. Observed ranges were 30.828–37.117 ms and
4.883–5.100 ms. Peak live allocation during the operation was 47,514,896 bytes
before and 3,119,576 bytes after, including the final 3,072,000-byte output but
excluding the already decoded 9,000,000-byte source. The remaining 47,576 bytes
are filter weights and row storage. These are helper allocations, not device RSS.

Separate sampling located serial CPU resampling work on both sides. It did not
separate arithmetic from memory-bandwidth cost. Full file-read/decode/resize
measurements also favored the new path on the five bundled photos, but neither
host result establishes smoother panel animation.

[Host raw runs](performance-2026-10-26/host-resize-runs.json) and
[host summaries](performance-2026-10-26/host-resize-summary.json) preserve input,
range, allocation, and output-hash evidence. The standalone probe and profiles
are retained locally under `.local/perf/resize`; its dependencies match the
application lockfile. The permanent equivalence suite runs with `cargo test`.

## Device procedure

The USB PF1007L is an Allwinner A33 ARMv7 frame with 1 GiB RAM, an 800×1280 panel,
and Mali-400/Lima graphics. Measurements use `linuxkms-femtovg`, the stock GPU
frequency policy, unchanged photos and settings, and disabled automatic rotation
only during the capture. Each run starts a fresh process in screensaver mode.
The original release is `879bd8b1`; the candidate is `48b37fba`.

Five initial 60-second baseline captures characterized the existing process.
One did not advance beyond photo zero and is excluded from slideshow comparisons.
The comparison uses a separate cold-cache candidate run followed by five
alternating 30-second old/new pairs. Each accepted run must show photo index one.
Both versions use the same 66-photo library with an existing grid thumbnail
store. The candidate's full-screen disk cache is warm after the initial run;
process RAM starts fresh each time. This tests reuse on later visits/restarts,
not the cost of every photo's first cache creation or a complete library loop.

CPU time is the process user-plus-system tick delta at 100 Hz. Peak RSS is the
process high-water mark. Rendering intervals are between Slint callbacks,
not panel scanout timestamps. The transition summary uses photo-one callbacks
100–700 ms after its first callback and excludes intervals over 200 ms to omit
idle gaps. No displayed-frame-rate or first-visible-image latency claim follows
from these intervals.

## Device results

All ten matched runs passed the capture checks, showed a photo advance, and
logged no photo decode/cache errors. Settings-file hashes match before and
after the workload. Temperatures across run receipts were 41.15–42.77 °C.

| Metric per 30-second run | Previous median (range) | Candidate median (range) |
| --- | --- | --- |
| Process CPU seconds | 6.74 (6.70–6.81) | 1.91 (1.89–1.93) |
| Peak RSS, MiB | 131.89 (131.11–132.86) | 71.23 (71.05–73.16) |
| Final RSS, MiB | 67.43 (67.21–68.41) | 66.20 (65.11–66.71) |
| Median transition callback interval, ms | 34.24 (34.14–34.47) | 34.31 (34.09–40.81) |

The warm repeat-view workload uses 71.7% less process CPU time and 46.0% less
peak RSS by the medians. Final resident memory is similar. This measures the
combined cache and sampler changes, not their separate contributions. The
new path avoids source decoding and resampling on disk hits; tests independently
verify that cache hits work even when the original cannot be decoded. The
host allocation/profile measurements support the reduced resize cost on misses.
No device stack profile apportioned the remaining CPU time.

Transition ranges overlap and the new run-to-run range is wider. There is no
demonstrated cadence improvement. Early transition callbacks still include long
intervals. Stable 60 FPS remains unproven; GPU upload/presentation work needs
separate investigation.

The single cold-cache candidate run used 7.10 CPU seconds and peaked at 76.92 MiB.
Its CPU time exceeded the old median while creating disk entries. This is an
explicit first-visit cost, not a cold-load speedup. The new cache held five
full-screen images after these runs; the remaining library will populate on
demand. No claim is made about a complete 66-photo first pass or first-visible
image latency.

[Device summary](performance-2026-10-26/device-summary.json) contains all runs.
[Raw captures, logs, and receipts](performance-2026-10-26/device-captures.tar.gz)
preserve the JSON reports and cold-cache result. The local capture controller
and analysis script are retained under `.claude/perf-20261026`. The analysis
checks successful photo advances rather than relying on the manual capture's
`valid` flag alone. Manual capture starts before the asynchronous playlist scan,
so its initial photo-count fields do not prove library readiness.

## Deployment

The candidate executable SHA-256 is
`48b37fba10eed2fdb98d6268b55ef5517a473f6bca1e7aa7eb02db56ce2419f2`.
It was built from `f2eabcd` plus this uncommitted implementation using the existing
sysroot and pinned renderer patches. A source snapshot, file hashes, build log,
and executable are retained locally under `.claude/perf-20261026`.

The previous release `879bd8b1` is retained for rollback. Deployment replaces
only the `current` release symlink after checksum verification, then verifies
the running process checksum. No service environment, settings, original photo,
kernel, or thermal-policy change is required.


The cutover occurred at 17:34:29 PDT on October 26. The running process hash
matched the staged executable, and the settings hash remained unchanged.
The service started the FemtoVG renderer at 800×1280 and restored its existing
180-degree UI orientation. [Deployment receipt](performance-2026-10-26/deployment-receipt.txt).
The pre-existing weather DNS failure remained present during both baseline and
candidate runs; it is unrelated to photo caching and was not changed here.

A final check at 4 minutes 13 seconds showed the same running PID, zero restarts,
the expected executable hash, unchanged settings, and no benchmark environment
left in production. The normal idle timer dimmed the backlight to 20 percent.
[Final health check](performance-2026-10-26/final-health.txt). The standard local
cross-build output also contains the deployed executable; the previous binary
is retained in the private validation directory as well as on the frame.
