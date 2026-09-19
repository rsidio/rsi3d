//! 远程渲染 / 转流的**端到端**测试：真起 HTTP 服务，用真 socket 连。
//!
//! 这些用例要证明的不是"函数返回值对"，而是"语义在真网络上成立"：
//! 没有 token 拿不到数据、静止场景零带宽、断线能用 `Last-Event-ID` 续上、
//! 命令走 SSE 之外的通道回到同一条流里。

use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::json;

use rsi3d_harness_core::Document;
use rsi3d_harness_serve::peer;
use rsi3d_harness_serve::{serve, ServeOptions};
use rsi3d_harness_stream::protocol::{ServerMessage, StreamKind};

const SCENE: &str = r##"{
  "units": "m",
  "room": { "size": [4.2, 2.8, 6.0] },
  "window": { "wall": "north", "spanX": [-1.2, 1.2], "height": 1.6, "z": -2.9, "bandDepth": 1.5 },
  "objects": [
    { "id": "obj:sofa_01", "role": "furniture", "material": "fabric",
      "aabb": { "min": [-1.1, 0.0, -2.6], "max": [0.9, 0.85, -1.7] } },
    { "id": "obj:table_01", "role": "furniture", "material": "oak",
      "aabb": { "min": [-0.5, 0.0, 2.2], "max": [0.5, 0.45, 2.9] } }
  ],
  "lights": [ { "id": "sun", "kind": "directional", "intensity": 2.0, "direction": [0.3,-1.0,0.4] } ],
  "clearance_rules": [
    { "pair": ["obj:sofa_01", "obj:table_01"], "min": 0.4, "max": 0.8, "reason": "茶几" }
  ]
}"##;

/// 起一个测试用的服务（端口随机、名字唯一，便于断言"还是不是我们那个服务"）。
fn start(token: &str) -> rsi3d_harness_serve::ServeHandle {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let mut opts = ServeOptions::new(Document::from_scene_json(SCENE).unwrap())
        .with_token(token)
        .with_port(0) // 让系统挑端口：测试并行跑也不会撞
        .with_fps(4);
    opts.name = format!("test-{}", N.fetch_add(1, Ordering::Relaxed));
    serve(opts).expect("服务应当能启动")
}

fn scene_path(token: &str, from: Option<u32>) -> String {
    peer::stream_path(StreamKind::Scene, None, token, from)
}

// ---------------------------------------------------------------- 门禁

#[test]
fn token_gate_and_healthz() {
    let h = start("s3cret");
    let base = h.base_url();

    // 健康检查不需要令牌（只有计数与哈希，没有资产内容）
    let (code, body) = peer::get(&base, "/healthz").unwrap();
    assert_eq!(code, 200);
    assert!(body.contains("\"protocol\""), "{}", body);

    // 没有令牌：拿不到流
    let p = peer::connect(&base, "/stream/scene").unwrap();
    assert_eq!(p.status, 401, "无令牌必须被拒");

    // 令牌不对：也拒绝（而且比较是定时的）
    let p = peer::connect(&base, "/stream/scene?token=wrong").unwrap();
    assert_eq!(p.status, 401);

    // 带上令牌：连通
    let p = peer::connect(&base, &scene_path("s3cret", None)).unwrap();
    assert_eq!(p.status, 200);
    assert!(
        p.content_type.contains("text/event-stream"),
        "Content-Type 应当是 SSE：{}",
        p.content_type
    );

    // 命令口同样要令牌
    let (code, _) = peer::post(&base, "/command", "{}").unwrap();
    assert_eq!(code, 401);
    let (code, body) = peer::post(
        &base,
        &format!("/command?token={}", "s3cret"),
        &json!({"type": "command", "op": "remove", "target": "obj:nope", "reason": "测试"}).to_string(),
    )
    .unwrap();
    assert_eq!(code, 409, "认出来了但目标不存在：{}", body);
    assert!(body.contains("unknown_target"), "{}", body);

    // 客户端页面与脚本是静态资源，不需要令牌（里面没有数据）
    let (code, page) = peer::get(&base, "/").unwrap();
    assert_eq!(code, 200);
    assert!(page.contains("rsi3d-harness"), "页面应当内嵌在二进制里");
    let (code, js) = peer::get(&base, "/client.js").unwrap();
    assert_eq!(code, 200);
    assert!(js.contains("EventSource"), "客户端要真用 EventSource");
    // 但数据口仍然要令牌
    assert_eq!(peer::get(&base, "/observe").unwrap().0, 401);

    h.stop();
}

// ---------------------------------------------------------------- 全流程

#[test]
fn scene_stream_pushes_snapshot_then_patches_and_obeys_commands() {
    let h = start("tk");
    let base = h.base_url();
    let mut p = peer::connect(&base, &scene_path("tk", None)).unwrap();

    // 握手 + 全量
    let welcome = p.next_message().unwrap().unwrap();
    match &welcome {
        ServerMessage::Welcome { protocol, kind, geometry, resumed, .. } => {
            assert_eq!(protocol, "rsi3d-stream/v1");
            assert_eq!(*kind, StreamKind::Scene);
            assert_eq!(geometry, "aabb-proxy", "必须老实说清几何档次");
            assert!(!*resumed);
        }
        other => panic!("第一条应当是 welcome：{:?}", other),
    }
    let snap = p.next_message().unwrap().unwrap();
    let sofa_z = match &snap {
        ServerMessage::Snapshot { gltf, revision, .. } => {
            assert_eq!(*revision, 0);
            let nodes = gltf["nodes"].as_array().unwrap();
            let sofa = nodes
                .iter()
                .find(|n| n["name"] == "obj:sofa_01")
                .expect("快照里应当有沙发");
            assert_eq!(sofa["extras"]["rsi3d"]["geometry"], "aabb-proxy");
            sofa["translation"][2].as_f64().unwrap()
        }
        other => panic!("第二条应当是全量快照：{:?}", other),
    };
    assert_eq!(p.last_id, Some(0), "事件 id 就是状态版本（续传靠它）");

    // 通过 HTTP 发一条命令：改动必须出现在**同一条流**里
    let (code, body) = peer::post(
        &base,
        &format!("/command?token=tk"),
        &json!({"op": "transform", "target": "sofa_01",
                "params": {"translate": [0, 0, 1.3]},
                "reason": "沙发挡住窗户采光，挪出挡光带"})
        .to_string(),
    )
    .unwrap();
    assert_eq!(code, 200, "{}", body);
    // 回执里认得出这一步：版本前进 + 理由是收到的那句
    let applied: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(applied["revision"], json!(1));
    assert_eq!(applied["op"], "transform");
    assert_eq!(applied["reason"], "沙发挡住窗户采光，挪出挡光带");

    let patch = p.next_message().unwrap().unwrap();
    match &patch {
        ServerMessage::Patch { from, to, changes, .. } => {
            assert_eq!((*from, *to), (0, 1));
            let upserts = changes["nodes_upsert"].as_array().unwrap();
            assert_eq!(upserts.len(), 1, "只有沙发受影响");
            let z = upserts[0]["translation"][2].as_f64().unwrap();
            assert!((z - (sofa_z + 1.3)).abs() < 1e-9, "新位置应当是 {}，得到 {}", sofa_z + 1.3, z);
            assert_eq!(changes["blockers"], json!([]), "挪开之后没人挡窗了");
        }
        other => panic!("应当是增量：{:?}", other),
    }
    assert_eq!(p.last_id, Some(1));

    // 撤销（checkout 到上一版）：流里必须体现"回去了"
    let (code, _) = peer::post(
        &base,
        "/command?token=tk",
        &json!({"op": "checkout", "params": {"rev": 0}, "reason": "撤销一步"}).to_string(),
    )
    .unwrap();
    assert_eq!(code, 200);
    let back = p.next_message().unwrap().unwrap();
    match &back {
        ServerMessage::Patch { to, changes, .. } => {
            assert_eq!(*to, 2, "回滚也是一次状态推进");
            let z = changes["nodes_upsert"][0]["translation"][2].as_f64().unwrap();
            assert!((z - sofa_z).abs() < 1e-9, "应当回到挪之前的位置");
            assert_eq!(changes["blockers"], json!(["obj:sofa_01"]), "又挡窗了");
        }
        other => panic!("应当是增量：{:?}", other),
    }

    h.stop();
}

#[test]
fn static_scene_costs_zero_bandwidth() {
    let h = start("tk");
    let base = h.base_url();

    // 场景流：拿到全量之后什么都不做，就不该再有消息
    let mut s = peer::connect(&base, &scene_path("tk", None)).unwrap();
    let _ = s.next_message().unwrap().unwrap(); // welcome
    let _ = s.next_message().unwrap().unwrap(); // snapshot

    // 图像流：拿到一帧之后不推第二帧
    let mut f = peer::connect(&base, &peer::stream_path(StreamKind::Frame, Some("top"), "tk", None)).unwrap();
    let _ = f.next_message().unwrap().unwrap(); // welcome
    match f.next_message().unwrap().unwrap() {
        ServerMessage::Frame { png_base64, .. } => assert!(png_base64.starts_with("iVBORw0KGgo")),
        other => panic!("应当是帧：{:?}", other),
    }

    let before = h.stats();
    std::thread::sleep(Duration::from_millis(600));
    let after = h.stats();
    assert_eq!(
        before["messages_sent"], after["messages_sent"],
        "静止场景不该有任何推送（这正是'只推变化'）"
    );
    assert_eq!(before["frames_sent"], after["frames_sent"], "像素没变就不该重推帧");
    assert_eq!(after["connections"], 2, "两个连接应当被记上");

    h.stop();
}

// ---------------------------------------------------------------- 续传

#[test]
fn reconnect_with_last_event_id_resumes_from_that_revision() {
    let h = start("tk");
    let base = h.base_url();

    // 第一个客户端：拿全量（rev 0）后"掉线"
    let mut a = peer::connect(&base, &scene_path("tk", None)).unwrap();
    let _ = a.next_message().unwrap().unwrap();
    let _ = a.next_message().unwrap().unwrap();
    let seen = a.last_id.unwrap();
    assert_eq!(seen, 0);
    drop(a);

    // 掉线期间服务端继续被改
    for (i, z) in [1.0f64, 2.0, 3.0].iter().enumerate() {
        let (code, _) = peer::post(
            &base,
            "/command?token=tk",
            &json!({"op": "transform", "target": "obj:table_01",
                    "params": {"translate": [0, 0, *z]},
                    "reason": format!("掉线期间第 {} 次改动", i + 1)})
            .to_string(),
        )
        .unwrap();
        assert_eq!(code, 200);
    }

    // 重连：带上 Last-Event-ID（= 我们**真正看到过**的版本）
    let mut b = peer::connect(&base, &scene_path("tk", Some(seen))).unwrap();
    match b.next_message().unwrap().unwrap() {
        ServerMessage::Welcome { resumed, revision, .. } => {
            assert!(resumed, "应当续上");
            assert_eq!(revision, 3);
        }
        other => panic!("应当是 welcome：{:?}", other),
    }
    // **关键**：续传拿到的是增量，不是又一张全量
    match b.next_message().unwrap().unwrap() {
        ServerMessage::Patch { from, to, .. } => {
            assert_eq!(from, seen);
            assert_eq!(to, 3, "一次补齐掉线期间的 3 个版本");
        }
        other => panic!("续传应当是增量：{:?}", other),
    }
    assert_eq!(b.last_id, Some(3));

    // 非法的 Last-Event-ID（超前/乱给）→ 退回全量，而不是猜
    let mut c = peer::connect(&base, &scene_path("tk", Some(9999))).unwrap();
    match c.next_message().unwrap().unwrap() {
        ServerMessage::Welcome { resumed, .. } => assert!(!resumed),
        other => panic!("{:?}", other),
    }
    assert!(matches!(
        c.next_message().unwrap().unwrap(),
        ServerMessage::Snapshot { .. }
    ));

    h.stop();
}

// ---------------------------------------------------------------- 图像流

#[test]
fn frame_stream_carries_png_and_servers_own_measurement() {
    let h = start("tk");
    let base = h.base_url();
    let mut p = peer::connect(
        &base,
        &peer::stream_path(StreamKind::Frame, Some("top"), "tk", None),
    )
    .unwrap();
    let _ = p.next_message().unwrap().unwrap();

    let frame = p.next_message().unwrap().unwrap();
    match &frame {
        ServerMessage::Frame { view, width, height, image_hash, band_occlusion, .. } => {
            assert_eq!(view, "top");
            assert_eq!((*width, *height), (480, 360));
            assert_eq!(image_hash.len(), 64, "sha256");
            // 图像流的价值：结论随帧一起到（浏览器不用自己算视觉）
            assert!(
                band_occlusion.unwrap() > 0.2,
                "沙发在窗带里，遮挡率应当明显：{:?}",
                band_occlusion
            );
        }
        other => panic!("{:?}", other),
    }

    // 换视角：另一条订阅（相机是每连接状态）
    let mut q = peer::connect(
        &base,
        &peer::stream_path(StreamKind::Frame, Some("front"), "tk", None),
    )
    .unwrap();
    let _ = q.next_message().unwrap().unwrap();
    match q.next_message().unwrap().unwrap() {
        ServerMessage::Frame { view, .. } => {
            assert_eq!(view, "front");
        }
        other => panic!("{:?}", other),
    }

    h.stop();
}

// ---------------------------------------------------------------- 观测一致性

#[test]
fn observe_endpoint_matches_the_kernel() {
    let h = start("tk");
    let base = h.base_url();
    let (code, body) = peer::get(&base, "/observe?token=tk").unwrap();
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    // 与 MCP/CLI 是同一份观测：节点表、挡窗者、告警都在
    assert!(v["view"]["objects"].is_array(), "{}", body);
    assert_eq!(v["window_blockers"], json!(["obj:sofa_01"]));
    assert!(v["warnings"].is_array());

    // glTF 导出也能直接拿走
    let (code, gltf) = peer::get(&base, "/snapshot.gltf?token=tk").unwrap();
    assert_eq!(code, 200);
    let g: serde_json::Value = serde_json::from_str(&gltf).unwrap();
    assert_eq!(g["asset"]["version"], "2.0");
    assert_eq!(g["nodes"].as_array().unwrap().len(), 2);

    h.stop();
}

#[test]
fn http_10_clients_are_refused_clearly() {
    let h = start("tk");
    // 手写一个 HTTP/1.0 请求：tiny_http 会为 1.0 选 Identity 编码（无分块），
    // 那样服务端会把永不结束的流缓冲到内存里 —— 与其挂死，不如 426。
    let mut sock = TcpStream::connect(h.addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    use std::io::{Read, Write};
    sock.write_all(b"GET /stream/scene?token=tk HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut buf = [0u8; 512];
    let n = sock.read(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(text.contains("426"), "HTTP/1.0 应当被明确拒绝：{}", text);
    assert!(text.contains("http_1_1_required"), "{}", text);
    h.stop();
}

#[test]
fn stop_ends_streams_and_releases_the_listener() {
    let h = start("tk");
    let base = h.base_url();
    let mut p = peer::connect(&base, &scene_path("tk", None)).unwrap();
    let _ = p.next_message().unwrap().unwrap(); // welcome

    // 1. 正在推的流必须结束——否则那条连接线程会一直挂着（CLI 停不掉就麻烦了）
    h.stop();
    let mut ended = false;
    for _ in 0..20 {
        match p.next_message() {
            Ok(Some(_)) => continue, // 已经发出去的几条，读完
            Ok(None) | Err(_) => {
                ended = true;
                break;
            }
        }
    }
    assert!(ended, "stop() 之后正在推的流必须结束");

    // 2. accept 循环退出，端口随之释放。
    //    注意：**不能**断言"连不上"——测试是并行跑的，刚释放的端口可能马上被
    //    另一个用例的服务拿走（那就是别人的服务）。所以断言的是"这个端口上
    //    已经没有**我们**这个服务了"（用唯一 name 区分）。
    let name = h.name.clone();
    drop(h);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let gone = match peer::get(&base, "/healthz") {
            Err(_) => true, // 连不上：正是我们要的
            Ok((code, body)) => code != 200 || !body.contains(&name),
        };
        if gone {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "stop() + drop 之后端口上不该还是我们这个服务"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
