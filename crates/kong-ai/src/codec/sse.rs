//! SSE（Server-Sent Events）流式解析器
//! 支持标准 SSE 格式和 NDJSON 格式，可跨 chunk 重组事件

/// SSE 事件
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    /// 事件类型（event: 字段），默认 "message"
    pub event_type: String,
    /// 数据内容（data: 字段，多行 data 用 \n 连接）
    pub data: String,
    /// 事件 ID（id: 字段）
    pub id: Option<String>,
}

impl SseEvent {
    /// 判断是否为 [DONE] 终止事件
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

/// SSE 格式类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SseFormat {
    /// 标准 SSE 格式（data: / event: / id: 前缀，双换行分隔）
    Standard,
    /// NDJSON 格式（每行一个 JSON 对象）
    Ndjson,
}

/// SSE 流式解析器 — 支持跨 chunk 重组
pub struct SseParser {
    format: SseFormat,
    /// 未完成的数据缓冲区
    buffer: String,
}

impl SseParser {
    /// 创建新的 SSE 解析器
    pub fn new(format: SseFormat) -> Self {
        Self {
            format,
            buffer: String::new(),
        }
    }

    /// 送入一块数据，返回已完成的事件列表
    pub fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buffer.push_str(chunk);

        match self.format {
            SseFormat::Standard => self.parse_standard(),
            SseFormat::Ndjson => self.parse_ndjson(),
        }
    }

    /// 刷新缓冲区，返回残留的事件（流结束时调用）
    pub fn flush(&mut self) -> Vec<SseEvent> {
        match self.format {
            SseFormat::Standard => {
                // 尝试解析缓冲区中剩余的内容（可能缺少尾部双换行）
                if self.buffer.trim().is_empty() {
                    return Vec::new();
                }
                let remaining = std::mem::take(&mut self.buffer);
                let mut events = Vec::new();
                let event = Self::parse_single_event(&remaining);
                if !event.data.is_empty() {
                    events.push(event);
                }
                events
            }
            SseFormat::Ndjson => {
                let remaining = std::mem::take(&mut self.buffer);
                let trimmed = remaining.trim();
                if trimmed.is_empty() {
                    return Vec::new();
                }
                vec![SseEvent {
                    event_type: "message".to_string(),
                    data: trimmed.to_string(),
                    id: None,
                }]
            }
        }
    }

    /// 解析标准 SSE 格式：双换行 (\n\n) 分隔事件
    fn parse_standard(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();

        loop {
            // 查找双换行边界
            let boundary = self.buffer.find("\n\n");
            match boundary {
                Some(pos) => {
                    let event_text: String = self.buffer.drain(..pos + 2).collect();
                    let event = Self::parse_single_event(&event_text);
                    if !event.data.is_empty() {
                        events.push(event);
                    }
                }
                None => break,
            }
        }

        events
    }

    /// 解析单个 SSE 事件块
    fn parse_single_event(text: &str) -> SseEvent {
        let mut event_type = "message".to_string();
        let mut data_lines: Vec<String> = Vec::new();
        let mut id: Option<String> = None;

        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            if let Some(value) = line.strip_prefix("data:") {
                // data: 后可能有空格，去掉前导空格
                data_lines.push(value.trim_start().to_string());
            } else if let Some(value) = line.strip_prefix("event:") {
                event_type = value.trim_start().to_string();
            } else if let Some(value) = line.strip_prefix("id:") {
                id = Some(value.trim_start().to_string());
            }
            // 忽略 retry: 和注释行（以 : 开头）
        }

        SseEvent {
            event_type,
            data: data_lines.join("\n"),
            id,
        }
    }

    /// 解析 NDJSON 格式：每行一个 JSON 对象
    fn parse_ndjson(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();

        loop {
            let newline_pos = self.buffer.find('\n');
            match newline_pos {
                Some(pos) => {
                    let line: String = self.buffer.drain(..pos + 1).collect();
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    events.push(SseEvent {
                        event_type: "message".to_string(),
                        data: trimmed.to_string(),
                        id: None,
                    });
                }
                None => break,
            }
        }

        events
    }
}
