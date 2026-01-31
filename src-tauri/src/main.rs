#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod llm;
mod oauth;
mod session;
mod tools;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
// 

pub use oauth::{OAuthToken, load_token, save_token, delete_token, is_logged_in};
pub use session::{Message, Session, SessionManager};

#[derive(Serialize)]
struct AppInfo {
    version: String,
    platform: String,
    arch: String,
}

#[derive(Serialize)]
struct AppPaths {
    config: String,
    mcp: String,
    gui: String,
    work_dir: String,
    share_dir: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct GuiSettings {
    work_dir: Option<String>,
    config_file: Option<String>,
    mcp_config_files: Vec<String>,
    skills_dir: Option<String>,
    model: Option<String>,
    thinking: Option<bool>,
    yolo: Option<bool>,
    pinned_sessions: Vec<String>,
}

#[derive(Clone, Serialize)]
struct GuiSettingsPayload {
    path: String,
    settings: GuiSettings,
}

#[derive(Clone, Serialize)]
struct SkillInfo {
    name: String,
    description: Option<String>,
    path: String,
    root: String,
}

#[derive(Clone, Serialize)]
struct SkillsPayload {
    roots: Vec<String>,
    skills: Vec<SkillInfo>,
}

#[derive(Clone, Serialize)]
struct SessionInfo {
    id: String,
    title: String,
    updated_at: f64,
    work_dir: String,
}

#[derive(Clone, Serialize)]
struct AuthStatus {
    is_logged_in: bool,
    user: Option<String>,
    mode: String, // "oauth" | "api_key" | "none"
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthConfig {
    pub mode: String, // "oauth" | "api_key"
    pub api_key: Option<String>,
    pub api_base: Option<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: "oauth".to_string(),
            api_key: None,
            api_base: None,
        }
    }
}

fn auth_config_path() -> PathBuf {
    kimi_share_dir().join("gui_auth.json")
}

fn load_auth_config() -> AuthConfig {
    let path = auth_config_path();
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(config) = serde_json::from_str::<AuthConfig>(&content) {
            return config;
        }
    }
    AuthConfig::default()
}

fn save_auth_config(config: &AuthConfig) -> Result<(), String> {
    let path = auth_config_path();
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize auth config: {}", e))?;
    fs::write(&path, json)
        .map_err(|e| format!("Failed to write auth config: {}", e))?;
    Ok(())
}

#[tauri::command]
fn auth_get_config() -> AuthConfig {
    load_auth_config()
}

#[tauri::command]
fn auth_set_config(config: AuthConfig) -> Result<(), String> {
    save_auth_config(&config)
}

#[tauri::command]
fn auth_set_api_key(api_key: String, api_base: Option<String>) -> Result<(), String> {
    let config = AuthConfig {
        mode: "api_key".to_string(),
        api_key: Some(api_key),
        api_base: api_base.filter(|b| !b.is_empty()),
    };
    save_auth_config(&config)
}

#[tauri::command]
fn auth_clear() -> Result<(), String> {
    // Clear OAuth token
    let _ = oauth::delete_token();
    // Clear API key config
    let path = auth_config_path();
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    Ok(())
}

struct AppState {
    sessions: Mutex<HashMap<u64, SessionHandle>>,
    next_id: AtomicU64,
    session_manager: Mutex<SessionManager>,
    approvals: Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
}

struct SessionHandle {
    cancel_tx: tokio::sync::oneshot::Sender<()>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            session_manager: Mutex::new(SessionManager::new()),
            approvals: Mutex::new(HashMap::new()),
        }
    }
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn kimi_share_dir() -> PathBuf {
    home_dir().join(".kimi")
}

fn default_config_path() -> PathBuf {
    kimi_share_dir().join("config.toml")
}

fn default_mcp_path() -> PathBuf {
    kimi_share_dir().join("mcp.json")
}

fn default_gui_path() -> PathBuf {
    kimi_share_dir().join("gui.json")
}

fn metadata_path() -> PathBuf {
    kimi_share_dir().join("kimi.json")
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create directory {parent:?}: {error}"))?;
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("Failed to read {path:?}: {error}"))
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    ensure_parent(path)?;
    fs::write(path, content).map_err(|error| format!("Failed to write {path:?}: {error}"))
}

fn default_config_data() -> serde_json::Value {
    serde_json::json!({
        "default_model": "",
        "default_thinking": false,
        "models": {},
        "providers": {},
        "loop_control": {
            "max_steps_per_turn": 100,
            "max_retries_per_step": 3,
            "max_ralph_iterations": 0,
            "reserved_context_size": 50000
        },
        "services": {},
        "mcp": {
            "client": {
                "tool_call_timeout_ms": 60000
            }
        }
    })
}

fn strip_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map
                .iter()
                .filter_map(|(key, value)| value.is_null().then(|| key.clone()))
                .collect();
            for key in keys {
                map.remove(&key);
            }
            for value in map.values_mut() {
                strip_nulls(value);
            }
        }
        serde_json::Value::Array(list) => {
            list.retain(|value| !value.is_null());
            for value in list.iter_mut() {
                strip_nulls(value);
            }
        }
        _ => {}
    }
}

fn parse_config_content(path: &Path, raw: &str) -> Result<serde_json::Value, String> {
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        serde_json::from_str(raw)
            .map_err(|error| format!("Invalid JSON in {path:?}: {error}"))
    } else {
        let value: toml::Value =
            toml::from_str(raw).map_err(|error| format!("Invalid TOML in {path:?}: {error}"))?;
        serde_json::to_value(value)
            .map_err(|error| format!("Failed to convert TOML to JSON: {error}"))
    }
}

fn encode_config_content(path: &Path, data: &serde_json::Value) -> Result<String, String> {
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        serde_json::to_string_pretty(data)
            .map_err(|error| format!("Failed to encode JSON: {error}"))
    } else {
        toml::to_string(data).map_err(|error| format!("Failed to encode TOML: {error}"))
    }
}

fn find_repo_root() -> Option<PathBuf> {
    let mut current = std::env::current_dir().ok()?;
    loop {
        if current.join("pyproject.toml").is_file() && current.join("src/kimi_cli").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn skills_root_candidates(work_dir: &Path) -> Vec<PathBuf> {
    let home = home_dir();
    vec![
        home.join(".config/agents/skills"),
        home.join(".agents/skills"),
        home.join(".kimi/skills"),
        home.join(".claude/skills"),
        home.join(".codex/skills"),
        work_dir.join(".agents/skills"),
        work_dir.join(".kimi/skills"),
        work_dir.join(".claude/skills"),
        work_dir.join(".codex/skills"),
    ]
}

fn parse_skill_frontmatter(contents: &str) -> (Option<String>, Option<String>) {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }

    let mut name = None;
    let mut description = None;

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            match key.trim() {
                "name" => {
                    if !value.is_empty() {
                        name = Some(value.to_string());
                    }
                }
                "description" => {
                    if !value.is_empty() {
                        description = Some(value.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    (name, description)
}

fn truncate_with_ellipsis(input: &str, max_chars: usize) -> String {
    let total = input.chars().count();
    if total <= max_chars {
        return input.to_string();
    }
    if max_chars <= 3 {
        return input.chars().take(max_chars).collect();
    }
    let prefix: String = input.chars().take(max_chars - 3).collect();
    format!("{prefix}...")
}

fn collect_skills(root: &Path) -> Vec<SkillInfo> {
    let mut skills = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return skills,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_file = path.join("SKILL.md");
        if !skill_file.is_file() {
            continue;
        }
        let contents = fs::read_to_string(&skill_file).unwrap_or_default();
        let (name, description) = parse_skill_frontmatter(&contents);
        let fallback_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("skill")
            .to_string();
        skills.push(SkillInfo {
            name: name.unwrap_or(fallback_name),
            description,
            path: skill_file.to_string_lossy().to_string(),
            root: root.to_string_lossy().to_string(),
        });
    }

    skills
}

fn load_sessions(work_dir: &str) -> Result<Vec<SessionInfo>, String> {
    let meta_path = metadata_path();
    if !meta_path.exists() {
        return Ok(Vec::new());
    }

    let raw = read_text(&meta_path)?;
    let data: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse metadata: {e}"))?;

    let empty_vec = Vec::new();
    let work_dirs = data.get("work_dirs").and_then(|v| v.as_array()).unwrap_or(&empty_vec);

    for wd in work_dirs {
        let path = wd.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if path == work_dir {
            let kaos = wd.get("kaos").and_then(|v| v.as_str()).unwrap_or("local");
            let sessions_dir = get_session_dir(path, kaos)?;
            
            let mut sessions = Vec::new();
            if let Ok(entries) = fs::read_dir(&sessions_dir) {
                for entry in entries.flatten() {
                    let session_path = entry.path();
                    if !session_path.is_dir() {
                        continue;
                    }

                    let session_id = session_path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();

                    let wire_file = session_path.join("wire.jsonl");

                    // Only show sessions that have wire.jsonl with actual content
                    if !wire_file.exists() {
                        continue;
                    }

                    // Check if wire.jsonl has content (more than just metadata line)
                    let wire_size = wire_file.metadata().map(|m| m.len()).unwrap_or(0);
                    if wire_size < 100 {
                        continue;
                    }

                    let updated_at = wire_file.metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);

                    let title = extract_session_title(&wire_file).unwrap_or_else(|| {
                        format!("Session {}", &session_id[..8.min(session_id.len())])
                    });

                    sessions.push(SessionInfo {
                        id: session_id,
                        title,
                        updated_at,
                        work_dir: path.to_string(),
                    });
                }
            }

            sessions.sort_by(|a, b| b.updated_at.partial_cmp(&a.updated_at).unwrap());
            return Ok(sessions);
        }
    }
    
    Ok(Vec::new())
}

fn get_session_dir(work_dir: &str, kaos: &str) -> Result<PathBuf, String> {
    use md5::{Md5, Digest};

    let mut hasher = Md5::new();
    hasher.update(work_dir.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    
    let dir_name = if kaos == "local" {
        hash
    } else {
        format!("{}_{}", kaos, hash)
    };
    
    let session_dir = kimi_share_dir().join("sessions").join(dir_name);
    Ok(session_dir)
}

fn extract_session_title(wire_file: &Path) -> Option<String> {
    if !wire_file.exists() {
        return None;
    }

    let content = fs::read_to_string(wire_file).ok()?;

    for line in content.lines().take(50) {
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
            // Handle nested message format: {"message": {"type": "TurnBegin", "payload": {"user_input": [...]}}}
            let msg_type = record.get("message")
                .and_then(|m| m.get("type"))
                .and_then(|v| v.as_str());

            if msg_type == Some("TurnBegin") {
                // user_input is an array of objects with "type" and "text" fields
                if let Some(user_input) = record.get("message")
                    .and_then(|m| m.get("payload"))
                    .and_then(|p| p.get("user_input"))
                    .and_then(|u| u.as_array())
                {
                    for input_item in user_input {
                        if let Some(text) = input_item.get("text").and_then(|t| t.as_str()) {
                            let title = truncate_with_ellipsis(text, 50);
                            return Some(title);
                        }
                    }
                }
            }
        }
    }

    None
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        platform: match std::env::consts::OS {
            "macos" => "macOS",
            "windows" => "Windows",
            "linux" => "Linux",
            other => other,
        }
        .to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

#[tauri::command]
fn app_paths() -> AppPaths {
    // Check for KIMI_GUI_WORK_DIR env var first, then PWD (original shell cwd), then find_repo_root, then current_dir
    let work_dir = std::env::var("KIMI_GUI_WORK_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("PWD").ok().map(PathBuf::from))
        .or_else(|| find_repo_root())
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });

    AppPaths {
        config: default_config_path().to_string_lossy().to_string(),
        mcp: default_mcp_path().to_string_lossy().to_string(),
        gui: default_gui_path().to_string_lossy().to_string(),
        work_dir: work_dir.to_string_lossy().to_string(),
        share_dir: kimi_share_dir().to_string_lossy().to_string(),
    }
}

#[tauri::command]
fn config_load(path: Option<String>) -> Result<session::ConfigPayload, String> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);

    if !path.exists() {
        let data = default_config_data();
        let mut clean = data.clone();
        strip_nulls(&mut clean);
        let raw = encode_config_content(&path, &clean)?;
        write_text(&path, &raw)?;
    }

    let raw = read_text(&path)?;
    let data = parse_config_content(&path, &raw)?;

    Ok(session::ConfigPayload {
        path: path.to_string_lossy().to_string(),
        raw,
        data,
    })
}

#[tauri::command]
fn config_save(path: Option<String>, data: serde_json::Value) -> Result<(), String> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let mut clean = data.clone();
    strip_nulls(&mut clean);
    let raw = encode_config_content(&path, &clean)?;
    write_text(&path, &raw)?;
    Ok(())
}

#[tauri::command]
fn config_save_raw(path: Option<String>, raw: String) -> Result<(), String> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    parse_config_content(&path, &raw)?;
    write_text(&path, &raw)?;
    Ok(())
}

#[tauri::command]
fn mcp_load(path: Option<String>) -> Result<session::McpPayload, String> {
    let path = path.map(PathBuf::from).unwrap_or_else(default_mcp_path);
    if !path.exists() {
        let raw = serde_json::json!({ "mcpServers": {} });
        let content =
            serde_json::to_string_pretty(&raw).map_err(|error| error.to_string())?;
        write_text(&path, &content)?;
    }
    let raw = read_text(&path)?;
    let data: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("Invalid MCP JSON: {error}"))?;

    Ok(session::McpPayload {
        path: path.to_string_lossy().to_string(),
        raw,
        data,
    })
}

#[tauri::command]
fn mcp_save(path: Option<String>, data: serde_json::Value) -> Result<(), String> {
    let path = path.map(PathBuf::from).unwrap_or_else(default_mcp_path);
    let raw = serde_json::to_string_pretty(&data).map_err(|error| error.to_string())?;
    write_text(&path, &raw)?;
    Ok(())
}

#[tauri::command]
fn mcp_save_raw(path: Option<String>, raw: String) -> Result<(), String> {
    let path = path.map(PathBuf::from).unwrap_or_else(default_mcp_path);
    let _: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("Invalid MCP JSON: {error}"))?;
    write_text(&path, &raw)?;
    Ok(())
}

#[tauri::command]
fn gui_settings_load(path: Option<String>) -> Result<GuiSettingsPayload, String> {
    let path = path.map(PathBuf::from).unwrap_or_else(default_gui_path);
    if !path.exists() {
        return Ok(GuiSettingsPayload {
            path: path.to_string_lossy().to_string(),
            settings: GuiSettings::default(),
        });
    }
    let raw = read_text(&path)?;
    let settings: GuiSettings =
        serde_json::from_str(&raw).map_err(|error| format!("Invalid GUI settings: {error}"))?;
    Ok(GuiSettingsPayload {
        path: path.to_string_lossy().to_string(),
        settings,
    })
}

#[tauri::command]
fn gui_settings_save(path: Option<String>, settings: GuiSettings) -> Result<(), String> {
    let path = path.map(PathBuf::from).unwrap_or_else(default_gui_path);
    let raw = serde_json::to_string_pretty(&settings).map_err(|error| error.to_string())?;
    write_text(&path, &raw)?;
    Ok(())
}

#[tauri::command]
fn skills_list(work_dir: Option<String>, skills_dir: Option<String>) -> Result<SkillsPayload, String> {
    let work_dir = work_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| find_repo_root().unwrap_or_else(|| PathBuf::from(".")));

    let mut roots = Vec::new();
    if let Some(skills_dir) = skills_dir {
        let root = PathBuf::from(skills_dir);
        if root.is_dir() {
            roots.push(root);
        }
    } else {
        for root in skills_root_candidates(&work_dir) {
            if root.is_dir() {
                roots.push(root);
            }
        }
    }

    let mut seen = HashMap::new();
    let mut skills = Vec::new();
    for root in &roots {
        for skill in collect_skills(root) {
            let key = skill.name.to_lowercase();
            if !seen.contains_key(&key) {
                seen.insert(key, true);
                skills.push(skill);
            }
        }
    }

    Ok(SkillsPayload {
        roots: roots
            .into_iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect(),
        skills,
    })
}

#[tauri::command]
fn session_list(
    state: tauri::State<'_, AppState>,
    work_dir: Option<String>
) -> Result<Vec<SessionInfo>, String> {
    let mut sessions = Vec::new();
    
    // Load CLI sessions if work_dir is provided
    if let Some(ref wd) = work_dir {
        sessions = load_sessions(wd)?;
    }
    
    // Also load GUI sessions from SessionManager
    let mut manager = state.session_manager.lock()
        .map_err(|_| "Session manager poisoned".to_string())?;
    
    if let Ok(gui_sessions) = manager.load_all_sessions() {
        for session in &gui_sessions {
            let include = if let Some(ref wd) = work_dir {
                // Normalize paths for comparison
                let session_path = Path::new(&session.work_dir).canonicalize().ok().unwrap_or_else(|| Path::new(&session.work_dir).to_path_buf());
                let work_path = Path::new(wd).canonicalize().ok().unwrap_or_else(|| Path::new(wd).to_path_buf());
                session_path == work_path || session.work_dir == *wd
            } else {
                // If no work_dir filter, include all sessions
                true
            };
            
            if include {
                sessions.push(SessionInfo {
                    id: session.id.clone(),
                    title: session.title.clone(),
                    updated_at: session.updated_at as f64,
                    work_dir: session.work_dir.clone(),
                });
            }
        }
    }
    
    // Sort by updated_at descending
    sessions.sort_by(|a, b| b.updated_at.partial_cmp(&a.updated_at).unwrap());
    
    // Remove duplicates (same id)
    let mut seen = HashMap::new();
    let mut unique = Vec::new();
    for s in sessions {
        if !seen.contains_key(&s.id) {
            seen.insert(s.id.clone(), true);
            unique.push(s);
        }
    }
    
    Ok(unique)
}

#[tauri::command]
fn auth_check_status() -> Result<AuthStatus, String> {
    // Check OAuth
    let oauth_logged_in = oauth::is_logged_in();
    
    // Check API Key
    let config = load_auth_config();
    let api_key_valid = config.mode == "api_key" && config.api_key.as_ref().map(|k| !k.is_empty()).unwrap_or(false);
    
    let is_logged_in = oauth_logged_in || api_key_valid;
    let mode = if oauth_logged_in {
        "oauth"
    } else if api_key_valid {
        "api_key"
    } else {
        "none"
    };
    
    Ok(AuthStatus {
        is_logged_in,
        user: if is_logged_in { Some("User".to_string()) } else { None },
        mode: mode.to_string(),
    })
}

#[tauri::command]
fn session_messages(
    state: tauri::State<'_, AppState>,
    work_dir: String, 
    session_id: String
) -> Result<Vec<Message>, String> {
    // First try GUI sessions from memory (most common case)
    {
        let manager = state.session_manager.lock()
            .map_err(|_| "Session manager poisoned".to_string())?;
        
        if let Some(session) = manager.sessions.get(&session_id) {
            return Ok(session.messages.clone());
        }
    }
    
    // Try loading from disk
    {
        let mut manager = state.session_manager.lock()
            .map_err(|_| "Session manager poisoned".to_string())?;
        
        match manager.load_all_sessions() {
            Ok(sessions) => {
                for session in sessions {
                    if session.id == session_id {
                        return Ok(session.messages);
                    }
                }
            }
            Err(_) => {}
        }
    }
    
    // Finally try CLI sessions (from wire files)
    {
        let manager = state.session_manager.lock()
            .map_err(|_| "Session manager poisoned".to_string())?;
        
        match manager.load_messages(&work_dir, &session_id) {
            Ok(messages) => {
                if !messages.is_empty() {
                    return Ok(messages);
                }
            }
            Err(_) => {}
        }
    }
    
    Ok(Vec::new())
}

#[tauri::command]
fn session_save_message(
    state: tauri::State<'_, AppState>,
    session_id: String,
    role: String,
    content: String,
) -> Result<(), String> {
    use crate::session::Message as SessionMessage;
    
    let mut manager = state.session_manager.lock()
        .map_err(|_| "Session manager poisoned".to_string())?;
    
    let message = SessionMessage {
        role: role.clone(),
        content: content.clone(),
        timestamp: chrono::Utc::now().timestamp(),
        tool_calls: None,
    };
    
    // Save to file and add to memory
    match manager.save_message(&session_id, &message) {
        Ok(_) => {}
        Err(_) => {}
    }
    
    match manager.add_message(&session_id, message) {
        Ok(_) => {}
        Err(_) => {}
    }
    Ok(())
}

#[tauri::command]
fn session_delete(
    state: tauri::State<'_, AppState>,
    work_dir: String,
    session_id: String,
) -> Result<(), String> {
    let mut manager = state
        .session_manager
        .lock()
        .map_err(|_| "Session manager poisoned".to_string())?;
    manager.delete_session(&work_dir, &session_id)?;
    Ok(())
}

#[tauri::command]
async fn chat_stream(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    session_id: String,
    message: String,
    settings: Option<GuiSettings>,
) -> Result<(), String> {
    use crate::session::{Message as SessionMessage};
    
    let settings = settings.unwrap_or_default();
    
    let model = settings.model
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "kimi-k2.5".to_string());
    
    let work_dir = settings.work_dir
        .unwrap_or_else(|| app_paths().work_dir);

    let config_path = settings
        .config_file
        .filter(|path| !path.is_empty())
        .or_else(|| Some(app_paths().config));

    let auto_approve = settings.yolo.unwrap_or(false);
    
    // Load auth config
    let auth_config = load_auth_config();
    
    let title = truncate_with_ellipsis(&message, 50);
    
    // Create or get session and save user message
    {
        let mut manager = state.session_manager.lock()
            .map_err(|_| "Session manager poisoned".to_string())?;
        
        // Get or create session
        let _session = manager.get_or_create_session(&session_id, &title, &work_dir);
        
        // Save user message
        let user_msg = SessionMessage {
            role: "user".to_string(),
            content: message.clone(),
            timestamp: chrono::Utc::now().timestamp(),
            tool_calls: None,
        };
        let _ = manager.save_message(&session_id, &user_msg);
        let _ = manager.add_message(&session_id, user_msg);
    }
    
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    
    {
        let mut sessions = state.sessions.lock()
            .map_err(|_| "Session store poisoned".to_string())?;
        let stream_id = state.next_id.fetch_add(1, Ordering::Relaxed);
        sessions.insert(stream_id, SessionHandle { cancel_tx });
    }
    
    let window_clone = window.clone();
    let session_id_clone = session_id.clone();
    
    // Wrap the stream_chat to capture the response
    let result = llm::stream_chat(
        window_clone,
        state.clone(),
        session_id_clone,
        message,
        model,
        work_dir.clone(),
        config_path,
        auto_approve,
        auth_config,
        cancel_rx,
    ).await;
    
    // Note: We can't easily capture the content from stream_chat since it emits to window.
    // For now, sessions will be tracked but full message persistence requires 
    // either a callback mechanism or frontend sending back the complete response.
    
    // Update session timestamp
    {
        let mut manager = state.session_manager.lock()
            .map_err(|_| "Session manager poisoned".to_string())?;
        let now = chrono::Utc::now().timestamp();
        if let Some(session) = manager.sessions.get_mut(&session_id) {
            session.updated_at = now;
            let session_clone = session.clone();
            let _ = manager.save_session(&session_clone);
        }
    }
    
    result
}

#[tauri::command]
fn tool_approval_respond(
    state: tauri::State<'_, AppState>,
    request_id: String,
    approved: bool,
) -> Result<(), String> {
    let mut approvals = state
        .approvals
        .lock()
        .map_err(|_| "Approval store poisoned".to_string())?;
    if let Some(tx) = approvals.remove(&request_id) {
        let _ = tx.send(approved);
        Ok(())
    } else {
        Err("Approval request not found".to_string())
    }
}

#[tauri::command]
fn cancel_chat(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut sessions = state.sessions.lock()
        .map_err(|_| "Session store poisoned".to_string())?;
    
    for (_, handle) in sessions.drain() {
        let _ = handle.cancel_tx.send(());
    }
    
    Ok(())
}

#[tauri::command]
fn list_files(work_dir: String, query: Option<String>) -> Result<Vec<String>, String> {
    let root = Path::new(&work_dir);
    if !root.exists() {
        return Ok(Vec::new());
    }
    
    let mut files = Vec::new();
    let query_lower = query.unwrap_or_default().to_lowercase();
    
    fn is_ignored(name: &str) -> bool {
        let ignored = [
            ".git", ".svn", ".hg", ".DS_Store",
            "node_modules", "target", "dist", "build",
            ".venv", "venv", "__pycache__", ".pytest_cache",
            ".idea", ".vscode", ".next", ".nuxt",
        ];
        ignored.iter().any(|&i| name == i || name.starts_with('.'))
    }
    
    fn walk_dir(path: &Path, root: &Path, files: &mut Vec<String>, query: &str, limit: usize) {
        if files.len() >= limit {
            return;
        }
        
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                if files.len() >= limit {
                    break;
                }
                
                let name = entry.file_name().to_string_lossy().to_string();
                if is_ignored(&name) {
                    continue;
                }
                
                let path = entry.path();
                let rel_path = path.strip_prefix(root).unwrap_or(&path);
                let rel_str = rel_path.to_string_lossy().to_string();
                
                if query.is_empty() || rel_str.to_lowercase().contains(query) {
                    files.push(rel_str);
                }
                
                if path.is_dir() {
                    walk_dir(&path, root, files, query, limit);
                }
            }
        }
    }
    
    walk_dir(root, root, &mut files, &query_lower, 50);
    files.sort();
    Ok(files)
}

#[tauri::command]
fn read_file(work_dir: String, file_path: String) -> Result<String, String> {
    let root = Path::new(&work_dir);
    let full_path = root.join(&file_path);
    
    // Security: ensure the path is within work_dir
    let canonical = full_path.canonicalize()
        .map_err(|e| format!("Failed to resolve path: {}", e))?;
    let canonical_root = root.canonicalize()
        .map_err(|e| format!("Failed to resolve work dir: {}", e))?;
    
    if !canonical.starts_with(&canonical_root) {
        return Err("Path is outside working directory".to_string());
    }
    
    // Limit file size to 100KB
    let metadata = std::fs::metadata(&canonical)
        .map_err(|e| format!("Failed to read file metadata: {}", e))?;
    
    if metadata.len() > 100_000 {
        return Err("File too large (max 100KB)".to_string());
    }
    
    std::fs::read_to_string(&canonical)
        .map_err(|e| format!("Failed to read file: {}", e))
}

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    
    // Use blocking_pick_folder in async context (it runs on main thread)
    let folder = app.dialog().file().blocking_pick_folder();
    
    Ok(folder.map(|p| p.to_string()))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            app_info,
            app_paths,
            config_load,
            config_save,
            config_save_raw,
            mcp_load,
            mcp_save,
            mcp_save_raw,
            gui_settings_load,
            gui_settings_save,
            skills_list,
            session_list,
            auth_check_status,
            auth_get_config,
            auth_set_config,
            auth_set_api_key,
            auth_clear,
            session_messages,
            session_save_message,
            session_delete,
            chat_stream,
            cancel_chat,
            list_files,
            read_file,
            pick_folder,
            tool_approval_respond,
            // OAuth commands
            oauth::oauth_check_status,
            oauth::oauth_logout,
            oauth::oauth_start_login,
            oauth::oauth_open_browser,
            oauth::oauth_get_user,
            // LLM commands
            llm::llm_fetch_models,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
