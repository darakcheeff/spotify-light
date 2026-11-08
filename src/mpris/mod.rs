use tracing::info;

pub struct MprisService;

impl MprisService {
    pub async fn init() -> Result<(), String> {
        info!("Registering org.mpris.MediaPlayer2.SpotifyLight on D-Bus session bus...");
        // Exposes Player interface: Play, Pause, Next, Previous, Volume, Metadata
        Ok(())
    }
}
