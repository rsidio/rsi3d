# harness-use 功能应用插件（`feature-rsi3d`）

把 rsi3d 接进 [harness-use](https://github.com/harnessuse) 的**功能应用**形态：装完顶栏出现「RSI 3D」，
里面有四块——身份绑定 / 发布制品 / 检索与消费 / 最近 Run。

> **v1 是源码贡献，不是动态加载。** 宿主目前（2026-09-20）**没有**外部插件格式、没有磁盘 manifest、
> 没有动态加载器：插件 = 编译期贡献到宿主 React bundle 的一个页面组件，「安装」= 往
> `localStorage['hu_plugins']` 写一条 `InstalledPlugin` 记录（**不下载任何代码**）。
> 这条路径写在 `prd/agent-plugin.md` §2.2 / §5 里，也写在 `plugin.json` 的 `install.kind` 上。

## 两个文件

| 文件 | 落到宿主哪里 | 是什么 |
| --- | --- | --- |
| `Rsi3dPage.tsx` | `agent/src/components/Rsi3dPage.tsx` | 页面组件（唯一的大文件） |
| `rsi3d.ts` | `agent/src/agent/rsi3d.ts` | 插件自有模块：连接配置 + API + sparkline |

拷贝与登记可以一次做完：

```bash
bash plugins/harness-use/integrate.sh /path/to/harnessuse/agent          # 应用
bash plugins/harness-use/integrate.sh /path/to/harnessuse/agent --check  # 只检查，不改
cd /path/to/harnessuse/agent && npx tsc --noEmit                          # 宿主的类型检查
```

脚本是**幂等**的：已经改过的位置会跳过；碰到与预期不符的内容会**停下并告诉你**，不做模糊匹配式的乱改。

## 五处登记（脚本做的事，也可以手动做）

| # | 文件 | 改动 |
| --- | --- | --- |
| 1 | `agent/src/components/Rsi3dPage.tsx` | **新增**（从本目录拷） |
| 2 | `agent/src/agent/rsi3d.ts` | **新增**（从本目录拷） |
| 3 | `agent/src/agent/plugins.ts` | `FEATURE_CATALOG` 加一条 `{ id:'feature-rsi3d', kind:'feature', name:'RSI 3D', vendor:'rsi3d', desc:'…', source:'builtin' }` |
| 4 | `agent/src/components/chrome.tsx` | `View` 加 `'rsi3d'`；`VIEW_META.rsi3d`；`VIEW_FEATURE.rsi3d='feature-rsi3d'`；`enabledViews()` 里 `out.push('rsi3d')`（受 `isFeatureOn` 门控） |
| 5 | `agent/src/App.tsx` | `import Rsi3dPage` + `{active === 'rsi3d' && viewAllowed('rsi3d') && <div className="hu-view on"><Rsi3dPage onNav={setView} /></div>}` |

装完默认**不显示**——要去「应用」页装上 `RSI 3D` 才会出现（与 `feature-studio` / `feature-me` 同一套开关）。

## 四块能力与依赖的 API

| 块 | 依赖 |
| --- | --- |
| ① 身份绑定 | `POST /api/auth/login` · `GET /api/auth/me`；凭据存 `localStorage['rsi3d_conn']`（`{baseUrl, token, email}`，对齐宿主 `hu_llm` 的「本地凭据 + 掩码展示 + 自测连通」模式） |
| ② 发布 | `POST /api/uploads`（multipart）→ `POST /api/artifacts` |
| ③ 检索与消费 | `GET /api/harnesses` · `GET /api/artifacts` · `POST /api/runs` · `GET /api/runs/:id`（轮询画曲线） |
| ④ 最近 Run | `GET /api/runs?limit=5` |

## 红线（对齐 `prd/agent-plugin.md` §2.5，插件里逐条对着写过）

1. **插件只是客户端** —— 不实现用户 / 组织 / 计费 / 审计；要管这些，界面里给的是跳浏览器控制台的链接。
2. **不合并账号** —— rsi3d 账号 ≠ harness-use 账号，只做「绑定」。
3. **凭据只存本地** —— `localStorage`（demo 档；生产建议系统 keyring），**不上报给任何平台**；界面上只显示掩码。
4. **不代理数据** —— 插件只拿 URL / 摘要；3D 产物由客户侧 Runtime 直连产出。
5. **不新增宿主依赖** —— 只用宿主已有的 React 与现成样式类。

## 与「Skill 形态」的关系

同一个 rsi3d，两种接入：

| | Agent 技能（`skills/rsi3d-harness/`） | harness-use 功能应用（本目录） |
| --- | --- | --- |
| 形态 | `SKILL.md` + 引用文件（包） | 宿主页面组件（源码贡献） |
| 给谁 | VS Code / Cursor / Claude Code | harness-use 普通用户（图形界面） |
| 装法 | `rsi3d install skill/rsi3d-harness --agent claude-code` | 应用中心安装「RSI 3D」 |
| 共同点 | 都是客户端，都不合并账号、不代理数据 | 同左 |

## 平台要先允许这个来源（实测会撞上）

插件在 `http://localhost:1420`（Vite dev）上跑，而平台默认不开 CORS 白名单 —— 直接登录会得到
`TypeError: Failed to fetch`（浏览器把 CORS 拦截表现成网络失败，**控制台里才有真正原因**）。

所以平台启动时要带上：

```bash
RSI3D_CORS_ORIGINS=http://localhost:1420 ./rsi3d-server
# 生产环境换成 harness-use 实际部署的源（多个用逗号分隔）
```

这条是实测出来的（2026-09-20，见下方「验证记录」），不是推测。

## 未验证的部分（如实说）

本目录里的文件是在**另一个仓库**（rsi3d-harness）里写的，落进宿主后才由宿主的 `tsc` 检查。
`integrate.sh` 跑完请务必执行 `npx tsc --noEmit`；宿主的 tsconfig 开了 `strict` +
`noUnusedLocals` + `noUnusedParameters`，未使用的参数会直接编译失败。

## 验证记录（2026-09-20）

| 项 | 怎么验的 | 结果 |
| --- | --- | --- |
| 类型与打包 | `cd <harnessuse>/agent && npm run build`（= `tsc && vite build`） | ✓ 54 modules，零错误 |
| 动态导航 | 往 `localStorage['hu_plugins']` 写一条并启用 → 刷新 | ✓ 顶栏出现「RSI 3D」 |
| 四块渲染 | 浏览器里打开该页 | ✓ ① 身份绑定（未绑定 → 已绑定）② 发布制品 ③ 检索与消费 ④ 最近 Run |
| 真连平台 | 填本机平台地址 + 账号 → 登录 | ✓ 拿到计划 / 命名空间 / 凭据掩码；搜索列出 4 个 Harness（含信誉分与已验证标记） |
| 未开 CORS 时 | 先不带 `RSI3D_CORS_ORIGINS` 跑一遍 | ✓ 界面如实报错（不静默失败）——并因此补上了上面那一节 |

⚠️ 还没验的：真实发布（② 上传大文件）与跑 Run（③ 需要客户侧 Runtime 真的在跑）。
这两块只验了「请求发得出去、错误报得出来」。
