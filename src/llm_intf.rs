use serde::{Deserialize, Serialize};
use std::vec::Vec;
use log::{info, warn, error};
use esp_idf_svc::{
    http::client::{EspHttpConnection, Configuration as HttpConfiguration},
    http::Method,
};
use anyhow::Result;

/// Enum representing different roles in a chat conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

impl ChatRole {
    fn as_str(&self) -> &'static str {
        match self {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        }
    }
}

/// Structure representing a chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    role: String,
    content: String,
}

/// Request structure for the DeepSeek API
#[derive(Debug, Serialize)]
struct DeepSeekRequest {
    messages: Vec<ChatMessage>,
    model: String,
    frequency_penalty: f32,
    max_tokens: u32,
    presence_penalty: f32,
    response_format: ResponseFormat,
    stop: Option<Vec<String>>,
    stream: bool,
    stream_options: Option<String>,
    temperature: f32,
    top_p: f32,
    tools: Option<String>,
    tool_choice: String,
    logprobs: bool,
    top_logprobs: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: String,
}

/// Response structure from the DeepSeek API
#[derive(Debug, Deserialize)]
struct DeepSeekResponse {
    #[allow(dead_code)]
    id: String,
    choices: Vec<Choice>,
    #[allow(dead_code)]
    created: u64,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    object: String,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[allow(dead_code)]
    finish_reason: String,
    #[allow(dead_code)]
    index: u32,
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct Usage {
    completion_tokens: u32,
    prompt_tokens: u32,
    total_tokens: u32,
}

/// Main structure for interacting with the DeepSeek LLM API
pub struct LlmHelper {
    /// API endpoint for the DeepSeek service
    api_endpoint: String,
    /// API token for authentication
    api_token: String,
    /// Model to use for generating responses
    model_name: String,
    /// Chat history
    message_history: Vec<ChatMessage>,
    /// Maximum number of tokens to generate
    max_tokens: u32,
    /// Temperature parameter for controlling randomness
    temperature: f32,
    /// Top_p parameter for nucleus sampling
    top_p: f32,
}

impl LlmHelper {
    /// Create a new instance of LlmHelper
    pub fn new(api_token: &str, model_name: &str) -> Self {
        let mut helper = LlmHelper {
            api_endpoint: "https://api.deepseek.com/chat/completions".to_string(),
            api_token: api_token.to_string(),
            model_name: model_name.to_string(),
            message_history: Vec::new(),
            max_tokens: 2048,
            temperature: 1.0,
            top_p: 1.0,
        };

        helper
    }

    /// Get a copy of the message history
    pub fn get_history(&self) -> Vec<String> {
        self.message_history
            .iter()
            .map(|msg| format!("[{}]: {}", msg.role, msg.content))
            .collect()
    }

    /// Clear the message history, keeping only the system message
    #[allow(dead_code)]
    pub fn clear_history(&mut self) {
        if !self.message_history.is_empty() {
            // Preserve system message if it exists
            let system_messages: Vec<ChatMessage> = self
                .message_history
                .iter()
                .filter(|msg| msg.role == "system")
                .cloned()
                .collect();

            self.message_history = system_messages;
        }
    }

    /// Configure parameters for the LLM requests
    pub fn configure(&mut self, max_tokens: Option<u32>, temperature: Option<f32>, top_p: Option<f32>) {
        if let Some(tokens) = max_tokens {
            self.max_tokens = tokens;
        }

        if let Some(temp) = temperature {
            self.temperature = temp;
        }

        if let Some(p) = top_p {
            self.top_p = p;
        }
    }

    /// Send a message to the LLM and get a response
    pub fn send_message(&mut self, text: String, role: ChatRole) -> String {
        // Create and store the new message
        let message = ChatMessage {
            role: role.as_str().to_string(),
            content: text,
        };

        self.message_history.push(message);

        // Don't make API calls for system messages
        if matches!(role, ChatRole::System) {
            return String::new();
        }

        // Build and send request
        match self.make_api_request() {
            Ok(response) => response,
            Err(e) => {
                let error_msg = format!("Error: {}", e);
                error!("{}", error_msg);
                error_msg
            }
        }
    }

    /// Make the actual API request to DeepSeek using ESP-IDF HTTP client
    fn make_api_request(&mut self) -> Result<String> {
        // Prepare request payload
        let request = DeepSeekRequest {
            messages: self.message_history.clone(),
            model: self.model_name.clone(),
            frequency_penalty: 0.0,
            max_tokens: self.max_tokens,
            presence_penalty: 0.0,
            response_format: ResponseFormat {
                format_type: "text".to_string(),
            },
            stop: None,
            stream: false,
            stream_options: None,
            temperature: self.temperature,
            top_p: self.top_p,
            tools: None,
            tool_choice: "none".to_string(),
            logprobs: false,
            top_logprobs: None,
        };

        let json_payload = serde_json::to_string(&request)?;

        info!("Sending request to DeepSeek API...");

        // Create HTTP client configuration with TLS support
        let config = HttpConfiguration {
            timeout: Some(std::time::Duration::from_secs(30)),
            use_global_ca_store: true,
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            ..Default::default()
        };

        let api_url = self.api_endpoint.clone();

        // Create HTTP client
        let mut client = match EspHttpConnection::new(&config) {
            Ok(client) => client,
            Err(e) => {
                error!("Failed to create HTTP client: {}", e);
                return Err(anyhow::anyhow!("HTTP client creation failed: {}", e));
            }
        };

        // Prepare headers for the request
        let headers = [
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
            ("Authorization", &format!("Bearer {}", self.api_token)),
            ("Content-Length", &json_payload.len().to_string()),
        ];

        // Send the request with better error handling
        info!("Initiating HTTP request to {}", &api_url);
        if let Err(e) = client.initiate_request(Method::Post, &api_url, &headers) {
            error!("Failed to initiate HTTP request: {}", e);
            return Err(anyhow::anyhow!("Failed to initiate HTTP request: {}", e));
        }

        if let Err(e) = client.write(json_payload.as_bytes()) {
            error!("Failed to write request body: {}", e);
            return Err(anyhow::anyhow!("Failed to write request body: {}", e));
        }

        // Finalize the request
        if let Err(e) = client.initiate_response() {
            error!("Failed to finalize HTTP request: {}", e);
            return Err(anyhow::anyhow!("Failed to finalize HTTP request: {}", e));
        }
        info!("HTTP request sent successfully.");

        // Get the response status
        let status = client.status();
        info!("HTTP response status: {}", status);

        if status != 200 {
            return Err(anyhow::anyhow!("HTTP request failed with status: {}", status));
        }

        // Read response body
        let mut response_body = Vec::new();
        let mut buffer = [0u8; 1024];

        loop {
            match client.read(&mut buffer) {
                Ok(bytes_read) => {
                    if bytes_read == 0 {
                        break;
                    }
                    response_body.extend_from_slice(&buffer[..bytes_read]);
                },
                Err(e) => {
                    error!("Error reading response: {}", e);
                    return Err(anyhow::anyhow!("Error reading response: {}", e));
                }
            }
        }

        // Parse the response
        let response_str = String::from_utf8(response_body)?;

        // Check if the response is valid JSON
        match serde_json::from_str::<DeepSeekResponse>(&response_str) {
            Ok(api_response) => {
                // Extract and store the assistant's response
                if !api_response.choices.is_empty() {
                    let assistant_message = api_response.choices[0].message.clone();

                    // Add the assistant response to the history
                    self.message_history.push(assistant_message.clone());

                    info!(
                        "Response received. Tokens used: {} (prompt) + {} (completion) = {} (total)",
                        api_response.usage.prompt_tokens,
                        api_response.usage.completion_tokens,
                        api_response.usage.total_tokens
                    );

                    Ok(assistant_message.content)
                } else {
                    let error_msg = "No response choices returned from API".to_string();
                    warn!("{}", error_msg);
                    Ok(error_msg)
                }
            },
            Err(e) => {
                error!("Failed to parse API response: {}", e);
                error!("Raw response: {}", response_str);
                Err(anyhow::anyhow!("Failed to parse API response: {}", e))
            }
        }
    }
}

// Unit tests
#[cfg(test)]
mod tests {
    use super::*;

    // Mock test for LlmHelper initialization
    #[test]
    fn test_llm_helper_new() {
        let helper = LlmHelper::new("fake_token", "deepseek-chat");
        assert_eq!(helper.model_name, "deepseek-chat");
        assert_eq!(helper.max_tokens, 2048);
        assert_eq!(helper.temperature, 1.0);
        assert!(!helper.message_history.is_empty()); // Should have system message
    }

    // Test configuration
    #[test]
    fn test_configure() {
        let mut helper = LlmHelper::new("fake_token", "deepseek-chat");
        helper.configure(Some(1024), Some(0.7), Some(0.9));
        assert_eq!(helper.max_tokens, 1024);
        assert_eq!(helper.temperature, 0.7);
        assert_eq!(helper.top_p, 0.9);
    }

    // Test clearing history
    #[test]
    fn test_clear_history() {
        let mut helper = LlmHelper::new("fake_token", "deepseek-chat");

        // Add a user message
        helper.send_message("Hello".to_string(), ChatRole::User);
        assert!(helper.message_history.len() > 1);

        // Clear history
        helper.clear_history();

        // Should keep system message(s)
        let system_count = helper.message_history
            .iter()
            .filter(|msg| msg.role == "system")
            .count();
        assert_eq!(helper.message_history.len(), system_count);
    }
}