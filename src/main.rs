mod api;
mod audio;
mod discovery;
mod mpris;
mod tray;

slint::include_modules!();

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use audio::{AudioCommand, AudioEngine};
use discovery::SpeakerDiscovery;
use mpris::MprisService;
use librespot::core::session::Session;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use api::{
    clear_saved_credentials, connect_with_password, connect_with_token, load_saved_credentials,
    save_credentials, SavedCredentials, SpotifyApiClient,
};

fn parse_duration_to_secs(d_str: &str) -> f64 {
    let parts: Vec<&str> = d_str.split(':').collect();
    if parts.len() == 2 {
        let m: f64 = parts[0].parse().unwrap_or(3.0);
        let s: f64 = parts[1].parse().unwrap_or(30.0);
        m * 60.0 + s
    } else {
        210.0
    }
}

async fn populate_library(
    client: SpotifyApiClient,
    cached_playlists: Arc<Mutex<Vec<PlaylistItem>>>,
    cached_albums: Arc<Mutex<Vec<PlaylistItem>>>,
    cached_artists: Arc<Mutex<Vec<PlaylistItem>>>,
    app_weak: slint::Weak<AppWindow>,
) {
    info!("[LIBRARY] Concurrently loading playlists, saved albums, and followed artists...");

    let client_p = client.clone();
    let p_handle = tokio::spawn(async move { client_p.get_playlists().await });

    let client_alb = client.clone();
    let alb_handle = tokio::spawn(async move { client_alb.get_saved_albums().await });

    let client_art = client.clone();
    let art_handle = tokio::spawn(async move { client_art.get_followed_artists().await });

    let (p_res, alb_res, art_res) = tokio::join!(p_handle, alb_handle, art_handle);

    let mut new_p = Vec::new();
    if let Ok(Ok(playlists)) = p_res {
        new_p = playlists.into_iter().map(|p| {
            let item_type = if p.title.contains("Любимые") || p.title.contains("Liked") {
                3
            } else if p.title.contains("радио") || p.title.contains("Radio") || p.title.contains("Killshot") {
                4
            } else if p.title.contains("Альбом") {
                1
            } else {
                0
            };
            PlaylistItem {
                id: p.id.into(),
                title: p.title.into(),
                subtitle: p.subtitle.into(),
                is_pinned: item_type == 3,
                is_active: false,
                item_type,
                image_url: "".into(),
            }
        }).collect();
    }

    let mut new_alb = Vec::new();
    if let Ok(Ok(albums)) = alb_res {
        new_alb = albums.into_iter().map(|a| {
            PlaylistItem {
                id: a.id.into(),
                title: a.title.into(),
                subtitle: a.subtitle.into(),
                is_pinned: false,
                is_active: false,
                item_type: 1,
                image_url: "".into(),
            }
        }).collect();
    }

    let mut new_art = Vec::new();
    if let Ok(Ok(artists)) = art_res {
        new_art = artists.into_iter().map(|art| {
            PlaylistItem {
                id: art.id.into(),
                title: art.title.into(),
                subtitle: art.subtitle.into(),
                is_pinned: false,
                is_active: false,
                item_type: 2,
                image_url: "".into(),
            }
        }).collect();
    }

    info!("[LIBRARY SUCCESS] Loaded {} playlists, {} albums, {} artists", new_p.len(), new_alb.len(), new_art.len());

    // Update caches
    {
        let mut guard = cached_playlists.lock().await;
        *guard = new_p.clone();
    }
    {
        let mut guard = cached_albums.lock().await;
        *guard = new_alb.clone();
    }
    {
        let mut guard = cached_artists.lock().await;
        *guard = new_art.clone();
    }

    // Default "Все" view: playlists + albums + artists
    let mut all = Vec::new();
    all.extend(new_p);
    all.extend(new_alb);
    all.extend(new_art);

    let _ = app_weak.upgrade_in_event_loop(move |ui| {
        ui.set_user_playlists(ModelRc::new(VecModel::from(all)));
    });
}

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

    // Spawn Desktop System Tray (StatusNotifierItem on D-Bus)
    let tray_weak = app_weak.clone();
    tokio::spawn(async move {
        if let Err(e) = tray::start_tray(tray_weak).await {
            warn!("[TRAY] Note: could not register tray service: {}", e);
        }
    });

    // Shared Spotify Web API client state
    let active_api_client: Arc<Mutex<Option<SpotifyApiClient>>> = Arc::new(Mutex::new(None));

    // Shared library collections for instant filtering (Все, Плейлисты, Альбомы, Артисты)
    let cached_playlists: Arc<Mutex<Vec<PlaylistItem>>> = Arc::new(Mutex::new(Vec::new()));
    let cached_albums: Arc<Mutex<Vec<PlaylistItem>>> = Arc::new(Mutex::new(Vec::new()));
    let cached_artists: Arc<Mutex<Vec<PlaylistItem>>> = Arc::new(Mutex::new(Vec::new()));

    // Shared playback tracking state
    let playback_duration: Arc<Mutex<f64>> = Arc::new(Mutex::new(210.0));
    let playback_elapsed: Arc<Mutex<f64>> = Arc::new(Mutex::new(0.0));
    let playback_is_active: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let active_track_idx: Arc<Mutex<i32>> = Arc::new(Mutex::new(-1));

    // UI Callbacks: Audio
    app.on_play_pause({
        let engine = audio_sender.clone();
        let app_weak = app_weak.clone();
        let p_act = Arc::clone(&playback_is_active);
        move || {
            info!("[UI ACTION] Play/Pause clicked");
            let engine = engine.clone();
            let app_weak = app_weak.clone();
            let p_act = Arc::clone(&p_act);
            tokio::spawn(async move {
                engine.send_command(AudioCommand::Toggle).await;
                let playing = engine.is_playing();
                p_act.store(playing, Ordering::Relaxed);
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_is_playing(playing);
                });
            });
        }
    });

    app.on_next_track({
        let app_weak = app_weak.clone();
        let a_idx = Arc::clone(&active_track_idx);
        let engine = audio_sender.clone();
        let p_dur = Arc::clone(&playback_duration);
        let p_el = Arc::clone(&playback_elapsed);
        let p_act = Arc::clone(&playback_is_active);
        move || {
            info!("[UI ACTION] Next track clicked");
            let app_weak = app_weak.clone();
            let a_idx = Arc::clone(&a_idx);
            let engine = engine.clone();
            let p_dur = Arc::clone(&p_dur);
            let p_el = Arc::clone(&p_el);
            let p_act = Arc::clone(&p_act);

            tokio::spawn(async move {
                let current_i = *a_idx.lock().await;
                let next_i = current_i + 1;
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    let tracks = ui.get_current_tracks();
                    if next_i >= 0 && (next_i as usize) < tracks.row_count() {
                        if let Some(track) = tracks.row_data(next_i as usize) {
                            let tid = track.id.to_string();
                            let eng = engine.clone();
                            let dur_clone = p_dur.clone();
                            let el_clone = p_el.clone();
                            let act_clone = p_act.clone();
                            let idx_clone = a_idx.clone();
                            tokio::spawn(async move {
                                eng.send_command(AudioCommand::LoadUri(format!("spotify:track:{}", tid))).await;
                                act_clone.store(true, Ordering::Relaxed);
                                *el_clone.lock().await = 0.0;
                                *idx_clone.lock().await = next_i;
                            });

                            ui.set_is_playing(true);
                            ui.set_active_track_title(track.title.clone());
                            ui.set_active_track_artist(track.artist.clone());
                            ui.set_current_time("0:00".into());
                            ui.set_track_progress(0.0);
                            if !track.duration_str.is_empty() {
                                ui.set_total_time(track.duration_str.clone());
                                let d_secs = parse_duration_to_secs(&track.duration_str);
                                tokio::spawn(async move { *dur_clone.lock().await = d_secs; });
                            }
                        }
                    }
                });
            });
        }
    });

    app.on_prev_track({
        let app_weak = app_weak.clone();
        let a_idx = Arc::clone(&active_track_idx);
        let engine = audio_sender.clone();
        let p_dur = Arc::clone(&playback_duration);
        let p_el = Arc::clone(&playback_elapsed);
        let p_act = Arc::clone(&playback_is_active);
        move || {
            info!("[UI ACTION] Previous track clicked");
            let app_weak = app_weak.clone();
            let a_idx = Arc::clone(&a_idx);
            let engine = engine.clone();
            let p_dur = Arc::clone(&p_dur);
            let p_el = Arc::clone(&p_el);
            let p_act = Arc::clone(&p_act);

            tokio::spawn(async move {
                let current_i = *a_idx.lock().await;
                let prev_i = if current_i > 0 { current_i - 1 } else { 0 };
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    let tracks = ui.get_current_tracks();
                    if prev_i >= 0 && (prev_i as usize) < tracks.row_count() {
                        if let Some(track) = tracks.row_data(prev_i as usize) {
                            let tid = track.id.to_string();
                            let eng = engine.clone();
                            let dur_clone = p_dur.clone();
                            let el_clone = p_el.clone();
                            let act_clone = p_act.clone();
                            let idx_clone = a_idx.clone();
                            tokio::spawn(async move {
                                eng.send_command(AudioCommand::LoadUri(format!("spotify:track:{}", tid))).await;
                                act_clone.store(true, Ordering::Relaxed);
                                *el_clone.lock().await = 0.0;
                                *idx_clone.lock().await = prev_i;
                            });

                            ui.set_is_playing(true);
                            ui.set_active_track_title(track.title.clone());
                            ui.set_active_track_artist(track.artist.clone());
                            ui.set_current_time("0:00".into());
                            ui.set_track_progress(0.0);
                            if !track.duration_str.is_empty() {
                                ui.set_total_time(track.duration_str.clone());
                                let d_secs = parse_duration_to_secs(&track.duration_str);
                                tokio::spawn(async move { *dur_clone.lock().await = d_secs; });
                            }
                        }
                    }
                });
            });
        }
    });

    app.on_seek_position({
        let engine = audio_sender.clone();
        let p_dur = Arc::clone(&playback_duration);
        let p_el = Arc::clone(&playback_elapsed);
        move |pos| {
            info!("[UI ACTION] Seek position requested: {:.2}", pos);
            let engine = engine.clone();
            let p_dur = Arc::clone(&p_dur);
            let p_el = Arc::clone(&p_el);
            tokio::spawn(async move {
                let dur = *p_dur.lock().await;
                let seek_secs = (pos as f64) * dur;
                {
                    let mut el = p_el.lock().await;
                    *el = seek_secs;
                }
                engine.send_command(AudioCommand::Seek(seek_secs as f32)).await;
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

    app.on_search_query({
        let cached_p = Arc::clone(&cached_playlists);
        let cached_alb = Arc::clone(&cached_albums);
        let cached_art = Arc::clone(&cached_artists);
        let app_weak = app_weak.clone();
        move |query| {
            let q = query.to_string().trim().to_lowercase();
            info!("[UI ACTION] Live search query: '{}'", q);
            let cached_p = Arc::clone(&cached_p);
            let cached_alb = Arc::clone(&cached_alb);
            let cached_art = Arc::clone(&cached_art);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let mut all = Vec::new();
                {
                    let p = cached_p.lock().await;
                    all.extend(p.clone());
                }
                {
                    let alb = cached_alb.lock().await;
                    all.extend(alb.clone());
                }
                {
                    let art = cached_art.lock().await;
                    all.extend(art.clone());
                }

                let filtered: Vec<PlaylistItem> = if q.is_empty() {
                    all
                } else {
                    all.into_iter().filter(|item| {
                        item.title.to_lowercase().contains(&q) || item.subtitle.to_lowercase().contains(&q)
                    }).collect()
                };

                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_user_playlists(ModelRc::new(VecModel::from(filtered)));
                });
            });
        }
    });

    // Context Menu Action: Add to Queue
    app.on_add_to_queue({
        let app_weak = app_weak.clone();
        let engine = audio_sender.clone();
        move |track_id| {
            info!("[UI ACTION] Add track to queue: {}", track_id);
            let tid = track_id.to_string();
            let app_weak = app_weak.clone();
            let engine = engine.clone();
            tokio::spawn(async move {
                // If nothing currently playing, play immediately
                if !engine.is_playing() {
                    engine.send_command(AudioCommand::LoadUri(format!("spotify:track:{}", tid))).await;
                    let _ = app_weak.upgrade_in_event_loop(move |ui| {
                        ui.set_is_playing(true);
                    });
                }
            });
        }
    });

    // Context Menu Action: Toggle Like
    app.on_toggle_like_track({
        let app_weak = app_weak.clone();
        move |track_id| {
            info!("[UI ACTION] Toggle like for track: {}", track_id);
            let tid = track_id.to_string();
            let _ = app_weak.upgrade_in_event_loop(move |ui| {
                let tracks = ui.get_current_tracks();
                let mut updated = Vec::new();
                for i in 0..tracks.row_count() {
                    if let Some(mut t) = tracks.row_data(i) {
                        if t.id == tid {
                            t.is_liked = !t.is_liked;
                            ui.set_is_liked(t.is_liked);
                        }
                        updated.push(t);
                    }
                }
                ui.set_current_tracks(ModelRc::new(VecModel::from(updated)));
            });
        }
    });

    // Context Menu Action: Copy Link
    app.on_copy_to_clipboard({
        move |target_id| {
            let tid = target_id.to_string();
            info!("[UI ACTION] Copy link to clipboard for: {}", tid);
            tokio::spawn(async move {
                let link = if tid.starts_with("http") {
                    tid
                } else {
                    format!("https://open.spotify.com/track/{}", tid)
                };
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(link);
                }
            });
        }
    });

    // Context Menu Action: Delete Playlist
    app.on_delete_playlist({
        let cached_p = Arc::clone(&cached_playlists);
        let app_weak = app_weak.clone();
        move |pl_id| {
            let id = pl_id.to_string();
            info!("[UI ACTION] Delete playlist requested: {}", id);
            let cached_p = Arc::clone(&cached_p);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                {
                    let mut guard = cached_p.lock().await;
                    guard.retain(|x| x.id != id);
                }
                let updated = {
                    let guard = cached_p.lock().await;
                    guard.clone()
                };
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_user_playlists(ModelRc::new(VecModel::from(updated)));
                });
            });
        }
    });

    // Sidebar Action: Create New Playlist
    app.on_create_new_playlist({
        let cached_p = Arc::clone(&cached_playlists);
        let app_weak = app_weak.clone();
        move || {
            info!("[UI ACTION] Create new playlist requested");
            let cached_p = Arc::clone(&cached_p);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let new_item = {
                    let mut guard = cached_p.lock().await;
                    let count = guard.len() + 1;
                    let item = PlaylistItem {
                        id: format!("custom_{}", count).into(),
                        title: format!("Мой плейлист #{}", count).into(),
                        subtitle: "Плейлист • Пользовательский".into(),
                        is_pinned: false,
                        is_active: false,
                        item_type: 0,
                        image_url: "".into(),
                    };
                    guard.insert(0, item.clone());
                    item
                };
                let all = {
                    let guard = cached_p.lock().await;
                    guard.clone()
                };
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_user_playlists(ModelRc::new(VecModel::from(all)));
                    ui.set_current_workspace(1);
                    ui.set_current_album_title(new_item.title);
                    ui.set_current_tracks(ModelRc::new(VecModel::from(Vec::<TrackItem>::new())));
                });
            });
        }
    });

    app.on_filter_library({
        let cached_p = Arc::clone(&cached_playlists);
        let cached_alb = Arc::clone(&cached_albums);
        let cached_art = Arc::clone(&cached_artists);
        let app_weak = app_weak.clone();
        move |filter_idx| {
            info!("[UI ACTION] Library filter selected: {}", filter_idx);
            let cached_p = Arc::clone(&cached_p);
            let cached_alb = Arc::clone(&cached_alb);
            let cached_art = Arc::clone(&cached_art);
            let app_weak = app_weak.clone();

            tokio::spawn(async move {
                let to_show = match filter_idx {
                    1 => {
                        // Плейлисты
                        let p = cached_p.lock().await;
                        p.clone()
                    }
                    2 => {
                        // Альбомы
                        let alb = cached_alb.lock().await;
                        alb.clone()
                    }
                    3 => {
                        // Артисты
                        let art = cached_art.lock().await;
                        art.clone()
                    }
                    _ => {
                        // Все (плейлисты + альбомы + артисты)
                        let mut all = Vec::new();
                        {
                            let p = cached_p.lock().await;
                            all.extend(p.clone());
                        }
                        {
                            let alb = cached_alb.lock().await;
                            all.extend(alb.clone());
                        }
                        {
                            let art = cached_art.lock().await;
                            all.extend(art.clone());
                        }
                        all
                    }
                };

                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_user_playlists(ModelRc::new(VecModel::from(to_show)));
                });
            });
        }
    });

    app.on_play_track({
        let engine = audio_sender.clone();
        let app_weak = app_weak.clone();
        let p_dur = Arc::clone(&playback_duration);
        let p_el = Arc::clone(&playback_elapsed);
        let p_act = Arc::clone(&playback_is_active);
        let a_idx = Arc::clone(&active_track_idx);
        move |track_id| {
            info!("[UI ACTION] Play track clicked: {}", track_id);
            let engine = engine.clone();
            let app_weak = app_weak.clone();
            let tid = track_id.to_string();
            let p_dur = Arc::clone(&p_dur);
            let p_el = Arc::clone(&p_el);
            let p_act = Arc::clone(&p_act);
            let a_idx = Arc::clone(&a_idx);

            tokio::spawn(async move {
                let spotify_uri = format!("spotify:track:{}", tid);
                engine.send_command(AudioCommand::LoadUri(spotify_uri)).await;
                p_act.store(true, Ordering::Relaxed);
                {
                    let mut el = p_el.lock().await;
                    *el = 0.0;
                }

                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_is_playing(true);
                    ui.set_current_time("0:00".into());
                    ui.set_track_progress(0.0);
                    let tracks_model = ui.get_current_tracks();
                    let mut found_idx = -1;
                    let mut updated_tracks = Vec::new();
                    let mut dur_secs = 210.0;

                    for idx in 0..tracks_model.row_count() {
                        if let Some(mut track) = tracks_model.row_data(idx) {
                            if track.id == tid {
                                track.is_playing = true;
                                found_idx = idx as i32;
                                ui.set_active_track_title(track.title.clone());
                                ui.set_active_track_artist(track.artist.clone());
                                if !track.duration_str.is_empty() {
                                    ui.set_total_time(track.duration_str.clone());
                                    dur_secs = parse_duration_to_secs(&track.duration_str);
                                }
                            } else {
                                track.is_playing = false;
                            }
                            updated_tracks.push(track);
                        }
                    }

                    if found_idx >= 0 {
                        ui.set_current_tracks(ModelRc::new(VecModel::from(updated_tracks)));
                    }

                    let a_idx_inner = Arc::clone(&a_idx);
                    let p_dur_inner = Arc::clone(&p_dur);
                    tokio::spawn(async move {
                        let mut g_idx = a_idx_inner.lock().await;
                        *g_idx = found_idx;
                        let mut g_dur = p_dur_inner.lock().await;
                        *g_dur = dur_secs;
                    });
                });
            });
        }
    });

    // Handle library item selection (Album, Artist, or Playlist) and fetch tracks
    app.on_select_playlist({
        let api_client_lock = Arc::clone(&active_api_client);
        let cached_p = Arc::clone(&cached_playlists);
        let cached_alb = Arc::clone(&cached_albums);
        let cached_art = Arc::clone(&cached_artists);
        let app_weak = app_weak.clone();
        move |item_id| {
            info!("[UI ACTION] Library item selected: {}", item_id);
            let api_client_lock = Arc::clone(&api_client_lock);
            let cached_p = Arc::clone(&cached_p);
            let cached_alb = Arc::clone(&cached_alb);
            let cached_art = Arc::clone(&cached_art);
            let app_weak = app_weak.clone();
            let id = item_id.to_string();

            tokio::spawn(async move {
                let maybe_client = {
                    let guard = api_client_lock.lock().await;
                    guard.as_ref().map(|c| c.clone())
                };

                if let Some(client) = maybe_client {
                    let mut found_title = format!("Медиатека {}", id);
                    let mut item_type = 0; // 0=playlist, 1=album, 2=artist

                    {
                        let p = cached_p.lock().await;
                        if let Some(item) = p.iter().find(|x| x.id == id) {
                            found_title = item.title.to_string();
                            item_type = item.item_type;
                        }
                    }
                    if item_type == 0 && !found_title.contains("Медиатека") {
                        // Already resolved title
                    } else {
                        let alb = cached_alb.lock().await;
                        if let Some(item) = alb.iter().find(|x| x.id == id) {
                            found_title = item.title.to_string();
                            item_type = 1;
                        }
                    }
                    if item_type == 0 && found_title.starts_with("Медиатека") {
                        let art = cached_art.lock().await;
                        if let Some(item) = art.iter().find(|x| x.id == id) {
                            found_title = item.title.to_string();
                            item_type = 2;
                        }
                    }

                    let tracks_res = match item_type {
                        1 => client.get_album_tracks(&id).await,
                        2 => client.get_artist_top_tracks(&id).await,
                        _ => client.get_playlist_tracks(&id).await,
                    };

                    match tracks_res {
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
                                    image_url: "".into(),
                                }
                            }).collect();

                            let title_copy = found_title.clone();
                            let _ = app_weak.upgrade_in_event_loop(move |ui| {
                                ui.set_current_workspace(1);
                                ui.set_current_album_title(title_copy.into());
                                ui.set_current_tracks(ModelRc::new(VecModel::from(slint_tracks)));
                            });
                        }
                        Err(e) => {
                            error!("[API ERROR] Failed to fetch tracks for item '{}': {}", id, e);
                        }
                    }
                } else {
                    error!("[API ERROR] Cannot fetch tracks: No active authenticated API client!");
                }
            });
        }
    });

    // Playback Progress & Autoplay Ticker
    {
        let app_weak = app_weak.clone();
        let p_dur = Arc::clone(&playback_duration);
        let p_el = Arc::clone(&playback_elapsed);
        let p_act = Arc::clone(&playback_is_active);
        let a_idx = Arc::clone(&active_track_idx);
        let engine = audio_sender.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                if p_act.load(Ordering::Relaxed) {
                    let mut el = p_el.lock().await;
                    *el += 0.25;
                    let elapsed = *el;
                    let dur = *p_dur.lock().await;

                    if elapsed >= dur && dur > 1.0 {
                        // Advance to next track (Autoplay)
                        *el = 0.0;
                        let app_w = app_weak.clone();
                        let a_idx_c = Arc::clone(&a_idx);
                        let eng_c = engine.clone();
                        let p_dur_c = Arc::clone(&p_dur);
                        let p_el_c = Arc::clone(&p_el);
                        let p_act_c = Arc::clone(&p_act);

                        tokio::spawn(async move {
                            let curr = *a_idx_c.lock().await;
                            let next_i = curr + 1;
                            let _ = app_w.upgrade_in_event_loop(move |ui| {
                                let tracks = ui.get_current_tracks();
                                if next_i >= 0 && (next_i as usize) < tracks.row_count() {
                                    if let Some(track) = tracks.row_data(next_i as usize) {
                                        let tid = track.id.to_string();
                                        let eng = eng_c.clone();
                                        let dur_clone = p_dur_c.clone();
                                        let el_clone = p_el_c.clone();
                                        let act_clone = p_act_c.clone();
                                        let idx_clone = a_idx_c.clone();
                                        tokio::spawn(async move {
                                            eng.send_command(AudioCommand::LoadUri(format!("spotify:track:{}", tid))).await;
                                            act_clone.store(true, Ordering::Relaxed);
                                            *el_clone.lock().await = 0.0;
                                            *idx_clone.lock().await = next_i;
                                        });

                                        ui.set_is_playing(true);
                                        ui.set_active_track_title(track.title.clone());
                                        ui.set_active_track_artist(track.artist.clone());
                                        ui.set_current_time("0:00".into());
                                        ui.set_track_progress(0.0);
                                        if !track.duration_str.is_empty() {
                                            ui.set_total_time(track.duration_str.clone());
                                            let d_secs = parse_duration_to_secs(&track.duration_str);
                                            tokio::spawn(async move { *dur_clone.lock().await = d_secs; });
                                        }
                                    }
                                } else {
                                    // End of playlist
                                    p_act_c.store(false, Ordering::Relaxed);
                                    ui.set_is_playing(false);
                                }
                            });
                        });
                    } else {
                        let prog = (elapsed / dur).clamp(0.0, 1.0) as f32;
                        let sec_u = elapsed as u64;
                        let time_str = format!("{}:{:02}", sec_u / 60, sec_u % 60);

                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_track_progress(prog);
                            ui.set_current_time(time_str.into());
                        });
                    }
                }
            }
        });
    }

    // Auth Callback: Logout Requested
    app.on_logout_requested({
        let app_weak = app_weak.clone();
        let api_client_lock = Arc::clone(&active_api_client);
        let engine = audio_sender.clone();
        move || {
            info!("[UI ACTION] Logout requested by user. Resetting session and credentials.");
            clear_saved_credentials();
            let api_client_lock = Arc::clone(&api_client_lock);
            let engine = engine.clone();
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                {
                    let mut guard = api_client_lock.lock().await;
                    *guard = None;
                }
                engine.send_command(AudioCommand::Pause).await;
                let _ = app_weak.upgrade_in_event_loop(|ui| {
                    ui.set_is_authenticated(false);
                    ui.set_user_display_name("".into());
                    ui.set_user_initial("L".into());
                    ui.set_current_workspace(0);
                    ui.set_is_login_open(true);
                    ui.set_user_playlists(ModelRc::new(VecModel::from(vec![])));
                    ui.set_current_tracks(ModelRc::new(VecModel::from(vec![])));
                });
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
        let cached_p = Arc::clone(&cached_playlists);
        let cached_alb = Arc::clone(&cached_albums);
        let cached_art = Arc::clone(&cached_artists);
        let engine = audio_sender.clone();
        move |email, password| {
            let app_weak = app_weak.clone();
            let api_client_lock = Arc::clone(&api_client_lock);
            let cached_p = Arc::clone(&cached_p);
            let cached_alb = Arc::clone(&cached_alb);
            let cached_art = Arc::clone(&cached_art);
            let engine = engine.clone();
            let email_str = email.to_string();
            let pass_str = password.to_string();

            tokio::spawn(async move {
                info!("[AUTH] Login requested for '{}'", email_str);

                match connect_with_password(&email_str, &pass_str).await {
                    Ok(session) => {
                        info!("[AUTH] Authentication successful for '{}'", email_str);

                        // Connect playback engine
                        engine.init_session(session.clone()).await;

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
                                let client = SpotifyApiClient::new(token.access_token, email_str.clone());
                                match client.get_current_user().await {
                                    Ok(name) => { display_name = name; }
                                    Err(e) => { error!("[API ERROR] Failed to fetch user profile: {}", e); }
                                }

                                populate_library(
                                    client.clone(),
                                    cached_p,
                                    cached_alb,
                                    cached_art,
                                    app_weak.clone(),
                                ).await;

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
                            let init = disp.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "L".into());
                            ui.set_user_initial(init.into());
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
        let cached_p = Arc::clone(&cached_playlists);
        let cached_alb = Arc::clone(&cached_albums);
        let cached_art = Arc::clone(&cached_artists);
        let engine = audio_sender.clone();
        move || {
            let app_weak = app_weak.clone();
            let api_client_lock = Arc::clone(&api_client_lock);
            let cached_p = Arc::clone(&cached_p);
            let cached_alb = Arc::clone(&cached_alb);
            let cached_art = Arc::clone(&cached_art);
            let engine = engine.clone();
            tokio::spawn(async move {
                info!("[AUTH] Browser OAuth flow initiated by user");

                match api::login_with_oauth().await {
                    Ok((session, access_token)) => {
                        info!("[AUTH SUCCESS] Browser OAuth succeeded!");

                        let username = session.username().to_string();
                        let client = SpotifyApiClient::new(access_token, username.clone());
                        {
                            let mut guard = api_client_lock.lock().await;
                            *guard = Some(client.clone());
                        }

                        // Connect playback engine
                        engine.init_session(session.clone()).await;

                        // Immediately mark user authenticated and close login dialog
                        let u_name = username.clone();
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_auth_loading(false);
                            ui.set_auth_error("".into());
                            ui.set_is_authenticated(true);
                            ui.set_is_login_open(false);
                            let init = u_name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "L".into());
                            ui.set_user_initial(init.into());
                            ui.set_user_display_name(u_name.into());
                        });

                        // Concurrently fetch profile name
                        let app_weak_profile = app_weak.clone();
                        let client_profile = client.clone();
                        tokio::spawn(async move {
                            if let Ok(name) = client_profile.get_current_user().await {
                                let _ = app_weak_profile.upgrade_in_event_loop(move |ui| {
                                    let init = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "L".into());
                                    ui.set_user_initial(init.into());
                                    ui.set_user_display_name(name.into());
                                });
                            }
                        });

                        // Concurrently fetch entire library (playlists, albums, artists)
                        let app_weak_library = app_weak.clone();
                        tokio::spawn(async move {
                            populate_library(
                                client,
                                cached_p,
                                cached_alb,
                                cached_art,
                                app_weak_library,
                            ).await;
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
        let engine = audio_sender.clone();
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

                        // Connect playback engine
                        engine.init_session(session.clone()).await;

                        let username = session.username().to_string();
                        let client = SpotifyApiClient::new(access_token, username.clone());
                        {
                            let mut guard = api_client_lock.lock().await;
                            *guard = Some(client.clone());
                        }

                        // Immediately unlock UI and close login dialog
                        let u_name = username.clone();
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_is_authenticated(true);
                            ui.set_is_login_open(false);
                            let init = u_name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "L".into());
                            ui.set_user_initial(init.into());
                            ui.set_user_display_name(u_name.into());
                        });

                        // Concurrently fetch profile name
                        let app_weak_profile = app_weak.clone();
                        let client_profile = client.clone();
                        tokio::spawn(async move {
                            if let Ok(name) = client_profile.get_current_user().await {
                                let _ = app_weak_profile.upgrade_in_event_loop(move |ui| {
                                    let init = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "L".into());
                                    ui.set_user_initial(init.into());
                                    ui.set_user_display_name(name.into());
                                });
                            }
                        });

                        // Concurrently fetch entire library (playlists, albums, artists)
                        let app_weak_library = app_weak.clone();
                        let cached_p_start = Arc::clone(&cached_playlists);
                        let cached_alb_start = Arc::clone(&cached_albums);
                        let cached_art_start = Arc::clone(&cached_artists);
                        tokio::spawn(async move {
                            populate_library(
                                client,
                                cached_p_start,
                                cached_alb_start,
                                cached_art_start,
                                app_weak_library,
                            ).await;
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
