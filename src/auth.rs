//! Twitch authentication via OAuth Device Code Flow.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// OAuth scopes requested during the device-code flow:
/// followed-live list (`user:read:follows`) + chat read/send (`chat:read chat:edit`).
const SCOPES: &str = "user:read:follows chat:read chat:edit";

const DEVICE_URL: &str = "https://id.twitch.tv/oauth2/device";
const TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const USERS_URL: &str = "https://api.twitch.tv/helix/users";
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Logged-in Twitch user + OAuth tokens. Persisted to
/// `~/.config/twitch-adblock/auth.json` (mode 600).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Auth {
    /// The registered app's Client ID (public client; no secret).
    pub(crate) client_id: String,
    pub(crate) access_token: String,
    refresh_token: String,
    /// Numeric Twitch user id of the logged-in account.
    pub(crate) user_id: String,
    /// Login name (lowercase), used as the IRC NICK.
    pub(crate) login: String,
}

impl Auth {
    /// Load cached auth from the config dir, if present and valid.
    pub fn load() -> Result<Option<Auth>> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading auth file {}", path.display()))?;
        let auth: Auth = serde_json::from_str(&data)
            .with_context(|| format!("parsing auth file {}", path.display()))?;
        Ok(Some(auth))
    }

    /// Persist to the config dir with permissions 600.
    fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serializing auth")?;
        write_private(&path, json.as_bytes())?;
        Ok(())
    }

    /// Delete the cached auth file (idempotent).
    pub fn logout() -> Result<()> {
        let path = config_path()?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing auth file {}", path.display())),
        }
    }

    /// Refresh `access_token` using `refresh_token`; persist on success.
    pub async fn refresh(&mut self, client: &reqwest::Client) -> Result<()> {
        let resp = client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.refresh_token.as_str()),
                ("client_id", self.client_id.as_str()),
            ])
            .send()
            .await
            .context("sending token refresh request")?;

        let status = resp.status();
        let body = resp.text().await.context("reading refresh response body")?;
        if !status.is_success() {
            bail!("token refresh failed (HTTP {status}): {body}");
        }
        let token: TokenResponse =
            serde_json::from_str(&body).context("parsing token refresh response")?;
        let access_token = token
            .access_token
            .ok_or_else(|| anyhow!("token refresh response missing access_token: {body}"))?;
        self.access_token = access_token;
        // Twitch may rotate the refresh token; keep the existing one if absent.
        if let Some(refresh_token) = token.refresh_token {
            self.refresh_token = refresh_token;
        }
        self.save()?;
        Ok(())
    }
}

/// Path to the persisted auth file: `<config dir>/twitch-adblock/auth.json`.
fn config_path() -> Result<std::path::PathBuf> {
    let dir = dirs::config_dir().ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(dir.join("twitch-adblock").join("auth.json"))
}

fn write_private(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        options.mode(0o600);
        let mut file = options
            .open(path)
            .with_context(|| format!("opening auth file {}", path.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("writing auth file {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        options
            .open(path)
            .and_then(|mut file| file.write_all(contents))
            .with_context(|| format!("writing auth file {}", path.display()))?;
    }

    Ok(())
}

/// Best-effort attempt to open `url` in the user's browser. Never fails: tries
/// the macOS `open`, then `xdg-open` (Linux), ignoring any errors.
fn open_browser(url: &str) {
    use std::process::Command;
    for program in ["open", "xdg-open"] {
        if Command::new(program).arg(url).spawn().is_ok() {
            return;
        }
    }
}

/// Response from `POST id.twitch.tv/oauth2/device`.
#[derive(Debug, Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

/// Response from `POST id.twitch.tv/oauth2/token` (success or error shape).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    /// Error message on failure, e.g. `authorization_pending` / `expired_token`.
    #[serde(default)]
    message: Option<String>,
}

/// Response from `GET helix/users`.
#[derive(Debug, Deserialize)]
struct UsersResponse {
    data: Vec<UserInfo>,
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    id: String,
    login: String,
}

/// Run the Device Code Flow with the given app Client ID. Prints the user code +
/// URL, polls until approved, fetches the user id/login, persists, and returns Auth.
pub async fn login(client: &reqwest::Client, client_id: String) -> Result<Auth> {
    let client_id = client_id.trim().to_string();
    if client_id.is_empty() {
        bail!("client ID cannot be empty");
    }

    // 1) Request a device + user code.
    let resp = client
        .post(DEVICE_URL)
        .form(&[("client_id", client_id.as_str()), ("scopes", SCOPES)])
        .send()
        .await
        .context("sending device authorization request")?;
    let status = resp.status();
    let body = resp.text().await.context("reading device response body")?;
    if !status.is_success() {
        bail!("device authorization failed (HTTP {status}): {body}");
    }
    let device: DeviceResponse =
        serde_json::from_str(&body).context("parsing device authorization response")?;

    // 2) Show the user where to go, and try to auto-open the browser.
    println!(
        "Go to {} and enter code: {}",
        device.verification_uri, device.user_code
    );
    open_browser(&device.verification_uri);

    // 3) Poll for the token until the user approves (or we hit a terminal error).
    let mut interval = device.interval.max(1);
    let expires_at =
        tokio::time::Instant::now() + std::time::Duration::from_secs(device.expires_in);
    let (access_token, refresh_token) = loop {
        let delay = std::time::Duration::from_secs(interval);
        if tokio::time::Instant::now() + delay >= expires_at {
            bail!("device code expired before authorization completed");
        }
        tokio::time::sleep(delay).await;

        let resp = client
            .post(TOKEN_URL)
            .form(&[
                ("client_id", client_id.as_str()),
                ("grant_type", DEVICE_CODE_GRANT),
                ("device_code", device.device_code.as_str()),
            ])
            .send()
            .await
            .context("sending token poll request")?;

        let status = resp.status();
        let body = resp.text().await.context("reading token poll body")?;
        let token: TokenResponse =
            serde_json::from_str(&body).context("parsing token poll response")?;

        if let Some(access_token) = token.access_token {
            let refresh_token = token
                .refresh_token
                .ok_or_else(|| anyhow!("token response missing refresh_token"))?;
            break (access_token, refresh_token);
        }

        // No access_token yet: decide whether to keep polling or give up.
        let message = token.message.unwrap_or_default();
        let pending = message.eq_ignore_ascii_case("authorization_pending");
        if pending {
            continue;
        }
        if message.eq_ignore_ascii_case("slow_down") {
            interval += 5;
            continue;
        }
        // Some implementations omit a message but still respond 400 while pending;
        // treat an empty message with a 400 as pending too, anything else is terminal.
        if message.is_empty() && status.as_u16() == 400 {
            continue;
        }
        bail!("device code authorization failed (HTTP {status}): {message}");
    };

    // 4) Fetch the logged-in user's identity, build, persist, and return.
    let (user_id, login) = fetch_identity(client, &client_id, &access_token).await?;
    let auth = Auth {
        client_id,
        access_token,
        refresh_token,
        user_id,
        login,
    };
    auth.save()?;
    Ok(auth)
}

/// Fetch the logged-in user's id + login from `helix/users` (also validates the token).
async fn fetch_identity(
    client: &reqwest::Client,
    client_id: &str,
    access_token: &str,
) -> Result<(String, String)> {
    let resp = client
        .get(USERS_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Client-Id", client_id)
        .send()
        .await
        .context("sending helix/users request")?;
    let status = resp.status();
    let body = resp.text().await.context("reading helix/users body")?;
    if !status.is_success() {
        bail!("fetching user identity failed (HTTP {status}): {body}\n(is the token valid and does --client-id match the token's app?)");
    }
    let users: UsersResponse =
        serde_json::from_str(&body).context("parsing helix/users response")?;
    let user = users
        .data
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("helix/users returned no user"))?;
    Ok((user.id, user.login))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_serde_round_trip() {
        let auth = Auth {
            client_id: "abc123".to_string(),
            access_token: "access-tok".to_string(),
            refresh_token: "refresh-tok".to_string(),
            user_id: "456789".to_string(),
            login: "somestreamer".to_string(),
        };

        let json = serde_json::to_string(&auth).expect("serialize");
        let back: Auth = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(auth.client_id, back.client_id);
        assert_eq!(auth.access_token, back.access_token);
        assert_eq!(auth.refresh_token, back.refresh_token);
        assert_eq!(auth.user_id, back.user_id);
        assert_eq!(auth.login, back.login);
    }

    #[test]
    fn device_response_parses() {
        let body = r#"{
            "device_code": "dev-code",
            "user_code": "ABCD-1234",
            "verification_uri": "https://www.twitch.tv/activate",
            "expires_in": 1800,
            "interval": 5
        }"#;
        let dr: DeviceResponse = serde_json::from_str(body).expect("parse device response");
        assert_eq!(dr.device_code, "dev-code");
        assert_eq!(dr.user_code, "ABCD-1234");
        assert_eq!(dr.verification_uri, "https://www.twitch.tv/activate");
        assert_eq!(dr.interval, 5);
    }

    #[test]
    fn token_pending_then_success_parses() {
        let pending = r#"{"status":400,"message":"authorization_pending"}"#;
        let tr: TokenResponse = serde_json::from_str(pending).expect("parse pending");
        assert!(tr.access_token.is_none());
        assert_eq!(tr.message.as_deref(), Some("authorization_pending"));

        let success = r#"{
            "access_token": "at",
            "refresh_token": "rt",
            "expires_in": 14000,
            "scope": ["user:read:follows", "chat:read", "chat:edit"],
            "token_type": "bearer"
        }"#;
        let tr: TokenResponse = serde_json::from_str(success).expect("parse success");
        assert_eq!(tr.access_token.as_deref(), Some("at"));
        assert_eq!(tr.refresh_token.as_deref(), Some("rt"));
    }

    #[test]
    fn users_response_parses() {
        let body = r#"{
            "data": [
                {"id": "141981764", "login": "twitchdev", "display_name": "TwitchDev",
                 "type": "", "broadcaster_type": "partner"}
            ]
        }"#;
        let ur: UsersResponse = serde_json::from_str(body).expect("parse users");
        assert_eq!(ur.data.len(), 1);
        assert_eq!(ur.data[0].id, "141981764");
        assert_eq!(ur.data[0].login, "twitchdev");
    }
}
