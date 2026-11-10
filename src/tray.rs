use slint::ComponentHandle;
use tracing::info;
use zbus::{connection::Builder, interface};

pub struct TrayService {
    window_handle: slint::Weak<crate::AppWindow>,
}

impl TrayService {
    pub fn new(window_handle: slint::Weak<crate::AppWindow>) -> Self {
        Self { window_handle }
    }
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl TrayService {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "spotify-light"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "spotify-light"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "spotify-light"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> &str {
        "/usr/share/icons/hicolor/256x256/apps"
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (&str, Vec<(i32, i32, Vec<u8>)>, &str, &str) {
        ("spotify-light", Vec::new(), "spotify-light", "spotify-light")
    }

    fn activate(&self, _x: i32, _y: i32) {
        info!("[TRAY ACTION] Left click on tray: toggle minimize / restore window");
        let w = self.window_handle.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = w.upgrade() {
                let win = ui.window();
                let is_min = win.is_minimized();
                win.set_minimized(!is_min);
                if is_min {
                    let _ = win.show();
                }
            }
        });
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        self.activate(_x, _y);
    }

    fn context_menu(&self, _x: i32, _y: i32) {
        info!("[TRAY ACTION] Right click on tray (Exit requested). Quitting spotify-light...");
        std::process::exit(0);
    }

    fn scroll(&self, _delta: i32, _orientation: &str) {}
}

pub async fn start_tray(window_handle: slint::Weak<crate::AppWindow>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = TrayService::new(window_handle);
    let pid = std::process::id();
    let service_name = format!("org.kde.StatusNotifierItem-{}-1", pid);

    info!("[TRAY] Hosting org.kde.StatusNotifierItem on D-Bus as '{}'...", service_name);

    let connection = Builder::session()?
        .name(service_name.clone())?
        .serve_at("/StatusNotifierItem", service)?
        .build()
        .await?;

    info!("[TRAY] StatusNotifierItem successfully hosted at /StatusNotifierItem");

    let watcher_proxy = zbus::Proxy::new(
        &connection,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    ).await;

    if let Ok(proxy) = watcher_proxy {
        let res: Result<(), _> = proxy.call("RegisterStatusNotifierItem", &service_name).await;
        match res {
            Ok(_) => info!("[TRAY SUCCESS] Successfully registered StatusNotifierItem with desktop watcher!"),
            Err(e) => info!("[TRAY] Note: desktop watcher response: {}", e),
        }
    }

    Ok(())
}
