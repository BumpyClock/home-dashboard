# Calendar sync validation

## Implemented boundary

`host-sync/` is the parent Rust application. `host-sync/calendar-sync/` owns calendar-specific providers and the shared snapshot model. The frame reads host and optional direct snapshots, merges them, and populates its existing Home agenda, tomorrow and next-up models. Direct Google authentication/fetching is not included.

## Observed checks

- macOS. `cargo test -p photoframe_proto` passed all 116 tests. This includes a headless Slint test loading separate direct/host files, choosing the direct copy of a duplicate, showing all-day and tomorrow rows, retaining direct data when host data clears, and distinguishing not-synced from a valid empty calendar.
- macOS. After the live-envelope fix, `bash scripts/check-host-sync.sh` passed 14 host/calendar tests and strict host Clippy checks. Provider tests execute fixture CLI processes, not live calendar services. The new fixture reproduces the observed status-prefix/JSON/correlation envelope without including account or meeting data. Tests cover rejected ambiguous/truncated results, valid empty results and query boundaries across the November DST change. `cargo test -p photoframe_proto calendar` also passed all four calendar/UI tests.
- macOS. `python3 scripts/test-calendar-sync.py` passed host preview to real frame receiver over subprocess pipes, persisted 80 events, replaced them with an empty snapshot, and rejected an invalid source without changing stored data. The serialport macOS baud ioctl rejects PTYs with `Not a typewriter`, so this is not a macOS serial-port test.
- Linux. `bash scripts/check-host-sync.sh --with-frame` passed inside a fresh secret-free Boat sandbox. It passed the 12 host/calendar tests, strict host Clippy checks, three frame calendar tests, and the real Rust host serial connection through a simulated login console to the real Rust frame receiver. The PTY test persisted 80 events, accepted an empty replacement, and retained stored data after source failure. The sandbox was deleted after the run.
- Linux follow-up. After the live-envelope and query-window fixes, `bash scripts/check-host-sync.sh` passed all 14 host/calendar tests and strict Clippy checks in another fresh secret-free Boat sandbox. This run used synthetic fixtures only and did not receive work-calendar captures. The sandbox was deleted after the run.
- Windows target. `cargo check --locked -p frame-host-sync --target x86_64-pc-windows-gnu` passed in both Linux sandbox runs, including the new time-zone dependency. This checks target-specific code and dependencies. It does not produce or run a Windows executable, exercise npm shims, or verify serial drivers.
- `cargo clippy -p photoframe_proto --all-targets` completed. Existing warnings remain in the screensaver guard and a weather test. Host and calendar crates pass Clippy with `-D warnings`.

The first Linux check exposed an existing frame-test requirement for Slint's software renderer when default features are disabled. The check script now enables `slint/renderer-software` for headless frame tests. The subsequent sandbox run passed.

## Live source follow-up

The initial check was blocked by mcporter's transport capacity limit. A subsequent user-authorized `mcporter daemon restart` cleared that limit. WorkIQ and Microsoft Calendar schema discovery now succeeds.

The first connected previews exposed a second problem. WorkIQ returned `complete:false`, and the Calendar MCP adapter assumed pure JSON text. Actual `ListCalendarView` output has a text block beginning `Calendar view retrieved successfully.` followed by Graph JSON, plus a separate correlation/timestamp text block. The adapter now normalizes this known envelope, prefers `structuredContent` when present and rejects multiple payloads rather than guessing. It does not ask WorkIQ to turn prose into calendar records.

A direct live read requested November 1 through November 8, 2026, using local midnight boundaries in America/Los_Angeles. The start offset was -07:00 and the end offset -08:00 because DST ended within the window. The request selected only IDs, UID, subject, times, all-day/cancellation flags and location, with UTC event timestamps and chronological ordering.

The fixed host app's `calendar preview --config host-sync/examples/calendar-mcp.json` succeeded with 78 events, including 11 all-day events. Every normalized occurrence ID, UID, title, location, timed instant and all-day date matched the direct MCP response after cancellation filtering. The resulting snapshot passed wire-schema validation. Raw live captures and the private snapshot remain in gitignored `.local/calendar-sync/`; only synthetic test fixtures are checked in.

Live Calendar MCP ingestion through the host app is now verified. Calendar MCP is the recommended work-calendar source. WorkIQ remains optional and model-generated, and `complete:false` still fails instead of clearing data. WorkIQ is unavailable as a standalone executable on this Mac. That preview changed no authentication, MCP configuration, device deployment, or getty/service state. Physical deployment was subsequently authorized and completed below.

## Physical frame deployment

The first calendar-capable deployment used release `d8fa1002`, executable SHA-256 `d8fa10023c3f8ebde6f9727706a2ee0209bb46d1b1bdf1da2e9ff11afda9ee72`. Previous release `00d5c7b2` remains untouched for rollback. The service uses a calendar-only drop-in pointing to `/home/diag/.local/share/family-frame/calendar-host.json`; its existing unit and photos/settings remain intact. Startup reported FemtoVG, 800x1280 and active/running service state.

Physical integration exposed two console details not covered by the initial simulation. Shell OSC metadata can precede receiver output without a newline, so the host now separates the READY line explicitly. Bulk JSON writes were rejected despite valid source data, while checksum-verified file import succeeded. Pacing serial writes in 2048-byte chunks with a 3 ms gap, matching the established console uploader, then produced successful direct Rust host syncs. The Linux regression now runs the actual console wrapper through a shell with an OSC preamble, rather than manually synthesizing the receiver handshake.

The physical frame acknowledged 78 events from live Calendar MCP data. Reading back the on-device snapshot verified the complete source/event data and a fresh `generated_at` of `2026-11-01T19:26:16.434556Z`. The dashboard remained active on the selected release. A background host watch is running with a 300-second interval; private configuration, process/log files and the deployment receipt are under gitignored `.local/calendar-sync/`. This is a running process, not an installed login/startup service.

The ARM build helper now copies the nested host/calendar workspace into its isolated build inputs. All 11 preparation tests passed, including a regression for those crates. The real-shell serial regression also passed on Linux. No firmware, kernel, network or getty configuration was changed.

## Calendar tab and Tomorrow preview

The Calendar UI deployment used release `3153b4f1`, executable SHA-256 `3153b4f126fb878af779d3d3243bab1be60b582dd8e90d10b398d0ac9d9012e3`. Release `d8fa1002` remains available for rollback. The actual frame service is active, renders with FemtoVG at 800x1280, and has a fresh 78-event snapshot after the host watch resumed. Device readback contains 16 timed events tomorrow; the Home preview selects the first three without including all-day entries. The host watch continues every 300 seconds.

The user selected an upcoming agenda rather than a month or week grid. The implementation inherits the existing typography, palettes, spacing and scroll physics. It groups seven local days, keeps all-day events in the full Calendar view, filters ended meetings, distinguishes current meetings, wraps long titles/locations and displays the latest input sync timestamp. The Today control supports touch, keyboard and a native accessibility action. Scroll position survives tab changes; changing inputs does not reset it.

`cargo test --locked -p photoframe_proto` passed all 121 tests. Coverage includes local-day and repeated-DST-hour behavior, unique counts for multi-day events, real snapshot reloads, duplicate removal and empty/not-synced states. Tomorrow preview regression data places three all-day entries before a timed meeting and separately checks an all-day-only tomorrow. Calendar input tests exercise Page Down/Home, mouse wheel, touch drag, Return on the Today control and retained position across tabs with motion disabled.

Actual Slint software-renderer captures cover 800x1280 portrait, 1280x800 landscape, Night with 125% text and 560x800 with 125% text. Each also captures empty and not-synced states. Native OCR checks confirmed readable wrapped titles and locations; contrast tests cover primary/secondary text, the Today heading and focus indication in all five palettes. Inline image preview was unavailable in this session, so a subjective image-based review and physical finger-gesture inspection are not claimed. The device build and deployment were verified directly over USB, not through Boat.

To reproduce render artifacts with synthetic, explicitly labelled data:

```sh
FRAME_CALENDAR_RENDER_DIR=.local/calendar-ui cargo test -p photoframe_proto calendar_render_tests
```

Artifacts and native OCR results are in gitignored `.local/calendar-ui/`; the physical deployment receipt is `.local/calendar-sync/calendar-ui-deployment-receipt.json`. No work-calendar titles are included in committed test fixtures or images.

## Host-lock privacy

The user selected fail-closed behaviour for sleep/disconnect and explicitly authorized deployment with the new option enabled. The connected frame now runs release `4bf124b2`, executable SHA-256 `4bf124b2eb3f44370e0f20a4f77673bafe9aa844be81fffda59feb5da1ccde07`. The previous executable release `3153b4f1` remains available, and existing settings were backed up before enabling privacy. The frame service is active. Its startup journal confirms `[calendar] Host data hidden by privacy setting`.

The Mac's native session probe reports `locked`. The updated Rust watcher sent that actual state over USB; device readback confirmed a fresh `locked` packet in `calendar-host.host-state.json`. Settings readback confirmed `hide_host_calendar_when_locked:true`. The host monitor was resumed and its first fresh 78-event calendar update succeeded. Calendar content remains cached while hidden. The private deployment receipt is `.local/calendar-sync/privacy-deployment-receipt.json`.

`cargo test --locked -p photoframe_proto` passed 125 tests. New coverage checks opt-in settings roundtrip, hiding host-only data from all agenda models/next-up text, preserving a direct duplicate, unlock restoration without fetching again, corrupt/unknown status, monotonic expiry and ignoring persisted unlock permission on restart. The Slint renderer exercises the new checkbox through touch and Space at 125% text, plus distinct locked/empty/not-synced calendar states. Native OCR identified a misaligned On indicator during iteration; it was corrected by centering the indicator through layout rather than a manual offset. Generated captures remain private under `.local/calendar-ui/`.

Host/calendar/shared-state checks passed on macOS and Linux, including strict Clippy for those crates. The Linux PTY test observed two lock heartbeats during an 18-second blocked fixture-provider fetch, demonstrating that provider I/O does not occupy the serial owner's loop. Windows target checking passed with its native WTS implementation. Linux/macOS parser tests cover active-session requirements and unknown state. Live Windows/Linux desktop lock transitions and a physical macOS unlock transition have not been performed; no user session was unlocked or locked by the agent. Restore/hide transitions are covered with synthetic protocol input, not a bypass of the user's session security.

The native status protocol lives in the separate `host-state` crate and never replaces calendar records. It uses a frame-local 20-second monotonic permission lease; old status on disk never grants visibility at startup. The feature is display privacy, not encryption or deletion. The host watcher must remain running. Native sensor and serial polling add delay to confirmed lock transitions.

## Review corrections

The follow-up addresses three reproduced review findings. Home and Calendar now use the same UTC instant for scheduling, and local event times are derived for presentation rather than stored as a second scheduling model. Regression fixtures cover both DST transitions, ended-event filtering, ordering, countdowns and exact end boundaries. A headless Slint test exercises both tabs through the repeated hour with one fixed clock sample. A separate isolated-process test preserves `FRAME_NOW` preview behavior for both surfaces without changing other tests' global time zone.

Deduplication now indexes known identities and all aliases before matching records without UIDs. It retains all contributing sources independently of display precedence. Regressions cover a direct event plus two distinct same-source host events, and every ordering of three providers with renamed duplicate records and revised end times.

The shared `host-command` crate replaces separate provider/probe subprocess implementations. A single deadline includes child execution and both output pipes. Owned Unix process groups or Windows Job Objects are terminated on success and failure, with bounded leader-reaping cleanup and no reader joins. Tests exercise inherited stdout/stderr after parent exit, a running child after both pipes close, per-pipe overflow, nonzero exits, and descendant cleanup after success and timeout. A native probe-wrapper test verifies that incomplete output returns no trusted result within its deadline.

After these corrections:

- macOS. The full frame suite passed 129 tests, including the new DST and `FRAME_NOW` UI regressions. All 27 host/calendar/shared tests passed, with strict Clippy for all four host/shared crates.
- Linux. `bash scripts/check-host-sync.sh --with-frame` passed in a fresh secret-free Boat sandbox, including 27 host/shared tests, strict Clippy, 10 headless frame calendar tests and real host-to-frame-receiver serial PTY checks. The PTY persisted 80 events, accepted empty replacement and retained stored data after source failure. The slow-provider test delivered two lock heartbeats during an 18-second fetch. Boat deleted the sandbox after success.
- Windows target. The same sandbox passed `cargo check --locked -p frame-host-sync --target x86_64-pc-windows-gnu`, including the new Job Object implementation. This is compilation evidence, not a Windows runtime test.
- Build preparation. All 11 isolated build-input tests passed, now including both shared host crates. The macOS subprocess-pipe receiver checks also passed.
- Frame Clippy completed with only the existing screensaver guard and weather-test warnings. Active LSP probes did not receive server answers; compiler, Clippy and test results provide the confirmed checks.

These corrections have not been deployed to the connected frame. Its watcher and serial port were left untouched.

## Remaining verification
- If WorkIQ is selected instead of Calendar MCP, establish that its response is complete. Its structured answer remains model-generated even after syntax and completeness checks.
- Build and run on Windows with the user's CLI installations and USB serial driver.
- Test direct Google ingestion when that provider is implemented.

See [host sync setup and protocol](../host-sync/README.md) for commands, failure semantics, deduplication rules and current limits.
