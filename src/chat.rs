//! Twitch chat over IRC on a WebSocket.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::auth::Auth;

const TWITCH_WS_URL: &str = "wss://irc-ws.chat.twitch.tv:443";
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub user: String,
    pub color: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum ChatEvent {
    Connected,
    Message(ChatMessage),
    System(String),
    Cleared,
    Reconnecting,
}

pub struct ChatHandle {
    commands: mpsc::UnboundedSender<Command>,
}

enum Command {
    Send(String),
    Switch(String),
}

#[derive(Clone)]
struct Identity {
    login: String,
    access_token: String,
}

impl ChatHandle {
    pub fn send(&self, text: String) {
        let _ = self.commands.send(Command::Send(text));
    }

    pub fn switch(&self, channel: String) {
        let _ = self.commands.send(Command::Switch(channel));
    }
}

/// Connect to one channel and start a worker that owns the socket. The worker
/// reconnects with bounded exponential backoff until its handle is dropped.
pub async fn connect(
    channel: &str,
    auth: &Auth,
) -> Result<(ChatHandle, mpsc::UnboundedReceiver<ChatEvent>)> {
    let channel = normalize_channel(channel)?;
    let identity = Identity {
        login: auth.login.clone(),
        access_token: auth.access_token.clone(),
    };
    let socket = open_socket(&channel, &identity).await?;

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let _ = event_tx.send(ChatEvent::Connected);

    tokio::spawn(run(socket, channel, identity, command_rx, event_tx));
    Ok((
        ChatHandle {
            commands: command_tx,
        },
        event_rx,
    ))
}

async fn run(
    mut socket: Socket,
    mut channel: String,
    identity: Identity,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<ChatEvent>,
) {
    let mut reconnect_delay = Duration::from_secs(1);

    loop {
        match drive_connection(&mut socket, &mut channel, &identity, &mut commands, &events).await {
            ConnectionEnd::Closed => return,
            ConnectionEnd::Disconnected => {}
        }

        if events.send(ChatEvent::Reconnecting).is_err() {
            return;
        }
        loop {
            tokio::time::sleep(reconnect_delay).await;
            match open_socket(&channel, &identity).await {
                Ok(new_socket) => {
                    socket = new_socket;
                    reconnect_delay = Duration::from_secs(1);
                    if events.send(ChatEvent::Connected).is_err() {
                        return;
                    }
                    break;
                }
                Err(error) => {
                    if events
                        .send(ChatEvent::System(format!("chat reconnect failed: {error}")))
                        .is_err()
                    {
                        return;
                    }
                    reconnect_delay = (reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
                }
            }
        }
    }
}

enum ConnectionEnd {
    Closed,
    Disconnected,
}

async fn drive_connection(
    socket: &mut Socket,
    channel: &mut String,
    identity: &Identity,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<ChatEvent>,
) -> ConnectionEnd {
    loop {
        tokio::select! {
            frame = socket.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        for line in text.split("\r\n").filter(|line| !line.is_empty()) {
                            if let Some(payload) = line.strip_prefix("PING ") {
                                if send_line(socket, &format!("PONG {payload}")).await.is_err() {
                                    return ConnectionEnd::Disconnected;
                                }
                                continue;
                            }

                            if let Some(event) = parse_irc_line(line) {
                                if matches!(event, ChatEvent::Reconnecting) {
                                    return ConnectionEnd::Disconnected;
                                }
                                if events.send(event).is_err() {
                                    return ConnectionEnd::Closed;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            return ConnectionEnd::Disconnected;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        return ConnectionEnd::Disconnected;
                    }
                    Some(Ok(_)) => {}
                }
            }
            command = commands.recv() => {
                match command {
                    Some(Command::Send(text)) => {
                        let text = text.replace(['\r', '\n'], " ").trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        if send_line(socket, &format!("PRIVMSG #{channel} :{text}")).await.is_err() {
                            return ConnectionEnd::Disconnected;
                        }
                        if events.send(ChatEvent::Message(ChatMessage {
                            user: identity.login.clone(),
                            color: None,
                            text,
                        })).is_err() {
                            return ConnectionEnd::Closed;
                        }
                    }
                    Some(Command::Switch(new_channel)) => {
                        let Ok(new_channel) = normalize_channel(&new_channel) else {
                            continue;
                        };
                        if new_channel == *channel {
                            continue;
                        }
                        let old_channel = std::mem::replace(channel, new_channel);
                        let switch = format!("PART #{old_channel}\r\nJOIN #{channel}");
                        if send_line(socket, &switch).await.is_err() {
                            return ConnectionEnd::Disconnected;
                        }
                        if events.send(ChatEvent::Cleared).is_err() {
                            return ConnectionEnd::Closed;
                        }
                    }
                    None => return ConnectionEnd::Closed,
                }
            }
        }
    }
}

async fn open_socket(channel: &str, identity: &Identity) -> Result<Socket> {
    let (mut socket, _) = match https_proxy() {
        Some(proxy) => connect_through_proxy(&proxy).await?,
        None => connect_async(TWITCH_WS_URL)
            .await
            .context("opening Twitch chat WebSocket")?,
    };
    for line in [
        format!("PASS oauth:{}", identity.access_token),
        format!("NICK {}", identity.login),
        "CAP REQ :twitch.tv/tags twitch.tv/commands".to_string(),
        format!("JOIN #{channel}"),
    ] {
        send_line(&mut socket, &line).await?;
    }
    Ok(socket)
}

fn https_proxy() -> Option<String> {
    ["HTTPS_PROXY", "https_proxy"].into_iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

async fn connect_through_proxy(
    proxy: &str,
) -> Result<(
    Socket,
    tokio_tungstenite::tungstenite::handshake::client::Response,
)> {
    let proxy = reqwest::Url::parse(proxy).context("invalid HTTPS_PROXY URL")?;
    if proxy.scheme() != "http" {
        bail!(
            "chat supports HTTP CONNECT proxies; got '{}://'",
            proxy.scheme()
        );
    }
    if !proxy.username().is_empty() || proxy.password().is_some() {
        bail!("authenticated HTTPS_PROXY URLs are not supported for chat");
    }

    let host = proxy.host_str().context("HTTPS_PROXY is missing a host")?;
    let port = proxy
        .port_or_known_default()
        .context("HTTPS_PROXY is missing a port")?;
    let mut stream = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("connecting to chat proxy {host}:{port}"))?;
    stream
        .write_all(
            b"CONNECT irc-ws.chat.twitch.tv:443 HTTP/1.1\r\n\
              Host: irc-ws.chat.twitch.tv:443\r\n\
              Proxy-Connection: Keep-Alive\r\n\r\n",
        )
        .await
        .context("sending chat proxy CONNECT request")?;

    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .context("reading chat proxy CONNECT response")?;
        if read == 0 {
            bail!("chat proxy closed the CONNECT tunnel");
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > 16 * 1024 {
            bail!("chat proxy returned an oversized CONNECT response");
        }

        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut response = httparse::Response::new(&mut headers);
        if response
            .parse(&bytes)
            .context("parsing chat proxy CONNECT response")?
            .is_complete()
        {
            if response.code != Some(200) {
                bail!(
                    "chat proxy CONNECT failed with HTTP {}",
                    response.code.unwrap_or(0)
                );
            }
            break;
        }
    }

    tokio_tungstenite::client_async_tls_with_config(TWITCH_WS_URL, stream, None, None)
        .await
        .context("opening Twitch chat WebSocket through HTTPS_PROXY")
}

async fn send_line(socket: &mut Socket, line: &str) -> Result<()> {
    socket.send(Message::text(format!("{line}\r\n"))).await?;
    Ok(())
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

fn parse_tags(blob: &str) -> HashMap<&str, &str> {
    blob.split(';')
        .filter_map(|pair| pair.split_once('='))
        .collect()
}

fn nick_from_prefix(prefix: &str) -> Option<&str> {
    prefix.split('!').next().filter(|nick| !nick.is_empty())
}

/// Parse one IRC line. Membership and user-state messages are intentionally ignored.
fn parse_irc_line(line: &str) -> Option<ChatEvent> {
    let mut rest = line.trim_end_matches(['\r', '\n']);
    if rest.is_empty() {
        return None;
    }

    let tags = if let Some(tagged) = rest.strip_prefix('@') {
        let (blob, remaining) = tagged.split_once(' ')?;
        rest = remaining;
        parse_tags(blob)
    } else {
        HashMap::new()
    };

    let prefix = if let Some(prefixed) = rest.strip_prefix(':') {
        let (prefix, remaining) = prefixed.split_once(' ')?;
        rest = remaining;
        Some(prefix)
    } else {
        None
    };

    match rest.split(' ').next()? {
        "PRIVMSG" => {
            let user = tags
                .get("display-name")
                .filter(|name| !name.is_empty())
                .copied()
                .or_else(|| prefix.and_then(nick_from_prefix))
                .unwrap_or("unknown")
                .to_string();
            let color = tags
                .get("color")
                .filter(|color| !color.is_empty())
                .map(|color| (*color).to_string());
            let text = rest.split_once(" :").map_or("", |(_, text)| text);
            Some(ChatEvent::Message(ChatMessage {
                user,
                color,
                text: text.to_string(),
            }))
        }
        "NOTICE" => Some(ChatEvent::System(
            rest.split_once(" :")
                .map_or("", |(_, text)| text)
                .to_string(),
        )),
        "CLEARCHAT" | "CLEARMSG" => Some(ChatEvent::Cleared),
        "RECONNECT" => Some(ChatEvent::Reconnecting),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tagged_message() {
        let line = "@badges=moderator/1;color=#1E90FF;display-name=CoolMod;emotes= :coolmod!coolmod@coolmod.tmi.twitch.tv PRIVMSG #channel :hello world";
        let Some(ChatEvent::Message(message)) = parse_irc_line(line) else {
            panic!("expected chat message");
        };
        assert_eq!(message.user, "CoolMod");
        assert_eq!(message.color.as_deref(), Some("#1E90FF"));
        assert_eq!(message.text, "hello world");
    }

    #[test]
    fn falls_back_to_nick_without_tags() {
        let line = ":someuser!someuser@someuser.tmi.twitch.tv PRIVMSG #channel :hello";
        let Some(ChatEvent::Message(message)) = parse_irc_line(line) else {
            panic!("expected chat message");
        };
        assert_eq!(message.user, "someuser");
        assert_eq!(message.color, None);
    }

    #[test]
    fn parses_control_events() {
        assert!(matches!(
            parse_irc_line(":tmi.twitch.tv NOTICE #channel :Login failed"),
            Some(ChatEvent::System(message)) if message == "Login failed"
        ));
        assert!(matches!(
            parse_irc_line(":tmi.twitch.tv CLEARCHAT #channel"),
            Some(ChatEvent::Cleared)
        ));
        assert!(matches!(
            parse_irc_line(":tmi.twitch.tv RECONNECT"),
            Some(ChatEvent::Reconnecting)
        ));
    }

    #[test]
    fn ignores_unneeded_messages() {
        assert!(parse_irc_line("").is_none());
        assert!(parse_irc_line(":tmi.twitch.tv 001 user :Welcome").is_none());
        assert!(parse_irc_line("@badges= :tmi.twitch.tv USERSTATE #channel").is_none());
    }

    #[test]
    fn validates_channel_names() {
        assert_eq!(
            normalize_channel(" #Some_Channel ").unwrap(),
            "some_channel"
        );
        assert!(normalize_channel("").is_err());
        assert!(normalize_channel("bad channel").is_err());
        assert!(normalize_channel("bad\r\nJOIN #other").is_err());
    }

    #[tokio::test]
    #[ignore = "requires Twitch network access and HTTPS_PROXY"]
    async fn opens_websocket_through_https_proxy() {
        let proxy = https_proxy().expect("HTTPS_PROXY must be set for this smoke test");
        let (_, response) = connect_through_proxy(&proxy).await.unwrap();
        assert_eq!(response.status().as_u16(), 101);
    }
}
