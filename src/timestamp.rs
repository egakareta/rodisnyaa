use web_time::Duration;

/// Formats a [`Duration`] as a string in the format `H:MM:SS` or `M:SS`.
pub fn format_timestamp(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Formats f64 seconds as a string in the format `H:MM:SS` or `M:SS`.
pub fn format_timestamp_secs(seconds: f64) -> String {
    format_timestamp(Duration::from_secs_f64(seconds))
}

/// Parses a timestamp string in the format `H:MM:SS`, `M:SS`, or `SS` into a number of seconds.
pub fn parse_timestamp(input: &str) -> Option<f64> {
    let components = input.trim().split(':').collect::<Vec<_>>();
    let parse_seconds = |value: &str| {
        value
            .parse::<f64>()
            .ok()
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
    };

    match components.as_slice() {
        [seconds] => parse_seconds(seconds),
        [minutes, seconds] => {
            let minutes = minutes.parse::<u64>().ok()?;
            let seconds = parse_seconds(seconds)?;

            (seconds < 60.0).then_some(minutes as f64 * 60.0 + seconds)
        }
        [hours, minutes, seconds] => {
            let hours = hours.parse::<u64>().ok()?;
            let minutes = minutes.parse::<u64>().ok()?;
            let seconds = parse_seconds(seconds)?;

            (minutes < 60 && seconds < 60.0)
                .then_some(hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds)
        }
        _ => None,
    }
}
