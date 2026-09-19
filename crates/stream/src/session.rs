//! 会话层：**传输无关**。SSE/HTTP（`crates/serve`）、将来的 WebSocket/WebRTC 都复用这里的逻辑。
//!
//! 四个东西：
//! 1. [`ServerSession`]：共享的服务端状态（文档 + 一个自带订阅者）。
//! 2. [`Subscription`]：**一个连接**的推流状态机（推什么、什么时候推）；多连接各一份。
//! 3. [`ClientSession`]：客户端把 snapshot + patch 收敛成一份本地场景（可断言与服务端一致）。
//! 4. [`Outbox`]：**背压规则**——这是本模块最值得读的部分。

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use rsi3d_harness_core::{CommandRequest, Document, Scene};
use rsi3d_harness_render::{render, RenderOptions, ViewKind};

use crate::gltf::{diff_ids, scene_to_gltf};
use crate::protocol::{Camera, ServerMessage, StreamKind, STREAM_PROTOCOL};

/// 默认帧尺寸（图像流）。480×360 在"看得清"与"上下文/带宽省"之间比较平衡。
pub const DEFAULT_FRAME_WIDTH: u32 = 480;
pub const DEFAULT_FRAME_HEIGHT: u32 = 360;

// ---------------------------------------------------------------- 服务端会话

/// 共享的服务端状态：文档 + 一个"自带"的订阅者。
///
/// 单连接场景（CLI 客户端、测试、`serve` 的第一个连接）用它就够；
/// 多连接时每个连接各自持一个 [`Subscription`]，共享同一个 `Document`。
pub struct ServerSession {
    doc: Document,
    sub: Subscription,
}

impl ServerSession {
    pub fn new(doc: Document, kind: StreamKind, camera: Camera, width: u32, height: u32) -> Self {
        ServerSession {
            doc,
            sub: Subscription::new(kind, camera, width, height),
        }
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// 自己的那个订阅者（多连接场景请另建 `Subscription` 并共享 `document()`）。
    pub fn subscription(&self) -> &Subscription {
        &self.sub
    }

    pub fn subscription_mut(&mut self) -> &mut Subscription {
        &mut self.sub
    }

    pub fn camera(&self) -> Camera {
        self.sub.camera.clone()
    }

    pub fn kind(&self) -> StreamKind {
        self.sub.kind
    }

    /// 当前状态版本（= 日志前沿）。
    ///
    /// **为什么不用游标（`cursor()`）**：游标是给"撤销/重做"用的（它可以因为 checkout 而**倒退**），
    /// 而线上版本号必须满足两件事：
    /// 1. **单调不减**——否则 SSE 的 `Last-Event-ID` 会乱（它默认 id 一直往前走）；
    /// 2. **`state_at(id)` 就是客户端手上那个状态**（对账的根据）。
    ///
    /// 日志前沿两条都满足：`state_at(revision()) == 当前状态`（内核自检过），
    /// 而且回滚只是在日志里再加一条 checkout（前沿继续往前走）——所以回滚也能用
    /// **增量**表达，而不是笨重地重发全量。
    pub fn state_revision(&self) -> u32 {
        self.doc.revision()
    }

    /// 握手。`from` 来自 `Last-Event-ID`（= 客户端上次看到的状态版本）。
    ///
    /// 续传能成立，是因为**历史版本的状态永远可重放**（内核的不变量）：
    /// 只要客户端说"我看到第 3 版"，我们就能算出 3 → 现在 的增量。
    pub fn welcome(&mut self, from: Option<u32>) -> ServerMessage {
        let current = self.doc.revision();
        let resumed = self.sub.resume(from, current);
        welcome_message(&self.doc, self.sub.kind, &self.sub.camera, resumed, current)
    }

    /// 换相机（图像流会因此重渲；场景流下只作为提示回给客户端）。
    pub fn set_camera(&mut self, camera: Camera) {
        self.sub.set_camera(camera);
    }

    /// 执行一条命令（**与 MCP / CLI 同一个信封**，含 reason/expect）。
    pub fn apply_command(&mut self, req: &CommandRequest) -> Result<u32, rsi3d_harness_core::CoreError> {
        let applied = self.doc.apply_request(req)?;
        Ok(applied.revision)
    }

    pub fn undo(&mut self) -> Result<u32, rsi3d_harness_core::CoreError> {
        Ok(self.doc.undo()?.revision)
    }

    /// 产出现在**该推**的消息。没有变化就返回空——静止场景零带宽。
    pub fn poll(&mut self) -> Vec<ServerMessage> {
        // 不相交字段借用：`&mut self.sub` 与 `&self.doc` 可以共存
        let sub = &mut self.sub;
        let doc = &self.doc;
        sub.tick(doc, now_ms())
    }

    /// 全量快照（glTF）+ 记状态。
    pub fn full_snapshot(&mut self) -> ServerMessage {
        let current = self.state_revision();
        self.sub.note_state(current);
        snapshot_message(&self.doc, current)
    }

    /// 增量：用内核的 diff 决定"哪些对象受影响"，再把它们的**当前完整定义**发出去。
    pub fn patch(&self, from: u32, to: u32) -> ServerMessage {
        patch_message(&self.doc, from, to)
    }

    /// 渲一帧（只支持预置视角；任意轨道相机要等真实渲染管线）。
    pub fn render_frame(&self) -> ServerMessage {
        frame_message(
            self.doc.scene(),
            &self.sub.camera,
            self.sub.width,
            self.sub.height,
            self.state_revision(),
        )
    }
}

// ---------------------------------------------------------------- 订阅者

/// 一个连接该推什么、什么时候推。
///
/// **必须每连接一份**：多个客户端各自“看到第几版”不同（有的刚断线要续传，
/// 有的只要图像流），把这件事共享在一个会话里必然出错。
///
/// 也不做“把状态变动广播给所有人”那种中间层：订阅者只面对 `Document`，
/// 每一轮自己算出该发什么——状态在客户端缺失时能靠 `Last-Event-ID` 补，广播器却不行。
pub struct Subscription {
    pub kind: StreamKind,
    pub camera: Camera,
    pub width: u32,
    pub height: u32,
    /// 这个连接**已知的状态版本**（= 内核游标，不是日志长度——回滚后两者不同）
    known_state: Option<u32>,
    /// 上一次推出去的帧哈希（相同就不推；这是渲染确定性换来的直接收益）
    known_image: Option<String>,
    /// 图像流的最小帧间隔：帧是“照片”，推太密只是浪费带宽与算力
    pub min_frame_interval_ms: u64,
    last_frame_ms: Option<u64>,
    pub outbox: Outbox,
    /// 已推出去的消息数（可观测）
    pub sent: u64,
    pub frames_sent: u64,
}

/// 默认增量预算：超过就压缩成一次全量。
///
/// 16 的取舍：足够覆盖正常交互的突发（连续拖几下），又不至于让积压变成延迟。
pub const DEFAULT_PATCH_BUDGET: usize = 16;

/// 当前时间（毫秒，单调性不重要，只用来算帧间隔）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Subscription {
    pub fn new(kind: StreamKind, camera: Camera, width: u32, height: u32) -> Self {
        Subscription {
            kind,
            camera,
            width,
            height,
            known_state: None,
            known_image: None,
            min_frame_interval_ms: 0,
            last_frame_ms: None,
            outbox: Outbox::new(DEFAULT_PATCH_BUDGET),
            sent: 0,
            frames_sent: 0,
        }
    }

    /// 帧率上限（图像流）。`fps = 0` 表示不限。
    pub fn with_fps(mut self, fps: u32) -> Self {
        self.min_frame_interval_ms = if fps == 0 { 0 } else { 1000 / fps.max(1) as u64 };
        self
    }

    pub fn set_camera(&mut self, camera: Camera) {
        // 相机变了，上一张帧就不再代表“现在”了
        self.known_image = None;
        self.camera = camera;
    }

    pub fn known_state(&self) -> Option<u32> {
        self.known_state
    }

    /// 手动标记“已告知客户端到这个版本”（用于“握手后紧跟全量”这种组合）。
    pub fn note_state(&mut self, rev: u32) {
        self.known_state = Some(rev);
    }

    /// 处理 `Last-Event-ID`。返回是否能续上。
    ///
    /// 能续的依据是**历史状态永远可重放**（内核不变量）：
    /// 客户端说“我看到第 3 版”，我们就算得出 3 → 现在 的增量。
    pub fn resume(&mut self, from: Option<u32>, current: u32) -> bool {
        match from {
            // 只有“确实存在且不超前”的版本才能续
            Some(r) if r <= current => {
                self.known_state = Some(r);
                // 图像流无法“续”：帧不是累计的，重连就该给一张新的
                self.known_image = None;
                true
            }
            _ => {
                self.known_state = None;
                self.known_image = None;
                false
            }
        }
    }

    /// 一轮：把“该产生的”推进 outbox，再把能发的取出来。
    pub fn tick(&mut self, doc: &Document, now: u64) -> Vec<ServerMessage> {
        let current = doc.revision();
        match self.kind {
            StreamKind::Scene => match self.known_state {
                Some(k) if k == current => {}
                Some(k) => {
                    // 包含回滚：`patch_message` 比的是**两个状态的差异**，
                    // 所以"回到旧版本"也是一条正常的增量。
                    let p = patch_message(doc, k, current);
                    self.known_state = Some(current);
                    self.outbox.push(p);
                }
                None => {
                    let s = snapshot_message(doc, current);
                    self.known_state = Some(current);
                    self.outbox.push(s);
                }
            },
            StreamKind::Frame => {
                let changed = self.known_image.is_none() || self.known_state != Some(current);
                let due = match self.last_frame_ms {
                    None => true,
                    Some(t) => now.saturating_sub(t) >= self.min_frame_interval_ms,
                };
                // 状态没变就**连渲都不渲**（渲染也要 CPU）
                if changed && due {
                    let msg = frame_message(doc.scene(), &self.camera, self.width, self.height, current);
                    let hash = match &msg {
                        ServerMessage::Frame { image_hash, .. } => image_hash.clone(),
                        _ => String::new(),
                    };
                    if self.known_image.as_deref() != Some(hash.as_str()) {
                        self.outbox.push(msg);
                        self.frames_sent += 1;
                    }
                    self.known_image = Some(hash);
                    self.known_state = Some(current);
                    self.last_frame_ms = Some(now);
                }
            }
        }

        // 背压：压缩过就得补一张全量（**同一轮内补齐**，见 `Outbox::needs_snapshot`）
        if self.outbox.needs_snapshot() {
            self.outbox.arm_snapshot(snapshot_message(doc, current));
        }

        let out = self.outbox.drain();
        self.sent += out.len() as u64;
        out
    }
}

// ---------------------------------------------------------------- 消息构造

// 下面四个是**无状态**的消息构造：把状态机（Subscription）与“消息长什么样”分开，
// 这样多连接、CLI 客户端、测试都能复用同一份构造逻辑。

/// 握手消息。
pub fn welcome_message(
    doc: &Document,
    kind: StreamKind,
    camera: &Camera,
    resumed: bool,
    current: u32,
) -> ServerMessage {
    ServerMessage::Welcome {
        protocol: STREAM_PROTOCOL.to_string(),
        server: "rsi3d-harness".to_string(),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        kind,
        revision: current,
        scene_hash: doc.scene_hash().unwrap_or_default(),
        camera: camera.clone(),
        // 老实说清几何档次：H0 是包围盒代理，别让客户端以为收到真网格
        geometry: crate::gltf::GEOMETRY_AABB_PROXY.to_string(),
        resumed,
    }
}

/// 全量快照消息。
pub fn snapshot_message(doc: &Document, revision: u32) -> ServerMessage {
    ServerMessage::Snapshot {
        revision,
        scene_hash: doc.scene_hash().unwrap_or_default(),
        gltf: scene_to_gltf(doc.scene()),
    }
}

/// 增量消息。
///
/// 为什么发完整对象而不是字段级 before/after：客户端的应用逻辑变成一句话
/// 「按 id upsert」——不需要为每种字段写一遍 patch 语义，也就不会出现
/// 「客户端少支持了一个字段」这类静默错误。代价是每次多几十到几百字节。
pub fn patch_message(doc: &Document, from: u32, to: u32) -> ServerMessage {
    // 算不出增量（比如客户端报了一个我们没见过的版本）→ **发全量**。
    // 绝不能发一个空补丁：那会让客户端带着错误状态继续跑，是最坏的静默错误。
    let computed = doc
        .diff(from, to)
        .ok()
        .zip(doc.state_at(to).ok());
    let (d, target) = match computed {
        Some(v) => v,
        None => return snapshot_message(doc, doc.revision()),
    };
    let (node_ids, light_ids) = diff_ids(&d);

    let nodes_upsert: Vec<Value> = node_ids
        .iter()
        .filter_map(|id| target.node(id))
        .map(|n| crate::gltf::node_to_gltf(n, None))
        .collect();
    let lights_upsert: Vec<Value> = light_ids
        .iter()
        .filter_map(|id| target.lights.iter().find(|l| &l.id == id))
        .map(crate::gltf::light_to_gltf)
        .collect();

    ServerMessage::Patch {
        from,
        to,
        scene_hash: target.scene_hash().unwrap_or_default(),
        changes: serde_json::to_value(ScenePatch {
            nodes_upsert,
            nodes_remove: d.removed.clone(),
            lights_upsert,
            lights_remove: Vec::new(),
            blockers: target.window_blockers(),
            edit_count: d.added.len() + d.removed.len() + d.changed.len(),
        })
        .unwrap_or(Value::Null),
    }
}

/// 渲染一帧。
pub fn frame_message(
    scene: &Scene,
    camera: &Camera,
    width: u32,
    height: u32,
    revision: u32,
) -> ServerMessage {
    let opts = RenderOptions {
        width,
        height,
        views: vec![camera.view_kind()],
        ..Default::default()
    };
    let r = render(scene, &opts);
    let v = &r.views[0];
    ServerMessage::Frame {
        revision,
        view: v.view.as_str().to_string(),
        width: v.width,
        height: v.height,
        image_hash: v.image_hash.clone(),
        png_base64: rsi3d_harness_render::png::base64(&v.png),
        band_occlusion: r.band_occlusion,
    }
}

/// 增量载荷（`Patch.changes`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenePatch {
    /// 受影响节点的**当前完整定义**（客户端按 name upsert）
    pub nodes_upsert: Vec<Value>,
    /// 要删掉的节点 id
    pub nodes_remove: Vec<String>,
    pub lights_upsert: Vec<Value>,
    pub lights_remove: Vec<String>,
    /// 顺带推「谁挡窗」：客户端可以高亮，省得自己算
    pub blockers: Vec<String>,
    /// 这一步影响了几处（客户端用来显示"刚刚变了什么"）
    pub edit_count: usize,
}

// ---------------------------------------------------------------- 客户端会话

/// 客户端本地重建出来的场景（几何代理层面）。
///
/// 它不是 `Scene`：客户端拿到的是 glTF（给 three.js 用），
/// 所以这里刻意只重建**代理几何 + 元数据**——足够验证"我看到的和服务端一致"。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamedScene {
    pub nodes: BTreeMap<String, ProxyNode>,
    pub lights: BTreeMap<String, Value>,
    /// 场景级信息（房间/窗/规则/意图），只在 snapshot 里给全
    pub extras: Value,
}

/// 一个节点的代理几何（= 服务端 AABB 的中心/尺寸）。
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyNode {
    pub name: String,
    pub translation: [f64; 3],
    pub scale: [f64; 3],
    pub extras: Value,
}

impl ProxyNode {
    /// 还原成 AABB，好跟服务端的场景逐字段比。
    pub fn aabb(&self) -> Option<rsi3d_harness_core::Aabb> {
        let min = [
            self.translation[0] - self.scale[0] / 2.0,
            self.translation[1] - self.scale[1] / 2.0,
            self.translation[2] - self.scale[2] / 2.0,
        ];
        let max = [
            self.translation[0] + self.scale[0] / 2.0,
            self.translation[1] + self.scale[1] / 2.0,
            self.translation[2] + self.scale[2] / 2.0,
        ];
        rsi3d_harness_core::Aabb::new(min, max).ok()
    }
}

/// 客户端状态机：吃服务端消息，收敛出本地场景与最后一帧。
#[derive(Debug, Default)]
pub struct ClientSession {
    scene: StreamedScene,
    /// 本地已知的状态版本（**自己看到的那个版本**，不是服务端声明的）
    pub revision: u32,
    /// 本地是否已经拿到过基准（全量）。没基准时收到的增量一律拒收——
    /// 宁可报错，也不能拿一份自己都不确定对不对的场景去渲染/做决策。
    pub has_state: bool,
    /// 服务端自称的版本（仅用于显示）
    pub server_revision: u32,
    pub welcome: Option<Value>,
    pub frames: usize,
    pub last_frame: Option<(String, Vec<u8>)>,
    pub patches_applied: usize,
    pub snapshots_applied: usize,
    pub errors: Vec<String>,
}

impl ClientSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scene(&self) -> &StreamedScene {
        &self.scene
    }

    /// 处理一条服务端消息。返回是否改变了本地场景。
    pub fn apply(&mut self, msg: &ServerMessage) -> Result<bool, String> {
        match msg {
            ServerMessage::Welcome { revision, .. } => {
                self.welcome = serde_json::to_value(msg).ok();
                // Welcome 里的版本是**服务端**的现状；客户端自己看到哪一版由它自己记着
                // （续传时两者不同，这正是断线期间差的那段）。
                self.server_revision = *revision;
                Ok(false)
            }
            ServerMessage::Snapshot { revision, gltf, .. } => {
                self.scene = from_gltf(gltf)?;
                self.revision = *revision;
                self.has_state = true;
                self.snapshots_applied += 1;
                Ok(true)
            }
            ServerMessage::Patch { from, to, changes, .. } => {
                // 防错位：增量必须接在本地已有状态上，否则宁可报错也不要静默错
                if !self.has_state || *from != self.revision {
                    let e = format!(
                        "增量错位或缺少基准：本地 rev {}（基准 {}），但收到 {} → {}",
                        self.revision,
                        if self.has_state { "有" } else { "无" },
                        from,
                        to
                    );
                    self.errors.push(e.clone());
                    return Err(e);
                }
                let p: ScenePatch = serde_json::from_value(changes.clone())
                    .map_err(|e| format!("增量载荷不合法：{}", e))?;
                for id in &p.nodes_remove {
                    self.scene.nodes.remove(id);
                }
                for n in &p.nodes_upsert {
                    let node = node_from_gltf(n)?;
                    self.scene.nodes.insert(node.name.clone(), node);
                }
                for id in &p.lights_remove {
                    self.scene.lights.remove(id);
                }
                for l in &p.lights_upsert {
                    if let Some(name) = l.get("name").and_then(|v| v.as_str()) {
                        self.scene.lights.insert(name.to_string(), l.clone());
                    }
                }
                // **场景级事实也要更新**：挡窗者会随着家具挪动而变。
                // 漏了这一步，客户端就会拿着旧的观测量去解释新的几何——
                // 画面看着对，结论已经错了（最坏的那种错）。
                if !self.scene.extras.is_null() {
                    self.scene.extras["window_blockers"] = json!(p.blockers);
                }
                self.revision = *to;
                self.patches_applied += 1;
                Ok(true)
            }
            ServerMessage::Frame { image_hash, png_base64, .. } => {
                self.frames += 1;
                if let Some(png) = decode_b64(png_base64) {
                    self.last_frame = Some((image_hash.clone(), png));
                }
                Ok(false)
            }
            ServerMessage::Error { message, .. } => {
                self.errors.push(message.clone());
                Ok(false)
            }
            _ => Ok(false),
        }
    }
}

/// glTF → 本地场景（只取我们发的那些字段）。
pub fn from_gltf(gltf: &Value) -> Result<StreamedScene, String> {
    let mut out = StreamedScene {
        extras: gltf
            .get("extras")
            .and_then(|e| e.get("rsi3d"))
            .cloned()
            .unwrap_or(Value::Null),
        ..Default::default()
    };
    for n in gltf.get("nodes").and_then(|v| v.as_array()).unwrap_or(&vec![]) {
        let node = node_from_gltf(n)?;
        out.nodes.insert(node.name.clone(), node);
    }
    if let Some(ls) = gltf
        .pointer("/extensions/KHR_lights_punctual/lights")
        .and_then(|v| v.as_array())
    {
        for l in ls {
            if let Some(name) = l.get("name").and_then(|v| v.as_str()) {
                out.lights.insert(name.to_string(), l.clone());
            }
        }
    }
    Ok(out)
}

fn node_from_gltf(n: &Value) -> Result<ProxyNode, String> {
    let name = n
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "glTF 节点缺 name".to_string())?
        .to_string();
    let translation = read3(n.get("translation")).ok_or_else(|| format!("{} 缺 translation", name))?;
    let scale = read3(n.get("scale")).ok_or_else(|| format!("{} 缺 scale", name))?;
    Ok(ProxyNode {
        name,
        translation,
        scale,
        extras: n.get("extras").cloned().unwrap_or(Value::Null),
    })
}

fn read3(v: Option<&Value>) -> Option<[f64; 3]> {
    let a = v?.as_array()?;
    if a.len() != 3 {
        return None;
    }
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

/// 自带 base64 解码（不想为客户端引依赖；与 render 的编码对称）。
pub fn decode_b64(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'\n' && *c != b'\r').collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let mut n: u32 = 0;
        let mut pad = 0;
        for (i, c) in chunk.iter().enumerate() {
            if *c == b'=' {
                pad += 1;
                n <<= 6;
            } else {
                n = (n << 6) | val(*c)?;
            }
            let _ = i;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- 背压

/// 一个订阅者的待发队列 + **背压规则**。
///
/// # 规则（本模块最值得读的地方）
///
/// - **帧可以丢**：帧是"当前状态的一张照片"，新的直接覆盖旧的（latest-wins）。
/// - **增量不能丢**：patch 是累计语义，丢一个就永久错位。
/// - **过载不丢数据，而是压缩**：当 patch 积压超过预算，就把它们**合并成一次全量 snapshot**——
///   用几十 KB 换取"状态一定正确"。这比"丢几个 patch"或"无限堆积直到 OOM"都好。
pub struct Outbox {
    patches: VecDeque<ServerMessage>,
    latest_frame: Option<ServerMessage>,
    /// 快照兜底：过载时用它重置客户端
    fallback_snapshot: Option<ServerMessage>,
    /// 正在等全量：这段时间的增量**必须是丢的**（它们将要被全量覆盖，留着反而会错位）
    need_snapshot: bool,
    patch_budget: usize,
    pub dropped_patches: u64,
    pub replaced_frames: u64,
    pub compressions: u64,
}

impl Outbox {
    pub fn new(patch_budget: usize) -> Self {
        Outbox {
            patches: VecDeque::new(),
            latest_frame: None,
            fallback_snapshot: None,
            need_snapshot: false,
            patch_budget: patch_budget.max(1),
            dropped_patches: 0,
            replaced_frames: 0,
            compressions: 0,
        }
    }

    /// 是否在等一张全量。调用方见到 true 就该立刻 `arm_snapshot`
    /// （**同一轮内补齐**：拖到下一轮，中间新产生的增量会因为 `from` 比全量新而错位）。
    pub fn needs_snapshot(&self) -> bool {
        self.need_snapshot && self.fallback_snapshot.is_none()
    }

    pub fn push(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::Patch { .. } => {
                // 压缩期间：增量只可能比将要发的全量旧，丢了才安全
                if self.need_snapshot {
                    self.dropped_patches += 1;
                    return;
                }
                if self.patches.len() >= self.patch_budget {
                    self.compress();
                    self.dropped_patches += 1;
                    return;
                }
                self.patches.push_back(msg);
            }
            ServerMessage::Snapshot { .. } | ServerMessage::Welcome { .. } => {
                // 全量能重置状态，于是积压的增量都没必要了
                self.patches.clear();
                self.patches.push_back(msg);
            }
            ServerMessage::Frame { .. } => {
                if self.latest_frame.is_some() {
                    self.replaced_frames += 1;
                }
                self.latest_frame = Some(msg);
            }
            other => self.patches.push_back(other),
        }
    }

    /// 超预算：**不丢数据，而是压缩**——把积压的增量换成一一次全量。
    fn compress(&mut self) {
        self.dropped_patches += self.patches.len() as u64;
        self.patches.clear();
        self.compressions += 1;
        self.need_snapshot = true;
    }

    /// 压缩时用的全量（由会话层提供：`ServerSession::full_snapshot`）。
    pub fn arm_snapshot(&mut self, snapshot: ServerMessage) {
        debug_assert!(matches!(snapshot, ServerMessage::Snapshot { .. }));
        self.fallback_snapshot = Some(snapshot);
    }

    /// 取出当前可发的消息（顺序：先补齐基准，再给后面的增量与最新帧）。
    pub fn drain(&mut self) -> Vec<ServerMessage> {
        let mut out: Vec<ServerMessage> = Vec::new();
        if self.need_snapshot {
            if let Some(s) = self.fallback_snapshot.take() {
                out.push(s);
                self.need_snapshot = false;
            }
        }
        out.extend(self.patches.drain(..));
        if let Some(f) = self.latest_frame.take() {
            out.push(f);
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.patches.is_empty() && self.latest_frame.is_none() && self.fallback_snapshot.is_none()
    }

    pub fn pending(&self) -> usize {
        self.patches.len() + usize::from(self.latest_frame.is_some())
    }
}

// ---------------------------------------------------------------- 视图

/// 服务端状态的可读摘要（`/healthz`、CLI 与日志都用它）。
pub fn session_status(doc: &Document, kind: StreamKind, camera: Camera) -> Value {
    json!({
        "revision": doc.cursor(),
        "log_revision": doc.revision(),
        "scene_hash": doc.scene_hash().unwrap_or_default(),
        "kind": kind.as_str(),
        "camera": {"preset": camera.view_kind().as_str()},
        "objects": doc.scene().objects.len(),
        "window_blockers": doc.scene().window_blockers(),
        "warnings": doc.warnings().len(),
    })
}

/// 从场景直接建一个服务端会话（CLI/测试的便利入口）。
pub fn session_for_scene(scene: Scene, kind: StreamKind) -> Result<ServerSession, String> {
    let doc = Document::new(scene).map_err(|e| e.to_string())?;
    Ok(ServerSession::new(
        doc,
        kind,
        Camera::default(),
        DEFAULT_FRAME_WIDTH,
        DEFAULT_FRAME_HEIGHT,
    ))
}

/// 用某个预置视角建会话（图像流常见用法）。
pub fn session_for_view(scene: Scene, view: ViewKind) -> Result<ServerSession, String> {
    let doc = Document::new(scene).map_err(|e| e.to_string())?;
    Ok(ServerSession::new(
        doc,
        StreamKind::Frame,
        Camera {
            preset: Some(view.as_str().to_string()),
            ..Default::default()
        },
        DEFAULT_FRAME_WIDTH,
        DEFAULT_FRAME_HEIGHT,
    ))
}
