//! 渲染层的测试：**图像能不能当证据**。
//!
//! 三条主线：
//! 1. 画得对（俯视北在上、遮挡真的挡住、id pass 指得准）；
//! 2. 量得准（窗前挡光带被遮挡多少 % —— 这条会进评测）；
//! 3. 对得上账（同场景同参数 ⇒ 同像素哈希；PNG 编解码无损）。

use rsi3d_harness_core::{Command, Document, Scene};
use rsi3d_harness_render::geometry::ids;
use rsi3d_harness_render::{render, png, RenderOptions, ViewKind};

/// H0 的客厅（与脚手架模板同形）：沙发在窗前挡光带里，茶几离沙发 3.9m。
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
  "lights": [ { "id": "sun", "kind": "directional", "intensity": 2.0, "direction": [0.3, -1.0, 0.4] } ]
}"##;

fn h0() -> Document {
    Document::from_scene_json(SCENE).expect("H0 场景应当合法")
}

fn opts(views: &[ViewKind]) -> RenderOptions {
    RenderOptions {
        views: views.to_vec(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------- 画得对

#[test]
fn top_view_is_north_up() {
    let doc = h0();
    let r = render(doc.scene(), &opts(&[ViewKind::Top]));
    let top = &r.views[0];

    // 沙发在 z≈-2.2（北），茶几在 z≈2.5（南）⇒ 俯视图里沙发必须在上方（y 更小）
    let sofa = top.centroid_of(1).expect("沙发应当可见");
    let table = top.centroid_of(2).expect("茶几应当可见");
    assert!(
        sofa.1 < table.1,
        "北在上：沙发(y={:.1}) 应当比茶几(y={:.1}) 更靠上",
        sofa.1,
        table.1
    );
    // 沙发偏西（x 中心 -0.1），茶几居中 ⇒ 沙发略靠左
    assert!(sofa.0 < top.width as f64 * 0.5 + 5.0, "沙发应当大致居中偏左：{:.1}", sofa.0);
}

#[test]
fn the_four_views_are_actually_different() {
    let doc = h0();
    let r = render(doc.scene(), &opts(&ViewKind::all()));
    assert_eq!(r.views.len(), 4);
    let hashes: Vec<&str> = r.views.iter().map(|v| v.image_hash.as_str()).collect();
    let mut uniq = hashes.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), 4, "四个视角必须互不相同");
    for v in &r.views {
        // 每张图都该有实质内容。閈值定在 5%：
        // 立面图本质上「房间里大半是空气」，但地面 + 窗 + 家俱加起来远高于这个线。
        let bg = v.coverage(ids::BACKGROUND);
        assert!(
            bg < 0.95,
            "{} 视角几乎是空白（背景 {:.0}%）",
            v.view.as_str(),
            bg * 100.0
        );
        assert!(v.png.len() > 1000, "{} 的 PNG 太小：{} 字节", v.view.as_str(), v.png.len());
    }
}

#[test]
fn each_surface_is_visible_in_the_views_where_it_makes_sense() {
    // 这条测试顺便把「为什么要四个视角」讲清楚了：
    // 面是**朝向敏感**的——水平面在水平视角里退化，垂直面在正上方看退化。
    // 所以单靠一个视角一定会漏掉东西；观测契约要的是固定四视角。
    let doc = h0();
    let r = render(doc.scene(), &opts(&ViewKind::all()));
    let by = |k: ViewKind| r.views.iter().find(|v| v.view == k).unwrap();

    // 地面（水平面）：俯视与斜视看得到；正立面里必然退化成一条线
    for k in [ViewKind::Top, ViewKind::IsoSw, ViewKind::IsoSe] {
        assert!(by(k).pixels_of(ids::FLOOR) > 0, "{} 里应当看得到地面", k.as_str());
    }
    assert_eq!(
        by(ViewKind::Front).pixels_of(ids::FLOOR),
        0,
        "正立面里水平面应当退化（几何事实，不是 bug）"
    );

    // 窗（垂直面）：立面与斜视看得到；正上方看退化 ⇒
    // 平面图里"窗在哪"是靠挡光带标记表达的。
    for k in [ViewKind::Front, ViewKind::IsoSw, ViewKind::IsoSe] {
        assert!(by(k).pixels_of(ids::WINDOW) > 0, "{} 里应当看得到窗", k.as_str());
    }
    assert_eq!(
        by(ViewKind::Top).pixels_of(ids::WINDOW),
        0,
        "俯视图里垂直面应当退化（几何事实，不是 bug）"
    );

    // 挡光带是地面标记 ⇒ 平面图里必须看得见
    assert!(
        by(ViewKind::Top).pixels_of(ids::BAND) > 0,
        "平面图里应当看得到挡光带"
    );
}

#[test]
fn id_pass_points_at_the_right_object() {
    let doc = h0();
    let r = render(doc.scene(), &opts(&[ViewKind::Top]));
    let top = &r.views[0];

    // 每个对象在俯视图里都该看得见
    for (i, node) in doc.scene().objects.iter().enumerate() {
        assert!(
            top.object_pixels(i) > 0,
            "{} 在俯视图里应当可见",
            node.id
        );
    }
    // 取沙发质心处的像素，id 必须就是沙发
    let (cx, cy) = top.centroid_of(1).unwrap();
    let idx = cy as usize * top.width as usize + cx as usize;
    assert_eq!(top.id[idx], 1, "质心处的 id 应当指向沙发");

    // 窗与挡光带也各自有 id（这样它们能被单独统计/核对）
    assert!(
        top.pixels_of(ids::BAND) > 0,
        "俯视图里应当看得到挡光带"
    );
    assert!(top.pixels_of(ids::FLOOR) > 0, "俯视图里应当看得到地面");
    assert!(!ids::is_object(ids::BAND));
    assert!(!ids::is_object(ids::FLOOR));
    assert!(!ids::is_object(ids::BACKGROUND));
    assert!(ids::is_object(1));
}

#[test]
fn nearer_geometry_wins_the_depth_test() {
    // 大盒子悬在小盒子正上方 ⇒ 俯视时下面的完全看不见（不管它在 objects 里排第几）
    let scene = Scene::from_json(
        r##"{
          "room": { "size": [4.0, 3.0, 4.0] },
          "objects": [
            { "id": "obj:low", "role": "furniture", "material": "oak",
              "aabb": { "min": [-0.5, 0.0, -0.5], "max": [0.5, 0.5, 0.5] } },
            { "id": "obj:high", "role": "furniture", "material": "oak",
              "aabb": { "min": [-1.0, 1.2, -1.0], "max": [1.0, 2.0, 1.0] } }
          ]
        }"##,
    )
    .unwrap();
    let r = render(&scene, &opts(&[ViewKind::Top]));
    let top = &r.views[0];
    assert_eq!(top.object_pixels(0), 0, "被上面盖住的东西不该有可见像素");
    assert!(top.object_pixels(1) > 0, "上面那个应当可见");
}

#[test]
fn an_object_outside_the_room_is_off_frame_but_not_a_crash() {
    let scene = Scene::from_json(
        r##"{
          "room": { "size": [4.0, 3.0, 4.0] },
          "objects": [
            { "id": "obj:far", "role": "furniture", "material": "oak",
              "aabb": { "min": [50.0, 0.0, 50.0], "max": [51.0, 1.0, 51.0] } }
          ]
        }"##,
    )
    .unwrap();
    let r = render(&scene, &opts(&ViewKind::all()));
    assert_eq!(r.views.len(), 4);
    for v in &r.views {
        assert_eq!(v.object_pixels(0), 0, "跑到房间外的东西本来就不该入画");
    }
}

#[test]
fn empty_scene_renders() {
    let r = render(&Scene::default(), &opts(&ViewKind::all()));
    assert_eq!(r.views.len(), 4);
    assert_eq!(r.object_count, 0);
    assert!(r.band_occlusion.is_none(), "没有窗就没有挡光带可算");
}

// ---------------------------------------------------------------- 量得准

#[test]
fn band_occlusion_measures_the_blocking_sofa() {
    let mut doc = h0();
    let before = render(doc.scene(), &opts(&[ViewKind::Top]));
    let blocked = before.band_occlusion.expect("有窗就该算出遮挡比例");
    assert!(
        blocked > 0.2,
        "沙发在窗带里，遮挡比例应当明显：{:.0}%",
        blocked * 100.0
    );

    // 把沙发挪出窗带（+1.3m，与 H0 剧本一致）
    doc.apply(Command::Transform {
        target: "sofa_01".into(),
        translate: Some([0.0, 0.0, 1.3]),
        rotate_y_deg: None,
        scale: None,
    })
    .unwrap();
    let after = render(doc.scene(), &opts(&[ViewKind::Top]));
    let clear = after.band_occlusion.unwrap();
    assert!(
        clear < 0.02,
        "挪开之后窗带应当基本通光，实际 {:.0}%",
        clear * 100.0
    );
}

#[test]
fn band_occlusion_is_visible_in_the_report() {
    let doc = h0();
    let r = render(doc.scene(), &opts(&ViewKind::all()));
    let text = r.to_text(doc.scene());
    assert!(text.contains("窗前挡光带被遮挡"), "报告里应当有这条数字：\n{}", text);
    assert!(text.contains("software"), "报告应当说明后端：\n{}", text);
    let v = r.to_json();
    assert!(v["band_occlusion"].as_f64().unwrap() > 0.2);
    assert_eq!(v["views"].as_array().unwrap().len(), 4);
    assert_eq!(v["backend"], "software");
    assert!(v["degraded"].is_null());
}

// ---------------------------------------------------------------- 对得上账

#[test]
fn same_scene_same_pixels() {
    let doc = h0();
    let a = render(doc.scene(), &opts(&ViewKind::all()));
    let b = render(doc.scene(), &opts(&ViewKind::all()));
    for (x, y) in a.views.iter().zip(b.views.iter()) {
        assert_eq!(x.image_hash, y.image_hash, "{} 的像素哈希应当稳定", x.view.as_str());
        assert_eq!(x.png, y.png, "{} 的 PNG 字节应当稳定", x.view.as_str());
    }
}

#[test]
fn pixels_change_when_the_scene_changes() {
    let mut doc = h0();
    let before = render(doc.scene(), &opts(&[ViewKind::IsoSw])).views[0].image_hash.clone();
    doc.apply(Command::Transform {
        target: "sofa_01".into(),
        translate: Some([0.0, 0.0, 1.3]),
        rotate_y_deg: None,
        scale: None,
    })
    .unwrap();
    let after = render(doc.scene(), &opts(&[ViewKind::IsoSw])).views[0].image_hash.clone();
    assert_ne!(before, after, "改了场景图像必须变");
}

#[test]
fn rollback_restores_the_exact_image() {
    // 这是 H0 的验收项之一：「checkout 之后画面逐像素一致」
    let mut doc = h0();
    let original = render(doc.scene(), &opts(&ViewKind::all()));
    let hashes: Vec<String> = original.views.iter().map(|v| v.image_hash.clone()).collect();

    doc.apply(Command::Transform {
        target: "sofa_01".into(),
        translate: Some([0.4, 0.0, 1.3]),
        rotate_y_deg: None,
        scale: None,
    })
    .unwrap();
    doc.apply(Command::SetLight {
        target: "sun".into(),
        intensity: Some(0.6),
        color: None,
    })
    .unwrap();
    assert_ne!(
        render(doc.scene(), &opts(&[ViewKind::IsoSw])).views[0].image_hash,
        hashes[2],
        "改过之后图像应当不同"
    );

    doc.checkout(0).unwrap();
    let back = render(doc.scene(), &opts(&ViewKind::all()));
    for (v, want) in back.views.iter().zip(hashes.iter()) {
        assert_eq!(
            &v.image_hash, want,
            "回滚后 {} 视角应当逐像素回到原样",
            v.view.as_str()
        );
    }
}

#[test]
fn png_roundtrips_to_the_same_pixels() {
    let doc = h0();
    let r = render(doc.scene(), &opts(&[ViewKind::Top]));
    let v = &r.views[0];
    assert_eq!(&v.png[..8], b"\x89PNG\r\n\x1a\n");
    let (w, h, pixels) = png::decode_rgb(&v.png).expect("PNG 应当能解回来");
    assert_eq!((w, h), (v.width, v.height));
    assert_eq!(pixels, v.rgb, "PNG 必须无损：解回来要逐字节相同");
}

#[test]
fn different_sizes_are_honoured() {
    let doc = h0();
    let r = render(
        doc.scene(),
        &RenderOptions {
            width: 120,
            height: 90,
            views: vec![ViewKind::Top],
            ..Default::default()
        },
    );
    let v = &r.views[0];
    assert_eq!((v.width, v.height), (120, 90));
    assert_eq!(v.rgb.len(), 120 * 90 * 3);
    assert_eq!(v.id.len(), 120 * 90);
    let (w, h, _) = png::decode_rgb(&v.png).unwrap();
    assert_eq!((w, h), (120, 90));
}

#[test]
fn view_kind_parsing_accepts_friendly_names() {
    assert_eq!(ViewKind::parse("plan"), Some(ViewKind::Top));
    assert_eq!(ViewKind::parse("iso"), Some(ViewKind::IsoSw));
    assert_eq!(ViewKind::parse("bogus"), None);
}
