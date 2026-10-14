# Frame performance

The target is 60 FPS, or 16.7 ms between displayed frames. The tested
PF1007L has an A33 Cortex-A7 CPU, Mali-400 GPU, 1 GiB RAM, and an 800×1280
panel. It runs kernel 7.2.10, Slint 1.18.1, LinuxKMS, FemtoVG, GLES 2, and
Mesa Lima. A 64 MiB CMA pool resolved the earlier KMS buffer-allocation
failure. GPU frequency scaling and temperature limits are described in
[the frame setup instructions](frame.md).

## Current changes

Home and Settings persist across tab changes. Home header, photo, weather,
and chores appear without entrance animations. Settings keeps its scroll
position. Hidden Home animation timers stop, and urgent
indicators stop after three pulses.

Entry stagger (`Reveal`) is removed entirely: agenda rows, summary cards,
placeholder screens, and Settings sections now appear instantly. Photo
transitions run 500 ms (was 1000 ms) with no separate caption crossfade
and no animated pager dots; the caption changes with the photo. Past-event
and avatar color changes are instant instead of a 500 ms fade. The Photos
tab backdrop is the solid browse color; the precomputed blur rendition
(`Kind::Blur`, window÷8) and its decode work are deleted, and the
screensaver's loading fallback is now thumb, then card image. These
simplifications are design decisions accepted without A/B captures; no
frame-rate claim is made for them yet.

Stable summary and weather cards use independent caches controlled by
`FRAME_CACHE_DASHBOARD`. The live Header and Chores draw directly.
`FRAME_CACHE_MAIN_CONTENT` controls caches for the Agenda heading and static
rows, navigation button layouts, and Settings headings and static Choice card bodies.
Moving navigation notches and animated Settings outlines remain outside these caches.
Frame builds enable this group by default; desktop builds leave it disabled
unless explicitly enabled. Agenda rows, navigation button layouts, and
Settings heading/card-body caches fall back to direct drawing above 4096
physical pixels on either axis. Check the captured GL limits when changing
hardware.

FemtoVG clears and traverses the full scene on every animated frame.
Cached layers reduce repeated text and shape drawing, but do not preserve
unchanged pixels in the display buffer. PhotoCard draws independently of
the Agenda caches, so a photo transition does not invalidate them. Scrolling
translates cached Agenda rows and Settings headings/card bodies through their
viewport clips. Changes to cached text, colors, or preview geometry
invalidate the affected region.

Photo decode runs on a worker thread. Completed batches are reused at the
same size; interrupted batches can reload. The decode size is the ceiling
of the card's physical pixel dimensions, and completed image geometry
uses unit physical scaling. At most two images draw during a transition.
The caption scrim comes from one embedded SVG, either drawn as an overlay
or fused into decoded photos. The page-colored ring preserves rounded
corners around the rectangular photo clip. Four
procedural corner paths and a cached procedural gradient produced visual
errors on the actual GPU, so their fast measurements are not acceptance
evidence. A later tiny SVG corner-mask experiment was also rolled back;
its unused assets were removed. Complex cards use pressed colors or leaf
opacity. Navigation icon tint changes immediately.

The Photos grid uses a fixed pool of tile slots. Slot `j` shows the photo
congruent to `j` modulo the pool size, so scrolling one row reassigns only
that row's slots. Images built from pixel buffers have no shared FemtoVG
texture cache key, so every reassigned tile uploads a new texture. Tiles
are not clipped: a rounded `clip` in Slint 1.18.1 FemtoVG renders the tile
through an offscreen layer. A browse-colored `RoundedCornerMask` ring
covers the corners instead. Decode results are accepted while their slot
is still wanted; a demand change, such as a scrolled row, no longer
discards an in-progress decode. The last grid range stays resident off the
Photos tab and during the screensaver, and grid thumbnails use their own
128-entry, 28 MiB cache. These changes are source-supported; no frame
capture of the Photos tab exists yet.

Scroll motion has no simulation timer. The offset binding samples
closed-form exponential coast and damped spring motion from
`animation-tick()`. The existing 16 ms `Physics.step` calibrates friction;
it is not a frame interval or catch-up step. Retargeting carries the sampled
position and velocity. Rest, hidden Home, and Motion Off stop clock reads;
hidden/Off also clamp overscroll. The continuous coast travels about 1.8%
less than the former 16 ms Euler integration at the same release velocity.
The fixed stiffness and damping select an underdamped spring.

Frame builds now default to `FRAME_PHOTO_SCRIM=baked`. It composites the
canonical SVG ramp into the opaque RGB photo buffers during decode and hides
the UI overlay only after a baked batch starts publishing. Use
`FRAME_PHOTO_SCRIM=svg` to compare the overlay; desktop builds retain that
default. A failed ramp rasterization preserves the previous
published batch and permits another request at the same size. Six cache-state
tests and two host SVG/compositor tests passed. Actual GPU dual readback
found the upper photo pixels 100% identical. In the caption region, 80.75%
of channels were identical; maximum error was 2/255 and mean error was
0.1938 channel units. The ring's corners were also verified. No duplicate
gradient stops or colors are introduced by baking.

Slides are the default in all builds; the Fade transition is selected
in the Settings tab. Production release
`b27b4b6f` includes these defaults; checkpoint `9e34868` remains a rollback. The matched slide workload and GPU appearance checks passed;
its cadence still falls short of stable 60 FPS.

## Reproduce a capture

Use a release build, the same five demo photos, settings, display mode,
GPU policy, and thermal conditions for both runs. Record the source
revision and local diff, binary hash, kernel and Mesa versions, GPU
frequency, temperature, and cache/transition options alongside the JSON.
Keep source snapshots, binaries, logs, receipts, and captures in a private
directory outside the checkout and application installation.

On the frame, create an external capture directory, then launch:

```sh
install -d /var/tmp/photoframe-metrics
SLINT_BACKEND=linuxkms-femtovg \
GALLIUM_DRIVER=lima \
FRAME_PHOTOS=/opt/photoframe-proto/assets/photos \
FRAME_METRICS_OUTPUT=/var/tmp/photoframe-metrics/run.json \
FRAME_BENCHMARK=1 \
FRAME_AUTO_ROTATE=0 \
/opt/photoframe-proto/photoframe_proto
```

The benchmark waits 30 seconds, resets the photo index, and captures a
60-second workload of photo changes and Home/Settings round trips.
Automatic slideshow changes pause during this workload. Disable sensor
rotation to keep the layout fixed across matched runs. The capture is
invalid if the playlist row count does not match the discovered photo count
or the slideshow did not pause. Rows contain metadata before their lazy image
decode completes; this check alone does not prove every image is ready.
Check the visible photos and decode errors before interpreting a capture.
Check `valid`, `invalid_reason`, `initial_photo_count`, and
`automatic_slideshow_paused` before comparing results. Use a new output
filename for each run; the app refuses to overwrite an existing capture.

For cache A/B tests, use the same executable with
`FRAME_CACHE_MAIN_CONTENT=0` and `FRAME_CACHE_MAIN_CONTENT=1`. The separate
`FRAME_CACHE_DASHBOARD=0` option disables the earlier card caches.
Use the same Photo transition setting on both sides of a cache
comparison; the default is Slide. Alternate or
repeat matched runs when temperature, CPU frequency, or other load drifts.
Compare `FRAME_PHOTO_SCRIM=baked` separately against `FRAME_PHOTO_SCRIM=svg`
with the same photos, transition, settings, GPU policy, and temperature.

## Interpret the metrics

`renderer_callback_wall_us` measures wall time from Slint's
`BeforeRendering` to `AfterRendering` callbacks. It includes UI traversal,
command generation, submission, and any blocking within that interval.
It is neither CPU time nor GPU completion time. It excludes the
pre-callback clear and later presentation. Installing the notifier also
adds a flush before the callback; keep that instrumentation the same on
both sides of a comparison.

`start_interval_us` measures time between `BeforeRendering` callbacks,
including presentation waits and idle gaps. It does not timestamp a panel
scanout. Report cadence within animation bursts; treating idle gaps as
slow frames would misrepresent a dashboard that sleeps between updates.
For the 60 FPS target, count intervals at or below 16,667 µs within those
bursts and report dispersion, rather than relying on average FPS alone.

Process user/system ticks provide CPU consumption over the capture window.
Convert their sum using the frame's `getconf CLK_TCK`. RSS and peak RSS are
memory measures; peak RSS includes warmup. Captures also report display
dimensions, scale factor, photo-card/image dimensions, and queried
`GL_MAX_TEXTURE_SIZE` and `GL_MAX_RENDERBUFFER_SIZE` when available.

## Measurements so far

These measurements are from the actual frame. The earlier workload
comparisons exclude warmup and use the 60-second capture window.

| Condition | Baseline | Optimized | Interpretation |
|---|---:|---:|---|
| Matched GPU at 144 MHz, process CPU seconds | 25–29 s | 3.3–3.4 s | Less CPU work for the measured workload. |
| Matched GPU at 144 MHz, animation cadence | Roughly 10 FPS | Roughly 15 FPS | Below the 60 FPS target. |
| Restored normal GPU setting, 384 MHz during photo animation | Not a matched baseline | About 29 FPS | Cannot attribute the difference solely to app changes. |

An AA-patch-only probe did not improve cadence before the physical image
geometry correction. That result does not establish the effect of the
final combination. Checkpoint `9e34868` with the verified ring-and-SVG render
runs at about 28–29 FPS under the normal GPU policy. The broken
corner-path/gradient build reached about 57–58 FPS, but those figures are
excluded from accepted performance
results.

Five valid matched 60-second captures tested the follow-up cache boundaries
and baked ramps with fades. Both cache groups should remain enabled:

| Scrim | Main cache | Dashboard cache | CPU seconds | Photo interval | Home-return interval |
|---|---|---|---:|---:|---:|
| SVG | On | On | 5.34 s | About 34 ms | About 34 ms |
| Baked | On | On | 6.02 s | About 34 ms | Median 17.06 ms; p90 33.02 ms |
| Baked | Off | On | — | — | About 34 ms |
| Baked | Off | Off | — | — | About 34 ms |
| Baked | On | Off | — | — | About 34 ms |

After the first 100 ms of Home returns, the baked case with both cache
groups enabled had a 17.02 ms median and 17.46 ms p90, roughly 59 FPS.
This is one run per condition. Baking did not improve the photo-fade
cadence and used more CPU seconds in this workload. Settings remained near
34 ms between callbacks; callback wall time was about 15 ms with the main
cache and 24 ms without it.

A valid matched slide capture contained 403 rendered frames. With baked
ramps and both cache groups enabled, its four 1.1-second photo windows
compared with the fade case as follows:

| Transition | Photo-window frames | Interval median / p90 | CPU seconds per 60 s | RSS |
|---|---:|---:|---:|---:|
| Fade | 119 | 34.04 / 34.87 ms | 6.02 s | 55.28 MiB |
| Slide | 188 | 17.12 / 33.92 ms | 7.29 s | 56.14 MiB |

Slides produced 58% more frames in those windows. After the first 100 ms,
the slide median/p90 was 17.12/33.82 ms: about 75% of intervals were at most
18 ms, while about a quarter exceeded 25 ms. The first slide burst was
slower at 25.86/34.16 ms; later medians were 17.04–17.12 ms. More rendered
frames increased CPU consumption. Home returns remained near 17 ms and
Settings near 34 ms. These matched captures support the slide default,
but do not establish stable 60 FPS.

A temporary backend timing probe collected 880 warmed samples. Its
reported stage timings were:

| Screen | Scene submission | Previous DRM page-flip wait | EGL swap | Total frame |
|---|---:|---:|---:|---:|
| Home before the final visual repair | 3.745 ms | 29.096 ms | 0.720 ms | 33.859 ms |
| Settings | 1.276 ms | 14.918 ms | 0.439 ms | 16.861 ms |

The Home submission interval is already below 16.7 ms; its measured
page-flip wait is longer. This separates the observed wait from CPU
submission cost, but does not establish whether GPU completion, buffering,
or scheduling causes the wait. The temporary probe is separate from the
public app callback metrics and is not included in the production build.

## Idle power and thermal policy

The restored GPU table contains 144, 240, and 384 MHz, selected by
`simple_ondemand`. The frame's GPU runtime-suspends when idle, so forcing a
fixed maximum clock is unnecessary for an unchanged dashboard. The
temperature service applies the stock 85/95 °C frequency ceilings with
2 °C hysteresis and a 144 MHz fallback on invalid readings or service stop.
Measured live chip temperatures during this work were 46–50 °C. No
electrical power measurement or battery-life estimate has been made.
The guard now warns on invalid readings and service exit, reports recovery,
and updates systemd's status during sensor fallback/recovery. Its 144 MHz
fallback policy is unchanged. The thermal harness passed 22 cases. See
[inactive-guard checks](frame.md#restore-gpu-scaling-and-temperature-limits)
for the condition result, actual ceiling, and journal checks.

## Frame renderer correction

`scripts/build-frame.sh` applies
[the pinned one-line patch](../patches/slint-1.18.1-image-aa.patch) to an
external Slint 1.18.1 copy by default. Set `PF_FEMTOVG_IMAGE_AA_FIX=0` for
an explicit comparison without it. The original condition compares the
vertical image scale to zero, so a unit-scale image still requests edge
anti-aliasing. Comparing it to one permits FemtoVG's simple texture-copy
path when the other conditions also hold. This is a source-supported
mechanism, not a measured speedup claim.

The helper pins `itemrenderer.rs` SHA-256 to
`30ee92fa6c8ce12c21fb7135f3696677131bb252d9ba34b7a5e0079687e3d82b` and fails
if the source or patch differs. It copies app build inputs and the renderer
outside the checkout, adds a local Cargo patch, and updates only the copy's
lockfile. It leaves the original manifest, lockfile, and Cargo registry
unchanged. See [the build command](frame.md#cross-build-on-linux-or-wsl).

## Follow-up: Settings static card bodies

Measured on the frame with the same temporary timing backend, stock GPU
scaling, FullSpeed workload, and unchanged settings: keeping each Choice
card body cached and its animated outline outside the cache reduced the
warmed median frame from 33.84 to 16.85 ms. Actual GPU readbacks before
and after retained the same layout, labels, previews, and control geometry.

| Settings variant | Scene submission | Page-flip wait | Total frame |
|---|---:|---:|---:|
| Headings only | 15.72 ms | 16.16 ms | 33.84 ms |
| Font text caches | 13.77 ms | 18.16 ms | 33.84 ms |
| Static Choice bodies, separate outlines | 11.65 ms | 3.62 ms | 16.85 ms |

Each row uses 480 warmed samples after discarding 120. FullSpeed console
readings reached 58–59 FPS after warmup; intermediate seconds varied.
The card-body run consumed 32.19 CPU seconds over 36.06 wall seconds,
versus 24.20 over 35.27 for headings only: it rendered more frames, so this
is not a lower CPU-per-second claim. A pressed card can invalidate its
body once; the outline tween does not rerender that body every frame.

Measured through DRM debugfs with the production app active: the selected
mode is 800x1280, pixel clock 67,000 kHz, totals 854 by 1314. That gives
59.71 Hz, rather than a 30 Hz mode. Home's earlier 34 ms intervals therefore
span two refresh periods; the wait still does not isolate GPU execution
from buffering or scheduling. No CPU frequency-policy directory was
available on this running kernel, so no CPU governor change was applied.

## Idle backlight

Frame source now dims to 20% after 180 seconds without human input and
restores 65% on input. It uses one idle deadline, optional night-boundary
deadlines, blocking independent input readers, and a coalescing backlight
writer. Automatic slides run every two minutes only with Full motion;
night windows are opt-in. Clock refreshes now align to minute boundaries
and unchanged nested rows keep their model identities. These changes
reduce scheduled work; electrical power and heat savings are unmeasured.

Measured device backlight checks used a five-second test timeout and
private settings. Brightness readbacks were 65%, 20%, 65% after an injected
center touch, then 20% after another idle period and 65% after SIGTERM.
The first hardware run exposed fragmented formatted sysfs writes; sending
one complete numeric buffer fixed the error. The host suite passes all
36 tests, including the sysfs write-protocol regression.

Measured opt-in night checks read 10% after idle, 65% after touch, then
10% after another idle period. Killing that temporary process with
SIGKILL ran the systemd stop helper and restored 65%. Production release
`b27b4b6f` uses the normal 180-second timeout and leaves night hours unset.
The validated binary was transferred over USB serial when the frame
changed Wi-Fi networks; USB console access remained available.

The device check also exposed a service conflict: starting the app reran
the completed hardware setup, whose console-login action then stopped the
app. The corrected oneshot boot unit retains its successful state; the app
drop-in suppresses that login action and the thermal unit waits for hardware
completion. Earlier timing captures did not separately record boot-helper
activity, so they do not isolate its possible scheduling impact.

Measured after the lifecycle correction: the production process remained
running beyond the earlier console takeover, and the normal idle deadline
changed brightness from 65% to 20% exactly 180.0006 seconds after controller
startup. This test used the powered frame; a cold power cycle was not run.

A fresh scripted capture of production `b27b4b6f` ran after hardware setup
was `active (exited)` throughout. It was valid, had five loaded photos,
484 callbacks in the 60-second capture, and used the installed slide/baked
defaults. Settings were copied to a private file; the persistent app resumed
automatically afterward.

| Warm window, excluding first 100 ms | Median interval | P90 interval | Intervals above 25 ms |
|---|---:|---:|---:|
| Four photo slides | 17.08 ms | 32.51 ms | 13.2% |
| Settings entry/returns | 17.01 ms | 17.39 ms | 2.2% |
| Home returns | 17.06 ms | 17.51 ms | 6.7% |

The last two photo slides each had a roughly 17 ms median and P90; the
first two still had longer intervals. Process CPU usage increased by
7.87 seconds over the 60-second capture. Peak RSS was 79.34 MiB across
warmup and capture; final RSS was 54.82 MiB. These results describe the
combined production changes on a quiescent boot, not an isolated speedup
from any one change. Stable 60 FPS across every transition remains unproven.
