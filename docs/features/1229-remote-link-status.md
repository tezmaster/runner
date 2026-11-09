# Remote link status in the sidebar footer

Tracking issue: [#1229](https://github.com/yicheng47/runner/issues/1229). Design: `runner/desktop/specs/1229-remote-link-status.pen` in the design repo, frame `kLrRu`, option B, signed off by Jason on 2026-11-09.

## Motivation

A window showing another computer keeps the footer's usage percentages beside the green host button. Those numbers are this computer's usage, while everything else in the window belongs to the other computer, so they read as the host's and say nothing about the session being controlled.

What matters while controlling another computer is the link: whether it is up and how responsive it is. Runner tracks the connection state but has never measured latency: the daemon answers WebSocket pings and ignores pongs, and nothing times a round trip.

## Behavior

### Footer

While a window shows another computer, the footer's left slot shows link status in place of usage. A window showing this computer keeps its usage, and switching back brings it back. The status appears under the same condition as usage today, so a collapsed sidebar shows neither. The status is not a control: the host button still opens the popup and the gear still returns to this computer's Settings.

The mark occupies a fixed 18 px by 12 px space, so the text does not move when the state changes. Text is 11 px, as usage is.

| State | Mark | Text | Mark and text colour | Host button |
|---|---|---|---|---|
| Connected, under 100 ms | 4 bars | `42 ms` | Accent bars, `text-mid` text | Green |
| Connected, 100 to 199 ms | 3 bars | `150 ms` | Accent bars, `text-mid` text | Green |
| Connected, 200 to 499 ms | 2 bars | `320 ms` | Amber | Green |
| Connected, 500 ms or more | 1 bar | `800 ms` | Amber | Green |
| Opening, or connected before the first sample | `loader-circle` | Connecting | `text-mid` | Green |
| Reconnecting | `refresh-cw` | Reconnecting | Amber | Amber |
| Unreachable | `unplug` | Unreachable | Red | Red |
| Revoked | None | Not recognized | Red | Red |
| Forgotten | None | Forgotten | `text-mid` | Grey |
| Connected, host cannot answer | None | Connected | `text-mid` | Green |

Bars that are not lit use `line-strong`. Slow is a property of the link, not a connection failure, so the host button stays green.

### Host popup

The connected computer's row replaces `Connected · relay · encrypted` with `42 ms · relay · encrypted`. Until the first sample arrives, and for a host that cannot answer, it keeps the current text.

### Unreachable turns red

Unreachable changes from amber to red in the footer, the host button and the popup row, because Reconnecting is still trying and Unreachable has failed. This changes `host_tone` and `connection_subtitle`. The reconnecting banner and the connection card in the remote workspace are unchanged.

### What latency measures

Latency is the round trip to the host through the end-to-end encrypted channel, so it includes the relay and reflects what typing feels like. Timing WebSocket pings would measure only this computer to the relay. The measurement uses a purpose-built `ping` and `pong` rather than timing another request's reply (Jason, 2026-11-09), so a host shows a number once it has updated.

Each open route sends one ping every 5 seconds. The displayed value is the median of the last 3 samples, so the number does not flicker. A ping with no reply within 10 seconds counts as a 10,000 ms sample, so the reading falls to one bar rather than freezing until the route itself reports the failure. Samples are discarded when the route leaves Connected, so a reconnect starts again from Connecting.

The ping never decides the connection state, and an unanswered or refused ping never disconnects a route. The route's existing failure handling stays the only source of Reconnecting and Unreachable.

## Non-goals

- Showing the host's agent usage. The companion protocol carries no usage data, and the footer's job here is the link.
- Throughput or real signal strength. Throughput is near zero on an idle terminal, and a route through a relay has no signal to read.
- Link status from a CLI command.

## Implementation Phases

### 1. Protocol and host

- Add a `ping` request to `ClientMessage` and a `pong` reply to `HostMessage` in `crates/runner-core/src/companion/messages.rs`, the `pong` carrying the request's id, with decoder arms and fixtures beside the existing ones. Add `ping` to `FEATURES` in `crates/runner-daemon/src/companion/mod.rs` and to `fixtures/welcome.json`.
- The host answers `ping` with `pong` straight from the channel loop, without waiting on overview or feed work.
- The feature is negotiated through the welcome's feature list, so the companion protocol `VERSION` stays 1. An older host never receives a ping. The iPhone app ignores the new feature.

### 2. Daemon client

- In `crates/runner-daemon/src/remote/client/route.rs`, when the welcome lists `ping`, send the ping every 5 seconds for as long as the route is open and time its `pong`.
- Compute the median of the last 3 samples, apply the 10-second rule and clear the samples when the route closes.
- Emit a `remote/latency` event carrying the computer and the milliseconds, beside the existing `remote/connection` event.

### 3. App

- `crates/runner-app/src/surfaces/remote_client/model.rs` stores the latest latency per computer from `remote/latency` and clears it when the connection leaves Connected.
- `crates/runner-app/src/surfaces/app_shell/sidebar.rs` renders the link status in place of `usage` when `remote_client.is_active()`. Closing the usage popover on the switch to a remote window follows from `show_usage` becoming false there.
- `crates/runner-app/src/surfaces/remote_client/picker.rs` maps Unreachable to `theme::danger()` in `host_tone` and `connection_subtitle`, and adds the latency to the connected row.
- Add the footer labels and the latency line to `crates/runner-app/i18n/en.toml` and `zh-hans.toml`.

### 4. Docs

Update `docs/guide/guides/remote-access.md` and `docs/guide/zh-CN/guides/remote-access.md`: the remote footer shows link status instead of this computer's usage, and the host button is red when the computer cannot be reached. Add `ping` to the protocol description in `docs/tech/remote-control.md`.

## Verification

### Automated

- The `ping` request, the `pong` reply and the updated welcome round-trip through the companion fixtures.
- A host answers `ping` with a `pong` carrying the same id, and a route to a host without the feature sends none and reports no latency.
- The median of the last 3 samples, the 10-second rule, and clearing samples when the route leaves Connected.
- Bar and colour boundaries at 99, 100, 199, 200, 499 and 500 ms.
- `every_connection_state_has_a_host_tone_and_a_row_label` updated for red Unreachable.
- A remote window renders the status and no usage, and a local window renders usage and no status.

### Live checks

These change what a person sees in the running app, so the change runs in a Claude trio. Each needs a second computer paired with the development build, because latency only exists across a real route.

| Check | Why |
|---|---|
| Connected: a window on the other computer shows bars and a plausible millisecond value, and the popup row shows the same latency | The main behavior, and the first real end-to-end measurement |
| Switching between the other computer and this one swaps status and usage in the same window | The status must replace usage only while remote |
| Reconnecting: interrupt the route, then see the amber icon and text, then bars again once it returns | A real state transition, and samples must reset |
| Unreachable: stop the host's remote access, then see the red icon, text and host button | The colour change from today |
| A host on 0.19.5, which has no `ping`: it stays connected, shows `Connected` with no number, and logs no errors | Compatibility with hosts that have not updated |
| Light and dark appearance, and Glass on macOS: bars, icons and text stay legible in the footer | New marks on the translucent sidebar |
