//! Per-minute traffic over time: queries, average time, longest query.
//!
//! # Three panels, not one chart with three lines
//!
//! The three measures are in different units and different orders of magnitude — a
//! count, milliseconds, and seconds. Putting them on one chart would need a second
//! y-axis, which is the single most misleading thing a chart can do: with two scales
//! the crossing point of two lines is an artefact of where the axes were pinned, and
//! readers reliably read it as meaning something.
//!
//! So each measure gets its own panel and **its own scale**. That is the opposite of
//! the load chart, where every panel shows the same measure for a different database
//! and therefore must share one scale to stay comparable. Same layout, opposite rule,
//! for the same underlying reason: a shared axis is only honest between like things.
//!
//! # What each panel is worth
//!
//! - **Queries** and **average** come from `pg_stat_statements` deltas and are exact
//!   for the minute, covering every statement the database ran.
//! - **Longest** is sampled from `pg_stat_activity` every 5 seconds, so it is a
//!   *floor*: a statement that begins and ends between two samples is never seen. The
//!   panel says so, because a floor drawn like a maximum invites the wrong conclusion
//!   from a flat line.

use dioxus::prelude::*;

use super::chart::{ChartPoint, MINUTE_GAP_MS, clock, peak_with_headroom, sample_at, segments};

const VIEW_W: f64 = 1000.0;
const VIEW_H: f64 = 100.0;

/// How a panel's values are written out. Each measure has its own unit, which is the
/// whole reason they are on separate panels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MinuteUnit {
    /// A plain count.
    Count,
    Milliseconds,
    Seconds,
}

impl MinuteUnit {
    fn format(&self, value: f64) -> String {
        match self {
            Self::Count => crate::models::fmt::int(Some(value.round() as i64)),
            Self::Milliseconds => crate::models::fmt::millis(Some(value)),
            Self::Seconds => crate::models::fmt::seconds(Some(value)),
        }
    }

    /// Axis labels are read at a glance and must not be as long as the tooltip.
    fn format_axis(&self, value: f64) -> String {
        match self {
            Self::Count => {
                if value >= 1_000.0 {
                    format!("{:.0}k", value / 1_000.0)
                } else {
                    format!("{:.0}", value)
                }
            }
            Self::Milliseconds => {
                if value >= 1_000.0 {
                    format!("{:.1}s", value / 1_000.0)
                } else {
                    format!("{:.0}ms", value)
                }
            }
            Self::Seconds => {
                if value >= 60.0 {
                    format!("{:.0}m", value / 60.0)
                } else {
                    format!("{:.0}s", value)
                }
            }
        }
    }

    /// Smallest ceiling a panel may scale to, so an idle minute is drawn as idle
    /// rather than as noise magnified to full height.
    fn min_ceiling(&self) -> f64 {
        match self {
            Self::Count => 10.0,
            Self::Milliseconds => 10.0,
            Self::Seconds => 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MinuteSeries {
    pub title: String,
    /// What the number means and, where it matters, how far to trust it.
    pub note: String,
    pub unit: MinuteUnit,
    /// Oldest first.
    pub points: Vec<ChartPoint>,
}

#[component]
fn MinutePanel(
    series: MinuteSeries,
    from_unix_ms: i64,
    to_unix_ms: i64,
    hover: Signal<Option<i64>>,
) -> Element {
    let span_ms = (to_unix_ms - from_unix_ms).max(1) as f64;

    let mut plot_width = use_signal(|| 0.0_f64);

    // Its own scale: this panel's measure has nothing in common with its neighbours'.
    let ceiling = peak_with_headroom(&series.points)
        .unwrap_or(0.0)
        .max(series.unit.min_ceiling());

    let x = |at_unix_ms: i64| {
        ((at_unix_ms - from_unix_ms) as f64 / span_ms * VIEW_W).clamp(0.0, VIEW_W)
    };
    let y = |value: f64| VIEW_H - (value / ceiling * VIEW_H).clamp(0.0, VIEW_H);

    let hovered = hover
        .read()
        .and_then(|at| sample_at(&series.points, at, MINUTE_GAP_MS))
        .map(|point| (point.at_unix_ms, point.value));

    let latest = series.points.last().map(|point| point.value);

    let mut marks: Vec<Element> = Vec::new();

    for (index, segment) in segments(&series.points, MINUTE_GAP_MS).iter().enumerate() {
        if segment.len() == 1 {
            let point = segment[0];
            marks.push(rsx! {
                circle {
                    key: "dot-{index}",
                    class: "chart__dot",
                    cx: "{x(point.at_unix_ms):.2}",
                    cy: "{y(point.value):.2}",
                    r: "2",
                }
            });
            continue;
        }

        let path: String = segment
            .iter()
            .enumerate()
            .map(|(at, point)| {
                let command = if at == 0 { "M" } else { "L" };
                format!("{}{:.2},{:.2}", command, x(point.at_unix_ms), y(point.value))
            })
            .collect::<Vec<_>>()
            .join(" ");

        let first = segment[0];
        let last = segment[segment.len() - 1];

        marks.push(rsx! {
            path {
                key: "area-{index}",
                class: "chart__area",
                d: "{path} L{x(last.at_unix_ms):.2},{VIEW_H} L{x(first.at_unix_ms):.2},{VIEW_H} Z",
            }
        });
        marks.push(rsx! {
            path { key: "line-{index}", class: "chart__line", d: "{path}" }
        });
    }

    rsx! {
        div { class: "panel",
            div { class: "panel__head",
                div { class: "panel__title",
                    span { class: "panel__desc", "{series.title}" }
                    span { class: "panel__path", "{series.note}" }
                }
                div { class: "panel__stats mono",
                    if let Some((_, value)) = hovered {
                        span { class: "panel__stats-hover", "{series.unit.format(value)}" }
                    } else if hover.read().is_some() {
                        span { class: "panel__stats-hover faint", "—" }
                    } else if let Some(latest) = latest {
                        span { title: "Most recent minute", "{series.unit.format(latest)}" }
                    }
                }
            }

            if series.points.is_empty() {
                div { class: "panel__empty", "Nothing recorded for this measure yet." }
            } else {
                div { class: "panel__plot",
                    div { class: "chart__y-axis mono",
                        span { "{series.unit.format_axis(ceiling)}" }
                        span { "0" }
                    }
                    svg {
                        class: "chart__svg",
                        view_box: "0 0 {VIEW_W} {VIEW_H}",
                        preserve_aspect_ratio: "none",

                        onmounted: move |evt| async move {
                            if let Ok(rect) = evt.get_client_rect().await {
                                plot_width.set(rect.size.width);
                            }
                        },
                        onresize: move |evt| {
                            if let Ok(size) = evt.get_content_box_size() {
                                plot_width.set(size.width);
                            }
                        },
                        onmousemove: move |evt| {
                            let width = *plot_width.read();

                            if width <= 0.0 {
                                return;
                            }

                            let fraction = (evt.element_coordinates().x / width).clamp(0.0, 1.0);
                            hover.clone().set(Some(from_unix_ms + (fraction * span_ms) as i64));
                        },
                        onmouseleave: move |_| hover.clone().set(None),

                        line { class: "chart__grid", x1: "0", x2: "{VIEW_W}", y1: "0", y2: "0" }
                        line {
                            class: "chart__grid chart__grid--base",
                            x1: "0",
                            x2: "{VIEW_W}",
                            y1: "{VIEW_H}",
                            y2: "{VIEW_H}",
                        }

                        {marks.into_iter()}

                        if let Some((at, value)) = hovered {
                            line {
                                class: "chart__crosshair",
                                x1: "{x(at):.2}",
                                x2: "{x(at):.2}",
                                y1: "0",
                                y2: "{VIEW_H}",
                            }
                            circle {
                                class: "chart__cursor-dot",
                                cx: "{x(at):.2}",
                                cy: "{y(value):.2}",
                                r: "3",
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn MinuteCharts(
    series: Vec<MinuteSeries>,
    from_unix_ms: i64,
    to_unix_ms: i64,
    empty_note: String,
) -> Element {
    if series.iter().all(|s| s.points.is_empty()) {
        return rsx! {
            div { class: "charts charts--empty",
                p { class: "muted", style: "margin: 0; font-size: 12.5px;", "{empty_note}" }
            }
        };
    }

    // One hover state across all three: the panels share a time axis, so pointing at
    // a minute should read every measure of it at once. Hovering each panel to collect
    // three numbers about the same minute would be the reader doing the chart's job.
    let hover = use_signal(|| None::<i64>);

    let panels: Vec<Element> = series
        .iter()
        .map(|s| {
            rsx! {
                MinutePanel {
                    key: "{s.title}",
                    series: s.clone(),
                    from_unix_ms,
                    to_unix_ms,
                    hover,
                }
            }
        })
        .collect();

    let hovered_at = hover.read().and_then(|at| {
        series
            .iter()
            .filter_map(|s| sample_at(&s.points, at, MINUTE_GAP_MS))
            .min_by_key(|point| (point.at_unix_ms - at).abs())
            .map(|point| point.at_unix_ms)
    });

    rsx! {
        div { class: "charts",
            {panels.into_iter()}

            div { class: "chart__x-axis mono",
                if let Some(at) = hovered_at {
                    span { class: "chart__x-axis-hover", "{clock(at)}" }
                } else {
                    span { "{clock(from_unix_ms)}" }
                    span { "{clock(from_unix_ms + (to_unix_ms - from_unix_ms) / 2)}" }
                    span { "{clock(to_unix_ms)}" }
                }
            }

            div { class: "chart__footnote",
                span { class: "faint",
                    "One point per minute · UTC · a break in a line is a minute that recorded nothing"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_unit_writes_its_axis_labels_short() {
        assert_eq!(MinuteUnit::Count.format_axis(2_500.0), "2k");
        assert_eq!(MinuteUnit::Count.format_axis(42.0), "42");
        assert_eq!(MinuteUnit::Milliseconds.format_axis(2_500.0), "2.5s");
        assert_eq!(MinuteUnit::Milliseconds.format_axis(40.0), "40ms");
        assert_eq!(MinuteUnit::Seconds.format_axis(120.0), "2m");
        assert_eq!(MinuteUnit::Seconds.format_axis(12.0), "12s");
    }

    #[test]
    fn an_idle_measure_is_not_magnified_to_full_height() {
        // Three queries a minute must not be drawn as a wall.
        let quiet = vec![
            ChartPoint { at_unix_ms: 0, value: 2.0 },
            ChartPoint { at_unix_ms: 60_000, value: 3.0 },
        ];

        let ceiling = peak_with_headroom(&quiet)
            .unwrap_or(0.0)
            .max(MinuteUnit::Count.min_ceiling());

        assert_eq!(ceiling, 10.0);
    }

    #[test]
    fn a_busy_measure_scales_past_its_floor() {
        let busy = vec![ChartPoint { at_unix_ms: 0, value: 4_000.0 }];

        let ceiling = peak_with_headroom(&busy)
            .unwrap_or(0.0)
            .max(MinuteUnit::Count.min_ceiling());

        assert!((ceiling - 4_600.0).abs() < 0.001);
    }
}
