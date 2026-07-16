use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use twitch_adblock::{
    auth::{self, Auth},
    playback::{self, PlaybackSession},
    tui,
};

#[derive(Parser)]
#[command(
    name = "twitch-adblock",
    about = "Ad-free Twitch in your terminal (TUI + chat)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Log in via Twitch's device code flow
    Login {
        /// Client ID of a public Twitch application with Device Code Grant enabled
        #[arg(long)]
        client_id: Option<String>,
    },
    /// Log out (delete cached token)
    Logout,
    /// Watch a single channel directly, without the TUI
    Watch {
        /// Twitch channel login name
        channel: String,
        /// Stream rendition such as best, source, 720p60, or audio_only
        #[arg(short, long, default_value = "best")]
        quality: String,
        /// HTTP or SOCKS proxy for playback requests
        #[arg(long)]
        proxy: Option<String>,
        /// Player executable to launch
        #[arg(long, default_value = "mpv")]
        player: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Login { client_id }) => {
            let client = playback::build_client(None)?;
            let client_id = match client_id {
                Some(id) => id,
                None => prompt("Enter your Twitch app Client ID: ")?,
            };
            let auth = auth::login(&client, client_id).await?;
            println!("logged in as {}", auth.login_name());
        }
        Some(Cmd::Logout) => {
            Auth::logout()?;
            println!("logged out");
        }
        Some(Cmd::Watch {
            channel,
            quality,
            proxy,
            player,
        }) => {
            let client = playback::build_client(proxy.as_deref())?;
            let mut session =
                PlaybackSession::start(&client, &channel, &quality, &player, None).await?;
            eprintln!("playing {channel}");
            session.wait().await?;
        }
        None => {
            let client = playback::build_client(None)?;
            match Auth::load()? {
                Some(a) => tui::run(client, a).await?,
                None => bail!(
                    "not logged in; run `twitch-adblock login --client-id <id>`\n\
                     or use `twitch-adblock watch <channel>` without an account"
                ),
            }
        }
    }
    Ok(())
}

fn prompt(msg: &str) -> Result<String> {
    use std::io::{self, Write};
    print!("{msg}");
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    let value = s.trim().to_string();
    if value.is_empty() {
        bail!("client ID cannot be empty");
    }
    Ok(value)
}
