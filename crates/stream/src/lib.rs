//! rsi3d-harness 的**流协议**：把 3D 资产转成可被远程消费的流。
//!
//! # 解决什么问题
//!
//! 3D 资产在数据面（服务端/客户边界内），用户想在别处「看见/使用」它。
//! 两种转流方式的**代价不同**，所以本模块提供两种订阅而不是二选一：
//!
//! | 订阅 | 推什么 | 谁渲染 | 代价 | 性质 |
//! | --- | --- | --- | --- | --- |
//! | [`StreamKind::Scene`] | glTF 快照 + 增量 | **客户端**（three.js） | 资产到了客户端 | 交互视图 |
//! | [`StreamKind::Frame`] | PNG 帧 | **服务端** | 服务端算力 + 带宽 | **权威观测**（可复现、能当证据） |
//!
//! 这个区分不是画蛇添足：`core` 的不变量保证"同状态同像素"，
//! 所以**只有服务端渲染出来的帧能当证据**；场景流是给人（和模型）看的交互视图。
//! 需要"可对账"的时候必须以图像流为准。
//!
//! # 分层
//!
//! ```text
//! crates/serve   ── 传输：HTTP + SSE（可换成 WebSocket/WebRTC，会话层不动）
//! crates/stream  ── 协议 + 会话 + glTF 编码（**本 crate，传输无关、可单测**）
//! crates/render  ── 帧从哪来
//! crates/core    ── 状态、命令、日志（唯一的真相来源）
//! ```
//!
//! # 三条值得记住的设计
//!
//! 1. **事件 id = 状态版本（内核游标）** → SSE 的 `Last-Event-ID` 天然就是"断线续传"，
//!    不用自己发明补发机制（见 [`protocol::ServerMessage::event_id`]）。
//! 2. **只推变化** → patch 靠 rev 比，帧靠 `image_hash` 比；静止场景零带宽。
//! 3. **背压不对称** → 帧可丢（latest-wins），**增量不能丢**；过载时压缩成一次全量
//!    （见 [`session::Outbox`]，这是本 crate 最值得读的一段）。
//!
//! 详细设计与取舍见 `docs/stream.md`。

pub mod gltf;
pub mod protocol;
pub mod render_mode;
pub mod session;

pub use gltf::{diff_ids, scene_to_gltf, GEOMETRY_AABB_PROXY};
pub use protocol::{
    is_evidence_renderer, parse_client, parse_px, query_get, Camera, Capability, CapabilityKind,
    ClientDeclaration, ClientMessage, DeclarationNote, NoteKind, ServerMessage, StreamKind,
    FRAME_RENDERERS, KNOWN_CLIENT_CAPABILITIES, MIN_FRAME_SIDE, RENDERER_CPU_RASTER,
    STREAM_PROTOCOL,
};
pub use session::{
    decode_b64, frame_message, now_ms, patch_message, session_for_scene, session_for_view,
    session_status, snapshot_message, welcome_message, ClientSession, Outbox, ProxyNode,
    ScenePatch, ServerSession, StreamedScene, Subscription, DEFAULT_FRAME_HEIGHT,
    DEFAULT_FRAME_WIDTH, DEFAULT_PATCH_BUDGET,
};
