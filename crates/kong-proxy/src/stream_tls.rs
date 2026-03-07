//! TLS ClientHello SNI 解析 — 从原始字节中提取 SNI
//!
//! 用于 TLS Passthrough 模式：peek 首字节后解析 ClientHello，
//! 提取 SNI 用于路由匹配，然后原样转发加密数据。
//!
//! 解析路径：
//! TLS Record Header (5 bytes)
//!   → Handshake Header (4 bytes)
//!     → ClientHello
//!       → Session ID (skip)
//!       → Cipher Suites (skip)
//!       → Compression Methods (skip)
//!       → Extensions
//!         → SNI Extension (type 0x0000)
//!           → ServerNameList
//!             → HostName

/// 判断首字节是否为 TLS Handshake record（content type 0x16）
pub fn is_tls_handshake(byte: u8) -> bool {
    byte == 0x16
}

/// 从 peek 到的缓冲区中解析 TLS ClientHello 的 SNI
///
/// 返回 None 表示：
/// - 不是有效的 TLS ClientHello
/// - ClientHello 不包含 SNI extension
/// - 缓冲区不够长（但不会 panic）
pub fn parse_sni_from_client_hello(buf: &[u8]) -> Option<String> {
    let mut pos = 0;

    // === TLS Record Header (5 bytes) ===
    // ContentType(1) + ProtocolVersion(2) + Length(2)
    if buf.len() < 5 {
        return None;
    }
    if buf[0] != 0x16 {
        return None; // 不是 Handshake
    }
    // 跳过 version (2 bytes) 和 record length (2 bytes)
    pos += 5;

    // === Handshake Header (4 bytes) ===
    // HandshakeType(1) + Length(3)
    if buf.len() < pos + 4 {
        return None;
    }
    if buf[pos] != 0x01 {
        return None; // 不是 ClientHello
    }
    pos += 4; // 跳过 handshake type + length

    // === ClientHello 内容 ===
    // ProtocolVersion(2)
    if buf.len() < pos + 2 {
        return None;
    }
    pos += 2;

    // Random(32)
    if buf.len() < pos + 32 {
        return None;
    }
    pos += 32;

    // Session ID: length(1) + data
    if buf.len() < pos + 1 {
        return None;
    }
    let session_id_len = buf[pos] as usize;
    pos += 1 + session_id_len;

    // Cipher Suites: length(2) + data
    if buf.len() < pos + 2 {
        return None;
    }
    let cipher_suites_len = read_u16(buf, pos) as usize;
    pos += 2 + cipher_suites_len;

    // Compression Methods: length(1) + data
    if buf.len() < pos + 1 {
        return None;
    }
    let comp_methods_len = buf[pos] as usize;
    pos += 1 + comp_methods_len;

    // === Extensions ===
    // Extensions length(2)
    if buf.len() < pos + 2 {
        return None;
    }
    let extensions_len = read_u16(buf, pos) as usize;
    pos += 2;

    let extensions_end = pos + extensions_len;
    if buf.len() < extensions_end {
        // 缓冲区不够，但仍尝试解析已有数据
    }

    // 遍历 extensions 查找 SNI (type = 0x0000)
    while pos + 4 <= buf.len() && pos + 4 <= extensions_end {
        let ext_type = read_u16(buf, pos);
        let ext_len = read_u16(buf, pos + 2) as usize;
        pos += 4;

        if ext_type == 0x0000 {
            // SNI Extension
            return parse_sni_extension(&buf[pos..buf.len().min(pos + ext_len)]);
        }

        pos += ext_len;
    }

    None
}

/// 解析 SNI Extension 的内容
///
/// 格式：ServerNameList length(2) + [NameType(1) + HostName length(2) + HostName]
fn parse_sni_extension(data: &[u8]) -> Option<String> {
    if data.len() < 2 {
        return None;
    }

    let _list_len = read_u16(data, 0) as usize;
    let mut pos = 2;

    // 遍历 ServerName 列表
    while pos + 3 <= data.len() {
        let name_type = data[pos];
        let name_len = read_u16(data, pos + 1) as usize;
        pos += 3;

        if name_type == 0x00 {
            // host_name 类型
            if pos + name_len <= data.len() {
                return String::from_utf8(data[pos..pos + name_len].to_vec()).ok();
            }
        }

        pos += name_len;
    }

    None
}

/// 读取大端序 u16
fn read_u16(buf: &[u8], pos: usize) -> u16 {
    ((buf[pos] as u16) << 8) | (buf[pos + 1] as u16)
}

/// ClientHello 的推荐 peek 缓冲区大小
///
/// 大多数 ClientHello 在 512 字节内包含 SNI extension。
/// 使用 1024 字节提供足够余量（某些客户端可能有较长的 session ticket）。
pub const CLIENT_HELLO_PEEK_SIZE: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_tls_handshake() {
        assert!(is_tls_handshake(0x16));
        assert!(!is_tls_handshake(0x17)); // Application Data
        assert!(!is_tls_handshake(0x00));
        assert!(!is_tls_handshake(b'G')); // HTTP GET
    }

    #[test]
    fn test_parse_sni_from_real_client_hello() {
        // 手工构造一个最小的 TLS 1.2 ClientHello，包含 SNI "example.com"
        let sni = b"example.com";
        let sni_ext = build_sni_extension(sni);
        let client_hello = build_client_hello(&sni_ext);
        let record = build_tls_record(&client_hello);

        let result = parse_sni_from_client_hello(&record);
        assert_eq!(result, Some("example.com".to_string()));
    }

    #[test]
    fn test_parse_no_sni() {
        // ClientHello 无 extensions
        let client_hello = build_client_hello_no_ext();
        let record = build_tls_record(&client_hello);

        let result = parse_sni_from_client_hello(&record);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_truncated_buffer() {
        assert!(parse_sni_from_client_hello(&[]).is_none());
        assert!(parse_sni_from_client_hello(&[0x16, 0x03, 0x01]).is_none());
        assert!(parse_sni_from_client_hello(&[0x17, 0x03, 0x01, 0x00, 0x05]).is_none());
    }

    #[test]
    fn test_parse_non_tls() {
        // HTTP 请求
        assert!(parse_sni_from_client_hello(b"GET / HTTP/1.1\r\n").is_none());
    }

    // ===== 测试辅助构造函数 =====

    fn build_sni_extension(hostname: &[u8]) -> Vec<u8> {
        let mut ext = Vec::new();
        // Extension type: SNI (0x0000)
        ext.push(0x00);
        ext.push(0x00);
        // Extension data length
        let sni_list_len = 3 + hostname.len(); // NameType(1) + NameLen(2) + Name
        let ext_data_len = 2 + sni_list_len; // ListLen(2) + sni_list
        ext.push((ext_data_len >> 8) as u8);
        ext.push(ext_data_len as u8);
        // ServerNameList length
        ext.push((sni_list_len >> 8) as u8);
        ext.push(sni_list_len as u8);
        // NameType: host_name (0)
        ext.push(0x00);
        // HostName length
        ext.push((hostname.len() >> 8) as u8);
        ext.push(hostname.len() as u8);
        // HostName
        ext.extend_from_slice(hostname);
        ext
    }

    fn build_client_hello(extensions: &[u8]) -> Vec<u8> {
        let mut hello = Vec::new();
        // Handshake type: ClientHello (1)
        hello.push(0x01);
        // 占位 length (3 bytes)，最后填充
        hello.push(0x00);
        hello.push(0x00);
        hello.push(0x00);

        let body_start = hello.len();

        // Protocol version: TLS 1.2
        hello.push(0x03);
        hello.push(0x03);
        // Random (32 bytes)
        hello.extend_from_slice(&[0u8; 32]);
        // Session ID length: 0
        hello.push(0x00);
        // Cipher Suites length: 2 (one suite)
        hello.push(0x00);
        hello.push(0x02);
        hello.push(0x00);
        hello.push(0xFF); // TLS_EMPTY_RENEGOTIATION_INFO_SCSV
        // Compression Methods length: 1
        hello.push(0x01);
        hello.push(0x00); // null compression
        // Extensions length
        hello.push((extensions.len() >> 8) as u8);
        hello.push(extensions.len() as u8);
        // Extensions
        hello.extend_from_slice(extensions);

        // 回填 handshake length
        let body_len = hello.len() - body_start;
        hello[1] = ((body_len >> 16) & 0xFF) as u8;
        hello[2] = ((body_len >> 8) & 0xFF) as u8;
        hello[3] = (body_len & 0xFF) as u8;

        hello
    }

    fn build_client_hello_no_ext() -> Vec<u8> {
        let mut hello = Vec::new();
        hello.push(0x01);
        hello.push(0x00);
        hello.push(0x00);
        hello.push(0x00);

        let body_start = hello.len();

        hello.push(0x03);
        hello.push(0x03);
        hello.extend_from_slice(&[0u8; 32]);
        hello.push(0x00); // Session ID length: 0
        hello.push(0x00);
        hello.push(0x02);
        hello.push(0x00);
        hello.push(0xFF);
        hello.push(0x01);
        hello.push(0x00);
        // 无 extensions

        let body_len = hello.len() - body_start;
        hello[1] = ((body_len >> 16) & 0xFF) as u8;
        hello[2] = ((body_len >> 8) & 0xFF) as u8;
        hello[3] = (body_len & 0xFF) as u8;

        hello
    }

    fn build_tls_record(handshake: &[u8]) -> Vec<u8> {
        let mut record = Vec::new();
        // Content type: Handshake (0x16)
        record.push(0x16);
        // Protocol version: TLS 1.0
        record.push(0x03);
        record.push(0x01);
        // Length
        record.push((handshake.len() >> 8) as u8);
        record.push(handshake.len() as u8);
        // Handshake data
        record.extend_from_slice(handshake);
        record
    }
}
