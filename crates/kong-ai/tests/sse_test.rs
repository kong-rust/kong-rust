//! SSE 解析器测试

use kong_ai::codec::{SseFormat, SseParser};

#[test]
fn test_single_event_parsing() {
    // 单个完整事件
    let mut parser = SseParser::new(SseFormat::Standard);
    let events = parser.feed("data: hello world\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "hello world");
    assert_eq!(events[0].event_type, "message");
    assert_eq!(events[0].id, None);
}

#[test]
fn test_split_across_chunks() {
    // 事件被拆分到两个 chunk 中
    let mut parser = SseParser::new(SseFormat::Standard);

    let events1 = parser.feed("data: hel");
    assert_eq!(events1.len(), 0, "不完整的事件不应该被解析");

    let events2 = parser.feed("lo world\n\n");
    assert_eq!(events2.len(), 1);
    assert_eq!(events2[0].data, "hello world");
}

#[test]
fn test_done_terminator() {
    // [DONE] 终止事件
    let mut parser = SseParser::new(SseFormat::Standard);
    let events = parser.feed("data: [DONE]\n\n");
    assert_eq!(events.len(), 1);
    assert!(events[0].is_done());
}

#[test]
fn test_multiple_events_in_one_chunk() {
    // 一个 chunk 中包含多个事件
    let mut parser = SseParser::new(SseFormat::Standard);
    let chunk = "data: event1\n\ndata: event2\n\ndata: event3\n\n";
    let events = parser.feed(chunk);
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].data, "event1");
    assert_eq!(events[1].data, "event2");
    assert_eq!(events[2].data, "event3");
}

#[test]
fn test_empty_lines_between_events() {
    // 事件之间有空行（双换行之间的空行被忽略）
    let mut parser = SseParser::new(SseFormat::Standard);
    let chunk = "data: first\n\n\n\ndata: second\n\n";
    let events = parser.feed(chunk);
    // 空行之间会产生一个空事件（data 为空被过滤），所以只有 2 个有效事件
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data, "first");
    assert_eq!(events[1].data, "second");
}

#[test]
fn test_event_field_with_type() {
    // event: 字段指定事件类型
    let mut parser = SseParser::new(SseFormat::Standard);
    let chunk = "event: custom_type\ndata: payload\n\n";
    let events = parser.feed(chunk);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "custom_type");
    assert_eq!(events[0].data, "payload");
}

#[test]
fn test_id_field() {
    // id: 字段
    let mut parser = SseParser::new(SseFormat::Standard);
    let chunk = "id: 42\ndata: with id\n\n";
    let events = parser.feed(chunk);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, Some("42".to_string()));
    assert_eq!(events[0].data, "with id");
}

#[test]
fn test_multi_line_data() {
    // 多行 data 字段用 \n 连接
    let mut parser = SseParser::new(SseFormat::Standard);
    let chunk = "data: line1\ndata: line2\ndata: line3\n\n";
    let events = parser.feed(chunk);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "line1\nline2\nline3");
}

#[test]
fn test_ndjson_format() {
    // NDJSON 格式：每行一个 JSON 对象
    let mut parser = SseParser::new(SseFormat::Ndjson);
    let chunk = r#"{"id":"1","text":"hello"}
{"id":"2","text":"world"}
"#;
    let events = parser.feed(chunk);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data, r#"{"id":"1","text":"hello"}"#);
    assert_eq!(events[1].data, r#"{"id":"2","text":"world"}"#);
    assert_eq!(events[0].event_type, "message");
}

#[test]
fn test_ndjson_split_across_chunks() {
    // NDJSON 跨 chunk
    let mut parser = SseParser::new(SseFormat::Ndjson);

    let events1 = parser.feed(r#"{"id":"1","te"#);
    assert_eq!(events1.len(), 0);

    let events2 = parser.feed("xt\":\"hello\"}\n");
    assert_eq!(events2.len(), 1);
    assert_eq!(events2[0].data, r#"{"id":"1","text":"hello"}"#);
}

#[test]
fn test_flush_incomplete_standard() {
    // flush 处理未完成的标准 SSE 事件（缺少尾部双换行）
    let mut parser = SseParser::new(SseFormat::Standard);
    let events = parser.feed("data: incomplete");
    assert_eq!(events.len(), 0);

    let flushed = parser.flush();
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].data, "incomplete");
}

#[test]
fn test_flush_incomplete_ndjson() {
    // flush 处理未完成的 NDJSON
    let mut parser = SseParser::new(SseFormat::Ndjson);
    let events = parser.feed(r#"{"partial": true}"#);
    assert_eq!(events.len(), 0);

    let flushed = parser.flush();
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].data, r#"{"partial": true}"#);
}

#[test]
fn test_flush_empty_buffer() {
    // flush 空缓冲区
    let mut parser = SseParser::new(SseFormat::Standard);
    let flushed = parser.flush();
    assert_eq!(flushed.len(), 0);
}

#[test]
fn test_openai_stream_format() {
    // 模拟真实 OpenAI 流式响应格式
    let mut parser = SseParser::new(SseFormat::Standard);
    let chunk = concat!(
        "data: {\"id\":\"chatcmpl-123\",\"object\":\"chat.completion.chunk\",",
        "\"created\":1234567890,\"model\":\"gpt-4\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-123\",\"object\":\"chat.completion.chunk\",",
        "\"created\":1234567890,\"model\":\"gpt-4\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );

    let events = parser.feed(chunk);
    assert_eq!(events.len(), 3);
    assert!(!events[0].is_done());
    assert!(!events[1].is_done());
    assert!(events[2].is_done());
}
