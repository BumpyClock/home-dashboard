# Frame host sync

Cross-platform Rust host application. Calendar sync is its first capability, not the application boundary.

```text
host-sync/                    frame-host-sync executable
  src/main.rs                 configuration and capability commands
  src/serial.rs               shared serial transport
  calendar-sync/              calendar sources, event model, merge, snapshots, receiver
  host-state/                 lock-state messages and frame-side visibility lease
  host-command/               bounded subprocess capture and process-group/job cleanup
```

The frame depends on `calendar-sync` without its `host` feature. It does not need WorkIQ, mcporter, or host-side authentication. Future host capabilities can sit beside calendar sync without depending on its event model.

## Build and configure

From the repository root:

```sh
cargo build --release -p frame-host-sync
cargo run -p frame-host-sync -- ports
cargo run -p frame-host-sync -- calendar preview --config host-sync/examples/calendar-mcp.json
cargo run -p frame-host-sync -- calendar sync --config host-sync/examples/calendar-mcp.json
cargo run -p frame-host-sync -- calendar watch --config host-sync/examples/calendar-mcp.json
```

`preview` prints the complete wire snapshot without opening serial. Treat its output as private calendar data. `sync` sends once. `watch` refreshes the calendar at `interval_seconds` and independently reports the host's lock state. Calendar fetching runs on a worker; one main-loop writer owns serial so slow provider calls do not block lock heartbeats. Ctrl+C stops it. Restart to reload configuration.

Copy an example to a private file and edit `connection.port`. Use the exact name from `ports`. Typical names are `/dev/cu.usbmodem*` on macOS, `/dev/ttyACM0` on Linux, and `COM3` on Windows. The app uses 115200, 8N1 and software flow control. Linux users need permission to open the port. No libudev development package is needed to build the host.

Connection can be omitted for `preview`. Source file paths are relative to the configuration file. Executables resolve from PATH, or use an absolute path. For npm CLIs on Windows, set `executable` to the installed `.cmd` shim when needed, for example `mcporter.cmd` or `workiq.cmd`.

`calendar.days` defaults to 7, allowed 1 to 31. Queries start at today's local midnight and end at midnight `days` calendar days later, exclusive. Today is included in that count. There is no yesterday padding. Each midnight is converted separately to UTC, preserving daylight-saving changes. `calendar.time_zone` optionally selects an IANA zone such as `America/Los_Angeles`; otherwise the host's system zone defines today. Set it to the frame's configured zone when host and frame differ.

`interval_seconds` defaults to 300, allowed 30 to 86400. Empty `sources` explicitly clears host calendar data on the next successful sync.

## Calendar sources

Use **Microsoft Calendar MCP** for reliable work-calendar export. It returns Graph event records without asking a language model to reconstruct the calendar. WorkIQ remains an explicit experimental alternative, not an automatic fallback.

All sources are read-only. No event bodies, attendees, or meeting links are requested by the WorkIQ prompt or copied from Calendar MCP responses. Event titles and locations still contain potentially sensitive work data. Only configure sources permitted on your frame.

### WorkIQ through mcporter

`examples/workiq.json` uses the configured `Workiq.ask` tool:

```json
{"kind":"workiq_mcp","id":"work","server":"Workiq","executable":"mcporter"}
```

Authenticate/configure WorkIQ in mcporter before starting sync. The app passes `--no-oauth`, so background sync never starts an interactive sign-in. MCP errors remain errors even if mcporter exits successfully.

### Local WorkIQ CLI

Replace the source with:

```json
{"kind":"workiq","id":"work","executable":"workiq"}
```

Install and authenticate [Microsoft Work IQ](https://learn.microsoft.com/en-us/microsoft-365/copilot/extensibility/work-iq/cli) separately. The adapter runs `workiq ask -q` with an explicit calendar window and requests structured JSON, actual occurrence IDs, expanded recurrence and an explicit completeness flag.

WorkIQ is a natural-language provider, not a guaranteed calendar export API. The adapter rejects prose, invalid JSON and `complete:false`; it cannot prove that a model's `complete:true` answer is accurate. Review `preview` before using it. Prefer the structured Calendar MCP source when accurate export is available. No silent provider fallback occurs.

### Structured Microsoft Calendar MCP

`examples/calendar-mcp.json` uses:

```json
{"kind":"mcporter_calendar","id":"work","server":"MicrosoftCalendar","executable":"mcporter"}
```

Calls `MicrosoftCalendar.ListCalendarView`, not `ListEvents`, to expand recurring instances. Sends `startDateTime` and `endDateTime` for today onwards, `timeZone:"UTC"`, `orderby:"start/dateTime"` and `top:1000`. Requests only `id,iCalUId,subject,start,end,isAllDay,isCancelled,location` through `select`; full event bodies never enter the response.

Observed live MCP output has one text payload prefixed with `Calendar view retrieved successfully.`, followed by Graph JSON, and a separate `CorrelationId`/timestamp text block. The adapter removes that known framing, parses JSON and validates events. It prefers `structuredContent` when the server supplies it. It does not search arbitrary prose for braces or use a model to recover missing fields. Unknown framing, multiple payloads, malformed JSON and MCP errors fail the cycle instead of clearing the calendar. A valid empty `value` array is accepted.

Maps `iCalUId`, subject, start/end, location, all-day events and cancellation state. Naive timed values are accepted only when explicitly marked UTC. All-day dates use the returned civil dates with an exclusive end.

A paginated result or 1000 returned rows fails the cycle instead of silently replacing the frame with a truncated calendar. Reduce `calendar.days` in that case. Automatic pagination is not implemented.

### JSON source

For another local integration, or offline testing:

```json
{"kind":"json","id":"personal","path":"events.json"}
```

The file contains an array of canonical events, not a whole snapshot. See `examples/events.json`. An empty array removes that source's events. Timed events require explicit UTC offsets. All-day events use dates, never converted to another time zone. The source is responsible for expanding recurrence and omitting cancelled events.

## Existing USB console

The default `console` connection keeps the frame's `ttyGS0` login/getty intact. Close `screen`, deployment scripts and other serial clients before syncing. One client owns the port at a time.

1. Log in as `connection.username`, default `diag`, if a login prompt is present.
2. Read the password from `connection.password_env` only when a password prompt appears. Never put a password in JSON or arguments.
3. Establish a shell handshake. Run the deployed frame executable with `--calendar-receive` and temporarily disable canonical input/echo. A 30-second device-side timeout bounds the receiver.
4. Wait for `FRAME_CALENDAR_READY_V1`, send one compact JSON line, then wait for `FRAME_CALENDAR_STORED_V1` and terminal restoration.
5. Release the port between cycles. The existing logged-in shell is retained, like the deployment tooling.

Use the existing secret-management workflow to populate the password environment variable. The app does not read `.claude/frame-credentials.json` or copy its password into another file.

The new frame executable must be deployed before syncing. Receiver runs as `diag` and writes `~/.local/share/family-frame/calendar-host.json`. The updated `deploy/photoframe-proto.service` reads that same path through `FRAME_CALENDAR_HOST`. An existing service installation needs that environment setting when this build is deployed. See [serial deployment](../docs/serial-deploy.md). No deployment or service changes are performed by the host app.

The receiver performs no UI initialization. Protocol can be exercised locally:

```sh
cargo run -p frame-host-sync -- calendar preview --config host-sync/examples/offline.json > /tmp/calendar.json
FRAME_CALENDAR_HOST=/tmp/calendar-host.json cargo run -p photoframe_proto -- --calendar-receive < /tmp/calendar.json
FRAME_CALENDAR_HOST=/tmp/calendar-host.json cargo run -p photoframe_proto
```

`{"kind":"raw","port":"..."}` supports a dedicated serial channel already connected to a running receiver. Do not use `raw` on the login console. Raw receiver startup, terminal setup and restarting after each exchange are the caller's responsibility.

## Frame modes and deduplication

- `FRAME_CALENDAR_MODE=host` reads only host data.
- `FRAME_CALENDAR_MODE=combined` also reads `FRAME_CALENDAR_DIRECT` if set. This is the default.
- `FRAME_CALENDAR_HOST` selects the host snapshot file. Default is `~/.local/share/family-frame/calendar-host.json`.
- `FRAME_CALENDAR_DIRECT` selects a separate snapshot file for an on-device provider. A future Google connection can publish the same schema here. Google authentication/fetching is not implemented in this change.

Host and direct providers own separate files. A host replacement never deletes direct events. Direct inputs win display fields on duplicates, then host source configuration order breaks ties. Snapshot arrival does not change that precedence.

Deduplication first matches a shared iCalendar UID plus occurrence start. End-time changes do not create another copy. Without a shared UID on both records, fallback matches normalized title plus exact time interval across different sources. Distinct known UIDs and recurring occurrences at different starts remain separate. All known UID occurrences and their title/time aliases are indexed before matching records without UIDs. Each occurrence retains every contributing source, so a second distinct event from the same source cannot disappear through fallback matching. Earlier inputs still choose display fields, even when their record lacks a UID. Matching by title/time can still merge unrelated meetings when one lacks a UID. Supply UIDs wherever available.

The frame reloads files off the UI thread every two seconds and applies updates on its UI thread within another second. Home and Calendar classify, order and count down timed events using their UTC instants and one clock sample per refresh. Local times are presentation only, including during repeated or skipped DST hours. Timed events display in the frame's configured zone. Overnight events appear on each intersected local day. All-day rows show “All day”; timed events take precedence in “next up”. No fake calendar events or attendee assignments remain. Home agenda, tomorrow and screensaver's next-up text use the real inputs. Home's Tomorrow preview and its count include timed events only. Calendar provides a date-grouped upcoming agenda for the next seven days, retaining all-day events there, omitting ended meetings and showing the latest input sync time. It does not request additional dates from the host when scrolled.

Before the first valid snapshot, the UI says calendar data is not synced. A valid empty snapshot means the calendar is clear. After a read/parse error, the last valid in-memory data is retained and the error is logged. The persisted last-good host snapshot survives app restarts and cable removal. Calendar shows the last sync timestamp, but there is no automatic expiry policy. To clear data, send an empty valid snapshot rather than deleting a file.

## Hide host calendars when the computer locks

On the frame, Settings → Calendar privacy → **Hide host calendar when computer locks**. The option is saved on the frame and defaults to Off for existing installations.

With the option On, host-origin events disappear from Home, Tomorrow, Calendar, counts and screensaver next-up text unless the frame has a fresh unlocked heartbeat. Direct on-device calendar records remain visible, including the direct copy of an event also found on the host. Host data is filtered before display rather than hiding a deduplicated record that might also belong to a direct calendar. Calendar shows why host data is hidden.

The `console` connection's `calendar watch` checks lock state every two seconds and sends changes immediately, plus a heartbeat approximately every five seconds. Serial login/transfer and the frame's two-second input polling add latency; this is not instantaneous protection. The frame expires unlocked permission 20 seconds after the last observed heartbeat using its own monotonic clock. Missing/unknown status, sleep, cable removal, watcher failure and expired heartbeats hide host data. A frame restart ignores the status packet already on disk and requires a newly received unlocked heartbeat. Unlocking restores cached host data without another calendar fetch.

Lock state uses a separate `host-state` protocol and sidecar, not calendar snapshot replacement. `--host-state-receive` stores a bounded version-1 JSON message with `state` (`locked`, `unlocked`, `unknown`) and `checked_at`. Its handshakes are `FRAME_HOST_STATE_READY_V1` and `FRAME_HOST_STATE_STORED_V1`. The frame stores it beside the host snapshot as `calendar-host.host-state.json`. The host timestamp distinguishes packets; it does not set the frame's visibility deadline. Both updated host and frame binaries are required.

`cargo run -p frame-host-sync -- host-state` prints the locally detected state without opening serial. Platform detection:

- macOS uses the current user's console session and `IOConsoleLocked`/`CGSSessionScreenIsLocked` from IORegistry. Another user's console or a failed/missing probe returns unknown.
- Linux uses `loginctl show-session`, requiring an active X11/Wayland session and `LockedHint`. The desktop must maintain logind's lock hint. Headless/SSH sessions or unavailable logind return unknown. Linux desktop lock/unlock still needs live verification.
- Windows 10+ uses the current session's WTS extended information and lock flags. The Windows target compiles, but live Windows lock/unlock and serial-driver behaviour are not verified. Windows 7/Server 2008 R2 are not supported.

macOS/Linux lock probes share the provider subprocess runner. A two-second deadline covers both the process and output collection; each pipe is limited to 1 MiB. Timeout, I/O failure, nonzero exit or excess output fails closed to unknown.

Use `watch`, not one-shot `sync`, for this feature. `raw` transport continues calendar-only delivery and does not send lock heartbeats; a privacy-enabled frame therefore keeps its host events hidden. A host whose detector cannot confirm unlocked also stays hidden. Turning the frame option Off restores the previous always-show behaviour.

This controls display privacy, not encryption or deletion. Calendar data remains stored on disk and the switch is locally changeable on the frame. It is not a security boundary against someone with frame/serial access. There is one trusted host, not per-host identities or authentication inside the status message.

## Wire contract and failure semantics

A snapshot is one UTF-8 JSON object followed by LF:

```json
{
  "version": 1,
  "generated_at": "2026-11-02T17:00:00Z",
  "sources": [{
    "id": "work",
    "events": [{
      "id": "provider-occurrence-id",
      "uid": "shared-ical-uid",
      "title": "Review",
      "detail": "Office",
      "time": {"kind":"timed","start":"2026-11-02T18:00:00Z","end":"2026-11-02T19:00:00Z"}
    }]
  }]
}
```

All-day `time` is `{"kind":"all_day","start":"2026-11-02","end":"2026-11-03"}`. End is exclusive and must follow start. IDs must be unique within a source. Sources must have unique IDs within each snapshot. UID is optional.

Each snapshot completely replaces the host group, including removals and cancellations. Fetch failure in any configured source prevents the entire send. This retains old data rather than accidentally clearing a failed source, but also delays healthy source updates until the next wholly successful cycle.

Maximum snapshot size is 1 MiB, at most 32 sources and 5000 events. Provider stdout and stderr are each bounded at 4 MiB. One 120-second deadline covers execution and draining both pipes, including pipes inherited by descendants after the immediate child exits. Success and failure both clean up the command's owned Unix process group or Windows Job Object. Cleanup permits at most 100 ms to reap the leader; output readers are never joined without a deadline. Version and event validation happen before replacement. The receiver writes/syncs a temporary file in the same directory and replaces the old file before ACK. Invalid, incomplete or oversized messages do not replace the last good file. Replaying a snapshot replaces rather than appends.

Only one host should send to a frame. There is no multi-host arbitration or timestamp-based conflict resolution. The serial console's OS login provides authentication; raw protocol adds none.

## Checks

```sh
bash scripts/check-host-sync.sh
bash scripts/check-host-sync.sh --with-frame
cargo test -p photoframe_proto
```

Host checks include source-order deduplication permutations, inherited-pipe timeouts, stdout/stderr limits, descendant cleanup, and the shared lock-state lease/receiver tests. On Linux, `scripts/test-host-privacy.py` runs the real watcher against a PTY and a slow fixture CLI, asserting that status heartbeats continue while calendar fetching is blocked. Host checks do not build Slint. `--with-frame` additionally exercises the real frame receiver and file reload logic. On Linux, it uses a PTY to test host login, serial framing, a payload larger than canonical TTY limits, receiver persistence, empty replacement and source failure. On macOS, serialport's baud ioctl rejects PTYs, so this script uses subprocess pipes instead. Rust integration tests also exercise the wire exchange over local sockets. The frame's desktop tests include a headless Slint check of agenda, duplicate removal, all-day rows, tomorrow, next-up and empty states.

Linux CI can run in a secret-free Boat sandbox following [Boat CI](https://docs.boat.dev/ci):

```sh
boat run --no-env --type large --push . --timeout 900 -- bash -lc 'export PATH="$HOME/.cargo/bin:$PATH"; bash scripts/check-host-sync.sh --with-frame'
```

[Recorded validation results and remaining checks](../docs/calendar-sync-validation.md) distinguish fixture, PTY, target compilation and live-provider evidence.

A Windows build or Wine test does not establish real Windows serial-driver compatibility. That still requires a Windows machine and frame. Live provider availability and hardware deployment must also be verified separately from fixtures.
