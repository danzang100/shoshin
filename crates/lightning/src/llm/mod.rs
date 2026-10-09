use serde::Serialize;
use strum::Display;
use reqwest;

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
