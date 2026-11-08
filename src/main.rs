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
        move || {
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                info!("[AUTH] Launching browser OAuth authentication...");
                let client_id = "27929427018d4157a41285e656d0d218";
                let redirect_uri = "http://127.0.0.1:8888/callback";
                let auth_url = format!(
                    "https://accounts.spotify.com/authorize?client_id={}&response_type=token&redirect_uri={}&scope=user-read-private%20playlist-read-private%20user-read-playback-state%20user-modify-playback-state",
                    client_id, redirect_uri
                );

                match std::process::Command::new("xdg-open").arg(&auth_url).spawn() {
                    Ok(_) => {
                        info!("[AUTH] Successfully opened default browser with Spotify OAuth URL");
                    }
                    Err(e) => {
                        error!("[SYSTEM ERROR] Failed to run xdg-open: {}. Please open manually: {}", e, auth_url);
                    }
                }

                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_auth_loading(false);
                    ui.set_auth_error("Браузер открыт. Если не открылся, введите логин и пароль напрямую.".into());
                });
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
            if let Some(saved) = load_saved_credentials() {
                info!("[STARTUP] Found saved credentials for '{}'. Attempting auto-login...", saved.username);

                let res = if let Some(pwd) = &saved.password {
                    connect_with_password(&saved.username, pwd).await
                } else if let Some(tok) = &saved.token {
                    connect_with_token(tok).await
                } else {
                    Err("No saved password or token found".to_string())
                };

                match res {
                    Ok(session) => {
                        info!("[STARTUP SUCCESS] Auto-login succeeded for '{}'", saved.username);
                        let scopes = "user-read-private,playlist-read-private";
                        let mut display_name = saved.username.clone();

                        match session.token_provider().get_token(scopes).await {
                            Ok(token) => {
                                let client = SpotifyApiClient::new(token.access_token);
                                if let Ok(name) = client.get_current_user().await {
                                    display_name = name;
                                }

                                if let Ok(playlists) = client.get_playlists().await {
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

                                let mut guard = api_client_lock.lock().await;
                                *guard = Some(client);
                            }
                            Err(e) => {
                                error!("[STARTUP ERROR] Auto-login session failed to get Web API token: {}", e);
                            }
                        }

                        let disp = display_name;
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_is_authenticated(true);
                            ui.set_is_login_open(false);
                            ui.set_user_display_name(disp.into());
                        });
                    }
                    Err(e) => {
                        error!("[STARTUP ERROR] Auto-login failed: {}. Clearing saved session and prompting login.", e);
                        clear_saved_credentials();
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
