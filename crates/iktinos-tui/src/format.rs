//! Numbers and times as short text.

use iktinos::record::Timestamp;
use iktinos::session::{Cost, CostSource};

/// `812`, `34.5k`, `1.2M`, `3.4B`.
pub fn tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=999_999 => format!("{:.1}k", count as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", count as f64 / 1e6),
        _ => format!("{:.1}B", count as f64 / 1e9),
    }
}

/// `$1.23` when Claude Code reported it, `~$1.23` when any of it is an
/// estimate, with a trailing `+` when part of the session could not be
/// priced, and `—` when there is nothing to go on.
pub fn cost(cost: &Cost) -> String {
    let Some(usd) = cost.usd else {
        return "—".to_owned();
    };
    let estimated = matches!(
        cost.source,
        CostSource::Estimated | CostSource::ReportedPlusEstimate
    );
    format!(
        "{}{}{}",
        if estimated { "~" } else { "" },
        usd_amount(usd),
        if cost.incomplete { "+" } else { "" }
    )
}

/// `$0.0042`, `$1.23`, `$415.91`: more digits only where cents say nothing.
pub fn usd_amount(usd: f64) -> String {
    if usd > 0.0 && usd < 0.01 {
        format!("${usd:.4}")
    } else {
        format!("${usd:.2}")
    }
}

/// How long ago `then` was: `now`, `12s`, `5m`, `3h`, `2d`.
pub fn ago(now: Timestamp, then: Timestamp) -> String {
    let seconds = (now.as_millis() - then.as_millis()).max(0) / 1000;
    match seconds {
        0..=1 => "now".to_owned(),
        2..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// `850ms`, `1.5s`, `2m05s`.
pub fn duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms}ms"),
        1_000..=59_999 => format!("{:.1}s", ms as f64 / 1e3),
        _ => format!("{}m{:02}s", ms / 60_000, ms % 60_000 / 1000),
    }
}

/// A tool's name as shown: an MCP tool `mcp__server__tool` as `server/tool`.
pub fn tool_name(name: &str) -> String {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.replacen("__", "/", 1),
        None => name.to_owned(),
    }
}

/// A model id without the prefix every one of them has: `fable-5-2`.
pub fn model_name(id: &str) -> String {
    id.strip_prefix("claude-").unwrap_or(id).to_owned()
}

/// `1 turn`, `5 turns`.
pub fn count(n: u64, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// `42%`.
pub fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

/// The text cut to `width` characters, with an ellipsis when cut.
pub fn truncate(text: &str, width: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() <= width {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_scale() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(812), "812");
        assert_eq!(tokens(34_500), "34.5k");
        assert_eq!(tokens(1_234_567), "1.2M");
        assert_eq!(tokens(164_201_579), "164.2M");
        assert_eq!(tokens(3_400_000_000), "3.4B");
    }

    #[test]
    fn cost_says_where_it_came_from() {
        let reported = Cost {
            usd: Some(1.234),
            source: CostSource::Reported,
            incomplete: false,
        };
        assert_eq!(cost(&reported), "$1.23");
        let estimated = Cost {
            source: CostSource::Estimated,
            ..reported
        };
        assert_eq!(cost(&estimated), "~$1.23");
        let partial = Cost {
            source: CostSource::ReportedPlusEstimate,
            incomplete: true,
            ..reported
        };
        assert_eq!(cost(&partial), "~$1.23+");
        assert_eq!(cost(&Cost::default()), "—");
        assert_eq!(usd_amount(0.008896), "$0.0089");
        assert_eq!(usd_amount(0.0), "$0.00");
    }

    #[test]
    fn ago_picks_a_unit() {
        let now = Timestamp::from_millis(10_000_000_000);
        let back = |seconds: i64| Timestamp::from_millis(now.as_millis() - seconds * 1000);
        assert_eq!(ago(now, back(0)), "now");
        assert_eq!(ago(now, back(12)), "12s");
        assert_eq!(ago(now, back(300)), "5m");
        assert_eq!(ago(now, back(3 * 3600 + 5)), "3h");
        assert_eq!(ago(now, back(2 * 86_400)), "2d");
        // A record from the future is "now", not a negative age.
        assert_eq!(ago(now, back(-50)), "now");
    }

    #[test]
    fn durations_and_truncation() {
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(1_500), "1.5s");
        assert_eq!(duration(125_000), "2m05s");
        assert_eq!(percent(0.4249), "42%");
        assert_eq!(count(1, "turn"), "1 turn");
        assert_eq!(model_name("claude-haiku-5-5"), "haiku-5-5");
        assert_eq!(model_name("gpt-x"), "gpt-x");
        assert_eq!(tool_name("Bash"), "Bash");
        assert_eq!(tool_name("mcp__example__lookup"), "example/lookup");
        assert_eq!(
            tool_name("mcp__plugin_a_b__get_item"),
            "plugin_a_b/get_item"
        );
        assert_eq!(count(0, "tool call"), "0 tool calls");
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a longer line", 8), "a longe…");
        assert_eq!(truncate("first\nsecond", 20), "first");
        assert_eq!(truncate("日本語のタイトルです", 5), "日本語の…");
    }
}
