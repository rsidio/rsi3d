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

/// 一条能力的**类别**。
///
/// 这个分类是抄 wgpu 的：它把一个适配器说清楚要三样东西——
/// **features**（*"Features that are not guaranteed to be supported"*）、
/// **limits**（数值上限）、**downlevel flags**（老后端缺了什么，
/// `wgpu_hal` 里每个后端都存一份 `downlevel_flags`）。
/// 我们原来把 `webgl1` 与 `webgl2` 当成两个并列的能力，其实它们是**同一件事的两个档位**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKind {
    /// 能消费哪条流
    Consume,
    /// 客户端渲染档位（越高越能干）
    Render,
    /// **降级档**：还能干活，但按最低公分母来（对应 wgpu 的 downlevel）
    Downlevel,
    /// 形态：有没有屏幕
    Form,
    /// 鲁棒性：坏情况处理得好不好
    Robustness,
}

/// 一条能力的元数据（名字 / 含义 / 类别 / 依赖）。
#[derive(Debug, Clone, Copy)]
pub struct Capability {
    pub name: &'static str,
    pub meaning: &'static str,
    pub kind: CapabilityKind,
    /// 要它还得先有这些里的**任意一个**（空 = 无依赖）
    pub needs_any: &'static [&'static str],
}

/// 客户端能力的**词汇表**（单一出处）。
///
/// 借 glow 的纪律：**能力是声明出来的，不是被假设的**（它把 `supported_extensions()`
/// 放进 `HasContext` trait，于是每个后端都必须回答"你支持什么"）。我们原来只有服务端
/// 声明自己（`Welcome.geometry`），客户端能干什么全靠猜——于是 CDN 被拦、GPU 上下文
/// 丢失这类事在服务端**完全看不见**。
///
/// 名字在线上是**自由字符串**：不认识的原样记录（旧服务端 + 新客户端要能共存），
/// 但绝不假装认识。
///
/// ⚠️ **不预先许诺**：词汇表里只放**真的有客户端在用**的名字。将来加 GPU/WebGPU 档时
/// 再加 `webgpu`，而不是先把名字挂上去（wgpu 那条纪律的另一面：一个名字必须对应真实
/// 行为，否则它就是在骗运维）。
pub const KNOWN_CLIENT_CAPABILITIES: [Capability; 7] = [
    Capability {
        name: "scene",
        meaning: "能消费场景流（glTF 快照 + 增量）",
        kind: CapabilityKind::Consume,
        needs_any: &[],
    },
    Capability {
        name: "image",
        meaning: "能消费图像流（PNG 帧）",
        kind: CapabilityKind::Consume,
        needs_any: &[],
    },
    Capability {
        name: "three",
        meaning: "有可用的 three.js 客户端渲染路径",
        kind: CapabilityKind::Render,
        // three.js 要有个 GL 上下文才画得出来；没有就是自相矛盾的声明
        needs_any: &["webgl2", "webgl1"],
    },
    Capability {
        name: "webgl2",
        meaning: "本机有 WebGL2（客户端渲染的正常档）",
        kind: CapabilityKind::Render,
        needs_any: &[],
    },
    Capability {
        name: "webgl1",
        meaning: "只有 WebGL1——**降级档**：场景流还能看，但要按最低公分母来",
        kind: CapabilityKind::Downlevel,
        needs_any: &[],
    },
    Capability {
        name: "context-loss",
        meaning: "会处理 GPU 上下文丢失/恢复，而不是假装没发生",
        kind: CapabilityKind::Robustness,
        needs_any: &[],
    },
    Capability {
        name: "headless",
        meaning: "没有屏幕（命令行/服务端消费者）",
        kind: CapabilityKind::Form,
        needs_any: &[],
    },
];

/// 帧尺寸的下限（像素）。再小就没有观测价值了。
pub const MIN_FRAME_SIDE: u32 = 64;

/// 本引擎的确定性软件光栅（**唯一能当证据的档**）。
pub const RENDERER_CPU_RASTER: &str = "cpu-raster/v1";

/// 帧来源档的**词汇表**：谁渲的、**能不能当证据**。
///
/// 为什么每一帧都要自报家门：我们的核心不变量是"**同状态必得同像素**"，所以才
/// 敢把帧当作可对账的证据。而"帧"这个形态是可以被**别的东西**灌进来的：
///
/// - GPU 渲染档（跨驱动/跨设备的浮点与光栅化差异，做不到逐像素可复现）；
/// - 从**别人的进程**里钩出来的画面（`veeenu/hudhook` 那种：注入 DLL + hook 人家
///   的 `Present`）——那种帧跟我们的命令日志**没有任何关系**，既不可复现也无法归因。
///
/// 这些帧不是"坏"的，但**不能混进证据**。所以：一个档必须在此登记，且必须说清
/// 它算不算证据；`scene verify` 那类验收只认 [`RENDERER_CPU_RASTER`]。
///
/// ⚠️ 这条表里**不允许**出现两个 `true`：证据档只能有一个（多了就说明有人想把
/// 不可复现的东西也算成证据）。
pub const FRAME_RENDERERS: [(&str, bool, &str); 1] = [(
    RENDERER_CPU_RASTER,
    true,
    "本引擎的 CPU 软件光栅：同状态必得同像素，可当证据",
)];

/// 这个来源档能不能当证据？
pub fn is_evidence_renderer(name: &str) -> bool {
    FRAME_RENDERERS
        .iter()
        .any(|(n, evidence, _)| *n == name && *evidence)
}

/// 声明里的一处**说明**（不是错误：不拒连接，只是必须说出来）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationNote {
    pub kind: NoteKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// 自相矛盾的声明（如 `webgl1` 与 `webgl2` 同时出现）
    Contradiction,
    /// 依赖没满足（如声明 `three` 却没有任何 GL 档位）
    Unmet,
    /// 能连上但干不了正事（如声明 `scene` 却没有渲染档）——**降级是合法状态**，
    /// 只是必须让运维看得见
    Degraded,
}

/// 客户端的自我声明：**谁连上来了、它能做什么**。
///
/// 为什么走**订阅 URL 的查询参数**而不是 `ClientMessage`：一条 SSE 连接就是一次订阅，
/// 而 POST 通道（`/command`）与服务端**没有连接身份**可对——声明塞进 POST 就无法归属
/// 到任何一条连接。查询参数是唯一天然带"连接身份"的位置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientDeclaration {
    /// 如 `rsi3d-web/0.1.0`、`rsi3d-cli/0.1.0`；不给就是 `unknown`
    #[serde(default)]
    pub agent: String,
    /// 能力名（见 [`KNOWN_CLIENT_CAPABILITIES`]）
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 客户端声明的**显示预算** `(宽, 高)`，来自 `?px=WxH`。
    ///
    /// 这是 wgpu 那套里的 **limits**：数值上限，而不是"有没有"。语义是**上限**——
    /// 服务端只会往下调（等比缩放到这个框内），绝不会超过自己的默认档。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_budget: Option<(u32, u32)>,
}

impl ClientDeclaration {
    /// 从查询参数解析（`?agent=…&cap=a,b,c&px=WxH`）。
    pub fn from_query(mut get: impl FnMut(&str) -> Option<String>) -> Self {
        let agent = get("agent").unwrap_or_default().trim().to_string();
        let capabilities = get("cap")
            .map(|raw| {
                raw.split(',')
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        ClientDeclaration {
            agent: if agent.is_empty() {
                "unknown".to_string()
            } else {
                agent
            },
            capabilities,
            frame_budget: get("px").as_deref().and_then(parse_px),
        }
    }

    /// 有没有这条能力。
    pub fn has(&self, name: &str) -> bool {
        self.capabilities.iter().any(|c| c == name)
    }

    /// 我们不认识的能力名（**如实报出来，而不是默默丢掉**）。
    pub fn unknown(&self) -> Vec<&str> {
        self.capabilities
            .iter()
            .filter(|c| !KNOWN_CLIENT_CAPABILITIES.iter().any(|k| k.name == *c))
            .map(|c| c.as_str())
            .collect()
    }

    /// **派生的渲染档位**：一句话说清这个客户端到底能画到什么程度。
    ///
    /// 为什么不让人自己拼字符串：`/healthz` 里罗列十个标志，运维得自己推结论。
    /// wgpu 也是这样——`Adapter::get_info()` + `get_downlevel_capabilities()` 给的是
    /// **可读的结论**，而不是一串原始位。
    pub fn render_tier(&self) -> &'static str {
        let gl = self.has("webgl2") || self.has("webgl1");
        if self.has("three") && gl {
            "three"
        } else if self.has("webgl2") {
            "webgl2"
        } else if self.has("webgl1") {
            "webgl1-downlevel"
        } else if self.has("headless") {
            // 没有屏幕就直说"headless"——比"只能看帧"准确（它连帧都不看，是落盘的）
            "headless"
        } else if self.has("image") {
            "frame-only" // 有屏幕但只能看服务端渲染的帧
        } else {
            "unknown"
        }
    }

    /// 声明里的问题与降级说明（**不拒连接**——旧服务端+新客户端要能共存；
    /// 但 wgpu 那条"越界即报"的精神在这里体现为：必须说出来）。
    pub fn notes(&self) -> Vec<DeclarationNote> {
        let mut out = Vec::new();

        // ① 自相矛盾：两个 GL 档位同时声明（同一件事只能有一个档）
        if self.has("webgl1") && self.has("webgl2") {
            out.push(DeclarationNote {
                kind: NoteKind::Contradiction,
                text: "同时声明了 webgl1 与 webgl2（同一件事只能有一个档）".to_string(),
            });
        }

        // ② 依赖没满足：wgpu 用 `MissingFeatures` 在**建设备时**就报错；我们只报不拒
        for cap in KNOWN_CLIENT_CAPABILITIES.iter() {
            if cap.needs_any.is_empty() || !self.has(cap.name) {
                continue;
            }
            if !cap.needs_any.iter().any(|n| self.has(n)) {
                out.push(DeclarationNote {
                    kind: NoteKind::Unmet,
                    text: format!(
                        "声明了 {} 但没有 {}（依赖没满足）",
                        cap.name,
                        cap.needs_any.join(" / ")
                    ),
                });
            }
        }

        // ③ 降级：能连、但干不了正事。**这是合法状态**（CDN 被拦就是这个样子），
        //    所以才更要让运维看得见，而不是静悄悄
        if self.has("scene") && !self.has("three") && !self.has("headless") && !self.has("image") {
            out.push(DeclarationNote {
                kind: NoteKind::Degraded,
                text: "声明了 scene 但没有任何渲染档（场景流收得到、画不出来）".to_string(),
            });
        }

        out
    }

    /// 实际要渲多大：把服务端默认尺寸**等比缩放到客户端的显示预算内**（只缩不放），
    /// 并保证至少 [`MIN_FRAME_SIDE`] 宽（再小就没有观测价值）。
    pub fn frame_size(&self, default: (u32, u32)) -> (u32, u32) {
        let (dw, dh) = (default.0.max(1), default.1.max(1));
        let Some((bw, bh)) = self.frame_budget else {
            return (dw, dh);
        };
        if bw == 0 || bh == 0 {
            return (dw, dh);
        }
        // 只缩不放 + 不低于下限（下限也按同一个比例缩放，避免把画面拉变形）
        let floor_scale = MIN_FRAME_SIDE as f64 / dw as f64;
        let scale = (bw as f64 / dw as f64)
            .min(bh as f64 / dh as f64)
            .clamp(0.0, 1.0)
            .max(floor_scale);
        (
            ((dw as f64 * scale).round() as u32).max(MIN_FRAME_SIDE),
            ((dh as f64 * scale).round() as u32).max(1),
        )
    }

    /// 供日志/展示的一行摘要。
    pub fn summary(&self) -> String {
        let caps = if self.capabilities.is_empty() {
            "（未声明能力）".to_string()
        } else {
            self.capabilities.join(" · ")
        };
        let unknown = self.unknown();
        let px = match self.frame_budget {
            Some((w, h)) => format!(" {}x{}", w, h),
            None => String::new(),
        };
        if unknown.is_empty() {
            format!("{} [{}]{}", self.agent, caps, px)
        } else {
            format!(
                "{} [{}]{}（不认识：{}）",
                self.agent,
                caps,
                px,
                unknown.join(",")
            )
        }
    }
}

/// 解析 `?px=WxH`（允许 `*`/`x`/`X` 分隔；给不合法的东西就当没声明）。
pub fn parse_px(raw: &str) -> Option<(u32, u32)> {
    let (w, h) = raw
        .split_once(['x', 'X', '*'])
        .map(|(a, b)| (a.trim(), b.trim()))?;
    let w: u32 = w.parse().ok()?;
    let h: u32 = h.parse().ok()?;
    // 0 或过大都是没意义的输入：不受理（当作没声明），而不是静默改成别的数
    if w == 0 || h == 0 || w > 16_384 || h > 16_384 {
        return None;
    }
    Some((w, h))
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
        /// 服务端**记录到的**客户端身份（回声：客户端据此确认服务端真的听到了）
        #[serde(default)]
        client_agent: String,
        /// 服务端记录到的客户端能力（不认识的原样带上，不丢）
        #[serde(default)]
        client_capabilities: Vec<String>,
        /// 服务端**派生出的**渲染档位（见 `ClientDeclaration::render_tier`）。
        /// 回声这个的理由与上面一样：不靠默契，靠对账——客户端能直接看出
        /// "服务端理解的我是几档"。
        #[serde(default)]
        client_render_tier: String,
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
        /// 这一帧是谁渲的（见 [`FRAME_RENDERERS`]）。
        ///
        /// **证据档必须自报家门**：客户端与验收脚本据此判断这帧能不能当证据——
        /// GPU 档、或从别人进程里钩出来的画面，都不得冒充 [`RENDERER_CPU_RASTER`]。
        renderer: String,
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
            renderer: RENDERER_CPU_RASTER.into(),
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

    #[test]
    fn client_declaration_is_parsed_and_defaults_honestly() {
        let q = "token=t&agent=rsi3d-web%2F0.1.0&cap=webgl2,three,thinking-machine";
        let c = ClientDeclaration::from_query(|k| query_get(q, k));
        assert_eq!(c.agent, "rsi3d-web/0.1.0");
        assert_eq!(c.capabilities, vec!["webgl2", "three", "thinking-machine"]);
        // 不认识的**原样留着**（旧服务端+新客户端要能共存），但要能如实报出来
        assert_eq!(c.unknown(), vec!["thinking-machine"]);
        assert!(c.summary().contains("thinking-machine"));

        // 什么都不给：agent 是 unknown（不编造），能力为空（不是"全能"）
        let bare = ClientDeclaration::from_query(|_| None);
        assert_eq!(bare.agent, "unknown");
        assert!(bare.capabilities.is_empty());
        assert!(bare.unknown().is_empty());

        // 空项/多余逗号不该变成"空能力名"
        let messy = ClientDeclaration::from_query(|k| match k {
            "cap" => Some(" , webgl1 ,, ".to_string()),
            _ => None,
        });
        assert_eq!(messy.capabilities, vec!["webgl1"]);
    }

    #[test]
    fn capability_vocabulary_is_documented_and_unique() {
        // 词汇表是契约：每个名字都要有一句话解释，且不能重名
        let mut names: Vec<&str> = KNOWN_CLIENT_CAPABILITIES.iter().map(|c| c.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "能力名不能重复");
        for cap in KNOWN_CLIENT_CAPABILITIES {
            assert!(!cap.name.trim().is_empty());
            assert!(cap.meaning.len() > 4, "{} 缺少解释", cap.name);
            // 依赖必须指向**真实存在**的能力名（否则就是在要求一个不存在的东西）
            for need in cap.needs_any {
                assert!(
                    KNOWN_CLIENT_CAPABILITIES.iter().any(|c| c.name == *need),
                    "{} 依赖了不存在的 {}",
                    cap.name,
                    need
                );
            }
        }
    }

    #[test]
    fn render_tier_is_derived_not_parsed_by_the_operator() {
        let mk = |caps: &[&str]| ClientDeclaration {
            agent: "t".into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            frame_budget: None,
        };
        // 全套 → three 档
        assert_eq!(
            mk(&["webgl2", "three", "scene", "image"]).render_tier(),
            "three"
        );
        // three 没了（CDN 被拦的那一版声明）→ 降回 webgl2
        assert_eq!(mk(&["webgl2", "scene", "image"]).render_tier(), "webgl2");
        assert_eq!(mk(&["webgl1"]).render_tier(), "webgl1-downlevel");
        // 只有图像流 → 只能看服务端渲的帧
        assert_eq!(mk(&["image"]).render_tier(), "frame-only");
        assert_eq!(mk(&["headless", "scene"]).render_tier(), "headless");
        // 命令行客户端：没屏幕，就算它能收帧也还是 headless（它不看，它落盘）
        assert_eq!(mk(&["headless", "image"]).render_tier(), "headless");
        assert_eq!(mk(&[]).render_tier(), "unknown");
    }

    #[test]
    fn notes_separate_contradictions_from_honest_degradation() {
        let mk = |caps: &[&str]| ClientDeclaration {
            agent: "t".into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            frame_budget: None,
        };

        // 自相矛盾：同一件事声明了两个档
        let notes = mk(&["webgl1", "webgl2"]).notes();
        assert_eq!(notes[0].kind, NoteKind::Contradiction);

        // 依赖没满足：three 要有 GL 上下文
        let notes = mk(&["three", "scene"]).notes();
        assert!(notes.iter().any(|n| n.kind == NoteKind::Unmet), "{:?}", notes);

        // 降级是**合法状态**（实测：three.js 没加载出来时就是这个样子），但要记下来
        let degraded = mk(&["webgl2", "scene"]).notes();
        assert!(degraded.iter().any(|n| n.kind == NoteKind::Degraded));
        // 命令行客户端（headless + scene）不算降级——它本来就不画
        assert!(mk(&["headless", "scene"]).notes().is_empty());
        // 全须全尾的客户端：一点问题都没有
        assert!(mk(&["webgl2", "three", "scene", "image", "context-loss"])
            .notes()
            .is_empty());
    }

    #[test]
    fn frame_budget_only_shrinks_and_keeps_the_aspect() {
        let with_px = |px: &str| ClientDeclaration {
            agent: "t".into(),
            capabilities: vec![],
            frame_budget: parse_px(px),
        };
        let default = (480, 360);

        // 没声明 → 用服务端默认档
        assert_eq!(with_px("").frame_size(default), default);
        // 声明得比默认大 → **不放**（客户端报的是上限，不是点菜）
        assert_eq!(with_px("1920x1080").frame_size(default), default);
        // 等比缩到框内
        assert_eq!(with_px("240x180").frame_size(default), (240, 180));
        assert_eq!(with_px("240x9999").frame_size(default), (240, 180));
        assert_eq!(with_px("9999x180").frame_size(default), (240, 180));
        // 小到没意义 → 抬到下限，且**不变形**（仍是 4:3）
        let (w, h) = with_px("10x10").frame_size(default);
        assert_eq!(w, MIN_FRAME_SIDE);
        assert_eq!(h, MIN_FRAME_SIDE * 3 / 4);
        // 垃圾输入当作没声明
        assert_eq!(parse_px("abc"), None);
        assert_eq!(parse_px("0x100"), None);
        assert_eq!(parse_px("99999x99999"), None);
        assert_eq!(parse_px("800*600"), Some((800, 600)));
        assert_eq!(parse_px("1024X768"), Some((1024, 768)));
    }

    #[test]
    fn evidence_grade_renderers_are_unique_and_declared() {
        // 证据档只能有一个：多了就说明有人想把不可复现的东西也算成证据
        let evidence: Vec<&str> = FRAME_RENDERERS
            .iter()
            .filter(|(_, e, _)| *e)
            .map(|(n, _, _)| *n)
            .collect();
        assert_eq!(evidence, vec![RENDERER_CPU_RASTER], "{:?}", evidence);

        // 每个档都得说清自己是什么
        for (name, _, meaning) in FRAME_RENDERERS {
            assert!(name.contains('/'), "{} 应当带版本号（档是会变的）", name);
            assert!(meaning.len() > 8, "{} 没说清楚", name);
        }

        assert!(is_evidence_renderer(RENDERER_CPU_RASTER));
        // 图上钩出来的、GPU 渲的：都不是证据（将来登进来时必须写 false）
        assert!(!is_evidence_renderer("gpu/wgpu-0.1"));
        assert!(!is_evidence_renderer("hooked/dx11-present"));
        assert!(!is_evidence_renderer(""));
    }

    #[test]
    fn the_declaration_is_parsed_from_the_subscribe_url() {
        let q = "token=t&agent=rsi3d-web%2F0.1.0&cap=webgl2%2Cscene&px=240x180";
        let d = ClientDeclaration::from_query(|k| query_get(q, k));
        assert_eq!(d.agent, "rsi3d-web/0.1.0");
        assert!(d.has("webgl2") && d.has("scene"));
        assert_eq!(d.frame_budget, Some((240, 180)));
        assert_eq!(d.render_tier(), "webgl2");
    }
}
