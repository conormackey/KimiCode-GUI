use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub timestamp: i64,
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub work_dir: String,
    pub messages: Vec<Message>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct SessionManager {
    pub sessions: HashMap<String, Session>,
    data_dir: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
struct SessionData {
    pub id: String,
    pub title: String,
    pub work_dir: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Serialize)]
pub struct ConfigPayload {
    pub path: String,
    pub raw: String,
    pub data: serde_json::Value,
}

#[derive(Clone, Serialize)]
pub struct McpPayload {
    pub path: String,
    pub raw: String,
    pub data: serde_json::Value,
}

impl SessionManager {
    pub fn new() -> Self {
        let data_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".kimi")
            .join("gui_sessions");
        
        // Ensure directory exists
        fs::create_dir_all(&data_dir).ok();
        
        Self {
            sessions: HashMap::new(),
            data_dir,
        }
    }
    
    fn session_file_path(&self, session_id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.json", session_id))
    }
    
    pub fn save_session(&self, session: &Session) -> Result<(), String> {
        let path = self.session_file_path(&session.id);
        let data = SessionData {
            id: session.id.clone(),
            title: session.title.clone(),
            work_dir: session.work_dir.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
        };
        let json = serde_json::to_string_pretty(&data)
            .map_err(|e| format!("Failed to serialize session: {}", e))?;
        fs::write(&path, json)
            .map_err(|e| format!("Failed to write session file: {}", e))?;
        Ok(())
    }
    
    pub fn add_message(&mut self, session_id: &str, message: Message) -> Result<(), String> {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.messages.push(message);
            session.updated_at = chrono::Utc::now().timestamp();
            let session_clone = session.clone();
            self.save_session(&session_clone)?;
        }
        Ok(())
    }
    
    pub fn load_all_sessions(&mut self) -> Result<Vec<Session>, String> {
        let mut sessions = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.data_dir) {
            let entries: Vec<_> = entries.flatten().collect();
            for entry in entries {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<SessionData>(&content) {
                        // Load messages from separate messages file
                        let messages_path = self.data_dir.join(format!("{}_messages.jsonl", data.id));
                        let messages = if messages_path.exists() {
                            Self::load_messages_from_file(&messages_path).unwrap_or_else(|_| Vec::new())
                        } else {
                            Vec::new()
                        };
                        
                        sessions.push(Session {
                            id: data.id,
                            title: data.title,
                            work_dir: data.work_dir,
                            messages,
                            created_at: data.created_at,
                            updated_at: data.updated_at,
                        });
                    } else {
                    }
                }
            }
        }

        // Update internal cache
        for session in &sessions {
            self.sessions.insert(session.id.clone(), session.clone());
        }
        
        Ok(sessions)
    }
    
    fn load_messages_from_file(path: &PathBuf) -> Result<Vec<Message>, String> {
        let mut messages = Vec::new();
        if let Ok(content) = fs::read_to_string(path) {
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(msg) = serde_json::from_str::<Message>(line) {
                    messages.push(msg);
                }
            }
        }
        Ok(messages)
    }
    
    pub fn save_message(&self, session_id: &str, message: &Message) -> Result<(), String> {
        let messages_path = self.data_dir.join(format!("{}_messages.jsonl", session_id));
        let line = serde_json::to_string(message)
            .map_err(|e| format!("Failed to serialize message: {}", e))?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&messages_path)
            .map_err(|e| format!("Failed to open messages file: {}", e))?;
        use std::io::Write;
        writeln!(file, "{}", line)
            .map_err(|e| format!("Failed to write message: {}", e))?;
        Ok(())
    }

    pub fn delete_session(&mut self, work_dir: &str, session_id: &str) -> Result<(), String> {
        self.sessions.remove(session_id);

        let session_path = self.session_file_path(session_id);
        if session_path.exists() {
            fs::remove_file(&session_path)
                .map_err(|e| format!("Failed to delete session file: {}", e))?;
        }

        let messages_path = self.data_dir.join(format!("{}_messages.jsonl", session_id));
        if messages_path.exists() {
            fs::remove_file(&messages_path)
                .map_err(|e| format!("Failed to delete session messages: {}", e))?;
        }

        let session_dir = self.get_session_dir(work_dir, session_id)?;
        if session_dir.exists() {
            fs::remove_dir_all(&session_dir)
                .map_err(|e| format!("Failed to delete CLI session directory: {}", e))?;
        }

        Ok(())
    }
    
    pub fn get_or_create_session(&mut self, session_id: &str, title: &str, work_dir: &str) -> Session {
        if let Some(session) = self.sessions.get(session_id) {
            return session.clone();
        }
        
        let now = chrono::Utc::now().timestamp();
        let session = Session {
            id: session_id.to_string(),
            title: title.to_string(),
            work_dir: work_dir.to_string(),
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        
        self.sessions.insert(session_id.to_string(), session.clone());
        let _ = self.save_session(&session);
        session
    }
    
    pub fn load_messages(&self, work_dir: &str, session_id: &str) -> Result<Vec<Message>, String> {
        let session_dir = self.get_session_dir(work_dir, session_id)?;
        let wire_file = session_dir.join("wire.jsonl");

        if !wire_file.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&wire_file)
            .map_err(|e| format!("Failed to read wire file: {}", e))?;

        let mut messages = Vec::new();
        let mut current_content = String::new();
        let mut current_role: Option<String> = None;

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
                // Handle nested message format: {"message": {"type": "...", "payload": {...}}}
                let msg_type = record.get("message")
                    .and_then(|m| m.get("type"))
                    .and_then(|v| v.as_str());

                match msg_type {
                    Some("TurnBegin") => {
                        // Flush any previous assistant content
                        if let Some(role) = &current_role {
                            if !current_content.is_empty() {
                                messages.push(Message {
                                    role: role.clone(),
                                    content: current_content.clone(),
                                    timestamp: chrono::Utc::now().timestamp(),
                                    tool_calls: None,
                                });
                            }
                        }

                        // Extract user message from payload.user_input array
                        let user_text = record.get("message")
                            .and_then(|m| m.get("payload"))
                            .and_then(|p| p.get("user_input"))
                            .and_then(|u| u.as_array())
                            .and_then(|arr| {
                                arr.iter()
                                    .find_map(|item| item.get("text").and_then(|t| t.as_str()))
                            })
                            .unwrap_or("")
                            .to_string();

                        if !user_text.is_empty() {
                            messages.push(Message {
                                role: "user".to_string(),
                                content: user_text,
                                timestamp: chrono::Utc::now().timestamp(),
                                tool_calls: None,
                            });
                        }

                        // Switch to assistant for subsequent content
                        current_role = Some("assistant".to_string());
                        current_content = String::new();
                    }
                    Some("ContentPart") => {
                        if current_role.as_deref() == Some("assistant") {
                            // Check payload.type to see if it's "text" (not "think" or other types)
                            let part_type = record.get("message")
                                .and_then(|m| m.get("payload"))
                                .and_then(|p| p.get("type"))
                                .and_then(|t| t.as_str());

                            if part_type == Some("text") {
                                if let Some(text) = record.get("message")
                                    .and_then(|m| m.get("payload"))
                                    .and_then(|p| p.get("text"))
                                    .and_then(|t| t.as_str())
                                {
                                    current_content.push_str(text);
                                }
                            }
                        }
                    }
                    Some("ToolCall") => {
                        // Handle tool calls if present
                    }
                    Some("StepEnd") | Some("TurnEnd") => {
                        if let Some(role) = &current_role {
                            if role == "assistant" && !current_content.is_empty() {
                                messages.push(Message {
                                    role: "assistant".to_string(),
                                    content: current_content.clone(),
                                    timestamp: chrono::Utc::now().timestamp(),
                                    tool_calls: None,
                                });
                                current_content = String::new();
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Flush any remaining assistant content
        if current_role.as_deref() == Some("assistant") && !current_content.is_empty() {
            messages.push(Message {
                role: "assistant".to_string(),
                content: current_content,
                timestamp: chrono::Utc::now().timestamp(),
                tool_calls: None,
            });
        }

        Ok(messages)
    }
    
    fn get_session_dir(&self, work_dir: &str, session_id: &str) -> Result<PathBuf, String> {
        use md5::{Md5, Digest};

        let mut hasher = Md5::new();
        hasher.update(work_dir.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        
        let share_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".kimi");
        
        let session_dir = share_dir
            .join("sessions")
            .join(hash)
            .join(session_id);
        
        Ok(session_dir)
    }
}
