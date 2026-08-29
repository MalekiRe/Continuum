use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for model invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub max_tokens: u32,
    pub temperature: f64,
    pub top_p: f64,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            endpoint: "https://openrouter.ai/api/v1".into(),
            model: "deepseek/deepseek-v4-flash".into(),
            api_key: detect_api_key(),
            max_tokens: 2048,
            temperature: 0.7,
            top_p: 0.95,
        }
    }
}

fn detect_api_key() -> Option<String> {
    // Check environment variables first
    for var in &["OPENROUTER_API_KEY", "OPENAI_API_KEY", "PRIME_API_KEY"] {
        if let Ok(key) = std::env::var(var) {
            if !key.is_empty() {
                return Some(key);
            }
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let home_path = std::path::Path::new(&home);

    // Check for OpenRouter key in auth.json (preferred — works with chat completions)
    let auth_path = home_path.join(".prime").join("agent").join("auth.json");
    if let Ok(content) = std::fs::read_to_string(&auth_path) {
        if let Some(start) = content.find("sk-or-v1-") {
            let rest = &content[start..];
            let end = rest.find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
                .unwrap_or(rest.len());
            let key = &rest[..end];
            if key.len() > 20 {
                return Some(key.to_string());
            }
        }
    }

    // Fall back to Prime API key from config.json (may only work for model listing)
    let config_path = home_path.join(".prime").join("config.json");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(key) = config["api_key"].as_str() {
                if !key.is_empty() && key.starts_with("pit_") {
                    return Some(key.to_string());
                }
            }
        }
    }

    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub temperature: f64,
    pub top_p: f64,
    pub extra: HashMap<String, serde_json::Value>,
}

impl ModelRequest {
    pub fn from_prompt(prompt: &str) -> Self {
        ModelRequest {
            messages: vec![Message {
                role: "user".into(),
                content: prompt.to_string(),
            }],
            max_tokens: 2048,
            temperature: 0.7,
            top_p: 0.95,
            extra: HashMap::new(),
        }
    }

    pub fn from_chat(messages: Vec<Message>) -> Self {
        ModelRequest {
            messages,
            max_tokens: 2048,
            temperature: 0.7,
            top_p: 0.95,
            extra: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub text: String,
    pub tokens_generated: u32,
    pub tokens_prompt: u32,
    pub finish_reason: String,
    pub cost: f64,
    pub model_used: String,
}

pub fn invoke_model(config: &ModelConfig, request: &ModelRequest) -> Result<ModelResponse, String> {
    let api_key = config.api_key.clone()
        .or_else(detect_api_key)
        .ok_or_else(|| "No API key found. Set PRIME_API_KEY, OPENROUTER_API_KEY, or OPENAI_API_KEY.".to_string())?;

    let endpoint = config.endpoint.trim_end_matches('/').to_string();
    let model = config.model.clone();

    let mut body = serde_json::json!({
        "model": model,
        "messages": request.messages.iter().map(|m| {
            serde_json::json!({"role": m.role, "content": m.content})
        }).collect::<Vec<_>>(),
        "max_tokens": request.max_tokens,
        "temperature": request.temperature,
        "top_p": request.top_p,
    });

    for (k, v) in &request.extra {
        body[k] = v.clone();
    }

    let client = reqwest::blocking::Client::new();
    let url = format!("{}/chat/completions", endpoint);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .header("HTTP-Referer", "http://localhost:5173")
        .header("X-Title", "Persistent-Agent-Lisp-Harness")
        .json(&body)
        .send()
        .map_err(|e| format!("API request error: {}", e))?;

    let status = resp.status();
    let json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("API response parse error: {}", e))?;

    if !status.is_success() {
        let error_msg = json["error"]["message"]
            .as_str()
            .unwrap_or(&json.to_string())
            .to_string();
        return Err(format!("API error ({}): {}", status, error_msg));
    }

    let text = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let finish_reason = json["choices"][0]["finish_reason"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    let usage = &json["usage"];
    let tokens_prompt = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
    let tokens_generated = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
    let cost = json["total_cost"].as_f64().unwrap_or(0.0);
    let model_used = json["model"].as_str().unwrap_or(&model).to_string();

    Ok(ModelResponse {
        text,
        tokens_generated,
        tokens_prompt,
        finish_reason,
        cost,
        model_used,
    })
}
