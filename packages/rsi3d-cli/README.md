# @rsi3d/cli

> rsi3d 官方命令行 + 3D 资产 Agentic 引擎的 npm 入口。装了它，`rsi3d` 与 `rsi3d-harness` 两个命令都能用。

```bash
npx @rsi3d/cli rsi3d-harness --help     # 引擎
npx @rsi3d/cli rsi3d --help             # 平台客户端
```

## 两个命令分别干什么

| 命令 | 用途 |
| --- | --- |
| `rsi3d-harness` | **引擎**：`mcp`（给 VS Code / Cursor / Claude Code 当工具）· `scene`（观察/编辑/回滚/渲染/校验）· `scaffold`（生成你自己的 3D 资产系统与 Agent） |
| `rsi3d` | **平台客户端**：发布 / 检索 / 下载 / 验签 / 打包 / 驱动 Run |

在 VS Code 里用（`.vscode/mcp.json`）：

```json
{
  "servers": {
    "rsi3d-harness": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@rsi3d/cli", "rsi3d-harness", "mcp", "--root", "${workspaceFolder}"]
    }
  }
}
```

## 二进制从哪来

这个包**默认只带启动器**，不带二进制。`bin/launcher.js` 按下面的顺序找（先本地、后远端，绝不静默换来源）：

1. `RSI3D_BIN` / `RSI3D_HARNESS_BIN`（显式指定）
2. 仓库内构建产物 `rsi3d-harness/target/release/<name>`（单仓开发场景）
3. `~/.rsi3d/bin/<name>`（之前装过的）
4. 包内 `vendor/<platform>-<arch>/<name>`
5. 从 `RSI3D_RELEASE_BASE` 下载到 `~/.rsi3d/bin/`（默认 GitHub Releases）

为什么不在包里直接带二进制：平台矩阵 ×2 个二进制会让**所有人**为别人的平台付下载成本。
预编译产物走 npm [optionalDependencies](https://docs.npmjs.com/cli/v10/configuring-npm/package-json#optionaldependencies)
的平台包（`@rsi3d/cli-<platform>-<arch>`）分发——那是下一步。

## 只想用源码

```bash
git clone https://github.com/rsi3d/rsi3d && cd rsi3d/rsi3d-harness
cargo build --release        # 一次出两个二进制，共用 target/
./target/release/rsi3d-harness --help
```

## 许可

Apache-2.0。
