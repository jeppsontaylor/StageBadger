use crate::ffmpeg::now_millis;
use crate::types::{
    DestinationTestResult, ManualDestination, ManualDestinationSaveRequest, ManualDestinationTestInput,
    ManualDestinationTestRequest, SecretWriteRequest,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::Manager;

pub const YOUTUBE_LIVE_CONTROL_ROOM_URL: &str = "https://studio.youtube.com/";
pub const YOUTUBE_RTMPS_SERVER: &str = "rtmps://a.rtmps.youtube.com/live2/";
const KEYCHAIN_SERVICE: &str = "com.jeppsontaylor.stagebadger.rtmp";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ManualDestinationStore {
    destinations: Vec<StoredManualDestination>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct StoredManualDestination {
    id: String,
    label: String,
    provider: String,
    server_url: String,
    last_used_at: Option<u64>,
    default_privacy_note: Option<String>,
    confirmed_live_enabled: bool,
}

pub trait SecretStore {
    fn save(&self, request: &SecretWriteRequest) -> Result<(), String>;
    fn load(&self, destination_id: &str) -> Result<Option<String>, String>;
    fn delete(&self, destination_id: &str) -> Result<(), String>;
}

pub struct MacOsSecurityKeychain;

impl SecretStore for MacOsSecurityKeychain {
    fn save(&self, request: &SecretWriteRequest) -> Result<(), String> {
        let stream_key = validate_stream_key(&request.stream_key)?;
        let output = std::process::Command::new("security")
            .args([
                "add-generic-password",
                "-a",
                request.destination_id.as_str(),
                "-s",
                KEYCHAIN_SERVICE,
                "-w",
                stream_key.as_str(),
                "-U",
            ])
            .output()
            .map_err(|e| format!("Keychain save failed: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "Keychain save failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    fn load(&self, destination_id: &str) -> Result<Option<String>, String> {
        let output = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-a",
                destination_id,
                "-s",
                KEYCHAIN_SERVICE,
                "-w",
            ])
            .output()
            .map_err(|e| format!("Keychain lookup failed: {}", e))?;

        if !output.status.success() {
            return Ok(None);
        }

        Ok(String::from_utf8(output.stdout)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()))
    }

    fn delete(&self, destination_id: &str) -> Result<(), String> {
        let _ = std::process::Command::new("security")
            .args(["delete-generic-password", "-a", destination_id, "-s", KEYCHAIN_SERVICE])
            .output()
            .map_err(|e| format!("Keychain delete failed: {}", e))?;
        Ok(())
    }
}

pub fn destinations_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("App data directory unavailable: {}", e))?;
    std::fs::create_dir_all(&directory).map_err(|e| format!("Failed to create app data directory: {}", e))?;
    Ok(directory.join("manual-destinations.json"))
}

pub fn normalize_server_url(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("Enter an RTMP or RTMPS server URL.".to_string());
    }

    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("rtmp://") && !lower.starts_with("rtmps://") {
        return Err("Server URL must start with rtmp:// or rtmps://.".to_string());
    }

    if trimmed.chars().any(char::is_whitespace) {
        return Err("Server URL cannot contain spaces.".to_string());
    }

    Ok(if trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{}/", trimmed)
    })
}

pub fn validate_stream_key(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("Enter a stream key before saving this destination.".to_string());
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("Stream key cannot contain spaces.".to_string());
    }
    Ok(trimmed.to_string())
}

pub fn join_server_and_key(server_url: &str, stream_key: &str) -> Result<String, String> {
    let server = normalize_server_url(server_url)?;
    let key = validate_stream_key(stream_key)?;
    Ok(format!("{}{}", server, key))
}

pub fn redact_stream_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "[redacted]".to_string();
    }
    if trimmed.len() <= 8 {
        return "[redacted]".to_string();
    }
    format!("{}...[redacted]", &trimmed[..4])
}

pub fn redacted_destination_url(server_url: &str, stream_key: &str) -> Result<String, String> {
    Ok(format!(
        "{}{}",
        normalize_server_url(server_url)?,
        redact_stream_key(stream_key)
    ))
}

pub fn load_destinations(path: &Path, secret_store: &dyn SecretStore) -> Result<Vec<ManualDestination>, String> {
    let store = read_store(path)?;
    let mut destinations: Vec<_> = store
        .destinations
        .into_iter()
        .map(|destination| destination.to_public(secret_store))
        .collect::<Result<Vec<_>, _>>()?;
    destinations.sort_by(|a, b| b.last_used_at.cmp(&a.last_used_at).then_with(|| a.label.cmp(&b.label)));
    Ok(destinations)
}

pub fn save_destination(
    path: &Path,
    secret_store: &dyn SecretStore,
    request: ManualDestinationSaveRequest,
) -> Result<ManualDestination, String> {
    let mut store = read_store(path)?;
    let provider = sanitize_provider(&request.provider);
    let id = request
        .id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(sanitize_id)
        .unwrap_or_else(|| format!("{}-{}", provider, now_millis()));
    let server_url = normalize_server_url(&request.server_url)?;
    let existing = store
        .destinations
        .iter()
        .find(|destination| destination.id == id)
        .cloned();

    if let Some(stream_key) = request.stream_key.as_deref().filter(|value| !value.trim().is_empty()) {
        secret_store.save(&SecretWriteRequest {
            destination_id: id.clone(),
            stream_key: validate_stream_key(stream_key)?,
        })?;
    } else if existing.is_none() || secret_store.load(&id)?.is_none() {
        return Err("Enter a stream key before saving this destination.".to_string());
    }

    let label = request
        .label
        .trim()
        .to_string()
        .if_empty_else(|| default_label(&provider).to_string());
    let now = now_millis();
    let stored = StoredManualDestination {
        id: id.clone(),
        label,
        provider,
        server_url,
        last_used_at: Some(now),
        default_privacy_note: request.default_privacy_note.filter(|value| !value.trim().is_empty()),
        confirmed_live_enabled: request.confirmed_live_enabled,
    };

    if let Some(existing) = store.destinations.iter_mut().find(|destination| destination.id == id) {
        *existing = stored.clone();
    } else {
        store.destinations.push(stored.clone());
    }
    write_store(path, &store)?;
    stored.to_public(secret_store)
}

pub fn delete_destination(path: &Path, secret_store: &dyn SecretStore, id: &str) -> Result<(), String> {
    let mut store = read_store(path)?;
    store.destinations.retain(|destination| destination.id != id);
    write_store(path, &store)?;
    secret_store.delete(id)
}

pub fn find_destination(path: &Path, id: &str) -> Result<Option<ManualDestination>, String> {
    let store = read_store(path)?;
    Ok(store
        .destinations
        .into_iter()
        .find(|destination| destination.id == id)
        .map(|destination| ManualDestination {
            id: destination.id,
            label: destination.label,
            provider: destination.provider,
            server_url: destination.server_url,
            has_saved_key: false,
            last_used_at: destination.last_used_at,
            default_privacy_note: destination.default_privacy_note,
            confirmed_live_enabled: destination.confirmed_live_enabled,
        }))
}

pub fn mark_destination_used(path: &Path, id: &str) -> Result<(), String> {
    let mut store = read_store(path)?;
    if let Some(destination) = store.destinations.iter_mut().find(|destination| destination.id == id) {
        destination.last_used_at = Some(now_millis());
        write_store(path, &store)?;
    }
    Ok(())
}

pub fn test_inline_destination(destination: ManualDestinationTestInput) -> DestinationTestResult {
    let server_url = match normalize_server_url(&destination.server_url) {
        Ok(server_url) => server_url,
        Err(message) => return DestinationTestResult::failed(message),
    };

    let stream_key = match destination.stream_key.as_deref() {
        Some(value) => match validate_stream_key(value) {
            Ok(key) => key,
            Err(message) => return DestinationTestResult::failed(message),
        },
        None => {
            return DestinationTestResult::failed("Enter a stream key before testing this destination.".to_string())
        }
    };

    DestinationTestResult {
        ok: true,
        normalized_server_url: Some(server_url.clone()),
        redacted_url: redacted_destination_url(&server_url, &stream_key).ok(),
        message: "Destination details are valid locally.".to_string(),
    }
}

pub fn test_saved_destination(
    path: &Path,
    secret_store: &dyn SecretStore,
    destination_id: &str,
) -> Result<DestinationTestResult, String> {
    let Some(destination) = find_destination(path, destination_id)? else {
        return Ok(DestinationTestResult::failed(
            "Saved destination was not found.".to_string(),
        ));
    };
    let Some(stream_key) = secret_store.load(destination_id)? else {
        return Ok(DestinationTestResult::failed(
            "Saved destination is missing its keychain stream key.".to_string(),
        ));
    };

    Ok(test_inline_destination(ManualDestinationTestInput {
        server_url: destination.server_url,
        stream_key: Some(stream_key),
    }))
}

pub fn test_rtmp_destination(
    path: &Path,
    secret_store: &dyn SecretStore,
    request: ManualDestinationTestRequest,
) -> Result<DestinationTestResult, String> {
    if let Some(destination_id) = request.destination_id {
        return test_saved_destination(path, secret_store, &destination_id);
    }

    if let Some(inline_destination) = request.inline_destination {
        return Ok(test_inline_destination(inline_destination));
    }

    Ok(DestinationTestResult::failed(
        "Provide either a saved destination id or inline destination details.".to_string(),
    ))
}

fn read_store(path: &Path) -> Result<ManualDestinationStore, String> {
    if !path.exists() {
        return Ok(ManualDestinationStore::default());
    }

    let content = std::fs::read_to_string(path).map_err(|e| format!("Failed to read destinations: {}", e))?;
    serde_json::from_str(&content).map_err(|e| format!("Failed to parse destinations: {}", e))
}

fn write_store(path: &Path, store: &ManualDestinationStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create destinations directory: {}", e))?;
    }
    let content =
        serde_json::to_string_pretty(store).map_err(|e| format!("Failed to serialize destinations: {}", e))?;
    std::fs::write(path, content).map_err(|e| format!("Failed to save destinations: {}", e))
}

fn sanitize_id(value: &str) -> String {
    let sanitized: String = value
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if sanitized.is_empty() {
        format!("destination-{}", now_millis())
    } else {
        sanitized
    }
}

fn sanitize_provider(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "youtube" => "youtube".to_string(),
        "custom" | "rtmp" | "manual" => "custom".to_string(),
        _ => "custom".to_string(),
    }
}

fn default_label(provider: &str) -> &str {
    if provider == "youtube" {
        "YouTube RTMPS"
    } else {
        "Custom RTMP"
    }
}

trait IfEmpty {
    fn if_empty_else(self, fallback: impl FnOnce() -> String) -> String;
}

impl IfEmpty for String {
    fn if_empty_else(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() {
            fallback()
        } else {
            self
        }
    }
}

impl StoredManualDestination {
    fn to_public(self, secret_store: &dyn SecretStore) -> Result<ManualDestination, String> {
        Ok(ManualDestination {
            id: self.id.clone(),
            label: self.label,
            provider: self.provider,
            server_url: self.server_url,
            has_saved_key: secret_store.load(&self.id)?.is_some(),
            last_used_at: self.last_used_at,
            default_privacy_note: self.default_privacy_note,
            confirmed_live_enabled: self.confirmed_live_enabled,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemorySecrets(Mutex<HashMap<String, String>>);

    impl SecretStore for MemorySecrets {
        fn save(&self, request: &SecretWriteRequest) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .insert(request.destination_id.clone(), request.stream_key.clone());
            Ok(())
        }

        fn load(&self, destination_id: &str) -> Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().get(destination_id).cloned())
        }

        fn delete(&self, destination_id: &str) -> Result<(), String> {
            self.0.lock().unwrap().remove(destination_id);
            Ok(())
        }
    }

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("stagebadger-{}-{}.json", name, now_millis()))
    }

    #[test]
    fn normalizes_rtmp_server_urls() {
        assert_eq!(
            normalize_server_url(" rtmps://a.rtmps.youtube.com/live2 ").unwrap(),
            YOUTUBE_RTMPS_SERVER
        );
        assert!(normalize_server_url("https://example.com/live").is_err());
    }

    #[test]
    fn keychain_wrapper_can_be_mocked_for_save_load_delete() {
        let secrets = MemorySecrets::default();
        let request = SecretWriteRequest {
            destination_id: "youtube-1".to_string(),
            stream_key: "abcd-1234".to_string(),
        };

        secrets.save(&request).unwrap();
        assert_eq!(secrets.load("youtube-1").unwrap(), Some("abcd-1234".to_string()));
        secrets.delete("youtube-1").unwrap();
        assert_eq!(secrets.load("youtube-1").unwrap(), None);
    }

    #[test]
    fn save_and_load_manual_destination_keeps_secret_out_of_config() {
        let path = test_path("save-load");
        let secrets = MemorySecrets::default();
        let saved = save_destination(
            &path,
            &secrets,
            ManualDestinationSaveRequest {
                id: Some("youtube-1".to_string()),
                label: "YouTube RTMPS".to_string(),
                provider: "youtube".to_string(),
                server_url: "rtmps://a.rtmps.youtube.com/live2".to_string(),
                stream_key: Some("abcd-1234".to_string()),
                default_privacy_note: Some("Confirm privacy in YouTube Studio".to_string()),
                confirmed_live_enabled: true,
            },
        )
        .unwrap();

        assert!(saved.has_saved_key);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("abcd-1234"));
        let loaded = load_destinations(&path, &secrets).unwrap();
        assert_eq!(loaded[0].server_url, YOUTUBE_RTMPS_SERVER);
        assert!(loaded[0].has_saved_key);
    }

    #[test]
    fn destination_validation_rejects_empty_key_and_invalid_url() {
        let empty_key = test_inline_destination(ManualDestinationTestInput {
            server_url: YOUTUBE_RTMPS_SERVER.to_string(),
            stream_key: Some(" ".to_string()),
        });
        assert!(!empty_key.ok);

        let invalid_url = test_inline_destination(ManualDestinationTestInput {
            server_url: "https://youtube.com/live".to_string(),
            stream_key: Some("abcd-1234".to_string()),
        });
        assert!(!invalid_url.ok);
    }

    #[test]
    fn redacted_result_never_contains_stream_key() {
        let result = test_inline_destination(ManualDestinationTestInput {
            server_url: YOUTUBE_RTMPS_SERVER.to_string(),
            stream_key: Some("super-secret-key".to_string()),
        });

        assert!(result.ok);
        let redacted_url = result.redacted_url.unwrap();
        assert!(!redacted_url.contains("super-secret-key"));
        assert!(redacted_url.contains("[redacted]"));
    }
}
