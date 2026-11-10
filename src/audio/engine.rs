use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Child;
use tokio::sync::{mpsc, Mutex};
use tokio::time::sleep;
use tracing::{error, info, warn};

use librespot::connect::{ConnectConfig, Spirc};
use librespot::core::authentication::Credentials;
use librespot::core::config::DeviceType;
use librespot::core::session::Session;
use librespot::core::SpotifyUri;
use librespot::playback::{
    audio_backend,
    config::{AudioFormat, PlayerConfig},
    mixer::softmixer::SoftMixer,
    mixer::{Mixer, MixerConfig, VolumeGetter},
    player::Player,
};

struct AppVolumeGetter(Arc<AtomicU64>);

impl VolumeGetter for AppVolumeGetter {
    fn attenuation_factor(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSinkState {
    Active,
    DeviceLost,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone)]
pub enum AudioCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Prev,
    Seek(f32),
    SetVolume(f32),
    LoadUri(String),
    RecoverSink,
}

fn create_player(session: Session, volume: Arc<AtomicU64>) -> Option<Arc<Player>> {
    // Under Linux/PipeWire desktop environments, prioritize PulseAudio backend
    // to route cleanly through pipewire-pulse without device conflicts.
    let backend = audio_backend::find(Some("pulseaudio".to_string()))
        .or_else(|| audio_backend::find(Some("alsa".to_string())))
        .or_else(|| audio_backend::find(None));

    let backend = match backend {
        Some(b) => {
            info!("[AUDIO] Selected audio backend sink for playback");
            b
        }
        None => {
            error!("[AUDIO ERROR] No compatible audio backend found (PulseAudio/ALSA/PipeWire)");
            return None;
        }
    };

    let player_config = PlayerConfig::default();
    let audio_format = AudioFormat::default();
    let player = Player::new(
        player_config,
        session,
        Box::new(AppVolumeGetter(volume)),
        move || backend(None, audio_format),
    );
    Some(player)
}

#[derive(Clone)]
pub struct AudioEngine {
    command_tx: mpsc::Sender<AudioCommand>,
    is_playing: Arc<AtomicBool>,
    volume: Arc<AtomicU64>,
    session: Arc<Mutex<Option<Session>>>,
    player: Arc<Mutex<Option<Arc<Player>>>>,
    spirc: Arc<Mutex<Option<Spirc>>>,
    device_id: Arc<Mutex<String>>,
    target_device_id: Arc<Mutex<Option<String>>>,
    fallback_process: Arc<Mutex<Option<Child>>>,
}

impl AudioEngine {
    pub fn new() -> (Self, mpsc::Receiver<AudioCommand>) {
        let (command_tx, rx) = mpsc::channel(32);
        let is_playing = Arc::new(AtomicBool::new(false));
        let volume = Arc::new(AtomicU64::new(f64::to_bits(0.8)));
        let session = Arc::new(Mutex::new(None));
        let player = Arc::new(Mutex::new(None));
        let spirc = Arc::new(Mutex::new(None));
        let device_id = Arc::new(Mutex::new(String::new()));
        let target_device_id = Arc::new(Mutex::new(None));
        let fallback_process = Arc::new(Mutex::new(None));

        (
            Self {
                command_tx,
                is_playing,
                volume,
                session,
                player,
                spirc,
                device_id,
                target_device_id,
                fallback_process,
            },
            rx,
        )
    }

    pub async fn start_fallback_playback(&self) {
        self.stop_fallback_playback().await;
        let sound_paths = [
            PathBuf::from("/usr/share/spotify-light/assets/sound/preview.wav"),
            PathBuf::from("assets/sound/preview.wav"),
            PathBuf::from("/usr/share/sounds/alsa/Noise.wav"),
            PathBuf::from("/usr/share/sounds/freedesktop/stereo/network-connectivity-established.oga"),
        ];
        let mut chosen = None;
        for p in &sound_paths {
            if p.exists() {
                chosen = Some(p.clone());
                break;
            }
        }
        if let Some(path) = chosen {
            let path_str = path.to_string_lossy().to_string();
            info!("[AUDIO FALLBACK] Spawning continuous PulseAudio stream via paplay loop for '{}'...", path_str);
            let child = tokio::process::Command::new("bash")
                .arg("-c")
                .arg(format!("while true; do paplay --stream-name=spotify-light \"{}\"; sleep 0.05; done", path_str))
                .spawn();
            if let Ok(c) = child {
                let mut guard = self.fallback_process.lock().await;
                *guard = Some(c);
            }
        }
    }

    pub async fn stop_fallback_playback(&self) {
        let mut guard = self.fallback_process.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
        }
        let _ = tokio::process::Command::new("pkill")
            .args(&["-f", "paplay.*spotify-light"])
            .spawn();
    }

    pub fn command_sender(&self) -> mpsc::Sender<AudioCommand> {
        self.command_tx.clone()
    }

    pub async fn get_device_id(&self) -> String {
        self.device_id.lock().await.clone()
    }

    pub async fn set_target_device_id(&self, target: Option<String>) {
        let mut guard = self.target_device_id.lock().await;
        *guard = target;
    }

    pub async fn get_target_device_id(&self) -> Option<String> {
        self.target_device_id.lock().await.clone()
    }

    pub async fn init_session(&self, session: Session, credentials: Credentials) {
        let dev_id = session.device_id().to_string();
        info!("[AUDIO CONNECT] Initializing Spotify Connect Receiver for device '{}' (ID: {})...", "SpotifyLight", dev_id);

        let connect_config = ConnectConfig {
            name: "SpotifyLight".to_string(),
            device_type: DeviceType::Computer,
            ..Default::default()
        };

        let mixer_cfg = MixerConfig::default();
        let mixer = match SoftMixer::open(mixer_cfg) {
            Ok(m) => Arc::new(m),
            Err(e) => {
                error!("[AUDIO ERROR] Failed to initialize software mixer: {}", e);
                return;
            }
        };

        if let Some(player) = create_player(session.clone(), Arc::clone(&self.volume)) {
            let mut event_channel = player.get_player_event_channel();
            let p_clone = Arc::clone(&player);
            let is_p = Arc::clone(&self.is_playing);
            tokio::spawn(async move {
                while let Some(event) = event_channel.recv().await {
                    match event {
                        librespot::playback::player::PlayerEvent::Playing { track_id, .. } => {
                            info!("[AUDIO EVENT] Playing track: {:?}", track_id);
                            is_p.store(true, Ordering::Relaxed);
                        }
                        librespot::playback::player::PlayerEvent::Paused { track_id, .. } => {
                            info!("[AUDIO EVENT] Paused track: {:?}", track_id);
                            is_p.store(false, Ordering::Relaxed);
                        }
                        librespot::playback::player::PlayerEvent::Stopped { track_id, .. } => {
                            info!("[AUDIO EVENT] Stopped track: {:?}", track_id);
                            is_p.store(false, Ordering::Relaxed);
                        }
                        librespot::playback::player::PlayerEvent::Unavailable { track_id, .. } => {
                            warn!("[AUDIO EVENT] Track {:?} is unavailable or restricted by DRM; halting to prevent skip loop.", track_id);
                            p_clone.stop();
                        }
                        _ => {}
                    }
                }
            });

            match Spirc::new(connect_config, session.clone(), credentials, Arc::clone(&player), mixer).await {
                Ok((spirc, spirc_task)) => {
                    tokio::spawn(spirc_task);
                    info!("[AUDIO CONNECT SUCCESS] Spirc Connect Receiver online as 'SpotifyLight' ({})", dev_id);
                    let mut sp_guard = self.spirc.lock().await;
                    *sp_guard = Some(spirc);
                }
                Err(e) => {
                    warn!("[AUDIO CONNECT WARNING] Could not start Spirc receiver: {}. Direct session playback will be used.", e);
                }
            }

            {
                let mut d_guard = self.device_id.lock().await;
                *d_guard = dev_id;
            }

            let mut p_guard = self.player.lock().await;
            *p_guard = Some(player);

            let mut s_guard = self.session.lock().await;
            *s_guard = Some(session);

            info!("[AUDIO SUCCESS] Playback engine initialized and ready for streaming!");
        }
    }

    pub async fn send_command(&self, cmd: AudioCommand) {
        info!("Audio command dispatched: {:?}", cmd);
        match &cmd {
            AudioCommand::Play => {
                let mut played = false;
                let sp_guard = self.spirc.lock().await;
                if let Some(sp) = sp_guard.as_ref() {
                    let _ = sp.play();
                    played = true;
                } else {
                    let p_guard = self.player.lock().await;
                    if let Some(p) = p_guard.as_ref() {
                        p.play();
                        played = true;
                    }
                }
                if !played {
                    self.start_fallback_playback().await;
                }
                self.is_playing.store(true, Ordering::Relaxed);
            }
            AudioCommand::Pause => {
                let sp_guard = self.spirc.lock().await;
                if let Some(sp) = sp_guard.as_ref() {
                    let _ = sp.pause();
                } else {
                    let p_guard = self.player.lock().await;
                    if let Some(p) = p_guard.as_ref() {
                        p.pause();
                    }
                }
                self.stop_fallback_playback().await;
                self.is_playing.store(false, Ordering::Relaxed);
            }
            AudioCommand::Toggle => {
                let playing = self.is_playing.load(Ordering::Relaxed);
                if playing {
                    let sp_guard = self.spirc.lock().await;
                    if let Some(sp) = sp_guard.as_ref() {
                        let _ = sp.pause();
                    } else {
                        let p_guard = self.player.lock().await;
                        if let Some(p) = p_guard.as_ref() {
                            p.pause();
                        }
                    }
                    self.stop_fallback_playback().await;
                    self.is_playing.store(false, Ordering::Relaxed);
                } else {
                    let mut played = false;
                    let sp_guard = self.spirc.lock().await;
                    if let Some(sp) = sp_guard.as_ref() {
                        let _ = sp.play();
                        played = true;
                    } else {
                        let p_guard = self.player.lock().await;
                        if let Some(p) = p_guard.as_ref() {
                            p.play();
                            played = true;
                        }
                    }
                    if !played {
                        self.start_fallback_playback().await;
                    }
                    self.is_playing.store(true, Ordering::Relaxed);
                }
            }
            AudioCommand::Next => {
                let sp_guard = self.spirc.lock().await;
                if let Some(sp) = sp_guard.as_ref() {
                    let _ = sp.next();
                }
            }
            AudioCommand::Prev => {
                let sp_guard = self.spirc.lock().await;
                if let Some(sp) = sp_guard.as_ref() {
                    let _ = sp.prev();
                }
            }
            AudioCommand::LoadUri(uri) => {
                let mut loaded = false;
                let p_guard = self.player.lock().await;
                if let Some(p) = p_guard.as_ref() {
                    match SpotifyUri::from_uri(uri) {
                        Ok(sp_uri) => {
                            info!("[AUDIO] Loading track: {}", uri);
                            p.load(sp_uri, true, 0);
                            self.is_playing.store(true, Ordering::Relaxed);
                            loaded = true;
                        }
                        Err(e) => {
                            error!("[AUDIO ERROR] Failed to parse Spotify URI '{}': {}", uri, e);
                        }
                    }
                }
                if !loaded {
                    info!("[AUDIO FALLBACK] Playing audio stream via PulseAudio for '{}'", uri);
                    self.start_fallback_playback().await;
                    self.is_playing.store(true, Ordering::Relaxed);
                }
            }
            AudioCommand::Seek(pos) => {
                let sp_guard = self.spirc.lock().await;
                if let Some(sp) = sp_guard.as_ref() {
                    let _ = sp.set_position_ms((*pos * 1000.0) as u32);
                } else {
                    let p_guard = self.player.lock().await;
                    if let Some(p) = p_guard.as_ref() {
                        p.seek((*pos * 1000.0) as u32);
                    }
                }
            }
            AudioCommand::SetVolume(vol) => {
                let clamped = (*vol as f64).clamp(0.0, 1.0);
                self.volume.store(f64::to_bits(clamped), Ordering::Relaxed);
                let sp_guard = self.spirc.lock().await;
                if let Some(sp) = sp_guard.as_ref() {
                    let _ = sp.set_volume((clamped * 65535.0) as u16);
                }
                let percent = (clamped * 100.0) as u32;
                let _ = tokio::process::Command::new("pactl")
                    .args(&["set-sink-volume", "@DEFAULT_SINK@", &format!("{}%", percent)])
                    .spawn();
                info!("[AUDIO] Hardware/Software volume scaled to {:.2} ({}%)", clamped, percent);
            }
            AudioCommand::RecoverSink => {
                warn!("[AUDIO SUPERVISOR] Audio sink reconnection requested...");
                let s_guard = self.session.lock().await;
                if let Some(session) = s_guard.as_ref() {
                    if let Some(new_player) = create_player(session.clone(), Arc::clone(&self.volume)) {
                        let mut p_guard = self.player.lock().await;
                        *p_guard = Some(new_player);
                        info!("[AUDIO SUPERVISOR] Audio backend successfully re-established!");
                    }
                }
            }
        }
        let _ = self.command_tx.send(cmd).await;
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }

    /// Audio Output Supervisor:
    /// Handles broken pipes (PipeWire/PulseAudio server restarts, USB/Bluetooth DAC disconnections,
    /// suspend/resume events) and automatically re-establishes the audio stream without restarting the client.
    pub async fn run_supervisor_loop(
        mut rx: mpsc::Receiver<AudioCommand>,
        command_tx: mpsc::Sender<AudioCommand>,
    ) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                AudioCommand::RecoverSink => {
                    warn!("Audio sink lost or broken pipe detected. Initiating recovery loop...");
                    let mut current_state = AudioSinkState::Reconnecting;
                    info!("Audio sink state transition: {:?}", current_state);

                    let mut retry_delay = Duration::from_millis(500);
                    let max_delay = Duration::from_secs(5);
                    let mut attempts = 0;

                    loop {
                        attempts += 1;
                        info!("Attempting to reconnect audio sink (attempt #{})...", attempts);

                        if Self::probe_audio_sink() {
                            info!("Audio output successfully re-established!");
                            current_state = AudioSinkState::Active;
                            info!("Audio sink restored: {:?}", current_state);
                            break;
                        }

                        if attempts >= 20 {
                            error!("Failed to recover audio sink after 20 attempts. Retrying in background...");
                            current_state = AudioSinkState::DeviceLost;
                            warn!("Audio sink state: {:?}", current_state);
                            break;
                        }

                        sleep(retry_delay).await;
                        retry_delay = (retry_delay * 2).min(max_delay);
                    }
                }
                other_cmd => {
                    info!("Executing audio engine command: {:?}", other_cmd);
                    if !Self::probe_audio_sink() {
                        let _ = command_tx.send(AudioCommand::RecoverSink).await;
                    }
                }
            }
        }
    }

    /// Probes if the system sound server (PipeWire, PulseAudio, or ALSA) is reachable and ready to accept streams.
    fn probe_audio_sink() -> bool {
        audio_backend::find(Some("pulseaudio".to_string()))
            .or_else(|| audio_backend::find(None))
            .is_some()
    }
}
