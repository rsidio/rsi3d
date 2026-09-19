# {{project}}

一个 rsi3d-harness **外部模板**：产出「{{purpose}}」。由 `scaffold new scaffold` 生成于 {{date}}。

## 一个模板由什么组成

```
{{project}}/
├─ scaffold.json      # 清单：id / 分类 / 变量 / 生成后提示
└─ files/             # 唯一会被生成的目录（里面的一切都会拷进产物）
```

| 规则 | 为什么 |
| --- | --- |
| **只有 `files/` 会被生成** | 避免把模板自己的清单、说明、脚本混进产物 |
| 路径与内容里都可以写占位符 | 文件名也能用变量，例如 `files/src/\{{project}}.mjs` |
| 模板**不会执行任何脚本** | 生成后只打印 `next` 里的提示，由使用者确认后再跑（供应链红线） |
| 变量必须声明 | 内容里出现未声明的变量会**直接报错**，不静默留白 |

## 两层渲染（这个模板最值得看的地方）

本文件此刻正在被渲染：上面的 `{{date}}` 已经变成了生成日期。
但 `files/scaffold.json` 与 `files/files/README.md` 里写的是 `\{{project}}`（带反斜杠）——
那是**刻意留给你这个模板的占位符**，渲染时反斜杠被吃掉，产物里留下真正的占位符。

一句话：**写 `\{{x}}` 表示"现在替换"，写 `\\{{x}}` 表示"留到下一层再替换"。**

## 本地生效

```bash
cp -R . ~/.rsi3d-harness/scaffolds/{{project}}
rsi3d-harness scaffold list                       # 应能看到 {{project}}
rsi3d-harness scaffold new {{project}} --var project=demo
```

外部模板**同名即覆盖内置模板** —— 想改内置行为不必 fork 引擎，导出一份改掉即可：

```bash
rsi3d-harness scaffold export pack ./my-templates/pack
```

## 变量声明

`scaffold.json` 的 `vars` 里，`default` 为空 = 必填：

```json
{ "key": "project", "prompt": "项目名（也是默认目录名）", "default": "" }
```

使用者用 `--var project=my-thing` 提供；未提供且无默认值时会明确报错。

## 三条建议

1. **模板要能一条命令跑通**：生成完照着 `next` 复制粘贴就能看到结果，别让使用者读文档猜。
2. **产物自带自测**：像 `harness-plugin` 那样带 `selftest.mjs`，别人改坏了能立刻发现。
3. **别把密钥写进模板**：模板是会被分发的东西，占位符只放非敏感变量。
