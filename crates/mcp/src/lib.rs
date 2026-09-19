//! rsi3d-harness 的 **MCP 服务**（Model Context Protocol，stdio 传输）。
//!
//! 目的：让 VS Code / Cursor / Claude Code 这类**通用工具**直接驱动 3D 资产引擎，
//! 而不必等宿主集成（WASM 内嵌是另一条路，两条路共用 `core`，见 `docs/mcp.md` §8）。
//!
//! # 暴露了什么
//!
//! 八个工具覆盖四原语 + RSI 循环真正需要的东西：
//! `scene_open` / `scene_observe` / `scene_edit` / `scene_rollback` /
//! `scene_history` / `scene_diff` / `scene_save` / `scene_verify`。
//!
//! 外加两个 resource（当前场景、命令日志），可以在 Chat 里当上下文挂上去。
//!
//! # 为什么工具是这样设计的
//!
//! - **每个结果第一行都是「哪份文件、哪一版」**：会话有状态（像 IDE 的当前文件），
//!   所以必须让模型随时知道自己在改谁，而不是靠记忆。
//! - **`scene_edit` 强制带 `reason`**：没有理由的改动无法归因，RSI 就退化成"随便改改"。
//! - **失败是"能读到的错误"而不是协议错误**：模型读到 `invalid_argument: translate 需要长度 3 的数组`
//!   才知道下一步怎么改。协议错误会被客户端吞进日志里，模型看不见。
//! - **回滚是一等工具**：RSI 的关键动作是「回到 best-so-far」，不是"再试一次"。

pub mod jsonrpc;
pub mod session;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::jsonrpc::{
    error_response, read_message, result_response, write_message, Id, Request, RpcError,
    INVALID_REQUEST, PARSE_ERROR,
};
use crate::session::{Args, Session};

/// 我们支持的协议版本（按新→旧）。协商时优先回客户端要的那个。
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
pub const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";

pub const SERVER_NAME: &str = "rsi3d-harness";
pub const SERVER_TITLE: &str = "rsi3d-harness（3D 资产引擎）";

/// 启动配置。
#[derive(Debug, Clone)]
pub struct Config {
    /// **唯一**允许读写的目录。
    pub root: PathBuf,
    pub server_version: String,
}

impl Config {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Config {
            root: root.into(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// `initialize` 结果里的 `instructions`——客户端会把它放进模型的上下文。
///
/// 这是**唯一**能一次性教会所有客户端「怎么用这个引擎」的地方，所以值得写清楚。
pub const INSTRUCTIONS: &str = r#"rsi3d-harness 是 3D 资产的「眼睛 + 手」。你负责观察场景、发出编辑命令、看告警增减、必要时回滚。

推荐工作方式：
1. `scene_open` 打开场景或文档，`scene_observe` 看清现状（节点、可编辑性、告警、挡窗者）。
2. **每轮只改一件事**，且必须给出 `reason`（为什么这么改）与 `expect`（期望发生什么）——它们会进归因表。
3. 每次 `scene_edit` 之后重点看 `new_warnings`：被修好的问题不该再出现，新出现的是这一步的代价。
4. 如果某一步让结果变糟，用 `scene_rollback` 回到最好那一版（`rev`）或退回一步（`undo`）。不要硬撑。
5. 收尾用 `scene_save` 落盘：文档形态含完整命令日志，可重放、可对账。
6. 需要证据时用 `scene_verify`：验证重放一致、哈希稳定。

务必注意：
- `editability` 为 `crop-only` / `replace-only` 的节点不能 `transform`（引擎会拒绝并说明原因）。先看清再动手。
- 相同内容必得相同 `scene_hash`。所以同一版本的重复观察结果一定一致，可以当基准来比对改动效果。
- 引擎只能读写它被授权的工作目录（`--root`）之内的文件。"#;

// ---------------------------------------------------------------- 工具清单

/// 工具目录（`tools/list` 的返回）。
pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "scene_open",
            "title": "打开场景",
            "description": "打开一个场景文件或文档（含日志的保存态），设为当前会话。\
                            自动识别：带 spec=rsi3d-document/v1 的按文档读（保留历史与版本），\
                            否则按场景读（从 rev 0 开始）。\
                            也可以直接用 scene 参数传一段内联场景 JSON，不需要文件。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string", "description": "场景/文档路径，相对工作目录"},
                    "scene": {
                        "type": "object",
                        "description": "内联场景 JSON（与 file 二选一）。形如 {units, intent, room:{size}, window:{spanX,height,z,bandDepth}, objects:[{id,role,material,aabb:{min,max}}], lights:[], clearance_rules:[], intent_keywords:[]}"
                    }
                }
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "scene_observe",
            "title": "观察场景",
            "description": "观察当前场景：节点表（含可编辑性）、灯光、间距规则实测值、挡窗者、全部告警、场景哈希。\
                            只读。观察结果是判断改动好坏的唯一依据，所以不要凭记忆猜场景内容。",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "scene_edit",
            "title": "编辑场景",
            "description": "对当前场景执行一条或多条编辑命令（按顺序），返回新版本号、逆命令与新增告警。\
                            一次只改一件事，且每条命令都要写 reason。中途某条被拒绝时，前面的改动仍然生效，\
                            结果里会明确说明哪些已生效、哪些没执行。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "commands": {
                        "type": "array",
                        "minItems": 1,
                        "description": "按顺序执行的命令。强烈建议一次只放一条，这样每一步的因果才清楚。",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": {
                                    "type": "string",
                                    "enum": ["transform", "set_light", "set_material", "remove", "checkout"],
                                    "description": "transform 移动/旋转/缩放；set_light 调灯光；set_material 换材质；remove 删除节点；checkout 跳到某一版"
                                },
                                "target": {
                                    "type": "string",
                                    "description": "节点 id 或灯光 id。裸名会自动补 obj: 前缀（sofa_01 等价于 obj:sofa_01）"
                                },
                                "params": {
                                    "type": "object",
                                    "description": "transform: {translate:[x,y,z]} 或 {rotate_y_deg:30} 或 {scale:1.2}；set_light: {intensity:1.0, color:\"#ffe9c9\"}；set_material: {material:\"walnut\", roughness:0..1, metallic:0..1, opacity:0..1}；remove: {}；checkout: {rev:1}"
                                },
                                "reason": {
                                    "type": "string",
                                    "description": "为什么这么改（必填）。写具体原因，例如「沙发挡住窗户 44%，挪出挡光带」。它会进归因表。"
                                },
                                "expect": {
                                    "type": "object",
                                    "description": "期望发生什么，例如 {\"lighting.window\": \"+\", \"rule.violated\": \"-\"}。用来事后核对你的判断对不对。"
                                }
                            },
                            "required": ["op", "reason"]
                        }
                    }
                },
                "required": ["commands"]
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false}
        }),
        json!({
            "name": "scene_rollback",
            "title": "回滚",
            "description": "回滚到历史状态。给 rev 就跳到那一版（RSI 的 best-so-far 就该用这个）；\
                            给 undo 就退回 N 步。回滚本身也是一条命令，会进日志，所以回滚之后还能再回滚回来。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "rev": {"type": "integer", "minimum": 0, "description": "目标版本号（0 = 初始）。用 scene_history 可以看全部版本"},
                    "undo": {"type": "integer", "minimum": 1, "description": "退回 N 步"}
                }
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false}
        }),
        json!({
            "name": "scene_history",
            "title": "版本历史与归因表",
            "description": "列出命令日志与归因表：每一步是谁改的、改了什么、为什么改、期望什么、当时新增了哪些告警。\
                            用来复盘「哪一步真的起作用」，或找到 best-so-far 的版本号。",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "scene_diff",
            "title": "比较两版差异",
            "description": "比较两个版本之间的差异（新增/删除/变化的对象与灯光属性）。\
                            默认比较 rev 0 与当前版本。用来精确回答「这一步到底改了什么」。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": {"type": "integer", "minimum": 0, "description": "起始版本，默认 0"},
                    "to": {"type": "integer", "minimum": 0, "description": "目标版本，默认当前版本"}
                }
            },
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "scene_render",
            "title": "渲染观测图",
            "description": "把当前场景渲染成静态观测图并**直接返回图片**（PNG），同时给出可见像素与「窗前挡光带被遮挡多少」。\
                            默认只出俯视图（平面图）——它对「挡没挡住窗」这类判断最有信息量。\
                            想看 3D 就加 views:[\"iso-sw\"]；四个标准视角是 top / front / iso-sw / iso-se。\
                            注意：图片是示意图（按包围盒画），不是照片；判断几何关系请以 scene_observe 的数字为准。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "views": {
                        "type": "array",
                        "items": {"type": "string", "enum": ["top", "front", "iso-sw", "iso-se"]},
                        "description": "要哪些视角，默认 [\"top\"]。多视角会多消耗上下文，只在确实需要时加"
                    },
                    "width": {"type": "integer", "minimum": 160, "maximum": 1600, "description": "像素宽，默认 480"},
                    "height": {"type": "integer", "minimum": 120, "maximum": 1200, "description": "像素高，默认 360"}
                }
            },
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "scene_save",
            "title": "保存文档",
            "description": "把当前场景连同完整命令日志保存成文档（可重放、可对账）。\
                            不给 file 就存回打开时的源文件。目标已存在且不是源文件时，需要 overwrite=true。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string", "description": "保存路径，相对工作目录"},
                    "overwrite": {"type": "boolean", "description": "允许覆盖已存在的其它文件", "default": false}
                }
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "scene_verify",
            "title": "可复现性自检",
            "description": "验证当前文档的可复现性：日志重放是否等于当前状态、每个版本是否都能重放、\
                            落盘往返后哈希是否不变、快照与重放是否一致、游标是否自洽。\
                            需要「结果可以拿去对账」的证据时用这个。",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        }),
    ]
}

/// 工具调用分发。
pub fn call_tool(session: &mut Session, name: &str, args: &Value) -> Result<session::ToolResult, RpcError> {
    let a = Args(args);
    match name {
        "scene_open" => {
            let file = a.str("file")?;
            let inline = a.object("scene")?;
            match (file, inline) {
                (Some(_), Some(_)) => Err(RpcError::invalid_params(
                    "file 与 scene 只能给一个",
                )),
                (None, None) => Err(RpcError::invalid_params(
                    "需要 file（打开文件）或 scene（内联场景）之一",
                )),
                (Some(f), None) => session.open_file(&f),
                (None, Some(s)) => session.open_inline(s),
            }
        }
        "scene_observe" => session.observe("场景观察"),
        "scene_edit" => {
            let cmds = a
                .array("commands")?
                .ok_or_else(|| RpcError::invalid_params("缺少必填参数 commands（命令数组）"))?;
            session.edit(cmds)
        }
        "scene_rollback" => session.rollback(a.u32("rev")?, a.u32("undo")?),
        "scene_history" => session.history(),
        "scene_diff" => session.diff(a.u32("from")?, a.u32("to")?),
        "scene_render" => session.render(
            a.array("views")?,
            a.u32("width")?.unwrap_or(480),
            a.u32("height")?.unwrap_or(360),
        ),
        "scene_save" => session.save(a.str("file")?.as_deref(), a.bool("overwrite")?.unwrap_or(false)),
        "scene_verify" => session.verify(),
        other => Err(RpcError::new(
            crate::jsonrpc::INVALID_PARAMS,
            format!("未知工具：{}", other),
        )
        .with_data(json!({"available": tools().iter().filter_map(|t| t["name"].as_str().map(String::from)).collect::<Vec<_>>()}))),
    }
}

// ---------------------------------------------------------------- resources

pub fn resources() -> Vec<Value> {
    vec![
        json!({
            "uri": "rsi3d://scene",
            "name": "当前场景",
            "title": "当前场景（规范化 JSON）",
            "description": "当前打开场景的规范化 JSON。可以挂到对话里当上下文。",
            "mimeType": "application/json",
        }),
        json!({
            "uri": "rsi3d://log",
            "name": "命令日志与归因表",
            "title": "命令日志与归因表",
            "description": "全部版本、每步的理由与预期，以及日志哈希（可对账）。",
            "mimeType": "application/json",
        }),
    ]
}

pub fn read_resource(session: &Session, uri: &str) -> Result<Value, RpcError> {
    let text = match uri {
        "rsi3d://scene" => session.scene_json()?,
        "rsi3d://log" => session.log_json()?,
        other => {
            return Err(RpcError::invalid_params(format!(
                "未知资源：{}（可用 rsi3d://scene、rsi3d://log）",
                other
            )))
        }
    };
    Ok(json!({
        "contents": [{"uri": uri, "mimeType": "application/json", "text": text}]
    }))
}

// ---------------------------------------------------------------- 协议处理

/// 处理一条报文，返回要回写的响应（`None` = 通知，不回复）。
pub fn handle_message(session: &mut Session, msg: &Value) -> Option<Value> {
    let req: Request = match serde_json::from_value(msg.clone()) {
        Ok(r) => r,
        Err(e) => {
            // 解析失败：id 可能压根读不出来，按规范回 null
            let id = msg.get("id").and_then(id_from_value);
            return Some(error_response(
                id.as_ref(),
                &RpcError::new(PARSE_ERROR, format!("报文不是合法 JSON-RPC：{}", e)),
            ));
        }
    };

    if req.jsonrpc != "2.0" {
        if req.is_notification() {
            return None;
        }
        return Some(error_response(
            req.id.as_ref(),
            &RpcError::new(INVALID_REQUEST, "jsonrpc 必须是 \"2.0\""),
        ));
    }

    let outcome = dispatch(session, &req);
    if req.is_notification() {
        // 通知一律不回复（即使出错）——规范如此，客户端也不等
        return None;
    }
    Some(match outcome {
        Ok(result) => result_response(req.id.as_ref(), result),
        Err(e) => error_response(req.id.as_ref(), &e),
    })
}

fn id_from_value(v: &Value) -> Option<Id> {
    match v {
        Value::Number(n) => n.as_i64().map(Id::Num),
        Value::String(s) => Some(Id::Str(s.clone())),
        _ => None,
    }
}

fn dispatch(session: &mut Session, req: &Request) -> Result<Value, RpcError> {
    match req.method.as_str() {
        "initialize" => {
            let params = req.params_obj()?;
            let requested = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // 规范：客户端要的版本我们支持就原样回；否则回我们最新的，由客户端决定是否继续
            let agreed = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
                requested.to_string()
            } else {
                if !requested.is_empty() {
                    eprintln!(
                        "[rsi3d-harness] 客户端请求协议版本 {}，我们支持 {:?}，将回 {}",
                        requested, SUPPORTED_PROTOCOL_VERSIONS, LATEST_PROTOCOL_VERSION
                    );
                }
                LATEST_PROTOCOL_VERSION.to_string()
            };
            let client = params.get("clientInfo").cloned().unwrap_or(Value::Null);
            eprintln!(
                "[rsi3d-harness] initialize：client={} protocol={} → {}，root={}",
                client.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                if requested.is_empty() { "?" } else { requested },
                agreed,
                session.root().display()
            );
            Ok(json!({
                "protocolVersion": agreed,
                "capabilities": {
                    "tools": {"listChanged": false},
                    "resources": {"subscribe": false, "listChanged": false},
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "title": SERVER_TITLE,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": INSTRUCTIONS,
            }))
        }
        "notifications/initialized" => Ok(json!({})),
        "notifications/cancelled" => Ok(json!({})),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let params = req.params_obj()?;
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::invalid_params("tools/call 需要 name"))?;
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = call_tool(session, name, &args)?;
            Ok(result.to_json())
        }
        "resources/list" => Ok(json!({"resources": resources()})),
        "resources/read" => {
            let params = req.params_obj()?;
            let uri = params
                .get("uri")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::invalid_params("resources/read 需要 uri"))?;
            read_resource(session, uri)
        }
        // 我们没声明 prompts 能力；但客户端若问了，回空列表比报错友好
        "prompts/list" => Ok(json!({"prompts": []})),
        other => Err(RpcError::method_not_found(other)),
    }
}

// ---------------------------------------------------------------- stdio 循环

/// 跑 stdio 服务：读一行报文、处理、写一行响应，直到 stdin 关闭。
///
/// 注意：**只有 MCP 报文进 stdout**，所有日志走 stderr
/// （客户端可能把 stdout 的每个字节都当协议解析）。
pub fn serve<R: BufRead, W: Write>(reader: &mut R, writer: &mut W, cfg: &Config) -> std::io::Result<()> {
    let mut session = Session::new(cfg.root.clone());
    eprintln!(
        "[rsi3d-harness] MCP stdio 已就绪 · 版本 {} · 工作目录 {}（引擎只能读写这里）",
        cfg.server_version,
        session.root().display()
    );

    while let Some(line) = read_message(reader)? {
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let resp = error_response(
                    None,
                    &RpcError::new(PARSE_ERROR, format!("无法解析这一行：{}", e)),
                );
                write_message(writer, &resp)?;
                continue;
            }
        };
        if let Some(resp) = handle_message(&mut session, &msg) {
            write_message(writer, &resp)?;
        }
    }
    eprintln!("[rsi3d-harness] stdin 已关闭，退出");
    Ok(())
}

/// 便捷入口：用进程的 stdin/stdout 跑服务。
pub fn serve_stdio(cfg: &Config) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut writer = std::io::BufWriter::new(stdout.lock());
    serve(&mut reader, &mut writer, cfg)
}

// ---------------------------------------------------------------- 内部测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::METHOD_NOT_FOUND;
    use std::io::BufReader;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rsi3d-mcp-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn run_script(root: &std::path::Path, lines: &[&str]) -> Vec<Value> {
        let input = lines.join("\n") + "\n";
        let mut reader = BufReader::new(input.as_bytes());
        let mut out: Vec<u8> = Vec::new();
        serve(&mut reader, &mut out, &Config::new(root)).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("每行都必须是合法 JSON"))
            .collect()
    }

    #[test]
    fn handshake_tools_list_and_ping() {
        let root = tmpdir("handshake");
        let resp = run_script(
            &root,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
            ],
        );
        // 通知不回复 → 只有 3 条响应
        assert_eq!(resp.len(), 3, "{:?}", resp);
        assert_eq!(resp[0]["result"]["protocolVersion"], "2025-06-18");
        assert!(resp[0]["result"]["instructions"].as_str().unwrap().contains("scene_edit"));
        let names: Vec<&str> = resp[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 9, "工具数要与目录一致：{:?}", names);
        for want in [
            "scene_open",
            "scene_observe",
            "scene_edit",
            "scene_rollback",
            "scene_history",
            "scene_diff",
            "scene_render",
            "scene_save",
            "scene_verify",
        ] {
            assert!(names.contains(&want), "缺工具 {}", want);
        }
        assert_eq!(resp[2]["result"], json!({}));
    }

    #[test]
    fn version_negotiation_falls_back_for_unknown_versions() {
        let root = tmpdir("version");
        let resp = run_script(
            &root,
            &[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#],
        );
        assert_eq!(resp[0]["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);
    }

    #[test]
    fn unknown_method_and_unknown_tool_are_protocol_errors() {
        let root = tmpdir("errors");
        let resp = run_script(
            &root,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"no/such/method"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
                r#"not json at all"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"scene_observe","arguments":{}}}"#,
            ],
        );
        assert_eq!(resp.len(), 4);
        assert_eq!(resp[0]["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(resp[1]["error"]["code"], crate::jsonrpc::INVALID_PARAMS);
        assert_eq!(resp[2]["error"]["code"], PARSE_ERROR);
        // 失败之后会话仍然可用
        assert!(resp[3]["result"]["structuredContent"]["context"]["root"].is_string());
    }

    #[test]
    fn edit_requires_reason_and_reports_readable_error() {
        let root = tmpdir("reason");
        let resp = run_script(
            &root,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scene_edit","arguments":{"commands":[{"op":"transform","target":"obj:x","params":{"translate":[1,0,0]}}]}}}"#,
            ],
        );
        let err = resp[1]["error"]["message"].as_str().unwrap();
        assert!(err.contains("reason"), "{}", err);
        assert!(err.contains("归因表"), "{}", err);
    }
}
