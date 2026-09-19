//! HTTP 客户端：全部走公开的 /api/… 协议，不依赖平台私有代码。

use anyhow::{anyhow, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;

use crate::config::CliConfig;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .redirects(10)
        .build()
}

/// 发一次请求。extra_headers 用于 X-Run-Token 之类的额外头。
pub fn request(
    cfg: &CliConfig,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    extra_headers: &[(&str, &str)],
) -> Result<Value> {
    let url = format!("{}{}", cfg.base_url.trim_end_matches('/'), path);
    let a = agent();
    let mut req = match method {
        "GET" => a.get(&url),
        "POST" => a.post(&url),
        "PATCH" => a.request("PATCH", &url),
        "DELETE" => a.request("DELETE", &url),
        m => return Err(anyhow!("不支持的 HTTP 方法: {}", m)),
    };

    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {}", t));
    }
    for (k, v) in extra_headers {
        req = req.set(k, v);
    }

    let resp = match body {
        Some(b) => req.set("Content-Type", "application/json").send_json(b),
        None => req.call(),
    };

    map_resp(resp)
}

/// 统一处理响应：成功取 JSON（非 JSON 体回落为 Null），失败抽出服务端错误信封。
fn map_resp(resp: std::result::Result<ureq::Response, ureq::Error>) -> Result<Value> {
    match resp {
        Ok(r) => Ok(r.into_json::<Value>().unwrap_or(Value::Null)),
        Err(ureq::Error::Status(code, r)) => {
            let v: Value = r.into_json().unwrap_or(Value::Null);
            let msg = v
                .pointer("/error/message")
                .and_then(|x| x.as_str())
                .unwrap_or("请求失败")
                .to_string();
            let code_s = v
                .pointer("/error/code")
                .and_then(|x| x.as_str())
                .unwrap_or("request_failed")
                .to_string();
            Err(anyhow!("[{}] {} ({})", code, code_s, msg))
        }
        Err(e) => Err(anyhow!("网络错误: {}", e)),
    }
}

/// 直接取响应体字节（下载用；支持任意 Content-Type）。
fn map_resp_reader(resp: std::result::Result<ureq::Response, ureq::Error>) -> Result<Box<dyn Read + Send + Sync>> {
    match resp {
        Ok(r) => Ok(r.into_reader()),
        Err(ureq::Error::Status(code, r)) => {
            let v: Value = r.into_json().unwrap_or(Value::Null);
            let msg = v
                .pointer("/error/message")
                .and_then(|x| x.as_str())
                .unwrap_or("下载失败")
                .to_string();
            Err(anyhow!("[{}] {}", code, msg))
        }
        Err(e) => Err(anyhow!("网络错误: {}", e)),
    }
}

/// GET 便捷封装。
pub fn get(cfg: &CliConfig, path: &str) -> Result<Value> {
    request(cfg, "GET", path, None, None, &[])
}

/// 带 token 的 GET。
pub fn get_authed(cfg: &CliConfig, token: &str, path: &str) -> Result<Value> {
    request(cfg, "GET", path, Some(token), None, &[])
}

// ---------------- 常用方法封装 ----------------

/// 匿名 POST（register / login）。
pub fn post_anon(cfg: &CliConfig, path: &str, body: Value) -> Result<Value> {
    request(cfg, "POST", path, None, Some(body), &[])
}

/// 带 token 的 POST。
pub fn post(cfg: &CliConfig, token: &str, path: &str, body: Value) -> Result<Value> {
    request(cfg, "POST", path, Some(token), Some(body), &[])
}

/// 带 token 的 PATCH。
pub fn patch(cfg: &CliConfig, token: &str, path: &str, body: Value) -> Result<Value> {
    request(cfg, "PATCH", path, Some(token), Some(body), &[])
}

/// 带 token 的 DELETE。
pub fn delete(cfg: &CliConfig, token: &str, path: &str) -> Result<Value> {
    request(cfg, "DELETE", path, Some(token), None, &[])
}

/// 直连数据面：调用 Harness 端点（平台不中转业务数据——这是控制面/数据面的分界）。
pub fn call_harness(endpoint: &str, body: &Value) -> Result<Value> {
    let resp = agent()
        .post(endpoint)
        .set("Content-Type", "application/json")
        .send_json(body.clone());
    map_resp(resp)
}

// ---------------- 上传与下载 ----------------

/// 手动构造 multipart/form-data 上传（ureq 的 multipart 特性会引入 multer，这里手写更省）。
pub fn upload_file(cfg: &CliConfig, token: &str, path: &str, filename: &str, bytes: &[u8]) -> Result<Value> {
    let boundary = format!("----rsi3d{}", crate::util::rand_hex(16)?);
    let mut body = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
            filename.replace(['"', '\r', '\n'], "_")
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!("Content-Type: {}\r\n\r\n", crate::util::guess_mime(filename)).as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());

    let url = format!("{}{}", cfg.base_url.trim_end_matches('/'), path);
    let resp = agent()
        .post(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={}", boundary),
        )
        .send_bytes(&body);
    map_resp(resp)
}

/// 下载制品字节到 `dest`，边写边算 sha256；返回 (字节数, sha256 hex)。
pub fn download_to(
    cfg: &CliConfig,
    token: Option<&str>,
    path: &str,
    dest: &std::path::Path,
) -> Result<(u64, String)> {
    let url = format!("{}{}", cfg.base_url.trim_end_matches('/'), path);
    let mut req = agent().get(&url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {}", t));
    }
    let mut reader = map_resp_reader(req.call())?;

    let mut file = std::fs::File::create(dest)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(anyhow!("读取响应失败: {}", e)),
        };
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut file, &buf[..n])?;
        total += n as u64;
    }
    Ok((total, crate::util::hex_encode(&hasher.finalize())))
}
