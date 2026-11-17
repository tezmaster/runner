use gpui::prelude::*;
use gpui::{
    div, px, AnyElement, Context, FocusHandle, FontWeight, IntoElement, MouseButton, SharedString,
    Window,
};
use runner_core::protocol::mission_control::{
    MissionControlCard, MissionControlLane, MissionControlSnapshot, MissionControlTicket,
};
use std::path::PathBuf;
use std::rc::Rc;

use crate::{
    file_links::FileLinkTarget,
    theme,
    ui::{Button, ButtonVariant, Modal, OverlayWidth},
    NativeRoot,
};

#[derive(Default)]
pub(crate) struct MissionControlState {
    project_id: Option<String>,
    snapshot: Option<MissionControlSnapshot>,
    error: Option<String>,
    generation: u64,
    loading: bool,
    health_expanded: bool,
    pub(crate) ticket_modal: Option<TicketModal>,
}

/// The card a ticket modal was opened from: header facts plus where the
/// footer can navigate. The Markdown itself arrives asynchronously.
#[derive(Clone)]
pub(crate) struct TicketModalCard {
    ticket_id: String,
    mission_id: Option<String>,
    severity: Option<String>,
    triage_state: Option<String>,
    lane: MissionControlLane,
    status: Option<CardStatus>,
}

pub(crate) enum TicketModalContent {
    Loading,
    Ready(MissionControlTicket),
    Failed(String),
}

pub(crate) struct TicketModal {
    card: TicketModalCard,
    content: TicketModalContent,
    close_focus: FocusHandle,
}

impl NativeRoot {
    pub(crate) fn render_mission_control_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project_id = self.active_project_id(cx);
        let projects = self.app_store.read(cx).projects.clone();
        let Some(project) = projects
            .iter()
            .find(|project| Some(project.id.as_str()) == project_id.as_deref())
        else {
            return empty_state("Open a project to read its local tickets and triage evidence.");
        };
        if self.mission_control.project_id.as_deref() != Some(project.id.as_str()) {
            self.refresh_mission_control(project.id.clone(), cx);
        }
        match (&self.mission_control.snapshot, &self.mission_control.error) {
            (Some(snapshot), _) => self.render_mission_control_snapshot(
                project.name.clone(),
                snapshot.clone(),
                window,
                cx,
            ),
            (_, Some(error)) => empty_state(format!("Could not read local evidence: {error}")),
            _ => empty_state("Loading local tickets and triage evidence…"),
        }
    }

    /// Loads the read-only daemon snapshot outside GPUI's render pass.
    pub(crate) fn refresh_mission_control(&mut self, project_id: String, cx: &mut Context<Self>) {
        if self.remote_client.is_active()
            || self.mission_control.loading
                && self.mission_control.project_id.as_deref() == Some(project_id.as_str())
        {
            return;
        }
        self.mission_control.project_id = Some(project_id.clone());
        self.mission_control.snapshot = None;
        self.mission_control.error = None;
        self.mission_control.loading = true;
        self.mission_control.generation += 1;
        let generation = self.mission_control.generation;
        let core = self.core(cx).clone();
        let task = cx.background_spawn(async move { core.mission_control_snapshot(&project_id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.mission_control.generation != generation {
                    return;
                }
                this.mission_control.loading = false;
                match result {
                    Ok(snapshot) => this.mission_control.snapshot = Some(snapshot),
                    Err(error) => this.mission_control.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the read-only ticket modal for a card, linked or not, and reads
    /// the ticket's current Markdown from the daemon on demand.
    pub(crate) fn open_ticket_modal(
        &mut self,
        card: TicketModalCard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let close_focus = cx.focus_handle();
        close_focus.focus(window, cx);
        let ticket_id = card.ticket_id.clone();
        let fetch_ticket_id = ticket_id.clone();
        self.mission_control.ticket_modal = Some(TicketModal {
            card,
            content: TicketModalContent::Loading,
            close_focus,
        });
        let Some(project_id) = self.mission_control.project_id.clone() else {
            if let Some(modal) = self.mission_control.ticket_modal.as_mut() {
                modal.content = TicketModalContent::Failed("no project is open".into());
            }
            cx.notify();
            return;
        };
        let core = self.core(cx).clone();
        let task = cx.background_spawn(async move {
            core.mission_control_ticket(&project_id, &fetch_ticket_id)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                let Some(modal) = this.mission_control.ticket_modal.as_mut() else {
                    return;
                };
                if modal.card.ticket_id != ticket_id {
                    return;
                }
                modal.content = match result {
                    Ok(ticket) => TicketModalContent::Ready(ticket),
                    Err(error) => TicketModalContent::Failed(error.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn render_ticket_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let modal = self
            .mission_control
            .ticket_modal
            .as_ref()
            .expect("ticket modal is open");
        let root = cx.entity();
        let close_root = root.clone();
        let title = div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(
                div()
                    .font_family(theme::UI_MONOSPACE_FONT)
                    .text_color(theme::muted())
                    .child(modal.card.ticket_id.clone()),
            )
            .children(modal.card.severity.clone().map(|severity| {
                let color = severity_color(&severity);
                chip(severity, color)
            }))
            .children(
                modal
                    .card
                    .triage_state
                    .clone()
                    .map(|state| chip(state, theme::accent())),
            )
            .child(chip(
                lane_label(modal.card.lane).to_owned(),
                lane_color(modal.card.lane),
            ))
            .children(modal.card.status.map(status_pill));
        let body: AnyElement = match &modal.content {
            TicketModalContent::Loading => div()
                .text_sm()
                .text_color(theme::muted())
                .child("Loading ticket…")
                .into_any_element(),
            TicketModalContent::Failed(error) => div()
                .text_sm()
                .text_color(theme::danger())
                .child(format!("Could not open ticket: {error}"))
                .into_any_element(),
            TicketModalContent::Ready(ticket) => {
                let blocks = crate::surfaces::mission_markdown::parse_blocks(&ticket.markdown);
                crate::surfaces::mission_markdown::render_markdown(
                    &format!("MISSION_CONTROL_TICKET:{}", modal.card.ticket_id),
                    &blocks,
                    cx.entity_id(),
                    None,
                    theme::accent(),
                    None,
                    cx,
                )
            }
        };
        let file_path = match &modal.content {
            TicketModalContent::Ready(ticket) => Some(ticket.path.clone()),
            _ => None,
        };
        let mission_id = modal.card.mission_id.clone();
        let open_mission_root = root.clone();
        let close_btn_root = root.clone();
        let close_focus = modal.close_focus.clone();
        let footer = div()
            .flex()
            .items_center()
            .gap_2()
            .children(file_path.map(|path| {
                div()
                    .debug_selector(|| "MISSION_CONTROL_TICKET_OPEN_FILE".into())
                    .child(
                        Button::new("MISSION_CONTROL_TICKET_OPEN_FILE", "Open file").on_press(
                            move |_, cx| {
                                crate::file_links::open_file_link(
                                    FileLinkTarget {
                                        path: PathBuf::from(path.clone()),
                                        line: None,
                                        column: None,
                                    },
                                    cx,
                                );
                            },
                        ),
                    )
            }))
            .children(mission_id.map(|mission_id| {
                div()
                    .debug_selector(|| "MISSION_CONTROL_TICKET_OPEN_MISSION".into())
                    .child(
                        Button::new("MISSION_CONTROL_TICKET_OPEN_MISSION", "Open mission")
                            .variant(ButtonVariant::Primary)
                            .on_press(move |window, cx| {
                                open_mission_root.update(cx, |this, cx| {
                                    this.mission_control.ticket_modal = None;
                                    this.open_mission(mission_id.clone(), window, cx);
                                });
                            }),
                    )
            }))
            .child(
                div()
                    .debug_selector(|| "MISSION_CONTROL_TICKET_CLOSE".into())
                    .child(
                        Button::new("MISSION_CONTROL_TICKET_CLOSE", "Close")
                            .focus_handle(close_focus)
                            .on_press(move |_, cx| {
                                close_btn_root.update(cx, |this, cx| {
                                    this.mission_control.ticket_modal = None;
                                    cx.notify();
                                });
                            }),
                    ),
            );
        Modal::new(
            title,
            body,
            Rc::new(move |_, cx| {
                close_root.update(cx, |this, cx| {
                    this.mission_control.ticket_modal = None;
                    cx.notify();
                });
            }),
        )
        .width(OverlayWidth::Lg)
        .footer(footer)
        .into_any_element()
    }

    fn render_mission_control_snapshot(
        &mut self,
        project_name: String,
        snapshot: MissionControlSnapshot,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (ticket_count, running_count, needs_you_count) = mission_control_counts(&snapshot);
        let warning_count = snapshot.health.len();
        let mut lanes = mission_control_lanes()
            .into_iter()
            .map(|lane| (lane, Vec::new()))
            .collect::<Vec<_>>();
        for card in snapshot.cards {
            let (_, cards) = lanes
                .iter_mut()
                .find(|(lane, _)| *lane == card.lane)
                .expect("every Mission Control lane is rendered");
            cards.push(card);
        }
        let lanes = lanes.into_iter().map(|(lane, mut cards)| {
            let closed_lane = lane == MissionControlLane::Closed;
            let total = cards.len();
            cards.sort_by_key(|card| std::cmp::Reverse(ticket_number(&card.ticket_id)));
            cards.truncate(lane_card_limit(lane));
            let hidden = total - cards.len();
            let cards = cards.into_iter().map(|card| {
            let status = card_status(&card);
            let ticket_id = card.ticket_id.clone();
            let mission_id = card.mission.as_ref().map(|mission| mission.id.clone());
            let modal_card = TicketModalCard {
                ticket_id: ticket_id.clone(),
                mission_id: mission_id.clone(),
                severity: card.severity.clone(),
                triage_state: card.triage_state.clone(),
                lane,
                status,
            };
            let pending_question = card
                .mission
                .as_ref()
                .and_then(|mission| mission.pending_question.clone());
            let handoff = card
                .mission
                .as_ref()
                .and_then(|mission| mission.newest_handoff.clone())
                .map(|handoff| format!("Handoff: {handoff}"));
            let branch = card
                .mission
                .as_ref()
                .and_then(|mission| mission.branch.clone());
            let active_slots = card.mission.as_ref().map(|mission| mission.active_slots);
            let evidence = card.evidence.to_vec();
            let card_border = match status {
                Some(CardStatus::NeedsYou) => theme::warning(),
                Some(CardStatus::Running) => theme::accent(),
                None => theme::border(),
            };
            let card_el = div()
                .id(SharedString::from(format!("MISSION_CONTROL_CARD:{ticket_id}")))
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(card_border)
                .bg(theme::raised())
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .min_w(px(0.))
                                .text_xs()
                                .child(
                                    div()
                                        .flex_none()
                                        .font_family(theme::UI_MONOSPACE_FONT)
                                        .text_color(theme::muted())
                                        .child(ticket_id.clone()),
                                )
                                .children(card.severity.map(|severity| {
                                    let color = severity_color(&severity);
                                    chip(severity, color)
                                }))
                                .children(card.triage_state.map(|state| chip(state, theme::accent()))),
                        )
                        .children(status.map(status_pill)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .line_clamp(2)
                        .child(card.summary),
                )
                .children(card.blockers.into_iter().map(|blocker| {
                    div()
                        .text_sm()
                        .text_color(theme::danger())
                        .truncate()
                        .child(format!("Blocked · {blocker}"))
                }))
                .children(pending_question.map(|question| {
                    let panel = div()
                        .id(SharedString::from(format!("MISSION_CONTROL_QUESTION:{ticket_id}")))
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::warning())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::warning())
                                .child("Needs you"),
                        )
                        .child(div().text_sm().child(question));
                    #[cfg(test)]
                    let panel = crate::theme_snapshot::record_fill("MISSION_CONTROL_QUESTION", panel);
                    panel
                }))
                .children(handoff.map(|handoff| {
                    div()
                        .text_sm()
                        .font_family(theme::UI_MONOSPACE_FONT)
                        .text_color(theme::muted())
                        .truncate()
                        .child(handoff)
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(theme::muted())
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .font_family(theme::UI_MONOSPACE_FONT)
                                .children(branch.map(|branch| format!("⎇ {branch}"))),
                        )
                        .children(active_slots.filter(|slots| *slots > 0).map(|slots| {
                            div().flex_none().child(format!(
                                "{slots} slot{}",
                                if slots == 1 { "" } else { "s" }
                            ))
                        }))
                        .child(
                    div()
                        .id(SharedString::from(format!("MISSION_CONTROL_SOURCES:{ticket_id}")))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(evidence.into_iter().map(|evidence| {
                    let path = evidence.path.clone();
                    let tip_path = path.clone();
                    let runner_mission = mission_id.clone();
                    let source_label = match evidence.kind {
                        runner_core::protocol::mission_control::MissionControlSourceKind::Ticket => "Ticket",
                        runner_core::protocol::mission_control::MissionControlSourceKind::Triage => "Triage",
                        runner_core::protocol::mission_control::MissionControlSourceKind::Runner => "Runner",
                    };
                    let evidence = div()
                        .id(SharedString::from(format!("MISSION_CONTROL_EVIDENCE:{path}")))
                        .px_1()
                        .rounded_sm()
                        .text_xs()
                        .text_color(theme::accent())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::panel()))
                        .child(source_label)
                        .on_mouse_down(MouseButton::Left, cx.listener(move |root, _, window, cx| {
                            cx.stop_propagation();
                            if evidence.kind
                                == runner_core::protocol::mission_control::MissionControlSourceKind::Runner
                            {
                                let Some(mission_id) = runner_mission.clone() else {
                                    return;
                                };
                                root.open_mission(mission_id, window, cx);
                            } else {
                                crate::file_links::open_file_link(
                                    FileLinkTarget { path: PathBuf::from(tip_path.clone()), line: None, column: None },
                                    cx,
                                );
                            }
                        }));
                    #[cfg(test)]
                    let evidence = crate::theme_snapshot::record_fill("MISSION_CONTROL_EVIDENCE", evidence);
                    crate::ui::Tooltip::new(
                        SharedString::from(format!("MISSION_CONTROL_EVIDENCE_TIP:{path}")),
                        path.clone(),
                        evidence,
                    )
                })),
                        ),
                );
            let card_el = if closed_lane {
                card_el.opacity(0.65)
            } else {
                card_el
            };
            #[cfg(test)]
            let card_el = crate::theme_snapshot::record_fill("MISSION_CONTROL_CARD", card_el);
            card_el.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                cx.listener(move |root, _, window, cx| {
                    root.open_ticket_modal(modal_card.clone(), window, cx);
                }),
            )
            });
            let lane = div()
                .id(SharedString::from(format!("MISSION_CONTROL_LANE:{}", lane_label(lane))))
                .w(px(272.))
                .flex_none()
                .self_stretch()
                .flex()
                .flex_col()
                .gap_2()
                .px_3()
                .py_2()
                .border_r_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if closed_lane {
                            theme::muted()
                        } else {
                            lane_color(lane)
                        })
                        .child(div().size(px(8.)).rounded_full().bg(lane_color(lane)))
                        .child(lane_label(lane).to_uppercase())
                        .child(
                            div()
                                .text_color(theme::muted())
                                .child(total.to_string()),
                        ),
                )
                .children(cards)
                .children((hidden > 0).then(|| {
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(theme::muted())
                        .child(format!("+{hidden} more, oldest not shown"))
                }));
            #[cfg(test)]
            let lane = crate::theme_snapshot::record_fill("MISSION_CONTROL_LANE", lane);
            lane
        });
        let health_expanded = self.mission_control.health_expanded;
        let health_strip = if snapshot.health.is_empty() {
            div()
                .id("MISSION_CONTROL_HEALTH")
                .py_2()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme::muted())
                .child("read-only · local evidence")
        } else {
            div()
                .id("MISSION_CONTROL_HEALTH")
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme::warning())
                .flex()
                .flex_col()
                .gap_1()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|root, _, _, cx| {
                        root.mission_control.health_expanded =
                            !root.mission_control.health_expanded;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::warning())
                        .child(format!(
                            "{warning_count} source warning{}",
                            if warning_count == 1 { "" } else { "s" }
                        )),
                )
                .children(health_expanded.then(|| {
                    div().flex().flex_col().gap_1().children(
                        snapshot.health.into_iter().map(|health| {
                            let kind = match health.kind {
                                runner_core::protocol::mission_control::MissionControlSourceKind::Ticket => "Ticket",
                                runner_core::protocol::mission_control::MissionControlSourceKind::Triage => "Triage",
                                runner_core::protocol::mission_control::MissionControlSourceKind::Runner => "Runner",
                            };
                            div()
                                .text_xs()
                                .text_color(theme::muted())
                                .child(format!("{kind} · {} · {}", health.path, health.warning))
                        }),
                    )
                }))
        };
        div()
            .id("mission-control")
            .size_full()
            .p(px(24.))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{project_name} / Mission Control")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(theme::muted())
                            .child(format!(
                                "{ticket_count} ticket{} · {running_count} running · {needs_you_count} needs you",
                                if ticket_count == 1 { "" } else { "s" },
                            ))
                            .children((warning_count > 0).then(|| {
                                div()
                                    .text_sm()
                                    .text_color(theme::warning())
                                    .child(format!(
                                        "{warning_count} source warning{}",
                                        if warning_count == 1 { "" } else { "s" }
                                    ))
                            })),
                    ),
            )
            .child(
                div()
                    .id("MISSION_CONTROL_BOARD_SCROLL")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_x_scroll()
                    .overflow_y_scroll()
                    .child(div().flex().items_stretch().children(lanes)),
            )
            .child(health_strip)
            .into_any_element()
    }
}

fn mission_control_lanes() -> [MissionControlLane; 7] {
    [
        MissionControlLane::Backlog,
        MissionControlLane::Ready,
        MissionControlLane::Running,
        MissionControlLane::NeedsYou,
        MissionControlLane::VerifyLand,
        MissionControlLane::Blocked,
        MissionControlLane::Closed,
    ]
}

/// Display-only counts for the slim header bar: tickets, running-lane cards
/// and needs-you-lane cards.
fn mission_control_counts(snapshot: &MissionControlSnapshot) -> (usize, usize, usize) {
    let tickets = snapshot.cards.len();
    let running = snapshot
        .cards
        .iter()
        .filter(|card| card.lane == MissionControlLane::Running)
        .count();
    let needs_you = snapshot
        .cards
        .iter()
        .filter(|card| card.lane == MissionControlLane::NeedsYou)
        .count();
    (tickets, running, needs_you)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardStatus {
    Running,
    NeedsYou,
}

/// The status pill for a card: NEEDS YOU wins when the mission has a pending
/// question, otherwise RUNNING when it has active slots.
fn card_status(card: &MissionControlCard) -> Option<CardStatus> {
    let mission = card.mission.as_ref()?;
    if mission.pending_question.is_some() {
        Some(CardStatus::NeedsYou)
    } else if mission.active_slots > 0 {
        Some(CardStatus::Running)
    } else {
        None
    }
}

fn status_pill(status: CardStatus) -> AnyElement {
    let (label, color) = match status {
        CardStatus::Running => ("RUNNING", theme::accent()),
        CardStatus::NeedsYou => ("NEEDS YOU", theme::warning()),
    };
    div()
        .px_1()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(color)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(label)
        .into_any_element()
}

/// Ticket number for newest-first ordering; non-numeric IDs sort last.
fn ticket_number(ticket_id: &str) -> u64 {
    ticket_id
        .strip_prefix("CR-")
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

/// Closed is history: show the newest few so it cannot swamp the board.
fn lane_card_limit(lane: MissionControlLane) -> usize {
    match lane {
        MissionControlLane::Closed => 25,
        _ => 200,
    }
}

fn severity_color(severity: &str) -> gpui::Hsla {
    let severity = severity.to_ascii_lowercase();
    if ["critical", "must", "high", "major", "blocker"]
        .iter()
        .any(|word| severity.contains(word))
    {
        theme::danger()
    } else if ["should", "medium"]
        .iter()
        .any(|word| severity.contains(word))
    {
        theme::warning()
    } else {
        theme::muted()
    }
}

fn lane_label(lane: MissionControlLane) -> &'static str {
    match lane {
        MissionControlLane::Closed => "Closed",
        MissionControlLane::NeedsYou => "Needs you",
        MissionControlLane::Running => "Running",
        MissionControlLane::VerifyLand => "Verify / Land",
        MissionControlLane::Blocked => "Blocked",
        MissionControlLane::Ready => "Ready",
        MissionControlLane::Backlog => "Backlog",
    }
}

fn lane_color(lane: MissionControlLane) -> gpui::Hsla {
    match lane {
        MissionControlLane::NeedsYou | MissionControlLane::VerifyLand => theme::warning(),
        MissionControlLane::Running | MissionControlLane::Ready => theme::accent(),
        MissionControlLane::Blocked => theme::danger(),
        MissionControlLane::Closed | MissionControlLane::Backlog => theme::muted(),
    }
}

fn chip(label: impl Into<SharedString>, color: gpui::Hsla) -> AnyElement {
    div()
        .px_1()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(color)
        .text_xs()
        .text_color(color)
        .child(label.into())
        .into_any_element()
}

fn empty_state(description: impl Into<String>) -> AnyElement {
    let empty = div().id("MISSION_CONTROL_EMPTY");
    #[cfg(test)]
    let empty = crate::theme_snapshot::record_fill("MISSION_CONTROL_EMPTY", empty);
    empty
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Mission Control"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::muted())
                        .child(description.into()),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size, Modifiers, TestAppContext, VisualTestContext};
    use runner_core::protocol::mission_control::{
        MissionControlCard, MissionControlEvidence, MissionControlLane, MissionControlMission,
        MissionControlSourceKind,
    };

    #[test]
    fn lanes_sort_newest_first_and_cap_closed_history() {
        assert_eq!(ticket_number("CR-188"), 188);
        assert_eq!(ticket_number("CR-verification"), 0);
        assert!(ticket_number("CR-1000") > ticket_number("CR-999"));
        assert_eq!(lane_card_limit(MissionControlLane::Closed), 25);
        assert!(lane_card_limit(MissionControlLane::Backlog) > 25);
    }

    #[test]
    fn severity_color_ranks_free_form_tracker_severities() {
        assert_eq!(severity_color("must-fix"), theme::danger());
        assert_eq!(severity_color("must-fix/should-fix"), theme::danger());
        assert_eq!(severity_color("High"), theme::danger());
        assert_eq!(severity_color("should-fix"), theme::warning());
        assert_eq!(severity_color("medium"), theme::warning());
        assert_eq!(severity_color("nit"), theme::muted());
    }

    #[test]
    fn lanes_display_in_flow_order() {
        assert_eq!(
            mission_control_lanes(),
            [
                MissionControlLane::Backlog,
                MissionControlLane::Ready,
                MissionControlLane::Running,
                MissionControlLane::NeedsYou,
                MissionControlLane::VerifyLand,
                MissionControlLane::Blocked,
                MissionControlLane::Closed,
            ]
        );
    }

    fn test_card(
        lane: MissionControlLane,
        mission: Option<MissionControlMission>,
    ) -> MissionControlCard {
        MissionControlCard {
            ticket_id: "CR-1".into(),
            summary: "Keep only the ticket summary".into(),
            severity: None,
            blockers: vec![],
            triage_state: None,
            lane,
            mission,
            evidence: vec![],
            provenance: vec![],
        }
    }

    fn test_mission(active_slots: usize, pending_question: Option<&str>) -> MissionControlMission {
        MissionControlMission {
            id: "mission-1".into(),
            title: "Review".into(),
            active_slots,
            pending_question: pending_question.map(str::to_owned),
            newest_handoff: None,
            branch: None,
            worktree: None,
        }
    }

    #[test]
    fn header_counts_derive_from_snapshot_lanes() {
        let snapshot = MissionControlSnapshot {
            project_id: "project".into(),
            project_path: "/tmp/project".into(),
            cards: vec![
                test_card(MissionControlLane::Running, None),
                test_card(MissionControlLane::NeedsYou, None),
                test_card(MissionControlLane::NeedsYou, None),
                test_card(MissionControlLane::Closed, None),
            ],
            health: vec![],
        };
        assert_eq!(mission_control_counts(&snapshot), (4, 1, 2));
    }

    #[test]
    fn status_pill_prefers_needs_you_over_running() {
        assert_eq!(
            card_status(&test_card(
                MissionControlLane::NeedsYou,
                Some(test_mission(2, Some("Approve?")))
            )),
            Some(CardStatus::NeedsYou)
        );
        assert_eq!(
            card_status(&test_card(
                MissionControlLane::Running,
                Some(test_mission(1, None))
            )),
            Some(CardStatus::Running)
        );
        assert_eq!(
            card_status(&test_card(
                MissionControlLane::Backlog,
                Some(test_mission(0, None))
            )),
            None
        );
        assert_eq!(
            card_status(&test_card(MissionControlLane::Backlog, None)),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn mission_control_route_renders_and_is_persisted() {
        let _theme = crate::theme_snapshot::ThemeGuard::new();
        let temp = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        let store = crate::app_store::test_lifecycle_store(&mut cx, temp.path());
        let host = cx.add_window(|window, cx| {
            NativeRoot::new(
                "mission-control".into(),
                temp.path().join("logs"),
                Some("/mission-control".into()),
                None,
                store.clone(),
                window,
                cx,
            )
        });
        let mut visual = VisualTestContext::from_window(host.into(), &cx);
        visual.simulate_resize(size(px(2600.), px(800.)));
        visual.run_until_parked();

        assert!(visual.debug_bounds("MISSION_CONTROL_EMPTY").is_some());
        host.read_with(&cx, |root, _| {
            assert_eq!(root.route, crate::AppRoute::MissionControl);
            assert_eq!(
                root.persisted_window_route().as_deref(),
                Some("/mission-control")
            );
            assert_eq!(
                root.runtime_navigation_history,
                vec![crate::RuntimeLocation::Entity(
                    crate::AppRoute::MissionControl
                )]
            );
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn populated_card_and_runner_evidence_open_the_linked_mission() {
        let _theme = crate::theme_snapshot::ThemeGuard::new();
        let temp = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        let store = crate::app_store::test_lifecycle_store(&mut cx, temp.path());
        let (project, mission, tab_id) = store.read_with(&cx, |store, _| {
            let mut conn = store.test_core.db.get().unwrap();
            let project = runner_daemon::ops::project::project_create(
                &store.test_core,
                "Board".into(),
                temp.path().display().to_string(),
            )
            .unwrap();
            let project_node = runner_daemon::repo::node::find_by_ref(
                &conn,
                runner_core::protocol::node::NodeType::Project,
                &project.id,
            )
            .unwrap()
            .unwrap();
            let layout = runner_app::pane_layout::PaneLayout::single(None, &[]);
            let tab_id = runner_daemon::repo::node::create_tab(
                &conn,
                Some(&project_node.id),
                "Board tab",
                0,
                &layout.serialize().unwrap(),
            )
            .unwrap()
            .id;
            let mission = runner_daemon::ops::mission::start(
                &mut conn,
                temp.path(),
                runner_core::protocol::mission::StartMissionInput {
                    project_id: Some(project.id.clone()),
                    crew_id: None,
                    title: "CR-1 review".into(),
                    goal_override: None,
                    cwd: Some(temp.path().display().to_string()),
                    started_by_session_id: None,
                    no_report: false,
                },
            )
            .unwrap()
            .mission;
            (project, mission, tab_id)
        });
        store.update(&mut cx, |store, cx| {
            store.refresh(crate::app_store::StoreRefreshKind::All, cx)
        });
        cx.run_until_parked();
        let snapshot = MissionControlSnapshot {
            project_id: project.id.clone(),
            project_path: temp.path().display().to_string(),
            cards: vec![MissionControlCard {
                ticket_id: "CR-1".into(),
                summary: "Review the populated board".into(),
                severity: Some("high".into()),
                blockers: vec!["needs evidence".into()],
                triage_state: Some("review".into()),
                lane: MissionControlLane::Closed,
                mission: Some(MissionControlMission {
                    id: mission.id.clone(),
                    title: mission.title.clone(),
                    active_slots: 1,
                    pending_question: Some("Approve?".into()),
                    newest_handoff: Some("handoff ready".into()),
                    branch: Some("feature/board".into()),
                    worktree: Some(temp.path().display().to_string()),
                }),
                evidence: vec![MissionControlEvidence {
                    kind: MissionControlSourceKind::Runner,
                    path: format!("mission:{}", mission.id),
                }],
                provenance: vec![],
            }],
            health: vec![],
        };
        let host = cx.add_window(|window, cx| {
            let mut root = NativeRoot::new(
                "mission-control-populated".into(),
                temp.path().join("logs"),
                None,
                None,
                store.clone(),
                window,
                cx,
            );
            root.tabs.activate(&tab_id);
            root.sync_active_project_from_active_tab(cx);
            root.mission_control.project_id = Some(project.id.clone());
            root.mission_control.snapshot = Some(snapshot.clone());
            root.set_route(crate::AppRoute::MissionControl, cx);
            root
        });
        let mut visual = VisualTestContext::from_window(host.into(), &cx);
        visual.simulate_resize(size(px(2600.), px(800.)));
        visual.run_until_parked();

        let card = visual.debug_bounds("MISSION_CONTROL_CARD").unwrap();
        assert!(visual.debug_bounds("MISSION_CONTROL_LANE").is_some());
        let evidence = visual
            .debug_bounds("MISSION_CONTROL_EVIDENCE")
            .expect("Runner evidence renders on populated cards");
        assert!(evidence.top() > card.top());
        visual.simulate_click(evidence.center(), Modifiers::default());
        visual.run_until_parked();
        host.read_with(&cx, |root, _| {
            assert_eq!(root.route, crate::AppRoute::Mission(mission.id.clone()))
        })
        .unwrap();

        host.update(&mut cx, |root, _, cx| {
            root.mission_control.snapshot = Some(snapshot);
            root.set_route(crate::AppRoute::MissionControl, cx);
        })
        .unwrap();
        visual.run_until_parked();
        let card = visual.debug_bounds("MISSION_CONTROL_CARD").unwrap();
        visual.simulate_click(
            point(card.left() + px(8.), card.top() + px(8.)),
            Modifiers::default(),
        );
        visual.run_until_parked();
        // Clicking the card opens the ticket modal instead of navigating.
        host.read_with(&cx, |root, _| {
            assert_eq!(root.route, crate::AppRoute::MissionControl);
            assert!(root.mission_control.ticket_modal.is_some());
        })
        .unwrap();
        assert!(visual.debug_bounds("MODAL_PANEL").is_some());
        let open_mission = visual
            .debug_bounds("MISSION_CONTROL_TICKET_OPEN_MISSION")
            .expect("linked cards offer Open mission");
        visual.simulate_click(open_mission.center(), Modifiers::default());
        visual.run_until_parked();
        host.read_with(&cx, |root, _| {
            assert_eq!(root.route, crate::AppRoute::Mission(mission.id.clone()));
            assert!(root.mission_control.ticket_modal.is_none());
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn question_panel_is_display_only_and_evidence_clicks_stay_on_the_card() {
        let _theme = crate::theme_snapshot::ThemeGuard::new();
        let temp = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        let store = crate::app_store::test_lifecycle_store(&mut cx, temp.path());
        let (project, mission, tab_id) = store.read_with(&cx, |store, _| {
            let mut conn = store.test_core.db.get().unwrap();
            let project = runner_daemon::ops::project::project_create(
                &store.test_core,
                "Board".into(),
                temp.path().display().to_string(),
            )
            .unwrap();
            let project_node = runner_daemon::repo::node::find_by_ref(
                &conn,
                runner_core::protocol::node::NodeType::Project,
                &project.id,
            )
            .unwrap()
            .unwrap();
            let layout = runner_app::pane_layout::PaneLayout::single(None, &[]);
            let tab_id = runner_daemon::repo::node::create_tab(
                &conn,
                Some(&project_node.id),
                "Board tab",
                0,
                &layout.serialize().unwrap(),
            )
            .unwrap()
            .id;
            let mission = runner_daemon::ops::mission::start(
                &mut conn,
                temp.path(),
                runner_core::protocol::mission::StartMissionInput {
                    project_id: Some(project.id.clone()),
                    crew_id: None,
                    title: "CR-2 review".into(),
                    goal_override: None,
                    cwd: Some(temp.path().display().to_string()),
                    started_by_session_id: None,
                    no_report: false,
                },
            )
            .unwrap()
            .mission;
            (project, mission, tab_id)
        });
        store.update(&mut cx, |store, cx| {
            store.refresh(crate::app_store::StoreRefreshKind::All, cx)
        });
        cx.run_until_parked();
        let ticket_path = temp
            .path()
            .join("issues")
            .join("CR-2.md")
            .display()
            .to_string();
        let snapshot = MissionControlSnapshot {
            project_id: project.id.clone(),
            project_path: temp.path().display().to_string(),
            cards: vec![MissionControlCard {
                ticket_id: "CR-2".into(),
                summary: "Review the question panel".into(),
                severity: None,
                blockers: vec![],
                triage_state: None,
                lane: MissionControlLane::NeedsYou,
                mission: Some(MissionControlMission {
                    id: mission.id.clone(),
                    title: mission.title.clone(),
                    active_slots: 1,
                    pending_question: Some("Approve?".into()),
                    newest_handoff: None,
                    branch: Some("feature/board".into()),
                    worktree: None,
                }),
                evidence: vec![MissionControlEvidence {
                    kind: MissionControlSourceKind::Ticket,
                    path: ticket_path.clone(),
                }],
                provenance: vec![],
            }],
            health: vec![],
        };
        let host = cx.add_window(|window, cx| {
            let mut root = NativeRoot::new(
                "mission-control-question".into(),
                temp.path().join("logs"),
                None,
                None,
                store.clone(),
                window,
                cx,
            );
            root.tabs.activate(&tab_id);
            root.sync_active_project_from_active_tab(cx);
            root.mission_control.project_id = Some(project.id.clone());
            root.mission_control.snapshot = Some(snapshot);
            root.set_route(crate::AppRoute::MissionControl, cx);
            root
        });
        let mut visual = VisualTestContext::from_window(host.into(), &cx);
        visual.simulate_resize(size(px(2600.), px(800.)));
        visual.run_until_parked();

        crate::file_links::LAUNCHED.with(|launched| launched.borrow_mut().clear());
        let panel = visual
            .debug_bounds("MISSION_CONTROL_QUESTION")
            .expect("pending question renders as a panel");
        visual.simulate_click(panel.center(), Modifiers::default());
        visual.run_until_parked();
        // The panel itself triggers no file open or answer action; the click
        // opens the read-only ticket modal without leaving the board.
        crate::file_links::LAUNCHED.with(|launched| {
            assert!(launched.borrow().is_empty());
        });
        host.read_with(&cx, |root, _| {
            assert_eq!(root.route, crate::AppRoute::MissionControl);
            assert!(root.mission_control.ticket_modal.is_some());
        })
        .unwrap();

        // The modal's only controls are file/mission navigation and close.
        let close = visual
            .debug_bounds("MISSION_CONTROL_TICKET_CLOSE")
            .expect("modal offers Close");
        visual.simulate_click(close.center(), Modifiers::default());
        visual.run_until_parked();
        host.read_with(&cx, |root, _| {
            assert!(root.mission_control.ticket_modal.is_none());
        })
        .unwrap();
        visual.run_until_parked();
        crate::file_links::LAUNCHED.with(|launched| launched.borrow_mut().clear());
        let card = visual.debug_bounds("MISSION_CONTROL_CARD").unwrap();
        let evidence = visual
            .debug_bounds("MISSION_CONTROL_EVIDENCE")
            .expect("ticket evidence renders on populated cards");
        assert!(evidence.top() > card.top());
        visual.simulate_click(evidence.center(), Modifiers::default());
        visual.run_until_parked();
        // The ticket source opens the file without also opening the card.
        crate::file_links::LAUNCHED.with(|launched| {
            let launched = launched.borrow();
            assert_eq!(launched.len(), 1);
            assert_eq!(launched[0].1.path.display().to_string(), ticket_path);
        });
        host.read_with(&cx, |root, _| {
            assert_eq!(root.route, crate::AppRoute::MissionControl);
            assert!(root.mission_control.ticket_modal.is_none());
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ticket_modal_shows_markdown_footer_routes_and_missing_file_error() {
        let _theme = crate::theme_snapshot::ThemeGuard::new();
        let temp = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        let store = crate::app_store::test_lifecycle_store(&mut cx, temp.path());
        let (project, tab_id) = store.read_with(&cx, |store, _| {
            let conn = store.test_core.db.get().unwrap();
            let project = runner_daemon::ops::project::project_create(
                &store.test_core,
                "Board".into(),
                temp.path().display().to_string(),
            )
            .unwrap();
            let project_node = runner_daemon::repo::node::find_by_ref(
                &conn,
                runner_core::protocol::node::NodeType::Project,
                &project.id,
            )
            .unwrap()
            .unwrap();
            let layout = runner_app::pane_layout::PaneLayout::single(None, &[]);
            let tab_id = runner_daemon::repo::node::create_tab(
                &conn,
                Some(&project_node.id),
                "Board tab",
                0,
                &layout.serialize().unwrap(),
            )
            .unwrap()
            .id;
            (project, tab_id)
        });
        store.update(&mut cx, |store, cx| {
            store.refresh(crate::app_store::StoreRefreshKind::All, cx)
        });
        cx.run_until_parked();
        let issues = temp.path().join("issues");
        std::fs::create_dir(&issues).unwrap();
        let markdown = "# CR-3: Unlinked ticket\n\n- Status: open\n";
        std::fs::write(issues.join("CR-3.md"), markdown).unwrap();
        let snapshot = MissionControlSnapshot {
            project_id: project.id.clone(),
            project_path: temp.path().display().to_string(),
            cards: vec![MissionControlCard {
                ticket_id: "CR-3".into(),
                summary: "Unlinked ticket".into(),
                severity: None,
                blockers: vec![],
                triage_state: None,
                lane: MissionControlLane::Backlog,
                mission: None,
                evidence: vec![MissionControlEvidence {
                    kind: MissionControlSourceKind::Ticket,
                    path: issues.join("CR-3.md").display().to_string(),
                }],
                provenance: vec![],
            }],
            health: vec![],
        };
        let host = cx.add_window(|window, cx| {
            let mut root = NativeRoot::new(
                "mission-control-ticket".into(),
                temp.path().join("logs"),
                None,
                None,
                store.clone(),
                window,
                cx,
            );
            root.tabs.activate(&tab_id);
            root.sync_active_project_from_active_tab(cx);
            root.mission_control.project_id = Some(project.id.clone());
            root.mission_control.snapshot = Some(snapshot);
            root.set_route(crate::AppRoute::MissionControl, cx);
            root
        });
        let mut visual = VisualTestContext::from_window(host.into(), &cx);
        visual.simulate_resize(size(px(2600.), px(800.)));
        visual.run_until_parked();

        // An unlinked card opens the modal too.
        let card = visual.debug_bounds("MISSION_CONTROL_CARD").unwrap();
        visual.simulate_click(card.center(), Modifiers::default());
        visual.run_until_parked();
        host.read_with(&cx, |root, _| {
            let modal = root
                .mission_control
                .ticket_modal
                .as_ref()
                .expect("modal opens");
            assert!(matches!(
                &modal.content,
                TicketModalContent::Ready(ticket) if ticket.markdown == markdown
            ));
            assert!(modal.card.mission_id.is_none());
        })
        .unwrap();
        // Unlinked cards offer file navigation but no mission target.
        assert!(visual.debug_bounds("MODAL_PANEL").is_some());
        assert!(visual
            .debug_bounds("MISSION_CONTROL_TICKET_OPEN_MISSION")
            .is_none());
        crate::file_links::LAUNCHED.with(|launched| launched.borrow_mut().clear());
        let open_file = visual
            .debug_bounds("MISSION_CONTROL_TICKET_OPEN_FILE")
            .expect("modal offers Open file");
        visual.simulate_click(open_file.center(), Modifiers::default());
        visual.run_until_parked();
        crate::file_links::LAUNCHED.with(|launched| {
            let launched = launched.borrow();
            assert_eq!(launched.len(), 1);
            assert_eq!(launched[0].1.path, issues.join("CR-3.md"));
        });
        host.read_with(&cx, |root, _| {
            assert_eq!(root.route, crate::AppRoute::MissionControl);
            assert!(root.mission_control.ticket_modal.is_some());
        })
        .unwrap();

        // A ticket deleted after the board loaded shows the error state.
        let close = visual
            .debug_bounds("MISSION_CONTROL_TICKET_CLOSE")
            .expect("modal offers Close");
        visual.simulate_click(close.center(), Modifiers::default());
        visual.run_until_parked();
        std::fs::remove_file(issues.join("CR-3.md")).unwrap();
        let card = visual.debug_bounds("MISSION_CONTROL_CARD").unwrap();
        visual.simulate_click(card.center(), Modifiers::default());
        visual.run_until_parked();
        host.read_with(&cx, |root, _| {
            let modal = root
                .mission_control
                .ticket_modal
                .as_ref()
                .expect("modal opens");
            assert!(matches!(
                &modal.content,
                TicketModalContent::Failed(error) if error.contains("not found")
            ));
        })
        .unwrap();
    }
}
