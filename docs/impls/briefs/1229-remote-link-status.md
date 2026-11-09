# #1229 — Show link status in a remote window's footer

Jason asked for a mission to implement [#1229](https://github.com/yicheng47/runner/issues/1229) on 2026-11-09, continuing draft [PR #1233](https://github.com/yicheng47/runner/pull/1233); this is the default claude trio. Work only in `/Volumes/samsung/repos/yicheng47/runner/.worktrees/feat-1229-remote-link-status` on `feat/1229-remote-link-status`, at `109a2817`, which holds only the spec on top of `b2c15e38`. `main` has since moved to `d7f3984b`; rebase when the repo's rules call for it. The #1232 trio runs at the same time and shares the QA lock.

## Source of truth

- **Spec:** `docs/features/1229-remote-link-status.md` on this branch owns behavior, thresholds, phases and verification. This brief does not repeat them.
- **Design:** `runner/desktop/specs/1229-remote-link-status.pen`, frame `kLrRu`, option B, signed off by Jason on 2026-11-09. It is in `~/repos/yeelinks/yeelinks_design`, pulled to `2d307cb` for this mission. Read it through the Pencil tools only, since `.pen` files are encrypted, and do not edit or commit it. The design wins on visual detail, the spec on behavior. If the two disagree on something a person would notice, ask rather than choose. If the Pencil tools are unavailable in your session, say so before building the footer.

## Problem and outcome

A window showing another computer keeps this computer's usage percentages in the footer, where they read as the host's. Runner tracks the connection state but has never measured latency.

While a window shows another computer, the footer's left slot shows link status in place of usage: latency bars and the round trip in milliseconds, or an icon and label for Connecting, Reconnecting and Unreachable, as the spec's table lists. The host popup's connected row shows the latency, and Unreachable turns red. A window on this computer is unchanged.

## Read first

- The spec, in full.
- `crates/runner-core/src/companion/messages.rs` and its fixtures; `crates/runner-daemon/src/companion/mod.rs` (`FEATURES`, the channel loop).
- `crates/runner-daemon/src/remote/client/route.rs`, and where `remote/connection` is emitted.
- `crates/runner-app/src/surfaces/remote_client/model.rs`, `picker.rs` (`host_tone`, `connection_subtitle`); `crates/runner-app/src/surfaces/app_shell/sidebar.rs` (`show_usage`).
- `docs/tech/remote-control.md`, `docs/guide/guides/remote-access.md` and its zh-CN page.
- `docs/tests/regression/remote.md`, for how remote cases run.

## Deliverables

The spec's four phases: protocol and host, daemon client, app, docs. Hold to these points in implementation and review:

1. **`ping` is negotiated, not versioned.** The companion `VERSION` stays 1. An older host never receives a ping, and a host that does not list `ping` stays connected and shows `Connected` with no number. The iPhone app ignores the feature.
2. **The ping never affects the connection.** An unanswered or refused ping never closes a route or changes its state; the route's existing failure handling stays the only source of Reconnecting and Unreachable.
3. **A slow host must not delay the `pong`.** It is answered from the channel loop without waiting on overview or feed work, or the number measures the host's queue and not the link.
4. **Per-route sampling.** Samples and any in-flight ping are cleared when the route leaves Connected, so a reconnect starts again from Connecting. A late `pong` from a previous route must not count.
5. **Footer layout.** The mark holds its fixed 18 by 12 px space so text does not move between states. The status follows the same visibility as usage today, so a collapsed sidebar shows neither.
6. **Scope of the red.** Only `host_tone` and `connection_subtitle` change. The reconnecting banner and the remote workspace's connection card stay as they are.

**Non-goals:** the spec's non-goals, no change to the relay heartbeat, and no iPhone work.

## Tests

The spec's Automated list, each at the layer it names. Bar and colour boundaries are at 99, 100, 199, 200, 499 and 500 ms. Say in the handoff which regression tests fail without the change. Gates: tests for `runner-core`, `runner-daemon` and `runner-app`, workspace Clippy with warnings denied, `cargo fmt --all --check`. Regenerate any golden protocol fixture through its existing workflow and inspect the change. Drive macOS, Windows and Linux CI green.

## Live checks (qa)

These are the spec's six checks, each with the reason the spec gives:

- **L1:** connected: bars and a plausible millisecond value, and the popup row agrees.
- **L2:** switching between the other computer and this one swaps status and usage in the same window.
- **L3:** Reconnecting: amber icon and text, then bars again once the route returns.
- **L4:** Unreachable: red icon, text and host button.
- **L5:** a host on 0.19.5, which has no `ping`: stays connected, shows `Connected` with no number, logs no errors.
- **L6:** light and dark, and Glass on macOS: bars, icons and text stay legible.

**Prerequisite, for qa to raise at handoff.** Every check needs a real route to a second paired computer. `docs/tests/regression/remote.md` says crew slots do not contact the production service or read a secret store unless Jason explicitly authorizes that session, and qa never touches the installed Runner, which is the 0.19.5 on this Mac. So when qa receives the reviewed candidate, before launching anything, it asks Jason through `runner signal ask_human` how to run them, with choices such as: authorize a second candidate daemon on this Mac with private storage and test enrollments on his account, cleaned up afterwards, as in the [#1025 record](../../tests/archive/1025-remote-switch-repaint.md); or leave the checks to Jason on his second computer. Do not guess or start pairing before he answers. L5 needs a real 0.19.5 host and is likely Jason's either way.

The verdict gives pass, fail or blocked per check with screenshot evidence, and the PR description records it. A blocked check keeps `needs-live-test`.

## Handoffs

The coder implements and commits, then hands the candidate to `reviewer`. When the review is clean, the coder hands the reviewed commit to `qa`. Any failure, and any change after review, goes back through `reviewer` before qa reruns the affected checks.

The PR is #1233: update its description as code lands, keep `Refs #1229` until the feature is complete and use `Fixes #1229` in its final form, mark it ready for review when CI is green, remove `no-behavior-change`, and replace `risk-low` with `risk-high`, since it adds a wire message both ends depend on. `core-review` is added automatically if the paths match. Keep `needs-live-test` until every selected check has passed. No co-author trailers, per AGENTS.md.

## Authorization

The claude trio may commit, push this branch, force-push it with lease after a rebase, and update and ready PR #1233. It does not merge.
