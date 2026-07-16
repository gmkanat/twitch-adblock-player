use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use twitch_adblock::playback::{PlaybackOptions, PlaybackRelay};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaybackSettings {
    enabled: bool,
    relay_url: String,
    twitch_token: String,
    relay_secret: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSettingsInput {
    pub enabled: bool,
    pub relay_url: String,
    pub twitch_token: Option<String>,
    pub relay_secret: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSettingsView {
    enabled: bool,
    relay_url: String,
    has_twitch_token: bool,
    has_relay_secret: bool,
}

impl PlaybackSettings {
    pub fn load() -> Result<Self> {
        let path = settings_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading playback settings {}", path.display()))?;
        serde_json::from_str(&data)
            .with_context(|| format!("parsing playback settings {}", path.display()))
    }

    pub fn apply(&mut self, input: PlaybackSettingsInput) -> Result<()> {
        let next = self.merged(input)?;
        next.persist()?;
        *self = next;
        Ok(())
    }

    fn merged(&self, input: PlaybackSettingsInput) -> Result<Self> {
        let mut next = self.clone();
        next.enabled = input.enabled;
        next.relay_url = input.relay_url.trim().trim_end_matches('/').to_string();
        if let Some(token) = input
            .twitch_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            next.twitch_token = token.to_string();
        }
        if let Some(secret) = input
            .relay_secret
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            next.relay_secret = secret.to_string();
        }
        next.validate()?;
        Ok(next)
    }

    pub fn clear(&mut self) -> Result<()> {
        let path = settings_path()?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("removing playback settings {}", path.display()));
            }
        }
        *self = Self::default();
        Ok(())
    }

    pub fn view(&self) -> PlaybackSettingsView {
        PlaybackSettingsView {
            enabled: self.enabled,
            relay_url: self.relay_url.clone(),
            has_twitch_token: !self.twitch_token.is_empty(),
            has_relay_secret: !self.relay_secret.is_empty(),
        }
    }

    pub fn playback_options(&self, supported_codecs: &[String]) -> Result<PlaybackOptions> {
        if !self.enabled {
            return Ok(PlaybackOptions::default());
        }
        let relay = PlaybackRelay::new(
            &self.relay_url,
            &self.twitch_token,
            (!self.relay_secret.is_empty()).then_some(self.relay_secret.as_str()),
        )?;
        Ok(PlaybackOptions::enhanced(relay, supported_codecs))
    }

    fn validate(&self) -> Result<()> {
        if self.enabled {
            let _ = PlaybackRelay::new(
                &self.relay_url,
                &self.twitch_token,
                (!self.relay_secret.is_empty()).then_some(self.relay_secret.as_str()),
            )?;
        }
        Ok(())
    }

    fn persist(&self) -> Result<()> {
        let path = settings_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        let contents = serde_json::to_vec_pretty(self).context("serializing playback settings")?;
        write_private(&path, &contents)
    }
}

fn settings_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not determine config directory")?;
    Ok(dir.join("twitch-adblock").join("playback.json"))
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
            .with_context(|| format!("opening playback settings {}", path.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing playback settings {}", path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("writing playback settings {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        options
            .open(path)
            .and_then(|mut file| file.write_all(contents))
            .with_context(|| format!("writing playback settings {}", path.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_stored_secrets_when_inputs_are_blank() {
        let settings = PlaybackSettings {
            enabled: true,
            relay_url: "https://relay.example".to_string(),
            twitch_token: "abcdefghijklmnopqrstuvwxyz1234".to_string(),
            relay_secret: "secret".to_string(),
        };
        let input = PlaybackSettingsInput {
            enabled: true,
            relay_url: "https://new-relay.example/".to_string(),
            twitch_token: None,
            relay_secret: Some(String::new()),
        };

        let settings = settings.merged(input).unwrap();

        assert_eq!(settings.relay_url, "https://new-relay.example");
        assert_eq!(settings.twitch_token, "abcdefghijklmnopqrstuvwxyz1234");
        assert_eq!(settings.relay_secret, "secret");
    }
}
