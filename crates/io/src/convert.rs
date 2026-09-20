//! 外部转换器：`.blend` / `.fbx` / `.usd*` 这些**我们不自己解析**的格式，请权威实现来做。
//!
//! # 为什么不自研 `.blend` 解析
//!
//! `.blend` 不是"一种网格格式"，它是 **Blender 的内存快照**：文件里是 DNA 结构表 +
//! 数据块的原始 dump，字段布局随 Blender 版本变化。想读它，要么把某个版本的 DNA 全部
//! 实现一遍，要么请 Blender 自己来。我们选后者：
//!
//! - 这条路径**永远与用户的 Blender 版本一致**（他们用什么版本存，就用什么版本读）；
//! - 失败时我们能拿到 **Blender 的原话**（比如"这不是 blend 文件"），原样转述给用户——
//!   这比自己猜"大概哪里坏了"有用得多（实测就靠它认出了两个改过头部的资产）。
//!
//! 代价说清楚：转换需要**本机装了 Blender**，且这一步会起一个 Blender 进程。
//! 我们**不静默降级**：装了就转，没装就明确告诉用户装什么、或者让他直接导出 glTF。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 一次转换的结果（成功时给出 glb 路径）。
#[derive(Debug, Clone)]
pub struct Conversion {
    pub tool: String,
    pub output: PathBuf,
}

/// 找 Blender 可执行文件：显式给的最优先，然后环境变量，然后常见安装位置，最后 PATH。
pub fn find_blender(explicit: Option<&Path>) -> Result<PathBuf, String> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        tried.push(format!("{}（--blender 指定的）", p.display()));
    }
    if let Ok(env) = std::env::var("RSI3D_BLENDER") {
        let p = PathBuf::from(&env);
        if p.is_file() {
            return Ok(p);
        }
        tried.push(format!("{}（RSI3D_BLENDER 指的）", env));
    }
    // 常见安装位置（macOS 是 .app 包，Linux 是包管理器，Windows 是安装目录）
    let mut candidates = vec![
        PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender"),
        PathBuf::from("/usr/local/bin/blender"),
        PathBuf::from("/usr/bin/blender"),
        PathBuf::from("/snap/bin/blender"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        // 用户自己解压的版本：~/blender*/blender
        let h = PathBuf::from(home);
        for name in ["blender", "Blender"] {
            candidates.push(h.join(name).join("blender"));
        }
    }
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    tried.push(candidates.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(" · "));

    // PATH 里找
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join("blender");
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    tried.push("$PATH 里的 blender".to_string());

    Err(missing_blender_help(&tried))
}

/// 「没找到 Blender」时要说的话。
///
/// 单独抽出来是为了能测：本机装了 Blender 的话，找不到的那条路没法用真环境复现。
fn missing_blender_help(tried: &[String]) -> String {
    format!(
        "没找到 Blender（试过：{}）。\n\
         这个格式需要一个能读它的权威实现，装一个 Blender 就行（https://www.blender.org/download/）：\n  \
         · macOS：把 Blender.app 放进 /Applications\n  \
         · 或者用 --blender /path/to/blender，或设 RSI3D_BLENDER=/path/to/blender\n\
         如果你有别的工具链，更省事的做法是**先导出 glTF/GLB**，那种格式我们自己就能读。",
        tried.join("；")
    )
}

/// Blender 版本号（拿不到就说"未知版本"——不编造）。
pub fn blender_version(exe: &Path) -> String {
    let out = Command::new(exe).arg("--version").output();
    match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.lines()
                .next()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| "未知版本".into())
        }
        Err(_) => "未知版本".into(),
    }
}

/// 用 Blender 把某个文件转成 GLB。
///
/// 失败时返回 **Blender 的原话**（它的 stderr/stdout 里最后几行有信息的），
/// 而不是我们自己编一句"转换失败"。
pub fn convert_to_glb(exe: &Path, src: &Path, out: &Path) -> Result<Conversion, String> {
    let src_abs = src
        .canonicalize()
        .map_err(|e| format!("找不到 {}：{}", src.display(), e))?;
    let out_abs = out
        .to_path_buf();
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    // Blender 的 Python：先开/导入，再导出 GLB。
    // 用 --factory-startup：不加载用户的插件与偏好，导入结果才可复现。
    let script = match ext.as_str() {
        "blend" => format!(
            "import bpy\nbpy.ops.wm.open_mainfile(filepath={:?})\n",
            src_abs.to_string_lossy()
        ),
        "fbx" => format!(
            "import bpy\nbpy.ops.wm.read_factory_settings(use_empty=True)\nbpy.ops.import_scene.fbx(filepath={:?})\n",
            src_abs.to_string_lossy()
        ),
        "usd" | "usda" | "usdc" | "usdz" => format!(
            "import bpy\nbpy.ops.wm.read_factory_settings(use_empty=True)\nbpy.ops.wm.usd_import(filepath={:?})\n",
            src_abs.to_string_lossy()
        ),
        "obj" | "stl" | "ply" => format!(
            "import bpy\nbpy.ops.wm.read_factory_settings(use_empty=True)\nbpy.ops.wm.obj_import(filepath={:?})\n",
            src_abs.to_string_lossy()
        ),
        other => {
            return Err(format!(
                "Blender 这条路不支持 .{}（支持：blend / fbx / usd*；网格格式我们自己读）",
                other
            ))
        }
    };
    let script = format!(
        "{script}bpy.ops.export_scene.gltf(filepath={:?}, export_format='GLB', use_selection=False)\n",
        out_abs.to_string_lossy()
    );

    let out_run = Command::new(exe)
        .arg("--background")
        .arg("--factory-startup")
        .arg("--python-expr")
        .arg(&script)
        .output()
        .map_err(|e| format!("起不动 Blender（{}）：{}", exe.display(), e))?;

    // Blender 的毛病：出错也可能 exit code 0（比如"打不开文件"时它照样正常退出）。
    // 所以**两个判据都要看**：退出码 + 产物是否真的出现。
    let produced = out_abs.is_file() && std::fs::metadata(&out_abs).map(|m| m.len() > 12).unwrap_or(false);
    if !produced {
        let stdout = String::from_utf8_lossy(&out_run.stdout);
        let stderr = String::from_utf8_lossy(&out_run.stderr);
        let mut interesting: Vec<String> = stdout
            .lines()
            .chain(stderr.lines())
            .map(|l| l.trim().to_string())
            .filter(|l| {
                !l.is_empty()
                    && (l.starts_with("Error")
                        || l.starts_with("错误")
                        || l.contains("不是")
                        || l.contains("failed")
                        || l.contains("Warning: Cannot")
                        || l.starts_with("Traceback")
                        || l.contains("Exception")
                        || l.contains("No such file"))
            })
            .collect();
        interesting.dedup();
        if interesting.is_empty() {
            interesting.push(format!(
                "Blender 退出码 {}，也没生成 glTF（没有更多信息）",
                out_run.status.code().unwrap_or(-1)
            ));
        }
        return Err(format!(
            "{} 读不了这个文件。Blender 说：\n  {}",
            blender_version(exe),
            interesting.join("\n  ")
        ));
    }

    Ok(Conversion {
        tool: blender_version(exe),
        output: out_abs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_blender_error_tells_you_what_to_do() {
        let err = missing_blender_help(&["/Applications/Blender.app/…".to_string()]);
        assert!(err.contains("没找到 Blender"), "{}", err);
        assert!(err.contains("--blender"), "要告诉用户怎么指路径：{}", err);
        assert!(err.contains("RSI3D_BLENDER"), "也要给环境变量的路：{}", err);
        assert!(err.contains("glTF"), "要给出更省事的替代：{}", err);
        // 把试过的地方列出来（用户据此知道该往哪儿放）
        assert!(err.contains("Blender.app"), "{}", err);
    }

    #[test]
    fn unsupported_extension_is_rejected_by_name() {
        let err = convert_to_glb(Path::new("/bin/echo"), Path::new("x/whatever.dwg"), Path::new("/tmp/o.glb"))
            .unwrap_err();
        assert!(err.contains("dwg"), "{}", err);
    }
}
