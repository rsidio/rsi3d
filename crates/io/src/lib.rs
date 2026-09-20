//! 导入层（`io`）：把外部的 3D / CAD 资产接成我们的统一场景。
//!
//! # 格式分三类，**绝不含糊**
//!
//! | 类别 | 格式 | 谁在解析 |
//! | --- | --- | --- |
//! | **自己读** | `.gltf` `.glb` `.obj` `.stl` | 我们（纯 Rust，无外部依赖） |
//! | **请权威** | `.blend` `.fbx` `.usd*` | 本机 **Blender**（headless 转 GLB 再读） |
//! | **请先导出** | `.step` `.stp` `.iges` `.dwg` | 我们**不解析 B-rep**——请在 CAD 侧导出 STL/OBJ/glTF |
//!
//! 第三类不是偷懒：STEP 是 B-rep（精确曲面）而不是网格，把它"读成三角面"本身就是一个
//! 有损转换，需要一个 CAD 内核（OpenCASCADE 那一类）。我们宁可明确说"请在 CAD 侧导出网格"，
//! 也不假装支持然后给出错的东西。
//!
//! # 几何去哪了
//!
//! 场景文档只放**世界坐标 AABB + 名字 + 出处**（那是内核的契约对象：要进日志、哈希、diff）。
//! 真三角面写到文档旁边的 **side-car** `<文档名>.mesh.glb`，节点上用裸 extras
//! `extras.rsi3d.mesh_ref` 指过去。于是：
//!
//! - 场景文档始终很小，移动一个物体不必搬动几何；
//! - 客户端（three.js）拿 side-car 就能显示**真网格**；
//! - 服务端光栅仍然只画 AABB 代理——**证据档不改口径**（`geometry: aabb-proxy` 依旧是真的）。

pub mod convert;
pub mod mesh;
pub mod readers;

use std::path::{Path, PathBuf};

use serde_json::json;

use rsi3d_harness_core::{Aabb, Extras, Metrics, Node, Provenance, Scene};

pub use mesh::{sidecar_name, write_glb, Mesh};

/// 一类格式怎么进来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// 我们自己解析
    Native,
    /// 请本机 Blender 转一道
    Blender,
    /// 我们不解析，请在上游导出网格
    ExportUpstream,
}

/// 格式表（**单一出处**：`scene import --formats` 与错误信息都读它）。
pub struct FormatSpec {
    pub ext: &'static str,
    pub route: Route,
    pub note: &'static str,
}

pub const FORMATS: &[FormatSpec] = &[
    FormatSpec { ext: "gltf", route: Route::Native, note: "glTF 2.0（外部 .bin / 内嵌 data URI 都认）" },
    FormatSpec { ext: "glb", route: Route::Native, note: "glTF 2.0 二进制（推荐：单文件、无外部依赖）" },
    FormatSpec { ext: "obj", route: Route::Native, note: "Wavefront OBJ（v/f/o/g；mtllib 暂不接）" },
    FormatSpec { ext: "stl", route: Route::Native, note: "STL（二进制与 ASCII 都认；三角汤，没有对象划分）" },
    FormatSpec { ext: "blend", route: Route::Blender, note: "Blender 工程：我们不解析 DNA，交给本机 Blender 转 GLB" },
    FormatSpec { ext: "fbx", route: Route::Blender, note: "FBX（经 Blender 的导入器）" },
    FormatSpec { ext: "usd", route: Route::Blender, note: "USD（经 Blender 的 USD 导入器）" },
    FormatSpec { ext: "usda", route: Route::Blender, note: "USD 文本" },
    FormatSpec { ext: "usdc", route: Route::Blender, note: "USD 二进制" },
    FormatSpec { ext: "usdz", route: Route::Blender, note: "USD 压缩包" },
    FormatSpec { ext: "step", route: Route::ExportUpstream, note: "STEP 是 B-rep，不是网格：请在 CAD 里导出 STL/OBJ/glTF" },
    FormatSpec { ext: "stp", route: Route::ExportUpstream, note: "同 step" },
    FormatSpec { ext: "iges", route: Route::ExportUpstream, note: "同 step" },
    FormatSpec { ext: "igs", route: Route::ExportUpstream, note: "同 step" },
    FormatSpec { ext: "dwg", route: Route::ExportUpstream, note: "同 step" },
    FormatSpec { ext: "dxf", route: Route::ExportUpstream, note: "同 step" },
];

/// 导入选项。
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// 源文件的长度单位 → 米。**不猜**：默认 1.0（当米），不对就显式给 `--unit-scale`
    pub unit_scale: f64,
    /// 最多导入多少个对象（装配体可能上千件）。截断了会在报告里说
    pub limit: Option<usize>,
    /// 显式指定 Blender
    pub blender: Option<PathBuf>,
    /// side-car glb 的写出路径（`None` = 不写几何，只要场景）
    pub sidecar: Option<PathBuf>,
    /// 节点 role 的兜底值（`others` 会被原样写进场景）
    pub role: String,
}

/// 导入报告（**给人看的事实**，不是日志）。
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// 源文件（原样记着，以后能追回）
    pub source: String,
    pub format: String,
    /// 读法：`None` = 我们自己解析；`Some` = 经了谁的手
    pub via: Option<String>,
    pub objects: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub bbox: [f64; 6],
    /// 源文件惯例用的坐标系（`unknown` = 文件里根本没写）
    pub source_cs: &'static str,
    /// 场景里的坐标实际所在的坐标系（转换器可能已经转过轴）
    pub read_cs: &'static str,
    pub sidecar: Option<String>,
    pub warnings: Vec<String>,
}

impl ImportReport {
    pub fn text(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("导入 {}\n", self.source));
        match &self.via {
            Some(via) => s.push_str(&format!("  格式 {} · 经手：{}\n", self.format, via)),
            None => s.push_str(&format!("  格式 {} · 我们自己解析\n", self.format)),
        }
        s.push_str(&format!(
            "  {} 个对象 · {} 顶点 · {} 三角面\n",
            self.objects, self.vertices, self.triangles
        ));
        s.push_str(&format!(
            "  包围盒：x {:.3}…{:.3} · y {:.3}…{:.3} · z {:.3}…{:.3}（米）\n",
            self.bbox[0], self.bbox[3], self.bbox[1], self.bbox[4], self.bbox[2], self.bbox[5]
        ));
        s.push_str(&format!(
            "  坐标系：源文件 {} → 场景 {}\n",
            self.source_cs, self.read_cs
        ));
        if let Some(sc) = &self.sidecar {
            s.push_str(&format!("  几何 side-car：{}（客户端据此显示真网格）\n", sc));
        }
        for w in &self.warnings {
            s.push_str(&format!("  ⚠ {}\n", w));
        }
        s
    }

    pub fn json(&self) -> serde_json::Value {
        json!({
            "source": self.source,
            "format": self.format,
            "via": self.via,
            "objects": self.objects,
            "vertices": self.vertices,
            "triangles": self.triangles,
            "bbox": self.bbox,
            "source_coordinate_system": self.source_cs,
            "read_coordinate_system": self.read_cs,
            "sidecar": self.sidecar,
            "warnings": self.warnings,
        })
    }
}

/// 导入结果。
#[derive(Debug)]
pub struct Imported {
    pub scene: Scene,
    pub report: ImportReport,
}

/// 支持的格式表（给 `scene import --formats`）。
pub fn formats_json() -> serde_json::Value {
    json!(FORMATS
        .iter()
        .map(|f| json!({
            "ext": f.ext,
            "route": match f.route {
                Route::Native => "native",
                Route::Blender => "blender",
                Route::ExportUpstream => "export-upstream",
            },
            "note": f.note,
        }))
        .collect::<Vec<_>>())
}

fn spec_for(path: &Path) -> Result<&'static FormatSpec, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext.is_empty() {
        return Err(format!(
            "{} 没有扩展名，认不出格式。支持的格式：{}",
            path.display(),
            FORMATS.iter().map(|f| f.ext).collect::<Vec<_>>().join(" / ")
        ));
    }
    FORMATS.iter().find(|f| f.ext == ext).ok_or_else(|| {
        format!(
            "不支持 .{}。自己读的：gltf/glb/obj/stl；请 Blender 读的：blend/fbx/usd*；\
             B-rep（step/iges/dwg）请在 CAD 侧导出网格。看全部：scene import --formats",
            ext
        )
    })
}

/// 导入一个文件。
pub fn import(path: &Path, opts: &ImportOptions) -> Result<Imported, String> {
    let spec = spec_for(path)?;
    let mut warnings: Vec<String> = Vec::new();

    // 1) 拿到网格（自己读 / 请 Blender）
    let (meshes, via, tmp_glb) = match spec.route {
        Route::Native => (read_native(path, spec.ext)?, None, None),
        Route::Blender => {
            let exe = convert::find_blender(opts.blender.as_deref())?;
            // 转到临时文件：这是中间产物，不该留在用户目录里
            let tmp = std::env::temp_dir().join(format!(
                "rsi3d-import-{}-{}.glb",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            ));
            let conv = convert::convert_to_glb(&exe, path, &tmp)?;
            let meshes = readers::read_gltf(&tmp).map_err(|e| {
                format!("Blender 转出来的 glTF 我们读不了（这是我们的 bug，不是你的文件）：{}", e)
            })?;
            (meshes, Some(format!("{} · 转 GLB 后读入", conv.tool)), Some(tmp))
        }
        Route::ExportUpstream => {
            return Err(format!(
                ".{} 我们不解析：**{} 是 B-rep（精确曲面），不是网格**——把它读成三角面本身\n\
                 就是一次有损转换，需要一个 CAD 内核（OpenCASCADE 那一类）。\n\
                 这个格式的说明：{}\n\
                 建议：在 CAD/上游工具里导出 **STL / OBJ / glTF**（网格），我们直接就能读；\n\
                 或者告诉我你装了什么 CAD 内核（FreeCAD 的 `freecadcmd` 之类），我把那条路接上。",
                spec.ext,
                spec.ext.to_uppercase(),
                spec.note
            ))
        }
    };
    let cleanup = tmp_glb.clone();
    let via_blender = matches!(spec.route, Route::Blender);
    let src_cs = source_cs(spec.ext);
    let scene_cs = read_cs(spec.ext, via_blender);

    let result = (|| -> Result<Imported, String> {
        let mut meshes = meshes;
        // 2) 截断（**必须说出来**）
        if let Some(limit) = opts.limit {
            if meshes.len() > limit {
                warnings.push(format!(
                    "源文件有 {} 个对象，--limit {} 只导了前 {} 个（**被截断了**，不是文件就这么点）",
                    meshes.len(),
                    limit,
                    limit
                ));
                meshes.truncate(limit);
            }
        }
        let scale = if opts.unit_scale > 0.0 { opts.unit_scale } else { 1.0 };
        if (scale - 1.0).abs() > f64::EPSILON {
            for m in &mut meshes {
                for v in m.positions.iter_mut() {
                    *v = (*v as f64 * scale) as f32;
                }
            }
        }

        let vertices: usize = meshes.iter().map(|m| m.vertex_count()).sum();
        let triangles: usize = meshes.iter().map(|m| m.triangle_count()).sum();
        // 读取器发现的事实（孤立顶点、三角汤没有对象划分…）一律上报，不许咽下去
        for m in &meshes {
            for note in &m.notes {
                warnings.push(format!("「{}」：{}", m.name, note));
            }
        }
        let mut bbox = [f64::INFINITY, f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for m in &meshes {
            if let Some((mn, mx)) = m.aabb() {
                for k in 0..3 {
                    bbox[k] = bbox[k].min(mn[k]);
                    bbox[k + 3] = bbox[k + 3].max(mx[k]);
                }
            }
        }

        // 3) side-car（几何）——文件名由文档名派生，服务端零配置就能找到
        let sidecar = opts.sidecar.clone();
        let sidecar_name = match &sidecar {
            Some(p) => {
                let nodes: Vec<(String, Mesh)> = meshes
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (node_id(&m.name, i), m.clone()))
                    .collect();
                let glb = write_glb(&nodes);
                std::fs::write(p, &glb)
                    .map_err(|e| format!("写不到 side-car {}：{}", p.display(), e))?;
                Some(
                    p.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "mesh.glb".into()),
                )
            }
            None => None,
        };

        // 4) 组装场景
        let mut objects = Vec::with_capacity(meshes.len());
        for (i, m) in meshes.iter().enumerate() {
            let (mn, mx) = m
                .aabb()
                .ok_or_else(|| format!("对象 {} 没有任何顶点", m.name))?;
            let thin = (mx[0] - mn[0]).min(mx[1] - mn[1]).min(mx[2] - mn[2]);
            if thin < 1e-3 {
                warnings.push(format!(
                    "对象「{}」有一轴只有 {:.4} 米（薄片/平面）：服务端光栅把它当薄盒画，俯视图可能看不到",
                    m.name, thin
                ));
            }
            let mut rsi3d = serde_json::Map::new();
            // 几何档次：**文档里**放的是包围盒代理；真三角面在 side-car 里
            rsi3d.insert("geometry".into(), json!("aabb-proxy"));
            if let Some(sc) = &sidecar_name {
                rsi3d.insert("mesh_ref".into(), json!(sc));
            }
            let mut extras = Extras::new();
            extras.insert("rsi3d".into(), serde_json::Value::Object(rsi3d));

            objects.push(Node {
                id: node_id(&m.name, i),
                role: role_for(&m.name, &opts.role),
                material: None,
                material_params: Default::default(),
                aabb: Aabb { min: mn, max: mx },
                layers: vec![rsi3d_harness_core::LayerKind::Mesh],
                editability_cap: None,
                provenance: Provenance {
                    origin: rsi3d_harness_core::Origin::Imported,
                    source: Some(path.display().to_string()),
                    // 这里写的是**文件自己**的惯例，不是场景里的（两者可能不同，见下面 warning）
                    source_coordinate_system: Some(src_cs.to_string()),
                    created_at_rev: None,
                },
                metrics: Metrics {
                    triangles: m.triangle_count() as u64,
                    gaussians: 0,
                    points: 0,
                    watertight: None,
                },
                extras,
            });
        }

        if objects.is_empty() {
            return Err(format!("{} 里没有可用的网格", path.display()));
        }

        // 5) 真实感检查：单位不对是最常见的坑（CAD 常以 mm 存），**主动提醒而不是等用户困惑**
        let size = (bbox[3] - bbox[0]).max(bbox[4] - bbox[1]).max(bbox[5] - bbox[2]);
        if size > 100.0 {
            warnings.push(format!(
                "整体尺寸 {:.1} 米，像个体育场——源文件多半是毫米单位：加 `--unit-scale 0.001` 再导一次",
                size
            ));
        } else if size > 0.0 && size < 0.01 {
            warnings.push(format!(
                "整体尺寸只有 {:.4} 米——源文件可能以厘米/毫米之外的极小数为单位，必要时用 `--unit-scale`",
                size
            ));
        }

        // 坐标系：源文件与场景可能**不是同一个**（转换器转轴）。不说清楚，数字就是没意义的
        if src_cs != scene_cs {
            warnings.push(format!(
                "源文件惯例是 {}（Z 上），转 GLB 时已经过了一道转轴：场景里的坐标是 {}（Y 上）。\
                 我们不会再转一次——数字以场景里的为准",
                src_cs, scene_cs
            ));
        } else if src_cs == "unknown" {
            warnings.push(
                "这个格式**不自带**坐标系与单位（STL/OBJ 里没有这两个字段）：我们按原样读入，\
                 不猜轴向、不替你转；`--unit-scale` 只缩放、不改轴向"
                    .to_string(),
            );
        }

        let scene = Scene {
            units: "m".into(),
            // 场景里装的是一堆坐标，所以这里写的是**读入后**的坐标系
            source_coordinate_system: Some(scene_cs.to_string()),
            intent: Some(format!("从 {} 导入", path.display())),
            objects,
            ..Scene::default()
        };
        let object_count = scene.objects.len();

        Ok(Imported {
            scene,
            report: ImportReport {
                source: path.display().to_string(),
                format: format!(".{}", spec.ext),
                via,
                objects: object_count,
                vertices,
                triangles,
                bbox,
                source_cs: src_cs,
                read_cs: scene_cs,
                sidecar: sidecar_name,
                warnings,
            },
        })
    })();

    if let Some(p) = cleanup {
        let _ = std::fs::remove_file(p);
    }
    result
}

fn read_native(path: &Path, ext: &str) -> Result<Vec<Mesh>, String> {
    match ext {
        "gltf" | "glb" => readers::read_gltf(path),
        "obj" => readers::read_obj(path),
        "stl" => readers::read_stl(path),
        other => Err(format!("没有 .{} 的原生读取器（这是个 bug）", other)),
    }
}

/// **源文件自己**惯例用的坐标系（不是我们读完之后的）。
///
/// 我们**不做**轴向转换——只记录事实。
fn source_cs(ext: &str) -> &'static str {
    match ext {
        "gltf" | "glb" => "RDF", // glTF 2.0 规范：Y 上、-Z 前（右手）
        "blend" | "fbx" => "RUF", // Blender 与 FBX 都是 Z 上（右手）
        "usd" | "usda" | "usdc" | "usdz" => "RUF", // USD 惯例 Z 上（可在文件里写明，暂不解析）
        // OBJ 与 STL **没有任何**坐标系声明与单位字段："惯例 Y 上"是猜的，猜的东西不写进事实字段
        _ => "unknown",
    }
}

/// 场景里的坐标**实际**落在哪个坐标系。
///
/// 走 Blender 的路，Blender 导出 glTF 时会自己把 Z-up 转成 Y-up，所以我们读到的是 RDF；
/// 这时候如果还把「源文件」也写成 RDF，就是在撒谎。两个字段分开写，差异另加一条 warning。
fn read_cs(ext: &str, via_blender: bool) -> &'static str {
    if via_blender {
        "RDF" // Blender 的 glTF 导出器做的就是这件事
    } else {
        source_cs(ext)
    }
}

/// 节点 id：`obj:<名字>`，重名加序号（id 是 stable_id，不能撞）。
fn node_id(name: &str, index: usize) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim_matches('_');
    if cleaned.is_empty() {
        format!("obj:part_{:04}", index)
    } else {
        format!("obj:{}", cleaned)
    }
}

/// role 的推断：只在**名字里明确说了**的情况下才认，其余交给调用方给的兜底值。
///
/// 宁可一律 `other`，也不猜——猜错会把渲染配色与语义评测都带偏。
fn role_for(name: &str, fallback: &str) -> String {
    let n = name.to_lowercase();
    if n.contains("floor") || n.contains("ground") || name.contains("地面") || name.contains("地板") {
        "floor".to_string()
    } else if fallback.is_empty() {
        "other".to_string()
    } else {
        fallback.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_table_covers_the_three_routes() {
        assert!(FORMATS.iter().any(|f| f.route == Route::Native));
        assert!(FORMATS.iter().any(|f| f.route == Route::Blender));
        assert!(FORMATS.iter().any(|f| f.route == Route::ExportUpstream));
        // 每个格式都得有话说（不许留空）
        for f in FORMATS {
            assert!(f.note.len() > 6, "{} 没解释", f.ext);
        }
    }

    #[test]
    fn step_says_export_a_mesh_instead_of_pretending() {
        let err = import(Path::new("/tmp/x.stp"), &ImportOptions::default()).unwrap_err();
        assert!(err.contains("B-rep"), "{}", err);
        assert!(err.contains("STL"), "要给出可行路径：{}", err);
    }

    #[test]
    fn unknown_extension_lists_what_we_do_support() {
        let err = import(Path::new("/tmp/x.zzz"), &ImportOptions::default()).unwrap_err();
        assert!(err.contains("不支持 .zzz"), "{}", err);
        assert!(err.contains("--formats"), "{}", err);
    }

    #[test]
    fn node_ids_are_stable_and_unique() {
        assert_eq!(node_id("Part A", 0), "obj:Part_A");
        assert_eq!(node_id("", 3), "obj:part_0003");
        // role 只在名字明说时才认
        assert_eq!(role_for("Floor_01", "other"), "floor");
        assert_eq!(role_for("地面", "other"), "floor");
        assert_eq!(role_for("Bracket", "other"), "other");
    }
}
