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
        let path = serve::peer::stream_path(
            kind,
            if kind == stream::StreamKind::Frame { Some(view) } else { None },
            token,
            resumed_from,
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
                stream::ServerMessage::Frame { revision, view, image_hash, png_base64, band_occlusion, .. } => {
                    frames += 1;
                    if let Some(dir) = out {
                        let p = dir.join(format!("frame-{:04}.png", frames));
                        let bytes = stream::session::decode_b64(png_base64)
                            .ok_or_else(|| anyhow::anyhow!("帧的 base64 解不开"))?;
                        std::fs::write(&p, bytes)
                            .with_context(|| format!("写不到 {}", p.display()))?;
                        lines.push(format!(
                            "帧 #{} rev {} {} · sha {} · 遮挡 {} → {}",
                            frames,
                            revision,
                            view,
                            &image_hash[..8],
                            band_occlusion
                                .map(|o| format!("{:.0}%", o * 100.0))
                                .unwrap_or_else(|| "–".into()),
                            p.display()
                        ));
                    } else {
                        lines.push(format!("帧 #{} rev {} {}", frames, revision, view));
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
