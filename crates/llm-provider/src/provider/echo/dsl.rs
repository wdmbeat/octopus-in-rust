use crate::chat_provider::{ChatProviderError, Part};
use crate::message::{
    AudioUrl, ContentPart, FunctionBody, ImageUrl, TokenUsage, ToolCall, ToolCallPart, VideoUrl,
};
use serde_json::Value;

/// Parse an echo DSL script into a list of parts, an optional message id, and optional usage.
///
/// The DSL is made of lines in the form `kind: payload`. Empty lines, comment lines starting
/// with `#`, and markdown fences starting with ``` are ignored.
///
/// Supported kinds:
/// - `id`: sets the streamed message id.
/// - `usage`: token usage, e.g. `usage: {"input_other": 10, "output": 2}` or
///   `usage: input_other=1 output=2 input_cache_read=3`.
/// - `text`: a text chunk.
/// - `think`: a thinking chunk.
/// - `image_url`: either a raw URL or `{"url": "...", "id": "opt"}`.
/// - `audio_url`: either a raw URL or `{"url": "...", "id": "opt"}`.
/// - `video_url`: either a raw URL or `{"url": "...", "id": "opt"}`.
/// - `tool_call`: a JSON or key/value object. Fields: `id`, `name` (or `function.name`),
///   optional `arguments`/`function.arguments`, optional `extras`.
/// - `tool_call_part`: a string/JSON with `arguments_part`; `null` becomes `None`.
/// - `error`: simulated error — `error: <status_code>`, `error: connection <msg>`,
///   `error: timeout <msg>`.
type ParsedScript = (Vec<Part>, Option<String>, Option<TokenUsage>);

pub fn parse_echo_script(script: &str) -> Result<ParsedScript, ChatProviderError> {
    let mut parts = Vec::new();
    let mut message_id: Option<String> = None;
    let mut usage: Option<TokenUsage> = None;

    for (lineno, raw_line) in script.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("```") {
            continue;
        }
        if line.eq_ignore_ascii_case("echo") {
            continue;
        }

        let Some((key, payload)) = line.split_once(':') else {
            return Err(ChatProviderError::new(format!(
                "Invalid echo DSL at line {}: {:?}",
                lineno + 1,
                raw_line
            )));
        };

        let kind = key.trim().to_lowercase();
        let payload = payload.strip_prefix(' ').unwrap_or(payload);

        match kind.as_str() {
            "error" => raise_simulated_error(payload.trim(), lineno + 1, raw_line)?,
            "id" => {
                message_id = Some(strip_quotes(payload.trim()).to_string());
            }
            "usage" => {
                usage = Some(parse_usage(payload)?);
            }
            _ => {
                let part = parse_part(&kind, payload, lineno + 1, raw_line)?;
                parts.push(part);
            }
        }
    }

    Ok((parts, message_id, usage))
}

fn parse_part(
    kind: &str,
    payload: &str,
    lineno: usize,
    raw_line: &str,
) -> Result<Part, ChatProviderError> {
    match kind {
        "text" => Ok(Part::Content(ContentPart::Text {
            text: strip_quotes(payload).to_string(),
        })),
        "think" => Ok(Part::Content(ContentPart::Think {
            think: strip_quotes(payload).to_string(),
            encrypted: None,
        })),
        "image_url" => {
            let (url, _id) = parse_url_payload(payload, kind)?;
            Ok(Part::Content(ContentPart::ImageUrl {
                image_url: ImageUrl { url, detail: None },
            }))
        }
        "audio_url" => {
            let (url, _id) = parse_url_payload(payload, kind)?;
            Ok(Part::Content(ContentPart::AudioUrl {
                audio_url: AudioUrl { url },
            }))
        }
        "video_url" => {
            let (url, _id) = parse_url_payload(payload, kind)?;
            Ok(Part::Content(ContentPart::VideoUrl {
                video_url: VideoUrl { url },
            }))
        }
        "tool_call" => Ok(Part::ToolCall(parse_tool_call(payload, lineno, raw_line)?)),
        "tool_call_part" => Ok(Part::ToolCallPart(parse_tool_call_part(payload)?)),
        _ => Err(ChatProviderError::new(format!(
            "Unknown echo DSL kind '{}' at line {}: {:?}",
            kind, lineno, raw_line
        ))),
    }
}

fn parse_usage(payload: &str) -> Result<TokenUsage, ChatProviderError> {
    let mapping = parse_mapping(payload, "usage")?;

    let int_value = |key: &str| -> Result<usize, ChatProviderError> {
        let value = mapping.get(key).unwrap_or(&Value::Null);
        match value {
            Value::Number(n) => n.as_u64().map(|v| v as usize).ok_or_else(|| {
                ChatProviderError::new(format!(
                    "Usage field '{}' must be an integer, got {}",
                    key, value
                ))
            }),
            _ => Err(ChatProviderError::new(format!(
                "Usage field '{}' must be an integer, got {}",
                key, value
            ))),
        }
    };

    Ok(TokenUsage {
        input_other: int_value("input_other")?,
        output: int_value("output")?,
        input_cache_read: int_value("input_cache_read")?,
        input_cache_creation: int_value("input_cache_creation")?,
    })
}

fn parse_url_payload(
    payload: &str,
    kind: &str,
) -> Result<(String, Option<String>), ChatProviderError> {
    let value = parse_value(payload)?;
    match value {
        Value::Object(mapping) => {
            let url = mapping.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                ChatProviderError::new(format!("{} requires a url field, got {:?}", kind, mapping))
            })?;
            let content_id = mapping
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Ok((url.to_string(), content_id))
        }
        Value::String(s) => Ok((s, None)),
        other => Err(ChatProviderError::new(format!(
            "{} expects url string or object, got {:?}",
            kind, other
        ))),
    }
}

fn parse_tool_call(
    payload: &str,
    lineno: usize,
    raw_line: &str,
) -> Result<ToolCall, ChatProviderError> {
    let mapping = parse_mapping(payload, "tool_call")?;
    let function = mapping.get("function").and_then(|v| v.as_object()).cloned();

    let tool_call_id = mapping
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            function
                .as_ref()
                .and_then(|f| f.get("id").and_then(|v| v.as_str()))
        })
        .map(|s| s.to_string());

    let name = mapping
        .get("name")
        .and_then(|v| v.as_str())
        .or_else(|| {
            function
                .as_ref()
                .and_then(|f| f.get("name").and_then(|v| v.as_str()))
        })
        .map(|s| s.to_string());

    let arguments = mapping
        .get("arguments")
        .cloned()
        .or_else(|| function.as_ref().and_then(|f| f.get("arguments").cloned()));

    let extras = mapping
        .get("extras")
        .cloned()
        .or_else(|| function.as_ref().and_then(|f| f.get("extras").cloned()));

    let tool_call_id = tool_call_id.ok_or_else(|| {
        ChatProviderError::new(format!(
            "tool_call requires string id at line {}: {:?}",
            lineno, raw_line
        ))
    })?;
    let name = name.ok_or_else(|| {
        ChatProviderError::new(format!(
            "tool_call requires string name at line {}: {:?}",
            lineno, raw_line
        ))
    })?;

    let arguments = match arguments {
        Some(Value::String(s)) => Some(s),
        Some(other) => Some(other.to_string()),
        None => None,
    };

    let extras = match extras {
        Some(Value::Object(m)) => Some(m.into_iter().collect()),
        _ => None,
    };

    Ok(ToolCall {
        call_type: crate::ToolCallType::Function,
        id: tool_call_id,
        function: FunctionBody { name, arguments },
        extras,
    })
}

fn parse_tool_call_part(payload: &str) -> Result<ToolCallPart, ChatProviderError> {
    let value = parse_value(payload)?;
    let arguments_part = match value {
        Value::Object(mapping) => mapping.get("arguments_part").cloned(),
        other => Some(other),
    };

    let arguments_part = match arguments_part {
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => Some(s),
        Some(Value::Null) => None,
        Some(other) => Some(other.to_string()),
        None => None,
    };

    Ok(ToolCallPart { arguments_part })
}

fn parse_mapping(
    raw: &str,
    context: &str,
) -> Result<serde_json::Map<String, Value>, ChatProviderError> {
    let raw = raw.trim();

    // Try JSON first
    if let Ok(Value::Object(mapping)) = serde_json::from_str(raw) {
        return Ok(mapping);
    }

    // Try key=value tokens
    let mut mapping = serde_json::Map::new();
    for token in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let Some((key, value)) = token.split_once('=') else {
            return Err(ChatProviderError::new(format!(
                "Invalid token '{}' in {} payload.",
                token, context
            )));
        };
        mapping.insert(key.trim().to_string(), parse_value(value.trim())?);
    }

    if mapping.is_empty() {
        return Err(ChatProviderError::new(format!(
            "{} payload cannot be empty.",
            context
        )));
    }

    Ok(mapping)
}

fn parse_value(raw: &str) -> Result<Value, ChatProviderError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Value::Null);
    }
    let lowered = raw.to_lowercase();
    if lowered == "null" || lowered == "none" {
        return Ok(Value::Null);
    }
    // Try JSON first
    if let Ok(value) = serde_json::from_str(raw) {
        return Ok(value);
    }
    // Fall back to plain string
    Ok(Value::String(strip_quotes(raw).to_string()))
}

fn strip_quotes(value: &str) -> &str {
    let value = value.trim();
    let is_quoted =
        |quote: char| value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote);
    if is_quoted('"') || is_quoted('\'') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn raise_simulated_error(
    payload: &str,
    lineno: usize,
    raw_line: &str,
) -> Result<(), ChatProviderError> {
    let payload = payload.trim();
    if payload.is_empty() {
        return Err(ChatProviderError::new(format!(
            "Empty error payload at line {}: {:?}",
            lineno, raw_line
        )));
    }
    let (first, rest) = payload.split_once(' ').unwrap_or((payload, ""));
    let message = rest.trim();
    let lower = first.to_lowercase();

    if lower == "connection" {
        return Err(ChatProviderError::connection(message));
    }
    if lower == "timeout" {
        return Err(ChatProviderError::timeout(message));
    }

    let status_code: u16 = first.parse().map_err(|_| {
        ChatProviderError::new(format!(
            "Invalid error spec at line {}: expected status code or 'connection'/'timeout', got {:?}",
            lineno, first
        ))
    })?;

    let status_message = if message.is_empty() {
        format!("Simulated {} error", status_code)
    } else {
        message.to_string()
    };

    Err(ChatProviderError::status(status_code, status_message, None))
}
