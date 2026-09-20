//! 极简 HTTP/SSE **客户端**：给 CLI 的 `stream` 子命令和集成测试用。
//!
//! 为什么不引 HTTP 客户端库：我们只需要"GET 一条流并按行读"，而依赖是要还的
//! （体积、供应链、编译时间）。这里 200 行把 HTTP/1.1 的 `Content-Length` 与
//! `Transfer-Encoding: chunked` 两种响应都处理掉，够用且能读。
//!
//! 只支持 `http://`（明文）。远程/跨网请把服务放在 TLS 之后，或用
//! `curl -N` 检查——**本模块不假装自己支持 HTTPS**。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use rsi3d_harness_stream::protocol::{ClientDeclaration, ServerMessage, StreamKind};

/// 一个已建立的 SSE 连接。
pub struct Peer {
    body: Body,
    /// 收到过的最新事件 id（= 状态版本）——重连时拿它当 `Last-Event-ID`
    pub last_id: Option<u32>,
    pub status: u16,
    pub content_type: String,
}

struct Conn {
    sock: TcpStream,
    buf: Vec<u8>,
    pos: usize,
}

impl Conn {
    fn fill(&mut self) -> std::io::Result<usize> {
        let mut tmp = [0u8; 8192];
        let n = self.sock.read(&mut tmp)?;
        self.buf.extend_from_slice(&tmp[..n]);
        Ok(n)
    }

    fn byte(&mut self) -> std::io::Result<u8> {
        while self.pos >= self.buf.len() {
            if self.fill()? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "连接被关闭",
                ));
            }
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn line(&mut self) -> std::io::Result<String> {
        let mut out = Vec::new();
        loop {
            let b = self.byte()?;
            if b == b'\n' {
                break;
            }
            if b != b'\r' {
                out.push(b);
            }
        }
        Ok(String::from_utf8_lossy(&out).to_string())
    }
}

/// 响应体：透明处理 chunked 与 content-length。
struct Body {
    conn: Conn,
    /// 当前 chunk 还剩多少字节（chunked 时）
    chunk_left: usize,
    chunked: bool,
    /// content-length 模式还剩多少
    remain: Option<usize>,
}

impl Body {
    fn read_byte(&mut self) -> std::io::Result<Option<u8>> {
        if let Some(r) = self.remain {
            if r == 0 {
                return Ok(None);
            }
        }
        if self.chunked {
            if self.chunk_left == 0 {
                let line = self.conn.line()?;
                let size = line
                    .trim()
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let n = usize::from_str_radix(&size, 16).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "chunk 长度不合法")
                })?;
                if n == 0 {
                    return Ok(None);
                }
                self.chunk_left = n;
            }
            let b = self.conn.byte()?;
            self.chunk_left -= 1;
            if self.chunk_left == 0 {
                let _ = self.conn.line()?; // 数据后的 CRLF
            }
            return Ok(Some(b));
        }
        match self.remain {
            Some(0) => Ok(None),
            Some(r) => {
                let b = self.conn.byte()?;
                self.remain = Some(r - 1);
                Ok(Some(b))
            }
            None => Ok(None),
        }
    }

    fn read_line(&mut self) -> std::io::Result<Option<String>> {
        let mut out = Vec::new();
        loop {
            match self.read_byte()? {
                None => {
                    return if out.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(String::from_utf8_lossy(&out).to_string()))
                    }
                }
                Some(b'\n') => return Ok(Some(String::from_utf8_lossy(&out).to_string())),
                Some(b'\r') => {}
                Some(b) => out.push(b),
            }
        }
    }
}

/// 订阅地址 + 读超时都由调用方决定：**空闲不是错误**，得能区分开。
pub fn connect(base: &str, path: &str) -> Result<Peer, String> {
    connect_with_timeout(base, path, Duration::from_secs(30))
}

/// 带自定义读超时的连接（空闲多少秒算"没动静了"）。
pub fn connect_with_timeout(base: &str, path: &str, read_timeout: Duration) -> Result<Peer, String> {
    let hostport = base
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .to_string();
    if base.starts_with("https://") {
        return Err("本客户端只支持 http://（放到 TLS 后面，或用 curl -N）".to_string());
    }
    let sock = TcpStream::connect(&hostport).map_err(|e| format!("连不上 {}：{}", hostport, e))?;
    sock.set_read_timeout(Some(read_timeout)).ok();
    sock.set_nodelay(true).ok();
    let mut sock = sock;

    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n",
        path, hostport
    );
    sock.write_all(req.as_bytes())
        .map_err(|e| format!("发送请求失败：{}", e))?;

    let mut conn = Conn {
        sock,
        buf: Vec::new(),
        pos: 0,
    };
    // 状态行
    let status_line = conn.line().map_err(|e| format!("读响应失败：{}", e))?;
    let mut parts = status_line.split_whitespace();
    let _http = parts.next();
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("响应状态行不合法：{}", status_line))?;

    let mut chunked = false;
    let mut content_length: Option<usize> = None;
    let mut content_type = String::new();
    loop {
        let line = conn.line().map_err(|e| format!("读响应头失败：{}", e))?;
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').unwrap_or((line.as_str(), ""));
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().to_string();
        match k.as_str() {
            "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
            "content-length" => content_length = v.parse().ok(),
            "content-type" => content_type = v,
            _ => {}
        }
    }

    let body = Body {
        conn,
        chunk_left: 0,
        chunked,
        remain: content_length,
    };
    Ok(Peer {
        body,
        last_id: None,
        status,
        content_type,
    })
}

/// 一次性 GET（拿 JSON 用；流请用 [`connect`]）。
pub fn get(base: &str, path: &str) -> Result<(u16, String), String> {
    one_shot(base, "GET", path, "")
}

/// 一行上行的 POST（一次性：发完读到底就关）。
pub fn post(base: &str, path: &str, body: &str) -> Result<(u16, String), String> {
    one_shot(base, "POST", path, body)
}

fn one_shot(base: &str, method: &str, path: &str, body: &str) -> Result<(u16, String), String> {
    let hostport = base
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let mut sock =
        TcpStream::connect(&hostport).map_err(|e| format!("连不上 {}：{}", hostport, e))?;
    sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let req = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        method,
        path,
        hostport,
        body.len(),
        body
    );
    sock.write_all(req.as_bytes())
        .map_err(|e| format!("发送失败：{}", e))?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).ok();
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body = match text.split_once("\r\n\r\n") {
        Some((_, b)) => b.to_string(),
        None => String::new(),
    };
    Ok((status, body))
}

/// 订阅时可能遇到的两种情况——**必须分开**：
///
/// - 读超时到了只是因为"没新消息"（静止场景本就不该有流量）→ [`PeerError::Idle`]；
/// - 其它才是真的坏了。
#[derive(Debug)]
pub enum PeerError {
    /// 超过读超时没收到任何东西（流还活着，只是没动静）
    Idle,
    Fatal(String),
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerError::Idle => write!(f, "空闲超时"),
            PeerError::Fatal(e) => write!(f, "{}", e),
        }
    }
}

fn is_idle(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

impl Peer {
    /// 取下一条消息。`Ok(None)` = 流正常结束。
    pub fn next_message(&mut self) -> Result<Option<ServerMessage>, PeerError> {
        let mut data = String::new();
        loop {
            let line = match self.body.read_line() {
                Ok(Some(l)) => l,
                Ok(None) => {
                    return if data.is_empty() {
                        Ok(None)
                    } else {
                        Err(PeerError::Fatal("流在事件中途结束".to_string()))
                    }
                }
                Err(e) if is_idle(&e) => return Err(PeerError::Idle),
                Err(e) => return Err(PeerError::Fatal(format!("读流失败：{}", e))),
            };
            if line.is_empty() {
                if data.is_empty() {
                    continue; // 心跳/空行
                }
                return match serde_json::from_str::<ServerMessage>(&data) {
                    Ok(m) => Ok(Some(m)),
                    Err(e) => Err(PeerError::Fatal(format!(
                        "事件不是合法消息：{}（原文前 120 字：{}）",
                        e,
                        &data[..data.len().min(120)]
                    ))),
                };
            }
            if let Some(v) = line.strip_prefix("id:") {
                self.last_id = v.trim().parse::<u32>().ok().or(self.last_id);
            } else if let Some(v) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(v.trim_start());
            }
            // event: / : 心跳 都忽略——真相在 data 的 JSON 里
        }
    }
}

/// 组合一个订阅地址（token 走 query：`EventSource` 无法设请求头）。
pub fn stream_path(kind: StreamKind, view: Option<&str>, token: &str, from: Option<u32>) -> String {
    stream_path_with_client(kind, view, token, from, &ClientDeclaration::default())
}

/// 带能力声明的订阅地址。
///
/// 声明走**查询参数**而不是模型里的 `ClientMessage`：一条 SSE 连接就是一次订阅，
/// 它是唯一天然带"连接身份"的位置（POST 通道归属不到具体的连接）。
pub fn stream_path_with_client(
    kind: StreamKind,
    view: Option<&str>,
    token: &str,
    from: Option<u32>,
    client: &ClientDeclaration,
) -> String {
    let mut p = format!("/stream/{}?token={}", kind.as_str(), urlencode(token));
    if let Some(v) = view {
        p.push_str(&format!("&view={}", urlencode(v)));
    }
    if let Some(f) = from {
        p.push_str(&format!("&from={}", f));
    }
    p.push_str(&format!("&agent={}", urlencode(&client.agent)));
    if !client.capabilities.is_empty() {
        p.push_str(&format!(
            "&cap={}",
            urlencode(&client.capabilities.join(","))
        ));
    }
    p
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
