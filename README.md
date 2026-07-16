# Twitch Adblock

A cross-platform Twitch desktop application with followed live channels, HLS
video, and chat in one window. Rust resolves and filters the Twitch playlist;
the operating system WebView renders the bundled `hls.js` player. Desktop users
do not need `mpv`, Node.js, or a browser extension.

## Run

Install the stable Rust toolchain, clone the repository, then run:

```sh
cargo run --release
```

The workspace defaults to the desktop application. On first launch, enter the
Client ID of a public Twitch application with Device Code Grant enabled. The app
opens Twitch's activation page and displays the authorization code.

Register an application at <https://dev.twitch.tv/console/apps> with OAuth
redirect URL `http://localhost`. A client secret is not used and must not be
distributed.

## Platform Setup

### Windows

- Install Rust with `rustup-init.exe` using the default MSVC toolchain.
- Install the Visual Studio C++ Build Tools when prompted by Rust.
- Windows 10 and 11 normally include the WebView2 runtime.

Then run from PowerShell:

```powershell
cargo run --release
```

### macOS

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo run --release
```

The application uses the WKWebView included with macOS.

### Ubuntu/Debian

Install Rust, a compiler, and the WebKitGTK development packages required to
build Tauri:

```sh
sudo apt update
sudo apt install -y build-essential curl wget file \
  libwebkit2gtk-4.1-dev libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev \
  gstreamer1.0-libav gstreamer1.0-plugins-good gstreamer1.0-plugins-bad
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo run --release
```

Other Linux distributions need equivalent WebKitGTK 4.1 and GStreamer codec
packages. These are system WebView components, not a separate media player.

## Credentials

OAuth credentials are refreshed automatically and stored in the platform config
directory:

- Windows: `%APPDATA%\twitch-adblock\auth.json`
- macOS: `~/Library/Application Support/twitch-adblock/auth.json`
- Linux: `${XDG_CONFIG_HOME:-~/.config}/twitch-adblock/auth.json`

Unix files are restricted to owner read/write permissions. Use the desktop Log
out command or the CLI logout command to delete the cached credentials.

## Desktop Build

The frontend is static and bundled, so no Node.js build step is required.

```sh
cargo build --release -p twitch-adblock-desktop
```

To produce a platform installer, install the Tauri CLI and build from the
desktop package:

```sh
cargo install tauri-cli --version '^2' --locked
cd desktop
cargo tauri build
```

Tauri produces the native format for the current platform, such as a Windows
installer, macOS app bundle, or Linux package. Code signing is a separate release
requirement.

## Legacy CLI

The terminal dashboard and direct `mpv` playback remain available while the
desktop application is validated:

```sh
cargo run --release -p twitch-adblock -- login --client-id <CLIENT_ID>
cargo run --release -p twitch-adblock
cargo run --release -p twitch-adblock -- watch <channel>
```

Only these legacy playback commands require `mpv` on `PATH`.

## Verification

```sh
cargo fmt --all -- --check
cargo test -p twitch-adblock --all-targets
cargo check -p twitch-adblock-desktop
cargo clippy --workspace --all-targets -- -D warnings
node desktop/tests/ui-smoke.mjs
```

The UI smoke test requires Chrome and uses only Node.js standard-library APIs;
Node is not needed to build or run the application. CI checks the Rust workspace
on Windows, macOS, and Ubuntu.

Live login, Twitch API, chat, and playback still require a manual test against a
live channel.

## Architecture

- `src/auth.rs`: device login, credential persistence, and token refresh
- `src/helix.rs`: followed live streams and stream metadata
- `src/chat.rs`: one reconnecting IRC WebSocket owner
- `src/playback.rs`: anonymous playback resolution and filtered local HLS proxy
- `src/playlist.rs`: pure HLS parsing and ad-segment filtering
- `desktop/src/main.rs`: Tauri state and commands
- `desktop/ui/`: bundled video, followed-channel, and chat interface
- `src/tui.rs`: retained terminal dashboard

The core is a library shared by the desktop and terminal binaries. The desktop
uses `StreamProxy` directly; the legacy terminal wraps the same proxy in an
external-player lifecycle.

## Rollback

The pre-desktop implementation is committed on `main` and tagged `pre-tauri`.
Desktop work lives on `desktop-tauri`.

```sh
git switch main
```

Return to the desktop implementation with:

```sh
git switch desktop-tauri
```

## Limitations

- Live channels only; VODs and clips are not supported.
- The desktop app currently lists the first 100 followed live channels.
- If every anonymous source is ad-gated, the proxy returns the original playlist.
- Chat supports messages and basic clear/reconnect events, not moderation tools,
  whispers, or rendered emote images.
