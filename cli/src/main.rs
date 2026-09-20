//! rsi3d CLI —— 3D 资产的自动生成与自进化平台的命令行客户端。
//!
//! 三条铁律（与本工程 README 的「设计约束」一致）：
//!
//! 1. **只走公开 HTTP API**：CLI 不 import 平台私有代码，任何语言都能照着实现一遍。
//! 2. **平台是控制面**：`rsi3d run` 只在平台登记 Run，然后**直连 Harness** 触发执行；
//!    平台只收迭代摘要（分数曲线），不中转 3D 业务数据。
//! 3. **凭据存本地**：token 落在 `~/.rsi3d/config.json`；口令优先用环境变量
//!    `RSI3D_PASSWORD`，避免出现在进程列表里。

mod api;
mod bundle;
mod config;
mod util;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use config::CliConfig;
use util::{ellipsis, human_size, now_iso8601, sha256_file, sha256_hex, sparkline, urlencode};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------- 命令行定义

#[derive(Parser)]
#[command(
    name = "rsi3d",
    version,
    about = "rsi3d — 3D 资产的自动生成与自进化平台命令行",
    after_help = "示例:\n  \
        rsi3d login --email you@x.com --password demo1234\n  \
        rsi3d publish --file ./home.pack.json --kind bizpack --slug home-display --name \"家居三维展示包\"\n  \
        rsi3d search --kind bizpack\n  \
        rsi3d download @you/home-display -o out/\n  \
        rsi3d run --harness @rsi3d/sceneweaver --intent \"北欧风客厅\" --watch"
)]
struct Cli {
    /// 平台地址（覆盖配置文件中的 base_url）
    #[arg(long, global = true, value_name = "URL")]
    base: Option<String>,

    /// 输出 JSON（便于脚本与 Agent 消费；进度信息走 stderr）
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 注册账号（邮箱 + 密码）
    Register {
        #[arg(long)]
        email: String,
        /// 口令；建议改用环境变量 RSI3D_PASSWORD
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// 平台开启邀请制时必填
        #[arg(long)]
        invite: Option<String>,
    },
    /// 登录并把 token 写入 ~/.rsi3d/config.json
    Login {
        #[arg(long)]
        email: String,
        #[arg(long)]
        password: Option<String>,
    },
    /// 退出登录（清除本地 token）
    Logout,
    /// 当前身份与命名空间
    Me,
    /// 命名空间
    Ns {
        #[command(subcommand)]
        action: NsCmd,
    },
    /// 发布制品（业务包 / 服务包 / Harness / 插件 / 技能 / 基准 / 资产）
    Publish {
        /// 制品文件；与 --url / --dir 三选一
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// 目录：打包成 skill/plugin 包再发布（目录里要有 skill.json / plugin.json）
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// 自托管地址（BYO URL）：不上传字节，只登记地址
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// 显式指定校验和（用 --url 且本地没有文件时必填）
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
        /// 显式指定字节数（配合 --url）
        #[arg(long)]
        size: Option<u64>,
        /// 命名空间内唯一的 slug；缺省由文件名推导
        #[arg(long)]
        slug: Option<String>,
        /// bizpack | svcpack | harness | plugin | skill | benchmark | asset
        #[arg(long, default_value = "bizpack")]
        kind: String,
        /// 展示名
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "0.1.0")]
        version: String,
        /// general | home | industry | culture | ecommerce
        #[arg(long)]
        domain: Option<String>,
        /// 逗号分隔
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// public | private | draft（private/draft 需 Pro）
        #[arg(long, default_value = "public")]
        visibility: String,
        /// 目标命名空间 slug（缺省个人空间）
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long)]
        summary: Option<String>,
        /// 自定义 manifest JSON 文件（缺省自动生成）
        #[arg(long, value_name = "PATH")]
        manifest: Option<PathBuf>,
        /// 追加版本时的变更说明
        #[arg(long, default_value = "")]
        changelog: String,
    },
    /// 检索目录
    Search {
        /// 关键词（匹配名称 / slug / 简介）
        query: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
        /// newest | downloads | score
        #[arg(long)]
        sort: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// 只搜我的（含 private / draft）
        #[arg(long)]
        mine: bool,
    },
    /// 查看制品详情
    Info {
        /// @ns/slug、ns/slug、A-xxx 或裸 slug
        reference: String,
    },
    /// 下载制品并校验 sha256
    Download {
        reference: String,
        /// 输出文件或目录（默认当前目录）
        #[arg(short = 'o', long = "out", value_name = "PATH")]
        out: Option<PathBuf>,
        /// 跳过校验和比对
        #[arg(long)]
        no_verify: bool,
    },
    /// 校验：本地文件摘要 / pack 清单 / 制品元数据
    Verify {
        /// 文件路径或制品引用
        target: String,
        /// 签名密钥文件（默认 ~/.rsi3d/signing.key）
        #[arg(long, value_name = "PATH")]
        key: Option<PathBuf>,
    },
    /// 登记并执行一次 3D RSI Run（直连 Harness，平台只记摘要）
    Run {
        /// Harness 的 H-xxx 或 @ns/slug
        #[arg(long)]
        harness: String,
        /// 3D 意图（文字描述）
        #[arg(long)]
        intent: String,
        /// 迭代预算，传给 Harness
        #[arg(long, default_value_t = 4)]
        iters: usize,
        /// 实时打印每轮迭代分数（产品最有说服力的演示）
        #[arg(long)]
        watch: bool,
        /// --watch 的最长等待秒数
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// 覆盖阶段链，如 generate,evaluate,critique,mutate,accept
        #[arg(long, value_delimiter = ',')]
        phases: Vec<String>,
    },
    /// 我的 Run 列表
    Runs {
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 业务包 / 服务包工具链
    Pack {
        #[command(subcommand)]
        action: PackCmd,
    },
    /// API-Key 管理
    Key {
        #[command(subcommand)]
        action: KeyCmd,
    },
    /// Harness Hub（注册 / 检索 / 询价 / 反馈）
    Harness {
        #[command(subcommand)]
        action: HarnessCmd,
    },
    /// 安装插件 / 技能到本地（技能包会被解开；--agent 直接落到该 Agent 的技能目录）
    Install {
        reference: String,
        /// 安装根目录（默认 ~/.rsi3d/installed）
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// 直接装进某个 Agent 的技能目录：claude-code | copilot | agents
        #[arg(long)]
        agent: Option<String>,
        /// user（默认）| project
        #[arg(long, default_value = "user")]
        scope: String,
        /// project 作用域下的项目根（默认当前目录）
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
    },
    /// 技能包：打包（发布用）
    Skill {
        #[command(subcommand)]
        action: SkillCmd,
    },
    /// 面向各 Agent 的接入形态
    Plugins {
        /// mcp | harness-use | claude-code | cursor | cli
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Subcommand)]
enum NsCmd {
    /// 我的命名空间
    List,
    /// 创建组织命名空间（需 Pro）
    Create {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum SkillCmd {
    /// 把技能目录打成确定性 zip 包（发布前想看包长什么样就用它）
    Pack {
        /// 技能目录（里面要有 skill.json，含 entry/files）
        dir: PathBuf,
        /// 输出文件（缺省 <name>.zip）
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// skill | plugin
        #[arg(long, default_value = "skill")]
        kind: String,
    },
    /// 看一个包里有什么（校验清单与每个条目的校验和；别人的包也能读）
    Inspect {
        /// 包文件（.zip）
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum PackCmd {
    /// 生成 pack 清单模板
    Init {
        #[arg(long, default_value = "my-pack")]
        slug: String,
        #[arg(long, default_value = "bizpack")]
        kind: String,
        #[arg(short = 'o', long = "out", default_value = "rsi3d.pack.json")]
        out: PathBuf,
    },
    /// 扫描目录、计算逐文件 sha256 并写入清单
    Build {
        /// 目录（默认当前目录）
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// 清单路径（默认 <dir>/rsi3d.pack.json）
        #[arg(short = 'o', long = "out")]
        out: Option<PathBuf>,
        #[arg(long)]
        slug: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        version: Option<String>,
    },
    /// 用本地密钥给清单签名（HMAC-SHA256）
    Sign {
        #[arg(long, default_value = "rsi3d.pack.json")]
        file: PathBuf,
        #[arg(long, value_name = "PATH")]
        key: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum KeyCmd {
    List,
    Create {
        #[arg(long, default_value = "cli")]
        label: String,
    },
    Revoke { id: String },
}

#[derive(Subcommand)]
enum HarnessCmd {
    /// 检索 Harness
    List {
        #[arg(long)]
        capability: Option<String>,
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        q: Option<String>,
        /// score | newest
        #[arg(long)]
        sort: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 详情
    Show { reference: String },
    /// 注册能力声明（初始信誉分 70，未验证）
    Register {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        endpoint: String,
        /// http | mcp
        #[arg(long, default_value = "http")]
        protocol: String,
        /// 逗号分隔，如 text-to-3d,evaluate
        #[arg(long, value_delimiter = ',')]
        capabilities: Vec<String>,
        #[arg(long)]
        domain: Option<String>,
        /// 定价 JSON，如 '{"unit":"run","amount":0.05}'
        #[arg(long)]
        pricing: Option<String>,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// 询价
    Quote {
        id: String,
        #[arg(long)]
        intent: String,
        #[arg(long, default_value_t = 4)]
        iters: usize,
    },
    /// 信誉反馈（-20..20）
    Feedback {
        id: String,
        #[arg(long)]
        delta: f64,
        #[arg(long, default_value = "")]
        comment: String,
        #[arg(long)]
        run: Option<String>,
    },
}

// ---------------------------------------------------------------- 上下文

struct Ctx {
    cfg: CliConfig,
    json: bool,
}

impl Ctx {
    fn base(&self) -> &str {
        self.cfg.base_url.trim_end_matches('/')
    }

    /// 登录态 GET（未登录也能用，服务端按匿名处理）。
    fn api_get(&self, path: &str) -> Result<Value> {
        match self.cfg.token.as_deref() {
            Some(t) => api::get_authed(&self.cfg, t, path),
            None => api::get(&self.cfg, path),
        }
    }

    /// 需要登录的 GET。
    fn api_get_auth(&self, path: &str) -> Result<Value> {
        api::get_authed(&self.cfg, &self.token()?, path)
    }

    fn api_post(&self, path: &str, body: Value) -> Result<Value> {
        api::post(&self.cfg, &self.token()?, path, body)
    }

    fn api_patch(&self, path: &str, body: Value) -> Result<Value> {
        api::patch(&self.cfg, &self.token()?, path, body)
    }

    fn api_delete(&self, path: &str) -> Result<Value> {
        api::delete(&self.cfg, &self.token()?, path)
    }

    fn token(&self) -> Result<String> {
        config::require_token(&self.cfg)
    }

    /// 人类可读内容与 JSON 内容二选一输出。
    fn emit(&self, human: String, value: Value) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else if !human.is_empty() {
            println!("{}", human);
        }
        Ok(())
    }

    /// 进度信息：人类模式下走 stdout，JSON 模式下走 stderr（不污染结果）。
    fn note(&self, msg: impl AsRef<str>) {
        if self.json {
            eprintln!("{}", msg.as_ref());
        } else {
            println!("{}", msg.as_ref());
        }
    }
}

// ---------------------------------------------------------------- 入口

fn main() {
    let Cli { base, json, cmd } = Cli::parse();

    let mut cfg = config::load();
    if let Some(b) = base {
        cfg.base_url = b.trim_end_matches('/').to_string();
    }
    let mut ctx = Ctx { cfg, json };

    if let Err(e) = dispatch(&mut ctx, cmd) {
        if ctx.json {
            println!(
                "{}",
                json!({"error": {"code": "cli_error", "message": format!("{:#}", e)}})
            );
        } else {
            eprintln!("✗ {:#}", e);
        }
        std::process::exit(1);
    }
}

fn dispatch(ctx: &mut Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Register {
            email,
            password,
            name,
            invite,
        } => cmd_register(ctx, email, password, name, invite),
        Cmd::Login { email, password } => cmd_login(ctx, email, password),
        Cmd::Logout => cmd_logout(ctx),
        Cmd::Me => cmd_me(ctx),
        Cmd::Ns { action } => match action {
            NsCmd::List => cmd_ns_list(ctx),
            NsCmd::Create { slug, name } => cmd_ns_create(ctx, slug, name),
        },
        Cmd::Publish {
            file,
            dir,
            url,
            sha256,
            size,
            slug,
            kind,
            name,
            version,
            domain,
            tags,
            visibility,
            namespace,
            summary,
            manifest,
            changelog,
        } => cmd_publish(
            ctx,
            PublishArgs {
                file,
                dir,
                url,
                sha256,
                size,
                slug,
                kind,
                name,
                version,
                domain,
                tags,
                visibility,
                namespace,
                summary,
                manifest,
                changelog,
            },
        ),
        Cmd::Search {
            query,
            kind,
            domain,
            tag,
            namespace,
            sort,
            limit,
            mine,
        } => cmd_search(ctx, query, kind, domain, tag, namespace, sort, limit, mine),
        Cmd::Info { reference } => cmd_info(ctx, reference),
        Cmd::Download {
            reference,
            out,
            no_verify,
        } => cmd_download(ctx, reference, out, no_verify),
        Cmd::Verify { target, key } => cmd_verify(ctx, target, key),
        Cmd::Run {
            harness,
            intent,
            iters,
            watch,
            timeout,
            phases,
        } => cmd_run(ctx, harness, intent, iters, watch, timeout, phases),
        Cmd::Runs { status, limit } => cmd_runs(ctx, status, limit),
        Cmd::Pack { action } => match action {
            PackCmd::Init { slug, kind, out } => cmd_pack_init(ctx, slug, kind, out),
            PackCmd::Build {
                dir,
                out,
                slug,
                kind,
                version,
            } => cmd_pack_build(ctx, dir, out, slug, kind, version),
            PackCmd::Sign { file, key } => cmd_pack_sign(ctx, file, key),
        },
        Cmd::Key { action } => match action {
            KeyCmd::List => cmd_key_list(ctx),
            KeyCmd::Create { label } => cmd_key_create(ctx, label),
            KeyCmd::Revoke { id } => cmd_key_revoke(ctx, id),
        },
        Cmd::Harness { action } => match action {
            HarnessCmd::List {
                capability,
                domain,
                q,
                sort,
                limit,
            } => cmd_harness_list(ctx, capability, domain, q, sort, limit),
            HarnessCmd::Show { reference } => cmd_harness_show(ctx, reference),
            HarnessCmd::Register {
                slug,
                title,
                endpoint,
                protocol,
                capabilities,
                domain,
                pricing,
                summary,
                namespace,
            } => cmd_harness_register(
                ctx,
                slug,
                title,
                endpoint,
                protocol,
                capabilities,
                domain,
                pricing,
                summary,
                namespace,
            ),
            HarnessCmd::Quote {
                id,
                intent,
                iters,
            } => cmd_harness_quote(ctx, id, intent, iters),
            HarnessCmd::Feedback {
                id,
                delta,
                comment,
                run,
            } => cmd_harness_feedback(ctx, id, delta, comment, run),
        },
        Cmd::Install {
            reference,
            dir,
            agent,
            scope,
            project,
        } => cmd_install(ctx, reference, dir, agent, scope, project),
        Cmd::Skill { action } => match action {
            SkillCmd::Pack { dir, out, kind } => cmd_skill_pack(ctx, dir, out, kind),
            SkillCmd::Inspect { file } => cmd_skill_inspect(ctx, file),
        },
        Cmd::Plugins { agent } => cmd_plugins(ctx, agent),
    }
}

// ---------------------------------------------------------------- 账号

fn cmd_register(
    ctx: &mut Ctx,
    email: String,
    password: Option<String>,
    name: Option<String>,
    invite: Option<String>,
) -> Result<()> {
    let pw = password_arg(password)?;
    let v = api::post_anon(
        &ctx.cfg,
        "/api/auth/register",
        json!({
            "email": email,
            "password": pw,
            "name": name.unwrap_or_default(),
            "invite": invite.unwrap_or_default(),
        }),
    )?;
    save_session(ctx, &v)?;
    let human = format!(
        "✓ 注册成功\n  账号     {}\n  计划     {}\n  命名空间 {}\n  配置     {}",
        v["user"]["email"].as_str().unwrap_or(""),
        v["user"]["plan"].as_str().unwrap_or("free"),
        ns_summary(&v),
        config::config_path().display()
    );
    ctx.emit(human, v)
}

fn cmd_login(ctx: &mut Ctx, email: String, password: Option<String>) -> Result<()> {
    let pw = password_arg(password)?;
    let v = api::post_anon(
        &ctx.cfg,
        "/api/auth/login",
        json!({"email": email, "password": pw}),
    )?;
    save_session(ctx, &v)?;
    let human = format!(
        "✓ 已登录\n  账号     {}\n  计划     {}\n  命名空间 {}\n  token    {}",
        v["user"]["email"].as_str().unwrap_or(""),
        v["user"]["plan"].as_str().unwrap_or("free"),
        ns_summary(&v),
        config::config_path().display()
    );
    ctx.emit(human, v)
}

fn cmd_logout(ctx: &Ctx) -> Result<()> {
    let was = ctx.cfg.email.clone().unwrap_or_else(|| "-".into());
    let mut cfg = ctx.cfg.clone();
    cfg.token = None;
    config::save(&cfg)?;
    ctx.emit(
        format!("✓ 已退出登录（本地 token 已清除，原账号 {}）", was),
        json!({"ok": true, "email": was}),
    )
}

fn cmd_me(ctx: &Ctx) -> Result<()> {
    let v = ctx.api_get_auth("/api/auth/me")?;
    let ns: Vec<String> = v["namespaces"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|n| {
                    format!(
                        "  @{:<22} {} ({})",
                        n["slug"].as_str().unwrap_or(""),
                        n["name"].as_str().unwrap_or(""),
                        n["kind"].as_str().unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let human = format!(
        "账号     {}\n名称     {}\n计划     {}\n鉴权方式 {}\n命名空间\n{}",
        v["user"]["email"].as_str().unwrap_or(""),
        v["user"]["name"].as_str().unwrap_or("-"),
        v["user"]["plan"].as_str().unwrap_or("free"),
        v["authKind"].as_str().unwrap_or("-"),
        if ns.is_empty() {
            "  -".to_string()
        } else {
            ns.join("\n")
        }
    );
    ctx.emit(human, v)
}

fn cmd_ns_list(ctx: &Ctx) -> Result<()> {
    let v = ctx.api_get("/api/namespaces")?;
    let human = v["namespaces"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|n| {
                    format!(
                        "@{:<22} {} ({}) plan={}",
                        n["slug"].as_str().unwrap_or(""),
                        n["name"].as_str().unwrap_or(""),
                        n["kind"].as_str().unwrap_or(""),
                        n["plan"].as_str().unwrap_or("free")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    ctx.emit(human, v)
}

fn cmd_ns_create(ctx: &Ctx, slug: String, name: Option<String>) -> Result<()> {
    let v = ctx.api_post(
        "/api/namespaces",
        json!({"slug": slug, "name": name.unwrap_or_default()}),
    )?;
    ctx.emit(
        format!(
            "✓ 已创建组织命名空间 @{}{}",
            v["namespace"]["slug"].as_str().unwrap_or(""),
            if v["namespace"]["plan"].as_str() == Some("free") {
                "（注意：free 计划下私有制品不可用）"
            } else {
                ""
            }
        ),
        v,
    )
}

// ---------------------------------------------------------------- 发布与检索

struct PublishArgs {
    file: Option<PathBuf>,
    dir: Option<PathBuf>,
    url: Option<String>,
    sha256: Option<String>,
    size: Option<u64>,
    slug: Option<String>,
    kind: String,
    name: Option<String>,
    version: String,
    domain: Option<String>,
    tags: Vec<String>,
    visibility: String,
    namespace: Option<String>,
    summary: Option<String>,
    manifest: Option<PathBuf>,
    changelog: String,
}

fn cmd_publish(ctx: &Ctx, args: PublishArgs) -> Result<()> {
    // ---- 1. 本地字节与摘要 ----
    let mut filename: Option<String> = None;
    let mut bytes: Option<Vec<u8>> = None;
    let mut sha = String::new();
    let mut size: u64 = 0;
    // 目录打包时带出来的清单（包内单一出处），后面当 manifest 的基座
    let mut packed: Option<bundle::BundleManifest> = None;

    if let Some(d) = &args.dir {
        // 目录 → skill/plugin 包。清单在目录里（skill.json / plugin.json），
        // 打包会顺便校验「清单声明的文件都在、清单没漏掉目录里的文件」。
        if !matches!(args.kind.as_str(), "skill" | "plugin") {
            return Err(anyhow!(
                "--dir 只能用于 skill / plugin（当前 --kind {}）",
                args.kind
            ));
        }
        let (zip, m) = bundle::pack_dir(d, &args.kind)
            .with_context(|| format!("打包失败: {}", d.display()))?;
        ctx.note(format!(
            "→ 已打包 {} 个文件 → {}（store 无压缩，同内容必得同字节）",
            m.files.len() + 1,
            m.name
        ));
        sha = sha256_hex(&zip);
        size = zip.len() as u64;
        filename = Some(format!("{}.zip", m.name));
        packed = Some(m);
        bytes = Some(zip);
    }

    if let Some(p) = &args.file {
        let data = std::fs::read(p).with_context(|| format!("读取文件失败: {}", p.display()))?;
        filename = Some(
            p.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .ok_or_else(|| anyhow!("无法从路径推断文件名: {}", p.display()))?,
        );
        sha = sha256_hex(&data);
        size = data.len() as u64;
        bytes = Some(data);
    }
    if let Some(s) = &args.sha256 {
        sha = s.trim().to_lowercase();
    }
    if let Some(n) = args.size {
        size = n;
    }

    // ---- 2. slug ----
    let slug = match args.slug.clone() {
        Some(s) => normalize_slug(&s)?,
        None => {
            let base = filename
                .as_deref()
                .ok_or_else(|| anyhow!("缺少 --slug（且没有 --file 可推导文件名）"))?;
            normalize_slug(&strip_extension(base))?
        }
    };

    // ---- 3. 字节地址：上传 or BYO URL ----
    let storage_url = match &args.url {
        Some(u) => {
            if sha.is_empty() {
                return Err(anyhow!("用 --url 时需要 --sha256（或配合 --file 自动计算）"));
            }
            u.clone()
        }
        None => {
            let data = bytes
                .as_ref()
                .ok_or_else(|| anyhow!("请用 --file <路径> 指定制品，或用 --url 提供自托管地址"))?;
            let fname = filename.clone().unwrap_or_else(|| format!("{}.bin", slug));
            ctx.note(format!("→ 上传制品字节（{}）…", human_size(size)));
            let up = ctx.api_post_upload(&fname, data)?;
            sha = up["sha256"].as_str().unwrap_or(&sha).to_string();
            size = up["size"].as_u64().unwrap_or(size);
            up["url"].as_str().unwrap_or_default().to_string()
        }
    };

    // ---- 4. manifest ----
    // 上传的文件本身就是清单（如 pack 产物）时直接沿用它，避免「自动包装」把声明信息丢掉。
    let mut manifest = match (&packed, &args.manifest) {
        // 包内清单优先：它就是那个「单一出处」，重打一份迟早会与包里的不一致
        (Some(m), _) => serde_json::to_value(m)?,
        (None, Some(p)) => serde_json::from_str::<Value>(
            &std::fs::read_to_string(p)
                .with_context(|| format!("读取 manifest 失败: {}", p.display()))?,
        )
        .with_context(|| format!("manifest 不是合法 JSON: {}", p.display()))?,
        (None, None) => bytes
            .as_ref()
            .and_then(|b| serde_json::from_slice::<Value>(b).ok())
            .filter(|v| v.is_object() && v.get("spec").is_some())
            .unwrap_or_else(|| json!({})),
    };
    if !manifest.is_object() {
        manifest = json!({});
    }
    let obj = manifest.as_object_mut().expect("已保证是对象");
    obj.entry("spec").or_insert(json!("rsi3d-artifact/v1"));
    obj.entry("slug").or_insert(json!(slug));
    obj.entry("kind").or_insert(json!(args.kind));
    obj.entry("version").or_insert(json!(args.version));
    obj.insert(
        "file".into(),
        json!({
            "name": filename.clone().unwrap_or_default(),
            "sha256": sha,
            "size": size,
        }),
    );
    obj.insert("generatedAt".into(), json!(now_iso8601()));
    obj.insert("generator".into(), json!(format!("rsi3d-cli/{}", VERSION)));

    // ---- 5. 创建制品；slug 撞了就追加版本 ----
    let mut body = json!({
        "slug": slug,
        "kind": args.kind,
        "version": args.version,
        "visibility": args.visibility,
        "domain": args.domain.clone().unwrap_or_else(|| "general".into()),
        "name": args.name.clone().unwrap_or_else(|| slug.clone()),
        "summary": args.summary.clone().unwrap_or_default(),
        "tags": args.tags,
        "sha256": sha,
        "size": size,
        "url": storage_url,
        "manifest": manifest,
    });
    if let Some(ns) = &args.namespace {
        body["namespace"] = json!(ns);
    }

    match ctx.api_post("/api/artifacts", body) {
        Ok(v) => {
            let human = format!(
                "✓ 已发布 {}\n  类型     {} · {}\n  版本     {}\n  大小     {}\n  sha256   {}\n  可见性   {}\n  下载     rsi3d download {}",
                v["artifact"]["ref"].as_str().unwrap_or(""),
                args.kind,
                v["artifact"]["domain"].as_str().unwrap_or("general"),
                v["artifact"]["version"].as_str().unwrap_or(""),
                human_size(size),
                v["artifact"]["sha256"].as_str().unwrap_or(""),
                v["artifact"]["visibility"].as_str().unwrap_or(""),
                v["artifact"]["ref"].as_str().unwrap_or("")
            );
            ctx.emit(human, v)
        }
        Err(e) if err_contains(&e, "slug_taken") => {
            let existing = find_artifact(ctx, args.namespace.as_deref(), &slug, Some(&args.kind), true)?
                .ok_or_else(|| {
                    anyhow!(
                        "该 slug 已存在但未能定位到制品（{}）；请确认 --namespace 是否正确",
                        slug
                    )
                })?;
            let id = existing["id"].as_str().unwrap_or_default().to_string();
            let changelog = if args.changelog.trim().is_empty() {
                format!("发布 {}", args.version)
            } else {
                args.changelog.clone()
            };
            let v = ctx.api_post(
                &format!("/api/artifacts/{}/versions", id),
                json!({
                    "version": args.version,
                    "sha256": sha,
                    "size": size,
                    "url": storage_url,
                    "changelog": changelog,
                }),
            )?;
            let human = format!(
                "✓ 已追加版本 {}\n  版本     {}\n  sha256   {}\n  变更     {}",
                v["artifact"]["ref"].as_str().unwrap_or(""),
                v["artifact"]["version"].as_str().unwrap_or(""),
                v["artifact"]["sha256"].as_str().unwrap_or(""),
                changelog
            );
            ctx.emit(human, v)
        }
        Err(e) => Err(e),
    }
}

fn cmd_search(
    ctx: &Ctx,
    query: Option<String>,
    kind: Option<String>,
    domain: Option<String>,
    tag: Option<String>,
    namespace: Option<String>,
    sort: Option<String>,
    limit: usize,
    mine: bool,
) -> Result<()> {
    let mut qs: Vec<String> = Vec::new();
    if let Some(v) = query {
        qs.push(format!("q={}", urlencode(&v)));
    }
    if let Some(v) = kind {
        qs.push(format!("kind={}", urlencode(&v)));
    }
    if let Some(v) = domain {
        qs.push(format!("domain={}", urlencode(&v)));
    }
    if let Some(v) = tag {
        qs.push(format!("tag={}", urlencode(&v)));
    }
    if let Some(v) = namespace {
        qs.push(format!("namespace={}", urlencode(&v)));
    }
    if let Some(v) = sort {
        qs.push(format!("sort={}", urlencode(&v)));
    }
    if mine {
        qs.push("mine=1".into());
    }
    qs.push(format!("limit={}", limit.clamp(1, 200)));

    let v = ctx.api_get(&format!("/api/artifacts?{}", qs.join("&")))?;
    let items = v["artifacts"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return ctx.emit("（没有匹配的制品）".into(), v);
    }
    let mut lines = vec![format!("共 {} 条", items.len())];
    for a in &items {
        lines.push(format!(
            "{:<34} {:<9} {:<8} ↓{:<5} {}",
            a["ref"].as_str().unwrap_or(""),
            a["kind"].as_str().unwrap_or(""),
            a["version"].as_str().unwrap_or(""),
            a["downloads"].as_i64().unwrap_or(0),
            a["name"].as_str().unwrap_or("")
        ));
        let summary = ellipsis(a["summary"].as_str().unwrap_or(""), 56);
        if !summary.is_empty() {
            lines.push(format!("    {}", summary));
        }
    }
    lines.push("（用 rsi3d info <ref> 看详情，rsi3d download <ref> 下载）".into());
    ctx.emit(lines.join("\n"), v)
}

fn cmd_info(ctx: &Ctx, reference: String) -> Result<()> {
    let art = resolve_artifact(ctx, &reference)?;
    let manifest = art
        .get("manifest")
        .map(|m| serde_json::to_string_pretty(m).unwrap_or_default())
        .unwrap_or_else(|| "-".into());

    let mut lines = vec![format!(
        "{}",
        art["ref"].as_str().unwrap_or(&reference)
    )];
    let kv = |k: &str, v: String| format!("  {:<8} {}", k, v);
    lines.push(kv("名称", art["name"].as_str().unwrap_or("-").into()));
    lines.push(kv(
        "类型",
        format!(
            "{} · {}",
            art["kind"].as_str().unwrap_or("-"),
            art["domain"].as_str().unwrap_or("-")
        ),
    ));
    lines.push(kv("版本", art["version"].as_str().unwrap_or("-").into()));
    lines.push(kv("可见性", art["visibility"].as_str().unwrap_or("-").into()));
    lines.push(kv("命名空间", art["namespace"]["slug"].as_str().unwrap_or("-").into()));
    let tags = art["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    lines.push(kv("标签", if tags.is_empty() { "-".into() } else { tags }));
    lines.push(kv(
        "大小",
        human_size(art["size"].as_u64().unwrap_or(0)),
    ));
    lines.push(kv(
        "校验和",
        format!(
            "sha256:{} {}",
            art["sha256"].as_str().unwrap_or("-"),
            if art["signed"].as_bool().unwrap_or(false) {
                "(已签名)"
            } else {
                "(未签名)"
            }
        ),
    ));
    lines.push(kv(
        "下载量",
        art["downloads"].as_i64().unwrap_or(0).to_string(),
    ));
    let url = art["url"].as_str().unwrap_or("");
    lines.push(kv(
        "地址",
        if url.is_empty() {
            "（BYO URL 为空，不可下载）".into()
        } else {
            url.to_string()
        },
    ));
    let summary = art["summary"].as_str().unwrap_or("");
    if !summary.is_empty() {
        lines.push(kv("简介", summary.to_string()));
    }
    if let Some(vers) = art["versions"].as_array() {
        if !vers.is_empty() {
            lines.push("  版本历史".into());
            for v in vers {
                lines.push(format!(
                    "    - {:<10} {:<22} {}",
                    v["version"].as_str().unwrap_or(""),
                    v["sha256"].as_str().unwrap_or(""),
                    ellipsis(v["changelog"].as_str().unwrap_or(""), 32)
                ));
            }
        }
    }
    lines.push("  manifest".into());
    for l in manifest.lines() {
        lines.push(format!("    {}", l));
    }
    ctx.emit(lines.join("\n"), art)
}

fn cmd_download(ctx: &Ctx, reference: String, out: Option<PathBuf>, no_verify: bool) -> Result<()> {
    let art = resolve_artifact(ctx, &reference)?;
    let id = art["id"].as_str().unwrap_or_default().to_string();
    let expect = art["sha256"].as_str().unwrap_or("").to_string();
    let url = art["url"].as_str().unwrap_or("");
    if url.is_empty() {
        return Err(anyhow!(
            "{} 没有可下载的字节地址（BYO URL 为空）",
            art["ref"].as_str().unwrap_or(&reference)
        ));
    }

    let dest = match &out {
        Some(p) if p.is_dir() || p.to_string_lossy().ends_with('/') => p.join(out_filename(&art)),
        Some(p) => p.clone(),
        None => PathBuf::from(out_filename(&art)),
    };
    if let Some(dir) = dest.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }

    ctx.note(format!("→ 下载 {}", url));
    let (size, got) = api::download_to(
        &ctx.cfg,
        ctx.cfg.token.as_deref(),
        &format!("/api/artifacts/{}/download", id),
        &dest,
    )?;

    let verified = !expect.is_empty() && expect == got;
    if !no_verify && !expect.is_empty() && !verified {
        let _ = std::fs::remove_file(&dest);
        return Err(anyhow!(
            "校验和不匹配，已删除下载文件\n  期望 {}\n  实际 {}",
            expect,
            got
        ));
    }
    let verdict = if verified {
        "通过 (sha256)".to_string()
    } else if expect.is_empty() {
        "制品未登记 sha256，跳过".to_string()
    } else {
        "已按 --no-verify 跳过".to_string()
    };
    let human = format!(
        "✓ 已下载 {}\n  制品   {}\n  版本   {}\n  大小   {}\n  校验   {}",
        dest.display(),
        art["ref"].as_str().unwrap_or(""),
        art["version"].as_str().unwrap_or(""),
        human_size(size),
        verdict
    );
    ctx.emit(
        human,
        json!({
            "path": dest.display().to_string(),
            "ref": art["ref"],
            "version": art["version"],
            "size": size,
            "sha256": got,
            "expectedSha256": expect,
            "verified": verified,
            "skipped": no_verify,
        }),
    )
}

fn cmd_verify(ctx: &Ctx, target: String, key: Option<PathBuf>) -> Result<()> {
    let path = Path::new(&target);
    if path.exists() {
        // 只有「已打包或已签名」的清单才走 pack 校验，否则按普通文件算摘要。
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                let is_pack = v["spec"].as_str() == Some("rsi3d-pack/v1")
                    && v["files"].is_array()
                    && (v["digest"].is_string() || v["signature"].is_string());
                if is_pack {
                    return verify_pack(ctx, path, &v, key.as_deref());
                }
            }
        }
        let (sha, size) = sha256_file(path)?;
        return ctx.emit(
            format!(
                "✓ {}\n  大小   {}\n  sha256 {}",
                path.display(),
                human_size(size),
                sha
            ),
            json!({"path": path.display().to_string(), "sha256": sha, "size": size}),
        );
    }

    // 制品引用：核对登记信息
    let art = resolve_artifact(ctx, &target)?;
    let mut lines = vec![format!(
        "✓ {} 登记信息",
        art["ref"].as_str().unwrap_or(&target)
    )];
    lines.push(format!(
        "  sha256   {}",
        art["sha256"].as_str().unwrap_or("（未登记）")
    ));
    lines.push(format!(
        "  签名     {}",
        if art["signed"].as_bool().unwrap_or(false) {
            art["signature"].as_str().unwrap_or("（缺失）")
        } else {
            "未验签（供给方未签名）"
        }
    ));
    if let Some(vers) = art["versions"].as_array() {
        lines.push(format!("  版本数   {}", vers.len()));
        for v in vers {
            lines.push(format!(
                "    - {:<10} {}",
                v["version"].as_str().unwrap_or(""),
                v["sha256"].as_str().unwrap_or("（无校验和）")
            ));
        }
    }
    lines.push("  提示     内容一致性需下载后比对：rsi3d download <ref>".into());
    ctx.emit(lines.join("\n"), art)
}

// ---------------------------------------------------------------- Run

fn cmd_run(
    ctx: &Ctx,
    harness: String,
    intent: String,
    iters: usize,
    watch: bool,
    timeout: u64,
    phases: Vec<String>,
) -> Result<()> {
    let h = resolve_harness(ctx, &harness)?;
    let hid = h["id"].as_str().unwrap_or_default().to_string();
    let protocol = h["protocol"].as_str().unwrap_or("http").to_string();
    let endpoint = h["endpoint"].as_str().unwrap_or_default().to_string();
    let title = h["title"].as_str().unwrap_or(&hid).to_string();

    // 1) 控制面登记 Run（拿到 run_token —— 最小权限的迭代上报凭据）
    let mut body = json!({"harnessId": hid, "intent": intent});
    if !phases.is_empty() {
        body["phases"] = json!(phases);
    }
    let reg = ctx.api_post("/api/runs", body)?;
    let run_id = reg["run"]["id"].as_str().unwrap_or_default().to_string();
    let run_token = reg["run_token"].as_str().unwrap_or_default().to_string();
    ctx.note(format!(
        "→ Run {} 已登记 · Harness {}（{}）",
        run_id, title, protocol
    ));

    // 2) 数据面：CLI 直连 Harness，平台不中转
    if protocol == "mcp" {
        ctx.note("! 该 Harness 走 MCP 协议，CLI 不能直接调用；请在 MCP 客户端触发，摘要会实时回流平台。");
        if watch {
            return watch_run(ctx, &run_id, timeout);
        }
        return ctx.emit(
            format!("Run {} 已登记。查看：rsi3d runs", run_id),
            reg,
        );
    }

    let call = json!({
        "spec": "rsi3d-harness/v1",
        "runId": run_id,
        "runToken": run_token,
        "intent": reg["run"]["intent"],
        "iters": iters,
        "phases": reg["run"]["phases"],
        "reportBase": ctx.base(),
        "reportPath": format!("/api/runs/{}/iterations", run_id),
        "finishPath": format!("/api/runs/{}", run_id),
    });
    ctx.note(format!("→ 直连 Harness：{}", endpoint));
    match api::call_harness(&endpoint, &call) {
        Ok(r) => ctx.note(format!("  Harness 返回：{}", ellipsis(&r.to_string(), 180))),
        Err(e) => {
            // 直连失败就把 Run 收尾成 failed，避免账本里留一个永远 running 的僵尸 Run。
            ctx.note(format!("! 调用 Harness 失败：{:#}", e));
            match ctx.api_patch(
                &format!("/api/runs/{}", run_id),
                json!({"status": "failed"}),
            ) {
                Ok(_) => ctx.note(format!("  Run {} 已标记为 failed", run_id)),
                Err(e2) => ctx.note(format!("  收尾 Run 也失败：{:#}", e2)),
            }
        }
    }

    if watch {
        return watch_run(ctx, &run_id, timeout);
    }
    ctx.emit(
        format!(
            "Run {} 已发起。\n  查看进度  rsi3d runs\n  实时曲线  rsi3d run --harness {} --intent \"…\" --watch",
            run_id, hid
        ),
        reg,
    )
}

fn watch_run(ctx: &Ctx, run_id: &str, timeout: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout.max(5));
    let mut seen = 0usize;
    loop {
        let v = ctx.api_get_auth(&format!("/api/runs/{}", run_id))?;
        let its = v["iterations"].as_array().cloned().unwrap_or_default();
        for it in its.iter().skip(seen) {
            print_iteration(it);
        }
        seen = its.len();

        let status = v["run"]["status"].as_str().unwrap_or("running").to_string();
        if status != "running" {
            let r = &v["run"];
            let curve: Vec<f64> = r["score_curve"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
                .unwrap_or_default();
            ctx.note(format!(
                "✓ Run {} · {} · 最佳 {:.2} · {} 轮 · 耗时 {} · 曲线 {}",
                run_id,
                status,
                r["best_score"].as_f64().unwrap_or(0.0),
                r["iterations"].as_i64().unwrap_or(0),
                fmt_cost(r["cost_ms"].as_i64().unwrap_or(0)),
                sparkline(&curve)
            ));
            return ctx.emit(String::new(), v);
        }
        if Instant::now() >= deadline {
            ctx.note(format!(
                "… 已等待 {}s，Run 仍在 running（harness 可能还在跑）；稍后用 `rsi3d runs` 复查",
                timeout
            ));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
}

fn print_iteration(it: &Value) {
    let idx = it["index"].as_i64().unwrap_or(0);
    let phase = it["phase"].as_str().unwrap_or("");
    let score = it["score"].as_f64().unwrap_or(0.0);
    let note = ellipsis(it["note"].as_str().unwrap_or(""), 42);
    if score > 0.0 {
        println!("  #{:<3} {:<9} {:.2}  {}", idx, phase, score, note);
    } else {
        println!("  #{:<3} {:<9}   —   {}", idx, phase, note);
    }
}

fn cmd_runs(ctx: &Ctx, status: Option<String>, limit: usize) -> Result<()> {
    let mut path = format!("/api/runs?limit={}", limit.clamp(1, 200));
    if let Some(s) = status {
        path.push_str(&format!("&status={}", urlencode(&s)));
    }
    let v = ctx.api_get_auth(&path)?;
    let runs = v["runs"].as_array().cloned().unwrap_or_default();
    if runs.is_empty() {
        return ctx.emit("（还没有 Run；用 rsi3d run --harness <ref> --intent \"…\" 跑一次）".into(), v);
    }
    let mut lines = vec![format!("共 {} 个 Run", runs.len())];
    for r in &runs {
        let curve: Vec<f64> = r["score_curve"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
            .unwrap_or_default();
        lines.push(format!(
            "{:<10} {:<16} 最佳 {:.2}  {:>3} 轮  {}  {}",
            r["id"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or(""),
            r["best_score"].as_f64().unwrap_or(0.0),
            r["iterations"].as_i64().unwrap_or(0),
            if curve.is_empty() {
                "        ".to_string()
            } else {
                sparkline(&curve)
            },
            ellipsis(r["intent"].as_str().unwrap_or(""), 40)
        ));
    }
    ctx.emit(lines.join("\n"), v)
}

// ---------------------------------------------------------------- pack

fn cmd_pack_init(ctx: &Ctx, slug: String, kind: String, out: PathBuf) -> Result<()> {
    if out.exists() {
        return Err(anyhow!("{} 已存在，请先删除或换 --out", out.display()));
    }
    let pack = json!({
        "spec": "rsi3d-pack/v1",
        "slug": slug,
        "kind": kind,
        "version": "0.1.0",
        "name": "",
        "summary": "",
        "domain": "general",
        "tags": [],
        "files": [],
        "createdAt": now_iso8601(),
    });
    std::fs::write(&out, format!("{}\n", serde_json::to_string_pretty(&pack)?))?;
    ctx.emit(
        format!(
            "✓ 已生成 {}\n  下一步  rsi3d pack build .   # 收集文件 + 逐文件 sha256\n          rsi3d pack sign            # HMAC 签名\n          rsi3d publish --file rsi3d.pack.json --kind {}",
            out.display(),
            kind
        ),
        pack,
    )
}

fn cmd_pack_build(
    ctx: &Ctx,
    dir: PathBuf,
    out: Option<PathBuf>,
    slug: Option<String>,
    kind: Option<String>,
    version: Option<String>,
) -> Result<()> {
    if !dir.is_dir() {
        return Err(anyhow!("{} 不是目录", dir.display()));
    }
    let pack_path = out.unwrap_or_else(|| dir.join("rsi3d.pack.json"));

    // 已有清单则作为模板（保留 name / summary / tags 等人工字段）
    let mut pack = if pack_path.exists() {
        serde_json::from_str::<Value>(&std::fs::read_to_string(&pack_path)?).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    if !pack.is_object() {
        pack = json!({});
    }
    let default_slug = dir
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "my-pack".into());
    {
        let obj = pack.as_object_mut().expect("已保证是对象");
        obj.entry("spec").or_insert(json!("rsi3d-pack/v1"));
        obj.entry("slug").or_insert(json!(default_slug));
        obj.entry("kind").or_insert(json!("bizpack"));
        obj.entry("version").or_insert(json!("0.1.0"));
    }
    if let Some(s) = slug {
        pack["slug"] = json!(s);
    }
    if let Some(k) = kind {
        pack["kind"] = json!(k);
    }
    if let Some(v) = version {
        pack["version"] = json!(v);
    }

    let mut paths: Vec<PathBuf> = Vec::new();
    collect_files(&dir, &pack_path, &mut paths)?;
    paths.sort();

    let mut files: Vec<Value> = Vec::new();
    let mut total: u64 = 0;
    for p in &paths {
        let rel = p
            .strip_prefix(&dir)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        let (sha, size) = sha256_file(p)?;
        total += size;
        files.push(json!({"path": rel, "sha256": sha, "size": size}));
    }

    pack["files"] = json!(files);
    pack["totalSize"] = json!(total);
    pack["fileCount"] = json!(files.len());
    pack["builtAt"] = json!(now_iso8601());
    if let Some(obj) = pack.as_object_mut() {
        // 重新构建后旧签名失效
        obj.remove("digest");
        obj.remove("signature");
        obj.remove("signedAt");
        obj.remove("signedBy");
    }

    let digest = pack_digest(&files);
    std::fs::write(&pack_path, format!("{}\n", serde_json::to_string_pretty(&pack)?))?;

    ctx.emit(
        format!(
            "✓ 打包完成 {}\n  文件   {} 个 · 合计 {}\n  digest sha256:{}\n  下一步 rsi3d pack sign --file {}",
            pack_path.display(),
            files.len(),
            human_size(total),
            digest,
            pack_path.display()
        ),
        json!({
            "pack": pack_path.display().to_string(),
            "fileCount": files.len(),
            "totalSize": total,
            "digest": digest,
        }),
    )
}

fn cmd_pack_sign(ctx: &Ctx, file: PathBuf, key: Option<PathBuf>) -> Result<()> {
    let mut pack: Value = serde_json::from_str(&std::fs::read_to_string(&file)?)
        .with_context(|| format!("{} 不是合法 JSON", file.display()))?;
    if pack["spec"].as_str() != Some("rsi3d-pack/v1") {
        return Err(anyhow!(
            "{} 的 spec 不是 rsi3d-pack/v1（用 `rsi3d pack init` 生成模板）",
            file.display()
        ));
    }
    let files = pack["files"].as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        return Err(anyhow!("清单里没有 files，先运行 `rsi3d pack build`"));
    }
    let digest = pack_digest(&files);
    let payload = pack_payload(&pack, &digest);
    let (key_hex, key_path) = signing_key(key.as_deref())?;
    let sig = util::hmac_sha256_hex(&util::hex_decode(&key_hex)?, payload.as_bytes());

    pack["digest"] = json!(digest);
    pack["signature"] = json!(format!("hmac-sha256:{}", sig));
    pack["signedAt"] = json!(now_iso8601());
    pack["signedBy"] = json!(ctx.cfg.email.clone().unwrap_or_else(|| "local".into()));
    std::fs::write(&file, format!("{}\n", serde_json::to_string_pretty(&pack)?))?;

    ctx.emit(
        format!(
            "✓ 已签名 {}\n  digest    sha256:{}\n  signature hmac-sha256:{}\n  密钥      {}（务必妥善备份；校验方需同一密钥）",
            file.display(),
            digest,
            sig,
            key_path.display()
        ),
        json!({"file": file.display().to_string(), "digest": digest, "signature": format!("hmac-sha256:{}", sig), "key": key_path.display().to_string()}),
    )
}

fn verify_pack(ctx: &Ctx, path: &Path, pack: &Value, key: Option<&Path>) -> Result<()> {
    let files = pack["files"].as_array().cloned().unwrap_or_default();
    let digest = pack_digest(&files);
    let declared = pack["digest"].as_str().unwrap_or("");
    let digest_ok = !declared.is_empty() && declared == digest;

    let payload = pack_payload(pack, &digest);
    let sig = pack["signature"].as_str().unwrap_or("").to_string();
    let (key_hex, _) = signing_key(key)?;
    let sign_ok = if let Some(want) = sig.strip_prefix("hmac-sha256:") {
        util::hmac_sha256_hex(&util::hex_decode(&key_hex)?, payload.as_bytes()) == want
    } else {
        false
    };

    // 如果文件就在清单旁边，顺带核对逐文件摘要
    let base = path.parent().unwrap_or(Path::new("."));
    let mut checked = 0usize;
    let mut mismatched: Vec<String> = Vec::new();
    for f in &files {
        let rel = f["path"].as_str().unwrap_or("");
        let fp = base.join(rel);
        if !fp.is_file() {
            continue;
        }
        let (sha, _) = sha256_file(&fp)?;
        checked += 1;
        if sha != f["sha256"].as_str().unwrap_or("") {
            mismatched.push(rel.to_string());
        }
    }

    let mut lines = vec![format!("{} 校验结果", path.display())];
    lines.push(format!(
        "  清单摘要 {}  {}",
        if digest_ok { "✓" } else { "✗" },
        if declared.is_empty() {
            "（清单未记录 digest）".to_string()
        } else {
            format!("declared sha256:{} · computed sha256:{}", declared, digest)
        }
    ));
    lines.push(format!(
        "  签名     {}  {}",
        if sign_ok { "✓" } else { "✗" },
        if sig.is_empty() {
            "（未签名）".to_string()
        } else {
            ellipsis(&sig, 40)
        }
    ));
    if checked > 0 {
        lines.push(format!(
            "  文件     {}  已核对 {}/{} 个{}",
            if mismatched.is_empty() { "✓" } else { "✗" },
            checked,
            files.len(),
            if mismatched.is_empty() {
                String::new()
            } else {
                format!("，不一致：{}", mismatched.join(", "))
            }
        ));
    }
    lines.push(format!("  签名者   {}", pack["signedBy"].as_str().unwrap_or("-")));

    let ok = digest_ok && sign_ok && mismatched.is_empty();
    if !ok {
        return Err(anyhow!("{}", lines.join("\n")));
    }
    ctx.emit(
        lines.join("\n"),
        json!({
            "file": path.display().to_string(),
            "digest": digest,
            "digestOk": digest_ok,
            "signatureOk": sign_ok,
            "filesChecked": checked,
            "mismatched": mismatched,
        }),
    )
}

/// 清单摘要：`path:sha256:size` 逐行排序后的 SHA-256（与文件顺序无关）。
fn pack_digest(files: &[Value]) -> String {
    let mut lines: Vec<String> = files
        .iter()
        .map(|f| {
            format!(
                "{}:{}:{}",
                f["path"].as_str().unwrap_or(""),
                f["sha256"].as_str().unwrap_or(""),
                f["size"].as_i64().unwrap_or(0)
            )
        })
        .collect();
    lines.sort();
    sha256_hex(lines.join("\n").as_bytes())
}

/// 签名载荷：spec:slug:version:digest。
fn pack_payload(pack: &Value, digest: &str) -> String {
    format!(
        "{}:{}:{}:{}",
        pack["spec"].as_str().unwrap_or(""),
        pack["slug"].as_str().unwrap_or(""),
        pack["version"].as_str().unwrap_or(""),
        digest
    )
}

/// 读取（或首次生成）本地签名密钥：~/.rsi3d/signing.key。
fn signing_key(explicit: Option<&Path>) -> Result<(String, PathBuf)> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => config::config_path()
            .parent()
            .map(|d| d.join("signing.key"))
            .unwrap_or_else(|| PathBuf::from("signing.key")),
    };
    if path.is_file() {
        let s = std::fs::read_to_string(&path)?;
        let s = s.trim().to_string();
        if !s.is_empty() {
            return Ok((s, path));
        }
    }
    let hex = util::rand_hex(32)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, format!("{}\n", hex))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok((hex, path))
}

/// 递归收集文件（跳过隐藏项、node_modules、target 与清单自身）。
fn collect_files(dir: &Path, skip: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || name == "target" || name == "dist" {
            continue;
        }
        let meta = entry.metadata()?;
        if meta.is_dir() {
            collect_files(&path, skip, out)?;
        } else if meta.is_file() && path != skip {
            out.push(path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- API-Key

fn cmd_key_list(ctx: &Ctx) -> Result<()> {
    let v = ctx.api_get_auth("/api/keys")?;
    let keys = v["keys"].as_array().cloned().unwrap_or_default();
    if keys.is_empty() {
        return ctx.emit("（还没有 API-Key；用 rsi3d key create --label ci 创建）".into(), v);
    }
    let human = keys
        .iter()
        .map(|k| {
            format!(
                "{:<12} {:<16} {}",
                k["id"].as_str().unwrap_or(""),
                k["prefix"].as_str().unwrap_or(""),
                k["label"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ctx.emit(format!("共 {} 个\n{}", keys.len(), human), v)
}

fn cmd_key_create(ctx: &Ctx, label: String) -> Result<()> {
    let v = ctx.api_post("/api/keys", json!({"label": label}))?;
    let secret = v["secret"].as_str().unwrap_or("");
    ctx.emit(
        format!(
            "✓ 已创建 API-Key {}\n  明文密钥（只显示这一次）\n    {}\n  用法  export RSI3D_TOKEN=… 或在 CI 里写 Authorization: Bearer <key>",
            v["key"]["id"].as_str().unwrap_or(""),
            secret
        ),
        v,
    )
}

fn cmd_key_revoke(ctx: &Ctx, id: String) -> Result<()> {
    let v = ctx.api_delete(&format!("/api/keys/{}", id))?;
    ctx.emit(format!("✓ 已撤销 {}", id), v)
}

// ---------------------------------------------------------------- Harness

fn cmd_harness_list(
    ctx: &Ctx,
    capability: Option<String>,
    domain: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    limit: usize,
) -> Result<()> {
    let mut qs: Vec<String> = Vec::new();
    if let Some(v) = capability {
        qs.push(format!("capability={}", urlencode(&v)));
    }
    if let Some(v) = domain {
        qs.push(format!("domain={}", urlencode(&v)));
    }
    if let Some(v) = q {
        qs.push(format!("q={}", urlencode(&v)));
    }
    if let Some(v) = sort {
        qs.push(format!("sort={}", urlencode(&v)));
    }
    qs.push(format!("limit={}", limit.clamp(1, 200)));
    let v = ctx.api_get(&format!("/api/harnesses?{}", qs.join("&")))?;
    let list = v["harnesses"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return ctx.emit("（没有匹配的 Harness）".into(), v);
    }
    let mut lines = vec![format!("共 {} 个 Harness", list.len())];
    for h in &list {
        let caps = h["capabilities"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        lines.push(format!(
            "{:<10} {:<28} 分 {:>5.1} {:<8} {:<5} {}",
            h["id"].as_str().unwrap_or(""),
            h["ref"].as_str().unwrap_or("-"),
            h["hub_score"].as_f64().unwrap_or(0.0),
            if h["verified"].as_bool().unwrap_or(false) {
                "已验证"
            } else {
                "未验证"
            },
            h["protocol"].as_str().unwrap_or(""),
            caps
        ));
        lines.push(format!(
            "           {} · {}",
            ellipsis(h["title"].as_str().unwrap_or(""), 30),
            h["domain"].as_str().unwrap_or("")
        ));
        lines.push(format!(
            "           {}",
            h["endpoint"].as_str().unwrap_or("")
        ));
    }
    ctx.emit(lines.join("\n"), v)
}

fn cmd_harness_show(ctx: &Ctx, reference: String) -> Result<()> {
    let h = resolve_harness(ctx, &reference)?;
    let caps = h["capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let human = format!(
        "{}\n  引用     {}\n  ID       {}\n  协议     {}\n  端点     {}\n  行业     {}\n  能力     {}\n  信誉分   {:.1}\n  已验证   {}\n  定价     {}",
        h["title"].as_str().unwrap_or(""),
        h["ref"].as_str().unwrap_or("-"),
        h["id"].as_str().unwrap_or(""),
        h["protocol"].as_str().unwrap_or(""),
        h["endpoint"].as_str().unwrap_or(""),
        h["domain"].as_str().unwrap_or(""),
        caps,
        h["hub_score"].as_f64().unwrap_or(0.0),
        h["verified"].as_bool().unwrap_or(false),
        h.get("pricing")
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into())
    );
    ctx.emit(human, h)
}

#[allow(clippy::too_many_arguments)]
fn cmd_harness_register(
    ctx: &Ctx,
    slug: String,
    title: Option<String>,
    endpoint: String,
    protocol: String,
    capabilities: Vec<String>,
    domain: Option<String>,
    pricing: Option<String>,
    summary: Option<String>,
    namespace: Option<String>,
) -> Result<()> {
    let pricing: Value = match &pricing {
        Some(s) => serde_json::from_str(s).context("--pricing 不是合法 JSON")?,
        None => Value::Null,
    };
    let mut body = json!({
        "slug": normalize_slug(&slug)?,
        "title": title.clone().unwrap_or_else(|| slug.clone()),
        "protocol": protocol,
        "endpoint": endpoint,
        "capabilities": capabilities,
        "domain": domain.unwrap_or_else(|| "general".into()),
        "summary": summary.unwrap_or_default(),
    });
    if !pricing.is_null() {
        body["pricing"] = pricing;
    }
    if let Some(ns) = namespace {
        body["namespace"] = json!(ns);
    }
    let v = ctx.api_post("/api/harnesses/register", body)?;
    ctx.emit(
        format!(
            "✓ 已注册 Harness {}\n  引用     {}\n  ID       {}\n  端点     {}\n  信誉分   {:.1}（初始 70，未验证）\n  能力声明 {}",
            v["harness"]["title"].as_str().unwrap_or(""),
            v["harness"]["ref"].as_str().unwrap_or("-"),
            v["harness"]["id"].as_str().unwrap_or(""),
            v["harness"]["endpoint"].as_str().unwrap_or(""),
            v["harness"]["hub_score"].as_f64().unwrap_or(70.0),
            v["artifact"]["ref"].as_str().unwrap_or("")
        ),
        v,
    )
}

fn cmd_harness_quote(ctx: &Ctx, id: String, intent: String, iters: usize) -> Result<()> {
    let v = ctx.api_post(
        &format!("/api/harnesses/{}/quote", id),
        json!({"intent": intent, "iters": iters}),
    )?;
    let q = &v["quote"];
    ctx.emit(
        format!(
            "✓ 报价 {}\n  金额     {} {}\n  单价     {} / {}\n  迭代     {}\n  有效期   至 {}\n  签名     {}\n  说明     平台不经手资金，成交与结算在供需双方之间",
            q["id"].as_str().unwrap_or(""),
            q["price"]["amount"].as_f64().unwrap_or(0.0),
            q["price"]["currency"].as_str().unwrap_or("USD"),
            q["price"]["amount"].as_f64().unwrap_or(0.0),
            q["price"]["unit"].as_str().unwrap_or("run"),
            q["price"]["iterations"].as_i64().unwrap_or(0),
            q["price"]["expires_at"].as_str().unwrap_or("-"),
            ellipsis(q["signature"].as_str().unwrap_or(""), 32)
        ),
        v,
    )
}

fn cmd_harness_feedback(
    ctx: &Ctx,
    id: String,
    delta: f64,
    comment: String,
    run: Option<String>,
) -> Result<()> {
    let v = ctx.api_post(
        &format!("/api/harnesses/{}/feedback", id),
        json!({"scoreDelta": delta, "comment": comment, "runId": run.unwrap_or_default()}),
    )?;
    ctx.emit(
        format!(
            "✓ 已提交反馈 {}\n  信誉分   {:.1}",
            id,
            v["harness"]["hub_score"].as_f64().unwrap_or(0.0)
        ),
        v,
    )
}

// ---------------------------------------------------------------- 插件与安装

/// 技能包里期望的清单文件名（按 kind 优先，其次通用名）。
const BUNDLE_MANIFESTS: [&str; 3] = ["skill.json", "plugin.json", "bundle.json"];

/// 制品类型（与平台 `/api/artifact-kinds` 一致）。
/// 引用里出现这些词就按「kind/slug」解释，而不是「命名空间/slug」。
const ARTIFACT_KINDS: [&str; 7] = [
    "bizpack", "svcpack", "harness", "plugin", "skill", "benchmark", "asset",
];

/// 从解开的包里找清单并校验。
fn manifest_of_entries(entries: &[bundle::Entry], kind: &str) -> Result<bundle::BundleManifest> {
    for want in BUNDLE_MANIFESTS {
        if let Some(e) = entries.iter().find(|e| e.name == want) {
            let m: bundle::BundleManifest = serde_json::from_slice(&e.bytes)
                .with_context(|| format!("{} 不是合法清单", want))?;
            m.validate(kind)?;
            return Ok(m);
        }
    }
    Err(anyhow!(
        "包里没有清单（{:?} 里要有一个）",
        BUNDLE_MANIFESTS
    ))
}

/// `--agent` 的技能目录。路径按各 Agent 公布的约定（VS Code 文档的 skills 表）：
/// 个人 `~/.claude/skills`、`~/.copilot/skills`、`~/.agents/skills`；
/// 项目 `.claude/skills`、`.github/skills`、`.agents/skills`。
fn skill_base(agent: &str, scope: &str, project: Option<&Path>) -> Result<PathBuf> {
    let a = agent.trim().to_lowercase();
    let (user_rel, project_rel) = match a.as_str() {
        "claude" | "claude-code" | "claudecode" => (".claude/skills", ".claude/skills"),
        "copilot" | "vscode" | "github" => (".copilot/skills", ".github/skills"),
        "agents" | "agent" | "generic" => (".agents/skills", ".agents/skills"),
        _ => {
            return Err(anyhow!(
                "不认识的 Agent: {}（支持 claude-code / copilot / agents）",
                agent
            ))
        }
    };
    match scope.trim() {
        "" | "user" => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("找不到 HOME；用 --dir 指定安装目录"))?;
            Ok(home.join(user_rel))
        }
        "project" => {
            let root = match project {
                Some(p) => p.to_path_buf(),
                None => std::env::current_dir()?,
            };
            Ok(root.join(project_rel))
        }
        s => Err(anyhow!("--scope 只能是 user / project，实际 {}", s)),
    }
}

fn cmd_install(
    ctx: &Ctx,
    reference: String,
    dir: Option<PathBuf>,
    agent: Option<String>,
    scope: String,
    project: Option<PathBuf>,
) -> Result<()> {
    let art = resolve_artifact(ctx, &reference)?;
    let slug = art["slug"].as_str().unwrap_or("artifact").to_string();
    let kind = art["kind"].as_str().unwrap_or("").to_string();
    if kind != "plugin" && kind != "skill" {
        ctx.note(format!(
            "! {} 的类型是 {}，不是 plugin/skill，仍继续安装",
            reference, kind
        ));
    }
    let expect = art["sha256"].as_str().unwrap_or("").to_string();
    let url = art["url"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        return Err(anyhow!("{} 没有可下载的字节地址（BYO URL 为空）", reference));
    }
    // 装到哪：--agent 直接进该 Agent 的技能目录；否则 --dir / ~/.rsi3d/installed
    let base = match (&agent, &dir) {
        (Some(a), _) => skill_base(a, &scope, project.as_deref())?,
        (None, Some(d)) => d.clone(),
        (None, None) => config::config_path()
            .parent()
            .map(|d| d.join("installed"))
            .unwrap_or_else(|| PathBuf::from("installed")),
    };
    std::fs::create_dir_all(&base).with_context(|| format!("创建目录失败: {}", base.display()))?;

    // 先落到临时文件：要判断这包到底是「一个文件」还是「一个包」，得先看到字节
    let tmp = base.join(format!(".rsi3d-download-{}.tmp", slug));
    ctx.note(format!("→ 下载 {}", url));
    let (size, got) = api::download_to(
        &ctx.cfg,
        ctx.cfg.token.as_deref(),
        &format!(
            "/api/artifacts/{}/download",
            art["id"].as_str().unwrap_or_default()
        ),
        &tmp,
    )?;
    if !expect.is_empty() && expect != got {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!(
            "校验和不匹配，安装中止\n  期望 {}\n  实际 {}",
            expect,
            got
        ));
    }
    let bytes = std::fs::read(&tmp).with_context(|| format!("读回下载文件失败: {}", tmp.display()))?;

    // 是包就解开（skill/plugin 都是包），否则按「单个文件」装（老行为，保持兼容）
    let is_bundle = bytes.starts_with(b"PK\x03\x04");
    if is_bundle {
        let entries = bundle::unzip(&bytes).with_context(|| format!("{} 解包失败", reference))?;
        let m = manifest_of_entries(&entries, if kind.is_empty() { "skill" } else { &kind })?;
        // 技能必须落在一个**以技能名命名的目录**里（SKILL.md 的 name 要与目录同名）
        let target_dir = base.join(&m.name);
        let files = bundle::unpack(&bytes, &target_dir)?;
        let entry_path = target_dir.join(&m.entry);
        if !entry_path.is_file() {
            return Err(anyhow!(
                "包里没有入口文件 {}（清单里写的）",
                m.entry
            ));
        }
        let _ = std::fs::remove_file(&tmp);

        let record = json!({
            "ref": art["ref"],
            "kind": kind,
            "version": art["version"],
            "sha256": got,
            "size": size,
            "bundle": {
                "name": m.name,
                "entry": m.entry,
                "files": m.files,
                "agents": m.agents,
            },
            "target": target_dir.display().to_string(),
            "installedAt": now_iso8601(),
        });
        std::fs::write(
            target_dir.join("rsi3d-install.json"),
            format!("{}\n", serde_json::to_string_pretty(&record)?),
        )?;

        let mut human = format!(
            "✓ 已安装 {} → {}\n  入口   {}（{} 个文件）\n  校验   {}",
            art["ref"].as_str().unwrap_or(""),
            target_dir.display(),
            entry_path.display(),
            files.len(),
            if expect.is_empty() {
                "制品未登记 sha256".to_string()
            } else {
                "sha256 通过".to_string()
            }
        );
        if let Some(a) = &agent {
            human.push_str(&format!("\n  生效   重启 {a} 会话后可用（技能目录已就位）"));
        }
        if let Some(mcp) = m.mcp.as_ref().and_then(|v| v.get("command")).and_then(|v| v.as_str()) {
            human.push_str(&format!(
                "\n  建议   把引擎接成 MCP 更省事：{} mcp --root <你的资产目录>",
                mcp
            ));
        }
        return ctx.emit(human, record);
    }

    // 单个文件（不是包）
    let target_dir = base.join(&slug);
    std::fs::create_dir_all(&target_dir)?;
    let dest = target_dir.join(out_filename(&art));
    std::fs::rename(&tmp, &dest).or_else(|_| {
        std::fs::copy(&tmp, &dest).map(|_| ()).and_then(|_| std::fs::remove_file(&tmp))
    })?;

    let record = json!({
        "ref": art["ref"],
        "kind": kind,
        "version": art["version"],
        "sha256": got,
        "size": size,
        "file": dest.display().to_string(),
        "installedAt": now_iso8601(),
    });
    std::fs::write(
        target_dir.join("rsi3d-install.json"),
        format!("{}\n", serde_json::to_string_pretty(&record)?),
    )?;

    ctx.emit(
        format!(
            "✓ 已安装 {} → {}\n  文件   {}（{}）\n  校验   {}",
            art["ref"].as_str().unwrap_or(""),
            target_dir.display(),
            dest.display(),
            human_size(size),
            if expect.is_empty() {
                "制品未登记 sha256".to_string()
            } else {
                "sha256 通过".to_string()
            }
        ),
        record,
    )
}

fn cmd_skill_inspect(ctx: &Ctx, file: PathBuf) -> Result<()> {
    let bytes = std::fs::read(&file).with_context(|| format!("读取失败: {}", file.display()))?;
    let entries = bundle::unzip(&bytes).with_context(|| format!("{} 不是可读的包", file.display()))?;
    // 清单在不在、自不自洽（kind 未知时用目录里的清单自己声明的那份来校验）
    let m = manifest_of_entries(&entries, "skill")
        .or_else(|_| manifest_of_entries(&entries, "plugin"))?;
    let total: usize = entries.iter().map(|e| e.bytes.len()).sum();
    let mut lines = vec![format!(
        "包 {}（{} 个条目，解开后 {}）",
        file.display(),
        entries.len(),
        human_size(total as u64)
    )];
    lines.push(format!(
        "  清单   {} · {} · {}",
        m.name,
        if m.kind.is_empty() { "skill" } else { &m.kind },
        if m.title.is_empty() { "-" } else { &m.title }
    ));
    lines.push(format!("  入口   {}", m.entry));
    for e in &entries {
        let mark = if e.name == m.entry { "← 入口" } else { "" };
        lines.push(format!("  {:>8}  {} {}", human_size(e.bytes.len() as u64), e.name, mark));
    }
    // 清单声明了、包里没有 → 这包是坏的，得说出来
    let missing: Vec<&String> = m
        .files
        .iter()
        .filter(|f| !entries.iter().any(|e| &e.name == *f))
        .collect();
    if !missing.is_empty() {
        return Err(anyhow!(
            "清单声明了但包里没有：{}",
            missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    lines.push("  校验   每个条目的 CRC 都过了（store/deflate 都能读）".to_string());
    let record = json!({
        "name": m.name,
        "kind": m.kind,
        "entry": m.entry,
        "files": entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
        "bytes": bytes.len(),
        "sha256": sha256_hex(&bytes),
    });
    ctx.emit(lines.join("\n"), record)
}

/// 只打包不发网络：发布前想看包长什么样、或手工上传时用。
fn cmd_skill_pack(ctx: &Ctx, dir: PathBuf, out: Option<PathBuf>, kind: String) -> Result<()> {
    let (zip, m) = bundle::pack_dir(&dir, &kind)
        .with_context(|| format!("打包失败: {}", dir.display()))?;
    let dest = out.unwrap_or_else(|| PathBuf::from(format!("{}.zip", m.name)));
    std::fs::write(&dest, &zip).with_context(|| format!("写包失败: {}", dest.display()))?;
    let sha = sha256_hex(&zip);
    let record = json!({
        "name": m.name,
        "kind": kind,
        "entry": m.entry,
        "files": m.files,
        "sha256": sha,
        "size": zip.len(),
        "file": dest.display().to_string(),
    });
    ctx.emit(
        format!(
            "✓ 已打包 {} → {}\n  入口   {}\n  文件   {} 个（{}）\n  sha256 {}\n  发布   rsi3d publish --dir {} --kind {}",
            m.name,
            dest.display(),
            m.entry,
            m.files.len() + 1,
            human_size(zip.len() as u64),
            sha,
            dir.display(),
            kind
        ),
        record,
    )
}

fn cmd_plugins(ctx: &Ctx, agent: Option<String>) -> Result<()> {
    let path = match &agent {
        Some(a) => format!("/api/plugins?agent={}", urlencode(a)),
        None => "/api/plugins".to_string(),
    };
    let v = ctx.api_get(&path)?;
    let items = v["plugins"].as_array().cloned().unwrap_or_default();
    let mut lines = vec![format!("可接入的 Agent 形态（{}）", items.len())];
    for p in &items {
        lines.push(format!(
            "{:<18} {:<12} {:<8} {}",
            p["name"].as_str().unwrap_or(""),
            p["agent"].as_str().unwrap_or(""),
            p["status"].as_str().unwrap_or(""),
            p["install"].as_str().unwrap_or("")
        ));
        lines.push(format!("    {}", ellipsis(p["desc"].as_str().unwrap_or(""), 72)));
    }
    ctx.emit(lines.join("\n"), v)
}

// ---------------------------------------------------------------- 共用的解析与工具

impl Ctx {
    /// 上传制品字节（multipart），返回 {url, sha256, size, name}。
    fn api_post_upload(&self, filename: &str, bytes: &[u8]) -> Result<Value> {
        api::upload_file(&self.cfg, &self.token()?, "/api/uploads", filename, bytes)
    }
}

/// 找到制品：限定命名空间（可选）与 slug（精确），返回完整详情（含 versions）。
fn find_artifact(
    ctx: &Ctx,
    ns: Option<&str>,
    slug: &str,
    kind: Option<&str>,
    mine: bool,
) -> Result<Option<Value>> {
    let mut qs = vec![
        format!("q={}", urlencode(slug)),
        "limit=200".to_string(),
    ];
    if mine {
        qs.push("mine=1".to_string());
    }
    if let Some(n) = ns {
        qs.push(format!("namespace={}", urlencode(n)));
    }
    if let Some(k) = kind {
        qs.push(format!("kind={}", urlencode(k)));
    }
    let v = ctx.api_get(&format!("/api/artifacts?{}", qs.join("&")))?;
    let items = v["artifacts"].as_array().cloned().unwrap_or_default();
    let hit = items
        .iter()
        .find(|a| {
            a["slug"].as_str() == Some(slug)
                && ns
                    .map(|n| a["namespace"]["slug"].as_str() == Some(n))
                    .unwrap_or(true)
        })
        .cloned();
    match hit {
        Some(a) => {
            // 列表视图没有 versions，详情再取一次
            let id = a["id"].as_str().unwrap_or_default();
            let full = ctx.api_get(&format!("/api/artifacts/{}", id))?;
            Ok(full.get("artifact").cloned().or(Some(a)))
        }
        None => Ok(None),
    }
}

/// 解析制品引用：`@ns/slug` / `ns/slug` / `A-xxx` / 裸 slug。
fn resolve_artifact(ctx: &Ctx, reference: &str) -> Result<Value> {
    let r = reference.trim();
    if r.is_empty() {
        return Err(anyhow!("制品引用不能为空"));
    }
    if let Some(rest) = r.strip_prefix('@') {
        let (ns, slug) = split_ref(rest)?;
        return find_artifact(ctx, Some(&ns), &slug, None, false)?
            .ok_or_else(|| anyhow!("未找到制品 @{}/{}", ns, slug));
    }
    // kind/slug：插件页与文档里给的就是这种写法（skill/rsi3d、plugin/foo）。
    // 注意要放在「裸 ns/slug」之前判，否则会把 skill 当成一个命名空间。
    if let Some((head, tail)) = r.split_once('/') {
        if ARTIFACT_KINDS.contains(&head) {
            if let Some(a) = find_artifact(ctx, None, tail, Some(head), false)? {
                return Ok(a);
            }
            return Err(anyhow!(
                "未找到 {} / {}（试试 rsi3d search --kind {}）",
                head,
                tail,
                head
            ));
        }
    }
    if r.contains('/') {
        let (ns, slug) = split_ref(r)?;
        return find_artifact(ctx, Some(&ns), &slug, None, false)?
            .ok_or_else(|| anyhow!("未找到制品 {}/{}", ns, slug));
    }
    if r.starts_with("A-") {
        let v = ctx.api_get(&format!("/api/artifacts/{}", r))?;
        if v.get("artifact").map(|a| !a.is_null()).unwrap_or(false) {
            return Ok(v["artifact"].clone());
        }
        return Err(anyhow!("制品不存在: {}", r));
    }
    // 裸 slug：自己的优先（可能还没公开），再回到公共目录
    for mine in [true, false] {
        let mine_q = if mine { "&mine=1" } else { "" };
        let v = ctx.api_get(&format!("/api/artifacts?q={}&limit=200{}", urlencode(r), mine_q))?;
        let items = v["artifacts"].as_array().cloned().unwrap_or_default();
        if let Some(hit) = items.iter().find(|a| a["slug"].as_str() == Some(r)) {
            let id = hit["id"].as_str().unwrap_or_default().to_string();
            let full = ctx.api_get(&format!("/api/artifacts/{}", id))?;
            return Ok(full.get("artifact").cloned().unwrap_or_else(|| hit.clone()));
        }
        if items.len() == 1 {
            let id = items[0]["id"].as_str().unwrap_or_default().to_string();
            let full = ctx.api_get(&format!("/api/artifacts/{}", id))?;
            return Ok(full.get("artifact").cloned().unwrap_or_else(|| items[0].clone()));
        }
        if items.len() > 1 {
            return Err(anyhow!(
                "{} 匹配到多个制品（{} 个），请用 @ns/slug 明确指定",
                r,
                items.len()
            ));
        }
    }
    Err(anyhow!("未找到制品 {}（试试 rsi3d search）", r))
}

/// 解析 Harness：`H-xxx` / `@ns/slug` / 裸 slug。
fn resolve_harness(ctx: &Ctx, reference: &str) -> Result<Value> {
    let r = reference.trim();
    if r.starts_with("H-") {
        let v = ctx.api_get(&format!("/api/harnesses/{}", r))?;
        if v.get("harness").map(|h| !h.is_null()).unwrap_or(false) {
            return Ok(v["harness"].clone());
        }
        return Err(anyhow!("Harness 不存在: {}", r));
    }
    // 先按制品找到候选，再用 artifact_id 关联到 Harness
    let art = resolve_artifact(ctx, r)?;
    let art_id = art["id"].as_str().unwrap_or_default().to_string();
    let list = ctx.api_get("/api/harnesses?limit=200")?;
    if let Some(arr) = list["harnesses"].as_array() {
        if let Some(h) = arr
            .iter()
            .find(|h| h["artifact_id"].as_str() == Some(art_id.as_str()))
        {
            return Ok(h.clone());
        }
    }
    Err(anyhow!(
        "{} 是制品但没有对应的已发布 Harness（用 rsi3d harness register 注册）",
        r
    ))
}

fn split_ref(s: &str) -> Result<(String, String)> {
    match s.split_once('/') {
        Some((ns, slug)) if !ns.is_empty() && !slug.is_empty() => {
            Ok((ns.trim().to_string(), slug.trim().to_string()))
        }
        _ => Err(anyhow!("引用格式应为 @命名空间/slug，例如 @you/home-display")),
    }
}

/// 校验并规范化 slug。
fn normalize_slug(s: &str) -> Result<String> {
    let slug = s.trim().to_lowercase();
    let valid = slug.len() >= 2
        && slug.len() <= 40
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid {
        return Err(anyhow!(
            "slug 「{}」不合法：只能包含小写字母、数字、-、_，长度 2-40",
            s
        ));
    }
    Ok(slug)
}

/// 文件名 → slug 候选（去掉扩展名并做字符替换）。
fn strip_extension(name: &str) -> String {
    let stem = match name.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem,
        _ => name,
    };
    let mut out = String::with_capacity(stem.len());
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// 下载文件名：优先 manifest 里的原始文件名，否则 `slug-version.ext`。
fn out_filename(art: &Value) -> String {
    if let Some(n) = art
        .pointer("/manifest/file/name")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    {
        return n.to_string();
    }
    let slug = art["slug"].as_str().unwrap_or("artifact");
    let version = art["version"].as_str().unwrap_or("0.0.0");
    let ext = art["url"].as_str().map(url_ext).unwrap_or_default();
    format!("{}-{}{}", slug, version, ext)
}

fn url_ext(url: &str) -> String {
    let base = url
        .split('?')
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    match base.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 8 => format!(".{}", ext),
        _ => String::new(),
    }
}

/// 命名空间摘要（`@a, @b`）。
fn ns_summary(v: &Value) -> String {
    v["namespaces"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|n| n["slug"].as_str())
                .map(|s| format!("@{}", s))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn fmt_cost(ms: i64) -> String {
    if ms <= 0 {
        "—".to_string()
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// 口令来源：`--password` > 环境变量 `RSI3D_PASSWORD`。
fn password_arg(pw: Option<String>) -> Result<String> {
    if let Some(p) = pw {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    if let Ok(p) = std::env::var("RSI3D_PASSWORD") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    Err(anyhow!(
        "缺少密码：请用 --password，或设置环境变量 RSI3D_PASSWORD（更安全，不会出现在进程列表里）"
    ))
}

fn save_session(ctx: &mut Ctx, v: &Value) -> Result<()> {
    let token = v["token"]
        .as_str()
        .ok_or_else(|| anyhow!("服务端未返回 token"))?;
    ctx.cfg.token = Some(token.to_string());
    ctx.cfg.email = v["user"]["email"].as_str().map(|s| s.to_string());
    ctx.cfg.name = v["user"]["name"]
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    config::save(&ctx.cfg)?;
    Ok(())
}

fn err_contains(e: &anyhow::Error, needle: &str) -> bool {
    format!("{:#}", e).contains(needle)
}
