#!/usr/bin/env node
// postinstall：尽力把两个二进制拉到 ~/.rsi3d/bin。
//
// 两条纪律：
//  1. **绝不因为下载失败而让安装失败**——离线、墙、公司代理都很常见，
//     那种情况下用户仍然能拿到启动器，然后自己用源码构建或设 RSI3D_BIN。
//  2. **不静默**——每次跳过都往 stderr 说一句为什么。

const { download } = require("./download");
const { findLocal } = require("./launcher");

async function main() {
  // 源码里 / 之前装过：什么都不用做
  const need = ["rsi3d", "rsi3d-harness"].filter((n) => !findLocal(n));
  if (need.length === 0) {
    console.error("[rsi3d] 已找到本地二进制，跳过下载");
    return;
  }
  for (const name of need) {
    try {
      const p = await download(name);
      console.error(`[rsi3d] 已安装 ${name} → ${p}`);
    } catch (e) {
      console.error(
        `[rsi3d] ${name} 下载失败（${e && e.message ? e.message : e}）；` +
          `首次运行时还会再试一次，也可以用 RSI3D_BIN 指定已有二进制`
      );
    }
  }
}

main().catch((e) => {
  console.error(`[rsi3d] postinstall 忽略了一个错误：${e && e.message ? e.message : e}`);
});
