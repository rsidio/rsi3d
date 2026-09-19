//! 端到端：一条**真实的 MCP 报文流**里跑完一轮 RSI。
//!
//! 这里不用任何内部 API——输入是客户端会发的 JSON 行，输出是服务器回的 JSON 行，
//! 而且是**同一个常驻会话**上的多轮交互（真实客户端就是长连接）。
//! 所以这个文件同时是「协议实现正确」与「引擎能被通用工具驱动」的证据。

use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use serde_json::{json, Value};

use rsi3d_harness_mcp::{serve, Config};

/// 与 `scaffolds/agent-app/files/scene.json` 同形的 H0 客厅。
const SCENE: &str = r##"{
  "units": "m",
  "intent": "3 米挑高客厅，北欧风，落地窗，暖光",
  "room": { "size": [4.2, 2.8, 6.0] },
  "window": { "wall": "north", "spanX": [-1.2, 1.2], "height": 1.6, "z": -2.9, "bandDepth": 1.5 },
  "objects": [
    { "id": "obj:sofa_01", "role": "furniture", "material": "fabric",
      "aabb": { "min": [-1.1, 0.0, -2.6], "max": [0.9, 0.85, -1.7] } },
    { "id": "obj:table_01", "role": "furniture", "material": "oak",
      "aabb": { "min": [-0.5, 0.0, 2.2], "max": [0.5, 0.45, 2.9] } },
    { "id": "obj:rug_01", "role": "floor", "material": "fabric",
      "aabb": { "min": [-1.6, 0.0, 0.2], "max": [1.4, 0.02, 3.2] } }
  ],
  "lights": [ { "id": "sun", "kind": "directional", "intensity": 2.4, "color": "#ffe9c9" } ],
  "clearance_rules": [
    { "pair": ["obj:sofa_01", "obj:table_01"], "min": 0.4, "max": 0.8,
      "reason": "茶几应在沙发正前方 0.4–0.8m" }
  ],
  "intent_keywords": [ { "word": "沙发", "present": true }, { "word": "绿植", "present": false } ]
}"##;

// ---------------------------------------------------------------- 管道适配

struct PipeReader {
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

impl std::io::Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            match self.rx.recv() {
                Ok(chunk) => self.pending = chunk,
                Err(_) => return Ok(0), // 写端没了 = EOF
            }
        }
        let n = out.len().min(self.pending.len());
        out[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

struct PipeWriter {
    tx: Sender<Vec<u8>>,
}

impl std::io::Write for PipeWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.tx
            .send(b.to_vec())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "读端已关闭"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------- 测试客户端

struct Client {
    to_server: Sender<Vec<u8>>,
    from_server: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    next_id: i64,
    root: PathBuf,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Client {
    /// 起一个真实的常驻服务线程 + 双向管道。
    fn start(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "rsi3d-mcp-it-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("scene.json"), SCENE).unwrap();

        let (to_server, server_in) = channel::<Vec<u8>>();
        let (server_out, from_server) = channel::<Vec<u8>>();
        let cfg = Config::new(&root);
        let handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(PipeReader {
                rx: server_in,
                pending: Vec::new(),
            });
            let mut writer = PipeWriter { tx: server_out };
            serve(&mut reader, &mut writer, &cfg).expect("serve 不该异常退出");
        });

        Client {
            to_server,
            from_server,
            pending: Vec::new(),
            next_id: 0,
            root,
            handle: Some(handle),
        }
    }

    fn send_raw(&mut self, line: &str) {
        self.to_server
            .send(format!("{}\n", line).into_bytes())
            .unwrap();
    }

    /// 读下一条报文（按换行分帧，与处理真实进程一致）。
    fn read_message(&mut self) -> Value {
        loop {
            if let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=pos).collect();
                let s = String::from_utf8(line).unwrap();
                let s = s.trim();
                if s.is_empty() {
                    continue;
                }
                return serde_json::from_str(s)
                    .unwrap_or_else(|e| panic!("不是合法 JSON 报文：{}（{}）", s, e));
            }
            match self.from_server.recv() {
                Ok(chunk) => self.pending.extend_from_slice(&chunk),
                Err(_) => panic!("服务端关闭了 stdout，但客户端还在等响应"),
            }
        }
    }

    /// 发一条通知（不期待响应）。
    fn notify(&mut self, method: &str, params: Value) {
        self.send_raw(&json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    /// 发一条请求并读回**对应 id** 的响应。
    ///
    /// id 必须对得上——这同时证明了「通知不会被回复」。
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send_raw(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        );
        let resp = self.read_message();
        assert_eq!(
            resp["id"].as_i64(),
            Some(id),
            "响应 id 与请求不符（通知不该被回复）：{}",
            resp
        );
        resp
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": args}))
    }

    fn handshake(&mut self) {
        let resp = self.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "it", "version": "1"}}),
        );
        assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
        self.notify("notifications/initialized", json!({}));
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // 关掉输入端 → 服务线程读到 EOF 正常退出
        let (dead, _) = channel::<Vec<u8>>();
        self.to_server = dead;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------- 小工具

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("响应里没有文本：{}", resp))
        .to_string()
}

fn structured(resp: &Value) -> &Value {
    &resp["result"]["structuredContent"]
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"] == json!(true)
}

/// 从结果里取图片（base64 + mimeType）。
fn images(resp: &Value) -> Vec<(String, String)> {
    resp["result"]["content"]
        .as_array()
        .expect("content 应当是数组")
        .iter()
        .filter(|c| c["type"] == "image")
        .map(|c| {
            (
                c["mimeType"].as_str().unwrap_or_default().to_string(),
                c["data"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------- 完整一轮

#[test]
fn full_rsi_session_over_the_wire() {
    let mut c = Client::start("rsi");
    c.handshake();

    // 1. 打开场景
    let open = c.tool("scene_open", json!({"file": "scene.json"}));
    assert!(!is_error(&open), "{}", open);
    let t = text(&open);
    assert!(t.contains("scene.json"), "抬头必须说明在改哪份文件：{}", t);
    assert!(t.contains("rev 0"), "{}", t);
    assert!(t.contains("挡窗者 1  obj:sofa_01"), "{}", t);
    assert!(t.contains("window.blocked"), "{}", t);
    assert!(t.contains("rule.violated"), "{}", t);
    assert!(
        t.contains("crop-only") || t.contains("full"),
        "要给出可编辑性：{}",
        t
    );
    assert_eq!(
        structured(&open)["window_blockers"],
        json!(["obj:sofa_01"]),
        "结构化数据必须与文本一致"
    );

    // 2. 重复观察必须完全一致（同内容同哈希）
    let h1 = structured(&c.tool("scene_observe", json!({})))["scene_hash"].clone();
    let h2 = structured(&c.tool("scene_observe", json!({})))["scene_hash"].clone();
    assert_eq!(h1, h2, "同一版本重复观察必须一致");

    // 3. 挪开沙发（会话状态连续，所以这里能找到 obj:sofa_01）
    let edit = c.tool(
        "scene_edit",
        json!({"commands": [{
            "op": "transform", "target": "sofa_01",
            "params": {"translate": [0, 0, 1.3]},
            "reason": "沙发挡住窗户 44%，挪出挡光带",
            "expect": {"lighting.window": "+"}
        }]}),
    );
    assert!(!is_error(&edit), "{}", edit);
    let t = text(&edit);
    assert!(t.contains("rev 1  transform  obj:sofa_01"), "{}", t);
    assert!(t.contains("新告警 无"), "{}", t);
    assert!(t.contains("挡窗者：无"), "{}", t);
    let steps = structured(&edit)["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(
        steps[0]["inverse"]["op"], "restore",
        "逆命令应当是 restore（恢复原值）：{:?}",
        steps[0]
    );

    // 4. 挪茶几，故意挪过头 → 应当报出新告警
    let bad = c.tool(
        "scene_edit",
        json!({"commands": [{
            "op": "transform", "target": "table_01",
            "params": {"translate": [0, 0, -2.4]},
            "reason": "把茶几挪到沙发正前方"
        }]}),
    );
    assert!(!is_error(&bad), "{}", bad);
    let t = text(&bad);
    assert!(
        t.contains("[rule.violated]") || t.contains("[layout.intersect]"),
        "挪过头应当产生告警：{}",
        t
    );

    // 5. 历史：两步行都有理由（归因表的原料）
    let hist = c.tool("scene_history", json!({}));
    let attr = structured(&hist)["attribution"].as_array().unwrap();
    assert_eq!(attr.len(), 2);
    assert_eq!(attr[0]["rev"], 1);
    assert!(attr[0]["reason"].as_str().unwrap().contains("挡光带"));
    assert_eq!(attr[0]["expect"]["lighting.window"], "+");
    assert!(structured(&hist)["log_hash"].is_string());

    // 6. 回滚到 rev 1（best-so-far）
    let back = c.tool("scene_rollback", json!({"rev": 1}));
    assert!(!is_error(&back), "{}", back);
    assert!(text(&back).contains("已回到 rev 1"), "{}", text(&back));
    let hash_after_rollback = structured(&back)["scene_hash"].clone();
    let same = c.tool("scene_diff", json!({"from": 1, "to": 1}));
    assert!(text(&same).contains("完全相同"), "{}", text(&same));

    // 7. 保存 + 可复现性自检
    let save = c.tool("scene_save", json!({}));
    assert!(!is_error(&save), "{}", save);
    assert_eq!(
        structured(&save)["scene_hash"], hash_after_rollback,
        "保存的哈希必须等于回滚后的哈希"
    );
    let verify = c.tool("scene_verify", json!({}));
    assert!(!is_error(&verify), "自检应当通过：{}", text(&verify));
    let v = structured(&verify);
    assert_eq!(v["ok"], true);
    assert_eq!(v["replay_matches"], true);
    assert_eq!(v["roundtrip_stable"], true);
    assert!(v["unplayable_revisions"].as_array().unwrap().is_empty());
    assert!(v["log_hash"].is_string());

    // 8. 重新打开保存的文档：日志与游标都还在
    let reopen = c.tool("scene_open", json!({"file": "scene.json"}));
    assert!(!is_error(&reopen), "{}", reopen);
    assert_eq!(structured(&reopen)["revision"], 3, "文档应当带着日志回来");
    assert_eq!(structured(&reopen)["cursor"], 1, "游标应当停在 rev 1");

    // 9. resources：列表 + 读取
    let list = c.request("resources/list", json!({}));
    let uris: Vec<&str> = list["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, vec!["rsi3d://scene", "rsi3d://log"]);
    let res = c.request("resources/read", json!({"uri": "rsi3d://scene"}));
    let body = res["result"]["contents"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(body).unwrap();
    assert!(parsed["objects"].is_array());
    let log = c.request("resources/read", json!({"uri": "rsi3d://log"}));
    let lb: Value =
        serde_json::from_str(log["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(lb["oplog"].as_array().unwrap().len(), 3);
}

// ---------------------------------------------------------------- 错误路径

#[test]
fn rejected_edit_is_readable_and_leaves_state_intact() {
    let mut c = Client::start("reject");
    c.handshake();
    let _ = c.tool("scene_open", json!({"file": "scene.json"}));
    let before = structured(&c.tool("scene_observe", json!({})))["scene_hash"].clone();

    // 未知目标 → `isError: true`（模型读得到），不是协议错误
    let resp = c.tool(
        "scene_edit",
        json!({"commands": [{
            "op": "transform", "target": "obj:ghost",
            "params": {"translate": [1, 0, 0]}, "reason": "试试"
        }]}),
    );
    assert!(is_error(&resp), "{}", resp);
    assert!(text(&resp).contains("unknown_target"), "{}", text(&resp));
    assert_eq!(structured(&resp)["failed_code"], "unknown_target");
    assert_eq!(structured(&resp)["failed_index"], 1);

    // 参数错误也要能定位到「该怎么改」
    let resp = c.tool(
        "scene_edit",
        json!({"commands": [{
            "op": "transform", "target": "sofa_01",
            "params": {"translate": [0, 0]}, "reason": "少一个分量"
        }]}),
    );
    assert!(is_error(&resp), "{}", resp);
    assert!(text(&resp).contains("长度 3"), "{}", text(&resp));

    // 状态一点没变
    let after = structured(&c.tool("scene_observe", json!({})))["scene_hash"].clone();
    assert_eq!(before, after, "被拒绝的命令不该改变状态");

    // 缺 reason 是参数级错误（信封不完整）
    let resp = c.tool(
        "scene_edit",
        json!({"commands": [{"op": "transform", "target": "sofa_01",
                             "params": {"translate": [1, 0, 0]}}]}),
    );
    let msg = resp["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("reason") && msg.contains("归因表"), "{}", resp);
}

#[test]
fn partial_failure_reports_what_landed_and_what_did_not() {
    let mut c = Client::start("partial");
    c.handshake();
    let _ = c.tool("scene_open", json!({"file": "scene.json"}));

    let resp = c.tool(
        "scene_edit",
        json!({"commands": [
            {"op": "set_light", "target": "sun", "params": {"intensity": 1.2}, "reason": "日光过曝"},
            {"op": "transform", "target": "obj:nope", "params": {"translate": [1,0,0]}, "reason": "试试"},
            {"op": "set_light", "target": "sun", "params": {"intensity": 0.8}, "reason": "再压一点"}
        ]}),
    );
    assert!(is_error(&resp), "{}", resp);
    let t = text(&resp);
    assert!(t.contains("前 1 条已经生效"), "{}", t);
    assert!(t.contains("剩下 1 条未执行"), "{}", t);
    let s = structured(&resp);
    assert_eq!(s["applied"].as_array().unwrap().len(), 1);
    assert_eq!(s["failed_index"], 2);
    assert_eq!(s["not_applied"], 1);
    // 第 3 条确实没跑：强度停在 1.2
    assert_eq!(s["view"]["lights"][0]["intensity"], 1.2);
    // 只有成功的那条进了日志
    assert_eq!(s["context"]["revision"], 1);
}

#[test]
fn path_escape_is_refused() {
    let mut c = Client::start("escape");
    c.handshake();
    for bad in ["../outside.json", "/etc/passwd", "sub/../../evil.json", ""] {
        let resp = c.tool("scene_open", json!({"file": bad}));
        let msg = resp["error"]["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("越界") || msg.contains("读不到") || msg.contains("不能为空"),
            "{} 应当被拒绝，实际：{}",
            bad,
            resp
        );
    }
    let _ = c.tool("scene_open", json!({"file": "scene.json"}));
    let resp = c.tool("scene_save", json!({"file": "../out.json"}));
    assert!(
        resp["error"]["message"].as_str().unwrap().contains("越界"),
        "{}",
        resp
    );
}

#[test]
fn inline_scene_needs_an_explicit_save_target() {
    let mut c = Client::start("inline");
    c.handshake();
    let scene: Value = serde_json::from_str(SCENE).unwrap();
    let open = c.tool("scene_open", json!({"scene": scene}));
    assert!(!is_error(&open), "{}", open);
    assert!(text(&open).contains("(尚未打开)"), "{}", text(&open));
    assert_eq!(structured(&open)["context"]["dirty"], true);

    // 没有源文件 → 必须给路径
    let resp = c.tool("scene_save", json!({}));
    assert!(resp["error"]["message"].as_str().unwrap().contains("file"));
    let ok = c.tool("scene_save", json!({"file": "fresh.json"}));
    assert!(!is_error(&ok), "{}", ok);
    assert!(c.root.join("fresh.json").exists());

    // 覆盖保护
    std::fs::write(c.root.join("other.json"), SCENE).unwrap();
    let resp = c.tool("scene_save", json!({"file": "other.json"}));
    assert!(
        resp["error"]["message"].as_str().unwrap().contains("overwrite"),
        "{}",
        resp
    );
    let ok = c.tool(
        "scene_save",
        json!({"file": "other.json", "overwrite": true}),
    );
    assert!(!is_error(&ok), "{}", ok);
}

#[test]
fn rollback_arguments_are_checked_and_are_agent_friendly() {
    let mut c = Client::start("rollback-args");
    c.handshake();
    let _ = c.tool("scene_open", json!({"file": "scene.json"}));

    let resp = c.tool("scene_rollback", json!({}));
    assert!(resp["error"]["message"].as_str().unwrap().contains("rev"));

    let resp = c.tool("scene_rollback", json!({"undo": 1}));
    assert!(is_error(&resp), "{}", resp);
    let t = text(&resp);
    assert!(t.contains("最初版本"), "{}", t);
    assert!(t.contains("checkout"), "要告诉模型改用 rev：{}", t);

    let resp = c.tool("scene_rollback", json!({"rev": 0, "undo": 1}));
    assert!(resp["error"]["message"].as_str().unwrap().contains("只能给一个"));

    let resp = c.tool("scene_rollback", json!({"rev": 99}));
    assert!(
        resp["error"]["message"].as_str().unwrap().contains("没有 revision"),
        "{}",
        resp
    );
}

#[test]
fn unknown_tools_list_the_available_ones() {
    let mut c = Client::start("unknown-tool");
    c.handshake();
    let resp = c.request("tools/call", json!({"name": "scene_fly", "arguments": {}}));
    assert_eq!(resp["error"]["code"], -32602);
    assert_eq!(resp["error"]["data"]["available"].as_array().unwrap().len(), 9);
}

#[test]
fn render_returns_a_real_image() {
    let mut c = Client::start("render");
    c.handshake();
    let _ = c.tool("scene_open", json!({"file": "scene.json"}));

    let resp = c.tool("scene_render", json!({"width": 240, "height": 180}));
    assert!(!is_error(&resp), "{}", resp);

    // 默认只出一张俯视图（省上下文）
    let imgs = images(&resp);
    assert_eq!(imgs.len(), 1, "默认应当只有一张图：{:?}", resp);
    assert_eq!(imgs[0].0, "image/png");
    // PNG 魔数的 base64 前缀——一眼就能确认这不是"随便一段 base64"
    assert!(
        imgs[0].1.starts_with("iVBORw0KGgo"),
        "不是 PNG：{}",
        &imgs[0].1[..24.min(imgs[0].1.len())]
    );

    // 文本里要有人能读的测量结果
    let t = text(&resp);
    assert!(t.contains("窗前挡光带被遮挡"), "{}", t);
    assert!(t.contains("obj:sofa_01"), "{}", t);

    let s = structured(&resp);
    assert_eq!(s["backend"], "software");
    assert!(
        s["band_occlusion"].as_f64().unwrap() > 0.2,
        "沙发在窗带里，遮挡比例应当明显：{}",
        s["band_occlusion"]
    );
    assert_eq!(s["views"].as_array().unwrap().len(), 1);

    // 要几个视角就给几个
    let two = c.tool(
        "scene_render",
        json!({"views": ["top", "iso-sw"], "width": 240, "height": 180}),
    );
    assert_eq!(images(&two).len(), 2);
    let hash0 = structured(&two)["views"][0]["image_hash"].clone();

    // 同场景同参数 ⇒ 同像素哈希（图片能当证据的前提）
    let again = c.tool(
        "scene_render",
        json!({"views": ["top", "iso-sw"], "width": 240, "height": 180}),
    );
    assert_eq!(structured(&again)["views"][0]["image_hash"], hash0);

    // 改场景 ⇒ 图变
    let _ = c.tool(
        "scene_edit",
        json!({"commands": [{
            "op": "transform", "target": "sofa_01",
            "params": {"translate": [0, 0, 1.3]}, "reason": "挪出挡光带"
        }]}),
    );
    let after = c.tool("scene_render", json!({"width": 240, "height": 180}));
    assert_ne!(structured(&after)["views"][0]["image_hash"], hash0);
    let cleared = structured(&after)["band_occlusion"].as_f64().unwrap();
    assert!(cleared < 0.05, "挪开后窗带应当基本通光，实际 {:.0}%", cleared * 100.0);
}

#[test]
fn render_rejects_unknown_views_with_a_readable_error() {
    let mut c = Client::start("render-args");
    c.handshake();
    let _ = c.tool("scene_open", json!({"file": "scene.json"}));
    let resp = c.tool("scene_render", json!({"views": ["sideways"]}));
    let msg = resp["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("sideways") && msg.contains("iso-sw"), "{}", resp);
    let resp = c.tool("scene_render", json!({"views": []}));
    assert!(resp["error"]["message"].as_str().unwrap().contains("空数组"));
}
