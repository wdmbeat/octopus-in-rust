use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;

/// A content part within a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Think {
        think: String,
        encrypted: Option<String>,
    },
    ImageUrl {
        image_url: ImageUrl,
    },
    AudioUrl {
        audio_url: AudioUrl,
    },
    VideoUrl {
        video_url: VideoUrl,
    },
}

impl ContentPart {
    /// Attempt to merge another part into this one in-place.
    /// Returns `true` if the merge succeeded.
    pub fn merge_in_place(&mut self, other: &ContentPart) -> bool {
        match (self, other) {
            (ContentPart::Text { text: a }, ContentPart::Text { text: b }) => {
                a.push_str(b);
                true
            }
            (
                ContentPart::Think {
                    think: a,
                    encrypted: None,
                },
                ContentPart::Think { think: b, .. },
            ) => {
                a.push_str(b);
                true
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioUrl {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoUrl {
    pub url: String,
}

/// The type of a tool call.
///
/// OpenAI's chat completions API currently defines only one tool call type:
/// `"function"`. This enum makes that invariant explicit while leaving room
/// for future variants if the API expands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolCallType {
    #[default]
    #[serde(rename = "function")]
    Function,
}

/// A tool call issued by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(rename = "type", default)]
    pub call_type: ToolCallType,
    pub id: String,
    pub function: FunctionBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<HashMap<String, serde_json::Value>>,
}

impl ToolCall {
    /// Attempt to merge a `ToolCallPart` into this tool call in-place.
    /// Returns `true` if the merge succeeded.
    pub fn merge_in_place(&mut self, part: &ToolCallPart) -> bool {
        if let Some(ref args) = part.arguments_part {
            self.function.arguments =
                Some(self.function.arguments.clone().unwrap_or_default() + args);
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionBody {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// A partial tool call argument chunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments_part: Option<String>,
}

/// Message role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A chat message.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(serialize_with = "serialize_content")]
    pub content: Vec<ContentPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
}

impl Message {
    pub fn extract_text(&self, sep: &str) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(sep)
    }
}

#[allow(dead_code)]
fn serialize_content<S: Serializer>(
    content: &Vec<ContentPart>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if content.len() == 1
        && let ContentPart::Text { text } = &content[0]
    {
        return serializer.serialize_str(text);
    }
    content.serialize(serializer)
}

/// Token usage for a generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_other: usize,
    pub output: usize,
    #[serde(default)]
    pub input_cache_read: usize,
    #[serde(default)]
    pub input_cache_creation: usize,
}

impl TokenUsage {
    pub fn total(&self) -> usize {
        self.input_other + self.input_cache_read + self.input_cache_creation + self.output
    }

    pub fn input(&self) -> usize {
        self.input_other + self.input_cache_read + self.input_cache_creation
    }
}
