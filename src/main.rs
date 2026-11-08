mod api;
mod audio;
mod discovery;
mod mpris;

slint::include_modules!();

use audio::{AudioCommand, AudioEngine};
use discovery::SpeakerDiscovery;
use mpris::MprisService;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    info!("Starting SpotifyLight (Native Rust + Slint, Zero-Electron)...");

    // Initialize Audio Engine with Self-Healing Output Supervisor
    let (audio_engine, rx) = AudioEngine::new();
    let audio_sender = audio_engine.clone();

    // Background Audio Supervisor (handles PipeWire/PulseAudio restarts and device disconnections)
    let supervisor_cmd_tx = audio_engine.command_sender();
    tokio::spawn(async move {
        AudioEngine::run_supervisor_loop(rx, supervisor_cmd_tx).await;
    });

    // Initialize Speaker Discovery (Google Home, Connect)
    let speaker_discovery = SpeakerDiscovery::new();
    speaker_discovery.start();

    // Initialize MPRIS2 (D-Bus)
    let _ = MprisService::init().await;

    // Build and run Slint UI
    let app = AppWindow::new()?;
    let app_weak = app.as_weak();

    // UI Callbacks
    app.on_play_pause({
        let engine = audio_sender.clone();
        move || {
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.send_command(AudioCommand::Toggle).await;
            });
        }
    });

    app.on_next_track({
        let engine = audio_sender.clone();
        move || {
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.send_command(AudioCommand::Next).await;
            });
        }
    });

    app.on_prev_track({
        let engine = audio_sender.clone();
        move || {
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.send_command(AudioCommand::Prev).await;
            });
        }
    });

    app.on_seek_position({
        let engine = audio_sender.clone();
        move |pos| {
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
        info!("Selected playback device: {}", device_id);
    });

    app.on_search_query(move |query| {
        info!("Search query: {}", query);
    });

    app.on_login_requested({
        let _app_weak = app_weak.clone();
        move |email, _pwd| {
            info!("Login requested for email: {}", email);
        }
    });

    app.on_otp_submitted({
        let _app_weak = app_weak.clone();
        move |email, code| {
            info!("OTP submitted for {}: {}", email, code);
        }
    });

    info!("Entering Slint UI event loop...");
    app.run()?;

    Ok(())
}
