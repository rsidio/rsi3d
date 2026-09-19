#!/usr/bin/env node
// npm 包 @rsi3d/cli 的命令入口之一：`rsi3d-harness`（3D 资产引擎）。
//
// 这个入口必须存在——本工程最主要的用法是 MCP 服务（`rsi3d-harness mcp`），
// 只给 `rsi3d` 一个入口的话，用户装了包却拿不到引擎。
require("./launcher")
  .run("rsi3d-harness")
  .catch((e) => {
    console.error(`✗ ${e && e.message ? e.message : e}`);
    process.exit(1);
  });
