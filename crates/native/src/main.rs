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
use rsi3d_harness_stream as stream;
use rsi3d_harness_contract as contract;
use rsi3d_harness_io as io;
use engine::{CommandRequest, Document};

/// `serve` / `stream` 两个子命令（远程渲染 / 转流）。
mod serve_cmd;

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
        rsi3d-harness scene export scene.json --out model.gltf（交给 Blender / Fyrox / three.js）\n  \
  rsi3d-harness scene import robot.blend --out robot.scene.json（接进来：自己读 glTF/OBJ/STL，blend/fbx/usd 交给本机 Blender）\n  \
        rsi3d-harness serve scene.json --port 8283 --open（浏览器里两条流并排看）\n  \
        rsi3d-harness stream http://127.0.0.1:8283 --token <T> --kind frame --out frames/\n  \
        rsi3d-harness contract --out contract（从 Rust 类型派生出给外部作者的键清单与 schema）\n  \
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
    /// 渲染模式：看规则表，或拿一份主机参数**判**这台机器该用什么档
    RenderMode {
        /// 主机参数（浏览器探测出来的那份 JSON；不给就只打印规则表）
        #[arg(long, value_name = "FILE")]
        profile: Option<PathBuf>,
        /// 用指定的规则表（缺省用内置；平台发的那份可以从 `/api/render/policy` 存下来）
        #[arg(long, value_name = "FILE")]
        policy: Option<PathBuf>,
    },
    /// 契约：从 Rust 类型**派生**给非 Rust 消费者用的键清单 / JSON Schema（单一出处）
    Contract {
        /// 输出目录；不给就只打印（配合 `--json` 给工具消费）
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// 只比对不落盘：产物与来源不一致就报错（CI / 冒烟用）
        #[arg(long)]
        check: bool,
    },
    /// 远程渲染 / 转流：把场景推成 three.js 场景流 + 图像流（HTTP + SSE）
    Serve {
        /// 场景或文档文件
        file: PathBuf,
        /// 绑定地址；缺省只绑回环（绑到 0.0.0.0 会告警）
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// 端口；0 = 让系统随机挑一个
        #[arg(long, default_value_t = 8283)]
        port: u16,
        /// 图像流的帧率上限（0 = 不限）
        #[arg(long, default_value_t = 4)]
        fps: u32,
        /// 指定访问令牌；缺省随机生成并打印
        #[arg(long)]
        token: Option<String>,
        /// 启动后用浏览器打开客户端页（macOS `open`）
        #[arg(long)]
        open: bool,
        /// 判定用的规则表（缺省用内置；平台发的那份可从 `/api/render/policy` 存下来）
        #[arg(long, value_name = "FILE")]
        policy: Option<PathBuf>,
    },
    /// 客户端：订阅远端流并落盘（帧存 PNG、快照导出 glTF）
    Stream {
        /// 服务地址，如 http://127.0.0.1:8283
        url: String,
        /// 访问令牌
        #[arg(long)]
        token: String,
        /// 要订阅的流：scene | frame
        #[arg(long, default_value = "frame")]
        kind: String,
        /// 图像流的视角：top | front | iso-sw | iso-se
        #[arg(long, default_value = "iso-sw")]
        view: String,
        /// 输出目录：帧存 frame-0001.png，场景流存 snapshot.json
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// 起手就当自己已经看到过这个版本（即模拟 `Last-Event-ID`）—— 用来手动验续传
        #[arg(long, value_name = "REV")]
        from: Option<u32>,
        /// 收到多少条消息后退出（0 = 一直跑）
        #[arg(long, default_value_t = 0)]
        limit: u64,
        /// 多久没新消息就算"没动静了"并收工（秒）。
        /// 静止场景**本就不该有流量**（服务端只推变化），所以这不是错误。
        #[arg(long, default_value_t = 8)]
        idle: u64,
        /// 断开后最多重连几次（重连会用 Last-Event-ID 续传）
        #[arg(long, default_value_t = 3)]
        retries: u32,
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
    /// 导出：把当前状态交给**外部工具链**（标准 glTF 2.0，可直接给 Blender / Fyrox / three.js）
    Export {        /// 场景或文档文件
        file: PathBuf,
        /// 输出文件；缺省用 <输入名>.gltf
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// 格式；目前只支持 gltf
        #[arg(long, default_value = "gltf")]
        format: String,
    },
    /// 导入：把外部资产（gltf/glb/obj/stl 自己读；blend/fbx/usd 请 Blender）接成场景
    Import {
        /// 源文件（没有这个参数时用 --formats 看支持哪些）
        file: Option<PathBuf>,
        /// 输出场景 JSON（缺省 <源文件名>.scene.json）；几何 side-car 写在它旁边
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// 源文件的长度单位 → 米（CAD 常见毫米：给 0.001）
        #[arg(long, default_value_t = 1.0)]
        unit_scale: f64,
        /// 最多导多少个对象（装配体可能上千件；截断会在报告里说明）
        #[arg(long)]
        limit: Option<usize>,
        /// 指定 Blender 可执行文件（也可用 RSI3D_BLENDER）
        #[arg(long, value_name = "PATH")]
        blender: Option<PathBuf>,
        /// 不写几何 side-car（只要 AABB 场景）
        #[arg(long)]
        no_mesh: bool,
        /// 列出支持的格式与各自的路子
        #[arg(long)]
        formats: bool,
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
            SceneCmd::Export { file, out, format } => {
                cmd_scene_export(cli, file, out.as_deref(), format)
            }
            SceneCmd::Import {
                file,
                out,
                unit_scale,
                limit,
                blender,
                no_mesh,
                formats,
            } => cmd_scene_import(
                cli,
                file.as_deref(),
                out.as_deref(),
                *unit_scale,
                *limit,
                blender.as_deref(),
                *no_mesh,
                *formats,
            ),
        },
        Cmd::Mcp { root } => cmd_mcp(root.as_deref()),
        Cmd::RenderMode { profile, policy } => {
            cmd_render_mode(cli, profile.as_deref(), policy.as_deref())
        }
        Cmd::Contract { out, check } => cmd_contract(cli, out.as_deref(), *check),
        Cmd::Serve {
            file,
            bind,
            port,
            fps,
            token,
            open,
            policy,
        } => serve_cmd::cmd_serve(
            cli,
            file,
            bind,
            *port,
            *fps,
            token.as_deref(),
            *open,
            policy.as_deref(),
        ),
        Cmd::Stream {
            url,
            token,
            kind,
            view,
            out,
            from,
            limit,
            idle,
            retries,
        } => serve_cmd::cmd_stream(
            cli,
            url,
            token,
            kind,
            view,
            out.as_deref(),
            *from,
            *limit,
            *idle,
            *retries,
        ),
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

/// 导出成**外部工具链能直接打开**的标准格式。
///
/// 为什么只做 glTF：那是唯一一个「我们不用交出发格式主权、对方不用装我们的东西」的交集。
/// 外部引擎（Blender / Fyrox / three.js / Unity…）都读 glTF；我们的自有信息放在
/// `extras.rsi3d`（**glTF 规范允许忽略 unknown extras**，所以不会污染别人的加载器）。
///
/// 这也是本工程与外部 3D 引擎的**唯一**对接方式：**格式级**，不是代码级。
fn cmd_scene_export(cli: &Cli, file: &Path, out: Option<&Path>, format: &str) -> Result<()> {
    if !format.eq_ignore_ascii_case("gltf") {
        bail!(
            "不支持的导出格式「{}」；目前只有 gltf。标准 glTF 2.0 已经覆盖 \
             Blender / FyroxEd / three.js / Unity 的导入路径，\
             其它格式（USDZ / SPZ / 引擎自有格式）等有真实需求再接。",
            format
        );
    }
    let doc = load_document(file)?;
    let gltf = stream::gltf::scene_to_gltf(doc.scene());
    let body = serde_json::to_string_pretty(&gltf)?;

    let target = match out {
        Some(p) => p.to_path_buf(),
        None => {
            let stem = file
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "scene".into());
            let name = format!("{}.gltf", stem);
            match file.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.join(name),
                _ => PathBuf::from(name),
            }
        }
    };
    std::fs::write(&target, &body)
        .with_context(|| format!("写不到 {}", target.display()))?;

    let nodes = gltf
        .get("nodes")
        .and_then(|n| n.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let lights = gltf
        .pointer("/extensions/KHR_lights_punctual/lights")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let mut human = String::new();
    human.push_str(&format!("✓ 已导出 {}\n", target.display()));
    human.push_str(&format!(
        "  格式        glTF 2.0（单文件，几何数据内嵌为 base64 data URI）\n"
    ));
    human.push_str(&format!("  节点 {} · 灯光 {}\n", nodes, lights));
    human.push_str(&format!(
        "  几何档次    {}（H0 还没有真网格，节点是包围盒代理；`io` 落地后才是真几何）\n",
        stream::GEOMETRY_AABB_PROXY
    ));
    human.push_str(&format!(
        "  自有信息    extras.rsi3d（role/材质/可编辑性/房间/窗/规则，外部加载器会按规范忽略）\n"
    ));
    human.push_str(&format!(
        "  版本        rev {} · 场景哈希 {}\n",
        doc.revision(),
        &doc.scene_hash()?[..8]
    ));
    human.push_str("\n这个文件可以直接给外部工具链：\n");
    human.push_str("  three.js   GLTFLoader.parse(json)\n");
    human.push_str("  Blender    导入 → glTF 2.0 (.gltf)\n");
    human.push_str("  Fyrox      FyroxEd 打开 glTF（详见 prd 里的评估）\n");

    let value = serde_json::json!({
        "ok": true,
        "out": target.display().to_string(),
        "bytes": body.len(),
        "format": "gltf",
        "gltf_version": "2.0",
        "nodes": nodes,
        "lights": lights,
        "geometry": stream::GEOMETRY_AABB_PROXY,
        "revision": doc.revision(),
        "scene_hash": doc.scene_hash()?,
    });
    emit(cli, human, value)
}

/// `contract`：把「非 Rust 消费者要遵守的形状」从 Rust 类型**派生**出来。
///
/// 这里**不手写第二份真相**：键清单与 JSON Schema 从序列化结果产出，枚举从穷尽 `match`
/// 产出。字段改名、加枚举变体都会让 **crate 编译不过**（见 `crates/contract/src/lib.rs`）。
/// 落盘的产物只是缓存，测试断言「重新生成 == 已提交」。
fn cmd_contract(cli: &Cli, out: Option<&Path>, check: bool) -> Result<()> {
    let arts = contract::artifacts();
    let known: Vec<(&str, usize)> = BLOCKS
        .iter()
        .map(|b| (*b, contract::known_keys(b).len()))
        .collect();

    if check {
        let dir = out.unwrap_or(Path::new("contract"));
        let mut stale = Vec::new();
        for (name, body) in &arts {
            let p = dir.join(name);
            match std::fs::read_to_string(&p) {
                Ok(cur) if cur == *body => {}
                Ok(_) => stale.push(format!("{} 与 Rust 类型不一致", p.display())),
                Err(_) => stale.push(format!("{} 不存在", p.display())),
            }
        }
        if !stale.is_empty() {
            bail!(
                "契约产物已过期：{}\n重新生成：rsi3d-harness contract --out {}",
                stale.join("；"),
                dir.display()
            );
        }
        return emit(
            cli,
            format!("✓ 契约产物与 Rust 类型一致（{} 个文件）", arts.len()),
            serde_json::json!({"ok": true, "files": arts.len(), "dir": dir.display().to_string()}),
        );
    }

    if let Some(dir) = out {
        let written = contract::write_all(dir).map_err(anyhow::Error::msg)?;
        let human = format!(
            "✓ 写出 {} 个契约产物到 {}\n{}",
            written.len(),
            dir.display(),
            written
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let value = serde_json::json!({
            "ok": true,
            "out": dir.display().to_string(),
            "files": written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "known_keys": known.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
        });
        return emit(cli, human, value);
    }

    // 只打印（`--json` 时给出全部产物内容，方便工具直接吃）
    let human = format!(
        "契约产物（派生自 Rust 类型，勿手改）\n{}\n已知键（含嵌套路径与裸名）：{}\n注：stream_server 里 Snapshot.gltf 内嵌的是标准 glTF 2.0 文档，键树不展开\n重新生成：rsi3d-harness contract --out contract",
        arts.iter()
            .map(|(name, body)| format!("  {:<20} {:>6} B", name, body.len()))
            .collect::<Vec<_>>()
            .join("\n"),
        known
            .iter()
            .map(|(b, n)| format!("{} {}", b, n))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    let value = serde_json::json!({
        "ok": true,
        "artifacts": arts
            .iter()
            .map(|(name, body)| {
                let parsed: serde_json::Value =
                    serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
                (name.to_string(), parsed)
            })
            .collect::<serde_json::Map<String, serde_json::Value>>(),
        "known_keys": known.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
    });
    emit(cli, human, value)
}

/// `scene import`：把外部资产接成我们的场景。
///
/// 报告里说清四件事：**谁解析的**（自己读 / 经谁的手）、**多少东西**（对象/顶点/三角面）、
/// **多大**（包围盒，米）、**几何在哪**（side-car 文件名）。任何截断与可疑单位都写进 warnings
/// ——导入器最忌讳的就是"看起来成功了"。
#[allow(clippy::too_many_arguments)]
fn cmd_scene_import(
    cli: &Cli,
    file: Option<&Path>,
    out: Option<&Path>,
    unit_scale: f64,
    limit: Option<usize>,
    blender: Option<&Path>,
    no_mesh: bool,
    formats: bool,
) -> Result<()> {
    if formats || file.is_none() {
        let table = io::formats_json();
        let human = io::FORMATS
            .iter()
            .map(|f| {
                let route = match f.route {
                    io::Route::Native => "自己读",
                    io::Route::Blender => "经 Blender",
                    io::Route::ExportUpstream => "请上游导出网格",
                };
                format!("  {:<6} {:<16} {}", f.ext, route, f.note)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let human = format!(
            "支持的格式（三条路，分得清清楚楚）\n{}\n\n用法：rsi3d-harness scene import <文件> [--out 场景.json] \
             [--unit-scale 0.001] [--limit N] [--blender PATH]",
            human
        );
        return emit(cli, human, table);
    }
    let file = file.expect("上面已经判过");

    // 输出场景 + 几何 side-car（放在同一个目录、同名前缀：服务端零配置就能找到）
    let out_path = match out {
        Some(p) => p.to_path_buf(),
        None => {
            let stem = file
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "imported".into());
            let dir = file.parent().unwrap_or(Path::new("."));
            dir.join(format!("{}.scene.json", stem))
        }
    };
    let sidecar = if no_mesh {
        None
    } else {
        let stem = out_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "scene".into());
        Some(out_path.with_file_name(format!("{}.mesh.glb", stem)))
    };

    let opts = io::ImportOptions {
        unit_scale,
        limit,
        blender: blender.map(|p| p.to_path_buf()),
        sidecar,
        role: "other".into(),
    };
    let imported = io::import(file, &opts).map_err(anyhow::Error::msg)?;

    // 落盘：场景 JSON。顺手用**内核**校验一遍——导入器不许产出内核读不了的东西
    let text = serde_json::to_string_pretty(&imported.scene)?;
    if let Err(e) = engine::Scene::from_json(&text) {
        bail!("导入器产出的场景内核读不了（这是我们的 bug）：{e}");
    }
    std::fs::write(&out_path, format!("{text}\n"))
        .with_context(|| format!("写不到 {}", out_path.display()))?;

    let human = format!(
        "{}\n  → 场景写到 {}\n下一步：\n  rsi3d-harness scene render {} --out /tmp/view\n  rsi3d-harness serve {}",
        imported.report.text().trim_end(),
        out_path.display(),
        out_path.display(),
        out_path.display()
    );
    let mut value = imported.report.json();
    value["out"] = serde_json::json!(out_path.display().to_string());
    emit(cli, human, value)
}

/// `render-mode`：规则表 + 判定。
///
/// 两个用途：① 不带 `--profile` 时把规则表打出来（"什么档要什么"，可对账）；
/// ② 带 `--profile` 时判一份主机参数——**这就是离线那条路**（联网时浏览器直接问
/// rsi3d.com，判定权威是平台）。
fn cmd_render_mode(cli: &Cli, profile: Option<&Path>, policy: Option<&Path>) -> Result<()> {
    use rsi3d_harness_stream::render_mode::{
        assess, default_policy, explain, mode_rank, HostProfile, RenderPolicy,
    };

    let policy: RenderPolicy = match policy {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("读不到规则表 {}", p.display()))?;
            serde_json::from_str(&text)
                .with_context(|| format!("{} 不是合法的规则表", p.display()))?
        }
        None => default_policy(),
    };

    let Some(file) = profile else {
        // 只打印规则表：每一档要什么、给什么限制
        let human = format!(
            "渲染模式规则表（版本 {}）\n{}\n\n{}\n\n改判定松紧 = 改这份表；平台发的是同一份（`/api/render/policy`）。",
            policy.version,
            policy.note,
            policy
                .modes
                .iter()
                .map(|m| {
                    let mut req = Vec::new();
                    if !m.require_gpu.is_empty() {
                        req.push(format!("GPU {}", m.require_gpu.join("/")));
                    }
                    if m.min_cores > 0 {
                        req.push(format!("≥{} 核", m.min_cores));
                    }
                    if m.min_sustained_fps > 0.0 {
                        req.push(format!("持续 ≥{:.0} fps", m.min_sustained_fps));
                    }
                    if m.min_max_texture > 0 {
                        req.push(format!("最大纹理 ≥{}", m.min_max_texture));
                    }
                    if m.min_viewport != (0, 0) {
                        req.push(format!("视口 ≥{}×{}", m.min_viewport.0, m.min_viewport.1));
                    }
                    if !m.allow_software {
                        req.push("要硬件加速".into());
                    }
                    format!(
                        "  {:<15} 要：{:<52} 给：≤{}×{} · ≤{} fps · 几何 {} · 流 {}",
                        m.mode,
                        if req.is_empty() { "无（兜底档）".into() } else { req.join(" · ") },
                        m.limits.max_px.0,
                        m.limits.max_px.1,
                        m.limits.max_fps,
                        m.limits.geometry,
                        m.limits.stream
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        );
        return emit(cli, human, serde_json::to_value(&policy)?);
    };

    let text =
        std::fs::read_to_string(file).with_context(|| format!("读不到 {}", file.display()))?;
    let profile: HostProfile = serde_json::from_str(&text)
        .with_context(|| format!("{} 不是合法的主机参数（见 docs/render-mode.md）", file.display()))?;
    let verdict = assess(&profile, &policy, "local");
    let _ = mode_rank(&verdict.mode);
    emit(cli, explain(&verdict), serde_json::to_value(&verdict)?)
}

/// 契约里的块名（与 `crates/contract` 保持一致）。
const BLOCKS: [&str; 7] = [
    "scene",
    "view",
    "stream_server",
    "stream_client",
    "gltf_node_extras",
    "gltf_scene_extras",
    "gltf_patch_changes",
];

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
