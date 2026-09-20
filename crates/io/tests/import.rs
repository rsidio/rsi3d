//! 导入层的端到端闸门。
//!
//! 这里守的是**导入器最容易撒的两个谎**：
//! ① 产出的场景我们自己读不回来（内核读不了 / 契约不认的键）；
//! ② 报告说"导入了"，但几何其实没落盘或落错了地方。
//!
//! 所以每个用例都走「文件 → import() → 场景 JSON 往返 → 契约校验 → side-car 存在且是 GLB」。

use std::path::PathBuf;

use rsi3d_harness_io::{import, ImportOptions, Imported};
use rsi3d_harness_core::Scene;

/// 每个用例自己的临时目录（不共享，免得互相踩）。
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rsi3d-io-test-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("临时目录建不出来");
    dir
}

fn opts(sidecar: Option<PathBuf>) -> ImportOptions {
    ImportOptions {
        unit_scale: 1.0,
        limit: None,
        blender: std::env::var_os("RSI3D_BLENDER").map(PathBuf::from),
        sidecar,
        role: "other".into(),
    }
}

/// **共同出口**：任何导入结果都得过这几关，一关不过就是 bug 不是"格式特殊"。
fn assert_import_is_usable(imported: &Imported, sidecar: Option<&PathBuf>) {
    // ① 场景 JSON 往返：内核读得回来，且每个对象原样保留（extras 不许被吞掉）
    let text = serde_json::to_string_pretty(&imported.scene).expect("序列化失败");
    let back: Scene = Scene::from_json(&text).expect("内核读不回自己写的场景");
    assert_eq!(
        back.objects.len(),
        imported.scene.objects.len(),
        "往返之后对象少了"
    );
    for (a, b) in imported.scene.objects.iter().zip(back.objects.iter()) {
        assert_eq!(a.id, b.id, "往返之后 id 变了");
        assert_eq!(a.aabb.min, b.aabb.min, "{} 的包围盒在往返里被改了", a.id);
        assert_eq!(
            a.extras, b.extras,
            "{} 的 extras 在往返里丢了（mesh_ref 就是这么丢的）",
            a.id
        );
    }

    // ② 契约：未知键 / 类型不符一律爆出来（rsi3d.* 是 extras，会以 UnknownKey 呈现，
    //    所以只允许它，别的一律不许）
    let findings = rsi3d_harness_contract::validate_scene_json(
        &serde_json::to_value(&imported.scene).unwrap(),
    );
    let bad: Vec<String> = findings
        .iter()
        .filter(|f| !f.pattern().starts_with("objects[].rsi3d"))
        .map(|f| f.to_string())
        .collect();
    assert!(bad.is_empty(), "导入产物不合契约：{:#?}", bad);

    // ③ side-car：说了写了就得真有，而且是能读的 GLB
    if let Some(p) = sidecar {
        assert!(p.exists(), "报告里说了 side-car {}，文件却不在", p.display());
        let bytes = std::fs::read(p).unwrap();
        assert_eq!(&bytes[0..4], b"glTF", "side-car 不是 GLB");
        let (_, _, _) = rsi3d_harness_io::readers::parse_glb(&bytes).expect("side-car 读不回来");
    }
}

/// 回归：**每个对象只许带自己的顶点**。
///
/// 曾经踩过的坑：图省事让所有对象共享全局顶点表 → 每个对象的 AABB 都等于整体 AABB。
/// 这个错误**不报错、不崩**，只是从此以后所有"间距/挡窗"测量全是假的。所以钉死它。
#[test]
fn each_object_gets_its_own_aabb_not_the_whole_file() {
    let dir = scratch("per-object-aabb");
    let src = dir.join("two_parts.obj");
    // A 在 x≈0，B 在 x≈10：两个对象各 1 米见方
    std::fs::write(
        &src,
        "o Part_A\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n\
         o Part_B\nv 10 0 0\nv 11 0 0\nv 11 1 0\nv 10 1 0\nf 5 6 7 8\n",
    )
    .unwrap();

    let imported = import(&src, &opts(None)).expect("两对象导入失败");
    assert_eq!(imported.report.objects, 2, "两个 `o` 段就该出两个对象");

    let a = &imported.scene.objects[0];
    let b = &imported.scene.objects[1];
    assert_eq!(a.id, "obj:Part_A");
    assert_eq!(b.id, "obj:Part_B");
    assert_eq!(
        (a.aabb.min[0], a.aabb.max[0]),
        (0.0, 1.0),
        "Part_A 的 AABB 被别的对象的顶点撑大了——间距测量会全错"
    );
    assert_eq!((b.aabb.min[0], b.aabb.max[0]), (10.0, 11.0));
    // 顶点数只算被面引用到的
    assert_eq!(a.metrics.triangles, 2);
    assert_eq!(imported.report.vertices, 8, "8 个被引用的顶点（不是 8×2）");
}

#[test]
fn obj_imports_end_to_end() {
    let dir = scratch("obj");
    let src = dir.join("part.obj");
    // 一个 2×2×2 的盒子（OBJ 是 Y 上惯例，但我们不做轴向转换，数字原样）
    // ⚠️ 这些夹具**不能**用行尾 `\` 续行：Rust 会把换行连同缩进一起吃掉，整个文件变成一行
    let obj = "o Box_A\n\
v -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nv -1 1 -1\n\
v -1 -1 1\nv 1 -1 1\nv 1 1 1\nv -1 1 1\n\
f 1 2 3 4\nf 5 6 7 8\nf 1 2 6 5\nf 3 4 8 7\nf 1 4 8 5\nf 2 3 7 6\n";
    std::fs::write(&src, obj).unwrap();
    let sidecar = dir.join("part.mesh.glb");

    let imported = import(&src, &opts(Some(sidecar.clone()))).expect("OBJ 导入失败");

    assert_eq!(imported.report.objects, 1);
    assert_eq!(imported.report.vertices, 8);
    assert_eq!(imported.report.triangles, 12, "6 个四边形 → 12 个三角面");
    assert_eq!(imported.report.bbox, [-1.0, -1.0, -1.0, 1.0, 1.0, 1.0]);
    assert_eq!(imported.report.source_cs, "unknown", "OBJ 没有坐标系声明，不许猜");
    assert_eq!(imported.report.read_cs, "unknown");
    assert!(
        imported
            .report
            .warnings
            .iter()
            .any(|w| w.contains("不自带") && w.contains("不猜轴向")),
        "没有声明坐标系的格式要出告警，且说清我们没替你转：{:?}",
        imported.report.warnings
    );
    assert_import_is_usable(&imported, Some(&sidecar));
}

#[test]
fn ascii_stl_imports_end_to_end() {
    let dir = scratch("stl-ascii");
    let src = dir.join("triangle.stl");
    std::fs::write(
        &src,
        // 三角面之间**必须**留换行，`facet`/`vertex` 是行长判断的锚点
        "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n",
    )
    .unwrap();

    let imported = import(&src, &opts(None)).expect("ASCII STL 导入失败");
    assert_eq!(imported.report.triangles, 1);
    assert_eq!(imported.report.vertices, 3);
    assert_eq!(imported.report.sidecar, None, "--no-mesh 时不该报 side-car");
    assert!(
        imported.report.warnings.iter().any(|w| w.contains("薄片")),
        "三角形是零厚度，得提醒它在光栅里看不见：{:?}",
        imported.report.warnings
    );
    assert_import_is_usable(&imported, None);
}

#[test]
fn binary_stl_imports_end_to_end() {
    let dir = scratch("stl-bin");
    let src = dir.join("plate.stl");
    // 二进制 STL：80 字节头 + 三角面数 + 每面 50 字节（12 字节法向 + 3×12 顶点 + 2 属性）
    let mut b = vec![0u8; 80];
    b[..8].copy_from_slice(b"rsi3d-io");
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&[0u8; 12]); // 法向
    for v in [
        [0.0f32, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [0.0, 0.0, 3.0],
    ] {
        for c in v {
            b.extend_from_slice(&c.to_le_bytes());
        }
    }
    b.extend_from_slice(&[0u8; 2]);
    assert_eq!(b.len(), 84 + 50, "夹具本身得符合 STL 的长度约定，否则测的是别的东西");
    std::fs::write(&src, &b).unwrap();

    let imported = import(&src, &opts(None)).expect("二进制 STL 导入失败");
    assert_eq!(imported.report.triangles, 1);
    assert_eq!(imported.report.bbox, [0.0, 0.0, 0.0, 2.0, 0.0, 3.0]);
    assert_import_is_usable(&imported, None);
}

#[test]
fn limit_says_out_loud_that_it_truncated() {
    let dir = scratch("limit");
    let src = dir.join("two.obj");
    std::fs::write(
        &src,
        "o A\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n\
o B\nv 5 0 0\nv 6 0 0\nv 5 1 0\nf 1 2 3\n\n",
    )
    .unwrap();

    let mut o = opts(None);
    o.limit = Some(1);
    let imported = import(&src, &o).expect("截断导入失败");

    assert_eq!(imported.report.objects, 1);
    assert!(
        imported.report.warnings.iter().any(|w| w.contains("被截断")),
        "截断必须说出来，而且要说清原文件有几个：{:?}",
        imported.report.warnings
    );
    assert_import_is_usable(&imported, None);
}

#[test]
fn unit_scale_shrinks_millimetre_models_and_says_so() {
    let dir = scratch("units");
    let src = dir.join("mm.obj");
    // 一个 2000 mm 宽的盒子：不缩放就是"体育场"
    std::fs::write(
        &src,
        "o Big\nv 0 0 0\nv 2000 0 0\nv 2000 100 0\nv 0 100 0\nf 1 2 3 4\n",
    )
    .unwrap();

    let mut o = opts(None);
    o.unit_scale = 0.001;
    let imported = import(&src, &o).expect("缩放导入失败");
    assert_eq!(imported.report.bbox[3], 2.0, "2000 mm → 2 m");
    assert!(
        !imported.report.warnings.iter().any(|w| w.contains("体育场")),
        "缩放对了就不该再报单位问题：{:?}",
        imported.report.warnings
    );

    // 反过来：不缩放时必须提醒
    let big = import(&src, &opts(None)).expect("未缩放导入失败");
    assert!(
        big.report.warnings.iter().any(|w| w.contains("unit-scale")),
        "2000 米不提醒就是把单位问题甩给用户：{:?}",
        big.report.warnings
    );
}

/// B-rep 与"Blender 读不了的 blend"这两条路，**必须给的是有用的错误**，不是"失败"。
#[test]
fn honest_errors_instead_of_silent_failure() {
    let dir = scratch("honest");

    // STEP：不许假装能读
    let step = dir.join("part.stp");
    std::fs::write(&step, "ISO-10303-21;\n").unwrap();
    let e = import(&step, &opts(None)).unwrap_err();
    assert!(e.contains("B-rep"), "得说清是 B-rep：{e}");
    assert!(e.contains("STL"), "得给出路：{e}");

    // 一个**假的** blend（头部被改过的真实文件我们也有，但这里要的是确定性）：
    // 必须把 Blender 自己的错误原样带出来，而不是说"导入失败"
    let fake = dir.join("fake.blend");
    std::fs::write(&fake, b"BLENDER17-01v0502NOTREALLY").unwrap();
    match import(&fake, &opts(None)) {
        Ok(_) => panic!("这不是 blend，居然读成功了"),
        Err(e) => {
            assert!(
                e.contains("Blender"),
                "错误里得点名是谁说的：{e}"
            );
            assert!(
                e.len() > 40,
                "错误太短，用户拿不到线索：{e}"
            );
        }
    }
}

/// 真 Blender 夹具：造一个 blend（Blender 自己写、自己读得回的那种），再导一遍。
///
/// 没装 Blender 就**说明原因跳过**，不静默通过。
#[test]
fn blender_roundtrip_on_a_real_blend() {
    let Some(blender) = rsi3d_harness_io::convert::find_blender(None).ok() else {
        println!("跳过：这台机器上没有 Blender（find_blender 找不到）——blend 那条路没法验");
        return;
    };
    let version = rsi3d_harness_io::convert::blender_version(&blender);
    println!("用 {} 造夹具", version);

    let dir = scratch("blender");
    let blend = dir.join("fixture.blend");
    let py = format!(
        "import bpy\n\
         bpy.ops.wm.read_factory_settings(use_empty=True)\n\
         bpy.ops.mesh.primitive_cube_add(size=1.0)\n\
         bpy.context.object.name='Cube_A'\n\
         bpy.ops.mesh.primitive_uv_sphere_add(radius=0.5, location=(3,0,0))\n\
         bpy.context.object.name='Sphere_B'\n\
         bpy.ops.mesh.primitive_plane_add(size=4.0, location=(0,0,-0.5))\n\
         bpy.context.object.name='Floor_01'\n\
         bpy.ops.wm.save_as_mainfile(filepath=r'{}')\n",
        blend.display()
    );
    let out = std::process::Command::new(&blender)
        .args(["--background", "--factory-startup", "--python-expr", &py])
        .output()
        .expect("Blender 起不来");
    if !blend.exists() {
        panic!(
            "Blender 没有写出夹具（退出码 {:?}）：\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let sidecar = dir.join("fixture.mesh.glb");
    let imported = import(&blend, &opts(Some(sidecar.clone()))).expect("标准 blend 导入失败");

    assert!(
        imported.report.objects >= 3,
        "三个对象（cube/sphere/plane）才对：{:?}",
        imported.report
    );
    assert!(imported.report.triangles > 0, "三角面为 0 就是没读到几何");
    assert!(
        imported.report.via.as_deref().unwrap_or("").contains("Blender"),
        "经手人得写清是哪个 Blender：{:?}",
        imported.report.via
    );
    // 坐标系的事实：blend 自己 Z 上，Blender 导出 glTF 时转了轴
    assert_eq!(imported.report.source_cs, "RUF");
    assert_eq!(imported.report.read_cs, "RDF");
    assert!(
        imported.report.warnings.iter().any(|w| w.contains("转轴")),
        "转过轴就得说：{:?}",
        imported.report.warnings
    );
    assert!(
        imported.report.warnings.iter().any(|w| w.contains("Floor_01")),
        "平面在光栅里是看不见的，得点名：{:?}",
        imported.report.warnings
    );

    // 节点上真的挂了 side-car 引用，而且每个对象都有 role
    for o in &imported.scene.objects {
        assert!(
            o.extras.contains_key("rsi3d"),
            "{} 上没有 rsi3d extras（客户端就没法显示真网格）",
            o.id
        );
    }
    assert!(imported
        .scene
        .objects
        .iter()
        .any(|o| o.role == "floor" && o.id.contains("Floor")));
    assert_import_is_usable(&imported, Some(&sidecar));
}
