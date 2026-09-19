//! 传输层：把 [`rsi3d_harness_stream`] 的会话跑在 HTTP 上。
//!
//! # 为什么是 SSE + POST，而不是 WebSocket
//!
//! 两条流都是**服务端单向推**，客户端只需要偶发地发命令。这个形状下：
//!
//! - `EventSource` 自带**自动重连**，而且会把上次的 `id:` 作为 `Last-Event-ID` 回传——
//!   我们的 `id` 就是状态版本，于是「断线续传」是**免费**的（服务端能重放任意历史版本）。
//! - `curl -N` 就能验收，不需要客户端库。
//! - 反方向用普通 `POST`（发命令/换相机），语义清楚，也天然带 4xx 错误返回。
//!
//! WebSocket 会更好的一点是双向低延迟与二进制帧，但那是**换传输**、不是换设计：
//! 会话层（`Subscription`/`Outbox`）与协议层（`ServerMessage`）都没碰 HTTP，
//! 将来换 WS/WebRTC 只改本文件。
//!
//! # 为什么 HTTP 是最小自写的（见 [`http`]）
//!
//! 我们先用了 `tiny_http`，然后发现**它做不了 SSE**：分块编码器会把小块攒到 4KB 才发，
//! 而 `flush` 只在响应结束时调一次——所以小增量永远出不去。
//! 这不是参数没调对，而是「缓冲整个响应」与「流」的冲突，所以自己写了 200 行。
//!
//! # 安全边界
//!
//! 默认只绑 `127.0.0.1`，并且**必须带一次性 token**（`?token=` 或 `Authorization: Bearer`）。
//! 这不是装饰：图像流和场景流都可能带着客户数据，绑定非回环地址时由调用方（CLI）明确告警。
//!
//! # 红线
//!
//! 本服务**跑在客户的边界内**（本机/内网/客户自己的容器），数据不经过 `rsi3d-online`。

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use rsi3d_harness_core::{report, Document};
use rsi3d_harness_stream::protocol::{
    parse_client, query_get, Camera, ClientMessage, ServerMessage, StreamKind, STREAM_PROTOCOL,
};
use rsi3d_harness_stream::session::{
    frame_message, now_ms, snapshot_message, welcome_message, Subscription, DEFAULT_FRAME_HEIGHT,
    DEFAULT_FRAME_WIDTH,
};

pub mod client;
pub mod http;
pub mod peer;

/// 服务端启动参数。
pub struct ServeOptions {
    /// 要服务的文档（**由调用方从磁盘读好**：路径与 `--root` 的边界检查留在 CLI，不在这里重复）
    pub doc: Document,
    /// 绑定地址，默认 `127.0.0.1`
    pub bind: String,
    /// 端口；`0` = 让系统挑一个空闲端口（测试用）
    pub port: u16,
    /// 访问令牌；`None` = 随机生成
    pub token: Option<String>,
    /// 图像流的帧率上限（0 = 不限）
    pub fps: u32,
    /// 推送轮的间隔（毫秒）
    pub tick_ms: u64,
    pub frame_width: u32,
    pub frame_height: u32,
    /// 显示名（客户端标题栏用）
    pub name: String,
}

impl ServeOptions {
    pub fn new(doc: Document) -> Self {
        ServeOptions {
            doc,
            bind: "127.0.0.1".to_string(),
            port: 0,
            token: None,
            fps: 4,
            tick_ms: 50,
            frame_width: DEFAULT_FRAME_WIDTH,
            frame_height: DEFAULT_FRAME_HEIGHT,
            name: "scene".to_string(),
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }

    pub fn with_fps(mut self, fps: u32) -> Self {
        self.fps = fps;
        self
    }

    pub fn with_bind(mut self, bind: &str) -> Self {
        self.bind = bind.to_string();
        self
    }
}

/// 运行中的服务。`stop()` 之前一直活着。
pub struct ServeHandle {
    pub addr: SocketAddr,
    pub token: String,
    /// 服务自己的名字（`/healthz` 里会带）：调用方用它区分"这个服务"与"占了同一端口的别人"
    pub name: String,
    shared: Arc<Shared>,
}

impl ServeHandle {
    /// 浏览器可直接打开的地址（带 token）。
    pub fn url(&self) -> String {
        format!("http://{}/?token={}", self.addr, self.token)
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 是否绑到了非回环地址（安全性提醒由调用方决定怎么喊）。
    pub fn is_public(&self) -> bool {
        !self.addr.ip().is_loopback()
    }

    pub fn stats(&self) -> Value {
        self.shared.stats()
    }

    /// 停止接受新连接，并让现有的 SSE 连接结束。
    ///
    /// accept 循环是**非阻塞**的，所以它会在几十毫秒内看到这个标志并退出，
    /// 监听套接字随即释放（端口立即可用，测试与"重启服务"都依赖这一点）。
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
    }

    /// 阻止等待直到 `stop()`。
    pub fn wait(&self) {
        while !self.shared.stopped.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for ServeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Shared {
    doc: Mutex<Document>,
    started: Instant,
    /// 停止标志放在这里：SSE 连接（长连接）靠它退出，否则线程会一直挂着
    stopped: AtomicBool,
    /// 当前连接数（可观测）
    connections: AtomicUsize,
    ticked: AtomicU64,
    messages_sent: AtomicU64,
    frames_sent: AtomicU64,
    patches_dropped: AtomicU64,
    name: String,
    token: String,
    fps: u32,
    tick_ms: u64,
    frame_width: u32,
    frame_height: u32,
}

impl Shared {
    fn stats(&self) -> Value {
        let doc = self.doc.lock().unwrap();
        json!({
            "ok": true,
            "server": "rsi3d-harness",
            "protocol": STREAM_PROTOCOL,
            "name": self.name,
            "uptime_ms": self.started.elapsed().as_millis() as u64,
            "connections": self.connections.load(Ordering::Relaxed),
            // 线上版本号 = 日志前沿（回滚也单调）；操作用户自己去比对
            "revision": doc.revision(),
            "cursor": doc.cursor(),
            "log_length": doc.oplog().len(),
            "scene_hash": doc.scene_hash().unwrap_or_default(),
            "objects": doc.scene().objects.len(),
            "messages_sent": self.messages_sent.load(Ordering::Relaxed),
            "frames_sent": self.frames_sent.load(Ordering::Relaxed),
            "patches_dropped": self.patches_dropped.load(Ordering::Relaxed),
            "ticks": self.ticked.load(Ordering::Relaxed),
        })
    }
}

/// 启动服务。返回后服务已在后台线程运行。
pub fn serve(opts: ServeOptions) -> Result<ServeHandle, String> {
    let token = match opts.token {
        Some(t) if !t.is_empty() => t,
        _ => random_token(),
    };
    let addr = format!("{}:{}", opts.bind, opts.port);
    let listener =
        TcpListener::bind(&addr).map_err(|e| format!("监听 {} 失败：{}", addr, e))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("拿不到监听地址：{}", e))?;
    // 非阻塞 accept：这样"停止"能在几十毫秒内生效，而不是卡在 accept 里
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("设置非阻塞失败：{}", e))?;

    let shared = Arc::new(Shared {
        doc: Mutex::new(opts.doc),
        started: Instant::now(),
        stopped: AtomicBool::new(false),
        connections: AtomicUsize::new(0),
        ticked: AtomicU64::new(0),
        messages_sent: AtomicU64::new(0),
        frames_sent: AtomicU64::new(0),
        patches_dropped: AtomicU64::new(0),
        name: opts.name.clone(),
        token: token.clone(),
        fps: opts.fps,
        tick_ms: opts.tick_ms.max(5),
        frame_width: opts.frame_width,
        frame_height: opts.frame_height,
    });

    let handle = ServeHandle {
        addr,
        token,
        name: opts.name,
        shared: shared.clone(),
    };

    let s = shared.clone();
    thread::spawn(move || {
        while !s.stopped.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((sock, _)) => {
                    // 每个连接一个线程：SSE 是长连接，占住不放；实现简单、隔离清楚
                    let s = s.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_conn(sock, &s) {
                            // 对端提前走掉是很正常的（关标签页），不当错误刷屏
                            if !e.contains("Broken pipe") && !e.contains("Connection reset") {
                                eprintln!("rsi3d-harness serve: 连接结束：{}", e);
                            }
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    eprintln!("rsi3d-harness serve: accept 失败：{}", e);
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
        // listener 在这里被 drop → 端口释放
    });

    Ok(handle)
}

/// 一条连接：读一个请求，回一个响应。
///
/// 只支持"一问一答 + `Connection: close`"：我们这个服务是观测/控制口，
/// 不需要 keep-alive 复用，少一层状态就少一类 bug（除了 SSE 那条长连接）。
fn handle_conn(mut sock: TcpStream, shared: &Arc<Shared>) -> Result<(), String> {
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_secs(30))).ok();
    sock.set_write_timeout(Some(Duration::from_secs(30))).ok();

    let req = match http::read_request(&mut sock)? {
        Some(r) => r,
        None => return Ok(()), // 对端连上就走了
    };
    route(&mut sock, &req, shared)
}

// ---------------------------------------------------------------- 路由

fn route(sock: &mut TcpStream, req: &http::Request, shared: &Arc<Shared>) -> Result<(), String> {
    let path = req.path.clone();
    let query = req.query.clone();

    // 健康检查与静态资源（HTML/JS）不需要 token：它们本身不含数据。
    // 数据口（流/命令/观测/导出）一律要 token——本页可被打开 ≠ 资产可被拿走。
    if path == "/healthz" {
        return reply_json(sock, 200, &shared.stats());
    }
    if path == "/" && req.method == "GET" {
        return reply(sock, 200, "text/html; charset=utf-8", client::CLIENT_HTML.as_bytes());
    }
    if path == "/client.js" && req.method == "GET" {
        return reply(
            sock,
            200,
            "text/javascript; charset=utf-8",
            client::CLIENT_JS.as_bytes(),
        );
    }

    if !authorized(req, &query, &shared.token) {
        return reply_json(
            sock,
            401,
            &json!({
                "ok": false,
                "error": "unauthorized",
                "hint": "需要 ?token=<一次性令牌>（服务启动时打印），或用 Authorization: Bearer / X-Rsi3d-Token 头",
            }),
        );
    }

    match (req.method.as_str(), path.as_str()) {
        ("GET", "/stream/scene") => stream(sock, req, shared, &query, StreamKind::Scene, Camera::default()),
        ("GET", "/stream/frame") => {
            let camera = query_get(&query, "view")
                .and_then(|v| Camera::from_preset(&v))
                .unwrap_or_default();
            stream(sock, req, shared, &query, StreamKind::Frame, camera)
        }
        ("GET", "/snapshot.gltf") => {
            let doc = shared.doc.lock().unwrap();
            let gltf = rsi3d_harness_stream::gltf::scene_to_gltf(doc.scene());
            let body = serde_json::to_string(&gltf).unwrap_or_else(|_| "{}".into());
            reply(sock, 200, "model/gltf+json", body.as_bytes())
        }
        ("GET", "/observe") => {
            // 与 MCP / CLI 同一份观测：浏览器里看到的告警和 Agent 看到的必须一致
            let doc = shared.doc.lock().unwrap();
            let obs = report::observe_json(&doc).unwrap_or_else(|e| {
                json!({"ok": false, "error": e.code(), "message": e.to_string()})
            });
            reply_json(sock, 200, &obs)
        }
        ("POST", "/command") => post_command(sock, req, shared),
        ("POST", "/camera") => post_camera(sock, req),
        ("POST", "/message") => post_message(sock, req, shared),
        _ => reply_json(
            sock,
            404,
            &json!({"ok": false, "error": "not_found", "path": path, "method": req.method}),
        ),
    }
}

fn authorized(req: &http::Request, query: &str, token: &str) -> bool {
    if let Some(t) = query_get(query, "token") {
        if constant_time_eq(t.as_bytes(), token.as_bytes()) {
            return true;
        }
    }
    if let Some(v) = req.header("Authorization") {
        if let Some(bearer) = v.strip_prefix("Bearer ") {
            if constant_time_eq(bearer.as_bytes(), token.as_bytes()) {
                return true;
            }
        }
    }
    if let Some(v) = req.header("X-Rsi3d-Token") {
        if constant_time_eq(v.as_bytes(), token.as_bytes()) {
            return true;
        }
    }
    false
}

/// 定时比较：不因为"提前退出"泄漏"对了几个字符"。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn random_token() -> String {
    // 不引 rand：时间 + 进程内递增 + 地址熵，够做一次性访问令牌
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in nanos
        .to_le_bytes()
        .iter()
        .chain(std::process::id().to_le_bytes().iter())
        .chain(fnv_seed().to_le_bytes().iter())
    {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{:016x}", h)
}

fn fnv_seed() -> u64 {
    use std::sync::atomic::AtomicU64 as A;
    static C: A = A::new(0x9e37_79b9_7f4a_7c15);
    let mut v = C.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    v ^= v >> 29;
    v
}

// ---------------------------------------------------------------- SSE

/// 一条推送流。
///
/// 这是"推流"的真身，也是**背压的物理位置**：这个循环一次只推进一块，
/// 写完 flush 再进下一轮。客户端读得慢 → `send_chunk` 变慢 → 循环变慢 →
/// 该连接的消息自然在 `Outbox` 里被合并/丢弃，**不会把服务端内存吃爆**。
/// 不需要额外的限流器，因为"写"本身就是限流器。
fn stream(
    sock: &mut TcpStream,
    req: &http::Request,
    shared: &Arc<Shared>,
    query: &str,
    default_kind: StreamKind,
    default_camera: Camera,
) -> Result<(), String> {
    // HTTP/1.0 没有分块编码，服务端只能"缓冲整个响应再发"，而这条流永不结束。
    // 与其让它默默挂死，不如明确拒绝。
    if !req.http11() {
        return reply_json(
            sock,
            426,
            &json!({
                "ok": false,
                "error": "http_1_1_required",
                "hint": "SSE 需要 HTTP/1.1（HTTP/1.0 无分块编码，服务端只能缓冲整个响应）",
            }),
        );
    }

    // 客户端可以在 URL 上指定 kind/相机（`?kind=frame&view=top`）
    let kind = query_get(query, "kind")
        .and_then(|k| StreamKind::parse(&k))
        .unwrap_or(default_kind);
    let camera = query_get(query, "view")
        .and_then(|v| Camera::from_preset(&v))
        .unwrap_or(default_camera);

    // 断线续传：EventSource 重连时会自动带 Last-Event-ID；也接受 ?from=
    let from = req
        .header("Last-Event-ID")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .or_else(|| query_get(query, "from").and_then(|v| v.parse::<u32>().ok()));

    let mut sub = Subscription::new(kind, camera, shared.frame_width, shared.frame_height)
        .with_fps(if kind == StreamKind::Frame { shared.fps } else { 0 });

    // 握手先发：客户端据此知道自己接上的是哪一版、几何是什么档次。
    //
    // 续不上时紧接着补一张全量——**只有场景流需要**（帧不累计，重连就给张新的；
    // 而且图像流不该混进 glTF，否则 kind 就失去意义了）。
    let initial = {
        let doc = shared.doc.lock().unwrap();
        let current = doc.revision();
        let resumed = sub.resume(from, current);
        let mut out = welcome_message(&doc, kind, &sub.camera, resumed, current).to_sse();
        if !resumed {
            if kind == StreamKind::Scene {
                out.push_str(&snapshot_message(&doc, current).to_sse());
            }
            sub.note_state(current);
        }
        out
    };

    http::start_chunked(sock, "text/event-stream; charset=utf-8")?;
    shared.connections.fetch_add(1, Ordering::Relaxed);
    let _guard = ConnGuard {
        shared: shared.clone(),
    };

    // 首块立刻发：客户端不用等下一轮 tick 才知道自己接上了
    http::send_chunk(sock, initial.as_bytes())?;

    let mut last_heartbeat = Instant::now();
    loop {
        if shared.stopped.load(Ordering::SeqCst) {
            break;
        }
        let msgs = {
            let doc = shared.doc.lock().unwrap();
            sub.tick(&doc, now_ms())
        };
        shared.ticked.fetch_add(1, Ordering::Relaxed);

        if msgs.is_empty() {
            // 心跳：注释行，客户端忽略；用来保活中间设备并让客户端知道服务端还活着
            if last_heartbeat.elapsed() >= Duration::from_secs(10) {
                last_heartbeat = Instant::now();
                http::send_chunk(sock, b": ping\n\n")?;
            }
            thread::sleep(Duration::from_millis(shared.tick_ms));
            continue;
        }

        shared
            .messages_sent
            .fetch_add(msgs.len() as u64, Ordering::Relaxed);
        for m in &msgs {
            match m {
                ServerMessage::Frame { .. } => {
                    shared.frames_sent.fetch_add(1, Ordering::Relaxed);
                }
                ServerMessage::Patch { .. } => {
                    shared
                        .patches_dropped
                        .store(sub.outbox.dropped_patches, Ordering::Relaxed);
                }
                _ => {}
            }
        }

        // 一次 tick 出来的消息合成一块发：既省 chunk 头，也不破坏事件边界（每个事件自带换行）
        let payload: String = msgs.iter().map(|m| m.to_sse()).collect();
        http::send_chunk(sock, payload.as_bytes())?;
    }

    http::end_chunked(sock);
    Ok(())
}

/// 连接计数：无论怎么退出（正常/出错/提前 return）都记账。
struct ConnGuard {
    shared: Arc<Shared>,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.shared.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- 上行

fn post_command(sock: &mut TcpStream, req: &http::Request, shared: &Shared) -> Result<(), String> {
    let body = req.body_text();
    // 两种都收：
    // 1. **裸信封** `{op,target,params,reason,expect}` —— 与 CLI `scene edit --cmd`、MCP 的入参**逐字一致**；
    // 2. `ClientMessage::Command`（带 `type`）—— 给“一个上行入口”的客户端用。
    let cmd = match serde_json::from_str::<rsi3d_harness_core::CommandRequest>(&body) {
        Ok(c) if !c.op.is_empty() => c,
        _ => match parse_client(&body) {
            Ok(ClientMessage::Command { op, target, params, reason, expect }) => {
                rsi3d_harness_core::CommandRequest { op, target, params, reason, expect }
            }
            Ok(_) => {
                return reply_json(
                    sock,
                    400,
                    &json!({"ok": false, "error": "expected_command",
                        "hint": "POST /command 的 body 形如 {\"op\":\"transform\",\"target\":\"sofa_01\",\"params\":{...},\"reason\":\"...\"}"}),
                )
            }
            Err(e) => {
                return reply_json(
                    sock,
                    400,
                    &json!({"ok": false, "error": "bad_json", "message": e}),
                )
            }
        },
    };

    // `checkout` 也走这条路（撤销 = 跳到 cursor-1，回滚 = 跳到指定 rev）
    let applied = {
        let mut doc = shared.doc.lock().unwrap();
        doc.apply_request(&cmd)
    };
    match applied {
        Ok(a) => {
            // 把 reason 回执回去（它已经进了内核的归因表；这里只是让调用方不用自己存）
            let mut j = report::applied_json(&a);
            j["reason"] = json!(cmd.reason);
            reply_json(sock, 200, &j)
        }
        Err(e) => reply_json(
            sock,
            409,
            &json!({"ok": false, "error": e.code(), "message": e.to_string()}),
        ),
    }
}

fn post_camera(sock: &mut TcpStream, req: &http::Request) -> Result<(), String> {
    let body = req.body_text();
    let camera = match parse_client(&body) {
        Ok(ClientMessage::SetCamera { camera }) => camera,
        Ok(_) => {
            return reply_json(sock, 400, &json!({"ok": false, "error": "expected_set_camera"}))
        }
        Err(e) => {
            return reply_json(sock, 400, &json!({"ok": false, "error": "bad_json", "message": e}))
        }
    };
    // 相机是**每连接**状态（多个观察者可以看不同视角），所以这里只回执；
    // 要换视角就换订阅地址：/stream/frame?view=top|front|iso-sw|iso-se
    reply_json(
        sock,
        200,
        &json!({
            "ok": true,
            "camera": camera,
            "note": "相机是每连接状态；图像流请用 /stream/frame?view=<top|front|iso-sw|iso-se>",
        }),
    )
}

/// 通用上行入口：把 `ClientMessage` 直接喂进来（客户端实现更省事）。
fn post_message(sock: &mut TcpStream, req: &http::Request, shared: &Shared) -> Result<(), String> {
    let body = req.body_text();
    match parse_client(&body) {
        Ok(ClientMessage::Ping { nonce }) => {
            reply_json(sock, 200, &json!({"ok": true, "type": "pong", "nonce": nonce}))
        }
        Ok(ClientMessage::Command { op, target, params, reason, expect }) => {
            let cmd = rsi3d_harness_core::CommandRequest { op, target, params, reason, expect };
            let applied = {
                let mut doc = shared.doc.lock().unwrap();
                doc.apply_request(&cmd)
            };
            match applied {
                Ok(a) => reply_json(sock, 200, &report::applied_json(&a)),
                Err(e) => reply_json(
                    sock,
                    409,
                    &json!({"ok": false, "error": e.code(), "message": e.to_string()}),
                ),
            }
        }
        Ok(ClientMessage::Subscribe { .. }) => reply_json(
            sock,
            400,
            &json!({"ok": false, "error": "subscribe_via_get", "hint": "订阅请用 GET /stream/scene 或 /stream/frame"}),
        ),
        Ok(ClientMessage::SetCamera { camera }) => {
            reply_json(sock, 200, &json!({"ok": true, "type": "camera_ack", "camera": camera}))
        }
        Err(e) => reply_json(sock, 400, &json!({"ok": false, "error": "bad_json", "message": e})),
    }
}

// ---------------------------------------------------------------- 响应工具

fn reply(sock: &mut TcpStream, code: u16, content_type: &str, body: &[u8]) -> Result<(), String> {
    http::respond(
        sock,
        code,
        content_type,
        body,
        &[("Cache-Control", "no-store"), ("Access-Control-Allow-Origin", "*")],
    )
}

fn reply_json(sock: &mut TcpStream, code: u16, v: &Value) -> Result<(), String> {
    let body = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    http::respond(
        sock,
        code,
        "application/json; charset=utf-8",
        body.as_bytes(),
        &[("Access-Control-Allow-Origin", "*")],
    )
}

/// 方便测试与 CLI：一次性把当前场景渲成 PNG 文件。
pub fn render_png(doc: &Document, view: Option<&str>, width: u32, height: u32) -> Vec<u8> {
    let camera = view.and_then(Camera::from_preset).unwrap_or_default();
    match frame_message(doc.scene(), &camera, width, height, doc.revision()) {
        ServerMessage::Frame { png_base64, .. } => {
            rsi3d_harness_stream::session::decode_b64(&png_base64).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_check_is_exact() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"x"));
    }

    #[test]
    fn random_tokens_differ() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
    }
}
