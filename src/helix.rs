//! Twitch Helix API access and stream-list formatting.

use anyhow::{bail, Context, Result};
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::auth::Auth;

/// One live channel as shown in the TUI list.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Stream {
    #[serde(default)]
    pub user_login: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub game_name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub thumbnail_url: String,
    #[serde(default)]
    pub viewer_count: u64,
    /// RFC3339 start time (for uptime).
    #[serde(default)]
    pub started_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub box_art_url: String,
}

#[derive(Debug, Deserialize)]
struct SearchChannel {
    broadcaster_login: String,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: Vec<T>,
}

/// Return the logged-in user's followed channels that are currently live.
/// An expired access token is refreshed and retried once.
pub async fn followed_live(client: &reqwest::Client, auth: &mut Auth) -> Result<Vec<Stream>> {
    let user_id = auth.user_id.clone();
    helix_data(
        client,
        auth,
        "streams/followed",
        &[("user_id", user_id.as_str()), ("first", "100")],
        "followed streams",
    )
    .await
}

pub async fn popular_live(client: &reqwest::Client, auth: &mut Auth) -> Result<Vec<Stream>> {
    helix_data(
        client,
        auth,
        "streams",
        &[("first", "30")],
        "popular streams",
    )
    .await
}

pub async fn top_categories(client: &reqwest::Client, auth: &mut Auth) -> Result<Vec<Category>> {
    helix_data(
        client,
        auth,
        "games/top",
        &[("first", "30")],
        "top categories",
    )
    .await
}

pub async fn category_live(
    client: &reqwest::Client,
    auth: &mut Auth,
    game_id: &str,
) -> Result<Vec<Stream>> {
    let game_id = game_id.trim();
    if game_id.is_empty() || !game_id.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("invalid Twitch category id");
    }
    helix_data(
        client,
        auth,
        "streams",
        &[("game_id", game_id), ("first", "30")],
        "category streams",
    )
    .await
}

pub async fn search_live(
    client: &reqwest::Client,
    auth: &mut Auth,
    query: &str,
) -> Result<Vec<Stream>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    if query.chars().count() > 100 {
        bail!("channel search is limited to 100 characters");
    }

    let channels: Vec<SearchChannel> = helix_data(
        client,
        auth,
        "search/channels",
        &[("query", query), ("first", "30"), ("live_only", "true")],
        "channel search",
    )
    .await?;
    if channels.is_empty() {
        return Ok(Vec::new());
    }

    let logins = channels
        .into_iter()
        .map(|channel| channel.broadcaster_login)
        .collect::<Vec<_>>();
    let mut params = Vec::with_capacity(logins.len() + 1);
    params.push(("first", "100"));
    params.extend(logins.iter().map(|login| ("user_login", login.as_str())));
    helix_data(client, auth, "streams", &params, "live search results").await
}

async fn helix_data<T: DeserializeOwned>(
    client: &reqwest::Client,
    auth: &mut Auth,
    endpoint: &str,
    query: &[(&str, &str)],
    operation: &str,
) -> Result<Vec<T>> {
    let mut response = helix_request(client, auth, endpoint, query, operation).await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        auth.refresh(client)
            .await
            .context("refreshing expired Twitch login")?;
        response = helix_request(client, auth, endpoint, query, operation).await?;
    }

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("loading {operation} failed (HTTP {status}): {body}");
    }

    let envelope: Envelope<T> = response
        .json()
        .await
        .with_context(|| format!("parsing {operation} response"))?;
    Ok(envelope.data)
}

async fn helix_request(
    client: &reqwest::Client,
    auth: &Auth,
    endpoint: &str,
    query: &[(&str, &str)],
    operation: &str,
) -> Result<Response> {
    client
        .get(format!("https://api.twitch.tv/helix/{endpoint}"))
        .query(query)
        .header("Authorization", format!("Bearer {}", auth.access_token))
        .header("Client-Id", auth.client_id.as_str())
        .send()
        .await
        .with_context(|| format!("requesting {operation}"))
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

    #[test]
    fn category_response_parses() {
        let envelope: Envelope<Category> = serde_json::from_str(
            r#"{"data":[{"id":"509658","name":"Just Chatting","box_art_url":"https://example/{width}x{height}.jpg"}]}"#,
        )
        .unwrap();
        assert_eq!(envelope.data[0].name, "Just Chatting");
    }

    #[test]
    fn search_channel_response_parses() {
        let envelope: Envelope<SearchChannel> =
            serde_json::from_str(r#"{"data":[{"broadcaster_login":"twitchdev","is_live":true}]}"#)
                .unwrap();
        assert_eq!(envelope.data[0].broadcaster_login, "twitchdev");
    }

    #[tokio::test]
    #[ignore = "requires cached Twitch credentials and network access"]
    async fn browse_endpoints_smoke() {
        let client = reqwest::Client::new();
        let mut auth = Auth::load().unwrap().expect("cached Twitch login required");
        assert!(!popular_live(&client, &mut auth).await.unwrap().is_empty());
        assert!(!top_categories(&client, &mut auth).await.unwrap().is_empty());
        let _ = search_live(&client, &mut auth, "twitch").await.unwrap();
    }
}
