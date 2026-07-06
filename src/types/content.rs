//! Content block types.
//!
//! Ported from the content-block dataclasses in the Python `types.py`
//! (`TextBlock`, `ThinkingBlock`, `ToolUseBlock`, `ToolResultBlock`,
//! `ServerToolUseBlock`, `ServerToolResultBlock`, and the `ContentBlock`
//! union). The wire representation is Anthropic's Messages API content-block
//! format, keyed on `type`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Text content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBlock {
    /// The text.
    pub text: String,
}

/// Thinking content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingBlock {
    /// The thinking text.
    pub thinking: String,
    /// Opaque signature that authenticates the thinking block.
    pub signature: String,
}

/// Tool use content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolUseBlock {
    /// Unique tool-use id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Tool input arguments.
    pub input: Map<String, Value>,
}

/// The `content` payload of a [`ToolResultBlock`] — either a plain string or a
/// list of content sub-blocks. Mirrors Python's `str | list[dict] | None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// A plain string result.
    Text(String),
    /// A list of structured content blocks.
    Blocks(Vec<Value>),
}

/// Tool result content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultBlock {
    /// Id of the tool use this result answers.
    pub tool_use_id: String,
    /// The result content, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ToolResultContent>,
    /// Whether the tool call errored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Server-side tool names the API may execute on the model's behalf. Mirrors
/// Python's `ServerToolName` literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerToolName {
    /// The advisor tool.
    Advisor,
    /// Web search.
    WebSearch,
    /// Web fetch.
    WebFetch,
    /// Code execution.
    CodeExecution,
    /// Bash code execution.
    BashCodeExecution,
    /// Text-editor code execution.
    TextEditorCodeExecution,
    /// Tool search (regex).
    ToolSearchToolRegex,
    /// Tool search (BM25).
    ToolSearchToolBm25,
}

/// Server-side tool use block (e.g. advisor, web_search, web_fetch).
///
/// These are tools the API executes server-side, so they appear in the message
/// stream alongside regular tool-use blocks but the caller never returns a
/// result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerToolUseBlock {
    /// Unique tool-use id.
    pub id: String,
    /// Which server tool was invoked.
    pub name: ServerToolName,
    /// Tool input arguments.
    pub input: Map<String, Value>,
}

/// Result block returned for a server-side tool call.
///
/// `content` is the raw dict from the API, opaque to this layer — callers that
/// care about a specific server tool's result schema can inspect
/// `content["type"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerToolResultBlock {
    /// Id of the server tool use this result answers.
    pub tool_use_id: String,
    /// Raw result content.
    pub content: Map<String, Value>,
}

/// A single content block within a message. Mirrors the `ContentBlock` union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// [`TextBlock`].
    Text(TextBlock),
    /// [`ThinkingBlock`].
    Thinking(ThinkingBlock),
    /// [`ToolUseBlock`].
    ToolUse(ToolUseBlock),
    /// [`ToolResultBlock`].
    ToolResult(ToolResultBlock),
    /// [`ServerToolUseBlock`].
    ServerToolUse(ServerToolUseBlock),
    /// [`ServerToolResultBlock`].
    ServerToolResult(ServerToolResultBlock),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_block_roundtrips_with_tag() {
        let block = ContentBlock::Text(TextBlock {
            text: "hi".into(),
        });
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(v, json!({"type": "text", "text": "hi"}));
        let back: ContentBlock = serde_json::from_value(v).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn tool_use_block_tag() {
        let v = json!({"type": "tool_use", "id": "t1", "name": "Bash", "input": {"cmd": "ls"}});
        let block: ContentBlock = serde_json::from_value(v).unwrap();
        match block {
            ContentBlock::ToolUse(b) => {
                assert_eq!(b.id, "t1");
                assert_eq!(b.name, "Bash");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn tool_result_content_variants() {
        let s: ToolResultContent = serde_json::from_value(json!("plain")).unwrap();
        assert_eq!(s, ToolResultContent::Text("plain".into()));
        let l: ToolResultContent =
            serde_json::from_value(json!([{"type": "text", "text": "x"}])).unwrap();
        assert!(matches!(l, ToolResultContent::Blocks(_)));
    }

    #[test]
    fn server_tool_name_snake_case() {
        assert_eq!(
            serde_json::to_value(ServerToolName::WebSearch).unwrap(),
            json!("web_search")
        );
    }
}
