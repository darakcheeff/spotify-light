use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::info;

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
}

#[derive(Clone)]
pub struct AudioEngine {
    command_tx: mpsc::Sender<AudioCommand>,
    is_playing: Arc<AtomicBool>,
}

impl AudioEngine {
    pub fn new() -> (Self, mpsc::Receiver<AudioCommand>) {
        let (command_tx, rx) = mpsc::channel(32);
        let is_playing = Arc::new(AtomicBool::new(false));

        (Self { command_tx, is_playing }, rx)
    }

    pub async fn send_command(&self, cmd: AudioCommand) {
        info!("Audio command dispatched: {:?}", cmd);
        let _ = self.command_tx.send(cmd).await;
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }
}
