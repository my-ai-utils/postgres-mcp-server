//! Primitives shared by every chart on the statistics page.
//!
//! Extracted rather than copied because the gap rule below is a correctness
//! property, not a drawing detail: three private copies of it would drift, and the
//! copy that drifts is the one that starts drawing straight lines across outages.

/// Break a line when samples are further apart than this.
///
/// The collector writes every 5 seconds and the minute rows every 60, so this is
/// scaled per series by [`gap_ms_for`] rather than fixed — but the rule is the same
/// everywhere: several missed samples, not one that ran late.
pub const FAST_GAP_MS: i64 = 30_000;

/// The same for a series sampled once a minute.
pub const MINUTE_GAP_MS: i64 = 180_000;

/// One point of any series.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartPoint {
    pub at_unix_ms: i64,
    pub value: f64,
}

/// Splits a series wherever a gap in recording makes a connecting line a lie.
///
/// A joined line across missing data is an invented measurement, and it reads as a
/// *low, steady* value across exactly the window where nothing was recorded — which
/// is usually the window something went wrong in.
pub fn segments(points: &[ChartPoint], gap_ms: i64) -> Vec<Vec<&ChartPoint>> {
    let mut result: Vec<Vec<&ChartPoint>> = Vec::new();
    let mut current: Vec<&ChartPoint> = Vec::new();

    for point in points {
        if let Some(previous) = current.last() {
            if point.at_unix_ms - previous.at_unix_ms > gap_ms {
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

/// The sample nearest `at_unix_ms`, or `None` when the pointer is inside a gap.
///
/// The same window as the line break, for the same reason: inside a gap there is no
/// sample to report, and snapping to one minutes away would put a number from before
/// an outage under a crosshair hovering the middle of it.
pub fn sample_at(points: &[ChartPoint], at_unix_ms: i64, gap_ms: i64) -> Option<&ChartPoint> {
    points
        .iter()
        .min_by_key(|point| (point.at_unix_ms - at_unix_ms).abs())
        .filter(|point| (point.at_unix_ms - at_unix_ms).abs() <= gap_ms)
}

/// The largest value in a series, with headroom so a peak is not drawn against the
/// top edge. `None` for an empty series — the caller decides what an empty panel's
/// scale should be, since that depends on what is being measured.
pub fn peak_with_headroom(points: &[ChartPoint]) -> Option<f64> {
    let peak = points
        .iter()
        .map(|point| point.value)
        .fold(f64::MIN, f64::max);

    if peak == f64::MIN {
        return None;
    }

    Some(peak * 1.15)
}

/// `hh:mm:ss` (UTC) from epoch milliseconds.
///
/// Hand-rolled to keep a date library out of the wasm bundle for one label row, and
/// UTC on purpose: every other timestamp on this page comes from the server as UTC,
/// and one row quietly in local time would not line up with them.
pub fn clock(at_unix_ms: i64) -> String {
    let seconds = at_unix_ms.div_euclid(1_000).rem_euclid(86_400);

    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(at_unix_ms: i64, value: f64) -> ChartPoint {
        ChartPoint { at_unix_ms, value }
    }

    #[test]
    fn a_continuous_series_is_one_segment() {
        let points: Vec<_> = (0..10).map(|i| point(i * 5_000, 0.5)).collect();

        assert_eq!(segments(&points, FAST_GAP_MS).len(), 1);
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

        let split = segments(&points, FAST_GAP_MS);

        assert_eq!(split.len(), 2);
        assert_eq!(split[0].len(), 2);
        assert_eq!(split[1].len(), 2);
    }

    #[test]
    fn one_late_tick_does_not_break_the_line() {
        let points = vec![point(0, 0.5), point(10_000, 0.6), point(15_000, 0.7)];

        assert_eq!(segments(&points, FAST_GAP_MS).len(), 1);
    }

    #[test]
    fn a_minute_series_tolerates_a_minute_sized_gap() {
        // Two minutes apart is one missed row, not an outage; five is an outage.
        let close = vec![point(0, 1.0), point(120_000, 2.0)];
        let far = vec![point(0, 1.0), point(300_000, 2.0)];

        assert_eq!(segments(&close, MINUTE_GAP_MS).len(), 1);
        assert_eq!(segments(&far, MINUTE_GAP_MS).len(), 2);
    }

    #[test]
    fn the_crosshair_snaps_to_the_nearest_sample() {
        let points = vec![point(0, 0.1), point(5_000, 0.2), point(10_000, 0.3)];

        assert_eq!(sample_at(&points, 2_600, FAST_GAP_MS).unwrap().value, 0.2);
        assert_eq!(sample_at(&points, 10_000, FAST_GAP_MS).unwrap().value, 0.3);
        // Slightly outside the series still reads its nearest end.
        assert_eq!(sample_at(&points, -1_000, FAST_GAP_MS).unwrap().value, 0.1);
    }

    #[test]
    fn the_crosshair_reads_nothing_inside_a_recording_gap() {
        let points = vec![point(0, 0.9), point(600_000, 0.4)];

        assert!(sample_at(&points, 300_000, FAST_GAP_MS).is_none());
        assert_eq!(sample_at(&points, 10_000, FAST_GAP_MS).unwrap().value, 0.9);
    }

    #[test]
    fn an_empty_series_reads_nothing_and_has_no_peak() {
        assert!(sample_at(&[], 1_000, FAST_GAP_MS).is_none());
        assert!(peak_with_headroom(&[]).is_none());
    }

    #[test]
    fn the_peak_leaves_room_above_it() {
        let points = vec![point(0, 3.0), point(5_000, 1.0)];

        let peak = peak_with_headroom(&points).unwrap();

        assert!(peak > 3.0, "the peak must not touch the top edge");
        assert!((peak - 3.45).abs() < 0.001);
    }

    #[test]
    fn the_clock_renders_utc_time_of_day() {
        assert_eq!(clock(3_723_000), "01:02:03");
        assert_eq!(clock(86_400_000 + 3_723_000), "01:02:03");
    }
}
