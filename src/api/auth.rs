use std::path::PathBuf;
use librespot::core::authentication::Credentials;
use librespot::core::config::SessionConfig;
use librespot::core::session::Session;
use librespot::oauth::OAuthClientBuilder;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCredentials {
    pub username: String,
    pub password: Option<String>,
    pub token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SimplePlaylist {
    pub id: String,
    pub title: String,
    pub track_count: i32,
}

#[derive(Debug, Clone)]
pub struct SimpleTrack {
    pub id: String,
    pub index: i32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_str: String,
}

pub fn get_credentials_path() -> Option<PathBuf> {
    dirs::config_dir().map(|mut p| {
        p.push("spotify-light");
        p.push("credentials.json");
        p
    })
}

pub fn load_saved_credentials() -> Option<SavedCredentials> {
    let path = match get_credentials_path() {
        Some(p) => p,
        None => {
            warn!("[CONFIG] Could not determine user config directory for spotify-light");
            return None;
        }
    };

    if !path.exists() {
        info!("[CONFIG] No saved credentials found at {:?}", path);
        return None;
    }

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<SavedCredentials>(&content) {
            Ok(creds) => {
                info!("[CONFIG] Successfully loaded saved credentials for user '{}' from {:?}", creds.username, path);
                Some(creds)
            }
            Err(e) => {
                error!("[CONFIG ERROR] Corrupted credentials file at {:?}: {}. Clearing damaged config...", path, e);
                let _ = std::fs::remove_file(&path);
                None
            }
        },
        Err(e) => {
            error!("[CONFIG ERROR] Failed to read credentials file at {:?}: {}", path, e);
            None
        }
    }
}

pub fn save_credentials(creds: &SavedCredentials) -> Result<(), std::io::Error> {
    if let Some(path) = get_credentials_path() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                error!("[CONFIG ERROR] Failed to create config directory {:?}: {}", parent, e);
                return Err(e);
            }
        }
        let content = match serde_json::to_string_pretty(creds) {
            Ok(s) => s,
            Err(e) => {
                error!("[CONFIG ERROR] Failed to serialize credentials: {}", e);
                return Err(std::io::Error::new(std::io::ErrorKind::Other, e));
            }
        };

        if let Err(e) = std::fs::write(&path, content) {
            error!("[CONFIG ERROR] Failed to write credentials file {:?}: {}", path, e);
            return Err(e);
        }

        info!("[CONFIG] User credentials successfully persisted to {:?}", path);
    } else {
        error!("[CONFIG ERROR] Cannot save credentials: No valid config directory available");
    }
    Ok(())
}

pub fn clear_saved_credentials() {
    if let Some(path) = get_credentials_path() {
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                error!("[CONFIG ERROR] Failed to delete credentials file {:?}: {}", path, e);
            } else {
                info!("[CONFIG] Cleared saved credentials file at {:?}", path);
            }
        }
    }
}

pub async fn connect_with_password(username: &str, password: &str) -> Result<Session, String> {
    info!("[AUTH] Initiating authentication with username/password for: {}", username);
    let session_config = SessionConfig::default();
    let session = Session::new(session_config, None);
    let credentials = Credentials::with_password(username, password);

    match session.connect(credentials, false).await {
        Ok(_) => {
            info!("[AUTH SUCCESS] Successfully authenticated Spotify session for '{}'", username);
            Ok(session)
        }
        Err(e) => {
            let err_str = e.to_string();
            error!("[AUTH FAILURE] Authentication failed for '{}': {}", username, err_str);
            if err_str.to_lowercase().contains("bad credentials") || err_str.to_lowercase().contains("invalid") {
                Err("Spotify отклонил прямой пароль (аккаунт привязан к Google/Apple или требует подтверждения). Используйте зеленую кнопку 'Войти через браузер'".to_string())
            } else if err_str.to_lowercase().contains("challenge") || err_str.to_lowercase().contains("needs approval") {
                Err("Spotify требует подтверждение через браузер. Нажмите 'Войти через браузер'".to_string())
            } else {
                Err(format!("Ошибка входа: {}", err_str))
            }
        }
    }
}

pub async fn connect_with_token(token: &str) -> Result<Session, String> {
    info!("[AUTH] Initiating authentication with access token");
    let session_config = SessionConfig::default();
    let session = Session::new(session_config, None);
    let credentials = Credentials::with_access_token(token);

    match session.connect(credentials, false).await {
        Ok(_) => {
            info!("[AUTH SUCCESS] Successfully authenticated Spotify session with token");
            Ok(session)
        }
        Err(e) => {
            error!("[AUTH FAILURE] Token authentication failed: {}", e);
            Err(format!("Ошибка авторизации по токену: {}", e))
        }
    }
}

pub async fn login_with_oauth() -> Result<(Session, String), String> {
    info!("[OAUTH] Starting interactive Spotify OAuth PKCE flow...");
    let client_id = "65b708073fc0480ea92a077233ca87bd";
    let redirect_uri = "http://127.0.0.1:8898/login";
    let scopes = vec![
        "streaming",
        "user-read-playback-state",
        "user-modify-playback-state",
        "playlist-read-private",
        "playlist-read-collaborative",
        "user-library-read",
        "user-read-email",
        "user-read-private",
    ];

    let custom_html = r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>SpotifyLight - Вход выполнен</title>
<style>
body { background: #121212; color: #1db954; font-family: -apple-system, sans-serif; display: flex; flex-direction: column; align-items: center; justify-content: center; height: 80vh; }
h1 { font-size: 28px; margin-bottom: 8px; }
p { color: #b3b3b3; font-size: 15px; }
</style>
<script>setTimeout(function(){ window.close(); }, 2000);</script>
</head>
<body>
    <h1>✔ Вход успешно выполнен!</h1>
    <p>SpotifyLight подключен к вашему аккаунту. Вы можете закрыть эту вкладку.</p>
</body>
</html>"#;

    let oauth_client = OAuthClientBuilder::new(client_id, redirect_uri, scopes)
        .open_in_browser()
        .with_custom_message(custom_html)
        .build()
        .map_err(|e| {
            error!("[OAUTH ERROR] Failed to construct OAuthClient: {}", e);
            format!("Ошибка инициализации OAuth: {}", e)
        })?;

    info!("[OAUTH] Waiting for authorization in browser at http://127.0.0.1:8898/login...");
    let token = oauth_client
        .get_access_token_async()
        .await
        .map_err(|e| {
            error!("[OAUTH ERROR] Failed to obtain token from browser: {}", e);
            format!("Ошибка авторизации в браузере: {}", e)
        })?;

    info!("[OAUTH] Received access token! Connecting session to Spotify...");
    let credentials = Credentials::with_access_token(&token.access_token);
    let session_config = SessionConfig::default();
    let session = Session::new(session_config, None);

    session
        .connect(credentials, false)
        .await
        .map_err(|e| {
            error!("[OAUTH ERROR] Failed to connect session with access token: {}", e);
            format!("Ошибка подключения к Spotify по токену: {}", e)
        })?;

    let username = session.username().to_string();
    info!("[OAUTH SUCCESS] Authenticated as user '{}' via OAuth PKCE", username);

    let _ = save_credentials(&SavedCredentials {
        username: username.clone(),
        password: None,
        token: Some(token.access_token.clone()),
    });

    Ok((session, token.access_token))
}

#[derive(Clone)]
pub struct SpotifyApiClient {
    client: reqwest::Client,
    access_token: String,
}

impl SpotifyApiClient {
    pub fn new(access_token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            access_token,
        }
    }

    pub async fn get_current_user(&self) -> Result<String, String> {
        let url = "https://api.spotify.com/v1/me";
        info!("[API] Fetching current user profile from {}", url);

        let res = self.client.get(url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| {
                error!("[API NETWORK ERROR] Failed to send request to {}: {}", url, e);
                format!("Сетевая ошибка: {}", e)
            })?;

        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            error!("[API HTTP ERROR] GET {} returned status {}: {}", url, status, body);
            return Err(format!("Spotify API returned status {}: {}", status, body));
        }

        let json: serde_json::Value = res.json().await.map_err(|e| {
            error!("[API PARSE ERROR] Failed to parse JSON response from {}: {}", url, e);
            format!("Ошибка парсинга ответа: {}", e)
        })?;

        let name = json.get("display_name")
            .and_then(|v| v.as_str())
            .or_else(|| json.get("id").and_then(|v| v.as_str()))
            .unwrap_or("User");

        info!("[API SUCCESS] Current user profile resolved: {}", name);
        Ok(name.to_string())
    }

    pub async fn get_playlists(&self) -> Result<Vec<SimplePlaylist>, String> {
        let url = "https://api.spotify.com/v1/me/playlists?limit=50";
        info!("[API] Fetching user playlists from {}", url);

        let res = self.client.get(url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| {
                error!("[API NETWORK ERROR] Failed to send request to {}: {}", url, e);
                format!("Сетевая ошибка: {}", e)
            })?;

        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            error!("[API HTTP ERROR] GET {} returned status {}: {}", url, status, body);
            return Err(format!("Spotify API returned status {}: {}", status, body));
        }

        let json: serde_json::Value = res.json().await.map_err(|e| {
            error!("[API PARSE ERROR] Failed to parse JSON response from {}: {}", url, e);
            format!("Ошибка парсинга ответа: {}", e)
        })?;

        let items = json.get("items").and_then(|v| v.as_array());
        let mut list = Vec::new();

        if let Some(arr) = items {
            for item in arr {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let total_tracks = item.get("tracks")
                    .and_then(|t| t.get("total"))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0) as i32;

                list.push(SimplePlaylist {
                    id,
                    title: name,
                    track_count: total_tracks,
                });
            }
        }

        info!("[API SUCCESS] Retrieved {} playlists from Spotify", list.len());
        Ok(list)
    }

    pub async fn get_playlist_tracks(&self, playlist_id: &str) -> Result<Vec<SimpleTrack>, String> {
        let url = format!("https://api.spotify.com/v1/playlists/{}/tracks?limit=100", playlist_id);
        info!("[API] Fetching tracks for playlist '{}' from {}", playlist_id, url);

        let res = self.client.get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| {
                error!("[API NETWORK ERROR] Failed to send request to {}: {}", url, e);
                format!("Сетевая ошибка: {}", e)
            })?;

        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            error!("[API HTTP ERROR] GET {} returned status {}: {}", url, status, body);
            return Err(format!("Spotify API returned status {}: {}", status, body));
        }

        let json: serde_json::Value = res.json().await.map_err(|e| {
            error!("[API PARSE ERROR] Failed to parse JSON response from {}: {}", url, e);
            format!("Ошибка парсинга ответа: {}", e)
        })?;

        let items = json.get("items").and_then(|v| v.as_array());
        let mut tracks = Vec::new();

        if let Some(arr) = items {
            for (idx, item) in arr.iter().enumerate() {
                if let Some(track) = item.get("track").filter(|t| !t.is_null()) {
                    let id = track.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let title = track.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let artist = track.get("artists")
                        .and_then(|a| a.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|art| art.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown Artist")
                        .to_string();
                    let album = track.get("album")
                        .and_then(|a| a.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let duration_ms = track.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let duration_str = format!("{}:{:02}", duration_ms / 60000, (duration_ms % 60000) / 1000);

                    tracks.push(SimpleTrack {
                        id,
                        index: (idx + 1) as i32,
                        title,
                        artist,
                        album,
                        duration_str,
                    });
                }
            }
        }

        info!("[API SUCCESS] Retrieved {} tracks for playlist '{}'", tracks.len(), playlist_id);
        Ok(tracks)
    }
}
