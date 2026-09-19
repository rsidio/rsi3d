//! rsi3d-harness 渲染层：把场景变成**可判读的静态观测图**。
//!
//! # 它要交付的三件事
//!
//! 1. **看得见**：多视角静态快照 → PNG。Agent 与人都能直接看。
//! 2. **判得准**：`id pass`（每个像素属于哪个对象）→ 精确拾取、按对象统计像素、
//!    以及**量出「窗前挡光带被遮挡了多少」**这种可进评测的数字。
//! 3. **对得上账**：同场景同参数 ⇒ 同像素哈希。图像因此可以当**证据**
//!    （"回滚后画面逐像素一致"是 H0 的验收项，靠它来断）。
//!
//! # 边界
//!
//! 渲染**只是视图**。`core` 不许 import 本 crate（反向依赖是单向的：`render → core`），
//! 所有评测维度都必须在没有渲染的情况下也能算——否则"引擎可离线"就假了。
//!
//! # 后端与降级
//!
//! 当前只有 `Software`（CPU 光栅）：确定性、无宿主依赖、`cargo test` 就能跑。
//! 浏览器里的 GPU 后端（wgpu：WebGPU → WebGL2 → 仅网格）会作为**性能档**加入，
//! 但观测契约不变；换了后端必须显式声明 [`Rendered::degraded`]，不许悄悄换画质。

pub mod camera;
pub mod geometry;
pub mod math;
pub mod png;
pub mod raster;
pub mod shading;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use rsi3d_harness_core::{Document, Scene};

pub use camera::{Camera, ViewKind};
pub use raster::Rgb;

use geometry::{band_quad, floor_quad, ids, room_edges, scene_solids, window_quad};
use raster::{Framebuffer, LightModel};

/// 渲染后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// CPU 软件光栅：确定性最高，无宿主依赖。当前的默认档。
    Software,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Software => "software",
        }
    }
}

/// 渲染参数。
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub width: u32,
    pub height: u32,
    pub views: Vec<ViewKind>,
    /// 背景色（浅色，便于判读；深色会让暗色家具糊成一团）
    pub background: Rgb,
    /// 画房间线框（给视角定坐标系；关掉会让人分不清哪面是墙）
    pub draw_room: bool,
    /// 画窗
    pub draw_window: bool,
    /// 画窗前挡光带
    pub draw_band: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            width: 480,
            height: 360,
            views: ViewKind::all().to_vec(),
            background: [248, 250, 252],
            draw_room: true,
            draw_window: true,
            draw_band: true,
        }
    }
}

impl RenderOptions {
    /// 只要一个视角（给 MCP 用：单张图省上下文）。
    pub fn single(view: ViewKind) -> Self {
        RenderOptions {
            views: vec![view],
            ..Default::default()
        }
    }
}

/// 一张视角图。
pub struct ViewImage {
    pub view: ViewKind,
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    /// 每个像素归属的 id（0 = 背景；1..=N = `scene.objects[i-1]`）
    pub id: Vec<u32>,
    /// 编码后的 PNG
    pub png: Vec<u8>,
    /// 原始像素的 sha256——逐像素可比的锚点（比 PNG 字节更稳：编码器换了它也不变）
    pub image_hash: String,
}

impl ViewImage {
    /// 某个 id 占了多少像素。
    pub fn pixels_of(&self, id: u32) -> usize {
        self.id.iter().filter(|v| **v == id).count()
    }

    /// 某个 id 占画面的比例。
    pub fn coverage(&self, id: u32) -> f64 {
        self.pixels_of(id) as f64 / self.id.len().max(1) as f64
    }

    /// 对象**可见**像素数（按节点下标，0 起）。
    pub fn object_pixels(&self, index: usize) -> usize {
        self.pixels_of(index as u32 + 1)
    }

    /// 图像的质心（像素坐标）——用来判断"这东西在图上的哪一边"。
    pub fn centroid_of(&self, id: u32) -> Option<(f64, f64)> {
        let mut n = 0usize;
        let (mut sx, mut sy) = (0.0, 0.0);
        for (i, v) in self.id.iter().enumerate() {
            if *v == id {
                n += 1;
                sx += (i % self.width as usize) as f64;
                sy += (i / self.width as usize) as f64;
            }
        }
        if n == 0 {
            None
        } else {
            Some((sx / n as f64, sy / n as f64))
        }
    }

    /// 场景里每个对象的可见像素数（按 id 升序 = 场景顺序）。
    pub fn object_visibility(&self, object_count: usize) -> Vec<usize> {
        (0..object_count).map(|i| self.object_pixels(i)).collect()
    }
}

/// 一次渲染的完整结果。
pub struct Rendered {
    pub backend: Backend,
    /// 非空表示降级了，字符串说明**为什么**（观测可信度会受影响，必须讲清楚）
    pub degraded: Option<String>,
    pub views: Vec<ViewImage>,
    /// 窗前挡光带被实体遮挡的比例（0..1）。只有俯视图能算——平视/斜视会被透视压扁。
    pub band_occlusion: Option<f64>,
    /// 参与计算的实体数
    pub object_count: usize,
}

impl Rendered {
    /// 结构化结果（CLI `--json` 与 MCP `structuredContent` 共用同一份）。
    pub fn to_json(&self) -> Value {
        json!({
            "backend": self.backend.as_str(),
            "degraded": self.degraded,
            "object_count": self.object_count,
            "band_occlusion": self.band_occlusion,
            "views": self.views.iter().map(|v| json!({
                "view": v.view.as_str(),
                "width": v.width,
                "height": v.height,
                "image_hash": v.image_hash,
                "bytes": v.png.len(),
                "visible_pixels": v.object_visibility(self.object_count),
            })).collect::<Vec<_>>(),
        })
    }

    /// 文本摘要（给模型读的"现场报告"）。
    pub fn to_text(&self, scene: &Scene) -> String {
        let mut lines = vec![format!(
            "渲染后端 {}（{}×{}，{} 个视角）",
            self.backend.as_str(),
            self.views.first().map(|v| v.width).unwrap_or(0),
            self.views.first().map(|v| v.height).unwrap_or(0),
            self.views.len()
        )];
        if let Some(d) = &self.degraded {
            lines.push(format!("⚠ 降级：{}", d));
        }
        lines.push("\n视角与哈希".into());
        for v in &self.views {
            lines.push(format!(
                "  {:<7} {}×{}  {}  {} KB",
                v.view.as_str(),
                v.width,
                v.height,
                &v.image_hash[..12],
                v.png.len() / 1024
            ));
        }

        // 可见性：0 像素 = 被完全挡住（这本身就是一条有价值的观测）
        lines.push("\n可见像素（0 = 被完全遮住）".into());
        for (i, n) in self
            .views
            .first()
            .map(|v| v.object_visibility(scene.objects.len()))
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            let node = &scene.objects[i];
            let mark = if n == 0 { "  ← 完全被遮挡" } else { "" };
            lines.push(format!("  {:<18} {}{}", node.id, n, mark));
        }

        if let Some(r) = self.band_occlusion {
            lines.push(format!(
                "\n窗前挡光带被遮挡 {:.0}%（俯视图量得；0% = 窗前完全通光）",
                r * 100.0
            ));
        }
        lines.join("\n")
    }
}

/// 渲染场景。
///
/// 这是本 crate 唯一的入口——参数只有场景与选项，没有隐藏状态，
/// 所以「同输入同输出」是可以被测试直接断言的。
pub fn render(scene: &Scene, opts: &RenderOptions) -> Rendered {
    let light = LightModel::from_scene(scene);
    let solids = scene_solids(scene);
    let aspect = opts.width.max(1) as f64 / opts.height.max(1) as f64;

    let mut views = Vec::with_capacity(opts.views.len());
    let mut band_occlusion = None;

    for kind in &opts.views {
        let cam = Camera::for_scene(scene, *kind, aspect);
        let mut fb = Framebuffer::new(opts.width, opts.height, opts.background);

        // 1) 地面（最先画，其它东西都压在它上面）
        if opts.draw_room {
            if let Some(room) = &scene.room {
                for t in floor_quad(room.size) {
                    fb.draw_triangle(&cam, &t, &light);
                }
            }
        }
        // 2) 挡光带标记（贴在地面上，y 略高一点）
        if opts.draw_band {
            if let Some(w) = &scene.window {
                for t in band_quad(w) {
                    fb.draw_triangle(&cam, &t, &light);
                }
            }
        }
        // 3) 实体
        for t in &solids {
            fb.draw_triangle(&cam, t, &light);
        }
        // 4) 窗（发光面；画在实体之后靠 z-buffer 决定可见性）
        if opts.draw_window {
            if let Some(w) = &scene.window {
                for t in window_quad(w) {
                    fb.draw_triangle(&cam, &t, &light);
                }
            }
        }
        // 5) 房间线框压在最上层，给视角定坐标系
        if opts.draw_room {
            if let Some(room) = &scene.room {
                for (a, b) in room_edges(room.size) {
                    fb.draw_line(&cam, a, b, [150, 158, 166]);
                }
            }
        }

        // 俯视图另算一次"挡光带原本覆盖了哪些像素"（不看遮挡），用来量遮挡比例
        if *kind == ViewKind::Top {
            if let (Some(w), true) = (&scene.window, opts.draw_band) {
                let mut mask = vec![false; fb.len()];
                for t in band_quad(w) {
                    fb.fill_mask(&cam, &t, &mut mask);
                }
                let total = mask.iter().filter(|m| **m).count();
                if total > 0 {
                    // 被"真实对象"盖住的像素 / 挡光带总面积
                    let covered = mask
                        .iter()
                        .enumerate()
                        .filter(|(i, m)| **m && ids::is_object(fb.id[*i]))
                        .count();
                    band_occlusion = Some(covered as f64 / total as f64);
                }
            }
        }

        let png = png::encode_rgb(opts.width, opts.height, &fb.rgb);
        let image_hash = rsi3d_harness_core::sha256_hex(&fb.rgb);
        views.push(ViewImage {
            view: *kind,
            width: opts.width,
            height: opts.height,
            rgb: fb.rgb,
            id: fb.id,
            png,
            image_hash,
        });
    }

    Rendered {
        backend: Backend::Software,
        degraded: None,
        views,
        band_occlusion,
        object_count: scene.objects.len(),
    }
}

/// 从文档渲染（顺手把"当前版本"记进摘要，免得图和人对不上）。
pub fn render_document(doc: &Document, opts: &RenderOptions) -> Rendered {
    render(doc.scene(), opts)
}

/// 把 PNG 编成 `data:` URI（MCP 的 image content 与 HTML 报告都能直接用）。
pub fn data_uri(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", png::base64(png))
}

/// 元素列表（给需要"逐个对象画/统计"的调用方）。
pub fn triangle_count(scene: &Scene) -> usize {
    scene_solids(scene).len()
}

/// 便利：把一组三角形按 id 汇总像素数。
pub fn id_histogram(view: &ViewImage) -> Vec<(u32, usize)> {
    let mut map = std::collections::BTreeMap::new();
    for id in &view.id {
        *map.entry(*id).or_insert(0usize) += 1;
    }
    map.into_iter().collect()
}
