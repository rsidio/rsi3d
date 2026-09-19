//! 软件光栅器：z-buffer + id pass（+"区域掩码"用它算遮挡比例）。
//!
//! # 为什么是 CPU 软件光栅
//!
//! 1. **确定性**：GPU 的浮点行为、光栅化细则、驱动版本都会影响像素，而这里的图像要能当证据
//!    （`scene verify` 会比对图像哈希、"回滚后画面逐像素一致"是 H0 的验收项）。
//! 2. **无宿主依赖**：`cargo test` 就能跑，不需要 GPU / 浏览器 / 窗口系统。
//! 3. **够用**：H0 的几何是包围盒（真实几何要等 `io`），画的本来就是示意图而非照片。
//!
//! GPU 后端（wgpu）会在 H3 接真实网格/高斯时作为**性能档**加进来，
//! 但观测契约（多视角 + id pass + 指标）不变——这正是"渲染后端是内部实现细节"的意思。

use crate::camera::Camera;
use crate::math::Vec3;

/// 8 位 RGB。
pub type Rgb = [u8; 3];

/// 三角形（世界系 + 面法线 + 归属 id）。
#[derive(Debug, Clone, Copy)]
pub struct Triangle {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
    pub normal: Vec3,
    pub color: Rgb,
    /// 这个像素属于谁（0 = 背景；见 [`crate::ids`]）
    pub id: u32,
    /// 自发光（窗、挡光带这种"标记"用它，避免被光照压暗到看不见）
    pub emissive: bool,
}

/// 画布：颜色 + 深度 + id。
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    depth: Vec<f64>,
    pub id: Vec<u32>,
    background: Rgb,
}

impl Framebuffer {
    pub fn new(width: u32, height: u32, background: Rgb) -> Self {
        let n = (width as usize) * (height as usize);
        Framebuffer {
            width,
            height,
            rgb: background.repeat(n),
            depth: vec![f64::INFINITY; n],
            id: vec![0; n],
            background,
        }
    }

    pub fn clear(&mut self) {
        self.rgb = self.background.repeat((self.width * self.height) as usize);
        self.depth.fill(f64::INFINITY);
        self.id.fill(0);
    }

    pub fn len(&self) -> usize {
        (self.width * self.height) as usize
    }

    /// 画一个实心三角形（z-buffer 遮挡）。
    pub fn draw_triangle(&mut self, cam: &Camera, tri: &Triangle, light: &LightModel) {
        let (color, _) = light.shade(tri);
        self.rasterize(cam, tri, |fb, idx, depth| {
            if depth < fb.depth[idx] {
                fb.depth[idx] = depth;
                fb.id[idx] = tri.id;
                let o = idx * 3;
                fb.rgb[o] = color[0];
                fb.rgb[o + 1] = color[1];
                fb.rgb[o + 2] = color[2];
            }
        });
    }

    /// 把三角形投影到「区域掩码」上（**不看遮挡**）。
    ///
    /// 用途：算「窗前挡光带被挡掉多少比例」——必须知道挡光带**原本**覆盖了哪些像素，
    /// 哪怕那些像素现在被家具盖住了。
    pub fn fill_mask(&mut self, cam: &Camera, tri: &Triangle, mask: &mut [bool]) {
        self.rasterize(cam, tri, |_, idx, _| {
            mask[idx] = true;
        });
    }

    /// 画一条线（房间线框用）。带深度测试，线宽 1px。
    pub fn draw_line(&mut self, cam: &Camera, a: Vec3, b: Vec3, color: Rgb) {
        let (w, h) = (self.width, self.height);
        let (Some((pa, da)), Some((pb, db))) = (project(cam, a, w, h), project(cam, b, w, h)) else {
            return; // 有端点在相机背后就整条丢掉（相机在包围盒外，正常不会发生）
        };
        let (x0, y0) = (pa.0, pa.1);
        let (x1, y1) = (pb.0, pb.1);
        let steps = ((x1 - x0).abs().max((y1 - y0).abs())).ceil().max(1.0) as i32;
        for s in 0..=steps {
            let t = s as f64 / steps as f64;
            let x = x0 + (x1 - x0) * t;
            let y = y0 + (y1 - y0) * t;
            let z = da + (db - da) * t;
            let ix = x.round() as i64;
            let iy = y.round() as i64;
            if ix < 0 || iy < 0 || ix >= self.width as i64 || iy >= self.height as i64 {
                continue;
            }
            let idx = iy as usize * self.width as usize + ix as usize;
            // 线要**压在面之上**：留一点深度余量，否则会被自己所在的墙面吃掉
            if z - 1e-4 <= self.depth[idx] {
                self.depth[idx] = z - 1e-4;
                self.id[idx] = crate::ids::LINE;
                let o = idx * 3;
                self.rgb[o] = color[0];
                self.rgb[o + 1] = color[1];
                self.rgb[o + 2] = color[2];
            }
        }
    }

    /// 共享的投影 + 边函数填充（实心绘制与掩码绘制走同一条路径，避免两份光栅化逻辑漂移）。
    ///
    /// 取 `&mut self` 只是因为回调要写字段；`fill_mask` 只是写进外部掩码。
    fn rasterize(
        &mut self,
        cam: &Camera,
        tri: &Triangle,
        mut put: impl FnMut(&mut Self, usize, f64),
    ) {
        let (w, h) = (self.width, self.height);
        let (Some(v0), Some(v1), Some(v2)) = (
            project(cam, tri.a, w, h),
            project(cam, tri.b, w, h),
            project(cam, tri.c, w, h),
        ) else {
            return;
        };
        let (p0, z0) = v0;
        let (p1, z1) = v1;
        let (p2, z2) = v2;

        let area = edge(p0, p1, p2);
        if area.abs() < 1e-12 {
            return; // 退化三角形
        }
        // 统一成逆时针，避免正反两种绕向各写一份判定
        let (p1, p2, z1, z2, area) = if area < 0.0 {
            (p2, p1, z2, z1, -area)
        } else {
            (p1, p2, z1, z2, area)
        };

        let min_x = p0.0.min(p1.0).min(p2.0).floor().max(0.0) as i64;
        let max_x = p0.0.max(p1.0).max(p2.0).ceil().min(self.width as f64 - 1.0) as i64;
        let min_y = p0.1.min(p1.1).min(p2.1).floor().max(0.0) as i64;
        let max_y = p0.1.max(p1.1).max(p2.1).ceil().min(self.height as f64 - 1.0) as i64;
        if max_x < min_x || max_y < min_y {
            return;
        }

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let p = (x as f64 + 0.5, y as f64 + 0.5);
                let w0 = edge(p1, p2, p);
                let w1 = edge(p2, p0, p);
                let w2 = edge(p0, p1, p);
                // 顶左规则：落在边上的像素只归一个三角形，避免相邻面互相覆盖出接缝
                if !inside(w0, p1, p2, p) || !inside(w1, p2, p0, p) || !inside(w2, p0, p1, p) {
                    continue;
                }
                let z = (w0 * z0 + w1 * z1 + w2 * z2) / area;
                put(self, y as usize * self.width as usize + x as usize, z);
            }
        }
    }
}

/// 光照模型：一盏方向光 + 环境光。
///
/// 刻意保持极简——H0 的「光照」评测维度是**规则化**的（强度范围、挡窗），
/// 不是照片级真实感；渲染的职责是把这些事实**画得能判读**。
#[derive(Debug, Clone, Copy)]
pub struct LightModel {
    pub direction: Vec3,
    pub intensity: f64,
    pub ambient: f64,
}

impl Default for LightModel {
    fn default() -> Self {
        LightModel {
            direction: Vec3::new(-0.4, -0.8, 0.45).normalized(),
            intensity: 1.0,
            ambient: 0.35,
        }
    }
}

impl LightModel {
    /// 从场景的第一盏方向光推导（没有就用默认）。
    pub fn from_scene(scene: &rsi3d_harness_core::Scene) -> Self {
        let mut m = LightModel::default();
        if let Some(l) = scene
            .lights
            .iter()
            .find(|l| l.kind.eq_ignore_ascii_case("directional"))
        {
            if let Some(d) = l.direction {
                let v = Vec3::from_arr(d);
                if v.length() > 1e-9 {
                    m.direction = v.normalized();
                }
            }
            m.intensity = l.intensity.clamp(0.0, 5.0);
        }
        m
    }

    /// 面法线 → 亮度系数（Lambert + 环境）。
    pub fn lum(&self, normal: Vec3) -> f64 {
        let n = if normal.length() < 1e-9 {
            Vec3::new(0.0, 1.0, 0.0)
        } else {
            normal.normalized()
        };
        // 取绝对值：盒子是闭合的，法线朝哪边都该被照亮（两面可见）
        let d = n.dot(self.direction).abs();
        (self.ambient + (1.0 - self.ambient) * d * self.intensity).clamp(0.0, 1.6)
    }

    pub fn shade(&self, tri: &Triangle) -> (Rgb, f64) {
        let lum = if tri.emissive { 1.0 } else { self.lum(tri.normal) };
        (
            [
                (tri.color[0] as f64 * lum).clamp(0.0, 255.0) as u8,
                (tri.color[1] as f64 * lum).clamp(0.0, 255.0) as u8,
                (tri.color[2] as f64 * lum).clamp(0.0, 255.0) as u8,
            ],
            lum,
        )
    }
}

// ---------------------------------------------------------------- 内部工具

type P2 = (f64, f64);

fn edge(a: P2, b: P2, c: P2) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// 是否在边的内侧（含顶左规则）。
fn inside(w: f64, a: P2, b: P2, p: P2) -> bool {
    if w > 0.0 {
        return true;
    }
    if w < 0.0 {
        return false;
    }
    // 落在边上：只接受"顶边"或"左边"上的像素
    let is_top = (a.1 == b.1) && (p.1 > a.1);
    let is_left = (a.1 != b.1) && (b.1 > a.1);
    is_top || is_left
}

/// 世界系 → 屏幕像素 + NDC 深度。返回 `None` 表示顶点在相机背后。
fn project(cam: &Camera, p: Vec3, width: u32, height: u32) -> Option<(P2, f64)> {
    let clip = cam.view_proj.transform_clip(p);
    if clip[3] <= 1e-9 {
        return None;
    }
    let ndc = Vec3::new(clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]);
    // NDC → 像素（y 翻转：NDC +y 在屏幕上方）
    let x = (ndc.x + 1.0) * 0.5 * width as f64 - 0.5;
    let y = (1.0 - ndc.y) * 0.5 * height as f64 - 0.5;
    Some(((x, y), ndc.z))
}
