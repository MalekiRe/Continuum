
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for model invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// API endpoint or model path.
    pub endpoint: String,
    /// Model name or filename.
    pub model: String,
    /// API key (if applicable).
    pub api_key: Option<String>,
    /// Max tokens to generate.
    pub max_tokens: u32,
    /// Temperature for sampling.
    pub temperature: f64,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            endpoint: "http://localhost:8080/v1/completions".into(),
            model: "default".into(),
            api_key: None,
            max_tokens: 2048,
            temperature: 0.7,
        }
    }
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Request to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub temperature: f64,
}

/// Response from the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub text: String,
    pub tokens_generated: u32,
    pub finish_reason: String,
}

/// Model invocation — calls an external LLM.
pub fn invoke_model(config: &ModelConfig, request: &ModelRequest) -> Result<ModelResponse, String> {
    // Build the prompt from messages
    let prompt = request.messages.iter()
        .map(|m| format!("<{}>\n{}\n</{}>", m.role, m.content, m.role))
        .collect::<Vec<_>>()
        .join("\n");

    let full_prompt = format!("{}\n<assistant>\n", prompt);

    // Try API endpoint first
    if let Some(api_key) = &config.api_key {
        return call_api(config, &full_prompt, request.max_tokens, request.temperature, api_key);
    }

    // Fall back to local inference (llama.cpp or similar)
    call_local(&full_prompt, request.max_tokens, request.temperature)
}

/// Call a remote API (OpenAI-compatible).
fn call_api(
    config: &ModelConfig,
    prompt: &str,
    max_tokens: u32,
    temperature: f64,
    api_key: &str,
) -> Result<ModelResponse, String> {
    let client = reqwest::blocking::Client::new();

    let body = serde_json::json!({
        "model": config.model,
        "prompt": prompt,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "stop": ["</assistant>", "<human>"],
    });

    let resp = client
        .post(&config.endpoint)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("API error: {}", e))?;

    let json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("API parse error: {}", e))?;

    let text = json["choices"][0]["text"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(ModelResponse {
        text,
        tokens_generated: json["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
        finish_reason: json["choices"][0]["finish_reason"].as_str().unwrap_or("unknown").to_string(),
    })
}

/// Call a local model via subprocess (llama.cpp, candle, etc.)
fn call_local(prompt: &str, max_tokens: u32, temperature: f64) -> Result<ModelResponse, String> {
    // Try llama.cpp
    let args = vec![
        "--prompt".to_string(),
        prompt.to_string(),
        "--n-predict".to_string(),
        max_tokens.to_string(),
        "--temp".to_string(),
        temperature.to_string(),
        "--stop".to_string(),
        "</assistant>".to_string(),
        "--stop".to_string(),
        "<human>".to_string(),
    ];

    match std::process::Command::new("llama-cli")
        .args(&args)
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if !output.status.success() {
                return Err(format!("model error: {}", stderr));
            }

            // Extract the generated text (after the prompt)
            let text = stdout.trim().to_string();

            Ok(ModelResponse {
                text,
                tokens_generated: 0, // not easily extractable
                finish_reason: "stop".into(),
            })
        }
        Err(e) => Err(format!("cannot run model: {}", e)),
    }
}

impl ModelRequest {
    /// Create a simple prompt-only request.
    pub fn from_prompt(prompt: &str) -> Self {
        ModelRequest {
            messages: vec![
                Message {
                    role: "user".into(),
                    content: prompt.to_string(),
                }
            ],
            max_tokens: 2048,
            temperature: 0.7,
        }
    }
}
