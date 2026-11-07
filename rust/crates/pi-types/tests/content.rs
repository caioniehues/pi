//! Content blocks (`content.rs`): each block kind with its optional fields, and the unions that hold them.

mod common;

use common::{round_trip, write};
use pi_types::{AssistantContentBlock, ContentBlock, MessageContent, SystemContent, TextContent, ToolCall};

#[test]
fn assistant_blocks_round_trip_with_optional_fields() {
	let blocks: Vec<AssistantContentBlock> = round_trip(
		r#"[{"type":"text","text":"a","textSignature":"{\"v\":1,\"id\":\"msg_1\"}"},{"type":"thinking","thinking":"t","thinkingSignature":"sig"},{"type":"toolCall","id":"c","name":"n","arguments":{},"thoughtSignature":"ts","namespace":"mcp","argumentsError":"not JSON: {"}]"#,
	);
	assert!(matches!(&blocks[2], AssistantContentBlock::ToolCall(call) if call.namespace.as_deref() == Some("mcp")));
}

#[test]
fn a_standalone_tool_call_writes_its_type() {
	let call: ToolCall =
		round_trip(r#"{"type":"toolCall","id":"c","name":"n","arguments":{"a":[1]},"partialJson":"{\"a\""}"#);
	assert!(call.extra.contains_key("partialJson"));
}

#[test]
fn user_content_is_text_or_blocks() {
	assert_eq!(
		round_trip::<MessageContent>(r#""plain""#),
		MessageContent::Text("plain".into())
	);
	let blocks: MessageContent =
		round_trip(r#"[{"type":"text","text":"t"},{"type":"image","data":"AA==","mimeType":"image/gif"}]"#);
	assert!(matches!(blocks, MessageContent::Blocks(blocks) if matches!(blocks[1], ContentBlock::Image(_))));
	// Tool calls are not user content.
	assert!(common::read::<ContentBlock>(r#"{"type":"toolCall","id":"c","name":"n","arguments":{}}"#).is_err());
}

#[test]
fn system_content_is_text_or_text_blocks() {
	round_trip::<SystemContent>(r#""s""#);
	round_trip::<SystemContent>(r#"[{"type":"text","text":"s"}]"#);
	assert_eq!(write(&TextContent::new("x")), r#"{"type":"text","text":"x"}"#);
}
