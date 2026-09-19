// 启动器：把一个 npm 包的两个命令（rsi3d / rsi3d-harness）解析到真正的原生二进制上。
//
// 解析顺序（先本地、后远端，绝不静默换来源）：
//   1. RSI3D_BIN / RSI3D_HARNESS_BIN 环境变量（显式指定，最高优先）
//   2. 仓库内构建产物 target/release/<name>（开发/单仓场景，装了就用它）
//   3. ~/.rsi3d/bin/<name>（之前装过的）
//   4. 包内 vendor/<target>/<name>（随包分发的预编译产物）
//   5. 从 RSI3D_RELEASE_BASE 下载到 ~/.rsi3d/bin（并校验 checksums.txt 里的 sha256）
//
// 为什么这么绕：npm 包的体积与平台矩阵是对不上的——把 4 个平台的二进制都塞进包里
// 会让所有人都为别人的平台付下载成本。所以默认只带启动器，二进制按需取。

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync } = require("child_process");

const ROOT = path.resolve(__dirname, "..");

/** 支持的命令 → 二进制基名。 */
const COMMANDS = {
  rsi3d: "rsi3d",
  "rsi3d-harness": "rsi3d-harness",
};

/** 当前平台标识（与发布产物的命名约定一致）。 */
function target() {
  const arch = { x64: "x64", arm64: "arm64" }[process.arch] || process.arch;
  const plat = { darwin: "darwin", linux: "linux", win32: "win32" }[process.platform] || process.platform;
  return `${plat}-${arch}`;
}

function exeName(base) {
  return process.platform === "win32" ? `${base}.exe` : base;
}

/** 显式指定的二进制（环境变量）。它是**权威**的：给了就必须能用，不静默换别的。 */
function envOverride(name) {
  const key = name === "rsi3d" ? "RSI3D_BIN" : "RSI3D_HARNESS_BIN";
  const v = process.env[key];
  return v && v.trim() ? { key, path: v.trim() } : null;
}

/** 本地能直接用的候选路径（不含环境变量覆盖与下载）。 */
function localCandidates(name) {
  const exe = exeName(name);
  const home = os.homedir();
  const out = [];
  // 单仓：<repo>/rsi3d-harness/target/release/（本文件位于 .../rsi3d-harness/packages/rsi3d-cli/bin/）
  out.push(path.resolve(ROOT, "..", "..", "target", "release", exe));
  out.push(path.join(home, ".rsi3d", "bin", exe));
  out.push(path.join(ROOT, "vendor", target(), exe));
  return out;
}

function isExecutable(p) {
  try {
    fs.accessSync(p, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function findLocal(name) {
  for (const p of localCandidates(name)) {
    if (isExecutable(p)) return p;
  }
  return null;
}

function die(name, message) {
  const lines = [
    `✗ 找不到 ${name} 的原生二进制。`,
    message ? `  ${message}` : null,
    "",
    "可以这样解决：",
    `  · 在源码里构建：cd rsi3d-harness && cargo build --release`,
    `  · 或者显式指定：RSI3D_BIN=/path/to/${exeName(name)} npx @rsi3d/cli ${name}`,
    `  · 或者允许自动下载：RSI3D_RELEASE_BASE=<发布地址> （默认 GitHub Releases）`,
    "",
    "支持的平台：" + target(),
  ].filter(Boolean);
  console.error(lines.join("\n"));
  process.exit(127);
}

/** 把参数原样透传给原生二进制，并沿用它的退出码。 */
async function run(name) {
  const bin = COMMANDS[name];
  if (!bin) {
    console.error(`✗ 未知命令：${name}`);
    process.exit(2);
  }

  let resolved;
  const override = envOverride(bin);
  if (override) {
    // 显式指定就是权威：不静默回退到别的来源（否则"我以为在用 A，其实在用 B"最难查）
    if (!isExecutable(override.path)) {
      die(bin, `${override.key}=${override.path} 不可执行（文件不存在或没有执行权限）`);
    }
    resolved = override.path;
  } else {
    resolved = findLocal(bin);
  }

  if (!resolved) {
    // 尽力下载一次；失败也给用户看得懂的提示，而不是一段栈
    try {
      const { download } = require("./download");
      resolved = (await download(bin)) || null;
    } catch (e) {
      die(bin, `自动下载失败：${e && e.message ? e.message : e}`);
    }
  }
  if (!resolved || !isExecutable(resolved)) {
    die(bin, `已尝试：${localCandidates(bin).join(" · ")}`);
  }

  const res = spawnSync(resolved, process.argv.slice(2), { stdio: "inherit" });
  if (res.error) {
    console.error(`✗ 启动 ${resolved} 失败：${res.error.message}`);
    process.exit(126);
  }
  process.exit(res.status === null ? 1 : res.status);
}

module.exports = { run, COMMANDS, target, exeName, localCandidates, findLocal, envOverride };
