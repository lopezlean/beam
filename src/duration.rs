use std::time::{Duration, SystemTime};

pub fn parse_duration_value(input: &str) -> Result<Duration, String> {
    humantime::parse_duration(input).map_err(|error| error.to_string())
}

pub fn format_duration(duration: Duration) -> String {
    if duration.is_zero() {
        return "0s".to_string();
    }

    let mut remaining = duration.as_secs();
    let days = remaining / 86_400;
    remaining %= 86_400;
    let hours = remaining / 3_600;
    remaining %= 3_600;
    let minutes = remaining / 60;
    let seconds = remaining % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }

    parts.join(" ")
}

pub fn remaining_until(deadline: SystemTime) -> Duration {
    deadline
        .duration_since(SystemTime::now())
        .unwrap_or_else(|_| Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::{format_duration, parse_duration_value};
    use std::time::Duration;

    #[test]
    fn parses_human_duration() {
        assert_eq!(
            parse_duration_value("15m").unwrap(),
            Duration::from_secs(900)
        );
        assert_eq!(
            parse_duration_value("2h 30m").unwrap(),
            Duration::from_secs(9_000)
        );
    }

    #[test]
    fn formats_human_duration() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m 5s");
        assert_eq!(format_duration(Duration::from_secs(3_661)), "1h 1m 1s");
    }
}
