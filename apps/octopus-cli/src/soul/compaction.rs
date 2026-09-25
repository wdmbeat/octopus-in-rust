use crate::exception::OctopusError;
use crate::llm::LLM;
use crate::soul::context::estimate_text_tokens;
use crate::soul::message::system;
use crate::wire::{ContentPart, Message, TokenUsage};

pub struct CompactionResult {
    pub messages: Vec<Message>,
    pub usage: Option<TokenUsage>,
}

impl CompactionResult {
    pub fn estimated_token_count(&self) -> usize {
        if let Some(ref usage) = self.usage
            && !self.messages.is_empty()
        {
            let summary_tokens = usage.output;
            let preserved_tokens = estimate_text_tokens(&self.messages[1..]);
            return summary_tokens + preserved_tokens;
        }
        estimate_text_tokens(&self.messages)
    }
}

pub fn should_auto_compact(
    token_count: usize,
    max_context_size: usize,
    trigger_ratio: f64,
    reserved_context_size: usize,
) -> bool {
    token_count >= (max_context_size as f64 * trigger_ratio) as usize
        || token_count + reserved_context_size >= max_context_size
}

pub struct SimpleCompaction {
    max_preserved_messages: usize,
}

impl SimpleCompaction {
    pub fn new(max_preserved_messages: usize) -> Self {
        Self {
            max_preserved_messages,
        }
    }

    pub async fn compact(
        &self,
        messages: &[Message],
        llm: &LLM,
        custom_instruction: &str,
    ) -> Result<CompactionResult, OctopusError> {
        let (compact_message, to_preserve) = self.prepare(messages, custom_instruction);

        if compact_message.is_none() {
            return Ok(CompactionResult {
                messages: to_preserve.into_iter().cloned().collect(),
                usage: None,
            });
        }

        // Call LLM to generate a compacted summary of the old context.
        // Mirrors the `llm_provider::step(...)` call in the original Python `compaction.py`.
        let system_prompt = "You are a helpful assistant that compacts conversation context.";
        let history = vec![compact_message.unwrap()];

        match llm.complete(Some(system_prompt), &history, None).await {
            Ok(completion) => {
                let mut content = vec![system(
                    "Previous context has been compacted. Here is the compaction output:",
                )];
                // Drop thinking parts if any (mirrors Python's `if not isinstance(part, ThinkPart)`)
                for part in &completion.message.content {
                    if !matches!(part, ContentPart::Think { .. }) {
                        content.push(part.clone());
                    }
                }

                let compacted_summary = Message {
                    role: "user".to_string(),
                    content,
                    tool_call_id: None,
                    tool_calls: None,
                };

                let mut result_messages = vec![compacted_summary];
                result_messages.extend(to_preserve.into_iter().cloned());

                Ok(CompactionResult {
                    messages: result_messages,
                    usage: completion.usage,
                })
            }
            Err(e) => {
                tracing::error!("Compaction LLM call failed: {}", e);
                Err(OctopusError::Other(format!("Compaction failed: {}", e)))
            }
        }
    }

    fn prepare<'a>(
        &self,
        messages: &'a [Message],
        custom_instruction: &str,
    ) -> (Option<Message>, Vec<&'a Message>) {
        if messages.is_empty() || self.max_preserved_messages == 0 {
            return (None, messages.iter().collect());
        }

        let mut n_preserved = 0;
        let mut preserve_start_index = messages.len();
        for (index, msg) in messages.iter().enumerate().rev() {
            if msg.role == "user" || msg.role == "assistant" {
                n_preserved += 1;
                if n_preserved == self.max_preserved_messages {
                    preserve_start_index = index;
                    break;
                }
            }
        }

        if n_preserved < self.max_preserved_messages {
            return (None, messages.iter().collect());
        }

        let to_compact = &messages[..preserve_start_index];
        let to_preserve: Vec<&Message> = messages[preserve_start_index..].iter().collect();

        if to_compact.is_empty() {
            return (None, to_preserve);
        }

        let mut compact_message = Message {
            role: "user".to_string(),
            content: Vec::new(),
            tool_call_id: None,
            tool_calls: None,
        };

        for (i, msg) in to_compact.iter().enumerate() {
            compact_message.content.push(ContentPart::Text {
                text: format!("## Message {}\nRole: {}\nContent:\n", i + 1, msg.role),
            });
            for part in &msg.content {
                if let ContentPart::Text { .. } = part {
                    compact_message.content.push(part.clone());
                }
            }
        }

        let mut prompt_text =
            "\nPlease compact the above conversation messages into a concise summary.".to_string();
        if !custom_instruction.is_empty() {
            prompt_text.push_str(&format!(
                "\n\n**User's Custom Compaction Instruction:**\nThe user has specifically requested the following focus during compaction. You MUST prioritize this instruction above the default compression priorities:\n{custom_instruction}"
            ));
        }
        compact_message
            .content
            .push(ContentPart::Text { text: prompt_text });

        (Some(compact_message), to_preserve)
    }
}

impl Default for SimpleCompaction {
    fn default() -> Self {
        Self::new(2)
    }
}
