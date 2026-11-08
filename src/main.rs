mod api;
mod audio;
mod discovery;
mod mpris;

slint::include_modules!();

use std::sync::Arc;
use std::time::Duration;
use audio::{AudioCommand, AudioEngine};
use discovery::SpeakerDiscovery;
use mpris::MprisService;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use api::{
    clear_saved_credentials, connect_with_password, connect_with_token, load_saved_credentials,
    save_credentials, SavedCredentials, SpotifyApiClient,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging with unsuppressed stderr/stdout output
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,spotify_light=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();

    info!("============================================================");
    info!("Starting SpotifyLight (Native Rust + Slint, Zero-Electron)...");
    info!("============================================================");

    // Initialize Audio Engine with Self-Healing Output Supervisor
    let (audio_engine, rx) = AudioEngine::new();
    let audio_sender = audio_engine.clone();

    // Background Audio Supervisor (handles PipeWire/PulseAudio restarts and device disconnections)
    let supervisor_cmd_tx = audio_engine.command_sender();
    tokio::spawn(async move {
        AudioEngine::run_supervisor_loop(rx, supervisor_cmd_tx).await;
    });

    // Initialize Speaker Discovery (Google Home, Connect)
    let speaker_discovery = Arc::new(SpeakerDiscovery::new());
    speaker_discovery.start();

    // Initialize MPRIS2 (D-Bus)
    if let Err(e) = MprisService::init().await {
        error!("[DBUS ERROR] Failed to register MPRIS2 service: {}", e);
    }

    // Build Slint UI
    let app = AppWindow::new()?;
    let app_weak = app.as_weak();

    // Shared Spotify Web API client state
    let active_api_client: Arc<Mutex<Option<SpotifyApiClient>>> = Arc::new(Mutex::new(None));

    // UI Callbacks: Audio
    app.on_play_pause({
        let engine = audio_sender.clone();
        move || {
            info!("[UI ACTION] Play/Pause clicked");
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.send_command(AudioCommand::Toggle).await;
            });
        }
    });

    app.on_next_track({
        let engine = audio_sender.clone();
        move || {
            info!("[UI ACTION] Next track clicked");
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.send_command(AudioCommand::Next).await;
            });
        }
    });

    app.on_prev_track({
        let engine = audio_sender.clone();
        move || {
            info!("[UI ACTION] Previous track clicked");
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.send_command(AudioCommand::Prev).await;
            });
        }
    });

    app.on_seek_position({
        let engine = audio_sender.clone();
        move |pos| {
            info!("[UI ACTION] Seek position requested: {:.2}", pos);
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.send_command(AudioCommand::Seek(pos)).await;
            });
        }
    });

    app.on_change_volume({
        let engine = audio_sender.clone();
        move |vol| {
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.send_command(AudioCommand::SetVolume(vol)).await;
            });
        }
    });

    app.on_select_connect_device(move |device_id| {
        info!("[UI ACTION] Playback device selected: {}", device_id);
    });

    app.on_search_query(move |query| {
        info!("[UI ACTION] Search query entered: '{}'", query);
    });

    app.on_play_track({
        let engine = audio_sender.clone();
        move |track_id| {
            info!("[UI ACTION] Play track clicked: {}", track_id);
            let engine = engine.clone();
            tokio::spawn(async move {
                let spotify_uri = format!("spotify:track:{}", track_id);
                engine.send_command(AudioCommand::LoadUri(spotify_uri)).await;
            });
        }
    });

    // Handle playlist selection and fetch tracks
    app.on_select_playlist({
        let api_client_lock = Arc::clone(&active_api_client);
        let app_weak = app_weak.clone();
        move |playlist_id| {
            info!("[UI ACTION] Playlist selected: {}", playlist_id);
            let api_client_lock = Arc::clone(&api_client_lock);
            let app_weak = app_weak.clone();
            let p_id = playlist_id.to_string();

            tokio::spawn(async move {
                let maybe_client = {
                    let guard = api_client_lock.lock().await;
                    guard.as_ref().map(|c| c.clone())
                };

                if let Some(client) = maybe_client {
                    match client.get_playlist_tracks(&p_id).await {
                        Ok(tracks) => {
                            let slint_tracks: Vec<TrackItem> = tracks.into_iter().map(|t| {
                                TrackItem {
                                    id: t.id.into(),
                                    index: t.index,
                                    title: t.title.into(),
                                    artist: t.artist.into(),
                                    album: t.album.into(),
                                    duration_str: t.duration_str.into(),
                                    play_count_str: "".into(),
                                    is_liked: false,
                                    is_playing: false,
                                }
                            }).collect();

                            let _ = app_weak.upgrade_in_event_loop(move |ui| {
                                ui.set_current_tracks(ModelRc::new(VecModel::from(slint_tracks)));
                            });
                        }
                        Err(e) => {
                            error!("[API ERROR] Failed to fetch playlist tracks for '{}': {}", p_id, e);
                        }
                    }
                } else {
                    error!("[API ERROR] Cannot fetch playlist tracks: No active authenticated API client!");
                }
            });
        }
    });

    // Auth Callback: Email submitted (pre-flight check)
    app.on_email_submitted(move |email| {
        info!("[UI ACTION] Email entered in login dialog: {}", email);
    });

    // Auth Callback: Direct Password Login
    app.on_login_requested({
        let app_weak = app_weak.clone();
        let api_client_lock = Arc::clone(&active_api_client);
        move |email, password| {
            let app_weak = app_weak.clone();
            let api_client_lock = Arc::clone(&api_client_lock);
            let email_str = email.to_string();
            let pass_str = password.to_string();

            tokio::spawn(async move {
                info!("[AUTH] Login requested for '{}'", email_str);

                match connect_with_password(&email_str, &pass_str).await {
                    Ok(session) => {
                        info!("[AUTH] Authentication successful for '{}'", email_str);

                        // Save credentials locally for auto-login
                        if let Err(e) = save_credentials(&SavedCredentials {
                            username: email_str.clone(),
                            password: Some(pass_str),
                            token: None,
                            refresh_token: None,
                            client_id: None,
                        }) {
                            error!("[CONFIG ERROR] Could not save credentials to disk: {}", e);
                        }

                        // Request Spotify Web API token
                        let scopes = "user-read-private,playlist-read-private";
                        let mut display_name = email_str.clone();

                        match session.token_provider().get_token(scopes).await {
                            Ok(token) => {
                                let client = SpotifyApiClient::new(token.access_token);
                                match client.get_current_user().await {
                                    Ok(name) => { display_name = name; }
                                    Err(e) => { error!("[API ERROR] Failed to fetch user profile: {}", e); }
                                }

                                match client.get_playlists().await {
                                    Ok(playlists) => {
                                        let slint_playlists: Vec<PlaylistItem> = playlists.into_iter().map(|p| {
                                            PlaylistItem {
                                                id: p.id.into(),
                                                title: p.title.into(),
                                                subtitle: format!("Плейлист • {} треков", p.track_count).into(),
                                                is_pinned: false,
                                                is_active: false,
                                            }
                                        }).collect();

                                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                                            ui.set_user_playlists(ModelRc::new(VecModel::from(slint_playlists)));
                                        });
                                    }
                                    Err(e) => {
                                        error!("[API ERROR] Failed to fetch playlists: {}", e);
                                    }
                                }

                                let mut guard = api_client_lock.lock().await;
                                *guard = Some(client);
                            }
                            Err(e) => {
                                error!("[AUTH ERROR] Failed to get Spotify Web API access token: {}", e);
                            }
                        }

                        let disp = display_name;
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_auth_loading(false);
                            ui.set_auth_error("".into());
                            ui.set_is_authenticated(true);
                            ui.set_is_login_open(false);
                            ui.set_user_display_name(disp.into());
                        });
                    }
                    Err(err) => {
                        error!("[AUTH ERROR] Login failed for '{}': {}", email_str, err);
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_auth_loading(false);
                            ui.set_auth_error(err.into());
                        });
                    }
                }
            });
        }
    });

    // Auth Callback: OTP Email Code
    app.on_otp_submitted({
        let app_weak = app_weak.clone();
        move |email, code| {
            warn!("[AUTH OTP] OTP code submitted for '{}': {}", email, code);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_auth_loading(false);
                    ui.set_auth_error("Для прямого входа используйте пароль или кнопку 'Войти через браузер'".into());
                });
            });
        }
    });

    // Auth Callback: Browser Login (OAuth / PKCE Loopback)
    app.on_browser_login_requested({
        let app_weak = app_weak.clone();
        let api_client_lock = Arc::clone(&active_api_client);
        move || {
            let app_weak = app_weak.clone();
            let api_client_lock = Arc::clone(&api_client_lock);
            tokio::spawn(async move {
                info!("[AUTH] Browser OAuth flow initiated by user");

                match api::login_with_oauth().await {
                    Ok((session, access_token)) => {
                        info!("[AUTH SUCCESS] Browser OAuth succeeded!");

                        let username = session.username().to_string();
                        let client = SpotifyApiClient::new(access_token);
                        {
                            let mut guard = api_client_lock.lock().await;
                            *guard = Some(client.clone());
                        }

                        // Immediately mark user authenticated and close login dialog
                        let u_name = username.clone();
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_auth_loading(false);
                            ui.set_auth_error("".into());
                            ui.set_is_authenticated(true);
                            ui.set_is_login_open(false);
                            ui.set_user_display_name(u_name.into());
                        });

                        // Concurrently fetch profile name
                        let app_weak_profile = app_weak.clone();
                        let client_profile = client.clone();
                        tokio::spawn(async move {
                            if let Ok(name) = client_profile.get_current_user().await {
                                let _ = app_weak_profile.upgrade_in_event_loop(move |ui| {
                                    ui.set_user_display_name(name.into());
                                });
                            }
                        });

                        // Concurrently fetch user playlists
                        let app_weak_playlists = app_weak.clone();
                        tokio::spawn(async move {
                            match client.get_playlists().await {
                                Ok(playlists) => {
                                    let slint_playlists: Vec<PlaylistItem> = playlists.into_iter().map(|p| {
                                        PlaylistItem {
                                            id: p.id.into(),
                                            title: p.title.into(),
                                            subtitle: format!("Плейлист • {} треков", p.track_count).into(),
                                            is_pinned: false,
                                            is_active: false,
                                        }
                                    }).collect();

                                    let _ = app_weak_playlists.upgrade_in_event_loop(move |ui| {
                                        ui.set_user_playlists(ModelRc::new(VecModel::from(slint_playlists)));
                                    });
                                }
                                Err(e) => {
                                    error!("[API ERROR] Failed to fetch playlists: {}", e);
                                }
                            }
                        });
                    }
                    Err(err) => {
                        error!("[AUTH ERROR] Browser OAuth failed: {}", err);
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_auth_loading(false);
                            ui.set_auth_error(err.into());
                        });
                    }
                }
            });
        }
    });

    // Speaker Discovery Sync Loop (updates device list in UI)
    {
        let disc = Arc::clone(&speaker_discovery);
        let app_weak = app_weak.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let speakers = disc.get_speakers();
                let slint_devices: Vec<DeviceItem> = speakers.into_iter().map(|s| {
                    DeviceItem {
                        id: s.id.into(),
                        name: s.name.into(),
                        device_type: if s.service_type.contains("cast") { "speaker".into() } else { "computer".into() },
                        is_active: false,
                    }
                }).collect();

                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_discovered_devices(ModelRc::new(VecModel::from(slint_devices)));
                });
            }
        });
    }

    // Startup Authentication Check: Auto-login from saved credentials
    {
        let app_weak = app_weak.clone();
        let api_client_lock = Arc::clone(&active_api_client);
        tokio::spawn(async move {
            if let Some(mut saved) = load_saved_credentials() {
                info!("[STARTUP] Found saved credentials for '{}'. Attempting auto-login...", saved.username);

                let auth_result: Result<(Session, String), String> = if let Some(pwd) = &saved.password {
                    match connect_with_password(&saved.username, pwd).await {
                        Ok(session) => {
                            let scopes = "user-read-private,playlist-read-private";
                            match session.token_provider().get_token(scopes).await {
                                Ok(token) => Ok((session, token.access_token)),
                                Err(e) => {
                                    error!("[STARTUP ERROR] Password session token provider failed: {}", e);
                                    Err(format!("Ошибка получения токена: {}", e))
                                }
                            }
                        }
                        Err(e) => Err(e),
                    }
                } else if let Some(tok) = &saved.token {
                    match connect_with_token(tok).await {
                        Ok(session) => Ok((session, tok.clone())),
                        Err(e) => {
                            warn!("[STARTUP] Token session connection failed: {}. Checking if refresh_token is available...", e);
                            if let (Some(rt), Some(cid)) = (&saved.refresh_token, &saved.client_id) {
                                match api::refresh_access_token(cid, rt).await {
                                    Ok((new_tok, maybe_new_rt)) => {
                                        saved.token = Some(new_tok.clone());
                                        if let Some(nrt) = maybe_new_rt {
                                            saved.refresh_token = Some(nrt);
                                        }
                                        let _ = save_credentials(&saved);
                                        match connect_with_token(&new_tok).await {
                                            Ok(session) => Ok((session, new_tok)),
                                            Err(e2) => Err(format!("Не удалось подключиться с новым токеном: {}", e2)),
                                        }
                                    }
                                    Err(re) => Err(format!("Не удалось обновить токен: {}", re)),
                                }
                            } else {
                                Err(format!("Токен устарел, и refresh_token отсутствует: {}", e))
                            }
                        }
                    }
                } else {
                    Err("В сохраненных данных отсутствует пароль или токен".to_string())
                };

                match auth_result {
                    Ok((session, access_token)) => {
                        info!("[STARTUP SUCCESS] Auto-login succeeded for '{}'", saved.username);

                        let username = session.username().to_string();
                        let client = SpotifyApiClient::new(access_token);
                        {
                            let mut guard = api_client_lock.lock().await;
                            *guard = Some(client.clone());
                        }

                        // Immediately unlock UI and close login dialog
                        let u_name = username.clone();
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_is_authenticated(true);
                            ui.set_is_login_open(false);
                            ui.set_user_display_name(u_name.into());
                        });

                        // Concurrently fetch profile name
                        let app_weak_profile = app_weak.clone();
                        let client_profile = client.clone();
                        tokio::spawn(async move {
                            if let Ok(name) = client_profile.get_current_user().await {
                                let _ = app_weak_profile.upgrade_in_event_loop(move |ui| {
                                    ui.set_user_display_name(name.into());
                                });
                            }
                        });

                        // Concurrently fetch playlists
                        let app_weak_playlists = app_weak.clone();
                        tokio::spawn(async move {
                            match client.get_playlists().await {
                                Ok(playlists) => {
                                    let slint_playlists: Vec<PlaylistItem> = playlists.into_iter().map(|p| {
                                        PlaylistItem {
                                            id: p.id.into(),
                                            title: p.title.into(),
                                            subtitle: format!("Плейлист • {} треков", p.track_count).into(),
                                            is_pinned: false,
                                            is_active: false,
                                        }
                                    }).collect();

                                    let _ = app_weak_playlists.upgrade_in_event_loop(move |ui| {
                                        ui.set_user_playlists(ModelRc::new(VecModel::from(slint_playlists)));
                                    });
                                }
                                Err(e) => {
                                    error!("[API ERROR] Failed to fetch playlists during startup: {}", e);
                                }
                            }
                        });
                    }
                    Err(e) => {
                        error!("[STARTUP ERROR] Auto-login failed: {}. Prompting login dialog.", e);
                        if e.contains("revoked") || e.contains("Bad credentials") || e.contains("invalid") {
                            clear_saved_credentials();
                        }
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_is_authenticated(false);
                            ui.set_is_login_open(true);
                        });
                    }
                }
            } else {
                info!("[STARTUP] No saved credentials found. Prompting login dialog.");
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_is_authenticated(false);
                    ui.set_is_login_open(true);
                });
            }
        });
    }

    info!("Entering Slint UI event loop...");
    app.run()?;

    info!("SpotifyLight exited cleanly.");
    Ok(())
}
