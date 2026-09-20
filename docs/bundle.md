# 技能包 / 插件包（`rsi3d-bundle/v1`）

**一个目录 = 一个包。** 目录里必须有一份清单，`rsi3d` 把它打成确定性 zip 发布；
平台会在入库前解包核一遍，安装侧再解到目标位置。

## 目录长什么样

```
skills/rsi3d-harness/
├─ skill.json            # 清单（kind=skill 用 skill.json，kind=plugin 用 plugin.json）
├─ SKILL.md              # 入口（清单里的 entry）
└─ references/           # 只在被引用时才读的深资料
   ├─ cli.md
   └─ discipline.md
```

清单：

```json
{
  "spec": "rsi3d-bundle/v1",
  "name": "rsi3d-harness",
  "kind": "skill",
  "title": "rsi3d-harness 3D 资产引擎",
  "summary": "一句话说明它是干什么的",
  "entry": "SKILL.md",
  "agents": ["claude-code", "copilot", "agents"],
  "files": ["SKILL.md", "references/cli.md", "references/discipline.md"],
  "requires": { "bins": ["rsi3d-harness"], "hint": "cargo build --release" },
  "mcp": { "command": "rsi3d-harness", "args": ["mcp", "--root", "."] }
}
```

| 字段 | 约束 |
| --- | --- |
| `spec` | 固定 `rsi3d-bundle/v1` |
| `name` | 小写字母/数字/连字符，≤64 位；**安装后的目录名**（`SKILL.md` 的 `name` 要与它同名） |
| `kind` | `skill` / `plugin`，必须与发布的制品类型一致 |
| `entry` | 必须有，且**必须出现在 `files` 里** |
| `files` | 目录里**除了清单自身**之外的**全部**文件，逐个列出（多一个少一个都会被打包拦下） |
| `agents` | 这个技能给哪些 Agent 用（`claude-code` / `copilot` / `agents`） |
| `requires` / `mcp` | 自由结构，随包走；安装侧会用它提示「下一步该装什么」 |

## 三条纪律

**1. `files` 必须是穷尽清单。** 打包时会比对「清单 vs 目录真实内容」，不一致直接拒绝并列出差异。
理由：包里少一个文件 = 别人装完少一块能力；多一个没登记的文件 = 没人知道它为什么在里面。
（加一个引用文件忘了登记，是最常见的那种错。）

**2. 路径不许穿越。** 条目名不能是绝对路径、不能含 `..`、不能用反斜杠、不能是软链。
写侧与读侧**都**挡——下载包的人未必会校验，两端都挡才是对的。

**3. 同内容必得同字节。** 写侧只用 **store**（不压缩）、时间戳固定、条目按名字排序，
所以同一个目录打两次的 sha256 一样，可以直接当版本锁用（供应链那条：安装侧校验
`slug + version + sha256`）。读侧能吃 store 与 deflate —— 别人的 zip 也要能解。

## 命令

```bash
rsi3d skill pack   skills/rsi3d-harness --out x.zip   # 打包（本地校验清单自洽）
rsi3d skill inspect x.zip                             # 看包里有什么 + 校验每个条目
rsi3d publish --dir skills/rsi3d-harness --kind skill # 打包 + 上传 + 发布
rsi3d install skill/rsi3d-harness --agent claude-code # 解包到 ~/.claude/skills/<name>/
rsi3d install skill/rsi3d-harness --dir ./somewhere   # 或解到你指定的目录
```

`--agent` 的落点按各 Agent 公布的约定（个人作用域）：

| `--agent` | 个人（默认 scope=user） | 项目（`--scope project`） |
| --- | --- | --- |
| `claude-code` | `~/.claude/skills/` | `<项目>/.claude/skills/` |
| `copilot` | `~/.copilot/skills/` | `<项目>/.github/skills/` |
| `agents` | `~/.agents/skills/` | `<项目>/.agents/skills/` |

装完会在目标目录写一份 `rsi3d-install.json`（引用、版本、sha256、文件清单、来源），
用来对账与回滚。

## 平台侧会再核一遍

发布 `kind=skill` / `plugin` 的制品时，平台解包校验：清单自洽、`entry` 在 `files` 里、
声明文件确实在包里、路径安全、体积上限。不过就 `400 bundle_invalid` 并告诉你原因。
通过后摘要写进制品的 `manifest.bundle`。

> **自托管 URL（BYO）拿不到字节** → 平台不假装验过，标 `{"validated": false, "reason": "…"}`。

## 我们的两个包

| 目录 | kind | 用途 |
| --- | --- | --- |
| `skills/rsi3d-harness/` | `skill` | 把引擎交给 Agent（九个 `scene_*` 工具 + CLI + 纪律） |
| `plugins/harness-use/` | `plugin` | harness-use 宿主的「RSI 3D」功能应用（源码贡献形态，见其 README） |

两者的 `files` 与目录实际内容一致这件事由冒烟钉住（`scripts/smoke.sh` 的「技能包 / 插件包」段），
包的字节还要让**系统 `unzip`** 读一遍——我们自己的测试证明不了互操作性。
