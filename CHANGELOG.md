# Changelog

## Unreleased

- Require successful hardware setup before starting the dashboard, so
  recovery kernels retain USB diagnostics without repeated DRM failures.
- Rotate the frame dashboard and touch coordinates from calibrated SC7A20
  gravity readings. Require a stable pose, defer changes during touch,
  retain the last orientation on uncertain readings, and wake brightness
  on a rotation. Keep native DRM mode and USB serial access unchanged.
- Preserve upright font weight in all four orientations with a pinned
  FemtoVG quarter-turn glyph-atlas correction. Enable it by default for
  frame builds, independently of the image anti-aliasing correction.

### Real photos, Photos tab, screensaver

- Keep the displayed photo until the requested image is decoded, including
  rapid swipes and wrapping. Prioritize full-screen loading and pause hidden
  Home-card prefetch and backdrop work while the slideshow is open.
- Show JPEGs from `FRAME_PHOTOS` instead of the Unsplash placeholders
  (`assets/photos` remains the empty-folder fallback). Decode only a bounded
  window around the current photo.
- Photos tab: blurred backdrop, thumbnail grid, "Start slideshow".
- Screensaver after `FRAME_SCREENSAVER_IDLE` seconds (default 120) or from
  the Photos tab; any tap closes it. Full-screen photos have no bottom
  gradient; the optional night variant dims the entire photo.
- Keep slides and motion policy in both photo views; retain rotation and
  touch calibration while browsing and showing full-screen photos.
- Scan metadata off the UI thread, virtualize grid tiles, bound decode
  allocations, and reject obsolete image results. Keep actual transition
  images until their animation ends, including rapid photo jumps.
- Use bundled photos from the installed release when the configured folder
  is empty. Prevent queued raw touch events from closing a slideshow opened
  by that same tap; keep brightness dimming and benchmark policy intact.
- Expand landscape navigation into unused edge padding, increasing the
  Home target from 80×66 to 88×94 while retaining icon and notch positions.
- Navigate full-screen photos with horizontal swipes. Swipe left for next,
  right for previous, and tap to exit. Wrap at the collection boundaries
  and preserve transition direction without treating drags as taps.

### Follow-up after checkpoint 9e34868

- Coordinate app startup with the one-time hardware boot service. Suppress
  its console-login action while the app owns tty1, so app restarts cannot
  rerun initialization and stop the dashboard.
- Dim frame brightness from 65% to 20% after three minutes without touch,
  key, or mouse activity. Restore 65% on interaction and service stop.
  Keep the screen on; optional night hours use 10% while idle.
- Advance photos every two minutes only with Full motion; pause automatic
  advances in configured night windows.
- Align clock updates to minute boundaries and retain unchanged nested
  calendar/chore models. Bound weather requests to 20 seconds and 64 KiB.
- Save settings off the UI thread, coalesce changes for 250 ms, and replace
  JSON atomically. Graceful service stops flush pending saves.
- Apply JPEG EXIF orientation, reduce decode copies, and reuse exact-size
  thumbnails in a cache capped at 16 entries and 16 MiB.

- Keep moving navigation notches, the live Header and Chores, and Settings
  animated outlines outside caches. Cache navigation button layouts, Settings
  headings/card bodies, and static Agenda rows.
- Sample closed-form scroll coast and spring motion at animation frames;
  remove the scroll timer and preserve hidden/Off clamping.
- Enable the pinned Slint 1.18.1 image anti-aliasing correction in frame
  builds by default; `PF_FEMTOVG_IMAGE_AA_FIX=0` opts out.
- Bake the shared SVG caption ramp into decoded photos by default in frame
  builds; `FRAME_PHOTO_SCRIM=svg` opts out. GPU readback showed identical
  upper pixels and at most 2/255 channel difference in the caption region.
- Default photo transitions to slides in all builds;
  `FRAME_PHOTO_TRANSITION=fade` selects crossfades.
- Log GPU thermal fallback, recovery, and exit warnings; retain the 144 MHz
  fallback ceiling. The thermal harness passed 22 cases.
- Matched fade captures reached roughly 59 FPS after the first 100 ms of
  Home returns with both cache groups and baked ramps. Photo fades and
  Settings stayed near 29 FPS. Matched slides improved the photo-window
  median from 34.04 to 17.12 ms and rendered 58% more frames; about a quarter
  of warmed intervals still exceeded 25 ms. Stable 60 FPS is not
  demonstrated. Verified production release `b27b4b6f` replaces checkpoint
  `9e34868`; its binary SHA-256 is recorded in the frame setup guide.

### Checkpoint 9e34868

- Retain Home and Settings across navigation; show the main Home content
  immediately and preserve Settings scroll position.
- Cache stable dashboard, navigation, and scroll content; use elapsed-time
  scroll physics, bounded urgent pulses, and lighter press feedback.
- Reuse completed photo decodes at physical pixel sizes. Use an embedded
  SVG caption scrim and restore correct rounded photo corners on Lima.
- Add external frame benchmark captures and an opt-in, pinned Slint image
  anti-aliasing patch. The verified render runs at about 28–29 FPS under
  the normal GPU policy; 60 FPS is not demonstrated.
- Document 64 MiB CMA, restored stock GPU scaling with temperature limits,
  persistent systemd startup, and verified SC7A20 raw IIO readings.
  Four-pose calibration and automatic rotation remain pending.
