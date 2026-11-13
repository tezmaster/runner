//! A mission's review, shown in its Info view: the person's rating and what
//! each slot used, so a finished run can be judged afterwards. Rating is the
//! person's alone (agents cannot rate), usage arrives from whoever harvested
//! the session logs, and neither is shown for a remote mission yet.

use gpui::prelude::*;
use gpui::{div, px, relative, rems, AnyElement, FontWeight, Hsla};
use runner_app::ui::{hue_for_seed, Button, ButtonSize, ButtonVariant, RoleAvatar};
use runner_core::protocol::{
    BudgetState, MissionBudget, MissionRateInput, MissionReview, RatingVerdict, SlotUsage,
};

use super::rail::meta_section;
use super::usage_chart::{self, Headline, Range, SlotGroup, UsageMode};
use super::*;
use crate::*;

#[derive(Default)]
pub(super) struct ReviewState {
    review: Option<MissionReview>,
    generation: u64,
    failed: bool,
    mode: UsageMode,
    /// Slots whose calls are listed.
    expanded: std::collections::BTreeSet<String>,
}

/// `391_000` -> `391k`, `42_959_037` -> `43.0M`; counts under a thousand stay whole.
fn compact(count: i64) -> String {
    match count {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{}k", (n as f64 / 1_000.0).round() as i64),
        n => n.to_string(),
    }
}

/// Input tokens of a slot, when it has any recorded.
fn input_of(slot: &SlotUsage) -> Option<i64> {
    slot.input_tokens
}

/// The slot to mark as the costliest, only when every recorded slot ran on
/// one runtime: counts from different runtimes are not comparable.
fn costliest(slots: &[SlotUsage]) -> Option<&str> {
    let mut runtimes = slots
        .iter()
        .filter(|slot| input_of(slot).is_some())
        .map(|slot| slot.runtime.as_deref());
    let first = runtimes.next()?;
    if runtimes.any(|runtime| runtime != first) {
        return None;
    }
    slots
        .iter()
        .filter(|slot| input_of(slot).is_some())
        .max_by_key(|slot| input_of(slot))
        .map(|slot| slot.handle.as_str())
}

#[cfg(test)]
impl MissionWorkspace {
    /// Puts a review in place without reading the daemon, with some slots' calls open.
    pub(super) fn set_review_for_test(&mut self, review: MissionReview, expanded: &[&str]) {
        self.review.review = Some(review);
        self.review.expanded = expanded.iter().map(|handle| (*handle).to_owned()).collect();
    }
}

impl MissionWorkspace {
    /// Reads the open mission's review in the background. Local missions only.
    pub(super) fn refresh_review(&mut self, cx: &mut Context<Self>) {
        if self.presentation.is_remote() {
            return;
        }
        let Some(mission_id) = self.mission_id.clone() else {
            return;
        };
        let generation = self.generation;
        self.review.generation += 1;
        let review_generation = self.review.generation;
        let core = self.core(cx).clone();
        let read_id = mission_id.clone();
        let task = cx.background_spawn(async move {
            // A fresh allowance reading while the mission runs (the daemon skips a
            // finished mission and one read within the last minute); a failed
            // reading must not hide the rest of the review.
            let _ = core.mission_budget_sample(&read_id);
            core.mission_review_get(&read_id)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if !this.is_current(&mission_id, generation)
                    || this.review.generation != review_generation
                {
                    return;
                }
                match result {
                    Ok(review) => {
                        this.review.review = Some(review);
                        this.review.failed = false;
                    }
                    Err(_) => this.review.failed = true,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Rates the open mission, or clears the rating when `verdict` is `None`.
    /// A note written elsewhere (the CLI) is kept when only the verdict changes.
    fn rate_mission(&mut self, verdict: Option<RatingVerdict>, cx: &mut Context<Self>) {
        if self.presentation.is_remote() {
            return;
        }
        let Some(mission_id) = self.mission_id.clone() else {
            return;
        };
        let note = self
            .review
            .review
            .as_ref()
            .and_then(|review| review.rating.as_ref())
            .filter(|_| verdict.is_some())
            .map(|rating| rating.note.clone())
            .unwrap_or_default();
        let generation = self.generation;
        let core = self.core(cx).clone();
        let rate_id = mission_id.clone();
        let task = cx.background_spawn(async move {
            core.mission_rate(MissionRateInput {
                mission_id: rate_id,
                verdict,
                note,
            })
            .map_err(|error| error.to_string())
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if !this.is_current(&mission_id, generation) {
                    return;
                }
                if let Err(error) = result {
                    this.warning = Some(action_failure(
                        crate::i18n::message("action.rate_mission"),
                        error,
                    ));
                }
                // The daemon announces the change; read it back either way so the view matches storage.
                this.refresh_review(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The Review sections of the Info view; none for a remote mission.
    pub(super) fn render_review_sections(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.presentation.is_remote() {
            return Vec::new();
        }
        let root = cx.entity();
        let rating = self
            .review
            .review
            .as_ref()
            .and_then(|review| review.rating.clone());
        let pressed = |verdict: RatingVerdict| rating.as_ref().is_some_and(|r| r.verdict == verdict);
        let rate_button = |id: &'static str,
                           label: &'static str,
                           verdict: RatingVerdict| {
            let root = root.clone();
            let target = if pressed(verdict) { None } else { Some(verdict) };
            Button::new(id, label)
                .size(ButtonSize::Compact)
                .variant(if pressed(verdict) {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Outline
                })
                .on_press(move |_, cx| {
                    let _ = root.update(cx, |this, cx| this.rate_mission(target, cx));
                })
        };

        let mut rating_body = div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(rate_button(
                    "mission-rating-up",
                    crate::i18n::text("ui.rating_up"),
                    RatingVerdict::Up,
                ))
                .child(rate_button(
                    "mission-rating-down",
                    crate::i18n::text("ui.rating_down"),
                    RatingVerdict::Down,
                )),
        );
        rating_body = match rating.as_ref() {
            Some(rating) if !rating.note.is_empty() => rating_body.child(
                div()
                    .text_size(theme::text_ui())
                    .line_height(rems(18. / 16.))
                    .text_color(theme::text())
                    .child(rating.note.clone()),
            ),
            Some(_) => rating_body,
            None => rating_body.child(
                div()
                    .text_size(theme::text_meta())
                    .italic()
                    .text_color(theme::faint())
                    .child(crate::i18n::text("ui.rating_none")),
            ),
        };

        let usage_body = match self.review.review.as_ref() {
            _ if self.review.failed => div()
                .text_size(theme::text_ui())
                .italic()
                .text_color(theme::faint())
                .child(crate::i18n::text("ui.usage_unavailable"))
                .into_any_element(),
            None => div()
                .text_size(theme::text_ui())
                .italic()
                .text_color(theme::faint())
                .child(crate::i18n::text("ui.loading"))
                .into_any_element(),
            Some(review) if review.slots.is_empty() => div()
                .text_size(theme::text_ui())
                .italic()
                .text_color(theme::faint())
                .child(crate::i18n::text("ui.usage_none"))
                .into_any_element(),
            Some(review) => {
                let marked = costliest(&review.slots).map(str::to_owned);
                let mut list = div().flex().flex_col().gap_2();
                for slot in &review.slots {
                    list = list.child(slot_row(slot, marked.as_deref() == Some(slot.handle.as_str())));
                }
                list.child(
                    div()
                        .text_size(theme::text_caption())
                        .text_color(theme::faint())
                        .child(crate::i18n::text("ui.usage_counts_note")),
                )
                .into_any_element()
            }
        };

        let mut sections = vec![meta_section(crate::i18n::text("ui.rating"), rating_body)];
        let has_calls = self
            .review
            .review
            .as_ref()
            .is_some_and(|review| !review.calls.is_empty());
        if let Some(card) = self.render_usage_card(cx) {
            sections.push(card);
        }
        if !has_calls {
            sections.push(meta_section(crate::i18n::text("ui.usage"), usage_body));
        }
        sections
    }

    /// The allowance card: the mission's points, the week's meter and where the
    /// points went. `None` when nothing about the allowance is known yet.
    fn render_usage_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let review = self.review.review.as_ref()?;
        if review.budget.is_empty() && review.calls.is_empty() {
            return None;
        }
        let root = cx.entity();
        let week = review.budget.iter().find(|budget| {
            budget.window == "Week" && budget.attributed_low.is_some() && budget.attributed_high.is_some()
        });
        let tint: Hsla = week
            .and_then(|budget| runner_core::protocol::Runtime::parse(&budget.runtime))
            .map(|runtime| crate::runtime_ui::runtime_ui(runtime).tint)
            .unwrap_or_else(theme::accent);

        let mut card = div().flex().flex_col().gap_3();
        match week {
            Some(budget) => {
                let range = Range {
                    low: budget.attributed_low.unwrap_or(0.0),
                    high: budget.attributed_high.unwrap_or(0.0),
                };
                let total = budget.window_points.unwrap_or(0.0);
                let runtime = runner_core::protocol::Runtime::parse(&budget.runtime)
                    .map_or_else(|| budget.runtime.clone(), |r| r.display_name().to_owned());
                let figure = match usage_chart::headline(range) {
                    Headline::Under(value) => crate::i18n::tf("ui.usage_headline_under", &[("value", value)]),
                    Headline::UpTo(value) => crate::i18n::tf("ui.usage_up_to", &[("value", value)]),
                    Headline::One(value) => value,
                    Headline::Between(low, high) => {
                        crate::i18n::tf("ui.usage_between", &[("low", low), ("high", high)])
                    }
                };
                card = card
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(22.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(figure),
                            )
                            .child(
                                div()
                                    .text_size(theme::text_ui())
                                    .text_color(theme::muted())
                                    .child(crate::i18n::text("ui.usage_points_unit")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_1()
                            .child(div().flex_none().pt(px(1.)).child(runtime_logo(&budget.runtime, 14.)))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .flex_1()
                                    .text_size(theme::text_meta())
                                    .text_color(theme::muted())
                                    .child(crate::i18n::tf(
                                        "ui.usage_of_week",
                                        &[("runtime", runtime), ("total", format!("{total:.0}"))],
                                    )),
                            ),
                    )
                    .child(week_meter(usage_chart::meter(total, range), tint))
                    .child(meter_legend(range, total, tint));
                if total < 5.0 {
                    card = card.child(
                        div()
                            .text_size(theme::text_meta())
                            .italic()
                            .text_color(theme::faint())
                            .child(crate::i18n::text("ui.budget_early")),
                    );
                }
            }
            None => {
                for budget in &review.budget {
                    card = card.child(budget_row(budget));
                }
            }
        }

        if !review.calls.is_empty() {
            let handle_of = |key: &str| {
                self.sessions
                    .iter()
                    .find(|row| row.agent_session_key.as_deref() == Some(key))
                    .map(|row| row.handle.clone())
            };
            let chart = usage_chart::build_chart(&review.calls, self.review.mode, &handle_of);
            let mode = if chart.has_points { self.review.mode } else { UsageMode::Tokens };
            card = card.child(self.render_waterfall(&chart, mode, tint, &root));
        } else {
            card = card.child(
                div()
                    .text_size(theme::text_meta())
                    .italic()
                    .text_color(theme::faint())
                    .child(crate::i18n::text("ui.usage_no_calls")),
            );
        }
        card = card.child(
            div()
                .text_size(theme::text_caption())
                .text_color(theme::faint())
                .child(crate::i18n::text("ui.budget_note")),
        );
        Some(meta_section(crate::i18n::text("ui.budget"), card))
    }

    fn render_waterfall(
        &self,
        chart: &usage_chart::Chart,
        mode: UsageMode,
        tint: Hsla,
        root: &Entity<Self>,
    ) -> AnyElement {
        let toggle = |id: &'static str, label: &'static str, target: UsageMode, enabled: bool| {
            let root = root.clone();
            Button::new(id, label)
                .size(ButtonSize::Sm)
                .variant(if mode == target {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Outline
                })
                .disabled(!enabled)
                .on_press(move |_, cx| {
                    let _ = root.update(cx, |this, cx| {
                        this.review.mode = target;
                        cx.notify();
                    });
                })
        };
        let mut header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .text_size(theme::text_ui())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(crate::i18n::text("ui.usage_where")),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(toggle("usage-mode-points", crate::i18n::text("ui.usage_mode_points"), UsageMode::Points, chart.has_points))
                    .child(toggle("usage-mode-tokens", crate::i18n::text("ui.usage_mode_tokens"), UsageMode::Tokens, true)),
            );
        let mut body = div().flex().flex_col().gap_3();
        if !chart.has_points {
            body = body.child(
                div()
                    .text_size(theme::text_meta())
                    .italic()
                    .text_color(theme::faint())
                    .child(crate::i18n::text("ui.usage_tokens_only")),
            );
        }
        if let Some((_, handle)) = chart.largest.clone() {
            let root = root.clone();
            let open = self.review.expanded.contains(&handle);
            body = body.child(
                div().flex().child(
                    Button::new(
                        "usage-show-largest",
                        crate::i18n::text(if open {
                            "ui.usage_hide_largest"
                        } else {
                            "ui.usage_show_largest"
                        }),
                    )
                    .size(ButtonSize::Sm)
                    .variant(ButtonVariant::Outline)
                    .on_press(move |_, cx| {
                        let handle = handle.clone();
                        let _ = root.update(cx, |this, cx| {
                            // Opens the slot holding the largest call, or closes it again.
                            if !this.review.expanded.remove(&handle) {
                                this.review.expanded.insert(handle);
                            }
                            cx.notify();
                        });
                    }),
                ),
            );
        }
        for group in &chart.groups {
            let open = self.review.expanded.contains(&group.handle);
            body = body.child(slot_group(group, chart.points_mid, mode, tint, open, root));
        }
        header = header.items_center();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

/// A runtime's logo in its own colour, as the sidebar footer draws it.
fn runtime_logo(runtime: &str, size: f32) -> AnyElement {
    let icon = crate::chat_icon::ChatIcon::for_runtime(runtime);
    icon.render(px(size), icon.color(theme::text(), true), true)
}

fn with_alpha(mut color: Hsla, alpha: f32) -> Hsla {
    color.a = alpha;
    color
}

/// A share of a whole drawn as a bar; never narrower than `MIN_BAR`.
fn share_bar(share: f32, color: Hsla) -> AnyElement {
    div()
        .w_full()
        .h(px(6.))
        .rounded(px(3.))
        .bg(theme::border())
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .rounded(px(3.))
                .w(relative(share.clamp(0.0, 1.0).max(usage_chart::MIN_BAR)))
                .bg(color),
        )
        .into_any_element()
}

/// The week's allowance as one bar: others' use, this mission's range, what is left.
fn week_meter(meter: usage_chart::Meter, tint: Hsla) -> AnyElement {
    div()
        .w_full()
        .h(px(10.))
        .rounded(px(5.))
        .bg(theme::border())
        .overflow_hidden()
        .flex()
        .child(div().h_full().w(relative(meter.others)).bg(theme::faint()))
        .child(div().h_full().min_w(px(2.)).w(relative(meter.mission_solid)).bg(tint))
        .child(div().h_full().w(relative(meter.mission_range)).bg(with_alpha(tint, 0.45)))
        .into_any_element()
}

fn legend_item(color: Hsla, text: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(div().size(px(8.)).rounded(px(2.)).bg(color))
        .child(
            div()
                .text_size(theme::text_meta())
                .text_color(theme::muted())
                .child(text),
        )
        .into_any_element()
}

fn meter_legend(range: Range, total: f64, tint: Hsla) -> AnyElement {
    let others = (total - range.mid()).max(0.0);
    let left = (100.0 - total).max(0.0);
    div()
        .flex()
        .flex_wrap()
        .gap_x_3()
        .gap_y_1()
        .child(legend_item(
            theme::faint(),
            crate::i18n::tf(
                "ui.usage_legend_others",
                &[("value", usage_chart::points_text(others))],
            ),
        ))
        .child(legend_item(
            tint,
            crate::i18n::tf(
                "ui.usage_legend_mission",
                &[("value", usage_chart::points_text(range.mid()))],
            ),
        ))
        .child(legend_item(
            theme::border_strong(),
            crate::i18n::tf(
                "ui.usage_legend_left",
                &[("value", usage_chart::points_text(left))],
            ),
        ))
        .into_any_element()
}

/// `1.4 to 2.1 pts`, `under 0.1 pts` or the token count, per the chosen unit.
fn value_text(points: Option<Range>, tokens: i64, mode: UsageMode) -> String {
    match (mode, points) {
        (UsageMode::Points, Some(range)) => {
            let value = match usage_chart::headline(range) {
                Headline::Under(value) => crate::i18n::tf("ui.usage_under", &[("value", value)]),
                Headline::UpTo(value) => crate::i18n::tf("ui.usage_up_to", &[("value", value)]),
                Headline::One(value) => value,
                Headline::Between(low, high) => {
                    crate::i18n::tf("ui.usage_between", &[("low", low), ("high", high)])
                }
            };
            format!("{value} {}", crate::i18n::text("ui.usage_pts"))
        }
        _ => compact(tokens),
    }
}

/// A call's value: its share of the mission in points mode (points per call
/// are tiny and alike, the share is what tells calls apart), tokens otherwise.
fn call_value_text(share: f32, tokens: i64, mode: UsageMode) -> String {
    match mode {
        UsageMode::Points if share < 0.01 => crate::i18n::text("ui.usage_share_under_one").to_owned(),
        UsageMode::Points => format!("{:.0}%", share * 100.0),
        UsageMode::Tokens => compact(tokens),
    }
}

fn row_line(label: String, value: String, label_weight: FontWeight) -> AnyElement {
    div()
        .flex()
        .items_baseline()
        .justify_between()
        .gap_2()
        .child(
            div()
                .min_w(px(0.))
                .text_size(theme::text_ui())
                .font_weight(label_weight)
                .text_color(theme::text())
                .child(label),
        )
        .child(
            div()
                .flex_none()
                .font_family(theme::UI_MONOSPACE_FONT)
                .text_size(theme::text_meta())
                .text_color(theme::text())
                .child(value),
        )
        .into_any_element()
}

/// A slot's value: one estimate, its share of the mission's middle figure, so
/// the slots add up to the headline. Tokens in tokens mode.
fn slot_value_text(group: &SlotGroup, points_mid: Option<f64>, mode: UsageMode) -> String {
    match (mode, points_mid) {
        (UsageMode::Points, Some(mid)) => format!(
            "≈ {} {}",
            usage_chart::points_text(group.share as f64 * mid),
            crate::i18n::text("ui.usage_pts")
        ),
        _ => compact(group.tokens),
    }
}

fn slot_group(
    group: &SlotGroup,
    points_mid: Option<f64>,
    mode: UsageMode,
    tint: Hsla,
    open: bool,
    root: &Entity<MissionWorkspace>,
) -> AnyElement {
    let handle = group.handle.clone();
    let toggle_root = root.clone();
    let toggle = Button::new(
        SharedString::from(format!("usage-slot-{}", group.handle)),
        if open {
            crate::i18n::text("ui.usage_hide_calls").to_owned()
        } else {
            crate::i18n::tf("ui.usage_show_calls", &[("count", group.calls.to_string())])
        },
    )
    .size(ButtonSize::Sm)
    .variant(ButtonVariant::Outline)
    .on_press(move |_, cx| {
        let handle = handle.clone();
        let _ = toggle_root.update(cx, |this, cx| {
            if !this.review.expanded.remove(&handle) {
                this.review.expanded.insert(handle);
            }
            cx.notify();
        });
    });
    let known = group.handle != "?";
    let name = if known {
        div()
            .min_w(px(0.))
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_none().child(RoleAvatar::new(group.handle.clone(), 20.)))
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .font_family(theme::UI_MONOSPACE_FONT)
                    .text_size(theme::text_body())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(hue_for_seed(&group.handle).color())
                    .child(format!("@{}", group.handle)),
            )
    } else {
        div().min_w(px(0.)).child(
            div()
                .text_size(theme::text_ui())
                .italic()
                .text_color(theme::faint())
                .child(crate::i18n::text("ui.usage_unknown_slot")),
        )
    }
    .child(div().flex_none().child(runtime_logo(&group.runtime, 13.)));
    let mut block = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(name)
                .child(
                    div()
                        .flex_none()
                        .font_family(theme::UI_MONOSPACE_FONT)
                        .text_size(theme::text_meta())
                        .text_color(theme::text())
                        .child(slot_value_text(group, points_mid, mode)),
                ),
        )
        .child(share_bar(group.share, tint))
        .child(
            div()
                .text_size(theme::text_caption())
                .text_color(theme::faint())
                .child(crate::i18n::tf(
                    "ui.usage_slot_share",
                    &[("percent", format!("{:.0}", group.share * 100.0))],
                )),
        )
        .child(div().flex().child(toggle));
    if open {
        let mut calls = div().flex().flex_col().gap_2().pl_2();
        for row in &group.shown {
            let label = if row.largest {
                format!("{}  ·  {}", row.time, crate::i18n::text("ui.usage_largest"))
            } else {
                row.time.clone()
            };
            calls = calls.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(row_line(
                        label,
                        call_value_text(row.share, row.tokens, mode),
                        if row.largest { FontWeight::SEMIBOLD } else { FontWeight::NORMAL },
                    ))
                    .child(share_bar(
                        row.share,
                        if row.largest { tint } else { with_alpha(tint, 0.6) },
                    ))
                    .child(
                        div()
                            .text_size(theme::text_caption())
                            .text_color(theme::faint())
                            .child(crate::i18n::tf(
                                "ui.usage_call_detail",
                                &[
                                    ("input", compact(row.input)),
                                    ("cached", compact(row.cached)),
                                    ("output", compact(row.output)),
                                    ("points", value_text(row.points, row.tokens, UsageMode::Points)),
                                ],
                            )),
                    ),
            );
        }
        if group.rest_calls > 0 {
            calls = calls.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(row_line(
                        crate::i18n::tf(
                            "ui.usage_smaller_calls",
                            &[("count", group.rest_calls.to_string())],
                        ),
                        call_value_text(group.rest_share, group.rest_tokens, mode),
                        FontWeight::NORMAL,
                    ))
                    .child(share_bar(group.rest_share, with_alpha(tint, 0.35))),
            );
        }
        block = block.child(calls);
    }
    block.into_any_element()
}

/// One allowance window of one runtime: the share the mission used, the
/// readings behind it and how many other Runner sessions overlapped.
fn budget_row(budget: &MissionBudget) -> AnyElement {
    let runtime = runner_core::protocol::Runtime::parse(&budget.runtime)
        .map_or_else(|| budget.runtime.clone(), |runtime| runtime.display_name().to_owned());
    let figure = match budget.state {
        BudgetState::Measured => match budget.used_points {
            Some(points) if points >= 1.0 => crate::i18n::tf(
                "ui.budget_about",
                &[("points", format!("{points:.0}"))],
            ),
            _ => crate::i18n::text("ui.budget_under_one").to_owned(),
        },
        BudgetState::NotYetMeasured => crate::i18n::text("ui.budget_measuring").to_owned(),
        BudgetState::SpannedReset => crate::i18n::text("ui.budget_reset").to_owned(),
    };
    let fmt = |value: f64| {
        if value >= 10.0 {
            format!("{value:.0}")
        } else {
            format!("{value:.1}")
        }
    };
    let split = match (budget.attributed_low, budget.attributed_high, budget.window_points) {
        (Some(low), Some(high), Some(total)) => Some(if (high - low).abs() < 0.05 {
            crate::i18n::tf(
                "ui.budget_split_one",
                &[("points", fmt(high)), ("total", format!("{total:.0}"))],
            )
        } else {
            crate::i18n::tf(
                "ui.budget_split",
                &[("low", fmt(low)), ("high", fmt(high)), ("total", format!("{total:.0}"))],
            )
        }),
        _ => None,
    };
    let share = match (budget.share_low, budget.share_high) {
        (Some(low), Some(high)) => Some(crate::i18n::tf(
            "ui.budget_share",
            &[
                ("low", format!("{:.1}", low * 100.0)),
                ("high", format!("{:.1}", high * 100.0)),
                ("margin", fmt(budget.rounding_margin.unwrap_or(0.0).max(0.05))),
            ],
        )),
        _ => None,
    };
    let early = budget.window_points.is_some_and(|total| total < 5.0);
    let readings = crate::i18n::tf(
        "ui.budget_range",
        &[
            ("from", format!("{:.0}", budget.first_percent)),
            ("to", format!("{:.0}", budget.latest_percent)),
        ],
    );
    let overlap = if budget.other_sessions == 0 {
        crate::i18n::text("ui.budget_alone").to_owned()
    } else {
        crate::i18n::tf(
            "ui.budget_shared",
            &[("count", budget.other_sessions.to_string())],
        )
    };
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(theme::text_ui())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(format!("{runtime} · {}", budget.window)),
                ),
        )
        .child(
            div()
                .text_size(theme::text_ui())
                .text_color(theme::text())
                .child(split.clone().unwrap_or_else(|| figure.clone())),
        )
        .children(share.map(|share| {
            div()
                .text_size(theme::text_meta())
                .text_color(theme::muted())
                .child(share)
        }))
        .when(early && split.is_some(), |row| {
            row.child(
                div()
                    .text_size(theme::text_meta())
                    .italic()
                    .text_color(theme::faint())
                    .child(crate::i18n::text("ui.budget_early")),
            )
        })
        .when(
            budget.state != BudgetState::NotYetMeasured || split.is_some(),
            |row| {
                let detail = if split.is_some() {
                    crate::i18n::tf(
                        "ui.budget_delta",
                        &[("figure", figure.clone()), ("readings", readings.clone())],
                    )
                } else {
                    format!("{readings}  ·  {overlap}")
                };
                row.child(
                    div()
                        .text_size(theme::text_meta())
                        .text_color(theme::muted())
                        .child(detail),
                )
            },
        )
        .into_any_element()
}

fn slot_row(slot: &SlotUsage, costliest: bool) -> AnyElement {
    let detail = match slot.not_recorded_reason.as_deref() {
        Some(reason) => div()
            .text_size(theme::text_meta())
            .italic()
            .text_color(theme::faint())
            .child(format!("{}: {reason}", crate::i18n::text("ui.usage_none")))
            .into_any_element(),
        None => {
            let part = |label: &'static str, value: Option<i64>| {
                value.map(|value| format!("{} {}", crate::i18n::text(label), compact(value)))
            };
            let parts = [
                part("ui.usage_input", slot.input_tokens),
                part("ui.usage_output", slot.output_tokens),
                part("ui.usage_cached", slot.cached_tokens),
                part("ui.usage_steps", slot.steps),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("  ·  ");
            div()
                .font_family(theme::UI_MONOSPACE_FONT)
                .text_size(theme::text_meta())
                .text_color(theme::muted())
                .child(parts)
                .into_any_element()
        }
    };
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(theme::text_ui())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(slot.handle.clone()),
                )
                .children(slot.runtime.clone().map(|runtime| {
                    div()
                        .text_size(theme::text_meta())
                        .text_color(theme::faint())
                        .child(runtime)
                }))
                .children(costliest.then(|| {
                    div()
                        .text_size(theme::text_caption())
                        .text_color(theme::muted())
                        .child(crate::i18n::text("ui.usage_most_input"))
                })),
        )
        .child(detail)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(handle: &str, runtime: &str, input: Option<i64>) -> SlotUsage {
        SlotUsage {
            handle: handle.into(),
            runtime: Some(runtime.into()),
            input_tokens: input,
            ..SlotUsage::default()
        }
    }

    #[test]
    fn counts_are_compact_and_never_round_small_ones() {
        assert_eq!(compact(7), "7");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_000), "1k");
        assert_eq!(compact(390_509), "391k");
        assert_eq!(compact(42_959_037), "43.0M");
    }

    #[test]
    fn the_costliest_slot_is_marked_only_within_one_runtime() {
        let one_runtime = [slot("a", "muse", Some(5)), slot("b", "muse", Some(9)), slot("c", "muse", None)];
        assert_eq!(costliest(&one_runtime), Some("b"));
        let mixed = [slot("a", "muse", Some(5)), slot("b", "codex", Some(9))];
        assert_eq!(costliest(&mixed), None, "counts across runtimes are not comparable");
        assert_eq!(costliest(&[slot("a", "muse", None)]), None);
    }
}
