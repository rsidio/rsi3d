// 按需下载预编译二进制到 ~/.rsi3d/bin/。
//
// 这是**兜底路径**：能本地构建就不用它。下载后校验 sha256（若发布侧提供了 checksums.txt），
// 校验不过就绝不安装——宁可失败，也不要让用户跑一个来路不明的二进制。

const crypto = require("crypto");
const fs = require("fs");
const https = require("https");
const os = require("os");
const path = require("path");
const { target, exeName } = require("./launcher");

// ⚠️ 组织名是 **rsidio** 不是 rsi3d —— 仓库在 github.com/rsidio/rsi3d，
//    package.json 的 repository 和 README 的 clone 命令也都是 rsidio。
//    写成 rsi3d/rsi3d 不会有任何本地报错（本地构建路径优先），只有在用户
//    postinstall 走远端下载时才暴露成 404，且提示信息不会指向这里。
const DEFAULT_BASE = "https://github.com/rsidio/rsi3d/releases/latest/download";

function base() {
  return (process.env.RSI3D_RELEASE_BASE || DEFAULT_BASE).replace(/\/+$/, "");
}

function binDir() {
  const dir = path.join(os.homedir(), ".rsi3d", "bin");
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

/** 下载到内存（跟随重定向；GitHub Releases 会 302 到对象存储）。 */
function fetch(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error("重定向次数过多"));
    https
      .get(url, { headers: { "user-agent": "@rsi3d/cli" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          const next = new URL(res.headers.location, url).toString();
          return fetch(next, redirects + 1).then(resolve, reject);
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`${url} → HTTP ${res.statusCode}`));
        }
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve(Buffer.concat(chunks)));
      })
      .on("error", reject);
  });
}

/** 解析 checksums.txt（`<sha256>  <文件名>`，与平台侧 install.sh 同格式）。 */
function parseChecksums(text) {
  const out = {};
  for (const line of text.split(/\r?\n/)) {
    const m = line.trim().match(/^([0-9a-f]{64})\s+\*?(.+)$/i);
    if (m) out[m[2].trim()] = m[1].toLowerCase();
  }
  return out;
}

function sha256(buf) {
  return crypto.createHash("sha256").update(buf).digest("hex");
}

/** 下载并安装某个二进制；返回安装后的路径。失败时抛错（由调用方决定怎么提示）。 */
async function download(name) {
  const asset = `${name}-${target()}${process.platform === "win32" ? ".exe" : ""}`;
  const dest = path.join(binDir(), exeName(name));

  const body = await fetch(`${base()}/${asset}`);
  if (body.length < 1024) {
    throw new Error(`下载到的内容太小（${body.length} 字节），可能不是二进制`);
  }

  // 校验和是可选的：没有就跳过，但要在 stderr 说明（不静默）
  let expected = null;
  try {
    const sums = parseChecksums((await fetch(`${base()}/checksums.txt`)).toString("utf8"));
    expected = sums[asset] || null;
  } catch {
    expected = null;
  }
  if (expected) {
    const got = sha256(body);
    if (got !== expected) {
      throw new Error(`sha256 不匹配（期望 ${expected.slice(0, 12)}…，实际 ${got.slice(0, 12)}…）`);
    }
  } else {
    console.error(`[rsi3d] 未取到 ${asset} 的校验和，已跳过校验`);
  }

  fs.writeFileSync(dest, body, { mode: 0o755 });
  return dest;
}

module.exports = { download, target, base, parseChecksums, sha256 };
