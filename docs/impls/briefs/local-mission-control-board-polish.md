# Mission brief: Mission Control board polish

Branch `feature/mission-control-board-polish`, worktree
`/Users/terrynovis/runner-monitor-ui/.worktrees/feature-mission-control-board-polish`,
cut from `fork/main` (9754ef56). The feature spec is
[`local-mission-control-stage1.md`](../../features/local-mission-control-stage1.md);
this mission improves the shipped Stage 1 board's presentation and closes its
outstanding live QA. Stage 1 stays strictly read-only.

## Why

The shipped board works but is hard to scan. Every card prints full evidence
paths, Closed is the leftmost lane, the header is a large title plus a raw
project path, and running or waiting work does not stand out. Bring it up to
the density and clarity of the reference board.

## Visual reference

- Screenshot: `/Users/terrynovis/runner-monitor-ui/.worktrees/mission-control-reference/kanbots-board-reference.png`
- Kanbots, interaction reference only: <https://github.com/leodavinci1/kanbots>

Take layout, hierarchy and density from these. Kanbots is MIT licensed, so
its code (for example card layout logic, spacing, status mapping or text
truncation) may be adapted where it genuinely helps. Any adapted code must keep
the Kanbots MIT copyright and permission notice in a header comment on that
code, and the PR must say what was adapted. Do not add its SQLite issue store,
autopilot, task creation, answer buttons or a second worktree system. Build it
with Runner's existing GPUI patterns and `theme::` tokens; no hard-coded
colours.

## Scope (presentation only)

Edit `crates/runner-app/src/surfaces/mission_control.rs` and its tests. Do
not change `MissionControlSnapshot`, the daemon join, or lane derivation.

1. **Lane order as a flow:** Backlog, Ready, Running, Needs you,
   Verify / Land, Blocked, Closed. Only the display order changes; the
   derivation precedence in the spec is untouched. Closed is visually muted.
2. **Slim header bar:** `<project name> / Mission Control` on the left. On the
   right, counts derived from the snapshot: tickets, running, needs you, and
   source warnings (only when non-zero). No "New task" or any other action.
3. **Columns:** full-height lanes separated by subtle dividers. Compact
   uppercase header with the lane's status dot and a muted count. Every lane
   stays visible when empty. Horizontal scroll still works on narrow windows.
4. **Cards:**
   - Top row: ticket ID chip, severity chip, triage chip, and a status pill
     on the right (RUNNING when the mission has active slots, NEEDS YOU when it
     has a pending question). Drop the redundant per-card lane chip.
   - Summary as the card title, wrapping to at most a few lines.
   - Newest handoff in a single-line, monospace, truncated strip.
   - A genuine pending question as a bordered, emphasised panel with the
     question text. It is display only: no buttons, numbered options or click
     targets that suggest answering.
   - Blockers as a compact danger-coloured row.
   - Footer: branch with a branch glyph (truncated), active slot count, and a
     small "N sources" affordance replacing the full-path evidence rows. The
     affordance still opens each source exactly as today (ticket and triage
     paths open the file, the Runner source opens the mission); a hover or
     expanded list may show paths. Keep `cx.stop_propagation()` so a source
     click does not also open the card.
   - Running cards get an accent border, Needs you cards a warning border.
   - Card click still opens the existing Mission Workspace only for linked
     cards.
5. **Source health:** replace the subtitle count with a bottom status strip
   showing "N source warnings", expandable to list each warning's source kind,
   path and message. Healthy projects show a quiet "read-only · local
   evidence" strip.
6. Update `docs/guide/concepts/projects.md` and its zh-CN copy only where the
   described appearance changes.

Out of scope, for a later data-model follow-up: ticket age, diff line counts,
cost and CI check chips. Do not add protocol fields for them.

## Addendum (Terry, 2026-11-17): adapter fixes and ticket details modal

**Adapter fixes, already in the working tree.** Run live on the Refinement
project, every ticket landed in Backlog. Claude (outside the mission) changed
`crates/runner-daemon/src/ops/mission/control.rs`: the ticket id is `CR-<digits>`
from the file stem, fields are read case-insensitively with list markers and
bold (`- Status: done`, `**Severity:** nit ([Nit])`), `done`/`wontfix` count as
closed, heading separators and parenthetical annotations are stripped, and
`Blocker: none` is ignored. The card density was also tightened. Review this as
part of the candidate: it changes derivation inputs, not only presentation.

**Ticket details modal (read-only).** Clicking a card opens a modal with the
ticket's details. Behaviour:

- Add one read-only daemon request, e.g. `mission_control_ticket(project_id,
  ticket_id)`, returning the ticket's current Markdown read from disk on demand,
  its source path, and its parsed fields. Resolve the ticket only from the
  project's own `issues/` tree (including `archive/`) and reject any id or path
  that escapes it. Never persist the text in Runner storage; a missing or
  unreadable file returns a clear error the modal shows, not a crash.
- Render with the shared `ui::overlay::Modal` and the existing Markdown
  renderer in `surfaces/mission_markdown.rs`. Header: id, severity, triage
  state, lane and status pill. Body: rendered Markdown, scrollable. Footer:
  "Open file" (existing file-link routing) and, only for linked cards, "Open
  mission" (existing Mission Workspace route). Escape and the close button
  dismiss it.
- No edit, status change, dispatch, answer or any other mutation control.
- Card click now opens the modal for every card, linked or not; Mission
  Workspace stays reachable through "Open mission" and the Runner source link.
  Source-link clicks keep stopping propagation and do not open the modal.
- Tests: protocol round trip, daemon resolution including traversal rejection
  and missing file, and an app test that a card click opens the modal and the
  modal exposes no mutation control.

QA adds: open the modal from a ticket-only card and from a linked card; "Open
mission" navigates to the existing workspace; "Open file" opens the file;
a ticket deleted after the board loaded shows the error state in the modal.

## Checks

Coder: focused `cargo test -p runner-app --lib mission_control`, plus updated
or new tests for lane display order, header counts, the status pill, the
pending-question panel having no interactive children, and evidence click
propagation. Then `cargo check -p runner-app`, `make fmt` and
`cargo clippy -p runner-app --all-targets -- -D warnings`. Coder and reviewer
do not launch the app.

Reviewer: confirm the diff beyond the addendum is presentation only (no
`MissionControlSnapshot`, daemon join or lane-derivation change), and review
the addendum's derivation and protocol changes (adapter input fixes in
`control.rs`, the new `mission_control_ticket` request) with the same care;
confirm no mutation affordance appeared, that theme tokens are used, and that
the source-evidence links still route as before. Name the live checks for QA.

QA: under `script/qa-lock`, on the reviewed candidate, using a disposable
fixture project under `/private/tmp` as the QA role allows. Check and record
screenshots of:

- a ticket-only project (no missions);
- an empty or no-tracker project;
- malformed or unavailable ticket and triage sources (the status strip lists them);
- an active mission with slots, branch and worktree (RUNNING pill, accent border);
- a genuine pending Runner question (Needs you lane and panel, nothing clickable to answer);
- card click opening the existing Mission Workspace/feed, and source clicks opening the file without opening the card;
- no mutation controls anywhere on the board;
- a narrow window forcing horizontal lane scroll.

Record the results in `docs/tests/` following the existing QA records, with
the candidate commit and per-check verdicts.

## Authorisation

Approved crew: the **Muse trio** (coder, reviewer, qa) on the development
daemon, approved by Terry on 2026-11-17. The crew may commit on this branch,
push to `fork` (`tezmaster/runner`) and open one PR against `main` with the
`core-review` label. Fold this brief into the first commit. It must not merge,
retarget the PR upstream to `yicheng47/runner`, cut a release, or touch other
worktrees or branches.
