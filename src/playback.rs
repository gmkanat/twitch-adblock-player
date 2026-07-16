//! Ad-block playback engine.
//!
//! Resolves an ad-free HLS stream for a channel and serves it on a localhost URL
//! that any player (mpv) can open. Strips server-stitched (SSAI) ads from the
//! media playlist and, when a whole playlist is an ad (pre-roll), swaps to a
//! clean stream via a different playerType — staying sticky to avoid the
//! media-sequence resets that stall the player.
//!
//! Extracted into a `PlaybackSession` so the TUI can start/stop playback per
//! selected channel.

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use axum::{
    extract::State,
    http::{header::CONTENT_TYPE, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tower_http::cors::CorsLayer;

use crate::playlist::{parse_master, select_variant, strip_ads};

/// Public web Client-ID Twitch's own player uses for anonymous playback tokens.
/// (Deliberately separate from the user's logged-in app id used by helix/chat.)
const CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const GQL_URL: &str = "https://gql.twitch.tv/gql";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/124.0.0.0 Safari/537.36";

const ACCESS_TOKEN_QUERY: &str = r#"query PlaybackAccessToken($login: String!, $playerType: String!, $platform: String!) {
  streamPlaybackAccessToken(channelName: $login, params: {platform: $platform, playerBackend: "mediaplayer", playerType: $playerType}) {
    value
    signature
  }
}"#;

#[derive(Clone)]
pub struct PlaybackRelay {
    endpoint: reqwest::Url,
    twitch_token: String,
    relay_secret: Option<String>,
}

impl PlaybackRelay {
    pub fn new(endpoint: &str, twitch_token: &str, relay_secret: Option<&str>) -> Result<Self> {
        let endpoint = reqwest::Url::parse(endpoint.trim()).context("invalid 2K relay URL")?;
        if endpoint.scheme() != "https" {
            bail!("2K relay URL must use HTTPS");
        }
        if endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            bail!("invalid 2K relay URL");
        }

        let twitch_token = normalize_twitch_token(twitch_token)?;
        let relay_secret = relay_secret
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if relay_secret.as_ref().is_some_and(|value| value.len() > 512) {
            bail!("relay secret is too long");
        }

        Ok(Self {
            endpoint,
            twitch_token,
            relay_secret,
        })
    }
}

#[derive(Clone)]
pub struct PlaybackOptions {
    relay: Option<PlaybackRelay>,
    supported_codecs: String,
}

impl PlaybackOptions {
    pub fn enhanced(relay: PlaybackRelay, supported_codecs: &[String]) -> Self {
        Self {
            relay: Some(relay),
            supported_codecs: normalize_codecs(supported_codecs),
        }
    }
}

impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            relay: None,
            supported_codecs: "h264".to_string(),
        }
    }
}

/// Build an HTTP client for the playback (anonymous) requests, with optional proxy.
pub fn build_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10));
    if let Some(p) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(p).context("invalid proxy URL")?);
    }
    Ok(builder.build()?)
}

/// Channel for surfacing ad-block engine notes (e.g. to the TUI status bar).
pub type StatusTx = mpsc::UnboundedSender<String>;

/// Send a status note to the TUI if a channel is set; otherwise print to stderr
/// (used by the CLI `watch` mode, which has no TUI to corrupt).
fn note(status: &Option<StatusTx>, msg: String) {
    match status {
        Some(tx) => {
            let _ = tx.send(msg);
        }
        None => eprintln!("[adblock] {msg}"),
    }
}

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    channel: String,
    quality: String,
    /// Sticky: the variant URL currently served; only changes when it goes ad-gated.
    current: Arc<Mutex<String>>,
    /// Where to surface ad-block notes (Some → TUI status bar, None → stderr).
    status: Option<StatusTx>,
    options: PlaybackOptions,
}

/// A running localhost server that exposes one filtered HLS media playlist.
pub struct StreamProxy {
    local_url: String,
    qualities: Vec<String>,
    server: JoinHandle<()>,
}

impl StreamProxy {
    /// Resolve a channel and start its local HLS filter.
    pub async fn start(
        client: &reqwest::Client,
        channel: &str,
        quality: &str,
        status: Option<StatusTx>,
    ) -> Result<Self> {
        Self::start_with_options(
            client,
            channel,
            quality,
            status,
            &PlaybackOptions::default(),
        )
        .await
    }

    pub async fn start_with_options(
        client: &reqwest::Client,
        channel: &str,
        quality: &str,
        status: Option<StatusTx>,
        options: &PlaybackOptions,
    ) -> Result<Self> {
        let channel = normalize_channel(channel)?;
        let player_type = if options.relay.is_some() {
            "site"
        } else {
            "embed"
        };

        let master =
            load_master_with_fallback(client, &channel, player_type, "web", options, &status)
                .await?;
        let variants = parse_master(&master);
        if variants.is_empty() {
            bail!("no stream variants returned — is '{channel}' live?");
        }
        let mut qualities = variants
            .iter()
            .map(|variant| (variant.bandwidth, variant.name.clone()))
            .collect::<Vec<_>>();
        qualities.sort_by_key(|(bandwidth, _)| std::cmp::Reverse(*bandwidth));
        let mut seen = std::collections::HashSet::new();
        qualities.retain(|(_, name)| seen.insert(name.to_ascii_lowercase()));
        let qualities = qualities
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>();
        let chosen =
            select_variant(&variants, quality).or_else(|_| select_variant(&variants, "best"))?;

        let state = AppState {
            client: client.clone(),
            channel: channel.clone(),
            quality: quality.to_string(),
            current: Arc::new(Mutex::new(chosen.url.clone())),
            status,
            options: options.clone(),
        };

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let local_url = format!("http://{addr}/live.m3u8");
        let app = Router::new()
            .route("/live.m3u8", get(serve_playlist))
            .layer(CorsLayer::permissive())
            .with_state(state);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Ok(Self {
            local_url,
            qualities,
            server,
        })
    }

    pub fn local_url(&self) -> &str {
        &self.local_url
    }

    pub fn qualities(&self) -> &[String] {
        &self.qualities
    }

    pub fn stop(self) {
        self.server.abort();
    }
}

impl Drop for StreamProxy {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A filtered HLS proxy paired with an external player process.
pub struct PlaybackSession {
    proxy: StreamProxy,
    player: tokio::process::Child,
}

impl PlaybackSession {
    pub async fn start(
        client: &reqwest::Client,
        channel: &str,
        quality: &str,
        player: &str,
        status: Option<StatusTx>,
    ) -> Result<Self> {
        let channel = normalize_channel(channel)?;
        let proxy = StreamProxy::start(client, &channel, quality, status).await?;

        let player = match spawn_player(player, proxy.local_url(), &channel) {
            Ok(child) => child,
            Err(error) => {
                proxy.stop();
                return Err(error).with_context(|| format!("launching player '{player}'"));
            }
        };

        Ok(Self { proxy, player })
    }

    /// Block until the player exits.
    pub async fn wait(&mut self) -> Result<()> {
        let status = self.player.wait().await.context("waiting for player")?;
        self.proxy.server.abort();
        if !status.success() {
            bail!("player exited with {status}");
        }
        Ok(())
    }

    /// Stop the player and the local server.
    pub async fn stop(mut self) {
        let _ = self.player.kill().await;
        self.proxy.server.abort();
    }
}

fn spawn_player(player: &str, url: &str, channel: &str) -> Result<tokio::process::Child> {
    use tokio::process::Command;
    let mut cmd = Command::new(player);
    cmd.kill_on_drop(true);
    if player.contains("mpv") {
        cmd.arg(format!("--force-media-title=twitch-adblock: {channel}"));
        cmd.arg("--no-ytdl");
        // Never read/write the controlling terminal — otherwise mpv's status
        // line (AV:/VO:/AO:) corrupts the TUI it's launched from.
        cmd.arg("--no-terminal");
    }
    cmd.arg(url);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    Ok(cmd.spawn()?)
}

/// HTTP handler: sticky-source ad filtering (see module docs).
async fn serve_playlist(State(st): State<AppState>) -> Response {
    let cur = st.current.lock().await.clone();

    if let Ok(body) = fetch_text(&st.client, &cur).await {
        let r = strip_ads(&body);
        if r.kept_segments > 0 {
            if r.removed_segments > 0 {
                note(
                    &st.status,
                    format!("stripped {} ad segment(s)", r.removed_segments),
                );
            }
            return playlist_response(r.playlist);
        }
    }

    note(&st.status, "ad break — switching source…".to_string());
    let enhanced_candidates = [
        ("site", "web"),
        ("embed", "web"),
        ("popout", "web"),
        ("autoplay", "android"),
    ];
    let standard_candidates = [("embed", "web"), ("popout", "web"), ("autoplay", "android")];
    let candidates = if st.options.relay.is_some() {
        enhanced_candidates.as_slice()
    } else {
        standard_candidates.as_slice()
    };
    for &(player_type, platform) in candidates {
        if let Ok((url, name)) = resolve_variant(
            &st.client,
            &st.channel,
            player_type,
            platform,
            &st.quality,
            &st.options,
            &st.status,
        )
        .await
        {
            if let Ok(body) = fetch_text(&st.client, &url).await {
                let r = strip_ads(&body);
                if r.kept_segments > 0 {
                    note(&st.status, format!("ad-free via {player_type} ({name})"));
                    *st.current.lock().await = url;
                    return playlist_response(r.playlist);
                }
            }
        }
    }

    note(
        &st.status,
        "no ad-free source — showing original".to_string(),
    );
    match fetch_text(&st.client, &cur).await {
        Ok(body) => playlist_response(body),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    }
}

fn playlist_response(body: String) -> Response {
    ([(CONTENT_TYPE, "application/vnd.apple.mpegurl")], body).into_response()
}

async fn resolve_variant(
    client: &reqwest::Client,
    channel: &str,
    player_type: &str,
    platform: &str,
    quality: &str,
    options: &PlaybackOptions,
    status: &Option<StatusTx>,
) -> Result<(String, String)> {
    let master =
        load_master_with_fallback(client, channel, player_type, platform, options, status).await?;
    let variants = parse_master(&master);
    if variants.is_empty() {
        bail!("no variants");
    }
    let chosen =
        select_variant(&variants, quality).or_else(|_| select_variant(&variants, "best"))?;
    Ok((chosen.url.clone(), chosen.name.clone()))
}

async fn get_access_token(
    client: &reqwest::Client,
    channel: &str,
    player_type: &str,
    platform: &str,
    relay: Option<&PlaybackRelay>,
) -> Result<(String, String)> {
    let body = json!({
        "operationName": "PlaybackAccessToken",
        "query": ACCESS_TOKEN_QUERY,
        "variables": {
            "login": channel,
            "playerType": player_type,
            "platform": platform,
        }
    });

    let device_id = rand_hex(32);
    let response: GraphQlResponse = if let Some(relay) = relay {
        let payload = json!({
            "type": "gql",
            "body": body.to_string(),
            "clientId": CLIENT_ID,
            "auth": format!("OAuth {}", relay.twitch_token),
            "deviceId": device_id,
        });
        serde_json::from_str(&relay_post(client, relay, &payload).await?)
            .context("parsing relayed playback token response")?
    } else {
        client
            .post(GQL_URL)
            .header("Client-ID", CLIENT_ID)
            .header("Device-ID", device_id)
            .json(&body)
            .send()
            .await
            .context("requesting playback token")?
            .error_for_status()
            .context("playback token request failed")?
            .json()
            .await
            .context("parsing playback token response")?
    };
    let errors = response
        .errors
        .into_iter()
        .map(|error| error.message)
        .collect::<Vec<_>>()
        .join("; ");
    match response.data.and_then(|data| data.token) {
        Some(token) => Ok((token.value, token.signature)),
        None if !errors.is_empty() => bail!("Twitch GraphQL error: {errors}"),
        None => bail!(
            "no playback token for '{channel}' (offline, misspelled, or integrity check required)"
        ),
    }
}

#[derive(Deserialize)]
struct GraphQlResponse {
    data: Option<GraphQlData>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlData {
    #[serde(rename = "streamPlaybackAccessToken")]
    token: Option<PlaybackToken>,
}

#[derive(Deserialize)]
struct PlaybackToken {
    value: String,
    signature: String,
}

#[derive(Deserialize)]
struct GraphQlError {
    message: String,
}

async fn fetch_master(
    client: &reqwest::Client,
    channel: &str,
    value: &str,
    signature: &str,
    platform: &str,
    supported_codecs: &str,
    relay: Option<&PlaybackRelay>,
) -> Result<String> {
    let url = format!("https://usher.ttvnw.net/api/v2/channel/hls/{channel}.m3u8");
    let p = (rand_u64() % 9_000_000 + 1_000_000).to_string();

    let request = client
        .get(&url)
        .query(&[
            ("allow_source", "true"),
            ("allow_audio_only", "true"),
            ("platform", platform),
            ("player_backend", "mediaplayer"),
            ("playlist_include_framerate", "true"),
            ("supported_codecs", supported_codecs),
            ("fast_bread", "true"),
            ("p", p.as_str()),
            ("token", value),
            ("sig", signature),
        ])
        .build()
        .context("building usher request")?;

    if let Some(relay) = relay {
        let payload = json!({
            "type": "usher",
            "url": request.url().as_str(),
        });
        return relay_post(client, relay, &payload)
            .await
            .context("relayed usher request failed");
    }

    let resp = client
        .execute(request)
        .await
        .context("usher request failed")?;

    let status = resp.status();
    let text = resp.text().await.context("could not read usher response")?;
    if !status.is_success() {
        bail!(
            "usher returned {status}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    Ok(text)
}

async fn load_master_with_fallback(
    client: &reqwest::Client,
    channel: &str,
    player_type: &str,
    platform: &str,
    options: &PlaybackOptions,
    status: &Option<StatusTx>,
) -> Result<String> {
    if let Some(relay) = options.relay.as_ref() {
        match load_master(
            client,
            channel,
            player_type,
            platform,
            &options.supported_codecs,
            Some(relay),
        )
        .await
        {
            Ok(master) => return Ok(master),
            Err(error) => note(
                status,
                format!("2K relay unavailable ({error}); using standard quality"),
            ),
        }
    }

    load_master(client, channel, player_type, platform, "h264", None).await
}

async fn load_master(
    client: &reqwest::Client,
    channel: &str,
    player_type: &str,
    platform: &str,
    supported_codecs: &str,
    relay: Option<&PlaybackRelay>,
) -> Result<String> {
    let (value, signature) =
        get_access_token(client, channel, player_type, platform, relay).await?;
    fetch_master(
        client,
        channel,
        &value,
        &signature,
        platform,
        supported_codecs,
        relay,
    )
    .await
}

async fn relay_post(
    client: &reqwest::Client,
    relay: &PlaybackRelay,
    payload: &serde_json::Value,
) -> Result<String> {
    let mut request = client.post(relay.endpoint.clone()).json(payload);
    if let Some(secret) = relay.relay_secret.as_ref() {
        request = request.bearer_auth(secret);
    }
    let response = request.send().await.context("contacting 2K relay")?;
    let status = response.status();
    let body = response.text().await.context("reading 2K relay response")?;
    if !status.is_success() {
        bail!(
            "2K relay returned {status}: {}",
            body.chars().take(200).collect::<String>()
        );
    }
    Ok(body)
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client.get(url).send().await?.error_for_status()?;
    Ok(resp.text().await?)
}

fn rand_u64() -> u64 {
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

fn rand_hex(n: usize) -> String {
    let hex = b"0123456789abcdef";
    let mut x = rand_u64();
    let mut s = String::with_capacity(n);
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.push(hex[(x & 0xf) as usize] as char);
    }
    s
}

fn normalize_channel(channel: &str) -> Result<String> {
    let channel = channel.trim().trim_start_matches('#').to_ascii_lowercase();
    if channel.is_empty()
        || !channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("invalid Twitch channel '{channel}'");
    }
    Ok(channel)
}

fn normalize_twitch_token(token: &str) -> Result<String> {
    let token = token
        .trim()
        .strip_prefix("OAuth ")
        .or_else(|| token.trim().strip_prefix("oauth:"))
        .unwrap_or(token.trim());
    if token.len() < 20
        || token.len() > 512
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("invalid Twitch website auth token");
    }
    Ok(token.to_string())
}

fn normalize_codecs(codecs: &[String]) -> String {
    let mut normalized = vec!["h264"];
    for codec in codecs {
        let codec = codec.trim().to_ascii_lowercase();
        if matches!(codec.as_str(), "h265" | "av1") && !normalized.contains(&codec.as_str()) {
            normalized.push(if codec == "h265" { "h265" } else { "av1" });
        }
    }
    normalized.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playback_token() {
        let json = r#"{
            "data": {
                "streamPlaybackAccessToken": {
                    "value": "token-value",
                    "signature": "token-signature"
                }
            }
        }"#;
        let response: GraphQlResponse = serde_json::from_str(json).unwrap();
        let token = response.data.unwrap().token.unwrap();
        assert_eq!(token.value, "token-value");
        assert_eq!(token.signature, "token-signature");
    }

    #[test]
    fn parses_graphql_error() {
        let json = r#"{
            "data": null,
            "errors": [{"message": "channel is offline", "path": ["stream"]}]
        }"#;
        let response: GraphQlResponse = serde_json::from_str(json).unwrap();
        assert!(response.data.is_none());
        assert_eq!(response.errors[0].message, "channel is offline");
    }

    #[test]
    fn validates_channel_names() {
        assert_eq!(
            normalize_channel(" #Some_Channel ").unwrap(),
            "some_channel"
        );
        assert!(normalize_channel("").is_err());
        assert!(normalize_channel("bad/channel").is_err());
    }

    #[test]
    fn validates_enhanced_playback_configuration() {
        let relay = PlaybackRelay::new(
            "https://relay.example.workers.dev/",
            "oauth:abcdefghijklmnopqrstuvwxyz1234",
            Some("relay-secret"),
        )
        .unwrap();
        assert_eq!(
            relay.endpoint.as_str(),
            "https://relay.example.workers.dev/"
        );
        assert_eq!(relay.twitch_token, "abcdefghijklmnopqrstuvwxyz1234");
        assert_eq!(relay.relay_secret.as_deref(), Some("relay-secret"));

        assert!(PlaybackRelay::new(
            "http://relay.example",
            "abcdefghijklmnopqrstuvwxyz1234",
            None,
        )
        .is_err());
        assert!(PlaybackRelay::new("https://relay.example", "too-short", None).is_err());
    }

    #[test]
    fn normalizes_supported_codecs() {
        assert_eq!(normalize_codecs(&[]), "h264");
        assert_eq!(
            normalize_codecs(&[
                "H265".to_string(),
                "av1".to_string(),
                "h265".to_string(),
                "vp9".to_string(),
            ]),
            "h264,h265,av1"
        );
    }
}
