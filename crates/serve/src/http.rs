//! 最小 HTTP/1.1 传输（`std::net`，无依赖）。
//!
//! # 为什么不用现成的 HTTP 库
//!
//! 先说结论：**试过 `tiny_http`，它做不了 SSE**。原因很具体：
//! 它把响应体交给 `chunked_transfer::Encoder`，而那个 Encoder 会先把数据攒进自己的
//! 缓冲区（到 4KB 才发一块），`flush()` 又只在**整个响应结束时**被调用一次。
//! 结果是：一次几百字节的增量永远发不出去——`curl -N` 连响应头都收不到。
//! 这不是参数没调对，是"缓冲整个响应"和"流"在骨子里冲突。
//!
//! 于是这里自己写：只支持我们真正需要的形状——
//!
//! - 请求：`请求行 + 头 + 可选 body`（有 `Content-Length` 上限保护）；
//! - 响应：定长（`Content-Length`）或 **分块（chunked）**；
//! - 分块响应**每块写完立刻 flush**，这才是"推流"。
//!
//! 换来的是：零依赖、行为完全可见、出问题时能一眼看懂。代价是：
//! 没有 TLS、没有 keep-alive 复用、没有 HTTP/2——对我们（本机/内网的观测口）都不需要。
//! 要对外暴露请放在反向代理后面（它负责 TLS 与并发）。

use std::io::{Read, Write};
use std::net::TcpStream;

/// 请求体上限：上行只有命令信封，64KB 足够；也顺手挡住了"拿它当上传口"。
pub const MAX_BODY: usize = 64 * 1024;
/// 请求头上限（防御畸形请求）。
const MAX_HEAD: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    /// (主, 次)，如 (1, 1)
    pub version: (u8, u8),
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn http11(&self) -> bool {
        self.version >= (1, 1)
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// 读一个请求。返回 `Ok(None)` = 对端正常关闭。
pub fn read_request(sock: &mut TcpStream) -> Result<Option<Request>, String> {
    // ---- 头部：一直读到空行
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match sock.read(&mut byte) {
            Ok(0) => {
                return if head.is_empty() {
                    Ok(None)
                } else {
                    Err("请求头未读完连接就断了".to_string())
                }
            }
            Ok(_) => {
                head.push(byte[0]);
                if head.len() >= 4 && &head[head.len() - 4..] == b"\r\n\r\n" {
                    break;
                }
                if head.len() > MAX_HEAD {
                    return Err("请求头过大".to_string());
                }
            }
            Err(e) => return Err(format!("读请求失败：{}", e)),
        }
    }

    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let version = match parts.next().unwrap_or("HTTP/1.1") {
        "HTTP/1.0" => (1, 0),
        _ => (1, 1),
    };
    if method.is_empty() {
        return Err("请求行不合法".to_string());
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    // ---- body（只支持 Content-Length；不支持 chunked 请求体）
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return Err(format!("请求体过大（{} > {}）", len, MAX_BODY));
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        sock.read_exact(&mut body)
            .map_err(|e| format!("读请求体失败：{}", e))?;
    }

    Ok(Some(Request {
        method,
        path,
        query,
        version,
        headers,
        body,
    }))
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        426 => "Upgrade Required",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

/// 定长响应（写完就关连接）：JSON / HTML / PNG 都走这里。
pub fn respond(
    sock: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, &str)],
) -> Result<(), String> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        code,
        status_text(code),
        content_type,
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{}: {}\r\n", k, v));
    }
    head.push_str("\r\n");
    sock.write_all(head.as_bytes())
        .and_then(|_| sock.write_all(body))
        .and_then(|_| sock.flush())
        .map_err(|e| format!("写响应失败：{}", e))
}

/// JSON 响应。
pub fn respond_json(sock: &mut TcpStream, code: u16, body: &str) -> Result<(), String> {
    respond(
        sock,
        code,
        "application/json; charset=utf-8",
        body.as_bytes(),
        &[("Access-Control-Allow-Origin", "*")],
    )
}

/// 开始一个分块响应（SSE 用）。**头立刻下发**，之后每块自带长度并立即 flush。
pub fn start_chunked(sock: &mut TcpStream, content_type: &str) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nCache-Control: no-cache, no-transform\r\n\
         X-Accel-Buffering: no\r\nAccess-Control-Allow-Origin: *\r\n\
         Transfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
        content_type
    );
    sock.write_all(head.as_bytes())
        .and_then(|_| sock.flush())
        .map_err(|e| format!("写响应头失败：{}", e))
}

/// 发一块。`Err` = 对端已经走了（调用方应当结束这条连接）。
pub fn send_chunk(sock: &mut TcpStream, data: &[u8]) -> Result<(), String> {
    if data.is_empty() {
        return Ok(());
    }
    let mut framed = format!("{:x}\r\n", data.len()).into_bytes();
    framed.extend_from_slice(data);
    framed.extend_from_slice(b"\r\n");
    sock.write_all(&framed)
        .and_then(|_| sock.flush()) // ← 这一步才是「推」
        .map_err(|e| format!("写流失败：{}", e))
}

/// 结束分块流（空块）。
pub fn end_chunked(sock: &mut TcpStream) {
    let _ = sock.write_all(b"0\r\n\r\n");
    let _ = sock.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_covers_what_we_return() {
        assert_eq!(status_text(401), "Unauthorized");
        assert_eq!(status_text(409), "Conflict");
        assert_eq!(status_text(426), "Upgrade Required");
    }
}
