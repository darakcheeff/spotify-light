use std::path::PathBuf;
use librespot::core::authentication::Credentials;
use librespot::core::config::SessionConfig;
use librespot::core::session::Session;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

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
    let path = get_credentials_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save_credentials(creds: &SavedCredentials) -> Result<(), std::io::Error> {
    if let Some(path) = get_credentials_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let content = serde_json::to_string_pretty(creds)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, content)?;
        info!("Saved user credentials to {:?}", path);
    }
    Ok(())
}

pub fn clear_saved_credentials() {
    if let Some(path) = get_credentials_path() {
        let _ = std::fs::remove_file(path);
        info!("Cleared saved credentials at {:?}", path);
    }
}

pub async fn connect_with_password(username: &str, password: &str) -> Result<Session, String> {
    info!("Connecting to Spotify with username/password for {}", username);
    let session_config = SessionConfig::default();
    let session = Session::new(session_config, None);
    let credentials = Credentials::with_password(username, password);

    match session.connect(credentials, false).await {
        Ok(_) => {
            info!("Successfully authenticated session for {}", username);
            Ok(session)
        }
        Err(e) => {
            let err_str = e.to_string();
            error!("Authentication failed for {}: {}", username, err_str);
            if err_str.to_lowercase().contains("bad credentials") || err_str.to_lowercase().contains("invalid") {
                Err("Неверный логин или пароль".to_string())
            } else if err_str.to_lowercase().contains("challenge") || err_str.to_lowercase().contains("needs approval") {
                Err("Spotify требует подтверждение через почту или браузер".to_string())
            } else {
                Err(format!("Ошибка входа: {}", err_str))
            }
        }
    }
}

pub async fn connect_with_token(token: &str) -> Result<Session, String> {
    info!("Connecting to Spotify with access token");
    let session_config = SessionConfig::default();
    let session = Session::new(session_config, None);
    let credentials = Credentials::with_access_token(token);

    match session.connect(credentials, false).await {
        Ok(_) => {
            info!("Successfully authenticated session with token");
            Ok(session)
        }
        Err(e) => {
            error!("Token authentication failed: {}", e);
            Err(format!("Ошибка авторизации по токену: {}", e))
        }
    }
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
        let res = self.client.get("https://api.spotify.com/v1/me")
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !res.status().is_success() {
            return Err(format!("Spotify API error: {}", res.status()));
        }

        let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        let name = json.get("display_name")
            .and_then(|v| v.as_str())
            .or_else(|| json.get("id").and_then(|v| v.as_str()))
            .unwrap_or("User");

        Ok(name.to_string())
    }

    pub async fn get_playlists(&self) -> Result<Vec<SimplePlaylist>, String> {
        let res = self.client.get("https://api.spotify.com/v1/me/playlists?limit=50")
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !res.status().is_success() {
            return Err(format!("Spotify API error: {}", res.status()));
        }

        let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
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

        Ok(list)
    }

    pub async fn get_playlist_tracks(&self, playlist_id: &str) -> Result<Vec<SimpleTrack>, String> {
        let url = format!("https://api.spotify.com/v1/playlists/{}/tracks?limit=100", playlist_id);
        let res = self.client.get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !res.status().is_success() {
            return Err(format!("Spotify API error: {}", res.status()));
        }

        let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
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

        Ok(tracks)
    }
}
