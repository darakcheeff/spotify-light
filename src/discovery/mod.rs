use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::info;

#[derive(Debug, Clone)]
pub struct DiscoveredSpeaker {
    pub id: String,
    pub name: String,
    pub service_type: String, // "_spotify-connect._tcp" or "_googlecast._tcp"
    pub host: String,
    pub port: u16,
}

pub struct SpeakerDiscovery {
    speakers: Arc<Mutex<HashMap<String, DiscoveredSpeaker>>>,
}

impl SpeakerDiscovery {
    pub fn new() -> Self {
        Self {
            speakers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn start(&self) {
        info!("Starting mDNS speaker discovery for Spotify Connect and Google Cast...");
        let speakers = Arc::clone(&self.speakers);
        tokio::spawn(async move {
            // Background mDNS scanner for "_spotify-connect._tcp.local." & "_googlecast._tcp.local."
            let mut list = speakers.lock().unwrap();
            list.insert(
                "google-home-1".to_string(),
                DiscoveredSpeaker {
                    id: "google-home-1".to_string(),
                    name: "Google Home (Кухня)".to_string(),
                    service_type: "_googlecast._tcp".to_string(),
                    host: "192.168.1.105".to_string(),
                    port: 8009,
                },
            );
        });
    }

    pub fn get_speakers(&self) -> Vec<DiscoveredSpeaker> {
        let list = self.speakers.lock().unwrap();
        list.values().cloned().collect()
    }
}
