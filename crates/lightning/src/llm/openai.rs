use crate::llm::{ChatMessage, LLMError, ToolSpec, LLMResponse};


struct ChatRequest {
    model: String, 
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDef> 
}

struct ToolDef {
    r#type: &'static str,
    function: ToolSpec
}

struct ChatResponse { 
    choices: Vec<Choice> 
}

struct Choice { 
    message: ResponseMessage 
}

struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallWire>>
}

struct ToolCallWire {
    id: String,
    function: FunctionWire
}

struct FunctionWire { 
    name: String, 
    arguments: String 
}

fn into_response(response: ChatResponse) -> Result<LLMResponse, LLMError> {
    let first = response.choices.iter().next();
    match first {
        None => {
            Err(LLMError::EmptyResponse)
        }
        Some(choice) => {
            if choice.message.tool_calls.is_some() {
                Ok(LLMResponse::ToolCall { id: choice.message.tool_calls.iter().next(), name: (), arguments: () })
            } else {
                Ok(LLMResponse::Text(first))
            }
        } 
    }
}