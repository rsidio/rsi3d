#!/usr/bin/env node
// npm 包 @rsi3d/cli 的命令入口之一：`rsi3d`（平台命令行）。
require("./launcher")
  .run("rsi3d")
  .catch((e) => {
    console.error(`✗ ${e && e.message ? e.message : e}`);
    process.exit(1);
  });
