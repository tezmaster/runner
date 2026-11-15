//! The Usage page: how much of the weekly allowance each recent mission used,
//! on a clock shared with a daily summary, with a queue of the expensive
//! missions that went badly or were never rated. Local missions only.
//!
//! What the page shows is worked out in `usage_timeline`; this file reads the
//! missions, holds the page's choices and draws it.

use chrono::Utc;
use gpui::prelude::*;
use gpui::{canvas, div, point, px, relative, rems, AnyElement, FontWeight, Hsla, PathBuilder, SharedString};
use runner_app::ui::list::LIST_PAGE_PADDING_X;
use runner_app::ui::{Button, ButtonSize, ButtonVariant};
use runner_core::protocol::{MissionRateInput, MissionUsageSummary, RatingVerdict};

use super::usage_timeline::{
    self as timeline, CurvePoint, Filter, Flag, Lane, Timeline, DAY_CHOICES, DEFAULT_EXPENSIVE, EXPENSIVE_CHOICES, FILTERS,
};
use super::*;

/// The mission column, and the points column: a bar scaled to the largest
/// mission, then its number over its range.
const LABEL_W: f32 = 248.;
const POINTS_BAR_W: f32 = 84.;
const POINTS_TEXT_W: f32 = 98.;
const VALUE_W: f32 = POINTS_BAR_W + 10. + POINTS_TEXT_W;
const ROW_H: f32 = 54.;
/// Queue cards listed before the rest are counted.
const QUEUE_SHOWN: usize = 6;
/// Window width, in pixels, under which the queue drops below the lanes.
const STACK_BELOW: f32 = 1180.;

pub(crate) struct MissionUsageState {
    list: Vec<MissionUsageSummary>,
    generation: u64,
    loaded: bool,
    days: u32,
    filter: Filter,
    expensive: f64,
    scroll: gpui::ScrollHandle,
    scrollbar: Entity<Scrollbar>,
}

impl MissionUsageState {
    pub(crate) fn new(cx: &mut Context<NativeRoot>) -> Self {
        let scroll = gpui::ScrollHandle::new();
        let owner = cx.entity_id();
        let scrollbar = {
            let handle = scroll.clone();
            cx.new(|_| Scrollbar::app(handle, owner))
        };
        Self {
            list: Vec::new(),
            generation: 0,
            loaded: false,
            days: DAY_CHOICES[0],
            filter: Filter::All,
            expensive: DEFAULT_EXPENSIVE,
            scroll,
            scrollbar,
        }
    }
}

fn with_alpha(mut color: Hsla, alpha: f32) -> Hsla {
    color.a = alpha;
    color
}

fn flag_color(flag: Flag) -> Option<Hsla> {
    match flag {
        Flag::WentBadly => Some(theme::danger()),
        Flag::NotRated => Some(theme::warning()),
        Flag::None => None,
    }
}

fn runtime_tint(runtime: Option<&str>) -> Hsla {
    runtime
        .and_then(runner_core::protocol::Runtime::parse)
        .map_or_else(theme::accent, |runtime| crate::runtime_ui::runtime_ui(runtime).tint)
}

fn runtime_logo(runtime: Option<&str>, size: f32) -> AnyElement {
    let icon = crate::chat_icon::ChatIcon::for_runtime(runtime.unwrap_or(""));
    icon.render(px(size), icon.color(theme::text(), true), true)
}

fn points(value: f64) -> String {
    crate::surfaces::mission_workspace::points_text(value)
}

fn filter_label(filter: Filter) -> &'static str {
    match filter {
        Filter::All => crate::i18n::text("ui.usage_filter_all"),
        Filter::Expensive => crate::i18n::text("ui.usage_filter_expensive"),
        Filter::WentBadly => crate::i18n::text("ui.usage_filter_bad"),
        Filter::NotRated => crate::i18n::text("ui.usage_filter_unrated"),
    }
}

/// A group of segments in one rounded track, the active one raised: the
/// filter and the period read as one control each, not as loose buttons.
fn segmented<T: std::marker::Copy + PartialEq + 'static>(
    prefix: &str,
    items: Vec<(T, String)>,
    active: T,
    on_select: impl Fn(T, &mut App) + 'static,
) -> AnyElement {
    let on_select = std::rc::Rc::new(on_select);
    let mut track = div()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(3.))
        .rounded_lg()
        .bg(with_alpha(theme::border(), 0.45));
    for (index, (value, label)) in items.into_iter().enumerate() {
        let on_select = on_select.clone();
        let on = value == active;
        track = track.child(
            div()
                .id(SharedString::from(format!("{prefix}-{index}")))
                .px_3()
                .py(px(5.))
                .rounded_md()
                .cursor_pointer()
                .text_size(theme::text_ui())
                .when(on, |segment| {
                    segment
                        .bg(theme::raised())
                        .border_1()
                        .border_color(theme::border())
                        .text_color(theme::text())
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!on, |segment| {
                    segment
                        .border_1()
                        .border_color(with_alpha(theme::border(), 0.0))
                        .text_color(theme::muted())
                        .hover(|segment| segment.text_color(theme::text()))
                })
                .on_click(move |_, _, cx| on_select(value, cx))
                .child(label),
        );
    }
    track.into_any_element()
}

fn icon(path: &'static str, size: f32, color: Hsla) -> AnyElement {
    gpui::svg().path(path).size(px(size)).flex_none().text_color(color).into_any_element()
}

fn card() -> gpui::Div {
    div()
        .rounded(px(12.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::raised())
}

/// A pill: a short status beside a name.
fn pill(text: impl Into<SharedString>, ink: Hsla) -> AnyElement {
    div()
        .flex_none()
        .px_2()
        .py(px(1.))
        .rounded_md()
        .bg(with_alpha(ink, 0.14))
        .text_size(theme::text_caption())
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .child(text.into())
        .into_any_element()
}

impl NativeRoot {
    /// Reads the recent missions in the background.
    pub(crate) fn refresh_mission_usage(&mut self, cx: &mut Context<Self>) {
        if !self.remote_client.is_local() {
            return;
        }
        self.mission_usage.generation += 1;
        let generation = self.mission_usage.generation;
        let days = self.mission_usage.days;
        let core = self.core(cx).clone();
        let task = cx.background_spawn(async move { core.mission_usage_list(days) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.mission_usage.generation != generation {
                    return;
                }
                if let Ok(list) = result {
                    this.mission_usage.list = list;
                    this.mission_usage.loaded = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Opens the Usage page, refreshed.
    pub(crate) fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.usage_open = false;
        self.dismiss_sidebar_transients(cx);
        self.set_route(AppRoute::Usage, cx);
        self.refresh_mission_usage(cx);
        window.focus(&self.root_focus, cx);
        cx.notify();
    }

    /// Rates a mission from the page and reads the list again. The note stays
    /// empty: the queue only offers this for a mission with no rating yet.
    fn rate_from_usage(&mut self, mission_id: String, verdict: RatingVerdict, cx: &mut Context<Self>) {
        let core = self.core(cx).clone();
        let task = cx.background_spawn(async move {
            core.mission_rate(MissionRateInput {
                mission_id,
                verdict: Some(verdict),
                note: String::new(),
            })
        });
        cx.spawn(async move |weak, cx| {
            let _ = task.await;
            let _ = weak.update(cx, |this, cx| this.refresh_mission_usage(cx));
        })
        .detach();
    }

    /// The popover's pointer to the page, with how many missions need a look.
    pub(super) fn render_mission_usage_link(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.mission_usage.loaded {
            return None;
        }
        let model = timeline::build(
            &self.mission_usage.list,
            self.mission_usage.days,
            Utc::now(),
            self.mission_usage.expensive,
            Filter::All,
        );
        let flagged = model.summary.flagged;
        let summary = if model.summary.measured == 0 {
            crate::i18n::text("ui.usage_missions_none").to_owned()
        } else {
            crate::i18n::tf(
                "ui.usage_missions_summary",
                &[("count", model.summary.measured.to_string()), ("flagged", flagged.to_string())],
            )
        };
        let root = cx.entity();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(theme::text_caption())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::faint())
                        .child(crate::i18n::text("ui.usage_missions_title")),
                )
                .child(
                    div()
                        .text_size(theme::text_ui())
                        .text_color(if flagged > 0 { theme::warning() } else { theme::muted() })
                        .child(summary),
                )
                .child(
                    div().flex().child(
                        Button::new("open-usage-page", crate::i18n::text("ui.usage_open_page"))
                            .size(ButtonSize::Sm)
                            .variant(ButtonVariant::Outline)
                            .on_press(move |window, cx| {
                                let _ = root.update(cx, |this, cx| this.open_usage(window, cx));
                            }),
                    ),
                )
                .into_any_element(),
        )
    }

    /// The Usage page, laid out as the Mission Timeline block: a summary card
    /// with the allowance curve, a card of lanes on a shared clock, and a
    /// side panel of the missions that need a look.
    pub(crate) fn render_usage_page(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let root = cx.entity();
        let now = Utc::now();
        let (days, current_filter, expensive_at, loaded) = {
            let state = &self.mission_usage;
            (state.days, state.filter, state.expensive, state.loaded)
        };
        let model = timeline::build(&self.mission_usage.list, days, now, expensive_at, current_filter);
        let counts = model.counts;
        let stacked = f32::from(window.viewport_size().width) / self.settings(cx).app_zoom < STACK_BELOW;

        // ---- header: the title, and one filter that drives everything below
        // Amber is for a small mark, not a paragraph: the description stays plain.
        let attention = (model.summary.flagged > 0).then(|| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py(px(3.))
                .rounded_md()
                .bg(with_alpha(theme::warning(), 0.14))
                .child(icon("triangle-alert.svg", 13., theme::warning()))
                .child(
                    div()
                        .text_size(theme::text_ui())
                        .text_color(theme::text())
                        .child(crate::i18n::tf(
                            "ui.usage_stat_look",
                            &[("count", model.summary.flagged.to_string())],
                        )),
                )
        });
        let filter_root = root.clone();
        let filter = segmented(
            "usage-filter",
            FILTERS
                .iter()
                .map(|filter| (*filter, format!("{} {}", filter_label(*filter), counts.of(*filter))))
                .collect(),
            current_filter,
            move |filter, cx| {
                let _ = filter_root.update(cx, |this, cx| {
                    this.mission_usage.filter = filter;
                    cx.notify();
                });
            },
        );
        let header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(theme::text_display_lg())
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme::text())
                                    .child(crate::i18n::text("ui.usage_page_title")),
                            )
                            .children(attention),
                    )
                    .child(
                        div()
                            .text_size(theme::text_title())
                            .text_color(theme::muted())
                            .child(crate::i18n::text("ui.usage_page_description")),
                    ),
            )
            .child(filter);

        // ---- body
        let body = if !loaded {
            div()
                .text_size(theme::text_title())
                .text_color(theme::muted())
                .child(crate::i18n::text("ui.loading"))
                .into_any_element()
        } else if counts.all == 0 {
            card()
                .max_w(px(520.))
                .flex()
                .flex_col()
                .gap_2()
                .px_6()
                .py_5()
                .child(
                    div()
                        .text_size(theme::text_body())
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(crate::i18n::text("ui.usage_page_empty_title")),
                )
                .child(
                    div()
                        .text_size(theme::text_ui())
                        .text_color(theme::muted())
                        .child(crate::i18n::text("ui.usage_page_empty_body")),
                )
                .into_any_element()
        } else {
            let main = div()
                .min_h(px(0.))
                .min_w(px(0.))
                .flex_1()
                .flex()
                .flex_col()
                .gap_4()
                .child(self.render_usage_summary(&model, days, cx))
                .child(self.render_usage_lanes(&model, days, expensive_at, now, cx))
                .into_any_element();
            let queue = self.render_usage_queue(&model, cx);
            if stacked {
                div()
                    .min_h(px(0.))
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(main)
                    .child(div().flex_none().child(queue))
                    .into_any_element()
            } else {
                div()
                    .min_h(px(0.))
                    .flex_1()
                    .flex()
                    .gap_4()
                    .child(main)
                    .child(div().w(px(330.)).flex_none().child(queue))
                    .into_any_element()
            }
        };

        div()
            .min_h(px(0.))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .min_h(px(0.))
                    .w_full()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(rems(20. / 16.))
                    .px(rems(LIST_PAGE_PADDING_X / 16.))
                    .pt(rems(36. / 16.))
                    .pb(rems(16. / 16.))
                    .child(header)
                    .child(body),
            )
            .into_any_element()
    }

    /// "Allowance used": the total, what it was made of, and the curve that
    /// shows the week's allowance being spent mission by mission.
    fn render_usage_summary(&mut self, model: &Timeline, days: u32, cx: &mut Context<Self>) -> AnyElement {
        let root = cx.entity();
        let period = segmented(
            "usage-period",
            DAY_CHOICES
                .iter()
                .map(|choice| (*choice, crate::i18n::tf("ui.usage_period", &[("days", choice.to_string())])))
                .collect(),
            days,
            move |choice, cx| {
                let _ = root.update(cx, |this, cx| {
                    this.mission_usage.days = choice;
                    this.refresh_mission_usage(cx);
                });
            },
        );
        let stat = |path: &'static str, ink: Hsla, count: usize, key: &'static str| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(icon(path, 14., ink))
                .child(
                    div()
                        .text_size(theme::text_ui())
                        .text_color(theme::muted())
                        .child(crate::i18n::tf(key, &[("count", count.to_string())])),
                )
        };
        let summary = model.summary;
        let bad = model.counts.went_badly;
        let stats = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_4()
            .gap_y_1()
            .child(stat("circle-check.svg", theme::muted(), summary.measured, "ui.usage_stat_measured"))
            .child(stat("triangle-alert.svg", theme::warning(), summary.flagged, "ui.usage_stat_look"))
            .child(stat("circle-alert.svg", theme::danger(), bad, "ui.usage_stat_bad"));
        let total = div()
            .flex()
            .items_baseline()
            .gap_2()
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(points(summary.points)),
            )
            .child(
                div()
                    .text_size(theme::text_ui())
                    .text_color(theme::muted())
                    .child(crate::i18n::text("ui.usage_points_unit")),
            );
        let first = model.day_labels.first().cloned().unwrap_or_default();
        let last = crate::i18n::text("ui.usage_today").to_owned();
        card()
            .flex_none()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(theme::text_body())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(crate::i18n::text("ui.usage_card_allowance")),
                            )
                            .child(total),
                    )
                    .child(div().flex().flex_col().items_end().gap_2().child(period).child(stats)),
            )
            .child(allowance_curve(&model.curve, &model.grid))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(theme::text_caption())
                    .text_color(theme::faint())
                    .child(first)
                    .child(last),
            )
            .into_any_element()
    }

    /// One lane per mission on a clock shared with the curve above.
    fn render_usage_lanes(
        &mut self,
        model: &Timeline,
        days: u32,
        expensive_at: f64,
        now: chrono::DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let root = cx.entity();
        let line = segmented(
            "usage-expensive",
            EXPENSIVE_CHOICES.iter().map(|choice| (*choice, points(*choice))).collect(),
            expensive_at,
            move |choice, cx| {
                let _ = root.update(cx, |this, cx| {
                    this.mission_usage.expensive = choice;
                    cx.notify();
                });
            },
        );
        let header = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon("gauge.svg", 16., theme::accent()))
                    .child(
                        div()
                            .text_size(theme::text_body())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(crate::i18n::text("ui.usage_card_missions")),
                    )
                    .child(
                        div()
                            .text_size(theme::text_ui())
                            .text_color(theme::muted())
                            .child(crate::i18n::tf("ui.usage_stat_measured", &[("count", model.summary.measured.to_string())])),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(theme::text_ui())
                            .text_color(theme::muted())
                            .child(crate::i18n::text("ui.usage_expensive_from")),
                    )
                    .child(line),
            );

        // The axis: a label for each day, and today in blue, as the block marks "now".
        let count = model.day_labels.len();
        let mut axis_days = div().flex_1().min_w(px(0.)).flex();
        let label_every = if count <= 14 { 1 } else { 5 };
        for (index, label) in model.day_labels.iter().enumerate() {
            let today = index + 1 == count;
            axis_days = axis_days.child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .flex()
                    .justify_center()
                    .text_size(theme::text_caption())
                    .text_color(if today { theme::accent() } else { theme::faint() })
                    .when(today, |cell| cell.font_weight(FontWeight::SEMIBOLD))
                    .when(index % label_every == 0 || today, |cell| {
                        cell.child(if today { crate::i18n::text("ui.usage_today").to_owned() } else { label.clone() })
                    }),
            );
        }
        let axis = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .child(div().w(px(LABEL_W)).flex_none())
            .child(axis_days)
            .child(div().w(px(VALUE_W)).flex_none());

        let grid = || {
            let mut layer = div().absolute().top_0().bottom_0().left_0().right_0();
            for fraction in model.grid.iter().copied() {
                layer = layer.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(relative(fraction))
                        .w(px(1.))
                        .bg(with_alpha(theme::border(), 0.6)),
                );
            }
            layer
        };

        let mut lanes = div().flex().flex_col();
        for lane in &model.lanes {
            lanes = lanes.child(self.render_usage_lane(lane, now, grid(), cx));
        }
        if model.lanes.is_empty() {
            lanes = lanes.child(
                div()
                    .px_4()
                    .py_4()
                    .text_size(theme::text_ui())
                    .text_color(theme::muted())
                    .child(crate::i18n::text("ui.usage_nothing_matches")),
            );
        }
        let _ = days;

        let mut notes = Vec::new();
        if model.summary.unmeasured > 0 {
            notes.push(crate::i18n::tf(
                "ui.usage_missions_unmeasured",
                &[("count", model.summary.unmeasured.to_string())],
            ));
        }
        notes.push(crate::i18n::text("ui.usage_range_note").to_owned());

        card()
            .min_h(px(0.))
            .flex_1()
            .flex()
            .flex_col()
            .child(header)
            .child(axis)
            .child(
                div()
                    .relative()
                    .min_h(px(0.))
                    .flex_1()
                    .child(
                        div()
                            .id("usage-page-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .scrollbar_width(px(0.))
                            .track_scroll(&self.mission_usage.scroll)
                            .px_2()
                            .child(lanes),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .child(div().relative().size_full().child(self.mission_usage.scrollbar.clone())),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .text_size(theme::text_caption())
                    .text_color(theme::faint())
                    .child(notes.join(" · ")),
            )
            .into_any_element()
    }

    fn render_usage_lane(
        &mut self,
        lane: &Lane,
        now: chrono::DateTime<Utc>,
        grid: gpui::Div,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = lane.mission_id.clone();
        let tint = runtime_tint(lane.runtime.as_deref());
        let when = runner_app::text_util::format_relative_time(lane.started_at, now);
        let runtime = lane
            .runtime
            .as_deref()
            .and_then(runner_core::protocol::Runtime::parse)
            .map_or_else(String::new, |runtime| runtime.display_name().to_owned());
        let subtitle = if runtime.is_empty() { when } else { format!("{runtime} · {when}") };
        // The status beside the name: what the person should know, nothing for the ordinary case.
        let chip = match (lane.flag, lane.verdict) {
            (Flag::WentBadly, _) => Some(pill(crate::i18n::text("ui.usage_flag_bad_short"), theme::danger())),
            (Flag::NotRated, _) => Some(pill(crate::i18n::text("ui.usage_needs_badge"), theme::warning())),
            (Flag::None, Some(RatingVerdict::Down)) => Some(pill(crate::i18n::text("ui.rating_down"), theme::danger())),
            (Flag::None, Some(RatingVerdict::Up)) => Some(pill(crate::i18n::text("ui.rating_up"), theme::accent())),
            (Flag::None, None) => None,
        };
        let flagged = lane.flag != Flag::None;
        let band_ink = if lane.flag == Flag::WentBadly { theme::danger() } else { theme::warning() };
        div()
            .id(SharedString::from(format!("usage-mission-{id}")))
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .h(px(ROW_H))
            .border_b_1()
            .border_color(with_alpha(theme::border(), 0.5))
            .cursor_pointer()
            .hover(|row| row.bg(theme::sidebar_selected()))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_mission(id.clone(), window, cx);
            }))
            .child(
                div()
                    .w(px(LABEL_W))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(34.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::bg())
                            .child(runtime_logo(lane.runtime.as_deref(), 16.)),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
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
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(theme::text_ui())
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme::text())
                                            .child(lane.title.clone()),
                                    )
                                    .children(chip),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(theme::text_caption())
                                    .text_color(theme::faint())
                                    .child(subtitle),
                            ),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(grid)
                    // A mission that needs a look gets a tinted band across its lane, as the block does.
                    .when(flagged, |cell| {
                        cell.child(
                            div()
                                .absolute()
                                .top(px(7.))
                                .bottom(px(7.))
                                .left_0()
                                .right_0()
                                .rounded_md()
                                .bg(with_alpha(band_ink, 0.12))
                                .flex()
                                .items_center()
                                .justify_end()
                                .pr_2()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .px_2()
                                        .py(px(2.))
                                        .rounded_md()
                                        .bg(theme::raised())
                                        .border_1()
                                        .border_color(with_alpha(band_ink, 0.4))
                                        .text_size(theme::text_caption())
                                        .text_color(theme::text())
                                        .child(icon("triangle-alert.svg", 11., band_ink))
                                        .child(crate::i18n::text("ui.usage_needs_badge")),
                                ),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .top(px(16.))
                            .h(px(22.))
                            .left(relative(lane.left))
                            .w(relative(lane.width))
                            .min_w(px(8.))
                            .rounded_md()
                            .border_1()
                            .border_color(with_alpha(tint, 0.6))
                            .bg(with_alpha(tint, 0.3)),
                    ),
            )
            .child(
                div()
                    .w(px(VALUE_W))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        // Solid to the low end of the range, lighter to the high end.
                        div()
                            .relative()
                            .w(px(POINTS_BAR_W))
                            .flex_none()
                            .h(px(8.))
                            .rounded_full()
                            .bg(theme::border())
                            .overflow_hidden()
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left_0()
                                    .w(relative(lane.reach.clamp(0.0, 1.0).max(0.02)))
                                    .bg(with_alpha(tint, 0.4)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left_0()
                                    .w(relative(lane.solid.clamp(0.0, 1.0).max(0.02)))
                                    .bg(tint),
                            ),
                    )
                    .child(
                        div()
                            .w(px(POINTS_TEXT_W))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_family(theme::UI_MONOSPACE_FONT)
                                    .text_size(theme::text_ui())
                                    .text_color(theme::text())
                                    .child(crate::i18n::tf("ui.usage_mission_points", &[("value", points(lane.mid))])),
                            )
                            .child(
                                div()
                                    .text_size(theme::text_caption())
                                    .text_color(theme::faint())
                                    .child(crate::i18n::tf(
                                        "ui.usage_range_small",
                                        &[("low", points(lane.low)), ("high", points(lane.high))],
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The missions that need a look, in a grey panel of white cards, with
    /// the rating one click away for those not yet rated.
    fn render_usage_queue(&mut self, model: &Timeline, cx: &mut Context<Self>) -> AnyElement {
        let root = cx.entity();
        let now = Utc::now();
        let mut cards = div().flex().flex_col().gap_2();
        for lane in model.queue.iter().take(QUEUE_SHOWN) {
            let id = lane.mission_id.clone();
            let open_id = lane.mission_id.clone();
            let ink = flag_color(lane.flag).unwrap_or_else(theme::muted);
            let (status_icon, status_text) = match lane.flag {
                Flag::WentBadly => ("circle-alert.svg", crate::i18n::text("ui.usage_flag_bad_short")),
                _ => ("triangle-alert.svg", crate::i18n::text("ui.usage_flag_unrated_short")),
            };
            let reason = match lane.flag {
                Flag::WentBadly => crate::i18n::tf("ui.usage_reason_bad", &[("points", points(lane.mid))]),
                _ => crate::i18n::tf("ui.usage_reason_unrated", &[("points", points(lane.mid))]),
            };
            let mut actions = div().flex().flex_wrap().items_center().gap_1();
            if lane.flag == Flag::NotRated {
                for (verdict, label, key) in [
                    (RatingVerdict::Up, crate::i18n::text("ui.rating_up"), "up"),
                    (RatingVerdict::Down, crate::i18n::text("ui.rating_down"), "down"),
                ] {
                    let root = root.clone();
                    let id = id.clone();
                    let button_id = SharedString::from(format!("usage-queue-{key}-{id}"));
                    actions = actions.child(
                        Button::new(button_id, label)
                            .size(ButtonSize::Sm)
                            .variant(ButtonVariant::Outline)
                            .on_press(move |_, cx| {
                                let id = id.clone();
                                let _ = root.update(cx, |this, cx| this.rate_from_usage(id, verdict, cx));
                            }),
                    );
                }
            }
            let open_root = root.clone();
            actions = actions.child(
                Button::new(
                    SharedString::from(format!("usage-queue-open-{open_id}")),
                    crate::i18n::text("ui.usage_queue_open"),
                )
                .size(ButtonSize::Sm)
                .variant(ButtonVariant::Ghost)
                .on_press(move |window, cx| {
                    let id = open_id.clone();
                    let _ = open_root.update(cx, |this, cx| this.open_mission(id, window, cx));
                }),
            );
            cards = cards.child(
                card()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_none().child(runtime_logo(lane.runtime.as_deref(), 16.)))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(theme::text_body())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(lane.title.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(theme::text_ui())
                            .text_color(theme::muted())
                            .child(reason),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(icon(status_icon, 14., ink))
                            .child(
                                div()
                                    .text_size(theme::text_ui())
                                    .text_color(theme::muted())
                                    .child(format!(
                                        "{status_text} · {}",
                                        runner_app::text_util::format_relative_time(lane.started_at, now)
                                    )),
                            ),
                    )
                    .child(actions),
            );
        }
        let more = model.queue.len().saturating_sub(QUEUE_SHOWN);
        let body = if model.queue.is_empty() {
            div()
                .px_1()
                .text_size(theme::text_ui())
                .text_color(theme::muted())
                .child(crate::i18n::text("ui.usage_queue_empty"))
                .into_any_element()
        } else {
            cards
                .when(more > 0, |cards| {
                    cards.child(
                        div()
                            .px_1()
                            .text_size(theme::text_caption())
                            .text_color(theme::faint())
                            .child(crate::i18n::tf("ui.usage_queue_more", &[("count", more.to_string())])),
                    )
                })
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .rounded(px(12.))
            .bg(with_alpha(theme::border(), 0.3))
            .p_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .child(icon("triangle-alert.svg", 14., theme::warning()))
                    .child(
                        div()
                            .text_size(theme::text_caption())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::muted())
                            .child(crate::i18n::text("ui.usage_queue_title")),
                    )
                    .child(
                        div()
                            .px_2()
                            .rounded_full()
                            .bg(with_alpha(theme::border(), 0.7))
                            .text_size(theme::text_caption())
                            .text_color(theme::muted())
                            .child(model.queue.len().to_string()),
                    ),
            )
            .child(
                div()
                    .px_1()
                    .text_size(theme::text_caption())
                    .text_color(theme::faint())
                    .child(crate::i18n::text("ui.usage_queue_hint")),
            )
            .child(body)
            .into_any_element()
    }
}

/// The cumulative allowance as a filled curve with a dot for each mission,
/// amber where it needs a look. The same clock as the lanes below it, so a
/// dot sits above the lane it belongs to.
fn allowance_curve(curve: &[CurvePoint], grid: &[f32]) -> AnyElement {
    let points_for_path: Vec<(f32, f32)> = curve.iter().map(|point| (point.x, point.y)).collect();
    let fill = with_alpha(theme::accent(), 0.14);
    let line = with_alpha(theme::accent(), 0.8);
    let mut chart = div().relative().w_full().h(px(104.));
    // Day gridlines, behind the curve.
    for fraction in grid.iter().copied() {
        chart = chart.child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(fraction))
                .w(px(1.))
                .bg(with_alpha(theme::border(), 0.5)),
        );
    }
    chart = chart.child(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                if points_for_path.is_empty() {
                    return;
                }
                let at = |fx: f32, fy: f32| {
                    point(
                        bounds.origin.x + bounds.size.width * fx,
                        bounds.origin.y + bounds.size.height * (1.0 - fy * 0.92),
                    )
                };
                // The curve rests at nothing until the first mission, then climbs.
                let first = points_for_path[0];
                let last = *points_for_path.last().unwrap_or(&first);
                let mut area = PathBuilder::fill();
                area.move_to(at(0.0, 0.0));
                area.line_to(at(first.0, 0.0));
                for (x, y) in &points_for_path {
                    area.line_to(at(*x, *y));
                }
                area.line_to(at(1.0, last.1));
                area.line_to(at(1.0, 0.0));
                area.close();
                if let Ok(path) = area.build() {
                    window.paint_path(path, fill);
                }
                let mut stroke = PathBuilder::stroke(px(1.5));
                stroke.move_to(at(0.0, 0.0));
                stroke.line_to(at(first.0, 0.0));
                for (x, y) in &points_for_path {
                    stroke.line_to(at(*x, *y));
                }
                stroke.line_to(at(1.0, last.1));
                if let Ok(path) = stroke.build() {
                    window.paint_path(path, line);
                }
            },
        )
        .size_full()
        .absolute()
        .top_0()
        .left_0(),
    );
    for point in curve {
        let ink = flag_color(point.flag).unwrap_or_else(theme::accent);
        chart = chart.child(
            div()
                .absolute()
                .left(relative(point.x))
                .bottom(relative(point.y * 0.92))
                .ml(px(-4.))
                .mb(px(-4.))
                .size(px(8.))
                .rounded_full()
                .bg(theme::raised())
                .border_2()
                .border_color(ink),
        );
    }
    chart.into_any_element()
}
