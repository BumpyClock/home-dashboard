# State protocols

These contracts name existing state and ownership. They do not introduce an app-wide state machine. Simple completion flags, mount latches and existing enums remain local.

## Theme transition

`State` in [`src/main.rs`](../src/main.rs) owns one reusable timer for `apply_settings`. The selected settings are authoritative; the displayed palette may lag only while that timer is pending.

| Event | Result |
| --- | --- |
| Select a different palette with motion enabled | Replace pending swap, raise the dip overlay, apply latest palette after 210 ms, then clear dip. |
| Select another palette before the timer fires | Cancel older callback and schedule latest palette. |
| Return to the displayed palette | Cancel pending swap and clear dip immediately. |
| Apply settings without animation or disable motion | Cancel pending swap, apply selected palette and clear dip immediately. |
| Change another preference while a swap is pending | Reapply current selected settings, never an older captured palette. |
| Window disappears before completion | Weak window upgrade fails; no UI mutation. |

Timer ownership supplies idle/pending state. A second phase flag or generation counter is unnecessary. No detached palette callback may overwrite a newer selection.

## Photo presentation

[`PhotoStage`](../ui/components.slint) is shared by Home's card and the screensaver. The following names describe derived conditions, not extra stored phases.

| Condition/event | Result |
| --- | --- |
| Stable, requested index equals current | Current slot remains visible; no outgoing slot. |
| Running, new target lacks pixels | Keep current photo; requested/current inequality represents waiting. |
| Running, ready target, motion enabled | Put destination in unused slot, swap current-slot selector, animate while retaining outgoing slot. |
| New requests while moving | Keep both transition slots intact; retain only latest requested destination. |
| Transition ends | Drop outgoing-slot visibility; start latest pending target only when its pixels are ready. |
| Disable motion while running | Cancel animation; snap to pending target only if ready, otherwise keep waiting. |
| Stop the stage | Cancel animation and assign requested index immediately, even if target is unready. Hidden-stage snapping is intentionally not readiness-gated. |

`running`, readiness, motion and autoplay are separate inputs. Autoplay requires ambient motion, multiple photos, no active transition or pending target, and neither night nor benchmark mode. Do not add a phase enum alongside `moving` and derived `pending`; any future replacement must remove state and preserve these semantics.

### Loading and residency

[`PhotoLoadState`](../src/photo_load_state.rs) owns card-size generations. Same active/completed requests reuse work. Size or scrim-mode changes invalidate obsolete generations. Only matching callbacks can publish or finish that generation. Failed batches remain retryable.

The scheduler in [`src/photos.rs`](../src/photos.rs) owns demand, resident slots and per-photo claims. Exact slot membership and dimensions decide whether work remains wanted. An unrelated demand change does not cancel still-wanted work. Both images actually in a transition remain demanded until presentation drops the outgoing one.

### Callback completion is not successful publication

[`completion::complete`](../src/photos/completion.rs) preserves this ordering:

1. Queue the UI callback. Rejection skips persistence and releases the claim.
2. Perform optional thumbnail persistence while the queued callback may also run.
3. Wait for the callback's sender to drop. It can finish applying rows, reject obsolete work, or be abandoned because the window is gone.
4. Admit optional full-rendition persistence, then release the photo claim.

This sender-drop barrier proves callback lifetime ended, not that pixels reached the model or display. Keep that distinction when interpreting `photo.ui.ack_wait` trace events. Full-cache admission is also not a guarantee of successful disk persistence.

## Host-calendar privacy

Privacy is a distributed protocol, not an isolated `hidden` flag.

| Owner | Responsibility |
| --- | --- |
| [`LockReporter`](../host-sync/src/lib.rs) | Send on host lock-state changes and five-second heartbeat deadlines. Failed attempts retry on a later heartbeat or state change. |
| [`Connection`](../host-sync/src/serial.rs) | Serialize console receiver handshakes and await storage acknowledgement. Raw transport does not supply automatic lock heartbeats. |
| [`host_state::receive`](../host-sync/host-state/src/lib.rs) | Validate and persist a complete status packet before emitting STORED. |
| [`Calendar`](../src/calendar.rs) privacy worker | Observe changed status packets separately from potentially blocked calendar file reads. Timestamp receipt with frame monotonic time. |
| [`Visibility`](../host-sync/host-state/src/lib.rs) | Permit host visibility only for an unlocked observation younger than 20 seconds. |
| [`Calendar`](../src/calendar.rs) presentation | Apply opt-in hiding to every host-origin calendar surface while preserving direct calendars and cached host data. |

### Lease transitions

- Default and frame restart begin unknown. A persisted unlocked packet is not fresh permission.
- Observing unlocked grants a lease from its original frame receipt `Instant`.
- A fresh unlocked observation renews that lease even if visible state does not change.
- Locked or unknown observation hides host events immediately when privacy is enabled.
- At age exactly 20 seconds the lease is expired and state becomes unknown.
- UI dequeue time never renews a queued observation. An old queued unlock is already expired.
- Host wall-clock timestamps identify packets but do not determine permission duration.
- Hiding does not erase or replace host snapshots. Fresh unlock can reveal cached host events without another calendar fetch. Direct data and its update timestamp remain independently available.
- Disabling the preference reveals cached host data without requiring a fresh unlock. The preference is off by default and is display privacy, not encryption of stored files.

Keep privacy observation independent from calendar acquisition. A blocked file read must not delay relock or lease expiry.

## Verification

Run the regression suite with:

```bash
bash scripts/check-state-protocols.sh
```

Tests drive actual Slint components with a fake clock, call real theme application, exercise callback/persistence ordering, and check frame lease behavior with controlled receipt times. No screen, USB device, provider account or network weather request is needed by these tests. Linux needs the usual Slint desktop build dependencies; the test platform itself is headless.

Release-note context for the next release or PR: rapid theme changes can no longer be overwritten by an older delayed palette swap; cancelling a theme dip restores the overlay immediately. Photo publication is queued before optional thumbnail persistence, and privacy observation runs independently of calendar reads. Additional contract tests cover these ownership boundaries.
