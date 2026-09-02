//! The platform's default agent.
//!
//! Runs a single conversational turn: hands the conversation to the model and
//! returns its reply. Deliberately minimal -- it exists to make the runtime
//! tier real, and to be the reference for what a guest looks like.
//!
//! Everything this component can reach is a host import. It has no sockets, no
//! filesystem and no credentials: a model call goes through `chat`, and the
//! host attaches the token.

#[allow(warnings)]
mod bindings;

use bindings::exports::outturn::agent::agent::Guest;
use bindings::outturn::agent::host::{self, CompletionRequest, Message};

struct Component;

impl Guest for Component {
    fn run(conversation: Vec<Message>, system_prompt: String) -> Result<String, String> {
        // The system prompt leads the conversation rather than being stored
        // with it, so editing an agent takes effect on its next turn instead
        // of only on new sessions.
        let mut messages = Vec::with_capacity(conversation.len() + 1);
        if !system_prompt.is_empty() {
            messages.push(Message {
                role: "system".to_string(),
                content: system_prompt,
            });
        }
        messages.extend(conversation);

        if messages.iter().all(|m| m.role != "user") {
            return Err("conversation contains nothing to respond to".to_string());
        }

        let completion = host::chat(&CompletionRequest {
            messages,
            model: None,
            temperature: None,
            max_tokens: None,
        })?;

        Ok(completion.content)
    }
}

bindings::export!(Component with_types_in bindings);
