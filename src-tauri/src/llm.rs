use serde::Serialize;
use std::path::Path;
use tauri::Emitter;
use uuid::Uuid;

use crate::oauth::{common_headers, ensure_fresh_token};
use crate::tools;
use crate::AppState;

#[derive(Clone, Serialize)]
pub struct StreamEvent {
    pub event: String,
    pub data: serde_json::Value,
}

const MAX_TOOL_STEPS: usize = 20;

fn api_base_url() -> String {
    std::env::var("KIMI_CODE_BASE_URL")
        .or_else(|_| std::env::var("KIMI_BASE_URL"))
        .unwrap_or_else(|_| "https://api.kimi.com/coding/v1".to_string())
}

/// Generate a detailed directory listing like `ls -la`
fn list_directory(work_dir: &str) -> String {
    let work_path = Path::new(work_dir);
    let mut entries: Vec<(String, bool, u64)> = Vec::new();
    
    if let Ok(dir_entries) = std::fs::read_dir(work_path) {
        for entry in dir_entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden files and common build directories
            if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist" || name == "build" {
                continue;
            }
            
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let size = if let Ok(metadata) = entry.metadata() {
                metadata.len()
            } else {
                0
            };
            
            entries.push((name, is_dir, size));
        }
    }
    
    // Sort: directories first, then files
    entries.sort_by(|a, b| {
        match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.cmp(&b.0),
        }
    });
    
    let mut output = String::new();
    output.push_str(&format!("total {}\n", entries.len()));
    
    for (name, is_dir, size) in entries {
        let size_str = if is_dir {
            "-".to_string()
        } else if size < 1024 {
            format!("{}", size)
        } else if size < 1024 * 1024 {
            format!("{:.1}K", size as f64 / 1024.0)
        } else {
            format!("{:.1}M", size as f64 / (1024.0 * 1024.0))
        };
        
        let type_char = if is_dir { "d" } else { "-" };
        let permissions = if is_dir { "rwxr-xr-x" } else { "rw-r--r--" };
        
        output.push_str(&format!(
            "{}{}  1 user  group  {:>8} Jan  1 00:00 {}\n",
            type_char, permissions, size_str, name
        ));
    }
    
    output
}

/// Read AGENTS.md if it exists
fn load_agents_md(work_dir: &str) -> Option<String> {
    let work_path = Path::new(work_dir);
    let paths = ["AGENTS.md", "agents.md"];
    
    for filename in &paths {
        let path = work_path.join(filename);
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }
    
    None
}

fn generate_system_prompt(work_dir: &str) -> String {
    let mut prompt = String::new();
    
    // Add directory listing
    let ls_output = list_directory(work_dir);
    prompt.push_str(&format!(
        "Current working directory: {}\n\nDirectory listing:\n{}\n",
        work_dir, ls_output
    ));
    
    // Add AGENTS.md if exists
    if let Some(agents_md) = load_agents_md(work_dir) {
        prompt.push_str("\nAGENTS.md:\n");
        prompt.push_str(&agents_md);
        prompt.push('\n');
    }
    
    prompt
}

fn parse_user_input(input: &str) -> String {
    // For now, just return the input as-is
    // Future: parse @file, $skill, etc. and load content
    input.to_string()
}

pub async fn stream_chat(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    session_id: String,
    user_message: String,
    model: String,
    work_dir: String,
    config_path: Option<String>,
    auto_approve: bool,
    auth_config: crate::AuthConfig,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    // Get auth token (OAuth or API Key)
    let (access_token, api_base) = if auth_config.mode == "api_key" {
        // API Key mode
        let api_key = auth_config.api_key.ok_or_else(|| {
            let _ = window.emit("chat://event", StreamEvent {
                event: "error".to_string(),
                data: serde_json::json!({
                    "session_id": session_id,
                    "message": "API key not configured. Please login first.",
                }),
            });
            "API key not configured"
        })?;
        let base = auth_config.api_base
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "https://api.moonshot.cn/v1".to_string());
        (api_key, base)
    } else {
        // OAuth mode
        match ensure_fresh_token().await {
            Some(token) => (token, api_base_url()),
            None => {
                let _ = window.emit("chat://event", StreamEvent {
                    event: "error".to_string(),
                    data: serde_json::json!({
                        "session_id": session_id,
                        "message": "Not logged in. Please login first.",
                    }),
                });
                return Err("Not logged in".to_string());
            }
        }
    };
    
    let client = reqwest::Client::new();

    // Build system prompt with directory context
    let system_prompt = generate_system_prompt(&work_dir);
    let tools_def = tools::tool_definitions();
    let mut messages = vec![
        serde_json::json!({
            "role": "system",
            "content": system_prompt,
        }),
        serde_json::json!({
            "role": "user",
            "content": parse_user_input(&user_message),
        }),
    ];

    for _ in 0..MAX_TOOL_STEPS {
        if cancel_rx.try_recv().is_ok() {
            let _ = window.emit(
                "chat://event",
                StreamEvent {
                    event: "cancelled".to_string(),
                    data: serde_json::json!({
                        "session_id": session_id,
                    }),
                },
            );
            return Ok(());
        }

        let request = serde_json::json!({
            "model": model,
            "messages": messages.clone(),
            "stream": false,
            "temperature": serde_json::Value::Null,
            "tools": tools_def.clone(),
            "tool_choice": "auto",
        });

        let mut req = client.post(format!("{}/chat/completions", api_base));
        for (key, value) in common_headers().into_iter() {
            req = req.header(key, value);
        }
        req = req.header("Authorization", format!("Bearer {}", access_token));

        let response = tokio::select! {
            _ = &mut cancel_rx => {
                let _ = window.emit(
                    "chat://event",
                    StreamEvent {
                        event: "cancelled".to_string(),
                        data: serde_json::json!({
                            "session_id": session_id,
                        }),
                    },
                );
                return Ok(());
            }
            resp = req.json(&request).send() => resp,
        }
        .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let message = data
            .get("choices")
            .and_then(|v| v.get(0))
            .and_then(|v| v.get("message"))
            .cloned()
            .ok_or_else(|| "No message in response".to_string())?;

        let reasoning = message
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !reasoning.is_empty() {
            let _ = window.emit(
                "chat://event",
                StreamEvent {
                    event: "thinking".to_string(),
                    data: serde_json::json!({
                        "session_id": session_id,
                        "content": reasoning,
                    }),
                },
            );
        }

        let tool_calls = message.get("tool_calls").and_then(|v| v.as_array()).cloned();
        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if let Some(tool_calls) = tool_calls {
            if !tool_calls.is_empty() {
                let mut assistant_message = serde_json::json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tool_calls,
                });
                if let Some(reasoning_value) = message.get("reasoning_content") {
                    assistant_message["reasoning_content"] = reasoning_value.clone();
                }
                messages.push(assistant_message);

                let calls = messages
                    .last()
                    .and_then(|v| v.get("tool_calls"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                for tool_call in calls {
                    if cancel_rx.try_recv().is_ok() {
                        let _ = window.emit(
                            "chat://event",
                            StreamEvent {
                                event: "cancelled".to_string(),
                                data: serde_json::json!({
                                    "session_id": session_id,
                                }),
                            },
                        );
                        return Ok(());
                    }
                    let mut tool_call_id = tool_call
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let function = tool_call.get("function").cloned().unwrap_or_default();
                    let name = function
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments_raw = function
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");

                    let args_value: serde_json::Value =
                        serde_json::from_str(arguments_raw).unwrap_or(serde_json::json!({}));

                    if tool_call_id.is_empty() {
                        tool_call_id = Uuid::new_v4().to_string();
                    }

                    let approved = if needs_approval(&name) && !auto_approve {
                        match request_approval(
                            &window,
                            &state,
                            &session_id,
                            &tool_call_id,
                            &name,
                            &args_value,
                            &mut cancel_rx,
                        )
                        .await
                        {
                            Ok(value) => value,
                            Err(_) => {
                                let _ = window.emit(
                                    "chat://event",
                                    StreamEvent {
                                        event: "cancelled".to_string(),
                                        data: serde_json::json!({
                                            "session_id": session_id,
                                        }),
                                    },
                                );
                                return Ok(());
                            }
                        }
                    } else {
                        true
                    };

                    let label = tool_label(&name, &args_value);
                    let output = if approved {
                        emit_tool_status(
                            &window,
                            &session_id,
                            &tool_call_id,
                            "start",
                            &name,
                            &label,
                            None,
                            None,
                        );

                        let tool_output = execute_tool(
                            &window,
                            &state,
                            &session_id,
                            &tool_call_id,
                            &name,
                            &args_value,
                            &work_dir,
                            config_path.as_deref(),
                        )
                        .await;

                        emit_tool_status(
                            &window,
                            &session_id,
                            &tool_call_id,
                            "end",
                            &name,
                            &label,
                            Some(tool_output.ok),
                            Some(tool_output.summary.clone()),
                        );

                        tool_output
                    } else {
                        emit_tool_status(
                            &window,
                            &session_id,
                            &tool_call_id,
                            "end",
                            &name,
                            &label,
                            Some(false),
                            Some("User rejected tool request.".to_string()),
                        );

                        tools::ToolOutput {
                            ok: false,
                            summary: "User rejected tool request.".to_string(),
                            output: String::new(),
                        }
                    };

                    let _ = window.emit(
                        "chat://event",
                        StreamEvent {
                            event: "tool_result".to_string(),
                            data: serde_json::json!({
                                "session_id": session_id,
                                "tool_call_id": tool_call_id,
                                "name": name,
                                "ok": output.ok,
                                "summary": output.summary,
                                "output": output.output,
                            }),
                        },
                    );

                    let tool_content = serde_json::json!({
                        "ok": output.ok,
                        "summary": output.summary,
                        "output": output.output,
                    })
                    .to_string();

                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": tool_content,
                    }));
                }

                continue;
            }
        }

        if !content.is_empty() {
            // Extract token usage from response if available
            let usage = data.get("usage").cloned().unwrap_or(serde_json::json!({}));
            let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let total_tokens = usage.get("total_tokens").and_then(|v| v.as_u64())
                .unwrap_or(prompt_tokens + completion_tokens);
            
            let _ = window.emit(
                "chat://event",
                StreamEvent {
                    event: "chunk".to_string(),
                    data: serde_json::json!({
                        "session_id": session_id,
                        "content": content,
                    }),
                },
            );
            let _ = window.emit(
                "chat://event",
                StreamEvent {
                    event: "done".to_string(),
                    data: serde_json::json!({
                        "session_id": session_id,
                        "usage": {
                            "prompt_tokens": prompt_tokens,
                            "completion_tokens": completion_tokens,
                            "total_tokens": total_tokens,
                        },
                    }),
                },
            );
            return Ok(());
        }
    }

    Err("Exceeded maximum tool steps".to_string())
}

#[tauri::command]
pub async fn llm_fetch_models(auth_config: crate::AuthConfig) -> Result<Vec<serde_json::Value>, String> {
    let (access_token, api_base) = if auth_config.mode == "api_key" {
        let api_key = auth_config.api_key.ok_or("API key not configured")?;
        let base = auth_config.api_base
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "https://api.moonshot.cn/v1".to_string());
        (api_key, base)
    } else {
        let token = ensure_fresh_token()
            .await
            .ok_or_else(|| "Not logged in".to_string())?;
        let base = std::env::var("KIMI_BASE_URL")
            .unwrap_or_else(|_| "https://api.kimi.com/coding/v1".to_string());
        (token, base)
    };
    
    let client = reqwest::Client::new();
    let mut req = client.get(format!("{}/models", api_base));
    for (key, value) in common_headers().into_iter() {
        req = req.header(key, value);
    }
    req = req.header("Authorization", format!("Bearer {}", access_token));
    let response = req.send().await.map_err(|e| format!("Request failed: {}", e))?;
    
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, text));
    }
    
    let data: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;
    
    let models = data["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    
    Ok(models)
}

fn needs_approval(tool_name: &str) -> bool {
    matches!(tool_name, "Shell" | "WriteFile" | "StrReplaceFile")
}

fn emit_tool_status(
    window: &tauri::Window,
    session_id: &str,
    tool_call_id: &str,
    state: &str,
    name: &str,
    label: &str,
    ok: Option<bool>,
    summary: Option<String>,
) {
    let _ = window.emit(
        "chat://event",
        StreamEvent {
            event: "tool_status".to_string(),
            data: serde_json::json!({
                "session_id": session_id,
                "tool_call_id": tool_call_id,
                "state": state,
                "name": name,
                "label": label,
                "ok": ok,
                "summary": summary,
            }),
        },
    );
}

fn tool_label(name: &str, args: &serde_json::Value) -> String {
    match name {
        "ReadFile" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("正在读取 {}", p))
            .unwrap_or_else(|| "正在读取文件".to_string()),
        "Shell" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| format!("正在执行 {}", cmd))
            .unwrap_or_else(|| "正在执行命令".to_string()),
        "WriteFile" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("正在写入 {}", p))
            .unwrap_or_else(|| "正在写入文件".to_string()),
        "StrReplaceFile" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("正在修改 {}", p))
            .unwrap_or_else(|| "正在修改文件".to_string()),
        "SearchWeb" => args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|q| format!("正在搜索 {}", q))
            .unwrap_or_else(|| "正在搜索网络".to_string()),
        "FetchURL" => args
            .get("url")
            .and_then(|v| v.as_str())
            .map(|u| format!("正在抓取 {}", u))
            .unwrap_or_else(|| "正在抓取网页".to_string()),
        _ => format!("正在执行 {}", name),
    }
}

async fn request_approval(
    window: &tauri::Window,
    state: &tauri::State<'_, AppState>,
    session_id: &str,
    tool_call_id: &str,
    name: &str,
    args: &serde_json::Value,
    cancel_rx: &mut tokio::sync::oneshot::Receiver<()>,
) -> Result<bool, String> {
    let request_id = format!("{}:{}", session_id, tool_call_id);
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let mut approvals = state
            .approvals
            .lock()
            .map_err(|_| "Approval store poisoned".to_string())?;
        approvals.insert(request_id.clone(), tx);
    }

    let _ = window.emit(
        "chat://event",
        StreamEvent {
            event: "tool_approval".to_string(),
            data: serde_json::json!({
                "session_id": session_id,
                "request_id": request_id,
                "name": name,
                "args": args,
            }),
        },
    );

    let approved = tokio::select! {
        _ = cancel_rx => {
            let mut approvals = state
                .approvals
                .lock()
                .map_err(|_| "Approval store poisoned".to_string())?;
            approvals.remove(&request_id);
            return Err("Cancelled".to_string());
        }
        result = rx => {
            result.unwrap_or(false)
        }
    };

    Ok(approved)
}

async fn execute_tool(
    _window: &tauri::Window,
    _state: &tauri::State<'_, AppState>,
    _session_id: &str,
    tool_call_id: &str,
    name: &str,
    args: &serde_json::Value,
    work_dir: &str,
    config_path: Option<&str>,
) -> tools::ToolOutput {
    match name {
        "ReadFile" => {
            let path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => {
                    return tools::ToolOutput {
                        ok: false,
                        summary: "Missing path".to_string(),
                        output: String::new(),
                    }
                }
            };
            let line_offset = args
                .get("line_offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as usize;
            let n_lines = args
                .get("n_lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(1000) as usize;
            tools::read_file(work_dir, path, line_offset, n_lines)
        }
        "Shell" => {
            let command = match args.get("command").and_then(|v| v.as_str()) {
                Some(cmd) => cmd,
                None => {
                    return tools::ToolOutput {
                        ok: false,
                        summary: "Missing command".to_string(),
                        output: String::new(),
                    }
                }
            };
            let timeout = args
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(60);
            tools::run_shell(work_dir, command, timeout).await
        }
        "WriteFile" => {
            let path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => {
                    return tools::ToolOutput {
                        ok: false,
                        summary: "Missing path".to_string(),
                        output: String::new(),
                    }
                }
            };
            let content = match args.get("content").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => {
                    return tools::ToolOutput {
                        ok: false,
                        summary: "Missing content".to_string(),
                        output: String::new(),
                    }
                }
            };
            let mode = args
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("overwrite");
            tools::write_file(work_dir, path, content, mode)
        }
        "StrReplaceFile" => {
            let path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => {
                    return tools::ToolOutput {
                        ok: false,
                        summary: "Missing path".to_string(),
                        output: String::new(),
                    }
                }
            };

            let mut edits = Vec::new();
            if let Some(edit_value) = args.get("edit") {
                if edit_value.is_array() {
                    if let Ok(list) = serde_json::from_value::<Vec<tools::ReplaceEdit>>(
                        edit_value.clone(),
                    ) {
                        edits = list;
                    }
                } else if let Ok(edit) =
                    serde_json::from_value::<tools::ReplaceEdit>(edit_value.clone())
                {
                    edits.push(edit);
                }
            }

            if edits.is_empty() {
                return tools::ToolOutput {
                    ok: false,
                    summary: "Missing edits".to_string(),
                    output: String::new(),
                };
            }

            tools::str_replace_file(work_dir, path, edits)
        }
        "SearchWeb" => {
            let query = match args.get("query").and_then(|v| v.as_str()) {
                Some(q) => q,
                None => {
                    return tools::ToolOutput {
                        ok: false,
                        summary: "Missing query".to_string(),
                        output: String::new(),
                    }
                }
            };
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;
            let include_content = args
                .get("include_content")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            tools::search_web(config_path, tool_call_id, query, limit, include_content).await
        }
        "FetchURL" => {
            let url = match args.get("url").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => {
                    return tools::ToolOutput {
                        ok: false,
                        summary: "Missing URL".to_string(),
                        output: String::new(),
                    }
                }
            };
            tools::fetch_url(config_path, tool_call_id, url).await
        }
        _ => tools::ToolOutput {
            ok: false,
            summary: format!("Unknown tool: {}", name),
            output: String::new(),
        },
    }
}
