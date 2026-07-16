#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use twitch_adblock::{
    auth::{self, Auth},
    chat::{self, ChatHandle},
    helix::{self, Category, Stream},
    playback::{self, StreamProxy},
};

struct DesktopState {
    client: reqwest::Client,
    auth: Mutex<Option<Auth>>,
    stream: Mutex<Option<StreamProxy>>,
    chat: Mutex<Option<ChatHandle>>,
    playback_generation: AtomicU64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionStatus {
    authenticated: bool,
    login: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackInfo {
    channel: String,
    playlist_url: String,
    qualities: Vec<String>,
}

#[tauri::command]
async fn session_status(state: State<'_, DesktopState>) -> Result<SessionStatus, String> {
    let auth = state.auth.lock().await;
    Ok(SessionStatus {
        authenticated: auth.is_some(),
        login: auth.as_ref().map(|auth| auth.login_name().to_string()),
    })
}

#[tauri::command]
async fn login(
    app: AppHandle,
    state: State<'_, DesktopState>,
    client_id: String,
) -> Result<SessionStatus, String> {
    let prompt_app = app.clone();
    let auth = auth::login_with_handler(&state.client, client_id, move |authorization| {
        let _ = prompt_app.emit("oauth-prompt", authorization);
        auth::open_browser(&authorization.verification_uri);
    })
    .await
    .map_err(|error| error.to_string())?;

    let login = auth.login_name().to_string();
    *state.auth.lock().await = Some(auth);
    Ok(SessionStatus {
        authenticated: true,
        login: Some(login),
    })
}

#[tauri::command]
async fn logout(state: State<'_, DesktopState>) -> Result<(), String> {
    state.playback_generation.fetch_add(1, Ordering::SeqCst);
    state.chat.lock().await.take();
    state.stream.lock().await.take();
    Auth::logout().map_err(|error| error.to_string())?;
    *state.auth.lock().await = None;
    Ok(())
}

#[tauri::command]
async fn followed_streams(state: State<'_, DesktopState>) -> Result<Vec<Stream>, String> {
    let mut auth = state.auth.lock().await;
    let auth = auth
        .as_mut()
        .ok_or_else(|| "log in before loading followed streams".to_string())?;
    helix::followed_live(&state.client, auth)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn popular_streams(state: State<'_, DesktopState>) -> Result<Vec<Stream>, String> {
    let mut auth = state.auth.lock().await;
    let auth = auth
        .as_mut()
        .ok_or_else(|| "log in before browsing streams".to_string())?;
    helix::popular_live(&state.client, auth)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn top_categories(state: State<'_, DesktopState>) -> Result<Vec<Category>, String> {
    let mut auth = state.auth.lock().await;
    let auth = auth
        .as_mut()
        .ok_or_else(|| "log in before browsing categories".to_string())?;
    helix::top_categories(&state.client, auth)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn category_streams(
    state: State<'_, DesktopState>,
    game_id: String,
) -> Result<Vec<Stream>, String> {
    let mut auth = state.auth.lock().await;
    let auth = auth
        .as_mut()
        .ok_or_else(|| "log in before browsing categories".to_string())?;
    helix::category_live(&state.client, auth, &game_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn search_streams(
    state: State<'_, DesktopState>,
    query: String,
) -> Result<Vec<Stream>, String> {
    let mut auth = state.auth.lock().await;
    let auth = auth
        .as_mut()
        .ok_or_else(|| "log in before searching channels".to_string())?;
    helix::search_live(&state.client, auth, &query)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn play_channel(
    app: AppHandle,
    state: State<'_, DesktopState>,
    channel: String,
    quality: Option<String>,
    switch_chat: Option<bool>,
) -> Result<PlaybackInfo, String> {
    let generation = state.playback_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let quality = quality.unwrap_or_else(|| "best".to_string());
    let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel();
    let proxy = StreamProxy::start(&state.client, &channel, &quality, Some(status_tx))
        .await
        .map_err(|error| error.to_string())?;
    if state.playback_generation.load(Ordering::SeqCst) != generation {
        return Err("playback request superseded".to_string());
    }
    let playlist_url = proxy.local_url().to_string();
    let qualities = proxy.qualities().to_vec();
    *state.stream.lock().await = Some(proxy);

    let status_app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(status) = status_rx.recv().await {
            let _ = status_app.emit("playback-status", status);
        }
    });

    if switch_chat.unwrap_or(true) {
        let chat_app = app.clone();
        let chat_channel = channel.clone();
        tauri::async_runtime::spawn(async move {
            let state = chat_app.state::<DesktopState>();
            if let Err(error) = connect_chat(&chat_app, &state, &chat_channel).await {
                let _ = chat_app.emit("chat-event", chat::ChatEvent::System(error));
            }
        });
    }

    Ok(PlaybackInfo {
        channel: channel.to_ascii_lowercase(),
        playlist_url,
        qualities,
    })
}

async fn connect_chat(
    app: &AppHandle,
    state: &State<'_, DesktopState>,
    channel: &str,
) -> Result<(), String> {
    let mut current_chat = state.chat.lock().await;
    if let Some(handle) = current_chat.as_ref() {
        return handle
            .switch(channel.to_string())
            .map_err(|error| error.to_string());
    }

    let auth = state
        .auth
        .lock()
        .await
        .clone()
        .ok_or_else(|| "log in before connecting to chat".to_string())?;
    let (handle, mut events) = chat::connect(channel, &auth)
        .await
        .map_err(|error| error.to_string())?;
    *current_chat = Some(handle);

    let chat_app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events.recv().await {
            let _ = chat_app.emit("chat-event", event);
        }
    });
    Ok(())
}

#[tauri::command]
async fn send_chat(state: State<'_, DesktopState>, message: String) -> Result<(), String> {
    let chat = state.chat.lock().await;
    let handle = chat
        .as_ref()
        .ok_or_else(|| "select a channel before sending chat".to_string())?;
    handle.send(message);
    Ok(())
}

#[tauri::command]
async fn stop_playback(state: State<'_, DesktopState>) -> Result<(), String> {
    state.playback_generation.fetch_add(1, Ordering::SeqCst);
    state.stream.lock().await.take();
    Ok(())
}

#[tauri::command]
fn set_fullscreen(window: tauri::WebviewWindow, fullscreen: bool) -> Result<(), String> {
    window
        .set_fullscreen(fullscreen)
        .map_err(|error| error.to_string())
}

fn main() {
    let client = playback::build_client(None).expect("build HTTP client");
    let auth = Auth::load().unwrap_or_else(|error| {
        eprintln!("could not load cached Twitch login: {error}");
        None
    });

    tauri::Builder::default()
        .manage(DesktopState {
            client,
            auth: Mutex::new(auth),
            stream: Mutex::new(None),
            chat: Mutex::new(None),
            playback_generation: AtomicU64::new(0),
        })
        .invoke_handler(tauri::generate_handler![
            session_status,
            login,
            logout,
            followed_streams,
            popular_streams,
            top_categories,
            category_streams,
            search_streams,
            play_channel,
            send_chat,
            stop_playback,
            set_fullscreen,
        ])
        .run(tauri::generate_context!())
        .expect("run Twitch Adblock desktop app");
}
