# twitch-adblock

Watch Twitch live streams from a terminal dashboard. The app lists followed
channels that are live, opens the selected stream in `mpv`, and provides
read/write chat. A local HLS proxy removes Twitch server-side ad segments before
the playlist reaches the player.

## Requirements

- Rust for building
- `mpv` on `PATH` for playback
- A Twitch application for the dashboard and chat login

On macOS, install the player with `brew install mpv`.

## Build

```sh
cargo build --release
```

The executable is written to `target/release/twitch-adblock`.

## Login

1. Register an application at <https://dev.twitch.tv/console/apps>.
2. Set its OAuth redirect URL to `http://localhost` and enable Device Code Grant.
3. Run:

```sh
twitch-adblock login --client-id <YOUR_CLIENT_ID>
```

The command opens Twitch's activation page and waits for approval. Credentials
are stored at `~/.config/twitch-adblock/auth.json` with owner-only permissions.
Expired access tokens are refreshed automatically.

## Usage

```sh
twitch-adblock
twitch-adblock watch <channel>
twitch-adblock watch <channel> --quality 720p60
twitch-adblock watch <channel> --proxy socks5://127.0.0.1:1080
twitch-adblock logout
```

The default command opens the dashboard and requires login. `watch` opens one
channel directly and does not require a Twitch account. Its options are:

- `--quality <name>`: `best`, `source`, `worst`, `audio_only`, or a rendition name
- `--player <command>`: player executable; defaults to `mpv`
- `--proxy <url>`: proxy for Twitch token and playlist requests

Dashboard controls:

- Up/Down or `j`/`k`: select a channel
- Enter: play the selected channel and join its chat
- `s`: sort by viewers, name, or uptime
- `r`: refresh followed live channels
- `c` or Tab: focus chat
- Esc or Tab: return to the channel list
- Page Up/Page Down: scroll chat
- `q` or Ctrl-C: quit

## Design

The code is one binary with six focused components:

- `auth.rs`: device login, credential storage, and token refresh
- `helix.rs`: followed live streams and display formatting
- `chat.rs`: one WebSocket owner task for IRC reads, writes, and reconnects
- `playback.rs`: playback-token resolution, local HLS proxy, and player lifecycle
- `playlist.rs`: pure master-playlist parsing and ad-segment filtering
- `tui.rs`: dashboard state, event loop, and rendering

Authentication used by Helix/chat is separate from the anonymous Twitch web
identity used to resolve playback. Playlist transformations remain pure and all
network/process resources have one explicit owner.

## Verification

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Device login, live Helix responses, chat, and playback require manual checks
against Twitch because they depend on external services and a live channel.

## Limitations

- Live channels only; VODs and clips are not supported.
- If every anonymous playback source is ad-gated, the proxy returns the original
  playlist. A proxy in another region may provide another source.
- Chat supports messages and basic clear/reconnect events, not moderation tools,
  whispers, or rendered emote images.
- The dashboard uses the first 100 followed live channels.
