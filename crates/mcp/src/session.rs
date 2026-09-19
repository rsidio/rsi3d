//! MCP 会话：把内核的四个原语包成 Agent 能调的工具。
//!
//! # 设计取舍
//!
//! **会话是有状态的**（像 IDE 的「当前打开的文件」），因为 RSI 循环要连续改很多步，
//! 每步都传一遍文件路径既啰嗦又容易传错。代价是"模型可能搞不清在改谁"——
//! 所以每个工具的结果**第一行永远是当前文件 + rev + 游标**，绝不让它猜。
//!
//! **写路径有边界**：所有读写都被限制在 `--root` 之内（默认工作目录）。
//! 引擎没有「执行任意命令」的能力，只有数据操作——这是与「应用内 socket + 无鉴权」
//! 那类 3D 工具桥的关键区别（它们自认无鉴权，且暴露了能力执行）。

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use rsi3d_harness_core as engine;
use engine::{CommandRequest, Document};
use rsi3d_harness_render as render;

use crate::jsonrpc::RpcError;

// ---------------------------------------------------------------- 路径边界

/// 把用户给的路径解析成 root 内的绝对路径；越界即拒绝。
pub fn resolve_in_root(root: &Path, raw: &str) -> Result<PathBuf, RpcError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(RpcError::invalid_params("路径不能为空"));
    }
    let p = Path::new(raw);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    // 先做词法归一（`..` 必须在**读到文件之前**就被消掉，否则/../.. 会绕过检查）
    let normalized = lexical_normalize(&joined);
    let root_norm = lexical_normalize(root);
    if !normalized.starts_with(&root_norm) {
        return Err(RpcError::invalid_params(format!(
            "路径越界：{} 不在工作目录 {} 之内（引擎只能读写它被授权的那棵目录）",
            normalized.display(),
            root_norm.display()
        ))
        .with_data(json!({"root": root_norm.display().to_string()})));
    }
    Ok(normalized)
}

/// 纯词法归一：不碰文件系统，所以对**尚不存在**的路径也有效。
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------- 参数读取

/// 参数读取器：每个错误都告诉模型**该怎么改**。
pub struct Args<'a>(pub &'a Value);

impl<'a> Args<'a> {
    fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    pub fn str(&self, key: &str) -> Result<Option<String>, RpcError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(RpcError::invalid_params(format!("{} 必须是字符串", key))),
        }
    }

    pub fn required_str(&self, key: &str) -> Result<String, RpcError> {
        self.str(key)?
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| RpcError::invalid_params(format!("缺少必填参数 {}", key)))
    }

    pub fn u32(&self, key: &str) -> Result<Option<u32>, RpcError> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .map(Some)
                .ok_or_else(|| {
                    RpcError::invalid_params(format!("{} 必须是非负整数（0 起的版本号）", key))
                }),
        }
    }

    pub fn bool(&self, key: &str) -> Result<Option<bool>, RpcError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(RpcError::invalid_params(format!("{} 必须是布尔值", key))),
        }
    }

    pub fn array(&self, key: &str) -> Result<Option<&'a Vec<Value>>, RpcError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Array(a)) => Ok(Some(a)),
            Some(_) => Err(RpcError::invalid_params(format!("{} 必须是数组", key))),
        }
    }

    pub fn object(&self, key: &str) -> Result<Option<&'a Value>, RpcError> {
        match self.get(key) {
            None => Ok(None),
            Some(v @ Value::Object(_)) => Ok(Some(v)),
            Some(_) => Err(RpcError::invalid_params(format!("{} 必须是对象", key))),
        }
    }
}

// ---------------------------------------------------------------- 工具结果

/// 工具结果里的图片（MCP 的 image content）。
pub struct ImagePart {
    pub mime: String,
    pub data: Vec<u8>,
}

/// 工具结果：给模型读的文本 + 给客户端用的结构化数据（+ 可选图片）。
///
/// 三者都给是有意的——文本是模型的**主通道**（写得像"现场报告"而不是 JSON 墙），
/// 结构化数据让客户端/脚本可以精确取字段而不是去解析文本，
/// 图片则是「看得见」本身（多模态模型可以直接看，人也能）。
pub struct ToolResult {
    pub text: String,
    pub structured: Option<Value>,
    pub images: Vec<ImagePart>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(text: impl Into<String>, structured: Value) -> Self {
        ToolResult {
            text: text.into(),
            structured: Some(structured),
            images: Vec::new(),
            is_error: false,
        }
    }

    /// 业务错误：按 MCP 约定放在结果里（`isError: true`），而不是协议错误。
    ///
    /// 这一点很关键：模型**能读到**这类错误，于是下一步能自己改对；
    /// 而协议错误通常被客户端吞掉，模型只看到"工具调用失败"。
    pub fn err(text: impl Into<String>, structured: Value) -> Self {
        ToolResult {
            text: text.into(),
            structured: Some(structured),
            images: Vec::new(),
            is_error: true,
        }
    }

    /// 带上图片（MCP 的 image content）。这是「Agent 能看见」本身，不是装饰。
    pub fn with_images(mut self, images: Vec<ImagePart>) -> Self {
        self.images = images;
        self
    }

    pub fn to_json(&self) -> Value {
        let mut content = vec![json!({"type": "text", "text": self.text})];
        // 结构化结果同时以 text 形态给一份：旧客户端不认 structuredContent 时也能读到
        if let Some(s) = &self.structured {
            if let Ok(pretty) = serde_json::to_string_pretty(s) {
                content.push(json!({"type": "text", "text": pretty}));
            }
        }
        // 图片排在文本之后：视觉信息占的"注意力"很大，别把文字挤到后面
        for img in &self.images {
            content.push(json!({
                "type": "image",
                "data": rsi3d_harness_render::png::base64(&img.data),
                "mimeType": img.mime,
            }));
        }
        let mut v = json!({ "content": content, "isError": self.is_error });
        if let Some(s) = &self.structured {
            v["structuredContent"] = s.clone();
        }
        v
    }
}

// ---------------------------------------------------------------- 会话

/// 一个 MCP 会话 = 一份文档 + 它从哪来 + 能写哪去。
pub struct Session {
    root: PathBuf,
    /// 打开的源文件（`scene_save` 缺省目标）
    source: Option<PathBuf>,
    doc: Document,
    dirty: bool,
}

impl Session {
    /// 新建会话。`root` 是**唯一**允许读写的目录。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = if root.as_os_str().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            root
        };
        Session {
            root,
            source: None,
            doc: Document::new(engine::Scene::default()).expect("空场景总是合法"),
            dirty: false,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn revision(&self) -> u32 {
        self.doc.revision()
    }

    /// 每份结果的抬头：**永远**先告诉模型在操作哪份文件、哪一版。
    fn header(&self) -> String {
        format!(
            "场景 {}  ·  rev {}  ·  游标 {}  ·  历史前沿 {}{}",
            self.source
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(尚未打开)".into()),
            self.doc.revision(),
            self.doc.cursor(),
            self.doc.head(),
            if self.dirty { "  ·  未保存" } else { "" }
        )
    }

    fn context_json(&self) -> Value {
        json!({
            "source": self.source.as_ref().map(|p| p.display().to_string()),
            "root": self.root.display().to_string(),
            "revision": self.doc.revision(),
            "cursor": self.doc.cursor(),
            "head": self.doc.head(),
            "dirty": self.dirty,
        })
    }

    // ------------------------------------------------------------ 打开

    /// 打开文件（自动识别场景 / 文档）。
    pub fn open_file(&mut self, raw_path: &str) -> Result<ToolResult, RpcError> {
        let path = resolve_in_root(&self.root, raw_path)?;
        let text = std::fs::read_to_string(&path).map_err(|e| {
            RpcError::invalid_params(format!("读不到 {}：{}", path.display(), e))
        })?;
        let peek: Value = serde_json::from_str(&text).map_err(|e| {
            RpcError::invalid_params(format!("{} 不是合法 JSON：{}", path.display(), e))
        })?;
        let is_doc = peek.get("spec").and_then(|s| s.as_str()) == Some(engine::DOCUMENT_SPEC);
        let doc = if is_doc {
            Document::from_json(&text)
        } else {
            Document::from_scene_json(&text)
        }
        .map_err(|e| {
            RpcError::invalid_params(format!("{} 无法作为场景加载：{}", path.display(), e))
        })?;

        self.doc = doc;
        self.source = Some(path);
        self.dirty = false;

        let mut out = self.observe("已打开")?;
        out.text = format!("{}\n\n{}\n", self.header(), out.text);
        Ok(out)
    }

    /// 直接用一段场景 JSON 开工（不需要文件）。
    pub fn open_inline(&mut self, scene: &Value) -> Result<ToolResult, RpcError> {
        let text = serde_json::to_string(scene)
            .map_err(|e| RpcError::invalid_params(format!("scene 无法序列化：{}", e)))?;
        let doc = Document::from_scene_json(&text)
            .map_err(|e| RpcError::invalid_params(format!("scene 无法作为场景加载：{}", e)))?;
        self.doc = doc;
        self.source = None;
        self.dirty = true;

        let mut out = self.observe("已装载内联场景")?;
        out.text = format!("{}\n\n{}\n", self.header(), out.text);
        Ok(out)
    }

    // ------------------------------------------------------------ 观察

    pub fn observe(&self, title: &str) -> Result<ToolResult, RpcError> {
        let summary = engine::report::scene_summary(&self.doc);
        let mut structured = engine::report::observe_json(&self.doc)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        structured["context"] = self.context_json();

        let mut text = format!("{}\n\n{}", title, summary);
        let blockers = self.doc.scene().window_blockers();
        if !blockers.is_empty() {
            text.push_str(&format!(
                "\n\n提示：{} 正落在窗前挡光带里，先把它们挪开再说别的。",
                blockers.join(", ")
            ));
        }
        text.push_str(&format!(
            "\n\n{}",
            self.cursor_hint()
        ));
        Ok(ToolResult::ok(text, structured))
    }

    fn cursor_hint(&self) -> String {
        match (self.doc.can_undo(), self.doc.can_redo()) {
            (true, true) => format!(
                "可回滚：rev {} 是上一版；也可 checkout 到任意历史版本（0–{}）。",
                self.doc.cursor().saturating_sub(1),
                self.doc.head()
            ),
            (true, false) => format!(
                "可回滚到 rev 0–{}（当前 rev {}）。",
                self.doc.cursor().saturating_sub(1),
                self.doc.revision()
            ),
            _ => "已经在最初版本，没有更早的状态可回滚。".to_string(),
        }
    }

    // ------------------------------------------------------------ 编辑

    /// 按顺序执行多条命令；**中途失败就停**，并如实报告已生效的那几条。
    pub fn edit(&mut self, commands: &[Value]) -> Result<ToolResult, RpcError> {
        if commands.is_empty() {
            return Err(RpcError::invalid_params("commands 不能为空"));
        }

        let mut applied: Vec<Value> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        for (i, raw) in commands.iter().enumerate() {
            let req: CommandRequest = serde_json::from_value(raw.clone()).map_err(|e| {
                RpcError::invalid_params(format!(
                    "第 {} 条命令不是合法信封（需要 op / target / params）：{}",
                    i + 1,
                    e
                ))
            })?;
            if req.reason.trim().is_empty() {
                return Err(RpcError::invalid_params(format!(
                    "第 {} 条命令缺少 reason。请写清「为什么这么改」——它会进归因表，\
                     是判断这一步到底有没有用的唯一依据。",
                    i + 1
                )));
            }
            match self.doc.apply_request(&req) {
                Ok(a) => {
                    self.dirty = true;
                    texts.push(engine::report::applied_text(&a, &req.reason));
                    applied.push(engine::report::applied_json(&a));
                }
                Err(e) => {
                    let mut text = format!(
                        "{}\n\n第 {} 条命令被拒绝（{}）：{}",
                        texts.join("\n"),
                        i + 1,
                        e.code(),
                        e
                    );
                    if !applied.is_empty() {
                        text.push_str(&format!(
                            "\n注意：前 {} 条已经生效（当前 rev {}），剩下 {} 条未执行。",
                            applied.len(),
                            self.doc.revision(),
                            commands.len() - i - 1
                        ));
                    }
                    // 失败时也要把**当前状态**给全：模型需要知道现在到底成什么样了，
                    // 才能决定是改参数重试、还是把刚生效的那步回滚掉。
                    text.push_str(&format!("\n\n当前状态：{}", engine::report::status_line(&self.doc)));
                    let warnings = engine::report::warnings_text(&self.doc.warnings());
                    if !warnings.is_empty() {
                        text.push_str(&format!("\n\n当前全部告警：\n{}", warnings));
                    }

                    let mut structured = engine::report::observe_json(&self.doc)
                        .map_err(|e| RpcError::internal(e.to_string()))?;
                    structured["context"] = self.context_json();
                    structured["applied"] = json!(applied);
                    structured["failed_index"] = json!(i + 1);
                    structured["failed_code"] = json!(e.code());
                    structured["failed_message"] = json!(e.to_string());
                    structured["not_applied"] = json!(commands.len() - i - 1);
                    return Ok(ToolResult::err(text, structured));
                }
            }
        }

        let mut structured = engine::report::observe_json(&self.doc)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        structured["context"] = self.context_json();
        structured["steps"] = json!(applied);

        let mut text = format!("{}\n\n{}", self.header(), texts.join("\n"));
        let warnings = engine::report::warnings_text(&self.doc.warnings());
        if !warnings.is_empty() {
            text.push_str(&format!("\n\n当前全部告警：\n{}", warnings));
        }
        let blockers = self.doc.scene().window_blockers();
        if blockers.is_empty() {
            text.push_str("\n\n挡窗者：无（窗前通光）");
        } else {
            text.push_str(&format!("\n\n挡窗者：{}", blockers.join(", ")));
        }
        text.push_str(&format!("\n\n{}", self.cursor_hint()));
        Ok(ToolResult::ok(text, structured))
    }

    // ------------------------------------------------------------ 回滚

    /// `rev` = 跳到某一版（RSI 的 best-so-far）；`undo` = 退回 N 步。
    pub fn rollback(&mut self, rev: Option<u32>, undo: Option<u32>) -> Result<ToolResult, RpcError> {
        match (rev, undo) {
            (Some(_), Some(_)) => Err(RpcError::invalid_params(
                "rev 与 undo 只能给一个：rev 是跳到某一版，undo 是退回 N 步",
            )),
            (None, None) => Err(RpcError::invalid_params(
                "需要 rev（跳到某一版）或 undo（退回 N 步）之一",
            )),
            (Some(r), None) => {
                let a = self
                    .doc
                    .checkout(r)
                    .map_err(|e| self.engine_error(e, "checkout"))?;
                self.dirty = true;
                Ok(self.rollback_result(
                    format!("已回到 rev {}（原来在 rev {}）", r, a.revision - 1),
                    a.revision,
                    "checkout",
                ))
            }
            (None, Some(n)) => {
                if n == 0 {
                    return Err(RpcError::invalid_params("undo 至少要 1"));
                }
                let mut done = 0;
                let mut last_rev = self.doc.revision();
                for _ in 0..n {
                    match self.doc.undo() {
                        Ok(a) => {
                            done += 1;
                            last_rev = a.revision;
                        }
                        Err(engine::CoreError::NothingToUndo) => break,
                        Err(e) => return Err(self.engine_error(e, "undo")),
                    }
                }
                if done == 0 {
                    return Ok(ToolResult::err(
                        "已经在最初版本，没有可撤销的步骤。\
                         如果目标是「回到某一版」，请改用 rev 参数做 checkout。"
                            .to_string(),
                        json!({"context": self.context_json()}),
                    ));
                }
                self.dirty = true;
                Ok(self.rollback_result(
                    format!("已退回 {} 步（新 rev {}）", done, last_rev),
                    last_rev,
                    "undo",
                ))
            }
        }
    }

    fn rollback_result(&self, what: String, rev: u32, kind: &str) -> ToolResult {
        let mut text = format!(
            "{}\n\n{}",
            self.header(),
            what
        );
        let blockers = self.doc.scene().window_blockers();
        text.push_str(&format!(
            "\n挡窗者：{}",
            if blockers.is_empty() {
                "无".to_string()
            } else {
                blockers.join(", ")
            }
        ));
        text.push_str(&format!(
            "\n告警 {} 条\n\n{}",
            self.doc.warnings().len(),
            self.cursor_hint()
        ));
        ToolResult::ok(
            text,
            json!({
                "context": self.context_json(),
                "kind": kind,
                "revision": rev,
                "scene_hash": self.doc.scene_hash().unwrap_or_default(),
            }),
        )
    }

    fn engine_error(&self, e: engine::CoreError, what: &str) -> RpcError {
        RpcError::invalid_params(format!("{} 失败（{}）：{}", what, e.code(), e)).with_data(
            json!({"context": self.context_json(), "code": e.code()}),
        )
    }

    // ------------------------------------------------------------ 历史 / 差异

    pub fn history(&self) -> Result<ToolResult, RpcError> {
        let text = format!("{}\n\n{}", self.header(), engine::report::history_text(&self.doc));
        Ok(ToolResult::ok(
            text,
            json!({
                "context": self.context_json(),
                "oplog": self.doc.oplog(),
                "attribution": self.doc.attribution(),
                "log_hash": self.doc.log_hash().unwrap_or_default(),
            }),
        ))
    }

    pub fn diff(&self, from: Option<u32>, to: Option<u32>) -> Result<ToolResult, RpcError> {
        let to = to.unwrap_or_else(|| self.doc.revision());
        let from = from.unwrap_or(0);
        let d = self.doc.diff(from, to).map_err(|e| self.engine_error(e, "diff"))?;
        let text = format!("{}\n\n{}", self.header(), engine::report::diff_text(&d));
        Ok(ToolResult::ok(
            text,
            json!({"context": self.context_json(), "diff": d}),
        ))
    }

    // ------------------------------------------------------------ 保存 / 自检

    pub fn save(&mut self, raw_path: Option<&str>, overwrite: bool) -> Result<ToolResult, RpcError> {
        let path = match raw_path {
            Some(p) => resolve_in_root(&self.root, p)?,
            None => self.source.clone().ok_or_else(|| {
                RpcError::invalid_params(
                    "这个场景是内联装载的，没有源文件；请给 file 参数指定保存位置",
                )
            })?,
        };

        // 覆盖保护：不要一上来就把别人的场景文件覆盖掉
        let same_as_source = self.source.as_deref() == Some(path.as_path());
        if path.exists() && !same_as_source && !overwrite {
            return Err(RpcError::invalid_params(format!(
                "{} 已存在。要覆盖请加 overwrite: true（或者换一个路径）",
                path.display()
            )));
        }

        let body = self
            .doc
            .to_json()
            .map_err(|e| RpcError::internal(e.to_string()))?;
        std::fs::write(&path, body)
            .map_err(|e| RpcError::invalid_params(format!("写不到 {}：{}", path.display(), e)))?;
        self.source = Some(path.clone());
        self.dirty = false;

        let text = format!(
            "{}\n\n✓ 已保存 {}（文档形态，含 {} 条日志，可重放）\n  场景哈希 {}\n  日志哈希 {}",
            self.header(),
            path.display(),
            self.doc.oplog().len(),
            self.doc.scene_hash().unwrap_or_default(),
            self.doc.log_hash().unwrap_or_default()
        );
        Ok(ToolResult::ok(
            text,
            json!({
                "context": self.context_json(),
                "file": path.display().to_string(),
                "scene_hash": self.doc.scene_hash().unwrap_or_default(),
                "log_hash": self.doc.log_hash().unwrap_or_default(),
            }),
        ))
    }

    /// 渲染：多视角静态观测图（返回**图片内容**，模型能直接看）。
    ///
    /// 默认只出一个俯视图（平面图）：它对「挡没挡住窗」这类判断最有信息量，
    /// 而且单张图的上下文开销最小。要看 3D 就显式要 `iso-sw` / `iso-se`。
    pub fn render(&self, views: Option<&Vec<Value>>, width: u32, height: u32) -> Result<ToolResult, RpcError> {
        let views = match views {
            None => vec![render::ViewKind::Top],
            Some(list) => {
                if list.is_empty() {
                    return Err(RpcError::invalid_params("views 不能是空数组"));
                }
                let mut out = Vec::new();
                for v in list {
                    let name = v.as_str().ok_or_else(|| {
                        RpcError::invalid_params("views 里每一项都得是字符串")
                    })?;
                    out.push(render::ViewKind::parse(name).ok_or_else(|| {
                        RpcError::invalid_params(format!(
                            "不认识的视角「{}」；可用：top, front, iso-sw, iso-se",
                            name
                        ))
                    })?);
                }
                out
            }
        };

        // 尺寸设上下限：太大既费上下文又慢，太小看不出问题
        let width = width.clamp(160, 1600);
        let height = height.clamp(120, 1200);

        let opts = render::RenderOptions {
            width,
            height,
            views,
            ..Default::default()
        };
        let rendered = render::render_document(&self.doc, &opts);

        let mut text = format!("{}\n\n{}", self.header(), rendered.to_text(self.doc.scene()));
        text.push_str(&format!("\n\n{}", self.cursor_hint()));

        let mut structured = rendered.to_json();
        structured["context"] = self.context_json();

        let images = rendered
            .views
            .iter()
            .map(|v| ImagePart {
                mime: "image/png".to_string(),
                data: v.png.clone(),
            })
            .collect();

        Ok(ToolResult::ok(text, structured).with_images(images))
    }

    /// 可复现性自检——这是「观测可以拿去对账」的证据。
    pub fn verify(&self) -> Result<ToolResult, RpcError> {
        let source = self
            .source
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(内存中)".to_string());
        let r = engine::report::verify_report(&self.doc, &source)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let mut structured = r.json.clone();
        structured["context"] = self.context_json();
        if r.ok {
            Ok(ToolResult::ok(r.text, structured))
        } else {
            Ok(ToolResult::err(r.text, structured))
        }
    }

    /// 当前场景的 JSON（给 MCP resource 用）。
    pub fn scene_json(&self) -> Result<String, RpcError> {
        self.doc
            .scene()
            .canonical_json()
            .map_err(|e| RpcError::internal(e.to_string()))
    }

    /// 历史 JSON（给 MCP resource 用）。
    pub fn log_json(&self) -> Result<String, RpcError> {
        let v = json!({
            "revision": self.doc.revision(),
            "cursor": self.doc.cursor(),
            "head": self.doc.head(),
            "log_hash": self.doc.log_hash().unwrap_or_default(),
            "oplog": self.doc.oplog(),
            "attribution": self.doc.attribution(),
        });
        serde_json::to_string_pretty(&v).map_err(|e| RpcError::internal(e.to_string()))
    }
}
