//! 流协议的端到端测试：**把服务端与客户端对着跑**，然后断言两边一致。
//!
//! 这里刻意不用真实网络（那是 `crates/serve` 的事）：会话层必须能脱离传输被测，
//! 否则"流坏了"和"HTTP 坏了"永远分不清。

use serde_json::json;

use rsi3d_harness_core::{CommandRequest, Document, Scene};
use rsi3d_harness_render::ViewKind;
use rsi3d_harness_stream::protocol::{
    Camera, ClientMessage, ServerMessage, StreamKind, RENDERER_CPU_RASTER,
};
use rsi3d_harness_stream::session::{
    session_for_view, ClientSession, Outbox, ScenePatch, ServerSession,
};

const SCENE: &str = r##"{
  "units": "m",
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
  "lights": [ { "id": "sun", "kind": "directional", "intensity": 2.0, "direction": [0.3,-1.0,0.4] } ],
  "clearance_rules": [
    { "pair": ["obj:sofa_01", "obj:table_01"], "min": 0.4, "max": 0.8, "reason": "茶几" }
  ]
}"##;

fn scene() -> Scene {
    Scene::from_json(SCENE).unwrap()
}

fn cmd(op: &str, target: &str, params: serde_json::Value, reason: &str) -> CommandRequest {
    CommandRequest {
        op: op.to_string(),
        target: Some(target.to_string()),
        params,
        reason: reason.to_string(),
        expect: None,
    }
}

/// 服务端场景 → 客户端能拿来比的 (id, aabb) 表。
fn server_nodes(doc: &Document) -> Vec<(String, [f64; 3], [f64; 3])> {
    let mut v: Vec<_> = doc
        .scene()
        .objects
        .iter()
        .map(|n| (n.id.clone(), n.aabb.center(), n.aabb.size()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn client_nodes(c: &ClientSession) -> Vec<(String, [f64; 3], [f64; 3])> {
    let mut v: Vec<_> = c
        .scene()
        .nodes
        .values()
        .map(|n| (n.name.clone(), n.translation, n.scale))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// 推一轮：让服务端把该发的都发出来，客户端全吃掉。
fn pump(server: &mut ServerSession, client: &mut ClientSession) -> usize {
    let msgs = server.poll();
    let n = msgs.len();
    for m in &msgs {
        client.apply(m).expect("客户端应当能吃下服务端的消息");
    }
    n
}

// ---------------------------------------------------------------- 核心不变量

#[test]
fn client_rebuilds_exactly_what_the_server_has_scene_stream() {
    let mut server = ServerSession::new(
        Document::new(scene()).unwrap(),
        StreamKind::Scene,
        Camera::default(),
        320,
        240,
    );
    let mut client = ClientSession::new();

    // 1. 握手 → 全量
    let welcome = server.welcome(None);
    client.apply(&welcome).unwrap();
    assert!(matches!(welcome, ServerMessage::Welcome { resumed: false, .. }));
    assert_eq!(pump(&mut server, &mut client), 1, "首推应当是一次全量快照");
    assert_eq!(server_nodes(server.document()), client_nodes(&client), "全量之后两边必须一致");

    // 2. 连续改：挪沙发 → 调灯光 → 删地毯 → 挪茶几
    server
        .apply_command(&cmd("transform", "sofa_01", json!({"translate":[0,0,1.3]}), "挪出挡光带"))
        .unwrap();
    assert_eq!(pump(&mut server, &mut client), 1, "有变化就该推一个增量");
    assert_eq!(server_nodes(server.document()), client_nodes(&client));

    server
        .apply_command(&cmd("set_light", "sun", json!({"intensity": 1.1}), "过曝"))
        .unwrap();
    pump(&mut server, &mut client);
    assert_eq!(
        client.scene().lights["sun"]["intensity"], 1.1,
        "灯光变化也要同步（它在场景级数组里，不是节点）"
    );

    server.apply_command(&cmd("remove", "obj:rug_01", json!({}), "地毯换成新的")).unwrap();
    pump(&mut server, &mut client);
    assert_eq!(server_nodes(server.document()), client_nodes(&client));
    assert!(!client.scene().nodes.contains_key("obj:rug_01"), "删掉的节点必须从客户端也消失");

    server
        .apply_command(&cmd("transform", "table_01", json!({"translate":[0,0,-2.4]}), "挪近"))
        .unwrap();
    pump(&mut server, &mut client);
    assert_eq!(server_nodes(server.document()), client_nodes(&client));

    // 3. 回滚（RSI 的关键动作）——客户端也必须跟着退回去
    server.document();
    let mut s = server;
    s.apply_command(&cmd("checkout", "", json!({"rev": 1}), "回到最好那一版"))
        .unwrap();
    pump(&mut s, &mut client);
    assert_eq!(
        server_nodes(s.document()),
        client_nodes(&client),
        "回滚之后客户端必须和服务端一致（用状态版本 diff，而不是日志序号）"
    );

    // 4. 元数据也一致：颜色/可编辑性/几何档次/挡窗者
    let gltf_node = client.scene().nodes["obj:sofa_01"].extras.clone();
    assert_eq!(gltf_node["rsi3d"]["geometry"], "aabb-proxy");
    assert!(gltf_node["rsi3d"]["color"].is_array());
    // 挡窗者与内核实测值一致（不另起一套判断）：此刻沙发已被回滚到窗带外
    assert_eq!(
        client.scene().extras["window_blockers"],
        json!(s.document().scene().window_blockers()),
        "挡窗者必须与内核测量一致"
    );
}

#[test]
fn no_change_no_traffic() {
    let mut server = ServerSession::new(
        Document::new(scene()).unwrap(),
        StreamKind::Scene,
        Camera::default(),
        320,
        240,
    );
    let _ = server.welcome(None);
    assert_eq!(server.poll().len(), 1, "首推一次全量");
    // 之后什么都不做：不推
    assert!(server.poll().is_empty());
    assert!(server.poll().is_empty());

    // 图像流同理：像素没变就不推
    let mut frames = session_for_view(scene(), ViewKind::Top).unwrap();
    let _ = frames.welcome(None);
    assert_eq!(frames.poll().len(), 1, "第一帧要推");
    assert!(frames.poll().is_empty(), "像素完全一样就不该重复推（这就是只推变化）");
}

#[test]
fn frames_are_reproducible_and_carry_the_measurement() {
    let mut server = session_for_view(scene(), ViewKind::Top).unwrap();
    let _ = server.welcome(None);
    let msgs = server.poll();
    match &msgs[0] {
        ServerMessage::Frame { image_hash, png_base64, band_occlusion, view, .. } => {
            assert_eq!(view, "top");
            assert!(image_hash.len() == 64, "应当是 sha256");
            assert!(png_base64.starts_with("iVBORw0KGgo"), "应当是 PNG");
            // 「窗前挡光带被遮挡 x%」——这是图像流最有分量的一个数字
            assert!(
                band_occlusion.unwrap() > 0.2,
                "沙发在窗带里，遮挡比例该明显：{:?}",
                band_occlusion
            );
        }
        other => panic!("应当是帧：{:?}", other),
    }

    // 换个场景（挪开沙发）→ 帧必须变，且遮挡率归零
    let mut server2 = session_for_view(scene(), ViewKind::Top).unwrap();
    let _ = server2.welcome(None);
    let _ = server2.poll();
    server2
        .apply_command(&cmd("transform", "sofa_01", json!({"translate":[0,0,1.3]}), "挪开"))
        .unwrap();
    match &server2.poll()[0] {
        ServerMessage::Frame { image_hash, band_occlusion, .. } => {
            assert!(*band_occlusion.as_ref().unwrap() < 0.05, "挪开后应当基本通光");
            assert_ne!(*image_hash, String::new());
        }
        other => panic!("应当是帧：{:?}", other),
    }
}

// ---------------------------------------------------------------- 断线续传

#[test]
fn resume_from_last_event_id_gets_only_the_delta() {
    let mut server = ServerSession::new(
        Document::new(scene()).unwrap(),
        StreamKind::Scene,
        Camera::default(),
        320,
        240,
    );
    let mut client = ClientSession::new();
    client.apply(&server.welcome(None)).unwrap();
    pump(&mut server, &mut client);

    // 客户端"掉线"：断线前它看到的是 rev 0，掉线期间服务端又改了两下
    let client_rev_before_offline = client.revision;
    server.apply_command(&cmd("transform", "table_01", json!({"translate":[0,0,-1.0]}), "a")).unwrap();
    server.apply_command(&cmd("set_light", "sun", json!({"intensity": 1.5}), "b")).unwrap();
    let lost = server.poll();
    assert_eq!(lost.len(), 1, "掉线期间服务端产出了增量（没送达）");

    // 重连：EventSource 自动带上 Last-Event-ID（= **它自己看到过**的版本，不是服务端发过的）
    let w = server.welcome(Some(client_rev_before_offline));
    assert!(
        matches!(w, ServerMessage::Welcome { resumed: true, .. }),
        "带合法 Last-Event-ID 应当能续上：{:?}",
        w
    );
    let msgs = server.poll();
    assert_eq!(msgs.len(), 1, "welcome 本身不推场景，场景只走 poll 出来的一条增量");

    // 关键：续传时**不发全量**，只发掉线期间那段增量
    match &msgs[0] {
        ServerMessage::Patch { from, to, .. } => {
            assert_eq!(*from, client_rev_before_offline, "增量要从客户端已知的那版接上");
            assert_eq!(*to, server.state_revision());
        }
        other => panic!("续传应当是增量：{:?}", other),
    }
    for m in &msgs {
        client.apply(m).unwrap();
    }
    assert_eq!(
        server_nodes(server.document()),
        client_nodes(&client),
        "续传之后必须与服务端一致"
    );
}

#[test]
fn resume_with_a_bogus_or_future_id_falls_back_to_snapshot() {
    let mut server = ServerSession::new(
        Document::new(scene()).unwrap(),
        StreamKind::Scene,
        Camera::default(),
        320,
        240,
    );
    // 超出范围的版本号（比如服务端换了文档）：不能猜，直接全量
    let w = server.welcome(Some(9999));
    assert!(matches!(w, ServerMessage::Welcome { resumed: false, .. }));
    assert!(matches!(server.poll()[0], ServerMessage::Snapshot { .. }));
}

#[test]
fn client_refuses_a_patch_that_does_not_connect() {
    let mut server = ServerSession::new(
        Document::new(scene()).unwrap(),
        StreamKind::Scene,
        Camera::default(),
        320,
        240,
    );
    let mut client = ClientSession::new();
    client.apply(&server.welcome(None)).unwrap();
    pump(&mut server, &mut client);

    // 构造一个"接不上"的增量：from 与本地版本不符
    server.apply_command(&cmd("transform", "sofa_01", json!({"translate":[0,0,0.5]}), "x")).unwrap();
    let patch = server.poll().remove(0);
    let mut stale = ClientSession::new();
    stale.apply(&patch).unwrap_err(); // 从 rev 0 出发的客户端收到 from=1 的增量 → 报错而不是静默错
    assert!(stale.errors[0].contains("错位"), "{:?}", stale.errors);
}

// ---------------------------------------------------------------- 背压

#[test]
fn patches_compress_into_a_snapshot_under_load() {
    // 预算 2：第 3 个增量进来就该压缩成快照
    let mut ob = Outbox::new(2);
    for rev in 1..=3u32 {
        ob.push(ServerMessage::Patch {
            from: rev - 1,
            to: rev,
            scene_hash: "h".into(),
            changes: json!({}),
        });
    }
    assert_eq!(ob.compressions, 1, "超预算应当触发压缩");
    // 压缩后：先给一张全量，再给压缩点之后的增量——状态依然是对的
    ob.arm_snapshot(ServerMessage::Snapshot {
        revision: 3,
        scene_hash: "h".into(),
        gltf: json!({}),
    });
    let out = ob.drain();
    assert!(matches!(out[0], ServerMessage::Snapshot { .. }), "压缩后先补全量");
}

#[test]
fn frames_are_latest_wins_and_never_queue_up() {
    let mut ob = Outbox::new(8);
    for i in 1..=5u32 {
        ob.push(ServerMessage::Frame {
            revision: i,
            view: "top".into(),
            width: 1,
            height: 1,
            renderer: RENDERER_CPU_RASTER.into(),
            image_hash: format!("h{}", i),
            png_base64: "x".into(),
            band_occlusion: None,
        });
    }
    assert_eq!(ob.replaced_frames, 4, "帧应当被覆盖而不是排队");
    let out = ob.drain();
    assert_eq!(out.len(), 1, "只留最新一帧");
    match &out[0] {
        ServerMessage::Frame { image_hash, .. } => assert_eq!(image_hash, "h5"),
        other => panic!("{:?}", other),
    }
}

#[test]
fn a_snapshot_resets_pending_patches_and_light_changes_ride_along() {
    let mut ob = Outbox::new(8);
    ob.push(ServerMessage::Patch { from: 0, to: 1, scene_hash: "a".into(), changes: json!({}) });
    ob.push(ServerMessage::Patch { from: 1, to: 2, scene_hash: "b".into(), changes: json!({}) });
    assert_eq!(ob.pending(), 2);
    ob.push(ServerMessage::Snapshot { revision: 2, scene_hash: "b".into(), gltf: json!({}) });
    let out = ob.drain();
    assert_eq!(out.len(), 1, "全量能重置状态，积压的增量就没必要了");
    assert!(matches!(out[0], ServerMessage::Snapshot { .. }));
}

// ---------------------------------------------------------------- 协议细节

#[test]
fn sse_output_is_parseable_by_a_generic_client() {
    let mut server = ServerSession::new(
        Document::new(scene()).unwrap(),
        StreamKind::Scene,
        Camera::default(),
        320,
        240,
    );
    let _ = server.welcome(None);
    let sse = server.poll()[0].to_sse();
    // 一个事件块：id / event / data + 空行
    assert!(sse.starts_with("id: 0\n"), "{}", sse);
    assert!(sse.contains("event: snapshot\n"));
    let data = sse
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .expect("必须有 data 行");
    let parsed: ServerMessage = serde_json::from_str(data).unwrap();
    assert!(matches!(parsed, ServerMessage::Snapshot { .. }));
}

#[test]
fn camera_presets_map_to_the_four_standard_views() {
    for name in ["top", "front", "iso-sw", "iso-se"] {
        let c = Camera::from_preset(name).unwrap();
        assert_eq!(c.view_kind().as_str(), name);
    }
    // 随便给个不认识的：退回默认视角而不是崩
    let bad = Camera { preset: Some("nope".into()), ..Default::default() };
    assert_eq!(bad.view_kind(), ViewKind::IsoSw);
}

#[test]
fn patch_payload_shape_is_stable() {
    let mut server = ServerSession::new(
        Document::new(scene()).unwrap(),
        StreamKind::Scene,
        Camera::default(),
        320,
        240,
    );
    let _ = server.welcome(None);
    let _ = server.poll();
    server.apply_command(&cmd("transform", "sofa_01", json!({"translate":[0,0,1.3]}), "挪")).unwrap();
    match server.poll().remove(0) {
        ServerMessage::Patch { changes, from, to, .. } => {
            assert_eq!((from, to), (0, 1));
            let p: ScenePatch = serde_json::from_value(changes).unwrap();
            // 受影响的那个节点：**完整定义**（客户端按 name upsert 就完事）
            assert_eq!(p.nodes_upsert.len(), 1);
            assert_eq!(p.nodes_upsert[0]["name"], "obj:sofa_01");
            assert!(p.nodes_upsert[0]["translation"].is_array());
            assert!(p.nodes_remove.is_empty());
            assert_eq!(p.blockers, Vec::<String>::new(), "挪开之后没人挡窗了");
            assert!(p.edit_count >= 1);
        }
        other => panic!("应当是增量：{:?}", other),
    }
}

#[test]
fn base64_decoder_matches_the_encoder() {
    for raw in [b"".as_slice(), b"f", b"fo", b"foo", b"foobar", &[0u8, 255, 128, 7]] {
        let enc = rsi3d_harness_render::png::base64(raw);
        assert_eq!(
            rsi3d_harness_stream::decode_b64(&enc).unwrap(),
            raw.to_vec(),
            "编解码必须对称：{:?}",
            raw
        );
    }
}

#[test]
fn client_message_types_cover_what_a_client_needs() {
    // 订阅 / 换相机 / 发命令 / 心跳 —— 上行就这四种
    let msgs = [
        ClientMessage::Subscribe {
            token: "t".into(),
            kind: StreamKind::Frame,
            from_revision: None,
            camera: Some(Camera::from_preset("top").unwrap()),
        },
        ClientMessage::SetCamera { camera: Camera::default() },
        ClientMessage::Command {
            op: "set_light".into(),
            target: Some("sun".into()),
            params: json!({"intensity": 1.0}),
            reason: "过曝".into(),
            expect: None,
        },
        ClientMessage::Ping { nonce: 7 },
    ];
    for m in msgs {
        let line = serde_json::to_string(&m).unwrap();
        assert!(!line.contains('\n'), "上行也必须单行");
        assert_eq!(rsi3d_harness_stream::parse_client(&line).unwrap(), m);
    }
}
