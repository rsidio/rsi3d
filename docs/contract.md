# 契约：从 Rust 类型派生，而不是手抄第二份

场景 JSON 的键名（`objects` / `window.spanX` / `bandDepth` / `nodes_upsert` …）不只是
Rust 内部的事。它们在**非 Rust 的代码里被读**：

| 谁 | 读什么 | 在哪 |
| --- | --- | --- |
| 浏览器客户端（three.js） | 节点 `name/translation/scale`、`extras.rsi3d.color`、增量 `changes.nodes_upsert`、帧 `png_base64` | `crates/serve/src/client.js` |
| 假引擎（脚手架演示） | `objects[].aabb`、`window.bandDepth`、`room.size`、`clearance_rules` | `scaffolds/agent-app/files/mock-engine.mjs` |
| 评测插件（脚手架） | `objects[].role/aabb` | `scaffolds/harness-plugin/files/src/evaluator.mjs` |
| 外部作者 | 手写 `scene.json`、写自己的工具链 | 你 |

改一处 Rust 字段、忘掉一处 JS，就是**静默漂移**。而最阴的一层在这里：

> `Scene` 与 `Node` 都带 `#[serde(flatten)] extras`，**任何拼错的键都会被吞进 extras**。
> 把 `bandDepth` 写成 `band_depth`，内核**照单全收、不报任何错**，只是窗带深度悄悄
> 变回默认值 1.5 —— 渲染测量跟着变，也没有任何提示。

所以这件事不能靠"仔细点"。这一层（`crates/contract`）把契约变成**可执行的门禁**。

---

## 1. 做法：派生，让编译器当门禁

不手写第二份真相，而是**从 Rust 类型算出来**（借自 rspirv 的做法，见
`prd/rsi3d-harness/prior-art.md` §9；我们用同一招，但规模小得多）：

| 手法 | 为什么它能当门禁 |
| --- | --- |
| 全字段样例用**结构体字面量**写（`full_scene()`） | 字段改名 / 删字段 / 加必填字段 → **编译不过** |
| 枚举取值用**穷尽 `match`**（`layer_name()` …） | 枚举加变体 → **编译不过**（另有单测钉住它与 serde 的改名规则一致） |
| 键清单从**序列化结果**推出 | `rename_all` / `alias` / `flatten` / `skip_serializing_if` 自动被算进去，不会漏 |
| **schema** 从 `#[derive(JsonSchema)]`（`schemars`）派生 | 类型是唯一出处；`required` / 枚举 / 字段文档都从类型来，不会随样例覆盖度缩水（见 §3.1） |
| 每条流消息都**构造一个样例** | 新增 / 改名消息变体 → 编译不过或门禁变红 |

`keys.json` 里的 `note` 就写着这句话：**schema 不是权威，内核（`Scene::from_json`）才是**。
这里产出的东西只有两个用途：给外部作者看的形状，以及**抓漂移的门禁**。

## 2. 产物

```bash
rsi3d-harness contract                  # 打印（--json 给工具消费）
rsi3d-harness contract --out contract   # 落盘（仓库里那份就是这么来的）
rsi3d-harness contract --check          # 只比对：产物与 Rust 类型不一致就报错
```

| 文件 | 是什么 | 谁看 |
| --- | --- | --- |
| `contract/keys.json` | 各块的键树（`scene` / `view` / `stream_server` / `stream_client` / `gltf_*`）+ 枚举取值表 | 工具、门禁 |
| `contract/scene.schema.json` | JSON Schema（2020-12）+ `x-known-keys` 键清单 | 编辑器、模型、外部作者 |
| `contract/view.schema.json` | **评测插件**看到的那份形状（`SceneView`，没有省略项，`null` 也是显式的） | 插件作者 |
| `contract/scene.example.json` | 一份**全字段填满**、合契约的场景样板 | 照抄用 |

样例是"纯粹的"：不多一个键（多一个键就会被当成契约的一部分，因为键清单是从样例派生的
—— 那是在骗人），也不少一个键。所以它里面既没有 `$comment` 说明字段，也没有演示用的
自定义 extras。

服务端也把同一份东西暴露出来（**现算**，不读磁盘——二进制旁边未必有 `contract/`）：

```bash
curl -s http://127.0.0.1:8283/contract                      # 索引 + 各块键数（无需令牌）
curl -s http://127.0.0.1:8283/contract/scene.schema.json     # 与 CLI 写出的字节一致
```

## 3. 权威边界：schema 只说形状

schema 断言的是**形状**：

* 已知的键有哪些（`x-known-keys`：既含裸名也含 `window.spanX` 这种路径，**仍从样例派生**——它喂的是镜像门禁，换来源要先过 §3.1 那道闸）；
* **哪些字段是真必需的**（`required`：只在类型里真的没有默认值的地方出现，例如
  `Aabb.min/max`、`ClearanceRule.pair/min/max`）；
* 具名类型与枚举（`$defs` + `anyOf`/`oneOf`）、serde 默认值（`default`）、
  以及**直接从 Rust doc comment 来的字段文档**；
* `additionalProperties: true` —— 因为 `extras` 的 flatten 是**有意支持**的。

> 以前这里写着「schema 里**没有** `required` —— 有的话就是假的」。那条断言**被实测推翻了**：
> 顶层那些字段确实都有默认值，但嵌套结构没有（`Aabb` 缺 `min` 内核就报错）。现在
> `required` 是真必需的那些，而且由 §3.1 的闸 1 钉住——"内核自己写出来的东西必须全部
> 通过这份 schema"，通不过就红。

`Snapshot.gltf` 里内嵌的是一份**标准 glTF 2.0 文档**，它的形状由 glTF 规范定义，不是我们
的契约，所以键树在那里**不展开**（`$opaque`）。属于我们的部分（`extras.rsi3d`）另有专门的块。

### "未知键"天生有两种读法

因为 flatten，任何一个键都能"合法地"落进 extras。所以契约校验器给出的 `UnknownKey`
**不一定是错误**——它只说明"内核认不出这个键"。要么拼错了，要么是有意扩展：

```rust
Finding { path: "window.band_depth", key: "band_depth", kind: FindingKind::UnknownKey }
```

对**我们自己出的**文件（脚手架场景、样板），我们要求"有意扩展"必须被**声明**：

```rust
// crates/contract/tests/contract.rs
const DELIBERATE_EXTRAS: &[(&str, &str)] = &[
    ("lights[].file", "HDRI 资产引用（内核不做资产解析）"),
    ("objects[].note", "节点上留给 RSI 循环的待修备注"),
    // …
];
```

这张表同时是审计记录：想让一个新 extras 键过关，就得写清为什么。

### 3.1 schema 从类型派生（schemars），以及为什么键清单还没换

2026-09-20 做了一次 spike（结论与实测见 `prd/rsi3d-harness/prior-art.md` §15）。
一句话：**scene / view 两份 schema 改由 `#[derive(JsonSchema)]` 从类型派生**，`keys.json`
（键清单）**仍然从样例派生**。

为什么只换一半：键清单喂的是 `mirrors_only_reference_known_keys` 这类镜像门禁，而那些门禁
依赖清单的**写法**（点路径 + 每段裸名）。换来源就得先对齐写法，而"没对齐"的表现是**门禁悄悄
变松**——不报错、只是不再挡东西。所以那一步留着，先用这道闸把两边钉在一起：

| 闸 | 守什么 | 实测 |
| --- | --- | --- |
| `typed_schema_accepts_everything_the_kernel_writes` | **不能太严**：`full_scene()` / `minimal_scene()` / `scene.example.json` / 所有出厂 `scene.json` 必须通过这份 schema（拿成熟校验器 `jsonschema` 验，不拿自己的代码验） | 5 份全过；`required` 写错就红 |
| `typed_schema_does_not_shrink_the_observed_named_fields` | **不能太松（防缩水）**：观察法得到的**具名字段**必须仍出现在 `properties` 里，且数量不低于钉住的下限；落在开放对象（`extras`）下的键单独计数 | scene：94 键 → 具名 88 · extras 7 · 讲不通 0；view：73 → 73 |
| `typed_schema_actually_constrains` | 这份 schema 真的在约束（有 `$defs` / `required` / `default` / `oneOf`，且 doc comment 变成了 `description`） | 全中 |

代价（实测）：`crates/core` 加 1 个**可选**依赖 + 16 行 `cfg_attr` 派生；`crates/contract`
删掉手写的观察法生成器 56 行、新增类型派生 90 行；`Cargo.lock` +66 包（其中 `schemars`
子树 18 个进运行时依赖树——**Cargo 特性统一**意味着一旦契约层打开这个特性，同一构建里的
`serve` / `native` 也会带上它；校验器那部分只进 dev）。收益：`scene.schema.json` 从
55 个平铺属性（无 `required`、无文档）变成 582 行 / 13 个具名 `$defs` / 带 `required`、
`default`、`oneOf` 与字段文档。

## 4. 五道门禁（每条都实测过会红）

`crates/contract/tests/contract.rs`：

| # | 门禁 | 改坏什么 → 哪条红 |
| --- | --- | --- |
| 1 | `committed_artifacts_match_the_source` | 改了 Rust 字段忘重新生成 → `committed_artifacts_match_the_source` |
| 2 | `mirrors_only_reference_known_keys` | 手抄的 JS 键名漂移 → `mirrors_only_reference_known_keys` |
| 3 | `shipped_scenes_conform_to_the_contract` | 出厂场景 `bandDepth` → `band_depth`（内核不报错） → `shipped_scenes_conform_to_the_contract` |
| 4 | `client_handles_every_server_message_type` | 服务端消息变体改名（浏览器会静默忽略整条消息） → `message_types_are_derived_not_guessed` |
| 5 | `scene_example_is_accepted_by_the_kernel` | Rust 字段改名（`camelCase` → `snake_case`） → `key_tree_uses_json_names_not_rust_names` + `validator_catches_typos_and_type_mismatch` |

第 2 条的表是手写的（JS 那边没有类型系统，抓不出它的读法），但**每一行都被双向检查**：
键既要在文件里真的被提到，也要在契约里真的存在。Rust 侧改名 → 红；JS 侧自己发明了个键 → 也红。

第 3 条的写法值得记一笔：把文件交给内核读进来再写回去，**文件里写过的每个键都必须原值保留**。
拼错键的症状就是"读起来不报错、写回去的值却变了"，这一条正好抓住。反方向不管：内核会把
默认值（如节点 `layers`）补全，那是它的权利。

## 5. 不做什么（以及为什么）

| 没做 | 原因 |
| --- | --- |
| 生成 TypeScript 类型 | 浏览器客户端是**没有构建步骤**的纯 JS（`crates/serve/src/client.js` 直接内嵌）。装饰性的 `.d.ts` 不会被执行，只会过期——那是另一种手抄 |
| 运行时在客户端校验版本 | 服务端与客户端**同一份二进制**内嵌，版本永远一致；校验只会在别处（npm 包、外部工具）才有意义 |
| 引入 `schemars` | **已经引了**（只用于 scene/view 两份 schema，见 §3.1）；`crates/core` 里是一个**可选**依赖，默认构建不带，靠 `schema` 特性打开 |
| 引入 `ts-rs` / 生成 TypeScript 类型 | 不做：浏览器客户端是**没有构建步骤**的纯 JS，装饰性的 `.d.ts` 不会被执行，只会过期 |
| 引入 `jsonschema`（校验器） | **只在 dev**（`cargo test`）：闸 1 拿它验收我们派生的 schema。不进任何交付二进制（`cargo tree -p rsi3d-harness-cli` 里为 0） |

## 6. 改了 Rust 类型之后

```bash
cargo run -p rsi3d-harness-cli -- contract --out contract   # 1. 重新生成
cargo test -p rsi3d-harness-contract                       # 2. 看还有哪道门禁红
```

第 2 步会把该改而没改的**手抄处**逐个点出来（客户端 JS、脚手架假引擎、评测插件、出厂场景）。
真改了形状，就得同时更新 `docs/core.md` 的键表——文档也在被审的范围内。
