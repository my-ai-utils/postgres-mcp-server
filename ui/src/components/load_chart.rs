//! Execution load over the last hour — **one panel per database**.
//!
//! # What it plots
//!
//! `busyBackends`: backend-seconds of execution per wall-clock second, from
//! `pg_stat_database.active_time`. `1.0` means one backend was executing
//! continuously. It is *not* CPU — a backend waiting on disk or on a lock is still
//! `active` and still accrues the time — and the card says so rather than being
//! labelled with a percentage it cannot deliver.
//!
//! # Why panels rather than one chart with N coloured lines
//!
//! `active_time` is a column of `pg_stat_database`, which has a row per **database**;
//! Postgres exposes no cluster-wide equivalent. So there is one series per database
//! and no meaningful way to add them up:
//!
//! - summing double-counts a database that is mounted twice (a read path and a write
//!   path onto the same database read the very same counter), and
//! - a sum silently *drops* when one database misses a tick, which reads as load
//!   falling exactly when the server is too busy to answer.
//!
//! Separate panels have neither problem. They also sidestep the categorical-colour
//! question: with N lines on one chart, a validated palette runs out — checked
//! all-pairs, olive/orange are indistinguishable under deuteranopia and blue/violet
//! are borderline for normal vision — whereas panels carry identity in their titles
//! and need exactly one hue.
//!
//! # Decisions worth not undoing
//!
//! - **One y-scale across every panel**, so the panels are comparable. Per-panel
//!   auto-scaling would draw an idle database's rounding noise at the same height as
//!   a saturated one's real load.
//! - **Gaps are drawn as gaps.** A tick against an unreachable database records
//!   nothing; the line breaks rather than being joined by a straight segment, which
//!   would be an invented measurement across exactly the window that is missing.
//! - **The ceiling never drops below [`MIN_CEILING`]**, which is also the meaningful
//!   threshold — one backend saturated.
//! - **Text lives in HTML, not in the SVG.** Panels are stretched horizontally
//!   (`preserveAspectRatio: none`) to fill the card; marks survive that via
//!   `vector-effect: non-scaling-stroke`, which text has no equivalent of.

use dioxus::prelude::*;

/// Break the line when samples are further apart than this. The collector writes
/// every 5 seconds, so this is "several ticks missing", not "one ran late".
const GAP_MS: i64 = 30_000;

/// How far the pointer may be from a sample and still read it.
///
/// Same window as [`GAP_MS`], and for the same reason: inside a recording gap there
/// is no sample to report, so the readout says so instead of snapping to whatever
/// lies minutes away on either side. Without this the crosshair would happily
/// display a number from before an outage while hovering the middle of it.
const SNAP_MS: i64 = GAP_MS;

/// The shared y-axis never scales below this, so an idle hour is drawn as an idle
/// hour. It is also the threshold worth seeing — one backend executing continuously.
const MIN_CEILING: f64 = 1.0;

/// Panel geometry in viewBox units. The box is stretched to the card's width, so
/// only the vertical proportion is real.
const VIEW_W: f64 = 1000.0;
const VIEW_H: f64 = 100.0;

#[derive(Clone, Debug, PartialEq)]
pub struct LoadChartPoint {
    pub at_unix_ms: i64,
    pub value: f64,
}

/// One database's series, with the identity shown as the panel's title.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadSeries {
    pub description: String,
    pub path: String,
    /// Oldest first.
    pub points: Vec<LoadChartPoint>,
    /// Set when this database's history could not be read, or has nothing in it.
    pub note: Option<String>,
    /// True when another mount already plotted this same database — two mounts can
    /// point at one database, and their series are then the same counter twice.
    pub duplicate_of: Option<String>,
}

/// Splits a series wherever a gap in recording makes a connecting line a lie.
fn segments(points: &[LoadChartPoint]) -> Vec<Vec<&LoadChartPoint>> {
    let mut result: Vec<Vec<&LoadChartPoint>> = Vec::new();
    let mut current: Vec<&LoadChartPoint> = Vec::new();

    for point in points {
        if let Some(previous) = current.last() {
            if point.at_unix_ms - previous.at_unix_ms > GAP_MS {
                result.push(std::mem::take(&mut current));
            }
        }

        current.push(point);
    }

    if !current.is_empty() {
        result.push(current);
    }

    result
}

/// One ceiling for every panel, so their heights mean the same thing.
pub fn shared_ceiling(series: &[LoadSeries]) -> f64 {
    let peak = series
        .iter()
        .flat_map(|s| s.points.iter())
        .map(|point| point.value)
        .fold(0.0_f64, f64::max);

    // Headroom so the tallest peak is not drawn touching the top edge.
    (peak * 1.15).max(MIN_CEILING)
}

/// The sample nearest `at_unix_ms`, or `None` when the pointer is inside a gap —
/// further than [`SNAP_MS`] from any sample this series actually has.
fn sample_at(points: &[LoadChartPoint], at_unix_ms: i64) -> Option<&LoadChartPoint> {
    points
        .iter()
        .min_by_key(|point| (point.at_unix_ms - at_unix_ms).abs())
        .filter(|point| (point.at_unix_ms - at_unix_ms).abs() <= SNAP_MS)
}

/// `hh:mm:ss` (UTC) from epoch milliseconds.
///
/// Hand-rolled to keep a date library out of the wasm bundle for one label row, and
/// UTC on purpose: every other timestamp on this page comes from the server as UTC,
/// and one row quietly in local time would not line up with them.
fn clock(at_unix_ms: i64) -> String {
    let seconds = at_unix_ms.div_euclid(1_000).rem_euclid(86_400);

    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

#[component]
fn LoadPanel(
    series: LoadSeries,
    ceiling: f64,
    from_unix_ms: i64,
    to_unix_ms: i64,
    /// The instant the pointer is on, shared by every panel so one crosshair reads
    /// every database at once. `None` when the pointer is not over the charts.
    hover: Signal<Option<i64>>,
) -> Element {
    let span_ms = (to_unix_ms - from_unix_ms).max(1) as f64;

    // The plot is stretched to the card's width, so a pointer position in CSS pixels
    // only becomes a time once that width is known. Measured on mount and again on
    // resize rather than assumed.
    let mut plot_width = use_signal(|| 0.0_f64);

    let x = |at_unix_ms: i64| {
        ((at_unix_ms - from_unix_ms) as f64 / span_ms * VIEW_W).clamp(0.0, VIEW_W)
    };
    let y = |value: f64| VIEW_H - (value / ceiling * VIEW_H).clamp(0.0, VIEW_H);

    let latest = series.points.last().map(|point| point.value);
    let peak = series
        .points
        .iter()
        .map(|point| point.value)
        .fold(0.0_f64, f64::max);

    let mut marks: Vec<Element> = Vec::new();

    for (index, segment) in segments(&series.points).iter().enumerate() {
        // A lone sample has no line to draw; a dot keeps a single tick that survived
        // an outage visible instead of silently absent.
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

    let threshold_y = y(MIN_CEILING);
    let show_threshold = ceiling > MIN_CEILING * 0.75;

    // What the crosshair is pointing at in *this* series. `None` while the pointer is
    // away, or over a stretch this database recorded nothing for.
    let hovered = hover.read().and_then(|at| {
        sample_at(&series.points, at).map(|point| (point.at_unix_ms, point.value))
    });

    rsx! {
        div { class: "panel",
            div { class: "panel__head",
                div { class: "panel__title",
                    span { class: "panel__desc", "{series.description}" }
                    span { class: "panel__path mono", "{series.path}" }
                }
                div { class: "panel__stats mono",
                    // Under the pointer the readout becomes the value at that instant;
                    // "now" and "peak" stay visible without hovering, so the hover
                    // layer only ever adds detail — it never gates a number.
                    if let Some((_, value)) = hovered {
                        span { class: "panel__stats-hover", title: "Value at the crosshair", "{value:.2}" }
                    } else if hover.read().is_some() {
                        span { class: "panel__stats-hover faint", title: "This database recorded nothing here", "—" }
                    } else if let Some(latest) = latest {
                        span { title: "Most recent sample", "{latest:.2}" }
                    }
                    if let Some(_) = latest {
                        span { class: "faint", title: "Highest sample in the window", "peak {peak:.2}" }
                    }
                }
            }

            if let Some(duplicate_of) = series.duplicate_of.clone() {
                div { class: "panel__note",
                    "Same database as "
                    b { class: "mono", "{duplicate_of}" }
                    " — these two mounts read the same counters, so this line is a copy, not extra load."
                }
            }

            if series.points.is_empty() {
                div { class: "panel__empty",
                    "{series.note.clone().unwrap_or_else(|| \"Nothing recorded for this database yet.\".to_string())}"
                }
            } else {
                div { class: "panel__plot",
                    div { class: "chart__y-axis mono",
                        span { "{ceiling:.2}" }
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

                        // The whole plot is the hit target, so the pointer only has to
                        // be at the right *time* — never on top of a 2px line.
                        onmousemove: move |evt| {
                            let width = *plot_width.read();

                            if width <= 0.0 {
                                return;
                            }

                            let fraction = (evt.element_coordinates().x / width).clamp(0.0, 1.0);

                            hover.clone().set(Some(
                                from_unix_ms + (fraction * span_ms) as i64,
                            ));
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
                        if show_threshold {
                            line {
                                class: "chart__threshold",
                                x1: "0",
                                x2: "{VIEW_W}",
                                y1: "{threshold_y:.2}",
                                y2: "{threshold_y:.2}",
                            }
                        }
                        {marks.into_iter()}

                        // Drawn last so it sits above the fills. The hairline snaps to
                        // the sample, not to the raw pointer position — the reader is
                        // aiming at a moment in time, not at a pixel.
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
pub fn LoadCharts(
    /// One entry per database of the selected server, in declaration order.
    series: Vec<LoadSeries>,
    /// Shared window, so every panel's x-axis lines up.
    from_unix_ms: i64,
    to_unix_ms: i64,
    /// Shown instead of the panels when there is nothing at all to draw.
    empty_note: String,
) -> Element {
    let has_any = series.iter().any(|s| !s.points.is_empty());

    if series.is_empty() || !has_any {
        return rsx! {
            div { class: "charts charts--empty",
                p { class: "muted", style: "margin: 0; font-size: 12.5px;", "{empty_note}" }
            }
        };
    }

    let ceiling = shared_ceiling(&series);

    // One hover state for every panel: the panels share a time axis, so pointing at a
    // moment in any of them should read every database at that moment. Hovering each
    // line separately to collect the same four numbers would be the reader doing the
    // chart's job.
    let hover = use_signal(|| None::<i64>);

    let panels: Vec<Element> = series
        .iter()
        .map(|s| {
            rsx! {
                LoadPanel {
                    key: "{s.path}",
                    series: s.clone(),
                    ceiling,
                    from_unix_ms,
                    to_unix_ms,
                    hover,
                }
            }
        })
        .collect();

    // The instant actually being read, snapped to a real sample rather than to the
    // pointer's pixel — so the time in the axis row matches the values in the panels.
    let hovered_at = hover.read().and_then(|at| {
        series
            .iter()
            .filter_map(|s| sample_at(&s.points, at))
            .min_by_key(|point| (point.at_unix_ms - at).abs())
            .map(|point| point.at_unix_ms)
    });

    rsx! {
        div { class: "charts",
            {panels.into_iter()}

            div { class: "chart__x-axis mono",
                if let Some(at) = hovered_at {
                    // Replaces the three fixed labels while hovering: one clear reading
                    // beats three that the eye has to interpolate between.
                    span { class: "chart__x-axis-hover", "{clock(at)}" }
                } else {
                    span { "{clock(from_unix_ms)}" }
                    span { "{clock(from_unix_ms + (to_unix_ms - from_unix_ms) / 2)}" }
                    span { "{clock(to_unix_ms)}" }
                }
            }

            div { class: "chart__footnote",
                span { "All panels share one scale, topping out at " b { class: "mono", "{ceiling:.2}" } }
                if ceiling > MIN_CEILING * 0.75 {
                    span { class: "chart__threshold-key", "1.00 = one backend executing continuously" }
                }
                span { class: "faint", "UTC · a break in a line is a tick that recorded nothing" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(at_unix_ms: i64, value: f64) -> LoadChartPoint {
        LoadChartPoint { at_unix_ms, value }
    }

    fn series(points: Vec<LoadChartPoint>) -> LoadSeries {
        LoadSeries {
            description: "db".to_string(),
            path: "/db".to_string(),
            points,
            note: None,
            duplicate_of: None,
        }
    }

    #[test]
    fn a_continuous_series_is_one_segment() {
        let points: Vec<_> = (0..10).map(|i| point(i * 5_000, 0.5)).collect();

        assert_eq!(segments(&points).len(), 1);
    }

    #[test]
    fn a_recording_gap_breaks_the_line() {
        // Five minutes of nothing: the database was unreachable. Joining across it
        // would draw a low flat line over an outage.
        let points = vec![
            point(0, 0.5),
            point(5_000, 0.6),
            point(305_000, 0.4),
            point(310_000, 0.5),
        ];

        let segments = segments(&points);

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].len(), 2);
        assert_eq!(segments[1].len(), 2);
    }

    #[test]
    fn one_late_tick_does_not_break_the_line() {
        let points = vec![point(0, 0.5), point(10_000, 0.6), point(15_000, 0.7)];

        assert_eq!(segments(&points).len(), 1);
    }

    #[test]
    fn an_idle_database_is_not_scaled_up_into_looking_busy() {
        let idle = vec![series((0..5).map(|i| point(i * 5_000, 0.004)).collect())];

        assert_eq!(shared_ceiling(&idle), MIN_CEILING);
    }

    #[test]
    fn the_ceiling_is_shared_across_panels_so_they_stay_comparable() {
        // A quiet database next to a busy one must not be drawn at the same height.
        let panels = vec![
            series(vec![point(0, 0.01), point(5_000, 0.02)]),
            series(vec![point(0, 3.0), point(5_000, 2.0)]),
        ];

        let ceiling = shared_ceiling(&panels);

        assert!(ceiling > 3.0, "the tallest peak must not touch the top edge");
        assert!((ceiling - 3.45).abs() < 0.001);
    }

    #[test]
    fn an_empty_panel_does_not_drag_the_shared_ceiling_down() {
        let panels = vec![series(Vec::new()), series(vec![point(0, 2.0)])];

        assert!((shared_ceiling(&panels) - 2.3).abs() < 0.001);
    }

    #[test]
    fn the_crosshair_snaps_to_the_nearest_sample() {
        let points = vec![point(0, 0.1), point(5_000, 0.2), point(10_000, 0.3)];

        // Just past the midpoint between the first two.
        assert_eq!(sample_at(&points, 2_600).unwrap().value, 0.2);
        // Exactly on one.
        assert_eq!(sample_at(&points, 10_000).unwrap().value, 0.3);
        // Slightly outside the series still reads its nearest end.
        assert_eq!(sample_at(&points, -1_000).unwrap().value, 0.1);
    }

    #[test]
    fn the_crosshair_reads_nothing_inside_a_recording_gap() {
        // Ten minutes missing. Hovering the middle of it must not report the value
        // from before the outage as though it were current.
        let points = vec![point(0, 0.9), point(600_000, 0.4)];

        assert!(sample_at(&points, 300_000).is_none());
        // ...but close to a real sample it reads again.
        assert_eq!(sample_at(&points, 10_000).unwrap().value, 0.9);
    }

    #[test]
    fn a_series_with_no_points_reads_nothing() {
        assert!(sample_at(&[], 1_000).is_none());
    }

    #[test]
    fn the_clock_renders_utc_time_of_day() {
        assert_eq!(clock(3_723_000), "01:02:03");
        assert_eq!(clock(86_400_000 + 3_723_000), "01:02:03");
    }
}
