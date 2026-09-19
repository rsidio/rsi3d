//! 插件式脚手架：模板发现 → 变量渲染 → 生成。
//!
//! 设计要点（详见 `docs/scaffold.md`）：
//!
//! - **模板即插件**：内置模板编译进二进制（离线可用）；外部模板放在
//!   `~/.rsi3d-harness/scaffolds/<id>/`，**同名覆盖内置**；`scaffold export` 可把内置模板
//!   导出成可编辑的外部模板（自举）。
//! - **只有 `files/` 会被生成**：模板目录里的其它内容（说明、脚本、未来的元数据）不会被复制，
//!   避免「模板自身的清单被拷进产物」。
//! - **绝不执行模板里的脚本**：生成后只打印下一步提示，不跑 post 命令（供应链红线）。
//! - **变量必须显式**：出现未声明的 `{{var}}` 直接报错，不静默留白（静默留白最难查）。
//! - **先规划再落盘**：`plan()` 一次性做完变量解析与冲突检测，`render()` 只负责写。

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod builtin;

/// 模板清单文件名。
pub const MANIFEST_NAME: &str = "scaffold.json";
/// 模板里唯一会被生成的子目录。
pub const FILES_DIR: &str = "files";
/// 外部模板默认目录（相对 HOME）。
pub const DEFAULT_EXTERNAL_DIR: &str = ".rsi3d-harness/scaffolds";

// ---------------------------------------------------------------- 数据模型

/// 模板来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 编译进二进制的内置模板
    Builtin,
    /// 用户目录下的外部模板插件
    External,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Builtin => "builtin",
            Source::External => "external",
        }
    }
}

/// 模板变量声明。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Var {
    pub key: String,
    /// 给用户看的提示
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub default: String,
    /// 从另一个变量派生（例如 slug 跟随 project）；优先于 default。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
}

/// `scaffold.json`：模板即插件的清单。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub desc: String,
    /// 分类：agent | plugin | pack | scaffold | other
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub vars: Vec<Var>,
    /// 生成后给用户的提示——**只打印，不执行**
    #[serde(default)]
    pub next: Vec<String>,
}

fn default_kind() -> String {
    "other".to_string()
}

/// 模板里的一个待生成文件。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TemplateFile {
    /// 相对输出根目录的路径，可含 `{{var}}`
    pub path: String,
    pub content: String,
    /// 生成后是否加可执行位
    #[serde(default)]
    pub exec: bool,
}

/// 一个模板。
#[derive(Debug, Clone)]
pub struct Template {
    pub manifest: Manifest,
    pub source: Source,
    /// 外部模板来源目录（内置为 None）
    pub dir: Option<PathBuf>,
    pub files: Vec<TemplateFile>,
}

/// 一次生成的计划（`--dry-run` 也会产出它）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Plan {
    pub template: String,
    pub source: String,
    pub out: String,
    pub vars: HashMap<String, String>,
    pub files: Vec<String>,
    pub next: Vec<String>,
    pub dry_run: bool,
}

impl Template {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// 输出根目录的默认名：取 `project` 变量（未声明则要求显式 `--out`）。
    pub fn default_out_name(&self) -> Option<String> {
        self.manifest
            .vars
            .iter()
            .find(|v| v.key == "project")
            .map(|v| v.default.clone())
            .filter(|d| !d.is_empty())
    }
}

// ---------------------------------------------------------------- 发现

/// 内置模板（编译期内嵌）。
pub fn builtin_templates() -> Vec<Template> {
    builtin::all()
}

/// 用户外部模板目录：`$RSI3D_HARNESS_SCAFFOLDS` > `~/.rsi3d-harness/scaffolds`。
pub fn default_external_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RSI3D_HARNESS_SCAFFOLDS") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(DEFAULT_EXTERNAL_DIR))
}

/// 发现顺序：内置 → 用户目录 → `extra`（后者同名覆盖前者）。
pub fn discover(extra: Option<&Path>) -> Vec<Template> {
    let mut list: Vec<Template> = Vec::new();
    for t in builtin_templates() {
        upsert(&mut list, t);
    }
    if let Some(dir) = default_external_dir() {
        if let Ok(ts) = load_external_dir(&dir) {
            for t in ts {
                upsert(&mut list, t);
            }
        }
    }
    if let Some(dir) = extra {
        match load_external_dir(dir) {
            Ok(ts) => {
                for t in ts {
                    upsert(&mut list, t);
                }
            }
            Err(e) => eprintln!("! 外部模板目录 {} 读取失败：{}", dir.display(), e),
        }
    }
    list.sort_by(|a, b| a.id().cmp(b.id()));
    list
}

fn upsert(list: &mut Vec<Template>, t: Template) {
    list.retain(|x| x.id() != t.id());
    list.push(t);
}

/// 按 id 找模板。
pub fn find(extra: Option<&Path>, id: &str) -> Option<Template> {
    discover(extra).into_iter().find(|t| t.id() == id)
}

/// 加载一个目录下的全部外部模板（每个子目录一个）。
pub fn load_external_dir(dir: &Path) -> Result<Vec<Template>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("读取 {}", dir.display()))? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        match load_external(&path) {
            Ok(t) => out.push(t),
            Err(e) => eprintln!("! 跳过模板 {}：{:#}", path.display(), e),
        }
    }
    Ok(out)
}

/// 加载单个外部模板目录（需含 `scaffold.json` 与 `files/`）。
pub fn load_external(dir: &Path) -> Result<Template> {
    let manifest_path = dir.join(MANIFEST_NAME);
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("缺少 {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .with_context(|| format!("{} 不是合法 JSON", manifest_path.display()))?;
    if manifest.id.trim().is_empty() {
        bail!("{} 的 id 为空", manifest_path.display());
    }

    let files_root = dir.join(FILES_DIR);
    let mut files = Vec::new();
    if files_root.is_dir() {
        collect(&files_root, &files_root, &mut files)?;
    }
    Ok(Template {
        manifest,
        source: Source::External,
        dir: Some(dir.to_path_buf()),
        files,
    })
}

/// 递归收集待生成文件（路径一律用 `/`，跨平台一致）。
fn collect(root: &Path, dir: &Path, out: &mut Vec<TemplateFile>) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("读取 {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(root, &path, out)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let content = fs::read_to_string(&path)
            .with_context(|| format!("读取 {}（模板文件必须是 UTF-8 文本）", path.display()))?;
        let exec = is_executable(&path);
        out.push(TemplateFile {
            path: rel,
            content,
            exec,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

// ---------------------------------------------------------------- 变量

/// 解析 `--var k=v`。
pub fn parse_kv(pairs: &[String]) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for p in pairs {
        match p.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => {
                out.insert(k.trim().to_string(), v.to_string());
            }
            _ => bail!("--var 需要 K=V 形式，收到「{}」", p),
        }
    }
    Ok(out)
}

/// 汇总变量：内置变量 + 模板声明（默认值）+ 用户提供；缺必填则报错。
pub fn resolve_vars(t: &Template, provided: &HashMap<String, String>) -> Result<HashMap<String, String>> {
    let mut vars = HashMap::new();
    vars.insert("template".to_string(), t.id().to_string());
    vars.insert("date".to_string(), today());
    vars.insert("engine_version".to_string(), env!("CARGO_PKG_VERSION").to_string());

    let mut missing: Vec<String> = Vec::new();
    for v in &t.manifest.vars {
        if let Some(got) = provided.get(&v.key) {
            vars.insert(v.key.clone(), got.clone());
        } else if !v.from.is_empty() {
            // 派生：声明顺序决定可派生的来源（例如 slug 跟随 project）
            match vars.get(&v.from) {
                Some(src) => {
                    vars.insert(v.key.clone(), src.clone());
                }
                None => bail!(
                    "模板 {} 的变量 {} 想从 {} 派生，但后者未解析（from 只能指向声明在前面的变量）",
                    t.id(),
                    v.key,
                    v.from
                ),
            }
        } else if !v.default.is_empty() {
            vars.insert(v.key.clone(), v.default.clone());
        } else {
            missing.push(format!("{}（{}）", v.key, v.prompt));
        }
    }
    if !missing.is_empty() {
        bail!(
            "模板 {} 缺少变量：{}；用 --var key=value 提供",
            t.id(),
            missing.join(", ")
        );
    }
    // 未声明的 --var 直接忽略（允许多模板共用一套命令行参数），但要说一声
    for k in provided.keys() {
        if !vars.contains_key(k) {
            eprintln!("! 模板 {} 未声明变量 {}，已忽略", t.id(), k);
        }
    }
    Ok(vars)
}

/// `{{var}}` 替换；未声明的变量报错。
///
/// 转义：`\{{` 会原样输出 `{{`（写模板说明时用得上）。
pub fn substitute(text: &str, vars: &HashMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(start) = rest.find("{{") else {
            out.push_str(rest);
            break;
        };
        // 转义：前一个字符是反斜杠则不替换
        if start > 0 && rest.as_bytes()[start - 1] == b'\\' {
            out.push_str(&rest[..start - 1]);
            out.push_str("{{");
            rest = &rest[start + 2..];
            continue;
        }
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .context("模板里有未闭合的 {{（缺少 }}）")?;
        let key = after[..end].trim();
        let val = vars
            .get(key)
            .with_context(|| format!("模板里用到未声明的变量 {{{{ {} }}}}", key))?;
        out.push_str(val);
        rest = &after[end + 2..];
    }
    Ok(out)
}

// ---------------------------------------------------------------- 生成

/// 计算生成计划并做冲突检测（不落盘）。
pub fn plan(
    t: &Template,
    provided: &HashMap<String, String>,
    out: &Path,
    force: bool,
) -> Result<Plan> {
    let vars = resolve_vars(t, provided)?;
    let mut files = Vec::new();
    let mut conflicts = Vec::new();

    for f in &t.files {
        let rel = substitute(&f.path, &vars)
            .with_context(|| format!("模板文件路径含未声明变量：{}", f.path))?;
        if rel.starts_with('/') || rel.split('/').any(|seg| seg == "..") {
            bail!("模板文件路径越界：{}", rel);
        }
        let target = out.join(&rel);
        if target.exists() && !force {
            conflicts.push(target.display().to_string());
        }
        files.push(rel);
    }

    if !conflicts.is_empty() {
        bail!(
            "目标已存在 {} 个文件（用 --force 覆盖）：\n  {}",
            conflicts.len(),
            conflicts.join("\n  ")
        );
    }

    // 生成后的提示也要渲染（里面会写「cd {{project}}」这类话）
    let next = t
        .manifest
        .next
        .iter()
        .map(|n| substitute(n, &vars))
        .collect::<Result<Vec<_>>>()?;

    Ok(Plan {
        template: t.id().to_string(),
        source: t.source.label().to_string(),
        out: out.display().to_string(),
        vars,
        files,
        next,
        dry_run: false,
    })
}

/// 生成到 `out`。`dry_run` 时只返回计划。
pub fn render(
    t: &Template,
    provided: &HashMap<String, String>,
    out: &Path,
    force: bool,
    dry_run: bool,
) -> Result<Plan> {
    let mut plan = plan(t, provided, out, force)?;
    plan.dry_run = dry_run;
    if dry_run {
        return Ok(plan);
    }

    for (f, rel) in t.files.iter().zip(plan.files.iter()) {
        let content = substitute(&f.content, &plan.vars)
            .with_context(|| format!("渲染 {} 失败", rel))?;
        let target = out.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录 {}", parent.display()))?;
        }
        fs::write(&target, content).with_context(|| format!("写入 {}", target.display()))?;
        if f.exec {
            make_executable(&target)?;
        }
    }
    Ok(plan)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(path)?.permissions();
    perm.set_mode(0o755);
    fs::set_permissions(path, perm)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// 把模板（内置或外部）导出成可编辑的外部模板目录。
pub fn export(t: &Template, dest: &Path, force: bool) -> Result<Vec<String>> {
    let manifest_path = dest.join(MANIFEST_NAME);
    if dest.exists() && !force {
        bail!("{} 已存在（用 --force 覆盖）", dest.display());
    }
    fs::create_dir_all(dest.join(FILES_DIR))?;

    let manifest = serde_json::to_string_pretty(&t.manifest)?;
    fs::write(&manifest_path, format!("{}\n", manifest))?;
    let mut written = vec![MANIFEST_NAME.to_string()];

    for f in &t.files {
        let target = dest.join(FILES_DIR).join(&f.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, &f.content)?;
        if f.exec {
            make_executable(&target)?;
        }
        written.push(format!("{}/{}", FILES_DIR, f.path));
    }
    Ok(written)
}

/// 今天（UTC，`YYYY-MM-DD`），不引 chrono。
pub fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    civil_date(secs.div_euclid(86_400))
}

/// 自 1970-01-01 起的天数 → `YYYY-MM-DD`（Howard Hinnant 算法）。
fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("hr-scaffold-{}-{}-{}", std::process::id(), tag, n));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn substitute_replaces_and_rejects_unknown() {
        let v = vars(&[("project", "my-app")]);
        assert_eq!(substitute("hi {{project}}!", &v).unwrap(), "hi my-app!");
        assert_eq!(substitute("{{ project }}", &v).unwrap(), "my-app");
        assert_eq!(substitute("no vars", &v).unwrap(), "no vars");
        assert!(substitute("{{nope}}", &v).is_err());
        assert!(substitute("{{ open", &v).is_err());
        // 转义后原样输出，且不影响后续占位符
        assert_eq!(
            substitute("写法是 \\{{project}}，实际是 {{project}}", &v).unwrap(),
            "写法是 {{project}}，实际是 my-app"
        );
    }

    #[test]
    fn all_builtin_templates_are_wellformed() {
        let all = builtin_templates();
        assert!(all.len() >= 4, "内置模板应有 4 个以上");
        let mut ids: Vec<&str> = all.iter().map(|t| t.id()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "内置模板 id 必须唯一");
        for t in &all {
            assert!(!t.manifest.title.is_empty(), "{} 缺 title", t.id());
            assert!(!t.manifest.desc.is_empty(), "{} 缺 desc", t.id());
            assert!(!t.files.is_empty(), "{} 没有任何产出文件", t.id());
            // 每个模板的变量都要能解析出来（必填项自动补一个测试值）
            let mut provided = vars(&[("project", "demo")]);
            for v in &t.manifest.vars {
                if v.default.is_empty() && !provided.contains_key(&v.key) {
                    provided.insert(v.key.clone(), format!("x-{}", v.key));
                }
            }
            resolve_vars(t, &provided).unwrap_or_else(|e| panic!("{} 变量解析失败: {}", t.id(), e));

            // 每个文件的内容与路径都必须能成功渲染（能抓出未转义的占位符）
            let plan = plan(t, &provided, Path::new("/tmp/hr-scaffold-rendercheck"), false)
                .unwrap_or_else(|e| panic!("{} 渲染失败: {}", t.id(), e));
            for f in &t.files {
                let rendered = substitute(&f.content, &plan.vars)
                    .unwrap_or_else(|e| panic!("{} 的文件 {} 渲染失败: {}", t.id(), f.path, e));
                // 渲染后的 JSON 必须仍是合法 JSON。
                // 这条防线专抓一类真出过的 bug：模板里写成 `\\\\{{var}}`（两个反斜杠），
                // 渲染后变成 `\{{var}}` —— 在 JSON 里是非法转义。
                if f.path.ends_with(".json") {
                    serde_json::from_str::<serde_json::Value>(&rendered).unwrap_or_else(|e| {
                        panic!(
                            "{} 的文件 {} 渲染后不是合法 JSON（检查 \\{{{{ 的转义写法）: {}",
                            t.id(),
                            f.path,
                            e
                        )
                    });
                }
            }
        }
    }

    #[test]
    fn render_writes_files_and_refuses_conflicts() {
        let dir = tmpdir("render");
        let t = find(None, "agent-app").expect("内置 agent-app 模板应存在");
        let out = dir.join("app");

        let plan = render(&t, &vars(&[("project", "demo-agent")]), &out, false, false).unwrap();
        assert_eq!(plan.template, "agent-app");
        assert!(plan.files.iter().any(|f| f == "agent.mjs"));
        for rel in &plan.files {
            assert!(out.join(rel).is_file(), "{} 未生成", rel);
        }
        let agent = fs::read_to_string(out.join("agent.mjs")).unwrap();
        assert!(agent.contains("demo-agent"), "变量未渲染进文件内容");

        // 二次生成必须拒绝
        let err = render(&t, &vars(&[("project", "demo-agent")]), &out, false, false).unwrap_err();
        assert!(format!("{:#}", err).contains("--force"));

        // --force 可覆盖
        render(&t, &vars(&[("project", "demo-agent")]), &out, true, false).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tmpdir("dry");
        let t = find(None, "pack").expect("内置 pack 模板应存在");
        let out = dir.join("out");
        let plan = render(&t, &vars(&[("project", "demo-pack")]), &out, false, true).unwrap();
        assert!(plan.dry_run);
        assert!(!out.exists(), "dry-run 不应创建目录");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_required_var_is_an_error() {
        let t = find(None, "agent-app").unwrap();
        // 先确认该模板的 project 变量无默认值（否则这个测试没意义）
        let project = t.manifest.vars.iter().find(|v| v.key == "project").unwrap();
        assert!(project.default.is_empty(), "本测试假设 project 无默认值");
        let err = plan(&t, &HashMap::new(), Path::new("/tmp/x"), false).unwrap_err();
        assert!(format!("{:#}", err).contains("project"));
        let _ = t;
    }

    #[test]
    fn export_then_reload_roundtrip() {
        let dir = tmpdir("export");
        let dest = dir.join("my-pack");
        let t = find(None, "pack").unwrap();
        let written = export(&t, &dest, false).unwrap();
        assert!(written.iter().any(|p| p == "scaffold.json"));

        let reloaded = load_external(&dest).unwrap();
        assert_eq!(reloaded.id(), t.id());
        assert_eq!(reloaded.files.len(), t.files.len());
        assert_eq!(reloaded.source, Source::External);

        // 导出物必须能作为外部模板被发现，且覆盖同名内置
        let found = discover(Some(&dir));
        let hit = found.iter().find(|x| x.id() == "pack").unwrap();
        assert_eq!(hit.source, Source::External);

        // 导出物仍带占位符（可编辑），渲染时才替换
        let readme = reloaded
            .files
            .iter()
            .find(|f| f.path == "README.md")
            .expect("pack 模板应有 README.md");
        assert!(readme.content.contains("{{project}}"), "导出的模板应保留占位符");

        // 用导出的模板生成，内容应被渲染
        let out = dir.join("out");
        render(&reloaded, &vars(&[("project", "demo-pack")]), &out, false, false).unwrap();
        let gen = fs::read_to_string(out.join("README.md")).unwrap();
        assert!(gen.contains("demo-pack"));
        assert!(!gen.contains("{{project}}"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_load_rejects_bad_manifest() {
        let dir = tmpdir("bad");
        fs::write(dir.join(MANIFEST_NAME), "{ not json").unwrap();
        assert!(load_external(&dir).is_err());

        let dir2 = tmpdir("nomanifest");
        assert!(load_external(&dir2).is_err());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dir2);
    }

    #[test]
    fn parse_kv_validates() {
        let got = parse_kv(&["a=1".to_string(), "b=x=y".to_string()]).unwrap();
        assert_eq!(got.get("a").unwrap(), "1");
        assert_eq!(got.get("b").unwrap(), "x=y");
        assert!(parse_kv(&["noequals".to_string()]).is_err());
    }

    #[test]
    fn var_can_be_derived_from_another() {
        let t = find(None, "pack").expect("内置 pack 模板应存在");
        // pack 的 slug 从 project 派生 → 只给 project 也能解析
        let got = resolve_vars(&t, &vars(&[("project", "my-pack")])).unwrap();
        assert_eq!(got.get("slug").map(String::as_str), Some("my-pack"));
        // 显式提供时以显式为准
        let got = resolve_vars(
            &t,
            &vars(&[("project", "my-pack"), ("slug", "explicit-slug")]),
        )
        .unwrap();
        assert_eq!(got.get("slug").map(String::as_str), Some("explicit-slug"));
    }

    #[test]
    fn date_is_iso_like() {
        let d = today();
        assert_eq!(d.len(), 10, "日期应为 YYYY-MM-DD：{}", d);
        assert!(d.starts_with("20"), "{}", d);
    }
}
