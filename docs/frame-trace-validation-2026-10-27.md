# Frame trace validation, 2026-10-27

Implemented with GPT-6.1 Terra high and GPT-6.1 Sol high. The historical ARMv7
executable validated in this report has SHA-256
`f163b66a39439855212cb97573507d24732b3250526d9d2e1b9e779f06f70e24`.
It predates the subsequent stable-slot, texture-prewarm, and cache-writer
changes. These results do not validate the current source on the device.

The USB-connected PF1007L ran this executable temporarily with its 66-photo
playlist, copied settings, and a private cold cache. No rendering notifier was
installed. The service and release link were restored to `48b37fba` after the
checks. This validation did not deploy the new executable as the installed
release.

| Check | Measured result |
| --- | --- |
| Finite capture | 35.000331 seconds, 1,267 events, 35 resource samples |
| Dropped records / truncated fields | 0 / 0 |
| DRM submissions / flip receipts | 24 / 22 |
| Frame correlation | Every submission shared its ID with render, swap, and present stages; every received flip matched an earlier non-initial submission |
| Photo work | 71 job claims and completions, including 66 background thumbnail jobs; 5 full renditions assigned to the UI |
| Transition boundaries | One complete screensaver transition, 505.899 ms; no incomplete stage spans |
| Shutdown during active capture | SIGTERM ended a requested 60-second capture after 3.987958 seconds; 498 events and final summary flushed; process exited successfully |
| Shutdown capture loss | 0 dropped records, 0 truncated fields |

Initial modeset and the final submission account for the two submissions
without flip receipts. The backend consumes the final flip event only when a
later frame waits for it.

An initial test pointed to an absent bundled-photo directory. Its scan reported
zero photos, so it was rejected for photo/animation validation. The repeated
run above used the actual playlist and passed those checks. Empty metadata
publication is therefore not treated as evidence of image readiness.

Host verification passed all 105 application tests, 5 analyzer tests, and 8
pinned-patch preparation tests. Collector tests passed on macOS and Linux,
including queue saturation, expiry, existing-file refusal, and Linux write
failure. The ARM release build and formatting/diff checks passed. Existing
upstream compiler warnings remain.

## Review follow-up verification

The later worktree received parallel read-only reviews from Copilot
GPT-6.1 Terra high, Copilot GPT-6.1 Sol high, and Muse Wonder Full 1.0 xhigh.
Fixes made after those reviews passed 112 application tests, four
renderer-handoff tests, the collector suite, six analyzer tests, and ten
patch-composition tests. Production and explicitly trace-enabled ARMv7
release builds passed. Source formatting and diff checks passed; raw terminal
receipts and unified patches retain their original whitespace. Clippy completed
with existing application warnings.

Regression checks cover all four Home photo corners through both fade
directions, stale preparation cancellation, renderer support registration,
rotation during a transition, endpoint-only animation overlap, writer event
ordering, and collector accounting for composed-field truncation. UI trace
registration now belongs to photo diagnostics, independently of metrics.
These checks do not establish current GPU-cache behavior, hardware appearance,
or device performance. The reviewers inspected earlier mutable snapshots;
their reports are not re-approvals of the final source.

## What the first trace suggests measuring next

Across these 24 frames, median scene wall time was 2.519 ms, command flush
3.616 ms, previous-page-flip wait 24.925 ms, and EGL swap 1.075 ms. These are
separate stage medians, not additive estimates of a median frame. Texture
allocation reached 78.684 ms and upload reached 55.308 ms during this cold run.

These observations make display waits and first-use texture work useful next
investigation targets. They do not establish GPU execution time, scanout
latency, or the cause of a wait. One short capture with cold background cache
work is not a performance benchmark. Trace-on versus trace-off overhead,
repeatability, grid scrolling, and sustained thermal behavior remain unmeasured.

See [capture semantics and commands](frame-trace.md). Raw captures, receipts,
the executable, source hashes, and the correlation check are retained locally
under the gitignored `.claude/trace-20261027/` directory.
