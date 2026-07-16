//! Twitch Helix API access and stream-list formatting.

use anyhow::{bail, Context, Result};
use reqwest::{Response, StatusCode};
use serde::Deserialize;

use crate::auth::Auth;

/// One live channel as shown in the TUI list.
#[derive(Debug, Clone, Deserialize)]
pub struct Stream {
    #[serde(default)]
    pub user_login: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub game_name: String,
    #[serde(default)]
    pub viewer_count: u64,
    /// RFC3339 start time (for uptime).
    #[serde(default)]
    pub started_at: String,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: Vec<T>,
}

/// Return the logged-in user's followed channels that are currently live.
/// An expired access token is refreshed and retried once.
pub async fn followed_live(client: &reqwest::Client, auth: &mut Auth) -> Result<Vec<Stream>> {
    let mut response = followed_request(client, auth).await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        auth.refresh(client)
            .await
            .context("refreshing expired Twitch login")?;
        response = followed_request(client, auth).await?;
    }

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("loading followed streams failed (HTTP {status}): {body}");
    }

    let envelope: Envelope<Stream> = response
        .json()
        .await
        .context("parsing followed streams response")?;
    Ok(envelope.data)
}

async fn followed_request(client: &reqwest::Client, auth: &Auth) -> Result<Response> {
    client
        .get("https://api.twitch.tv/helix/streams/followed")
        .query(&[("user_id", auth.user_id.as_str()), ("first", "100")])
        .header("Authorization", format!("Bearer {}", auth.access_token))
        .header("Client-Id", auth.client_id.as_str())
        .send()
        .await
        .context("requesting followed streams")
}

/// Human-readable viewer count, e.g. 12_345 → "12.3k", 1_200_000 → "1.2M".
pub fn humanize_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 999_950 {
        compact_decimal(n as f64 / 1_000.0, "k")
    } else {
        compact_decimal(n as f64 / 1_000_000.0, "M")
    }
}

fn compact_decimal(value: f64, suffix: &str) -> String {
    let value = format!("{value:.1}");
    format!("{}{suffix}", value.strip_suffix(".0").unwrap_or(&value))
}

/// Format a duration given in seconds as `H:MM:SS` (hours unpadded,
/// minutes/seconds zero-padded). Negative durations are clamped to zero.
fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!("{}:{:02}:{:02}", hours, minutes, seconds)
}

/// Uptime string from an RFC3339 start time, e.g. "2:14:07". Returns "?" if unparseable.
pub fn uptime(started_at: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(started_at) {
        Ok(start) => {
            let now = chrono::Utc::now();
            let elapsed = now.signed_duration_since(start);
            fmt_duration(elapsed.num_seconds())
        }
        Err(_) => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_count_below_thousand() {
        assert_eq!(humanize_count(0), "0");
        assert_eq!(humanize_count(1), "1");
        assert_eq!(humanize_count(532), "532");
        assert_eq!(humanize_count(999), "999");
    }

    #[test]
    fn humanize_count_thousands() {
        assert_eq!(humanize_count(1_000), "1k");
        assert_eq!(humanize_count(1_500), "1.5k");
        assert_eq!(humanize_count(12_345), "12.3k");
        // Rounds to one decimal.
        assert_eq!(humanize_count(999_949), "999.9k");
        assert_eq!(humanize_count(999_950), "1M");
    }

    #[test]
    fn humanize_count_millions() {
        assert_eq!(humanize_count(1_000_000), "1M");
        assert_eq!(humanize_count(1_250_000), "1.2M");
        assert_eq!(humanize_count(1_200_000), "1.2M");
    }

    #[test]
    fn fmt_duration_cases() {
        assert_eq!(fmt_duration(8047), "2:14:07");
        assert_eq!(fmt_duration(0), "0:00:00");
        assert_eq!(fmt_duration(59), "0:00:59");
        assert_eq!(fmt_duration(60), "0:01:00");
        assert_eq!(fmt_duration(3600), "1:00:00");
        assert_eq!(fmt_duration(3661), "1:01:01");
        // Negative clamps to zero.
        assert_eq!(fmt_duration(-5), "0:00:00");
    }

    #[test]
    fn uptime_unparseable_returns_question_mark() {
        assert_eq!(uptime("not-a-date"), "?");
        assert_eq!(uptime(""), "?");
    }
}
