//! Durations written the way `timeout(1)` accepts them: `90`, `90s`, `500ms`, `5m`, `1h`.

use std::time::Duration;

/// Parses a positive duration, a bare number meaning seconds.
pub(crate) fn parse(text: &str) -> Result<Duration, String> {
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("invalid duration {text:?}, expected a number followed by ms, s, m or h"))?;
    let duration = match unit {
        "ms" => Duration::from_millis(number),
        "" | "s" => Duration::from_secs(number),
        "m" => Duration::from_secs(number.saturating_mul(60)),
        "h" => Duration::from_secs(number.saturating_mul(3600)),
        _ => {
            return Err(format!(
                "invalid duration unit {unit:?} in {text:?}, expected ms, s, m or h"
            ));
        }
    };
    if duration.is_zero() {
        return Err(format!("invalid duration {text:?}, it must be greater than zero"));
    }
    Ok(duration)
}

/// Formats a duration in the largest unit that renders it exactly.
pub(crate) fn format(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis.is_multiple_of(3_600_000) {
        format!("{}h", millis / 3_600_000)
    } else if millis.is_multiple_of(60_000) {
        format!("{}m", millis / 60_000)
    } else if millis.is_multiple_of(1000) {
        format!("{}s", millis / 1000)
    } else {
        format!("{millis}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_unit() {
        assert_eq!(parse("90"), Ok(Duration::from_secs(90)));
        assert_eq!(parse("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse("5m"), Ok(Duration::from_mins(5)));
        assert_eq!(parse("1h"), Ok(Duration::from_hours(1)));
    }

    #[test]
    fn rejects_zero_unknown_units_and_missing_numbers() {
        assert!(parse("0s").is_err());
        assert!(parse("5d").is_err());
        assert!(parse("s").is_err());
        assert!(parse("").is_err());
        assert!(parse("-1s").is_err());
    }

    #[test]
    fn formats_in_the_largest_exact_unit() {
        assert_eq!(format(Duration::from_millis(1500)), "1500ms");
        assert_eq!(format(Duration::from_secs(2)), "2s");
        assert_eq!(format(Duration::from_mins(30)), "30m");
        assert_eq!(format(Duration::from_hours(2)), "2h");
    }
}
