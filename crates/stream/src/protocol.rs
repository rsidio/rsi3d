//! 流协议：**一行一个 JSON**（与 MCP 同一套经验：紧凑、单行、UTF-8）。
//!
//! # 为什么是这个形状
//!
//! 1. **下行两条流，上行只有零星请求**——所以下行用 SSE（服务器单向推），
//!    上行用普通 POST。见 `docs/stream.md` §2 的完整取舍（以及为什么不是 WebSocket）。
//! 2. **事件 id = 文档 rev**。SSE 的 `Last-Event-ID` 因此变成「从这个版本续传」，
//!    断线重连不丢步——这是协议层白拿的可靠性，不用自己发明补发机制。
//! 3. **只推变化**：patch 靠 rev 比较，帧靠 `image_hash` 比较；静止场景零带宽。
//!
//! # 两种订阅
//!
//! | 订阅 | 推什么 | 谁渲染 | 性质 |
//! | --- | --- | --- | --- |
//! | [`StreamKind::Scene`] | glTF 快照 + 增量 patch | 客户端（three.js） | 交互视图 |
//! | [`StreamKind::Frame`] | PNG 帧 | 服务端 | **权威观测**（可复现、能当证据） |

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 协议版本。客户端与服务端必须谈得拢（见 `Welcome`）。
pub const STREAM_PROTOCOL: &str = "rsi3d-stream/v1";

/// 一次订阅想要什么流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamKind {
    /// 场景流：把场景图（glTF）与后续增量推给客户端，**客户端自己渲染**。
    Scene,
    /// 图像流：服务端渲染，把帧推给客户端显示。
    Frame,
}

impl StreamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamKind::Scene => "scene",
            StreamKind::Frame => "frame",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "scene" | "three" | "threejs" | "gltf" => Some(StreamKind::Scene),
            "frame" | "frames" | "image" | "images" | "pixel" => Some(StreamKind::Frame),
            _ => None,
        }
    }
}

/// 客户端可请求的相机。
///
/// 场景流下它只影响客户端画法提示；图像流下它直接决定服务端渲哪张图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Camera {
    pub azimuth_deg: f64,
    pub elevation_deg: f64,
    pub distance: f64,
    /// 视角预设（给图像流的四个标准视角用）；给了就忽略上面三个
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub preset: Option<String>,
}

impl Default for Camera {
    fn default() -> Self {
        // 与 render 的 iso-sw 一致的手感：45° 方位、30° 仰角
        Camera {
            azimuth_deg: -45.0,
            elevation_deg: 30.0,
            distance: 0.0, // 0 = 由服务端按包围盒自动取
            preset: None,
        }
    }
}

impl Camera {
    pub fn from_preset(name: &str) -> Option<Self> {
        let k = rsi3d_harness_render::ViewKind::parse(name)?;
        Some(Camera {
            preset: Some(k.as_str().to_string()),
            ..Default::default()
        })
    }

    /// 落到具体的预置视角（图像流只能渲预置视角——任意轨道相机要等真实的裸机渲染管线）。
    pub fn view_kind(&self) -> rsi3d_harness_render::ViewKind {
        self.preset
            .as_deref()
            .and_then(rsi3d_harness_render::ViewKind::parse)
            .unwrap_or(rsi3d_harness_render::ViewKind::IsoSw)
    }
}

/// 上行请求（POST body）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// 订阅前握手：带上 token 与要什么流。
    Subscribe {
        token: String,
        kind: StreamKind,
        /// 断线续传：希望从这个 rev 之后开始（SSE 重连时会由 `Last-Event-ID` 带来）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_revision: Option<u32>,
        #[serde(default)]
        camera: Option<Camera>,
    },
    /// 换相机（图像流会立刻重渲一帧）。
    SetCamera { camera: Camera },
    /// 发命令：**与 MCP / CLI 完全同一个信封**，包括 reason/expect。
    Command {
        op: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default)]
        params: Value,
        #[serde(default)]
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<Value>,
    },
    /// 心跳/测延迟。
    Ping { #[serde(default)] nonce: u64 },
}

/// 下行事件（SSE event 的 data）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// 握手应答：说清服务端是谁、现在第几版、以及**几何是什么档次**（别让客户端误以为收到真网格）。
    Welcome {
        protocol: String,
        server: String,
        server_version: String,
        kind: StreamKind,
        revision: u32,
        scene_hash: String,
        camera: Camera,
        /// `aabb-proxy`（H0：包围盒代理）| `mesh`（接上真实几何后）
        geometry: String,
        /// 订阅是否从 `from_revision` 续上了；没续上就发了全量
        resumed: bool,
    },
    /// 全量场景（glTF 2.0 JSON）。客户端拿它建场景。
    Snapshot {
        revision: u32,
        scene_hash: String,
        /// glTF 2.0 文档（可直接喂 `GLTFLoader.parse`）
        gltf: Value,
    },
    /// 增量：两版之间的差异（**复用内核的 `Document::diff`**，不另造一套 delta 格式）。
    Patch {
        from: u32,
        to: u32,
        scene_hash: String,
        changes: Value,
    },
    /// 一帧（图像流）。
    Frame {
        revision: u32,
        view: String,
        width: u32,
        height: u32,
        /// 原始像素的 sha256——客户端可以据此判断"这帧我真的没见过"
        image_hash: String,
        /// base64 PNG（`data:image/png;base64,...` 可直接塞进 img.src）
        png_base64: String,
        /// 服务端算出来的「窗前挡光带被遮挡比例」（俯视图才有）
        #[serde(skip_serializing_if = "Option::is_none")]
        band_occlusion: Option<f64>,
    },
    /// 心跳应答。
    Pong { nonce: u64 },
    /// 出错：**能读到的错误**（与 MCP 同一原则）。
    Error { code: String, message: String },
    /// 服务端要收工。
    Bye { reason: String },
}

impl ServerMessage {
    /// SSE 的 `event:` 名。
    pub fn event_name(&self) -> &'static str {
        match self {
            ServerMessage::Welcome { .. } => "welcome",
            ServerMessage::Snapshot { .. } => "snapshot",
            ServerMessage::Patch { .. } => "patch",
            ServerMessage::Frame { .. } => "frame",
            ServerMessage::Pong { .. } => "pong",
            ServerMessage::Error { .. } => "error",
            ServerMessage::Bye { .. } => "bye",
        }
    }

    /// SSE 的 `id:` —— **用文档 rev**，于是客户端重连时的 `Last-Event-ID` 天然表示"我看到了第几版"。
    pub fn event_id(&self) -> Option<u32> {
        match self {
            ServerMessage::Snapshot { revision, .. }
            | ServerMessage::Frame { revision, .. }
            | ServerMessage::Welcome { revision, .. } => Some(*revision),
            ServerMessage::Patch { to, .. } => Some(*to),
            _ => None,
        }
    }

    pub fn to_sse(&self) -> String {
        let data = serde_json::to_string(self).unwrap_or_else(|_| {
            json!({"type": "error", "code": "internal", "message": "序列化失败"}).to_string()
        });
        let mut out = String::new();
        if let Some(id) = self.event_id() {
            out.push_str(&format!("id: {}\n", id));
        }
        out.push_str(&format!("event: {}\n", self.event_name()));
        // 数据里绝不能有裸换行（SSE 用换行分帧）——JSON 序列化本来就转义了换行
        debug_assert!(!data.contains('\n'));
        out.push_str(&format!("data: {}\n\n", data));
        out
    }
}

/// 解析一行客户端消息。
pub fn parse_client(line: &str) -> Result<ClientMessage, String> {
    serde_json::from_str(line.trim())
        .map_err(|e| format!("报文不是合法 JSON（{}）：{}", e, line.trim()))
}

// ---------------------------------------------------------------- CSV/查询参数小工具

/// `?token=x&kind=scene&from=42` → 取值（传输层用；协议层不依赖具体框架）。
pub fn query_get(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        if k == key {
            return Some(url_decode(it.next().unwrap_or("")));
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_framing_is_one_event_per_call_with_id_and_event_name() {
        let msg = ServerMessage::Patch {
            from: 1,
            to: 2,
            scene_hash: "abc".into(),
            changes: json!({"changed": []}),
        };
        let sse = msg.to_sse();
        assert!(sse.starts_with("id: 2\n"), "{}", sse);
        assert!(sse.contains("event: patch\n"), "{}", sse);
        assert!(sse.ends_with("\n\n"), "SSE 事件必须以空行结束：{:?}", sse);
        // 关键：data 行里不能有裸换行（否则会被切成两个事件）
        let data_lines = sse.lines().filter(|l| l.starts_with("data: ")).count();
        assert_eq!(data_lines, 1, "一个事件只能有一行 data：{:?}", sse);
    }

    #[test]
    fn event_id_is_the_document_revision() {
        // 这是断线续传的地基：id 就是 rev
        let f = ServerMessage::Frame {
            revision: 7,
            view: "top".into(),
            width: 1,
            height: 1,
            image_hash: "h".into(),
            png_base64: "x".into(),
            band_occlusion: None,
        };
        assert_eq!(f.event_id(), Some(7));
        assert_eq!(f.event_name(), "frame");
    }

    #[test]
    fn client_messages_roundtrip() {
        let raw = r#"{"type":"subscribe","token":"t","kind":"scene","from_revision":3}"#;
        match parse_client(raw).unwrap() {
            ClientMessage::Subscribe {
                token,
                kind,
                from_revision,
                ..
            } => {
                assert_eq!(token, "t");
                assert_eq!(kind, StreamKind::Scene);
                assert_eq!(from_revision, Some(3));
            }
            other => panic!("解析错了：{:?}", other),
        }
        // 命令信封与 MCP 同形
        let cmd = r#"{"type":"command","op":"transform","target":"sofa_01","params":{"translate":[0,0,1.3]},"reason":"挪出挡光带"}"#;
        match parse_client(cmd).unwrap() {
            ClientMessage::Command { op, reason, .. } => {
                assert_eq!(op, "transform");
                assert_eq!(reason, "挪出挡光带");
            }
            other => panic!("解析错了：{:?}", other),
        }
    }

    #[test]
    fn bad_json_gives_a_readable_error() {
        let e = parse_client("not json").unwrap_err();
        assert!(e.contains("不是合法 JSON"), "{}", e);
    }

    #[test]
    fn stream_kind_accepts_friendly_names() {
        assert_eq!(StreamKind::parse("threejs"), Some(StreamKind::Scene));
        assert_eq!(StreamKind::parse("images"), Some(StreamKind::Frame));
        assert_eq!(StreamKind::parse("nope"), None);
    }

    #[test]
    fn query_parsing_handles_encoding() {
        let q = "token=ab%2Fcd&kind=scene&note=a+b";
        assert_eq!(query_get(q, "token").unwrap(), "ab/cd");
        assert_eq!(query_get(q, "kind").unwrap(), "scene");
        assert_eq!(query_get(q, "note").unwrap(), "a b");
        assert!(query_get(q, "missing").is_none());
    }
}
