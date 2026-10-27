# Local performance tracing

The trace answers where photo loading and frame delivery spend time. It is
disabled by default. No service sends it elsewhere. Producers enqueue fixed-size
records without JSON serialization or file writes. A background thread writes
JSON Lines and samples Linux process resources once per second.

## Capture

Build a diagnostic executable with `PF_RENDER_TRACE=1 scripts/build-frame.sh`.
This adds hooks to isolated, checksum-pinned copies of Slint 1.18.1 and
FemtoVG 0.27.0 without modifying the Cargo registry. Normal frame builds default
to `PF_RENDER_TRACE=0` and omit renderer/backend tracing and GPU fence diagnostics.
Frame builds also default to `PF_TEXTURE_PREWARM=1`. This separately pinned
renderer patch prepares the next photo on the render thread and reuses its
texture by pixel-buffer identity. Setting it to `0` disables that renderer
support. The patched renderer explicitly registers support when creating its
texture cache. Cargo feature activation alone does not enable queue admission.
The app-side queue is a no-op in unpatched builds.

Set these variables on a temporary test process using the normal frame service
environment. Give each run a new, absolute output path in an existing writable
directory.

```sh
FRAME_TRACE_OUTPUT=/var/tmp/photoframe-metrics/trace-001.jsonl \
FRAME_TRACE_SECONDS=60 \
/path/to/test/photoframe_proto
```

Capture starts at process initialization, before playlist scanning. The default
duration is 60 seconds, clamped to 1 through 180 seconds. Expiry stops recording
without stopping the application. The file uses create-new semantics. Existing
files are never overwritten. Normal shutdown flushes queued records; an abrupt
kill or I/O failure can leave an incomplete capture. Remove local traces when
the investigation is complete; there is no automatic retention cleanup.

Leave `FRAME_METRICS_OUTPUT` unset for a trace without a rendering notifier.
That older metrics option installs a notifier which adds a pre-render flush.
If using the existing scripted benchmark, explicitly set both metrics and
benchmark options and allow for its 30-second warmup in the trace duration.
Compare runs with identical instrumentation settings.

Desktop builds and builds without the patches still record app events. They
lack the backend stage events. The session header reports
`render_hooks_compiled`; absent stages are unknown, not zero cost.

Analyze a retrieved capture locally.

```sh
python3 scripts/analyze-frame-trace.py /path/to/trace-001.jsonl > /path/to/summary.json
```

The analyzer reports stage distributions, photo durations by event and
rendition, actual transition intervals, and render-start gaps that overlap
transitions. It retains all overlapping gaps, including those longer than 200 ms.
Boundary gaps can include idle
time before a transition starts or after it ends; inspect the recorded
interval endpoints before treating a whole boundary gap as animation time.
Missing summaries, drops, truncation, and incomplete spans make results partial.
Medians average the two central values for even sample counts. P95 uses the
nearest-rank definition.

## Event meaning

Every event has `at_us` from one process-local monotonic origin, a static name,
an optional numeric `op_id`, and bounded fields. Durations use microseconds.
IDs are correlation fields, never aggregate metric dimensions. Photo content,
captions, source paths, settings values, and credentials are not recorded.
Photo indices are local playlist positions and can reveal ordering/activity.

| Events | Authoritative boundary and interpretation |
| --- | --- |
| `photo.playlist.scan`, `photo.playlist.publish` | Scan completion and model publication, with count/fallback. Neither proves pixels are ready. |
| `photo.demand`, `photo.job.claim` | Demand snapshot and worker claim. `demand_id` links the claim to that snapshot. `demand_age_us` is snapshot age, not exact per-rendition queue delay. |
| `photo.cache.ram`, `photo.cache.disk` | Cache lookup outcome for card/full/thumb. Disk lookup includes reading and decoding a stored JPEG. |
| `photo.original.read`, `photo.original.decode`, `photo.fit`, `photo.scrim` | Source read, decode/orientation, output resize/crop, and caption ramp work. `elapsed_us` measures wall duration. |
| `photo.ui.enqueue`, `photo.ui.begin`, `photo.ui.apply`, `photo.ui.reject`, `photo.ui.end` | Worker handoff, event-loop delay, model assignment or stale-result rejection. Assignment is not proof of displayed pixels. |
| `photo.store.*` | Deferred cache work, queue admission/wait, writer completion, encoding, buffered flush, fsync, rename, and pruning. Older captures also include write-lock wait. Encoding includes buffered encoder writes. `photo.store.write` includes nested phases; do not sum it with them. |
| `photo.job.end` | Worker completion. `warm=true` distinguishes background cache work. Warm retry status alone is not proof of a successful cache write. |
| `photo_requested`, `photo_readiness`, `photo_transition_*` | UI-owned requested/displayed indices and start/end/cancel/snap. Transition IDs connect only that transition's boundaries. Home and screensaver are distinct surfaces. Recreating a stage during layout rotation cancels the previous instance's open span. |
| `grid_range`, `grid_slot`, `grid_thumbnail_ready` | Changed wanted/visible ranges, tile assignment, and pixel readiness. These are state changes, not per-frame samples. |
| `render_begin/end`, `scene_begin/end`, `flush_begin/end` | Renderer CPU wall boundaries. Scene work includes texture operations; flush is command submission, not GPU completion. Render ends after EGL swap, before GBM presentation. |
| `texture_alloc_begin/end`, `texture_upload_begin/end` | CPU time in GL allocation/update calls. Upload dimensions and byte count describe pixel payload, not driver allocation or completed GPU transfer. |
| `texture_prewarm` | Prepared texture upload, eviction, and first image-render lookup, estimated texture bytes, and trace-local texture ID when available. Later lookups and deduplication are silent. The operation ID identifies the render frame. |
| `gpu_fence_capability`, `gpu_fence_submitted`, `gpu_fence_poll` | Optional probe support and zero-timeout completion polls. A signaled poll is an upper bound on completion time, not its exact timestamp. |
| `page_flip_wait_begin/end`, `swap_begin/end` | Waiting for the previous submitted page flip, then EGL buffer swap. Both carry the current render's ID. |
| `present_begin/end`, `drm_submit` | GBM front-buffer acquisition/framebuffer creation/submission, then accepted DRM submission. Initial modeset is marked separately. |
| `drm_flip_received` | Receipt of the previous submission's DRM flip event, carrying that submitted frame's ID. `kernel_sequence` and raw `kernel_time_us` come from DRM. Their clock is separate from `at_us`; do not subtract across clocks. Receipt latency can include scheduling or idle delay. |

Renderer IDs link render, swap, submit, and flip receipt. Photo job IDs link
worker stages and UI assignments. Those are separate lifecycles. A trace does
not claim that a specific photo reached the panel on a specific frame.
Initial modeset has no flip-receipt pair. The final submission can remain
without a receipt until another frame waits for the event.
Normal tracing adds no GPU fence or extra flush. Set `FRAME_GPU_FENCE_TRACE=1`
alongside `FRAME_TRACE_OUTPUT` for a diagnostic run. The backend detects
EGL_KHR_fence_sync or supported core GL fences, inserts a fence after render
commands, explicitly flushes, and polls without waiting before/after the prior
DRM flip wait and after swap. At most one fence is live and it is deleted after
those polls. This extra flush can alter scheduling; do not mix probe captures
with normal traces in an A/B comparison. No `glFinish` is used.

New session headers set `gpu_completion_available=null` until runtime capability
is reported, and `gpu_completion_timestamp_available=false`. Older captures use
`gpu_completion_available=false`. Read the capability event rather than treating
unknown as unsupported. Exact GPU completion timestamps remain unavailable.

Linux resource rows contain user/system CPU ticks, RSS in KiB, and minor
faults. The header gives `cpu_ticks_per_second` for CPU conversion. GPU
frequency, temperature, and display runtime power state are not collected.
Record those and the binary hash separately for matched hardware runs.

## Bounds and failure behavior

Texture preparation is latest-only and consumes at most one queued image per
render, outside active photo transitions. The app waits until the current photo
has pixels, and the renderer defers preparation for one render so that the
current image can be presented first. The renderer retains prepared RGB
buffers and textures under a 16 MiB estimated RGBA texture budget. Retained CPU
pixels and driver overhead are additional; this is not a total GPU/process
memory cap. LRU eviction releases old prepared entries. Normal image-element
textures have their own lifetimes. RGB pixel buffers have no shared renderer
cache key. The small byte-bounded prepared set intentionally survives idle
frames so unused next-photo textures are not drained before the transition.
Prewarming targets the next sequential
photo; rapid or reverse navigation can still require an upload during motion.

Full-screen persistence has one writer and at most eight active/queued buffers
within a 24 MiB pixel-buffer budget. Duplicate or over-budget writes are dropped
without dropping display work. Source-version checks still run before writing.
The store owns its writer; admission is recorded before handoff, and cold
persistence is handed off only after the UI publication callback finishes.
Queued writes may be lost on process exit; this cache can be rebuilt. The
existing 128 MiB disk limit and atomic file replacement remain in effect.

The queue holds at most 8,192 records, each with at most 12 fields. Producers
use nonblocking queue submission and count drops if full. The summary reports
written event counts, dropped records, and truncated fields. Static event
names and rendition labels bound aggregation keys. There is no trace worker
when tracing is disabled. I/O failure disables capture and logs one diagnostic;
it does not fail photo loading. The writer flushes at least once per second.
Photo context remains explicit across worker, UI, and writer handoffs.
Only the collector packs fields, so truncation includes composed context fields.

Instrumentation adds work even without a rendering notifier. Compare trace-off
and trace-on runs with the same build and workload before claiming negligible
overhead. A successful build or unit test does not establish on-device timing
accuracy or a performance improvement.

## Checks

```sh
cargo test --offline
cargo test --offline --manifest-path instrumentation/frame-trace/Cargo.toml --features render-hooks
cargo test --offline --manifest-path renderer/texture-prewarm/Cargo.toml --features renderer
python3 scripts/test_analyze_frame_trace.py
cargo metadata --locked --offline --format-version 1 > /tmp/frame-metadata.json
python3 scripts/test-prepare-femtovg-patch.py /tmp/frame-metadata.json
```

The app test suite includes actual JSONL photo-job correlation, stage recreation,
and privacy checks, plus photo-corner pixel checks through both transition slots
and pending-upload cancellation on navigation. Collector tests cover disabled mode, expiry, saturation, file refusal,
and Linux write failure. Patch tests verify source/result checksums, shared
collector dependency paths, and unchanged registry inputs. Analyzer tests
preserve long animation gaps and identify missing boundaries. Frame gaps require
positive overlap with an animation interval; endpoint-only contact does not
classify an idle gap as animation.

The [2026-10-27 hardware validation](frame-trace-validation-2026-10-27.md)
records historical finite capture, frame correlation, and active-capture
shutdown results on the USB-connected frame. It predates the subsequent
stable-slot, texture-prewarm, and cache-writer changes.
