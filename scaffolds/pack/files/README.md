# {{project}}

**{{domain}}** 行业业务包（slug `{{slug}}`，版本 `{{version}}`），由 `rsi3d-harness scaffold new pack` 生成于 {{date}}。

业务包交付的是**行业知识与流程**（买断、可离线），不是算力。它由四件东西组成，
缺任何一件都会让 Agent 只能"看感觉调"：

| 目录 | 是什么 | 缺了会怎样 |
| --- | --- | --- |
| `scenes/` | 场景模板：给 Agent 的起手式 | 每次从空场景开始，Agent 无从下手 |
| `materials/` | 材质/工艺规范：**什么样算对** | Agent 只能猜"好不好看" |
| `workflow/` | 阶段链与门禁：**怎么算跑完** | 每次跑 Run 标准都不一样 |
| `benchmark/` | 评测基准：**怎么证明没退步** | 无回归防线，改一个坏一个 |

`rsi3d.pack.json` 是清单：它把上面四件东西、评测维度与**权重**绑成一个可验签的制品。

## 打包 → 签名 → 验证 → 发布

```bash
# 1. 收集文件、逐文件 sha256、算清单 digest
rsi3d pack build . --slug {{slug}}

# 2. HMAC 签名（密钥首次自动生成在 ~/.rsi3d/signing.key）
rsi3d pack sign

# 3. 自己先验一遍（摘要 + 签名 + 逐文件一致性）
rsi3d verify rsi3d.pack.json

# 4. 发布到 rsi3d 平台（含 sha256，别人下载时会自动校验）
rsi3d publish --file rsi3d.pack.json --kind bizpack --slug {{slug}} --name "{{project}}"
```

别人拿到的方式：

```bash
rsi3d search --kind bizpack
rsi3d download @you/{{slug}} -o out/
rsi3d verify out/rsi3d.pack.json
```

## 权重是你的行业判断，不是旋钮

`rsi3d.pack.json` 里的 `evaluation.weights` 定义了这个行业**什么更重要**。
例如文旅场景可以把 `semantics.intent` 调高（像不像比几何精度重要），
工业件则应该把几何/公差类维度调到主导，并让 `layout.*` 接近 0。

改动权重等于改产品定义，建议每次改动都在 `benchmark/cases.json` 里留一条对照 case。

## 上架前自检

- [ ] `materials/` 里每条规范都有**机器可判的判据**（判不了的移出，别写进去）
- [ ] 每个 `benchmark` case 都有 `expect`（区间），不是"跑一下看看"
- [ ] `workflow` 里有**停止条件**（否则 Agent 会一直改下去）
- [ ] `known_gaps` 如实写明做不到的部分 —— 这比假装全能更能赢得信任
