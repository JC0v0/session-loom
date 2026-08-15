use crate::canonical::{Message, ToolCall};
use serde_json::Value;
use std::collections::HashMap;

pub mod claude;
pub mod codex;

fn json_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => String::new(),
        Some(value) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn json_argument(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(value)) => {
            serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.clone()))
        }
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

fn attach_tool_outputs(messages: &mut [Message], outputs: &HashMap<String, String>) {
    for call in messages
        .iter_mut()
        .flat_map(|message| message.tool_calls.iter_mut())
    {
        if let Some(output) = outputs.get(&call.id) {
            call.output = Some(output.clone());
        }
    }
}

fn push_tool_call(messages: &mut Vec<Message>, call: ToolCall) {
    if let Some(message) = messages
        .last_mut()
        .filter(|message| message.role == crate::canonical::Role::Assistant)
    {
        message.tool_calls.push(call);
    } else {
        messages.push(Message {
            role: crate::canonical::Role::Assistant,
            text: String::new(),
            tool_calls: vec![call],
        });
    }
}

pub fn encode_claude_project(cwd: &str) -> String {
    cwd.chars()
        .map(|character| match character {
            ':' | '\\' | '/' => '-',
            other => other,
        })
        .collect()
}
