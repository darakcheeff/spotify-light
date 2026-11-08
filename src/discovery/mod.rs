use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tracing::{info, warn};

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
        info!("Starting genuine mDNS speaker discovery for Spotify Connect and Google Cast...");
        let speakers = Arc::clone(&self.speakers);

        tokio::task::spawn_blocking(move || {
            let mdns = match ServiceDaemon::new() {
                Ok(daemon) => daemon,
                Err(e) => {
                    warn!("Failed to initialize mDNS daemon: {}", e);
                    return;
                }
            };

            let services = ["_googlecast._tcp.local.", "_spotify-connect._tcp.local."];
            for service in services {
                match mdns.browse(service) {
                    Ok(receiver) => {
                        let speakers_inner = Arc::clone(&speakers);
                        std::thread::spawn(move || {
                            while let Ok(event) = receiver.recv() {
                                if let ServiceEvent::ServiceResolved(info) = event {
                                    let id = info.get_fullname().to_string();
                                    let name = info.get_property_val_str("fn")
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(|| info.get_fullname().trim_end_matches(".local.").to_string());
                                    let host = info.get_hostname().to_string();
                                    let port = info.get_port();
                                    let s_type = info.get_type().to_string();

                                    info!("Discovered network speaker: {} ({}:{})", name, host, port);

                                    if let Ok(mut map) = speakers_inner.lock() {
                                        map.insert(
                                            id.clone(),
                                            DiscoveredSpeaker {
                                                id,
                                                name,
                                                service_type: s_type,
                                                host,
                                                port,
                                            },
                                        );
                                    }
                                }
                            }
                        });
                    }
                    Err(e) => {
                        warn!("Failed to browse service {}: {}", service, e);
                    }
                }
            }
        });
    }

    pub fn get_speakers(&self) -> Vec<DiscoveredSpeaker> {
        match self.speakers.lock() {
            Ok(list) => list.values().cloned().collect(),
            Err(poisoned) => poisoned.into_inner().values().cloned().collect(),
        }
    }
}
