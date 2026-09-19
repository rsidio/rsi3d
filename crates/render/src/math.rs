//! 最小线性代数：只为「把盒子画到屏幕上」服务。
//!
//! # 为什么不引 glam
//!
//! 一是没必要（这里只有 Vec3/Mat4 与几个构造），二是**确定性**：
//! 渲染结果要能被当证据（`scene verify` 会比对图像哈希），所以我宁可把
//! 用到的运算都写在自己眼前，也不要引一个可能换实现的依赖。
//!
//! 三角函数是唯一的例外：`sin/cos` 由平台 libm 提供，跨平台可能有末位差异，
//! 所以**像素级逐字节一致只在同平台同构建下承诺**（见 `docs/render.md` §确定性）。

use std::ops::{Add, Mul, Sub};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Vec3 { x, y, z }
    }

    pub const fn splat(v: f64) -> Self {
        Vec3::new(v, v, v)
    }

    pub fn from_arr(a: [f64; 3]) -> Self {
        Vec3::new(a[0], a[1], a[2])
    }

    pub fn dot(self, o: Vec3) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    /// 归一化；零向量返回零向量（调用方自己判退化，不要在这里 panic）。
    pub fn normalized(self) -> Vec3 {
        let l = self.length();
        if l <= f64::EPSILON {
            Vec3::splat(0.0)
        } else {
            self * (1.0 / l)
        }
    }

    pub fn min(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }

    pub fn max(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Mul<f64> for Vec3 {
    type Output = Vec3;
    fn mul(self, s: f64) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}

/// 4×4 矩阵（行主序）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4 {
    pub m: [[f64; 4]; 4],
}

impl Mat4 {
    pub const fn identity() -> Self {
        Mat4 {
            m: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn mul(&self, o: &Mat4) -> Mat4 {
        let mut r = [[0.0f64; 4]; 4];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = (0..4).map(|k| self.m[i][k] * o.m[k][j]).sum();
            }
        }
        Mat4 { m: r }
    }

    /// 变换到**裁剪空间**（保留 w）。
    ///
    /// 光栅器必须看到 w：w ≤ 0 表示顶点在相机背后，这类三角形要整块丢掉
    /// （我们把相机放在包围盒之外，所以丢掉它们不会丢正确的东西）。
    pub fn transform_clip(&self, p: Vec3) -> [f64; 4] {
        let v = [p.x, p.y, p.z, 1.0];
        let mut out = [0.0f64; 4];
        for (i, o) in out.iter_mut().enumerate() {
            *o = (0..4).map(|k| self.m[i][k] * v[k]).sum();
        }
        out
    }

    /// 变换点（w 分量参与透视除法）。
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        let out = self.transform_clip(p);
        // 齐次除法（w 可能是 1 或透视深度）
        if out[3].abs() > f64::EPSILON && (out[3] - 1.0).abs() > f64::EPSILON {
            Vec3::new(out[0] / out[3], out[1] / out[3], out[2] / out[3])
        } else {
            Vec3::new(out[0], out[1], out[2])
        }
    }

    /// 视图矩阵（右手系，与 glTF 约定一致：+Y 上、+Z 朝观察者）。
    ///
    /// 屏幕右侧 = `cross(f, up)` 归一化；屏幕上方 = `cross(right, f)`。
    /// 选这个顺序是为了让俯视图满足「+X 向右、+Z 向下」（也就是北在上）。
    pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
        let f = (target - eye).normalized();
        let r = f.cross(up).normalized();
        let u = r.cross(f);
        Mat4 {
            m: [
                [r.x, r.y, r.z, -r.dot(eye)],
                [u.x, u.y, u.z, -u.dot(eye)],
                [-f.x, -f.y, -f.z, f.dot(eye)],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// 正交投影 → NDC（[-1,1]^3）。
    pub fn orthographic(left: f64, right: f64, bottom: f64, top: f64, near: f64, far: f64) -> Mat4 {
        Mat4 {
            m: [
                [2.0 / (right - left), 0.0, 0.0, -(right + left) / (right - left)],
                [0.0, 2.0 / (top - bottom), 0.0, -(top + bottom) / (top - bottom)],
                [0.0, 0.0, -2.0 / (far - near), -(far + near) / (far - near)],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// 透视投影 → NDC。`fov_y_deg` 为竖直视场角。
    pub fn perspective(fov_y_deg: f64, aspect: f64, near: f64, far: f64) -> Mat4 {
        let f = 1.0 / (fov_y_deg.to_radians() / 2.0).tan();
        Mat4 {
            m: [
                [f / aspect, 0.0, 0.0, 0.0],
                [0.0, f, 0.0, 0.0],
                [0.0, 0.0, (far + near) / (near - far), 2.0 * far * near / (near - far)],
                [0.0, 0.0, -1.0, 0.0],
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_view_puts_positive_z_downward() {
        // 俯视：眼睛在 +Y 上方看向原点，up = -Z ⇒ 屏幕上方是 -Z（北），+Z 向下
        let v = Mat4::look_at(Vec3::new(0.0, 10.0, 0.0), Vec3::splat(0.0), Vec3::new(0.0, 0.0, -1.0));
        let north = v.transform_point(Vec3::new(0.0, 0.0, -2.0));
        let south = v.transform_point(Vec3::new(0.0, 0.0, 2.0));
        assert!(north.y > 0.0, "北（-Z）应当落在屏幕上方：{:?}", north);
        assert!(south.y < 0.0, "南（+Z）应当落在屏幕下方：{:?}", south);
        let east = v.transform_point(Vec3::new(2.0, 0.0, 0.0));
        assert!(east.x > 0.0, "东（+X）应当落在屏幕右方：{:?}", east);
    }

    #[test]
    fn orthographic_maps_bounds_to_ndc() {
        let p = Mat4::orthographic(-2.0, 2.0, -3.0, 3.0, 0.1, 100.0);
        let a = p.transform_point(Vec3::new(-2.0, 3.0, -0.1));
        assert!((a.x + 1.0).abs() < 1e-9 && (a.y - 1.0).abs() < 1e-9, "{:?}", a);
    }
}
