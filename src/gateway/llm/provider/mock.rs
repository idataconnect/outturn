use uuid::Uuid;

use super::{LlmProvider, Provider, ProviderError};
use crate::gateway::llm::types::*;

pub struct MockProvider;

impl MockProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockProvider {
    fn provider(&self) -> Provider {
        Provider::Mock
    }

    async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let last_message = request
            .messages
            .last()
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            })
            .unwrap_or_default();

        let response_text = if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                let tool = &tools[0];
                return Ok(ChatCompletionResponse {
                    id: format!("mock-{}", Uuid::now_v7()),
                    object: "chat.completion".to_string(),
                    created: 0,
                    model: request.model.clone(),
                    choices: vec![Choice {
                        index: 0,
                        parts: Vec::new(),
                        message: Message {
                            role: Role::Assistant,
                            content: MessageContent::Text(String::new()),
                            name: None,
                            tool_calls: Some(vec![ToolCall {
                                provider_signature: None,
                                id: format!("call_{}", Uuid::now_v7()),
                                tool_type: "function".to_string(),
                                function: FunctionCall {
                                    name: tool.function.name.clone(),
                                    arguments: tool
                                        .function
                                        .parameters
                                        .as_ref()
                                        .map(|_| "{}".to_string())
                                        .unwrap_or_else(|| "{}".to_string()),
                                },
                            }]),
                            tool_call_id: None,
                        },
                        finish_reason: Some("tool_calls".to_string()),
                    }],
                    usage: Some(Usage {
                        prompt_tokens: 10,
                        completion_tokens: 5,
                        prompt_tokens_details: None,
                        completion_tokens_details: None,
                        total_tokens: 15,
                        extra: Default::default(),
                    }),
                });
            }
            format!("Mock response to: {last_message}")
        } else {
            format!("Mock response to: {last_message}")
        };

        Ok(ChatCompletionResponse {
            id: format!("mock-{}", Uuid::now_v7()),
            object: "chat.completion".to_string(),
            created: 0,
            model: request.model.clone(),
            choices: vec![Choice {
                index: 0,
                parts: Vec::new(),
                message: Message {
                    role: Role::Assistant,
                    content: MessageContent::Text(response_text),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 20,
                prompt_tokens_details: None,
                completion_tokens_details: None,
                total_tokens: 30,
                extra: Default::default(),
            }),
        })
    }

    fn endpoint(&self) -> String {
        "mock".to_string()
    }
}
