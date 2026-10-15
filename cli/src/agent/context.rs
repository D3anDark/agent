use serde_json::Value;

/// /context rows, in this order.
pub const PARTS: [&str; 5] = ["System prompt", "Tool schemas", "Messages", "Tool results", "Images"];

// Anthropic caps an image near 1,600 tokens; at ~4 bytes per token that is 6,400 bytes, far below its base64 size.
const IMAGE_BYTES: u64 = 6_400;

/// Splits `total` context tokens across `PARTS` in proportion to the bytes each part sends.
pub fn split(system: &str, tools: &Value, messages: &[Value], total: u64) -> [u64; 5] {
    let mut bytes = [system.len() as u64, tools.to_string().len() as u64, 0, 0, 0];
    for message in messages {
        let Some(blocks) = message["content"].as_array() else {
            bytes[2] += message["content"].to_string().len() as u64;
            continue;
        };
        for block in blocks {
            match block["type"].as_str() {
                Some("image") => bytes[4] += IMAGE_BYTES,
                Some("tool_result") => add_tool_result(&mut bytes, &block["content"]),
                _ => bytes[2] += block.to_string().len() as u64,
            }
        }
    }
    let sum: u64 = bytes.iter().sum();
    bytes.map(|part| (part * total).checked_div(sum).unwrap_or(0))
}

fn add_tool_result(bytes: &mut [u64; 5], content: &Value) {
    let Some(blocks) = content.as_array() else {
        bytes[3] += content.to_string().len() as u64;
        return;
    };
    for block in blocks {
        if block["type"] == "image" {
            bytes[4] += IMAGE_BYTES;
        } else {
            bytes[3] += block.to_string().len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_scales_each_part_by_its_bytes_and_counts_images_by_estimate() {
        let image = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "A".repeat(100_000)}});
        let messages = [
            json!({"role": "user", "content": "x".repeat(98)}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "read", "input": {}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "y"}, image]}]}),
        ];
        let tool_use_bytes = messages[1]["content"][0].to_string().len() as u64;
        let result_bytes = json!({"type": "text", "text": "y"}).to_string().len() as u64;
        let tools = json!([]);
        let bytes = [100, 2, 100 + tool_use_bytes, result_bytes, IMAGE_BYTES];
        let sum: u64 = bytes.iter().sum();

        assert_eq!(split(&"s".repeat(100), &tools, &messages, sum * 2), bytes.map(|part| part * 2));
        assert_eq!(split("", &tools, &[], 0), [0; 5]);
    }
}
