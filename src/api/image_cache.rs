use std::fs;
use std::path::PathBuf;
use tracing::error;

pub fn get_cache_dir() -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = base.join("spotify-light").join("covers");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn url_to_path(url: &str) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let h = hasher.finish();
    get_cache_dir().join(format!("{:016x}.jpg", h))
}

pub fn get_cached_image_path(url: &str) -> Option<PathBuf> {
    if url.is_empty() {
        return None;
    }
    let path = url_to_path(url);
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

pub async fn load_image_path_async(url: &str) -> Option<PathBuf> {
    if url.is_empty() {
        return None;
    }
    let path = url_to_path(url);
    if path.exists() {
        return Some(path);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;

    match client.get(url).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                if let Ok(bytes) = resp.bytes().await {
                    let _ = fs::write(&path, &bytes);
                    return Some(path);
                }
            }
        }
        Err(e) => {
            error!("[IMAGE ERROR] Failed to fetch image {}: {}", url, e);
        }
    }
    None
}
