//! rsi3d-harness 命令行（壳②/native）。
//!
//! 今天有两件事：
//! 1. **插件式脚手架**——用模板生成用户自己的 3D 资产系统与 Agent；
//! 2. **场景内核入口**（`scene show/edit/verify`）——不用写代码就能跑「观察 → 编辑 → 回滚 → 重放」，
//!    用的命令信封与 Agent 之后发给 MCP/HTTP 的**完全一致**。
//!
//! 后续 `serve` / `connect` / `run` 也会挂到这个二进制上（见 `docs/mcp.md` §8）。

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use rsi3d_harness_scaffold as scaffold;
// `engine` 就是 `crates/core`（内核），`render` 就是 `crates/render`（渲染层）。
// 换个名字，免得与 Rust 内置的 `core` 混淆。
use rsi3d_harness_core as engine;
use rsi3d_harness_render as render;
use engine::{CommandRequest, Document};

#[derive(Parser)]
#[command(
    name = "rsi3d-harness",
    version,
    about = "rsi3d-harness —— 3D 资产的 Agentic 引擎命令行",
    after_help = "示例:\n  \
        rsi3d-harness scaffold list\n  \
        rsi3d-harness scaffold info agent-app\n  \
        rsi3d-harness scaffold new agent-app --var project=my-3d-agent\n  \
        rsi3d-harness scaffold export harness-plugin ./my-templates/harness-plugin\n  \
        rsi3d-harness scene show scene.json\n  \
        rsi3d-harness scene edit scene.json --cmd '{\"op\":\"transform\",\"target\":\"sofa_01\",\"params\":{\"translate\":[0,0,1.3]},\"reason\":\"把沙发挪出窗带\"}'\n  \
        rsi3d-harness scene verify doc.json\n  \
        rsi3d-harness mcp --root .（在 VS Code 里由 .vscode/mcp.json 启动）"
)]
struct Cli {
    /// 额外模板目录（同名覆盖内置模板）
    #[arg(long, global = true, value_name = "DIR")]
    scaffold_dir: Option<PathBuf>,

    /// 输出 JSON（便于脚本与 Agent 消费）
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 插件式脚手架：用模板生成你自己的 3D 资产系统 / Agent
    Scaffold {
        #[command(subcommand)]
        action: ScaffoldCmd,
    },
    /// 场景内核：观察 / 编辑 / 校验（命令信封与 Agent 发给 MCP 的一致）
    Scene {
        #[command(subcommand)]
        action: SceneCmd,
    },
    /// MCP 服务（stdio）：把引擎接进 VS Code / Cursor / Claude Code 等工具
    Mcp {
        /// 允许读写的工作目录（缺省用当前目录）；引擎不会碰这之外的任何文件
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SceneCmd {
    /// 观察：节点表、挡窗、告警、哈希（只读）
    Show {
        /// 场景文件，或已有文档（自动识别）
        file: PathBuf,
    },
    /// 对场景执行一条或多条命令；每步打印新告警与归因
    Edit {
        /// 场景文件，或已有文档（自动识别）
        file: PathBuf,
        /// 线上信封 JSON，可重复且按顺序执行
        #[arg(long = "cmd", value_name = "JSON", required = true)]
        cmds: Vec<String>,
        /// 落盘成文档（含日志，可 verify）
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// 校验文档：日志重放是否与落盘状态一致（可复现性自检）
    Verify {
        /// 文档文件
        file: PathBuf,
    },
    /// 日志与归因表：每一步谁改了什么、为什么改
    Log {
        /// 场景或文档文件
        file: PathBuf,
    },
    /// 渲染：多视角静态观测图（PNG）+ 可见性与挡窗测量
    Render {
        /// 场景或文档文件
        file: PathBuf,
        /// 输出前缀，产出 <前缀>-<视角>.png；不给就只看测量结果
        #[arg(long, value_name = "PREFIX")]
        out: Option<PathBuf>,
        /// 视角，逗号分隔：top, front, iso-sw, iso-se（缺省四个都要）
        #[arg(long, value_name = "LIST")]
        views: Option<String>,
        #[arg(long, default_value_t = 480)]
        width: u32,
        #[arg(long, default_value_t = 360)]
        height: u32,
    },
}

#[derive(Subcommand)]
enum ScaffoldCmd {
    /// 列出可用模板（内置 + 外部）
    List,
    /// 查看模板详情：变量、产出文件、下一步
    Info {
        /// 模板 id
        template: String,
    },
    /// 用模板生成项目
    New {
        /// 模板 id
        template: String,
        /// 输出目录；缺省用变量的 project 值
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// 变量覆盖，可重复：--var project=my-app
        #[arg(long = "var", value_name = "K=V")]
        vars: Vec<String>,
        /// 目标已存在时覆盖
        #[arg(long)]
        force: bool,
        /// 只看会生成什么，不落盘
        #[arg(long)]
        dry_run: bool,
    },
    /// 把模板（含内置）导出成可编辑的外部模板目录 —— 自举的起点
    Export {
        /// 模板 id
        template: String,
        /// 目标目录
        dest: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// 打印外部模板目录（把自定义模板放这儿就会被发现）
    Dir,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = dispatch(&cli) {
        if cli.json {
            println!(
                "{}",
                serde_json::json!({"error": {"code": "cli_error", "message": format!("{:#}", e)}})
            );
        } else {
            eprintln!("✗ {:#}", e);
        }
        std::process::exit(1);
    }
}

fn dispatch(cli: &Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Scaffold { action } => match action {
            ScaffoldCmd::List => cmd_list(cli),
            ScaffoldCmd::Info { template } => cmd_info(cli, template),
            ScaffoldCmd::New {
                template,
                out,
                vars,
                force,
                dry_run,
            } => cmd_new(cli, template, out.as_deref(), vars, *force, *dry_run),
            ScaffoldCmd::Export {
                template,
                dest,
                force,
            } => cmd_export(cli, template, dest, *force),
            ScaffoldCmd::Dir => cmd_dir(cli),
        },
        Cmd::Scene { action } => match action {
            SceneCmd::Show { file } => cmd_scene_show(cli, file),
            SceneCmd::Edit { file, cmds, out } => {
                cmd_scene_edit(cli, file, cmds, out.as_deref())
            }
            SceneCmd::Verify { file } => cmd_scene_verify(cli, file),
            SceneCmd::Log { file } => cmd_scene_log(cli, file),
            SceneCmd::Render {
                file,
                out,
                views,
                width,
                height,
            } => cmd_scene_render(cli, file, out.as_deref(), views.as_deref(), *width, *height),
        },
        Cmd::Mcp { root } => cmd_mcp(root.as_deref()),
    }
}

/// 启动 MCP stdio 服务。
///
/// 注意：这条路径**只往 stdout 写协议报文**，日志全部走 stderr——
/// 客户端会把 stdout 的每个字节都当协议解析。
fn cmd_mcp(root: Option<&Path>) -> Result<()> {
    let root = match root {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("取不到当前目录；请用 --root 指定工作目录")?,
    };
    let cfg = rsi3d_harness_mcp::Config::new(root);
    rsi3d_harness_mcp::serve_stdio(&cfg).context("MCP 服务异常退出")?;
    Ok(())
}

fn emit(cli: &Cli, human: String, value: serde_json::Value) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if !human.is_empty() {
        println!("{}", human);
    }
    Ok(())
}

fn cmd_list(cli: &Cli) -> Result<()> {
    let list = scaffold::discover(cli.scaffold_dir.as_deref());
    let mut lines = vec![format!(
        "共 {} 个模板（内置编译进二进制；外部模板同名即覆盖）",
        list.len()
    )];
    for t in &list {
        lines.push(format!(
            "{:<16} {:<9} {:<7} {}",
            t.id(),
            t.manifest.kind,
            t.source.label(),
            t.manifest.title
        ));
        lines.push(format!(
            "                 {}（{} 个文件）",
            t.manifest.desc,
            t.files.len()
        ));
    }
    if let Some(dir) = scaffold::default_external_dir() {
        lines.push(format!("\n外部模板目录：{}", dir.display()));
    }
    lines.push("下一步：rsi3d-harness scaffold new <模板> --var project=<名字>".into());

    let value = serde_json::json!({
        "templates": list.iter().map(|t| serde_json::json!({
            "id": t.id(),
            "kind": t.manifest.kind,
            "title": t.manifest.title,
            "desc": t.manifest.desc,
            "source": t.source.label(),
            "files": t.files.len(),
            "vars": t.manifest.vars,
        })).collect::<Vec<_>>(),
        "external_dir": scaffold::default_external_dir().map(|p| p.display().to_string()),
    });
    emit(cli, lines.join("\n"), value)
}

fn cmd_info(cli: &Cli, id: &str) -> Result<()> {
    let t = must_find(cli, id)?;
    let mut lines = vec![
        format!("{}  {}", t.id(), t.manifest.title),
        format!("  分类     {}", t.manifest.kind),
        format!("  来源     {}", t.source.label()),
        format!("  说明     {}", t.manifest.desc),
    ];
    if let Some(dir) = &t.dir {
        lines.push(format!("  目录     {}", dir.display()));
    }
    lines.push("  变量".into());
    if t.manifest.vars.is_empty() {
        lines.push("    （无）".into());
    }
    for v in &t.manifest.vars {
        let default = if v.default.is_empty() {
            "（必填）".to_string()
        } else {
            format!("默认 {}", v.default)
        };
        lines.push(format!("    {:<12} {:<8} {}", v.key, default, v.prompt));
    }
    lines.push("  产出文件".into());
    for f in &t.files {
        lines.push(format!(
            "    {}{}",
            f.path,
            if f.exec { "  (可执行)" } else { "" }
        ));
    }
    if !t.manifest.next.is_empty() {
        lines.push("  生成后（仅提示，不会自动执行）".into());
        for n in &t.manifest.next {
            lines.push(format!("    - {}", n));
        }
    }

    let value = serde_json::json!({
        "id": t.id(),
        "kind": t.manifest.kind,
        "title": t.manifest.title,
        "desc": t.manifest.desc,
        "source": t.source.label(),
        "dir": t.dir.as_ref().map(|p| p.display().to_string()),
        "vars": t.manifest.vars,
        "files": t.files.iter().map(|f| serde_json::json!({"path": f.path, "exec": f.exec})).collect::<Vec<_>>(),
        "next": t.manifest.next,
    });
    emit(cli, lines.join("\n"), value)
}

fn cmd_new(
    cli: &Cli,
    id: &str,
    out: Option<&Path>,
    raw_vars: &[String],
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let t = must_find(cli, id)?;
    let provided = scaffold::parse_kv(raw_vars)?;

    // 先解析变量，才能在缺省 --out 时用 project 命名目录
    let vars = scaffold::resolve_vars(&t, &provided)?;
    let out_dir: PathBuf = match out {
        Some(p) => p.to_path_buf(),
        None => match vars.get("project") {
            Some(p) => PathBuf::from(p),
            None => bail!("未指定 --out，且模板 {} 没有 project 变量可作目录名", t.id()),
        },
    };

    let plan = scaffold::render(&t, &provided, &out_dir, force, dry_run)?;

    let mut lines = Vec::new();
    if dry_run {
        lines.push(format!(
            "→ 预览（未落盘）：{} → {}",
            plan.template, plan.out
        ));
    } else {
        lines.push(format!(
            "✓ 已生成 {} → {}（{} 个文件）",
            plan.template,
            plan.out,
            plan.files.len()
        ));
    }
    for f in &plan.files {
        lines.push(format!("  {}", f));
    }
    if !plan.next.is_empty() {
        lines.push("下一步".into());
        for n in &plan.next {
            lines.push(format!("  - {}", n));
        }
    }
    lines.push("提示：模板不会自动执行任何脚本，上面命令由你确认后再跑。".into());
    emit(cli, lines.join("\n"), serde_json::to_value(&plan)?)
}

fn cmd_export(cli: &Cli, id: &str, dest: &Path, force: bool) -> Result<()> {
    let t = must_find(cli, id)?;
    let written = scaffold::export(&t, dest, force)?;
    let human = format!(
        "✓ 已把模板 {} 导出到 {}（{} 个文件）\n  \
         说明：导出**保留原 id**，所以放进外部模板目录后是「覆盖内置」而不是新建。\n  \
         想要一个新模板？改 {} 里的 id 即可（例如改成 my-{}）。\n  \
         当前 id 为内置 id 时，本目录会**隐藏**同名内置模板（scaffold list 会标 external）。",
        t.id(),
        dest.display(),
        written.len(),
        dest.join("scaffold.json").display(),
        t.id()
    );
    let mut human = human;
    human.push_str(&format!(
        "\n  外部模板目录：{}",
        scaffold::default_external_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(未设置 HOME)".into())
    ));
    emit(
        cli,
        human,
        serde_json::json!({"template": t.id(), "dest": dest.display().to_string(), "files": written}),
    )
}

fn cmd_dir(cli: &Cli) -> Result<()> {
    let dir = scaffold::default_external_dir();
    match &dir {
        Some(d) => {
            let exists = d.is_dir();
            let human = format!(
                "{}\n  {}\n  放法：在该目录下建一个子目录，内含 scaffold.json 与 files/，即成为一个外部模板插件。\n  覆盖内置：子目录用内置模板 id 即覆盖（scaffold export 是快捷方式）。",
                d.display(),
                if exists { "已存在" } else { "尚未创建（首次 scaffold export 时会自动建）" }
            );
            emit(
                cli,
                human,
                serde_json::json!({"external_dir": d.display().to_string(), "exists": exists}),
            )
        }
        None => {
            let human = "未能确定外部模板目录（HOME 未设置）；可用 --scaffold-dir 显式指定".to_string();
            emit(cli, human, serde_json::json!({"external_dir": null}))
        }
    }
}

fn must_find(cli: &Cli, id: &str) -> Result<scaffold::Template> {
    if let Some(t) = scaffold::find(cli.scaffold_dir.as_deref(), id) {
        return Ok(t);
    }
    let available: Vec<String> = scaffold::discover(cli.scaffold_dir.as_deref())
        .iter()
        .map(|t| t.id().to_string())
        .collect();
    bail!(
        "没有模板「{}」；可用：{}（用 scaffold list 看详情）",
        id,
        available.join(", ")
    )
}

// ---------------------------------------------------------------- 场景内核

/// 读入场景或文档（自动识别：有 `spec: rsi3d-document/v1` 就当文档读）。
///
/// 支持文档很重要——否则 `scene edit` 在循环里用不起来（每次都得从 rev 0 重来）。
fn load_document(path: &Path) -> Result<Document> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("读不到 {}", path.display()))?;
    let peek: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} 不是合法 JSON", path.display()))?;
    let is_doc = peek.get("spec").and_then(|s| s.as_str()) == Some(engine::DOCUMENT_SPEC);
    if is_doc {
        Document::from_json(&raw).map_err(Into::into)
    } else {
        Document::from_scene_json(&raw).map_err(Into::into)
    }
}

fn cmd_scene_show(cli: &Cli, file: &Path) -> Result<()> {
    let doc = load_document(file)?;
    let mut human = engine::report::scene_summary(&doc);
    human.push_str(
        "\n下一步：rsi3d-harness scene edit <file> --cmd '{\"op\":\"transform\",\
         \"target\":\"sofa_01\",\"params\":{\"translate\":[0,0,1.3]}}'",
    );
    let mut value = engine::report::observe_json(&doc)?;
    value["file"] = serde_json::json!(file.display().to_string());
    emit(cli, human, value)
}

fn cmd_scene_log(cli: &Cli, file: &Path) -> Result<()> {
    let doc = load_document(file)?;
    let human = engine::report::history_text(&doc);
    let value = serde_json::json!({
        "revision": doc.revision(),
        "cursor": doc.cursor(),
        "head": doc.head(),
        "log_hash": doc.log_hash()?,
        "oplog": doc.oplog(),
        "attribution": doc.attribution(),
    });
    emit(cli, human, value)
}

/// 解析 `--views top,iso` 之类的列表。
fn parse_views(raw: Option<&str>) -> Result<Vec<render::ViewKind>> {
    let Some(raw) = raw else {
        return Ok(render::ViewKind::all().to_vec());
    };
    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(render::ViewKind::parse(part).ok_or_else(|| {
            anyhow::anyhow!(
                "不认识的视角「{}」；可用：top, front, iso-sw, iso-se",
                part
            )
        })?);
    }
    if out.is_empty() {
        bail!("--views 是空的");
    }
    Ok(out)
}

fn cmd_scene_render(
    cli: &Cli,
    file: &Path,
    out: Option<&Path>,
    views: Option<&str>,
    width: u32,
    height: u32,
) -> Result<()> {
    let doc = load_document(file)?;
    let opts = render::RenderOptions {
        width,
        height,
        views: parse_views(views)?,
        ..Default::default()
    };
    let rendered = render::render_document(&doc, &opts);

    let mut human = rendered.to_text(doc.scene());
    if let Some(prefix) = out {
        human.push_str("\n文件\n");
        for v in &rendered.views {
            let path = with_suffix(prefix, v.view.as_str());
            render::png::write_file(&path, &v.png)
                .with_context(|| format!("写不到 {}", path.display()))?;
            human.push_str(&format!("  {}\n", path.display()));
        }
    } else {
        human.push_str("\n提示：加 --out <前缀> 会把图落盘（<前缀>-top.png 等）。\n");
    }

    let mut value = rendered.to_json();
    value["revision"] = serde_json::json!(doc.revision());
    value["source"] = serde_json::json!(file.display().to_string());
    emit(cli, human, value)
}

/// `out` + 视角名 → `out-top.png`（若前缀已带扩展名就插在扩展名前）。
fn with_suffix(prefix: &Path, suffix: &str) -> PathBuf {
    let stem = prefix
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "render".into());
    let name = format!("{}-{}.png", stem, suffix);
    match prefix.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}

fn cmd_scene_edit(
    cli: &Cli,
    file: &Path,
    raw_cmds: &[String],
    out: Option<&Path>,
) -> Result<()> {
    let mut doc = load_document(file)?;

    let mut lines = Vec::new();
    let mut steps = Vec::new();
    for raw in raw_cmds {
        let req: CommandRequest = serde_json::from_str(raw)
            .with_context(|| format!("命令信封不是合法 JSON：{}", raw))?;
        let before_rev = doc.revision();
        let applied = doc
            .apply_request(&req)
            .map_err(|e| anyhow::anyhow!("rev {} 上的命令被拒绝（{}）：{}", before_rev, e.code(), e))?;

        lines.push(engine::report::applied_text(&applied, &req.reason));
        steps.push(engine::report::applied_json(&applied));
    }

    lines.push(format!("\n{}", engine::report::status_line(&doc)));

    if let Some(p) = out {
        std::fs::write(p, doc.to_json()?)
            .with_context(|| format!("写不到 {}", p.display()))?;
        lines.push(format!(
            "✓ 已保存 {}（日志 {} 条，可用 scene verify 校验）",
            p.display(),
            doc.oplog().len()
        ));
    } else {
        lines.push("提示：加 --out <file> 保存成文档，就能接着改、并校验重放一致。".into());
    }

    let mut value = engine::report::observe_json(&doc)?;
    value["log_hash"] = serde_json::json!(doc.log_hash()?);
    value["steps"] = serde_json::json!(steps);
    value["out"] = serde_json::json!(out.map(|p| p.display().to_string()));
    emit(cli, lines.join("\n"), value)
}

fn cmd_scene_verify(cli: &Cli, file: &Path) -> Result<()> {
    // 加载时已经强制校验「重放 == 落盘状态」，这里再把其余可复现性证据摆出来。
    let doc = load_document(file)?;
    let report = engine::report::verify_report(&doc, &file.display().to_string())?;
    emit(cli, report.text.clone(), report.json.clone())?;
    if !report.ok {
        bail!("文档未能通过可复现性校验");
    }
    Ok(())
}
