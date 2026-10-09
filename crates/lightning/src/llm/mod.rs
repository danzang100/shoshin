use serde::Serialize;
use strum::Display;
use reqwest;

use crate::tools::registry::ToolSpec;

pub mod gemini;
// pub mod openai;

#[derive(Debug, Display)]
pub enum LLMError {
    Http(reqwest::Error),
    EmptyResponse,
    BadToolArguments(serde_json::Error)
}

impl std::error::Error for LLMError {}

impl From<reqwest::Error> for LLMError {
    fn from(err: reqwest::Error) -> Self {
        LLMError::Http(err)
    }
}

impl From<serde_json::Error> for LLMError {
    fn from(err: serde_json::Error) -> Self {
        LLMError::BadToolArguments(err)
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String
}

pub enum LLMResponse {
    Text(String),
    ToolCall { id: String, name: String, arguments: serde_json::Value}
}

pub struct LLMClient {
    http: reqwest::Client,
    base_url: String,
    model: String
}

// impl LLMClient {
//     pub fn new(base_url: String, model: String) -> Self {
//         LLMClient { http: reqwest::Client::new(), base_url, model }
//     }

//     pub fn from_env() -> Self {
//         LLMClient { http: (), base_url: (), model: () }
//     }

//     pub async fn chat(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> Result<LLMResponse, LLMError> {
//         response = self.http.post(url).json(&request).send().await()?;

//     }
// }
//
