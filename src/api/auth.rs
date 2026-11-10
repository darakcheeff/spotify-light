use std::path::PathBuf;
use std::time::Duration;
use librespot::core::authentication::Credentials;
use librespot::core::config::SessionConfig;
pub use librespot::core::session::Session;
use librespot::oauth::OAuthClientBuilder;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCredentials {
    pub username: String,
    pub password: Option<String>,
    pub token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SimplePlaylist {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub track_count: i32,
    pub image_url: String,
}

#[derive(Debug, Clone)]
pub struct SimpleTrack {
    pub id: String,
    pub index: i32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_str: String,
    pub image_url: String,
}

pub fn extract_image_url(val: &serde_json::Value) -> String {
    val.get("images")
        .and_then(|imgs| imgs.as_array())
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn extract_track_image_url(val: &serde_json::Value) -> String {
    if let Some(url) = val.get("album")
        .and_then(|a| a.get("images"))
        .and_then(|imgs| imgs.as_array())
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(|u| u.as_str()) {
        return url.to_string();
    }
    extract_image_url(val)
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

    let refresh_token_opt = if token.refresh_token.is_empty() {
        None
    } else {
        Some(token.refresh_token.clone())
    };

    let _ = save_credentials(&SavedCredentials {
        username: username.clone(),
        password: None,
        token: Some(token.access_token.clone()),
        refresh_token: refresh_token_opt,
        client_id: Some(client_id.to_string()),
    });

    Ok((session, token.access_token))
}

pub async fn refresh_access_token(client_id: &str, refresh_token: &str) -> Result<(String, Option<String>), String> {
    info!("[OAUTH] Refreshing Spotify access token using refresh_token...");
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    let res = client.post("https://accounts.spotify.com/api/token")
        .form(&params)
        .send()
        .await
        .map_err(|e| {
            error!("[OAUTH REFRESH ERROR] Failed to send refresh request: {}", e);
            format!("Ошибка сети при обновлении токена: {}", e)
        })?;

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        error!("[OAUTH REFRESH ERROR] Spotify returned status {}: {}", status, err_body);
        return Err(format!("Не удалось обновить токен ({}): {}", status, err_body));
    }

    let json: serde_json::Value = res.json().await.map_err(|e| {
        error!("[OAUTH REFRESH ERROR] Failed to parse JSON: {}", e);
        format!("Ошибка парсинга ответа токена: {}", e)
    })?;

    let new_access_token = json.get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing access_token in response".to_string())?
        .to_string();

    let new_refresh_token = json.get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    info!("[OAUTH REFRESH SUCCESS] Successfully acquired refreshed access token");
    Ok((new_access_token, new_refresh_token))
}

#[derive(Clone)]
pub struct SpotifyApiClient {
    client: reqwest::Client,
    access_token: String,
    username: String,
}

impl SpotifyApiClient {
    pub fn new(access_token: String, username: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            access_token,
            username,
        }
    }

    async fn send_with_retry(&self, url: &str) -> Result<serde_json::Value, String> {
        let mut attempts = 0;
        let max_attempts = 4;

        loop {
            attempts += 1;
            let res = self.client.get(url)
                .bearer_auth(&self.access_token)
                .send()
                .await
                .map_err(|e| {
                    error!("[API NETWORK ERROR] Failed to send request to {}: {}", url, e);
                    format!("Сетевая ошибка: {}", e)
                })?;

            let status = res.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempts <= max_attempts {
                let delay = res.headers()
                    .get("Retry-After")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(2)
                    .clamp(1, 15);
                warn!("[API RATE LIMIT] Status 429 on {}. Retrying in {} seconds (attempt {}/{})...", url, delay, attempts, max_attempts);
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }

            if !status.is_success() {
                let body = res.text().await.unwrap_or_default();
                error!("[API HTTP ERROR] GET {} returned status {}: {}", url, status, body);
                return Err(format!("Spotify API error {}: {}", status, body));
            }

            let json: serde_json::Value = res.json().await.map_err(|e| {
                error!("[API PARSE ERROR] Failed to parse JSON response from {}: {}", url, e);
                format!("Ошибка парсинга ответа: {}", e)
            })?;

            return Ok(json);
        }
    }

    pub async fn get_current_user(&self) -> Result<String, String> {
        info!("[API] Fetching current user profile for '{}'...", self.username);

        // 1. Native spclient profile endpoint (instant, unthrottled)
        let sp_url = format!("https://spclient.wg.spotify.com/user-profile-view/v3/profile/{}", self.username);
        if let Ok(res) = self.client.get(&sp_url)
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await 
        {
            if res.status().is_success() {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    if let Some(name) = json.get("name").and_then(|v| v.as_str()) {
                        info!("[API SUCCESS] User profile resolved via spclient: {}", name);
                        return Ok(name.to_string());
                    }
                }
            }
        }

        // 2. Fallback to Web API
        let url = "https://api.spotify.com/v1/me";
        info!("[API] Falling back to Web API: {}", url);
        match self.send_with_retry(url).await {
            Ok(json) => {
                let name = json.get("display_name")
                    .and_then(|v| v.as_str())
                    .or_else(|| json.get("id").and_then(|v| v.as_str()))
                    .unwrap_or(&self.username);
                info!("[API SUCCESS] Current user profile resolved via Web API: {}", name);
                Ok(name.to_string())
            }
            Err(_) => Ok(self.username.clone()),
        }
    }

    pub async fn get_playlists(&self) -> Result<Vec<SimplePlaylist>, String> {
        info!("[API] Fetching user playlists for '{}'...", self.username);

        // 1. Native spclient rootlist (unthrottled, instant)
        let sp_url = format!("https://spclient.wg.spotify.com/playlist/v2/user/{}/rootlist", self.username);
        if let Ok(res) = self.client.get(&sp_url)
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await 
        {
            if res.status().is_success() {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    if let Some(items) = json.get("contents").and_then(|c| c.get("items")).and_then(|i| i.as_array()) {
                        let mut pids = Vec::new();
                        for item in items.iter().take(40) {
                            if let Some(uri) = item.get("uri").and_then(|u| u.as_str()) {
                                if let Some(pid) = uri.strip_prefix("spotify:playlist:") {
                                    pids.push(pid.to_string());
                                }
                            }
                        }

                        let mut handles = Vec::new();
                        for pid in pids {
                            let client = self.client.clone();
                            let token = self.access_token.clone();
                            handles.push(tokio::spawn(async move {
                                let p_url = format!("https://spclient.wg.spotify.com/playlist/v2/playlist/{}", pid);
                                let mut title = format!("Плейлист {}", pid);
                                let mut count = 0;

                                let mut image_url = "".to_string();
                                if let Ok(pres) = client.get(&p_url)
                                    .bearer_auth(&token)
                                    .header("Accept", "application/json")
                                    .send()
                                    .await 
                                {
                                    if let Ok(pjson) = pres.json::<serde_json::Value>().await {
                                        if let Some(name) = pjson.get("attributes").and_then(|a| a.get("name")).and_then(|n| n.as_str()) {
                                            title = name.to_string();
                                        }
                                        if let Some(len) = pjson.get("length").and_then(|l| l.as_i64()) {
                                            count = len as i32;
                                        }
                                        image_url = extract_image_url(&pjson);
                                    }
                                }

                                SimplePlaylist {
                                    id: pid,
                                    title,
                                    subtitle: format!("Плейлист • {} треков", count),
                                    track_count: count,
                                    image_url,
                                }
                            }));
                        }

                        let mut list = Vec::new();
                        for h in handles {
                            if let Ok(pl) = h.await {
                                list.push(pl);
                            }
                        }

                        if !list.is_empty() {
                            info!("[API SUCCESS] Retrieved {} playlists via spclient", list.len());
                            return Ok(list);
                        }
                    }
                }
            }
        }

        // 2. Fallback to Web API
        let url = "https://api.spotify.com/v1/me/playlists?limit=50";
        info!("[API] Falling back to Web API: {}", url);
        let json = self.send_with_retry(url).await?;
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

                let image_url = extract_image_url(item);
                list.push(SimplePlaylist {
                    id,
                    title: name,
                    subtitle: format!("Плейлист • {} треков", total_tracks),
                    track_count: total_tracks,
                    image_url,
                });
            }
        }

        info!("[API SUCCESS] Retrieved {} playlists from Spotify Web API", list.len());
        Ok(list)
    }

    pub async fn get_followed_artists(&self) -> Result<Vec<SimplePlaylist>, String> {
        info!("[API] Fetching followed artists for '{}' via spclient...", self.username);
        let sp_url = format!("https://spclient.wg.spotify.com/user-profile-view/v3/profile/{}/following", self.username);
        let res = self.client.get(&sp_url)
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("Сетевая ошибка: {}", e))?;

        if res.status().is_success() {
            if let Ok(json) = res.json::<serde_json::Value>().await {
                if let Some(profiles) = json.get("profiles").and_then(|p| p.as_array()) {
                    let mut artists = Vec::new();
                    for item in profiles {
                        let uri = item.get("uri").and_then(|u| u.as_str()).unwrap_or_default();
                        let name = item.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                        let id = uri.strip_prefix("spotify:artist:").unwrap_or(uri).to_string();
                        let image_url = extract_image_url(item);
                        if !id.is_empty() && !name.is_empty() {
                            artists.push(SimplePlaylist {
                                id,
                                title: name.to_string(),
                                subtitle: "Исполнитель".to_string(),
                                track_count: 0,
                                image_url,
                            });
                        }
                    }
                    info!("[API SUCCESS] Retrieved {} followed artists via spclient", artists.len());
                    return Ok(artists);
                }
            }
        }
        Ok(Vec::new())
    }

    pub async fn get_saved_albums(&self) -> Result<Vec<SimplePlaylist>, String> {
        info!("[API] Fetching saved albums for '{}'...", self.username);
        let url = "https://api.spotify.com/v1/me/albums?limit=50";
        match self.send_with_retry(url).await {
            Ok(json) => {
                let items = json.get("items").and_then(|v| v.as_array());
                let mut list = Vec::new();
                if let Some(arr) = items {
                    for item in arr {
                        if let Some(album) = item.get("album") {
                            let id = album.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                            let name = album.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                            let artist = album.get("artists")
                                .and_then(|a| a.as_array())
                                .and_then(|arr| arr.first())
                                .and_then(|art| art.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("Альбом")
                                .to_string();
                            let total_tracks = album.get("total_tracks")
                                .and_then(|n| n.as_u64())
                                .unwrap_or(0) as i32;
                            let image_url = extract_image_url(album);

                            list.push(SimplePlaylist {
                                id,
                                title: name,
                                subtitle: format!("Альбом • {}", artist),
                                track_count: total_tracks,
                                image_url,
                            });
                        }
                    }
                }
                info!("[API SUCCESS] Retrieved {} saved albums", list.len());
                Ok(list)
            }
            Err(e) => {
                warn!("[API WARNING] Could not fetch saved albums (rate limit/auth): {}", e);
                Ok(Vec::new())
            }
        }
    }

    pub async fn get_playlist_tracks(&self, playlist_id: &str) -> Result<Vec<SimpleTrack>, String> {
        info!("[API] Fetching tracks for playlist '{}'...", playlist_id);

        // 1. Native spclient playlist contents (unthrottled)
        let sp_url = format!("https://spclient.wg.spotify.com/playlist/v2/playlist/{}", playlist_id);
        if let Ok(res) = self.client.get(&sp_url)
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await 
        {
            if res.status().is_success() {
                if let Ok(pjson) = res.json::<serde_json::Value>().await {
                    if let Some(items) = pjson.get("contents").and_then(|c| c.get("items")).and_then(|i| i.as_array()) {
                        let mut track_ids = Vec::new();
                        for item in items.iter().take(100) {
                            if let Some(uri) = item.get("uri").and_then(|u| u.as_str()) {
                                if let Some(tid) = uri.strip_prefix("spotify:track:") {
                                    track_ids.push(tid.to_string());
                                }
                            }
                        }

                        let mut handles = Vec::new();
                        for (idx, tid) in track_ids.into_iter().enumerate() {
                            let client = self.client.clone();
                            let token = self.access_token.clone();
                            handles.push(tokio::spawn(async move {
                                let cred_url = format!("https://spclient.wg.spotify.com/track-credits-view/v0/experimental/{}/credits", tid);
                                let mut title = format!("Трек {}", idx + 1);
                                let mut artist = "Неизвестный исполнитель".to_string();

                                if let Ok(cres) = client.get(&cred_url)
                                    .bearer_auth(&token)
                                    .header("Accept", "application/json")
                                    .send()
                                    .await 
                                {
                                    if let Ok(cjson) = cres.json::<serde_json::Value>().await {
                                        if let Some(t) = cjson.get("trackTitle").and_then(|v| v.as_str()) {
                                            title = t.to_string();
                                        }
                                        if let Some(roles) = cjson.get("roleCredits").and_then(|v| v.as_array()) {
                                            for role in roles {
                                                if let Some(title_role) = role.get("roleTitle").and_then(|v| v.as_str()) {
                                                    if title_role.eq_ignore_ascii_case("Performers") || title_role.eq_ignore_ascii_case("Artists") {
                                                        if let Some(arts) = role.get("artists").and_then(|a| a.as_array()) {
                                                            if let Some(first_art) = arts.first() {
                                                                if let Some(aname) = first_art.get("name").and_then(|n| n.as_str()) {
                                                                    artist = aname.to_string();
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                SimpleTrack {
                                    id: tid,
                                    index: (idx + 1) as i32,
                                    title,
                                    artist,
                                    album: "".to_string(),
                                    duration_str: "".to_string(),
                                    image_url: "".to_string(),
                                }
                            }));
                        }

                        let mut tracks = Vec::new();
                        for h in handles {
                            if let Ok(t) = h.await {
                                tracks.push(t);
                            }
                        }

                        if !tracks.is_empty() {
                            // Enrich durations and album names in batch requests (up to 50 at a time)
                            let client = self.client.clone();
                            let token = self.access_token.clone();
                            for chunk in tracks.chunks_mut(50) {
                                let ids: Vec<String> = chunk.iter().map(|t| t.id.clone()).collect();
                                let batch_url = format!("https://api.spotify.com/v1/tracks?ids={}", ids.join(","));
                                if let Ok(b_res) = client.get(&batch_url).bearer_auth(&token).send().await {
                                    if b_res.status().is_success() {
                                        if let Ok(b_json) = b_res.json::<serde_json::Value>().await {
                                            if let Some(arr) = b_json.get("tracks").and_then(|v| v.as_array()) {
                                                for (i, t_val) in arr.iter().enumerate() {
                                                    if let Some(t_item) = chunk.get_mut(i) {
                                                        if let Some(album_name) = t_val.get("album").and_then(|a| a.get("name")).and_then(|n| n.as_str()) {
                                                            t_item.album = album_name.to_string();
                                                        }
                                                        if let Some(d_ms) = t_val.get("duration_ms").and_then(|d| d.as_u64()) {
                                                            t_item.duration_str = format!("{}:{:02}", d_ms / 60000, (d_ms % 60000) / 1000);
                                                        }
                                                        t_item.image_url = extract_track_image_url(t_val);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            info!("[API SUCCESS] Retrieved {} tracks via spclient for playlist '{}'", tracks.len(), playlist_id);
                            return Ok(tracks);
                        }
                    }
                }
            }
        }

        // 2. Fallback to Web API
        let url = format!("https://api.spotify.com/v1/playlists/{}/tracks?limit=100", playlist_id);
        info!("[API] Falling back to Web API for tracks: {}", url);
        let json = self.send_with_retry(&url).await?;
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
                    let image_url = extract_track_image_url(track);

                    tracks.push(SimpleTrack {
                        id,
                        index: (idx + 1) as i32,
                        title,
                        artist,
                        album,
                        duration_str,
                        image_url,
                    });
                }
            }
        }

        info!("[API SUCCESS] Retrieved {} tracks for playlist '{}'", tracks.len(), playlist_id);
        Ok(tracks)
    }

    pub async fn get_album_tracks(&self, album_id: &str) -> Result<Vec<SimpleTrack>, String> {
        info!("[API] Fetching album tracks for '{}' via spclient...", album_id);
        let sp_url = format!("https://spclient.wg.spotify.com/album-entity-view/v2/album/{}", album_id);
        if let Ok(res) = self.client.get(&sp_url)
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await 
        {
            if res.status().is_success() {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    let album_title = json.get("title").and_then(|t| t.as_str()).unwrap_or("Альбом").to_string();
                    if let Some(body) = json.get("body").and_then(|b| b.as_array()) {
                        let mut list = Vec::new();
                        let mut idx = 1;
                        for b in body {
                            let meta = b.get("metadata");
                            let uri = meta.and_then(|m| m.get("uri")).and_then(|u| u.as_str()).unwrap_or_default();
                            if let Some(tid) = uri.strip_prefix("spotify:track:") {
                                let title = b.get("text").and_then(|t| t.get("title")).and_then(|s| s.as_str()).unwrap_or("Трек").to_string();
                                let artist = b.get("text").and_then(|t| t.get("subtitle")).and_then(|s| s.as_str()).unwrap_or("Исполнитель").to_string();

                                list.push(SimpleTrack {
                                    id: tid.to_string(),
                                    index: idx,
                                    title,
                                    artist,
                                    album: album_title.clone(),
                                    duration_str: "3:30".to_string(),
                                    image_url: "".to_string(),
                                });
                                idx += 1;
                            }
                        }
                        if !list.is_empty() {
                            info!("[API SUCCESS] Retrieved {} tracks via spclient for album '{}'", list.len(), album_id);
                            return Ok(list);
                        }
                    }
                }
            }
        }

        info!("[API] Falling back to Web API for album tracks: {}", album_id);
        let url = format!("https://api.spotify.com/v1/albums/{}/tracks?limit=50", album_id);
        match self.send_with_retry(&url).await {
            Ok(json) => {
                let mut list = Vec::new();
                if let Some(items) = json.get("items").and_then(|v| v.as_array()) {
                    for (idx, item) in items.iter().enumerate() {
                        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                        let title = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                        let artist = item.get("artists")
                            .and_then(|a| a.as_array())
                            .and_then(|arr| arr.first())
                            .and_then(|art| art.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("Неизвестный исполнитель")
                            .to_string();
                        let duration_ms = item.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                        let duration_str = format!("{}:{:02}", duration_ms / 60000, (duration_ms % 60000) / 1000);

                        list.push(SimpleTrack {
                            id,
                            index: (idx + 1) as i32,
                            title,
                            artist,
                            album: "".to_string(),
                            duration_str,
                            image_url: "".to_string(),
                        });
                    }
                }
                info!("[API SUCCESS] Retrieved {} tracks for album '{}'", list.len(), album_id);
                Ok(list)
            }
            Err(e) => {
                error!("[API ERROR] Could not fetch album tracks: {}", e);
                Err(e)
            }
        }
    }

    pub async fn get_artist_top_tracks(&self, artist_id: &str) -> Result<Vec<SimpleTrack>, String> {
        info!("[API] Fetching top tracks for artist '{}' via spclient...", artist_id);
        let sp_url = format!("https://spclient.wg.spotify.com/artistview/v1/artist/{}", artist_id);
        if let Ok(res) = self.client.get(&sp_url)
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json")
            .send()
            .await 
        {
            if res.status().is_success() {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    let artist_name = json.get("title").and_then(|t| t.as_str()).unwrap_or("Исполнитель").to_string();
                    if let Some(body) = json.get("body").and_then(|b| b.as_array()) {
                        let mut list = Vec::new();
                        let mut idx = 1;
                        for b in body {
                            let meta = b.get("metadata");
                            let uri = meta.and_then(|m| m.get("uri")).and_then(|u| u.as_str()).unwrap_or_default();
                            if let Some(tid) = uri.strip_prefix("spotify:track:") {
                                let title = b.get("text").and_then(|t| t.get("title")).and_then(|s| s.as_str()).unwrap_or("Трек").to_string();
                                let playcount = b.get("text").and_then(|t| t.get("subtitle")).and_then(|s| s.as_str()).unwrap_or_default().to_string();

                                list.push(SimpleTrack {
                                    id: tid.to_string(),
                                    index: idx,
                                    title,
                                    artist: artist_name.clone(),
                                    album: playcount,
                                    duration_str: "3:30".to_string(),
                                    image_url: "".to_string(),
                                });
                                idx += 1;
                            }
                        }
                        if !list.is_empty() {
                            info!("[API SUCCESS] Retrieved {} top tracks via spclient for artist '{}'", list.len(), artist_id);
                            return Ok(list);
                        }
                    }
                }
            }
        }

        info!("[API] Falling back to Web API for artist top tracks: {}", artist_id);
        let url = format!("https://api.spotify.com/v1/artists/{}/top-tracks?market=from_token", artist_id);
        match self.send_with_retry(&url).await {
            Ok(json) => {
                let mut list = Vec::new();
                if let Some(tracks) = json.get("tracks").and_then(|v| v.as_array()) {
                    for (idx, item) in tracks.iter().enumerate() {
                        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                        let title = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                        let artist = item.get("artists")
                            .and_then(|a| a.as_array())
                            .and_then(|arr| arr.first())
                            .and_then(|art| art.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("Неизвестный исполнитель")
                            .to_string();
                        let album = item.get("album")
                            .and_then(|a| a.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let duration_ms = item.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                        let duration_str = format!("{}:{:02}", duration_ms / 60000, (duration_ms % 60000) / 1000);
                        let image_url = extract_track_image_url(item);

                        list.push(SimpleTrack {
                            id,
                            index: (idx + 1) as i32,
                            title,
                            artist,
                            album,
                            duration_str,
                            image_url,
                        });
                    }
                }
                info!("[API SUCCESS] Retrieved {} top tracks for artist '{}'", list.len(), artist_id);
                Ok(list)
            }
            Err(e) => {
                error!("[API ERROR] Could not fetch artist top tracks: {}", e);
                Err(e)
            }
        }
    }

    pub async fn search(&self, query: &str) -> Result<SearchResult, String> {
        let url = reqwest::Url::parse_with_params(
            "https://api.spotify.com/v1/search",
            &[("q", query), ("type", "track,artist,album,playlist"), ("limit", "10")]
        ).map_err(|e| format!("URL error: {}", e))?;

        info!("[API] Global search for query: '{}'", query);
        let json = self.send_with_retry(url.as_str()).await?;
        let mut res = SearchResult::default();

        if let Some(items) = json.get("tracks").and_then(|t| t.get("items")).and_then(|v| v.as_array()) {
            for (idx, item) in items.iter().enumerate() {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let title = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let artist = item.get("artists")
                    .and_then(|a| a.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|art| art.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Неизвестный исполнитель")
                    .to_string();
                let album = item.get("album")
                    .and_then(|a| a.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let duration_ms = item.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let duration_str = format!("{}:{:02}", duration_ms / 60000, (duration_ms % 60000) / 1000);
                let image_url = extract_track_image_url(item);

                res.tracks.push(SimpleTrack {
                    id,
                    index: (idx + 1) as i32,
                    title,
                    artist,
                    album,
                    duration_str,
                    image_url,
                });
            }
        }

        if let Some(items) = json.get("artists").and_then(|a| a.get("items")).and_then(|v| v.as_array()) {
            for item in items {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let title = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let followers = item.get("followers").and_then(|f| f.get("total")).and_then(|t| t.as_u64()).unwrap_or(0);
                let image_url = extract_image_url(item);
                res.artists.push(SimplePlaylist {
                    id,
                    title,
                    subtitle: format!("Исполнитель • {} слушателей", followers),
                    track_count: 0,
                    image_url,
                });
            }
        }

        if let Some(items) = json.get("albums").and_then(|a| a.get("items")).and_then(|v| v.as_array()) {
            for item in items {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let title = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let artist = item.get("artists")
                    .and_then(|a| a.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|art| art.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Альбом")
                    .to_string();
                let image_url = extract_image_url(item);
                res.albums.push(SimplePlaylist {
                    id,
                    title,
                    subtitle: format!("Альбом • {}", artist),
                    track_count: 0,
                    image_url,
                });
            }
        }

        if let Some(items) = json.get("playlists").and_then(|p| p.get("items")).and_then(|v| v.as_array()) {
            for item in items {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let title = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let owner = item.get("owner").and_then(|o| o.get("display_name")).and_then(|v| v.as_str()).unwrap_or("Spotify");
                let image_url = extract_image_url(item);
                res.playlists.push(SimplePlaylist {
                    id,
                    title,
                    subtitle: format!("Плейлист • {}", owner),
                    track_count: 0,
                    image_url,
                });
            }
        }

        info!("[API SUCCESS] Search resolved {} tracks, {} artists, {} albums, {} playlists",
            res.tracks.len(), res.artists.len(), res.albums.len(), res.playlists.len());
        Ok(res)
    }

    pub async fn get_recently_played(&self) -> Result<Vec<SimpleTrack>, String> {
        let url = "https://api.spotify.com/v1/me/player/recently-played?limit=20";
        info!("[API] Fetching recently played tracks...");
        let json = self.send_with_retry(url).await?;
        let mut list = Vec::new();
        if let Some(items) = json.get("items").and_then(|v| v.as_array()) {
            for (idx, item) in items.iter().enumerate() {
                if let Some(track) = item.get("track") {
                    let id = track.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let title = track.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let artist = track.get("artists")
                        .and_then(|a| a.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|art| art.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Неизвестный исполнитель")
                        .to_string();
                    let album = track.get("album")
                        .and_then(|a| a.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let duration_ms = track.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let duration_str = format!("{}:{:02}", duration_ms / 60000, (duration_ms % 60000) / 1000);
                    let image_url = extract_track_image_url(track);

                    list.push(SimpleTrack {
                        id,
                        index: (idx + 1) as i32,
                        title,
                        artist,
                        album,
                        duration_str,
                        image_url,
                    });
                }
            }
        }
        info!("[API SUCCESS] Retrieved {} recently played tracks", list.len());
        Ok(list)
    }

    pub async fn add_track_to_playlist(&self, playlist_id: &str, track_id: &str) -> Result<(), String> {
        let url = format!("https://api.spotify.com/v1/playlists/{}/tracks", playlist_id);
        let track_uri = if track_id.starts_with("spotify:track:") {
            track_id.to_string()
        } else {
            format!("spotify:track:{}", track_id)
        };
        let body = serde_json::json!({
            "uris": [track_uri]
        });
        info!("[API] Adding track {} to playlist {}", track_id, playlist_id);
        let res = self.client.post(&url)
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;
        if res.status().is_success() {
            info!("[API SUCCESS] Track added to playlist");
            Ok(())
        } else {
            let err = res.text().await.unwrap_or_default();
            Err(format!("Error adding to playlist: {}", err))
        }
    }

    pub async fn save_track(&self, track_id: &str) -> Result<(), String> {
        let url = format!("https://api.spotify.com/v1/me/tracks?ids={}", track_id);
        info!("[API] Saving track to Liked Songs: {}", track_id);
        let res = self.client.put(&url)
            .bearer_auth(&self.access_token)
            .header("Content-Length", 0)
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;
        if res.status().is_success() {
            info!("[API SUCCESS] Track saved to Liked Songs");
            Ok(())
        } else {
            Err(format!("Error saving track: {}", res.status()))
        }
    }

    pub async fn remove_track(&self, track_id: &str) -> Result<(), String> {
        let url = format!("https://api.spotify.com/v1/me/tracks?ids={}", track_id);
        info!("[API] Removing track from Liked Songs: {}", track_id);
        let res = self.client.delete(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;
        if res.status().is_success() {
            info!("[API SUCCESS] Track removed from Liked Songs");
            Ok(())
        } else {
            Err(format!("Error removing track: {}", res.status()))
        }
    }

    pub async fn create_playlist(&self, name: &str) -> Result<SimplePlaylist, String> {
        let url = "https://api.spotify.com/v1/me/playlists";
        let body = serde_json::json!({
            "name": name,
            "public": false,
            "description": "Создано в SpotifyLight"
        });
        info!("[API] Creating new playlist: '{}'", name);
        let res = self.client.post(url)
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;
        if res.status().is_success() {
            let json: serde_json::Value = res.json().await.map_err(|e| format!("JSON error: {}", e))?;
            let id = json.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let title = json.get("name").and_then(|v| v.as_str()).unwrap_or(name).to_string();
            info!("[API SUCCESS] Playlist created with id: {}", id);
            Ok(SimplePlaylist {
                id,
                title,
                subtitle: "Плейлист • Вы".to_string(),
                track_count: 0,
                image_url: "".to_string(),
            })
        } else {
            let err = res.text().await.unwrap_or_default();
            Err(format!("Error creating playlist: {}", err))
        }
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<(), String> {
        let url = format!("https://api.spotify.com/v1/playlists/{}/followers", playlist_id);
        info!("[API] Unfollowing/deleting playlist: {}", playlist_id);
        let res = self.client.delete(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;
        if res.status().is_success() {
            info!("[API SUCCESS] Playlist unfollowed/deleted");
            Ok(())
        } else {
            Err(format!("Error deleting playlist: {}", res.status()))
        }
    }

    pub async fn get_devices(&self) -> Result<Vec<(String, String, String, bool)>, String> {
        let url = "https://api.spotify.com/v1/me/player/devices";
        let json = self.send_with_retry(url).await?;
        let mut devices = Vec::new();
        if let Some(items) = json.get("devices").and_then(|d| d.as_array()) {
            for item in items {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("Устройство").to_string();
                let d_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("Speaker").to_string();
                let is_active = item.get("is_active").and_then(|v| v.as_bool()).unwrap_or(false);
                devices.push((id, name, d_type, is_active));
            }
        }
        Ok(devices)
    }

    pub async fn play_uri_on_device(&self, uri: &str, device_id: Option<&str>) -> Result<(), String> {
        let mut url = "https://api.spotify.com/v1/me/player/play".to_string();
        if let Some(did) = device_id {
            url.push_str(&format!("?device_id={}", did));
        }
        let body = serde_json::json!({
            "uris": [uri]
        });
        let res = self.client.put(&url)
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;
        if res.status().is_success() {
            info!("[API SUCCESS] Playback started on Spotify device");
            Ok(())
        } else {
            let err = res.text().await.unwrap_or_default();
            Err(format!("Error starting device playback: {}", err))
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub tracks: Vec<SimpleTrack>,
    pub artists: Vec<SimplePlaylist>,
    pub albums: Vec<SimplePlaylist>,
    pub playlists: Vec<SimplePlaylist>,
}
