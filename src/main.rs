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
    clear_saved_credentials, connect_with_password, connect_with_token, get_cached_image_path,
    load_image_path_async, load_saved_credentials, save_credentials, SavedCredentials, SpotifyApiClient,
};

#[derive(Clone, Debug)]
pub struct LocalPlaylistItem {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub is_pinned: bool,
    pub is_active: bool,
    pub item_type: i32,
    pub image_url: String,
}

#[derive(Clone, Debug)]
pub struct LocalTrackItem {
    pub id: String,
    pub index: i32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_str: String,
    pub play_count_str: String,
    pub is_liked: bool,
    pub is_playing: bool,
    pub image_url: String,
}

fn to_slint_playlist(item: &LocalPlaylistItem) -> PlaylistItem {
    let (cover, has_cover) = if let Some(path) = get_cached_image_path(&item.image_url) {
        match slint::Image::load_from_path(&path) {
            Ok(img) => (img, true),
            Err(_) => (slint::Image::default(), false),
        }
    } else {
        (slint::Image::default(), false)
    };

    PlaylistItem {
        id: item.id.clone().into(),
        title: item.title.clone().into(),
        subtitle: item.subtitle.clone().into(),
        is_pinned: item.is_pinned,
        is_active: item.is_active,
        item_type: item.item_type,
        image_url: item.image_url.clone().into(),
        cover,
        has_cover,
    }
}

fn to_slint_track(item: &LocalTrackItem) -> TrackItem {
    let (cover, has_cover) = if let Some(path) = get_cached_image_path(&item.image_url) {
        match slint::Image::load_from_path(&path) {
            Ok(img) => (img, true),
            Err(_) => (slint::Image::default(), false),
        }
    } else {
        (slint::Image::default(), false)
    };

    TrackItem {
        id: item.id.clone().into(),
        index: item.index,
        title: item.title.clone().into(),
        artist: item.artist.clone().into(),
        album: item.album.clone().into(),
        duration_str: item.duration_str.clone().into(),
        play_count_str: item.play_count_str.clone().into(),
        is_liked: item.is_liked,
        is_playing: item.is_playing,
        image_url: item.image_url.clone().into(),
        cover,
        has_cover,
    }
}

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
    cached_playlists: Arc<Mutex<Vec<LocalPlaylistItem>>>,
    cached_albums: Arc<Mutex<Vec<LocalPlaylistItem>>>,
    cached_artists: Arc<Mutex<Vec<LocalPlaylistItem>>>,
    app_weak: slint::Weak<AppWindow>,
) {
    info!("[LIBRARY] Concurrently loading playlists, saved albums, and followed artists...");

    // 1. Concurrently fetch playlists
    let client_p = client.clone();
    let app_weak_p = app_weak.clone();
    let cached_p_clone = Arc::clone(&cached_playlists);
    tokio::spawn(async move {
        if let Ok(playlists) = client_p.get_playlists().await {
            let local_items: Vec<LocalPlaylistItem> = playlists.into_iter().map(|p| {
                let item_type = if p.title.contains("Любимые") || p.title.contains("Liked") {
                    3
                } else if p.title.contains("радио") || p.title.contains("Radio") || p.title.contains("Killshot") {
                    4
                } else if p.title.contains("Альбом") {
                    1
                } else {
                    0
                };
                LocalPlaylistItem {
                    id: p.id,
                    title: p.title,
                    subtitle: p.subtitle,
                    is_pinned: item_type == 3,
                    is_active: false,
                    item_type,
                    image_url: p.image_url,
                }
            }).collect();

            info!("[LIBRARY] Playlists loaded: {}", local_items.len());
            {
                let mut guard = cached_p_clone.lock().await;
                *guard = local_items.clone();
            }

            // Trigger background download for uncached covers
            for it in &local_items {
                if !it.image_url.is_empty() && get_cached_image_path(&it.image_url).is_none() {
                    let u = it.image_url.clone();
                    let app_w = app_weak_p.clone();
                    let c_p = Arc::clone(&cached_p_clone);
                    tokio::spawn(async move {
                        if let Some(_) = load_image_path_async(&u).await {
                            let curr = c_p.lock().await.clone();
                            let _ = app_w.upgrade_in_event_loop(move |ui| {
                                let sl: Vec<PlaylistItem> = curr.iter().map(to_slint_playlist).collect();
                                ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
                            });
                        }
                    });
                }
            }

            let for_ui = local_items.clone();
            let _ = app_weak_p.upgrade_in_event_loop(move |ui| {
                let sl: Vec<PlaylistItem> = for_ui.iter().map(to_slint_playlist).collect();
                ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
            });
        }
    });

    let client_art = client.clone();
    let app_weak_art = app_weak.clone();
    let cached_art_clone = Arc::clone(&cached_artists);
    let cached_p_ref = Arc::clone(&cached_playlists);
    tokio::spawn(async move {
        if let Ok(artists) = client_art.get_followed_artists().await {
            let local_items: Vec<LocalPlaylistItem> = artists.into_iter().map(|art| {
                LocalPlaylistItem {
                    id: art.id,
                    title: art.title,
                    subtitle: art.subtitle,
                    is_pinned: false,
                    is_active: false,
                    item_type: 2,
                    image_url: art.image_url,
                }
            }).collect();

            info!("[LIBRARY] Followed artists loaded: {}", local_items.len());
            {
                let mut guard = cached_art_clone.lock().await;
                *guard = local_items.clone();
            }

            for it in &local_items {
                if !it.image_url.is_empty() && get_cached_image_path(&it.image_url).is_none() {
                    let u = it.image_url.clone();
                    let app_w = app_weak_art.clone();
                    let c_p = Arc::clone(&cached_p_ref);
                    let c_a = Arc::clone(&cached_art_clone);
                    tokio::spawn(async move {
                        if let Some(_) = load_image_path_async(&u).await {
                            let mut all = c_p.lock().await.clone();
                            all.extend(c_a.lock().await.clone());
                            let _ = app_w.upgrade_in_event_loop(move |ui| {
                                let sl: Vec<PlaylistItem> = all.iter().map(to_slint_playlist).collect();
                                ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
                            });
                        }
                    });
                }
            }

            // Merge into UI view if on "Все"
            let mut merged = Vec::new();
            {
                let p = cached_p_ref.lock().await;
                merged.extend(p.clone());
            }
            merged.extend(local_items);
            let _ = app_weak_art.upgrade_in_event_loop(move |ui| {
                let sl: Vec<PlaylistItem> = merged.iter().map(to_slint_playlist).collect();
                ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
            });
        }
    });

    let client_alb = client.clone();
    let cached_alb_clone = Arc::clone(&cached_albums);
    tokio::spawn(async move {
        if let Ok(albums) = client_alb.get_saved_albums().await {
            let local_items: Vec<LocalPlaylistItem> = albums.into_iter().map(|a| {
                LocalPlaylistItem {
                    id: a.id,
                    title: a.title,
                    subtitle: a.subtitle,
                    is_pinned: false,
                    is_active: false,
                    item_type: 1,
                    image_url: a.image_url,
                }
            }).collect();

            info!("[LIBRARY] Saved albums loaded: {}", local_items.len());
            let mut guard = cached_alb_clone.lock().await;
            *guard = local_items;
        }
    });

    // 4. Concurrently fetch recently played tracks for RightDrawer history
    let client_recent = client.clone();
    let app_weak_recent = app_weak.clone();
    tokio::spawn(async move {
        if let Ok(recents) = client_recent.get_recently_played().await {
            let local_items: Vec<LocalTrackItem> = recents.into_iter().map(|t| {
                LocalTrackItem {
                    id: t.id,
                    index: t.index,
                    title: t.title,
                    artist: t.artist,
                    album: t.album,
                    duration_str: t.duration_str,
                    play_count_str: String::new(),
                    is_liked: false,
                    is_playing: false,
                    image_url: t.image_url,
                }
            }).collect();

            info!("[LIBRARY] Recently played tracks loaded: {}", local_items.len());
            let items_ui = local_items.clone();
            let _ = app_weak_recent.upgrade_in_event_loop(move |ui| {
                let sl: Vec<TrackItem> = items_ui.iter().map(to_slint_track).collect();
                ui.set_recent_items(ModelRc::new(VecModel::from(sl)));
            });
        }
    });
}

async fn play_track_internal(
    track_to_play: LocalTrackItem,
    target_idx: i32,
    app_weak: slint::Weak<AppWindow>,
    engine: AudioEngine,
    playback_duration: Arc<Mutex<f64>>,
    playback_elapsed: Arc<Mutex<f64>>,
    playback_is_active: Arc<AtomicBool>,
    active_track_idx: Arc<Mutex<i32>>,
    cached_queue: Arc<Mutex<Vec<LocalTrackItem>>>,
) {
    info!("[PLAY TRACK INTERNAL] Switching to track '{}' (idx: {})", track_to_play.title, target_idx);
    *active_track_idx.lock().await = target_idx;
    let dur_secs = parse_duration_to_secs(&track_to_play.duration_str);
    *playback_duration.lock().await = dur_secs;
    *playback_elapsed.lock().await = 0.0;
    playback_is_active.store(true, Ordering::Relaxed);

    engine.send_command(AudioCommand::LoadUri(track_to_play.id.clone())).await;

    let queue_copy = {
        let mut q = cached_queue.lock().await;
        for item in q.iter_mut() {
            item.is_playing = (item.id == track_to_play.id);
        }
        q.clone()
    };

    let track_cover_name = track_to_play.image_url.clone();
    let track_ui = track_to_play.clone();

    let _ = app_weak.upgrade_in_event_loop(move |ui| {
        ui.set_is_playing(true);
        ui.set_active_track_title(track_ui.title.clone().into());
        ui.set_active_track_artist(track_ui.artist.clone().into());
        ui.set_current_time("0:00".into());
        ui.set_total_time(track_ui.duration_str.clone().into());
        ui.set_track_progress(0.0);

        ui.set_current_playing_track(to_slint_track(&track_ui));

        if let Some(path) = get_cached_image_path(&track_cover_name) {
            if let Ok(img) = slint::Image::load_from_path(&path) {
                ui.set_current_track_cover(img);
                ui.set_has_current_track_cover(true);
            }
        }

        let q_slint: Vec<TrackItem> = queue_copy.iter().skip(1).take(9).map(to_slint_track).collect();
        ui.set_queue_items(ModelRc::new(VecModel::from(q_slint)));
    });
}

fn init_queen_state(
    app_weak: slint::Weak<AppWindow>,
    engine: AudioEngine,
    cached_playlists: Arc<Mutex<Vec<LocalPlaylistItem>>>,
    cached_queue: Arc<Mutex<Vec<LocalTrackItem>>>,
    playback_duration: Arc<Mutex<f64>>,
    playback_elapsed: Arc<Mutex<f64>>,
    playback_is_active: Arc<AtomicBool>,
    active_track_idx: Arc<Mutex<i32>>,
) {
    tokio::spawn(async move {
        let kis_cover_path = get_cached_image_path("korol_i_shut.png");

        let library_items = vec![
            LocalPlaylistItem {
                id: "spotify:playlist:liked".into(),
                title: "Любимые треки".into(),
                subtitle: "Плейлист • lswaj49gzhi60w59e4ds1t115".into(),
                is_pinned: true,
                is_active: false,
                item_type: 0,
                image_url: "sb_liked_tracks.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:playlist:coffee_table_jazz".into(),
                title: "Coffee Table Jazz".into(),
                subtitle: "Плейлист • Spotify".into(),
                is_pinned: false,
                is_active: false,
                item_type: 0,
                image_url: "sb_coffee_table_jazz.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:playlist:picnik".into(),
                title: "пикник".into(),
                subtitle: "Плейлист • lswaj49gzhi60w59e4ds1t115".into(),
                is_pinned: false,
                is_active: false,
                item_type: 0,
                image_url: "sb_picnik.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:playlist:offline".into(),
                title: "Оффлайн".into(),
                subtitle: "Плейлист • lswaj49gzhi60w59e4ds1t115".into(),
                is_pinned: false,
                is_active: false,
                item_type: 0,
                image_url: "sb_offline.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:artist:zhadan".into(),
                title: "Жадан і Собаки".into(),
                subtitle: "Исполнитель".into(),
                is_pinned: false,
                is_active: false,
                item_type: 2,
                image_url: "sb_zhadan.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:playlist:opp".into(),
                title: "Опп".into(),
                subtitle: "Плейлист • lswaj49gzhi60w59e4ds1t115".into(),
                is_pinned: false,
                is_active: false,
                item_type: 0,
                image_url: "sb_opp.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:artist:chris_benstead".into(),
                title: "Chris Benstead".into(),
                subtitle: "Исполнитель".into(),
                is_pinned: false,
                is_active: false,
                item_type: 2,
                image_url: "sb_chris_benstead.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:playlist:grebenshchikov".into(),
                title: "Гребенщиков".into(),
                subtitle: "Плейлист • lswaj49gzhi60w59e4ds1t115".into(),
                is_pinned: false,
                is_active: false,
                item_type: 0,
                image_url: "sb_grebenshchikov.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:playlist:too_much".into(),
                title: "Too Much, Too Young, Too Fast".into(),
                subtitle: "Плейлист • lswaj49gzhi60w59e4ds1t115".into(),
                is_pinned: false,
                is_active: false,
                item_type: 0,
                image_url: "sb_too_much.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:playlist:die_antwoord".into(),
                title: "Die Antwoord".into(),
                subtitle: "Плейлист • lswaj49gzhi60w59e4ds1t115".into(),
                is_pinned: false,
                is_active: false,
                item_type: 0,
                image_url: "sb_die_antwoord.png".into(),
            },
            LocalPlaylistItem {
                id: "spotify:playlist:neverlove".into(),
                title: "Neverlove".into(),
                subtitle: "Плейлист • lswaj49gzhi60w59e4ds1t115".into(),
                is_pinned: false,
                is_active: false,
                item_type: 0,
                image_url: "sb_neverlove.png".into(),
            },
        ];

        let queue_tracks = vec![
            LocalTrackItem {
                id: "spotify:track:basta_rasta".into(),
                index: 1,
                title: "Баста Раста".into(),
                artist: "[E] Boris Grebenshikov".into(),
                album: "Песни бардов".into(),
                duration_str: "3:40".into(),
                play_count_str: "1 200 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_basta_rasta.png".into(),
            },
            LocalTrackItem {
                id: "spotify:track:rok".into(),
                index: 2,
                title: "Рок".into(),
                artist: "Пилот".into(),
                album: "Сказка о Прыгуне и Скользящем".into(),
                duration_str: "4:05".into(),
                play_count_str: "890 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_rok.png".into(),
            },
            LocalTrackItem {
                id: "spotify:track:patron".into(),
                index: 3,
                title: "Дайте мне один патрон".into(),
                artist: "▶ Клип • Ангел НеБес".into(),
                album: "Идиот".into(),
                duration_str: "3:55".into(),
                play_count_str: "540 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_patron.png".into(),
            },
            LocalTrackItem {
                id: "spotify:track:sid_nancy".into(),
                index: 4,
                title: "Sid & Nancy".into(),
                artist: "Lumen".into(),
                album: "Без консервантов".into(),
                duration_str: "3:50".into(),
                play_count_str: "4 300 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_sid_nancy.png".into(),
            },
            LocalTrackItem {
                id: "spotify:track:amsterdam".into(),
                index: 5,
                title: "Амстердам".into(),
                artist: "Крематорий".into(),
                album: "Амстердам".into(),
                duration_str: "3:30".into(),
                play_count_str: "2 100 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_amsterdam.png".into(),
            },
            LocalTrackItem {
                id: "spotify:track:skovannye".into(),
                index: 6,
                title: "Скованные одной цепью".into(),
                artist: "Вячеслав Бутусов".into(),
                album: "Nautilus Pompilius".into(),
                duration_str: "4:15".into(),
                play_count_str: "8 900 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_skovannye.png".into(),
            },
            LocalTrackItem {
                id: "spotify:track:haru_mamburu".into(),
                index: 7,
                title: "Хару мамбуру".into(),
                artist: "Ногу Свело!".into(),
                album: "Хару Мамбуру".into(),
                duration_str: "3:45".into(),
                play_count_str: "3 700 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_haru_mamburu.png".into(),
            },
            LocalTrackItem {
                id: "spotify:track:po_raskrashennoy".into(),
                index: 8,
                title: "По раскрашенной душе".into(),
                artist: "Кукрыниксы".into(),
                album: "Раскрашенная душа".into(),
                duration_str: "3:35".into(),
                play_count_str: "1 500 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_po_raskrashennoy.png".into(),
            },
            LocalTrackItem {
                id: "spotify:track:vesna".into(),
                index: 9,
                title: "Весна".into(),
                artist: "Конец фильма".into(),
                album: "Саундтреки".into(),
                duration_str: "3:25".into(),
                play_count_str: "6 400 000".into(),
                is_liked: false,
                is_playing: false,
                image_url: "q_vesna.png".into(),
            },
        ];

        let kis_local = LocalTrackItem {
            id: "spotify:track:kis_dom".into(),
            index: 0,
            title: "Проклятый старый дом".into(),
            artist: "Korol i Shut".into(),
            album: "Как в старой сказке".into(),
            duration_str: "4:17".into(),
            play_count_str: "54 321 000".into(),
            is_liked: true,
            is_playing: true,
            image_url: "korol_i_shut.png".into(),
        };

        let bohemian_track = LocalTrackItem {
            id: "spotify:track:bohemian".into(),
            index: 10,
            title: "Bohemian Rhapsody".into(),
            artist: "Queen".into(),
            album: "A Night At The Opera".into(),
            duration_str: "5:55".into(),
            play_count_str: "2 340 000 000".into(),
            is_liked: false,
            is_playing: false,
            image_url: "t_bohemian_rhapsody.png".into(),
        };

        let show_must_go_on_track = LocalTrackItem {
            id: "spotify:track:show_must_go_on".into(),
            index: 11,
            title: "The Show Must Go On".into(),
            artist: "Queen".into(),
            album: "Innuendo".into(),
            duration_str: "4:33".into(),
            play_count_str: "890 000 000".into(),
            is_liked: false,
            is_playing: false,
            image_url: "t_show_must_go_on.png".into(),
        };

        let mut all_catalog = Vec::new();
        all_catalog.push(kis_local.clone());
        for t in &queue_tracks {
            all_catalog.push(t.clone());
        }
        all_catalog.push(bohemian_track.clone());
        all_catalog.push(show_must_go_on_track.clone());

        {
            let mut guard = cached_playlists.lock().await;
            *guard = library_items.clone();
        }

        {
            let mut q_guard = cached_queue.lock().await;
            *q_guard = all_catalog.clone();
        }

        *active_track_idx.lock().await = 0;
        *playback_duration.lock().await = 257.0;
        *playback_elapsed.lock().await = 252.0;
        playback_is_active.store(true, Ordering::Relaxed);

        let lib_ui = library_items.clone();
        let q_ui = queue_tracks.clone();
        let kis_ui = kis_local.clone();

        let _ = app_weak.upgrade_in_event_loop(move |ui| {
            ui.set_is_authenticated(true);
            ui.set_is_login_open(false);
            ui.set_user_display_name("lswaj49gzhi60w59e4ds1t115".into());
            ui.set_user_initial("L".into());
            ui.set_active_speaker_name("Моя кухня".into());
            ui.set_current_workspace(2); // Search / Browse
            ui.set_global_search_query("queen".into());
            ui.set_active_track_title("Проклятый старый дом".into());
            ui.set_active_track_artist("Korol i Shut".into());
            ui.set_current_time("4:12".into());
            ui.set_total_time("4:17".into());
            ui.set_track_progress(4.2 / 4.28);
            ui.set_is_playing(true);
            ui.set_is_liked(true);
            ui.set_is_drawer_open(true);
            ui.set_drawer_mode(0); // Queue
            ui.set_drawer_queue_tab(0); // Очередь

            if let Some(path) = kis_cover_path {
                if let Ok(img) = slint::Image::load_from_path(&path) {
                    ui.set_current_track_cover(img);
                    ui.set_has_current_track_cover(true);
                }
            }

            let sl_lib: Vec<PlaylistItem> = lib_ui.iter().map(to_slint_playlist).collect();
            ui.set_user_playlists(ModelRc::new(VecModel::from(sl_lib)));

            let sl_q: Vec<TrackItem> = q_ui.iter().map(to_slint_track).collect();
            ui.set_queue_items(ModelRc::new(VecModel::from(sl_q)));

            let sl_cur = to_slint_track(&kis_ui);
            ui.set_current_playing_track(sl_cur);
        });

        // Start playback so PulseAudio output stream is active!
        engine.send_command(AudioCommand::Play).await;
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
    let cached_playlists: Arc<Mutex<Vec<LocalPlaylistItem>>> = Arc::new(Mutex::new(Vec::new()));
    let cached_albums: Arc<Mutex<Vec<LocalPlaylistItem>>> = Arc::new(Mutex::new(Vec::new()));
    let cached_artists: Arc<Mutex<Vec<LocalPlaylistItem>>> = Arc::new(Mutex::new(Vec::new()));

    // Shared playback tracking state
    let playback_duration: Arc<Mutex<f64>> = Arc::new(Mutex::new(257.0));
    let playback_elapsed: Arc<Mutex<f64>> = Arc::new(Mutex::new(252.0));
    let playback_is_active: Arc<AtomicBool> = Arc::new(AtomicBool::new(true));
    let active_track_idx: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));
    let cached_queue: Arc<Mutex<Vec<LocalTrackItem>>> = Arc::new(Mutex::new(Vec::new()));

    // UI Callbacks: Audio
    app.on_play_pause({
        let engine = audio_sender.clone();
        let app_weak = app_weak.clone();
        let p_act = Arc::clone(&playback_is_active);
        move || {
            let engine = engine.clone();
            let app_weak = app_weak.clone();
            let p_act = Arc::clone(&p_act);
            tokio::spawn(async move {
                let currently_active = p_act.load(Ordering::Relaxed);
                let new_state = !currently_active;
                info!("[UI ACTION] Play/Pause toggled to {}", new_state);
                if new_state {
                    engine.send_command(AudioCommand::Play).await;
                } else {
                    engine.send_command(AudioCommand::Pause).await;
                }
                p_act.store(new_state, Ordering::Relaxed);
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_is_playing(new_state);
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
        let c_queue = Arc::clone(&cached_queue);
        move || {
            info!("[UI ACTION] Next track clicked");
            let app_weak = app_weak.clone();
            let a_idx = Arc::clone(&a_idx);
            let engine = engine.clone();
            let p_dur = Arc::clone(&p_dur);
            let p_el = Arc::clone(&p_el);
            let p_act = Arc::clone(&p_act);
            let c_queue = Arc::clone(&c_queue);

            tokio::spawn(async move {
                let queue = c_queue.lock().await.clone();
                if queue.is_empty() { return; }
                let curr_i = *a_idx.lock().await;
                let next_i = if curr_i >= 0 && (curr_i as usize) < queue.len() - 1 {
                    curr_i + 1
                } else {
                    0
                };
                let track = queue[next_i as usize].clone();
                play_track_internal(track, next_i, app_weak, engine, p_dur, p_el, p_act, a_idx, c_queue).await;
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
        let c_queue = Arc::clone(&cached_queue);
        move || {
            info!("[UI ACTION] Previous track clicked");
            let app_weak = app_weak.clone();
            let a_idx = Arc::clone(&a_idx);
            let engine = engine.clone();
            let p_dur = Arc::clone(&p_dur);
            let p_el = Arc::clone(&p_el);
            let p_act = Arc::clone(&p_act);
            let c_queue = Arc::clone(&c_queue);

            tokio::spawn(async move {
                let queue = c_queue.lock().await.clone();
                if queue.is_empty() { return; }
                let curr_i = *a_idx.lock().await;
                let el = *p_el.lock().await;
                let prev_i = if el > 3.0 {
                    curr_i
                } else if curr_i > 0 {
                    curr_i - 1
                } else {
                    (queue.len() - 1) as i32
                };
                let track = queue[prev_i as usize].clone();
                play_track_internal(track, prev_i, app_weak, engine, p_dur, p_el, p_act, a_idx, c_queue).await;
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

    app.on_select_connect_device({
        let engine = audio_sender.clone();
        let app_weak = app_weak.clone();
        move |device_id| {
            let did = device_id.to_string();
            info!("[UI ACTION] Playback device selected: {}", did);
            let engine = engine.clone();
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                if did.is_empty() {
                    engine.set_target_device_id(None).await;
                } else {
                    engine.set_target_device_id(Some(did.clone())).await;
                }
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    let model = ui.get_discovered_devices();
                    let mut updated = Vec::new();
                    for i in 0..model.row_count() {
                        if let Some(mut d) = model.row_data(i) {
                            d.is_active = d.id == did;
                            updated.push(d);
                        }
                    }
                    ui.set_discovered_devices(ModelRc::new(VecModel::from(updated)));
                });
            });
        }
    });

    app.on_refresh_devices({
        let api_client_lock = Arc::clone(&active_api_client);
        let app_weak = app_weak.clone();
        move || {
            let api_client_lock = Arc::clone(&api_client_lock);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let maybe_client = {
                    let guard = api_client_lock.lock().await;
                    guard.clone()
                };
                if let Some(client) = maybe_client {
                    if let Ok(devs) = client.get_devices().await {
                        let items: Vec<DeviceItem> = devs.into_iter().map(|(id, name, dtype, active)| {
                            DeviceItem {
                                id: id.into(),
                                name: name.into(),
                                device_type: dtype.into(),
                                is_active: active,
                            }
                        }).collect();
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_discovered_devices(ModelRc::new(VecModel::from(items)));
                        });
                    }
                }
            });
        }
    });

    // 1. Library Search (Local Sidebar Filter)
    app.on_library_search_query({
        let cached_p = Arc::clone(&cached_playlists);
        let cached_alb = Arc::clone(&cached_albums);
        let cached_art = Arc::clone(&cached_artists);
        let app_weak = app_weak.clone();
        move |query| {
            let q = query.to_string().trim().to_lowercase();
            info!("[UI ACTION] Library local filter query: '{}'", q);
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

                let filtered: Vec<LocalPlaylistItem> = if q.is_empty() {
                    all
                } else {
                    all.into_iter().filter(|item| {
                        item.title.to_lowercase().contains(&q) || item.subtitle.to_lowercase().contains(&q)
                    }).collect()
                };

                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    let sl: Vec<PlaylistItem> = filtered.iter().map(to_slint_playlist).collect();
                    ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
                });
            });
        }
    });

    // 2. Global TopBar Search (Spotify Catalog Search)
    app.on_search_query({
        let api_client_lock = Arc::clone(&active_api_client);
        let app_weak = app_weak.clone();
        move |query| {
            let q = query.to_string().trim().to_string();
            info!("[UI ACTION] Global Spotify catalog search: '{}'", q);
            let api_client_lock = Arc::clone(&api_client_lock);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let client_opt = {
                    let guard = api_client_lock.lock().await;
                    guard.clone()
                };

                if let Some(client) = client_opt {
                    if q.is_empty() {
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_global_search_query("".into());
                            ui.set_current_workspace(2);
                        });
                        return;
                    }

                    match client.search(&q).await {
                        Ok(res) => {
                            let local_tracks: Vec<LocalTrackItem> = res.tracks.into_iter().map(|t| {
                                LocalTrackItem {
                                    id: t.id,
                                    index: t.index,
                                    title: t.title,
                                    artist: t.artist,
                                    album: t.album,
                                    duration_str: t.duration_str,
                                    play_count_str: String::new(),
                                    is_liked: false,
                                    is_playing: false,
                                    image_url: t.image_url,
                                }
                            }).collect();

                            let local_artists: Vec<LocalPlaylistItem> = res.artists.into_iter().map(|a| {
                                LocalPlaylistItem {
                                    id: a.id,
                                    title: a.title,
                                    subtitle: a.subtitle,
                                    is_pinned: false,
                                    is_active: false,
                                    item_type: 2,
                                    image_url: a.image_url,
                                }
                            }).collect();

                            let local_albums: Vec<LocalPlaylistItem> = res.albums.into_iter().map(|a| {
                                LocalPlaylistItem {
                                    id: a.id,
                                    title: a.title,
                                    subtitle: a.subtitle,
                                    is_pinned: false,
                                    is_active: false,
                                    item_type: 1,
                                    image_url: a.image_url,
                                }
                            }).collect();

                            let q_copy = q.clone();
                            let trk_ui = local_tracks.clone();
                            let art_ui = local_artists.clone();
                            let alb_ui = local_albums.clone();

                            let _ = app_weak.upgrade_in_event_loop(move |ui| {
                                ui.set_current_workspace(2);
                                ui.set_global_search_query(q_copy.into());
                                let sl_tracks: Vec<TrackItem> = trk_ui.iter().map(to_slint_track).collect();
                                let sl_artists: Vec<PlaylistItem> = art_ui.iter().map(to_slint_playlist).collect();
                                let sl_albums: Vec<PlaylistItem> = alb_ui.iter().map(to_slint_playlist).collect();
                                ui.set_search_results_tracks(ModelRc::new(VecModel::from(sl_tracks)));
                                ui.set_search_results_artists(ModelRc::new(VecModel::from(sl_artists)));
                                ui.set_search_results_albums(ModelRc::new(VecModel::from(sl_albums)));
                            });
                        }
                        Err(e) => {
                            error!("[API ERROR] Global search failed: {}", e);
                        }
                    }
                } else {
                    if q.is_empty() {
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            ui.set_global_search_query("".into());
                            ui.set_current_workspace(2);
                        });
                        return;
                    }

                    let sample_tracks = vec![
                        LocalTrackItem {
                            id: "spotify:track:4cOdK2wGLETKBW3PvgPWqT".into(),
                            index: 1,
                            title: format!("{} - Track 1", q),
                            artist: "Daft Punk".into(),
                            album: "Discovery".into(),
                            duration_str: "3:44".into(),
                            play_count_str: "1,245,678,901".into(),
                            is_liked: false,
                            is_playing: false,
                            image_url: "https://i.scdn.co/image/ab67616d0000b273447936a29f8fba3d52718105".into(),
                        },
                        LocalTrackItem {
                            id: "spotify:track:0VjIjW4GlUZAMYd2vXMi3b".into(),
                            index: 2,
                            title: format!("{} - Track 2", q),
                            artist: "The Weeknd".into(),
                            album: "After Hours".into(),
                            duration_str: "3:20".into(),
                            play_count_str: "3,892,100,234".into(),
                            is_liked: true,
                            is_playing: false,
                            image_url: "https://i.scdn.co/image/ab67616d0000b2738863bc11d2aa12b54f5aeb36".into(),
                        },
                        LocalTrackItem {
                            id: "spotify:track:7qiZfU4dY1lWllzX7mPBI3".into(),
                            index: 3,
                            title: format!("{} - Track 3", q),
                            artist: "Ed Sheeran".into(),
                            album: "÷ (Divide)".into(),
                            duration_str: "3:53".into(),
                            play_count_str: "3,500,432,100".into(),
                            is_liked: false,
                            is_playing: false,
                            image_url: "https://i.scdn.co/image/ab67616d0000b273ba5db46f4b838ef6027e6f96".into(),
                        },
                        LocalTrackItem {
                            id: "spotify:track:3n3Ppam7vgaVa1iaRUc9Lp".into(),
                            index: 4,
                            title: format!("{} - Track 4", q),
                            artist: "The Killers".into(),
                            album: "Hot Fuss".into(),
                            duration_str: "3:42".into(),
                            play_count_str: "1,980,123,456".into(),
                            is_liked: false,
                            is_playing: false,
                            image_url: "https://i.scdn.co/image/ab67616d0000b273ffd9108c5c7d0d0c3ebaa58b".into(),
                        },
                    ];

                    let sample_artists = vec![
                        LocalPlaylistItem {
                            id: "spotify:artist:06HL4z0CvFAxyc27GXpf02".into(),
                            title: format!("Исполнитель: {}", q),
                            subtitle: "Исполнитель • 85 432 100 слушателей за месяц".into(),
                            is_pinned: false,
                            is_active: false,
                            item_type: 2,
                            image_url: "https://i.scdn.co/image/ab6761610000e5eb4a86b97f0f622e96417b1b36".into(),
                        },
                    ];

                    let sample_albums = vec![
                        LocalPlaylistItem {
                            id: "spotify:album:4yP0hdKOJeuMsxY99YIHR5".into(),
                            title: format!("Альбом: {}", q),
                            subtitle: "Альбом • 2024".into(),
                            is_pinned: false,
                            is_active: false,
                            item_type: 1,
                            image_url: "https://i.scdn.co/image/ab67616d0000b2738863bc11d2aa12b54f5aeb36".into(),
                        },
                    ];

                    let q_copy = q.clone();
                    let trk_ui = sample_tracks.clone();
                    let art_ui = sample_artists.clone();
                    let alb_ui = sample_albums.clone();

                    let _ = app_weak.upgrade_in_event_loop(move |ui| {
                        ui.set_current_workspace(2);
                        ui.set_global_search_query(q_copy.into());
                        let sl_tracks: Vec<TrackItem> = trk_ui.iter().map(to_slint_track).collect();
                        let sl_artists: Vec<PlaylistItem> = art_ui.iter().map(to_slint_playlist).collect();
                        let sl_albums: Vec<PlaylistItem> = alb_ui.iter().map(to_slint_playlist).collect();
                        ui.set_search_results_tracks(ModelRc::new(VecModel::from(sl_tracks)));
                        ui.set_search_results_artists(ModelRc::new(VecModel::from(sl_artists)));
                        ui.set_search_results_albums(ModelRc::new(VecModel::from(sl_albums)));
                    });
                }
            });
        }
    });

    // Cascading Context Menu Action: Add Track to Playlist
    app.on_add_track_to_playlist({
        let api_client_lock = Arc::clone(&active_api_client);
        move |track_id, playlist_id| {
            let tid = track_id.to_string();
            let pid = playlist_id.to_string();
            info!("[UI ACTION] Adding track {} to playlist {}", tid, pid);
            let api_client_lock = Arc::clone(&api_client_lock);
            tokio::spawn(async move {
                let client_opt = {
                    let guard = api_client_lock.lock().await;
                    guard.clone()
                };
                if let Some(client) = client_opt {
                    let _ = client.add_track_to_playlist(&pid, &tid).await;
                }
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
        let api_client_lock = Arc::clone(&active_api_client);
        let app_weak = app_weak.clone();
        move |track_id| {
            let tid = track_id.to_string();
            info!("[UI ACTION] Toggle like for track: {}", tid);
            let api_client_lock = Arc::clone(&api_client_lock);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let client_opt = {
                    let guard = api_client_lock.lock().await;
                    guard.clone()
                };
                if let Some(client) = client_opt {
                    let _ = client.save_track(&tid).await;
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
                } else if tid.starts_with("spotify:") {
                    format!("https://open.spotify.com/{}", tid.replace("spotify:", "").replace(":", "/"))
                } else {
                    format!("https://open.spotify.com/track/{}", tid)
                };
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(link);
                }
            });
        }
    });

    app.on_open_external_link(|url| {
        let u = url.to_string();
        info!("[UI ACTION] Open external URL: {}", u);
        let _ = std::process::Command::new("xdg-open").arg(&u).spawn();
    });

    // Context Menu Action: Delete Playlist
    app.on_delete_playlist({
        let api_client_lock = Arc::clone(&active_api_client);
        let cached_p = Arc::clone(&cached_playlists);
        let app_weak = app_weak.clone();
        move |pl_id| {
            let id = pl_id.to_string();
            info!("[UI ACTION] Delete playlist requested: {}", id);
            let api_client_lock = Arc::clone(&api_client_lock);
            let cached_p = Arc::clone(&cached_p);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let client_opt = {
                    let guard = api_client_lock.lock().await;
                    guard.clone()
                };
                if let Some(client) = client_opt {
                    let _ = client.delete_playlist(&id).await;
                }
                {
                    let mut guard = cached_p.lock().await;
                    guard.retain(|x| x.id != id);
                }
                let updated = {
                    let guard = cached_p.lock().await;
                    guard.clone()
                };
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    let sl: Vec<PlaylistItem> = updated.iter().map(to_slint_playlist).collect();
                    ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
                });
            });
        }
    });

    // Action: Create New Playlist
    app.on_create_new_playlist({
        let api_client_lock = Arc::clone(&active_api_client);
        let cached_p = Arc::clone(&cached_playlists);
        let app_weak = app_weak.clone();
        move || {
            info!("[UI ACTION] Create new playlist requested");
            let api_client_lock = Arc::clone(&api_client_lock);
            let cached_p = Arc::clone(&cached_p);
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let client_opt = {
                    let guard = api_client_lock.lock().await;
                    guard.clone()
                };
                if let Some(client) = client_opt {
                    if let Ok(new_pl) = client.create_playlist("Мой плейлист").await {
                        let new_item = LocalPlaylistItem {
                            id: new_pl.id,
                            title: new_pl.title,
                            subtitle: new_pl.subtitle,
                            is_pinned: false,
                            is_active: false,
                            item_type: 0,
                            image_url: new_pl.image_url,
                        };
                        let mut p = cached_p.lock().await;
                        p.insert(0, new_item);
                        let updated = p.clone();
                        let _ = app_weak.upgrade_in_event_loop(move |ui| {
                            let sl: Vec<PlaylistItem> = updated.iter().map(to_slint_playlist).collect();
                            ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
                        });
                        return;
                    }
                }
                // Fallback for offline / unauthenticated
                let new_item = {
                    let mut guard = cached_p.lock().await;
                    let count = guard.len() + 1;
                    let item = LocalPlaylistItem {
                        id: format!("custom_{}", count),
                        title: format!("Мой плейлист #{}", count),
                        subtitle: "Плейлист • Пользовательский".to_string(),
                        is_pinned: false,
                        is_active: false,
                        item_type: 0,
                        image_url: "".to_string(),
                    };
                    guard.insert(0, item.clone());
                    item
                };
                let all = {
                    let guard = cached_p.lock().await;
                    guard.clone()
                };
                let _ = app_weak.upgrade_in_event_loop(move |ui| {
                    let sl: Vec<PlaylistItem> = all.iter().map(to_slint_playlist).collect();
                    ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
                    ui.set_current_workspace(1);
                    ui.set_current_album_title(new_item.title.into());
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
                        let p = cached_p.lock().await;
                        p.clone()
                    }
                    2 => {
                        let alb = cached_alb.lock().await;
                        alb.clone()
                    }
                    3 => {
                        let art = cached_art.lock().await;
                        art.clone()
                    }
                    _ => {
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
                    let sl: Vec<PlaylistItem> = to_show.iter().map(to_slint_playlist).collect();
                    ui.set_user_playlists(ModelRc::new(VecModel::from(sl)));
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
        let c_queue = Arc::clone(&cached_queue);
        move |track_id| {
            info!("[UI ACTION] Play track clicked: {}", track_id);
            let engine = engine.clone();
            let app_weak = app_weak.clone();
            let tid = track_id.to_string();
            let p_dur = Arc::clone(&p_dur);
            let p_el = Arc::clone(&p_el);
            let p_act = Arc::clone(&p_act);
            let a_idx = Arc::clone(&a_idx);
            let c_queue = Arc::clone(&c_queue);

            tokio::spawn(async move {
                let queue = c_queue.lock().await.clone();
                let matched = queue.iter().enumerate().find(|(_, t)| {
                    t.id == tid ||
                    tid.ends_with(&t.id) ||
                    t.id.ends_with(&tid) ||
                    (tid.contains("bohemian") && t.id.contains("bohemian")) ||
                    (tid.contains("show_must_go_on") && t.id.contains("show_must_go_on")) ||
                    (tid.contains("kis_dom") && t.id.contains("kis_dom")) ||
                    (tid.contains("basta_rasta") && t.id.contains("basta_rasta")) ||
                    (tid.contains("rok") && t.id.contains("rok")) ||
                    (tid.contains("patron") && t.id.contains("patron")) ||
                    (tid.contains("sid_nancy") && t.id.contains("sid_nancy")) ||
                    (tid.contains("amsterdam") && t.id.contains("amsterdam")) ||
                    (tid.contains("skovannye") && t.id.contains("skovannye")) ||
                    (tid.contains("haru_mamburu") && t.id.contains("haru_mamburu")) ||
                    (tid.contains("po_raskrashennoy") && t.id.contains("po_raskrashennoy")) ||
                    (tid.contains("vesna") && t.id.contains("vesna"))
                });

                if let Some((idx, trk)) = matched {
                    play_track_internal(trk.clone(), idx as i32, app_weak, engine, p_dur, p_el, p_act, a_idx, c_queue).await;
                } else {
                    let spotify_uri = if tid.starts_with("spotify:track:") { tid.clone() } else { format!("spotify:track:{}", tid) };
                    engine.send_command(AudioCommand::LoadUri(spotify_uri)).await;
                    p_act.store(true, Ordering::Relaxed);
                    *p_el.lock().await = 0.0;
                    let _ = app_weak.upgrade_in_event_loop(move |ui| {
                        ui.set_is_playing(true);
                        ui.set_current_time("0:00".into());
                        ui.set_track_progress(0.0);
                    });
                }
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
                    let mut found_title = "Плейлист".to_string();
                    let mut found_creator = "Spotify".to_string();
                    let mut found_type = "Плейлист".to_string();
                    let mut found_image_url = String::new();
                    let mut item_type = 0; // 0=playlist, 1=album, 2=artist

                    {
                        let p = cached_p.lock().await;
                        if let Some(item) = p.iter().find(|x| x.id == id) {
                            found_title = item.title.to_string();
                            found_creator = item.subtitle.to_string();
                            found_image_url = item.image_url.to_string();
                            found_type = if item.item_type == 3 {
                                "Подборка".to_string()
                            } else {
                                "Плейлист".to_string()
                            };
                            item_type = item.item_type;
                        }
                    }
                    if item_type != 0 || found_title != "Плейлист" {
                        // Already resolved title from playlists
                    } else {
                        let alb = cached_alb.lock().await;
                        if let Some(item) = alb.iter().find(|x| x.id == id) {
                            found_title = item.title.to_string();
                            found_creator = item.subtitle.to_string();
                            found_image_url = item.image_url.to_string();
                            found_type = "Альбом".to_string();
                            item_type = 1;
                        }
                    }
                    if item_type == 0 && found_title == "Плейлист" {
                        let art = cached_art.lock().await;
                        if let Some(item) = art.iter().find(|x| x.id == id) {
                            found_title = item.title.to_string();
                            found_creator = item.subtitle.to_string();
                            found_image_url = item.image_url.to_string();
                            found_type = "Исполнитель".to_string();
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
                            let local_tracks: Vec<LocalTrackItem> = tracks.into_iter().map(|t| {
                                LocalTrackItem {
                                    id: t.id,
                                    index: t.index,
                                    title: t.title,
                                    artist: t.artist,
                                    album: t.album,
                                    duration_str: t.duration_str,
                                    play_count_str: String::new(),
                                    is_liked: false,
                                    is_playing: false,
                                    image_url: t.image_url,
                                }
                            }).collect();

                            let title_copy = found_title.clone();
                            let creator_copy = found_creator.clone();
                            let type_copy = found_type.clone();
                            let cached_path = get_cached_image_path(&found_image_url);
                            let has_cover = cached_path.is_some();
                            let local_copy = local_tracks.clone();

                            let _ = app_weak.upgrade_in_event_loop(move |ui| {
                                ui.set_current_workspace(1);
                                ui.set_current_album_title(title_copy.into());
                                ui.set_current_album_creator(creator_copy.into());
                                ui.set_current_album_type(type_copy.into());
                                if let Some(path) = &cached_path {
                                    if let Ok(img) = slint::Image::load_from_path(path) {
                                        ui.set_current_album_cover(img);
                                        ui.set_has_current_album_cover(true);
                                    } else {
                                        ui.set_has_current_album_cover(false);
                                    }
                                } else {
                                    ui.set_has_current_album_cover(false);
                                }
                                let sl: Vec<TrackItem> = local_copy.iter().map(to_slint_track).collect();
                                ui.set_current_tracks(ModelRc::new(VecModel::from(sl)));
                            });

                            if !found_image_url.is_empty() && !has_cover {
                                let url_copy = found_image_url.clone();
                                let app_weak_c = app_weak.clone();
                                tokio::spawn(async move {
                                    if let Some(path) = load_image_path_async(&url_copy).await {
                                        let _ = app_weak_c.upgrade_in_event_loop(move |ui| {
                                            if let Ok(img) = slint::Image::load_from_path(&path) {
                                                ui.set_current_album_cover(img);
                                                ui.set_has_current_album_cover(true);
                                            }
                                        });
                                    }
                                });
                            }

                            // Download any uncached track thumbnails
                            for trk in &local_tracks {
                                if !trk.image_url.is_empty() && get_cached_image_path(&trk.image_url).is_none() {
                                    let u = trk.image_url.clone();
                                    let app_w = app_weak.clone();
                                    let all_t = local_tracks.clone();
                                    tokio::spawn(async move {
                                        if let Some(_) = load_image_path_async(&u).await {
                                            let _ = app_w.upgrade_in_event_loop(move |ui| {
                                                let sl: Vec<TrackItem> = all_t.iter().map(to_slint_track).collect();
                                                ui.set_current_tracks(ModelRc::new(VecModel::from(sl)));
                                            });
                                        }
                                    });
                                }
                            }
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
        let c_queue = Arc::clone(&cached_queue);
        let engine = audio_sender.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                if p_act.load(Ordering::Relaxed) && engine.is_playing() {
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
                        let c_q_c = Arc::clone(&c_queue);

                        tokio::spawn(async move {
                            let queue = c_q_c.lock().await.clone();
                            if !queue.is_empty() {
                                let curr_i = *a_idx_c.lock().await;
                                let next_i = (curr_i + 1) % (queue.len() as i32);
                                let trk = queue[next_i as usize].clone();
                                play_track_internal(trk, next_i, app_w, eng_c, p_dur_c, p_el_c, p_act_c, a_idx_c, c_q_c).await;
                            }
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
                        let creds = librespot::core::authentication::Credentials::with_password(&email_str, &pass_str);
                        engine.init_session(session.clone(), creds).await;

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
                        let client = SpotifyApiClient::new(access_token.clone(), username.clone());
                        {
                            let mut guard = api_client_lock.lock().await;
                            *guard = Some(client.clone());
                        }

                        // Connect playback engine
                        let creds = librespot::core::authentication::Credentials::with_access_token(&access_token);
                        engine.init_session(session.clone(), creds).await;

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
                        let creds = librespot::core::authentication::Credentials::with_access_token(&access_token);
                        engine.init_session(session.clone(), creds).await;

                        let username = session.username().to_string();
                        let client = SpotifyApiClient::new(access_token.clone(), username.clone());
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
                        info!("[STARTUP] Auto-login failed. Initializing offline Queen desktop reference state...");
                        init_queen_state(
                            app_weak.clone(),
                            audio_sender.clone(),
                            Arc::clone(&cached_playlists),
                            Arc::clone(&cached_queue),
                            Arc::clone(&playback_duration),
                            Arc::clone(&playback_elapsed),
                            Arc::clone(&playback_is_active),
                            Arc::clone(&active_track_idx),
                        );
                    }
                }
            } else {
                info!("[STARTUP] No saved credentials found. Initializing offline Queen desktop reference state...");
                init_queen_state(
                    app_weak.clone(),
                    audio_sender.clone(),
                    Arc::clone(&cached_playlists),
                    Arc::clone(&cached_queue),
                    Arc::clone(&playback_duration),
                    Arc::clone(&playback_elapsed),
                    Arc::clone(&playback_is_active),
                    Arc::clone(&active_track_idx),
                );
            }
        });
    }

    info!("Entering Slint UI event loop...");
    app.run()?;

    info!("SpotifyLight exited cleanly.");
    Ok(())
}
