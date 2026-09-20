//! `serve` 与 `stream`：远程渲染/转流的两个入口（服务端 + 客户端）。
//!
//! 为什么两者都在 CLI 里：
//! - `serve` 是**在客户边界内**跑的那个进程（本机/容器/内网机器）；
//! - `stream` 是**不用浏览器**的客户端：CI 里录帧、脚本里导出 glTF、验证续传。
//!
//! 两个入口共用 `crates/serve`（传输）与 `crates/stream`（协议/会话），
//! 所以浏览器看到的东西与命令行看到的东西**逐字段一致**。

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::time::Duration;

use rsi3d_harness_serve as serve;
use rsi3d_harness_stream as stream;

use crate::{emit, load_document, Cli};

/// 读与文档同名的几何 side-car（`<stem>.mesh.glb`）。
///
/// 命名规则与 `scene import` 写出来的一致——**同一目录、同一前缀**，所以这里不用任何配置，
/// 也不需要场景里写路径（场景里的 `mesh_ref` 只是文件名，不参与寻址）。
///
/// 读不到 / 不是 GLB 一律返回 `None`：这条链上"没有真网格"是正常状态（手搭场景就是），
/// 不该让 `serve` 起不来。
fn load_mesh_sidecar(doc: &Path) -> Option<Vec<u8>> {
    let stem = doc.file_stem()?.to_string_lossy().to_string();
    let sidecar = doc.with_file_name(format!("{}.mesh.glb", stem));
    let bytes = std::fs::read(&sidecar).ok()?;
    if bytes.len() < 12 || &bytes[0..4] != b"glTF" {
        eprintln!(
            "⚠ {} 不是 GLB（magic 不对），忽略：浏览器会画包围盒代理",
            sidecar.display()
        );
        return None;
    }
    eprintln!(
        "几何 side-car：{}（{} KB，浏览器据此显示真网格）",
        sidecar.display(),
        bytes.len() / 1024
    );
    Some(bytes)
}

/// 启动转流服务。
///
/// 安全默认：只绑回环 + 必须带一次性 token。绑到非回环地址时**大声警告**，
/// 因为那等于把这个场景（可能是客户资产）暴露到网络上。
pub(crate) fn cmd_serve(
    cli: &Cli,
    file: &Path,
    bind: &str,
    port: u16,
    fps: u32,
    token: Option<&str>,
    open: bool,
    policy: Option<&Path>,
) -> Result<()> {
    let doc = load_document(file)?;
    let mut opts = serve::ServeOptions::new(doc)
        .with_bind(bind)
        .with_port(port)
        .with_fps(fps);
    if let Some(t) = token {
        opts = opts.with_token(t);
    }
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "scene".to_string());
    opts.name = name;

    // 几何 side-car：与文档同名的 `<stem>.mesh.glb`。
    // 有就带上（浏览器能把包围盒换成真网格），没有就什么都不说——**不是错误**：
    // 手搭的场景本来就没有几何文件。
    if let Some(bytes) = load_mesh_sidecar(file) {
        opts = opts.with_mesh(bytes);
    }

    // 判定用的规则表：给了就用它（平台 `/api/render/policy` 发的那份），
    // 没给就用内置的——**同一条链**，不是两套阈值。
    if let Some(p) = policy {
        let text = std::fs::read_to_string(p)
            .with_context(|| format!("读不到规则表 {}", p.display()))?;
        let parsed: rsi3d_harness_stream::render_mode::RenderPolicy = serde_json::from_str(&text)
            .with_context(|| format!("{} 不是合法的规则表", p.display()))?;
        eprintln!(
            "渲染模式规则表：{}（版本 {}）——/capability 用它判定",
            p.display(),
            parsed.version
        );
        opts = opts.with_policy(parsed);
    }

    let handle = serve::serve(opts).map_err(|e| anyhow::anyhow!(e))?;
    let base = handle.base_url();

    if handle.is_public() {
        eprintln!(
            "⚠  绑到了 {}（非回环）：这个地址上的任何人都能连进来。\n\
             ⚠  请确认它在可信网络里，或改回 --bind 127.0.0.1。\n\
             ⚠  另外：HTTP 是明文，跨网请放在 TLS 后面。",
            handle.addr
        );
    }

    let mut human = String::new();
    human.push_str(&format!("服务已启动  {}\n", base));
    human.push_str(&format!("访问令牌      {}\n", handle.token));
    human.push_str(&format!("客户端页面    {}\n", handle.url()));
    human.push_str("\n两条流（都是服务端单向推，客户端用 POST 改东西）：\n");
    human.push_str(&format!(
        "  场景流（客户端渲染）  {}/stream/scene?token={}\n",
        base, handle.token
    ));
    human.push_str(&format!(
        "  图像流（服务端渲染）  {}/stream/frame?token={}&view=iso-sw\n",
        base, handle.token
    ));
    human.push_str("\n命令行也能看（不需要浏览器）：\n");
    human.push_str(&format!(
        "  curl -N '{}/stream/frame?token={}&view=top' | head -c 400\n",
        base, handle.token
    ));
    human.push_str(&format!(
        "  rsi3d-harness stream {} --token {} --kind frame --out /tmp/frames\n",
        base, handle.token
    ));
    human.push_str(&format!(
        "  rsi3d-harness stream {} --token {} --kind scene --out /tmp/scene\n",
        base, handle.token
    ));
    human.push_str("\n停止：Ctrl-C\n");

    let value = serde_json::json!({
        "ok": true,
        "url": base,
        "client_url": handle.url(),
        "token": handle.token,
        "addr": handle.addr.to_string(),
        "public": handle.is_public(),
        "endpoints": {
            "scene": format!("{}/stream/scene", base),
            "frame": format!("{}/stream/frame", base),
            "command": format!("{}/command", base),
            "observe": format!("{}/observe", base),
            "gltf": format!("{}/snapshot.gltf", base),
            "healthz": format!("{}/healthz", base),
        },
        "protocol": stream::STREAM_PROTOCOL,
    });
    emit(cli, human, value)?;

    if open {
        let url = handle.url();
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        match std::process::Command::new(opener).arg(&url).spawn() {
            Ok(_) => eprintln!("已尝试用浏览器打开 {}", url),
            Err(e) => eprintln!("打不开浏览器（{}）：请手动访问 {}", e, url),
        }
    }

    handle.wait();
    Ok(())
}

/// 订阅远端流并落盘。
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_stream(
    cli: &Cli,
    url: &str,
    token: &str,
    kind: &str,
    view: &str,
    out: Option<&Path>,
    from: Option<u32>,
    limit: u64,
    idle_secs: u64,
    retries: u32,
) -> Result<()> {
    let kind = stream::StreamKind::parse(kind)
        .ok_or_else(|| anyhow::anyhow!("不认识的流类型「{}」；可用 scene | frame", kind))?;

    if let Some(dir) = out {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("建不了输出目录 {}", dir.display()))?;
    }

    let mut lines: Vec<String> = Vec::new();
    let mut received: u64 = 0;
    let mut frames: u64 = 0;
    let mut patches: u64 = 0;
    let mut snapshots: u64 = 0;
    let mut resumed_from: Option<u32> = from;
    let mut last_id: Option<u32> = None;
    let mut attempt: u32 = 0;

    'outer: loop {
        // 命令行客户端老实声明自己是什么：没有屏幕，只能消费流（不能自己渲染、
        // 也不会处理 GPU 上下文丢失——它根本没有 GPU）。服务端据此知道对面是谁。
        let client = stream::ClientDeclaration {
            agent: format!("rsi3d-cli/{}", env!("CARGO_PKG_VERSION")),
            capabilities: vec![
                "headless".to_string(),
                match kind {
                    stream::StreamKind::Scene => "scene".to_string(),
                    stream::StreamKind::Frame => "image".to_string(),
                },
            ],
            // 不声明显示预算：命令行落盘，要的就是服务端默认档
            frame_budget: None,
        };
        let path = serve::peer::stream_path_with_client(
            kind,
            if kind == stream::StreamKind::Frame { Some(view) } else { None },
            token,
            resumed_from,
            &client,
        );
        let mut peer = match serve::peer::connect_with_timeout(
            url,
            &path,
            Duration::from_secs(idle_secs.max(1)),
        ) {
            Ok(p) => p,
            Err(e) => {
                if attempt < retries {
                    attempt += 1;
                    lines.push(format!("连接失败（第 {} 次）：{}；稍后重试", attempt, e));
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
                bail!("{}", e);
            }
        };
        if peer.status != 200 {
            bail!("服务返回 {}（令牌不对或地址不对？）", peer.status);
        }
        lines.push(format!(
            "已连接 {}（{}）{}",
            url,
            if kind == stream::StreamKind::Frame { "图像流" } else { "场景流" },
            match resumed_from {
                Some(r) => format!("，带 Last-Event-ID={} 续传", r),
                None => String::new(),
            }
        ));

        loop {
            let msg = match peer.next_message() {
                Ok(Some(m)) => m,
                Ok(None) => {
                    lines.push("流结束".to_string());
                    break 'outer;
                }
                // 空闲不是错误：静止场景服务端**本就不该发东西**（只推变化）。
                // 对收帧的人来说这是个坑，所以这里把原因说清楚再正常收工。
                Err(serve::peer::PeerError::Idle) => {
                    lines.push(format!(
                        "{} 秒没有新消息，收工。静止场景服务端不重发（图像流只在像素变化时推、\
                         场景流只在状态变化时推）——想持续收帧就先让场景动起来。",
                        idle_secs
                    ));
                    break 'outer;
                }
                Err(serve::peer::PeerError::Fatal(e)) => {
                    lines.push(format!("读流中断：{}", e));
                    break;
                }
            };
            received += 1;
            if let Some(id) = peer.last_id {
                last_id = Some(id);
            }

            // 实时进度走 stderr：stdout 留给最后的报告（管道里只想要结果）
            match &msg {
                stream::ServerMessage::Frame { revision, view, .. } => eprintln!(
                    "  帧 #{} rev {} {}",
                    frames + 1,
                    revision,
                    view
                ),
                stream::ServerMessage::Patch { from, to, .. } => {
                    eprintln!("  增量 {} → {}", from, to)
                }
                stream::ServerMessage::Snapshot { revision, .. } => {
                    eprintln!("  全量 rev {}", revision)
                }
                _ => {}
            }

            match &msg {
                stream::ServerMessage::Welcome { revision, resumed, geometry, scene_hash, .. } => {
                    lines.push(format!(
                        "握手：rev {} · hash {} · 几何 {} · 会话{}",
                        revision,
                        &scene_hash[..scene_hash.len().min(8)],
                        geometry,
                        if *resumed { "续传" } else { "新建" }
                    ));
                }
                stream::ServerMessage::Snapshot { revision, gltf, .. } => {
                    snapshots += 1;
                    let nodes = gltf["nodes"].as_array().map(|a| a.len()).unwrap_or(0);
                    if let Some(dir) = out {
                        let p = dir.join("snapshot.json");
                        std::fs::write(&p, serde_json::to_vec_pretty(gltf)?)
                            .with_context(|| format!("写不到 {}", p.display()))?;
                        lines.push(format!(
                            "全量 rev {}：{} 个节点 → {}",
                            revision,
                            nodes,
                            p.display()
                        ));
                    } else {
                        lines.push(format!("全量 rev {}：{} 个节点", revision, nodes));
                    }
                }
                stream::ServerMessage::Patch { from, to, changes, .. } => {
                    patches += 1;
                    let up = changes["nodes_upsert"].as_array().map(|a| a.len()).unwrap_or(0);
                    let rm = changes["nodes_remove"].as_array().map(|a| a.len()).unwrap_or(0);
                    lines.push(format!(
                        "增量 {} → {}：改 {} 删 {}，挡窗者 {}",
                        from,
                        to,
                        up,
                        rm,
                        changes["blockers"]
                    ));
                }
                stream::ServerMessage::Frame { revision, view, renderer, image_hash, png_base64, band_occlusion, .. } => {
                    frames += 1;
                    // 落盘的帧必须能说清"谁渲的、能不能当证据"：非证据档就当场说出来，
                    // 而不是让人以后拿它去对账时才发现对不上
                    let grade = if stream::is_evidence_renderer(&renderer) {
                        format!("{}（证据档）", renderer)
                    } else {
                        format!("{}（**非证据档**：不可复现，不得用于验收）", renderer)
                    };
                    if let Some(dir) = out {
                        let p = dir.join(format!("frame-{:04}.png", frames));
                        let bytes = stream::session::decode_b64(png_base64)
                            .ok_or_else(|| anyhow::anyhow!("帧的 base64 解不开"))?;
                        std::fs::write(&p, bytes)
                            .with_context(|| format!("写不到 {}", p.display()))?;
                        lines.push(format!(
                            "帧 #{} rev {} {} · {} · sha {} · 遮挡 {} → {}",
                            frames,
                            revision,
                            view,
                            grade,
                            &image_hash[..8],
                            band_occlusion
                                .map(|o| format!("{:.0}%", o * 100.0))
                                .unwrap_or_else(|| "–".into()),
                            p.display()
                        ));
                    } else {
                        lines.push(format!("帧 #{} rev {} {} · {}", frames, revision, view, grade));
                    }
                }
                stream::ServerMessage::Pong { nonce } => lines.push(format!("pong {}", nonce)),
                stream::ServerMessage::Error { code, message } => {
                    lines.push(format!("服务端错误 {}：{}", code, message))
                }
                stream::ServerMessage::Bye { reason } => {
                    lines.push(format!("服务端关闭：{}", reason));
                    break 'outer;
                }
            }

            if limit > 0 && received >= limit {
                lines.push(format!("已达 --limit {}，退出", limit));
                break 'outer;
            }
        }

        // 断了：带 Last-Event-ID 重连，服务端只补差量
        if attempt >= retries {
            lines.push("重连次数用尽".to_string());
            break;
        }
        attempt += 1;
        resumed_from = last_id;
        std::thread::sleep(Duration::from_millis(300));
    }

    let human = format!(
        "{}\n\n收到 {} 条（全量 {} · 增量 {} · 帧 {}）",
        lines.join("\n"),
        received,
        snapshots,
        patches,
        frames
    );
    let value = serde_json::json!({
        "ok": true,
        "url": url,
        "kind": kind.as_str(),
        "received": received,
        "snapshots": snapshots,
        "patches": patches,
        "frames": frames,
        "last_event_id": last_id,
        "out": out.map(|p| p.display().to_string()),
    });
    emit(cli, human, value)
}
