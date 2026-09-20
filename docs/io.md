# 导入：把外部资产接进场景，并且不撒谎

这一层（`crates/io`）回答一个很具体的问题：**"我有一个 .blend / .step / .glb，怎么让它出现在
rsi3d-harness 里？"**

答案分三条路，而且**哪条路都要说清是哪条**——导入器最容易撒的谎就是"看起来成功了"。

```
rsi3d-harness scene import <文件> [--out 场景.json] [--unit-scale 0.001] [--limit N]
rsi3d-harness scene import --formats        # 看支持哪些、各走哪条路
```

---

## 1. 三条路

| 路 | 格式 | 谁在解析 |
| --- | --- | --- |
| **自己读** | `gltf` `glb` `obj` `stl` | 我们（`crates/io/src/readers.rs`，纯 Rust，无依赖） |
| **请 Blender** | `blend` `fbx` `usd` `usda` `usdc` `usdz` | 本机 Blender 转成 GLB，我们读那个 GLB |
| **请上游导出网格** | `step` `stp` `iges` `igs` `dwg` `dxf` | 没有人——这些是 B-rep，我们会**明确拒绝** |

### 1.1 为什么 STEP 要拒绝而不是"尽力而为"

STEP/IGES/DWG 里存的是**精确曲面**（NURBS 那一类），不是三角面。把它读成网格是一次**有损转换**，
需要一个 CAD 内核（OpenCASCADE 那一类）。所以我们不装：

```
✗ .step 我们不解析：**STEP 是 B-rep（精确曲面），不是网格**——把它读成三角面本身
就是一次有损转换，需要一个 CAD 内核（OpenCASCADE 那一类）。
建议：在 CAD/上游工具里导出 **STL / OBJ / glTF**（网格），我们直接就能读；
或者告诉我你装了什么 CAD 内核（FreeCAD 的 `freecadcmd` 之类），我把那条路接上。
```

### 1.2 Blender 那条路的两个坑

1. **Blender 退出码 0 不等于成功**：读不了的 blend，Blender 也会打印错误然后正常退出。
   所以我们同时检查**退出码**与**产物是否存在**，失败时把 Blender 自己的错误行原样带出来。
2. **版本**：Blender 只能开"不比自己新"的文件。`scene import` 会先报 Blender 的版本：

```
✗ Blender 4.2.9 LTS 读不了这个文件。Blender 说：
  Error: Loading "…/H0_URDF_rigged.blend" failed: Failed to read blend file
  '…/H0_URDF_rigged.blend', not a blend file
```

找 Blender 的顺序：`--blender` → `RSI3D_BLENDER` → `/Applications/Blender.app/Contents/MacOS/Blender`
→ `/usr/local/bin` `/usr/bin` `/snap/bin` `$HOME/blender*/blender` → `PATH`。找不到时的提示里会把
**试过哪些路径**列出来。

---

## 2. 几何放哪：side-car，不进场景文档

场景文档（`Scene`）是**契约对象**：进日志、进哈希、进增量。一个机器人总成上百万三角面，
塞进 `Scene` 会让"移动一个物体"都要搬几十 MB 的几何——日志、哈希、增量全部跟着变重。

所以几何写到**旁边那个文件**：

```
foo.scene.json          场景（对象/AABB/角色/provenance）
foo.scene.mesh.glb      几何 side-car（标准 glTF 2.0 二进制）
```

- 命名规则：`<场景文件名去后缀>.mesh.glb`，**同目录同前缀**，所以 `serve` 不需要任何配置就能找到它；
- 场景里的节点只写一句 `extras.rsi3d.mesh_ref = "foo.scene.mesh.glb"`（**裸 extras**，
  内核不必为它改字段，也就不会污染核心不变量）；
- GLB 里的**节点名就是我们的节点 id**（`obj:Part_A`），客户端读一遍按名字建索引即可，不用另传映射表；
- side-car **不参与版本与哈希**：它是"视图"不是"状态"。

客户端怎么用它（`crates/serve/src/client.js`）：

| 情况 | 画什么 |
| --- | --- |
| 节点声明了 `mesh_ref` 且 side-car 里有同名节点 | **真网格** |
| 其它一切（没 side-car / 名字对不上 / 解析失败） | 包围盒代理（**不白屏**） |

顶部「几何」那一栏显示 `真网格 N/M`：**部分成功是这里的常态**，所以写成比例而不是一句话。
服务端光栅（权威观测那一档）**始终**画 AABB 代理，证据档次不因此改变。

---

## 3. 报告里必须出现的事实

`scene import` 的每一行都是用户要的账：

```
导入 /tmp/iofix/fixture.blend
  格式 .blend · 经手：Blender 4.2.9 LTS · 转 GLB 后读入     ← 谁解析的
  3 个对象 · 2012 顶点 · 974 三角面                        ← 多少东西
  包围盒：x -2.000…2.000 · y -0.500…0.500 · z -2.000…2.000（米）
  坐标系：源文件 RUF → 场景 RDF                            ← 转轴了没有
  几何 side-car：fixture.scene.mesh.glb
  ⚠ 对象「Floor_01」有一轴只有 0.0000 米（薄片/平面）：服务端光栅把它当薄盒画，俯视图可能看不到
  ⚠ 源文件惯例是 RUF（Z 上），转 GLB 时已经过了一道转轴：场景里的坐标是 RDF（Y 上）…
```

**坐标系写两个字段**，因为它们是两件事：

- `provenance.source_coordinate_system` = **源文件自己**的惯例（`.blend`/`.fbx`/`.usd*` 是 Z 上 RUF）；
- `scene.source_coordinate_system` = **场景里的坐标实际在哪**（Blender 导出 glTF 时自己转过轴 → RDF）。

两者不同就出告警。**我们不做轴向转换、不猜单位**：`--unit-scale` 只缩放、不改轴向。
`obj` / `stl` 干脆写 `unknown`——这两个格式里**没有**坐标系与单位字段，"惯例 Y 上"是猜的，猜的东西不写进事实字段。

其它会主动说出来的事：

| 情况 | 报告里写什么 |
| --- | --- |
| `--limit` 截断了 | "源文件有 N 个对象，`--limit` 只导了前 M 个（**被截断了**）" |
| 整体尺寸 > 100 m | "像个体育场——源文件多半是毫米单位：加 `--unit-scale 0.001`" |
| 某一轴 < 1e-3 m | 点名是哪个对象，并说明它在光栅里会看不见 |
| OBJ 里有孤立顶点 | "声明了 N 个顶点，其中 M 个没有任何面引用（丢掉了）" |
| 二进制 STL | "三角汤：所有面挤在一个对象里（源文件本来就没有对象划分）" |

---

## 4. 读取器的两个坑（都无声）

写在注释里，也钉在用例上（`crates/io/tests/import.rs::each_object_gets_its_own_aabb_not_the_whole_file`）：

1. **每个对象只能带自己的顶点**。图省事让所有对象共享全局顶点表 → 每个对象的 AABB 都等于整体
   AABB。这个错误**不报错、不崩**，只是从此所有"间距/挡窗"测量全是假的。
2. **判断"这个对象有没有东西"要看面索引**，不能看顶点：解析过程中顶点还在全局表里。

另外：STL 按位模式去重顶点（立方体 36 → 8），**不引入任何浮点容差**——容差合并会把锐角件的顶点吃掉，
那是改数据不是压数据。

---

## 5. 非标准 / 更新版 Blender 文件：怎么诊断

`assets/` 里那两个 `.blend` 就是活例子。它们**不是坏文件**，而是被"重打包"过 + 由更新的 Blender 写的。
诊断顺序（每一步都是可复现的命令，不是猜）：

```bash
# ① 外层是什么？
python3 -c "print(open('assets/H0_URDF_rigged.blend','rb').read(4).hex())"
#  28b52ffd = zstd magic ⇒ 整文件套了一层 zstd（多帧，pzstd 那类）
```

```bash
# ② 解开看里面
node -e "const z=require('zlib'),fs=require('fs');const b=fs.readFileSync('assets/H0_URDF_rigged.blend');
let o=[],i=0;for(;i+4<=b.length;i++)if(b[i]==0x28&&b[i+1]==0xb5&&b[i+2]==0x2f&&b[i+3]==0xfd)o.push(i);
const p=[];for(let k=0;k<o.length;k++){try{p.push(z.zstdDecompressSync(b.subarray(o[k],o[k+1]||b.length)))}catch(e){}}
const all=Buffer.concat(p);console.log(all.length, JSON.stringify(all.slice(0,20).toString('latin1')));"
```

`H0_URDF_rigged.blend` 的结果（可复现）：106 帧 → 84,667,954 字节，开头是
`BLENDER17-01v0502` + `REND`。**标准头是 12 字节**（`BLENDER` + 指针宽度 + 端序 + 3 位版本，
例如 Blender 4.2.9 写的 `BLENDER-v402`）；这里是 **17 字节**，多出 5 个字符。

接着按**块头**走一遍就能看出真相：这个文件用的是 **32 字节块头**
（`code(4) + ?(4) + old(8) + len(4) + sdna(4) + nr(4) + ?(4)`，数据从 `+32` 起），
标准是 24 字节（数据从 `+24` 起）。按 32 字节走，1,904 个块**零失步**走到文件尾：

```
REND@17(+264) TEST@313(+65544) GLOB@65889(+1216) WM..@67137(+1496) …
块码统计: DATA:1823 OB..:23 ME..:22 WS..:11 SN..:11 IM..:2 REND:1 TEST:1
```

`OB..:23` `ME..:22` = **23 个对象、22 个网格**，`DNA1`/`ENDB` 都在，末尾算术精确吻合
（`ENDB@84,667,922 + 32 = 文件尾`）。**这是一个完整的 Blender 文件，数据没有损坏。**

最后一块拼图在 `GLOB` 块的 `FileGlobal` 里：

```
subversion = 45   minversion = 405   curversion = 85
```

于是 Blender 的判断是：**"文件由 4.5（sub 45）保存，需要 4.5（sub 85）或更高"**——
本机的 4.2.9 版本不够。（sub 45/85 不像官方 4.5.x 的补丁号，所以写它的很可能是一个
**厂商定制构建**；"17-01v0502" 那个标签也说明中间过了一手工具。）

> **结论与出路**：装 Blender 4.5+（或让产出这个文件的工具直接导出 `.blend` / glTF / FBX），
> 然后 `scene import` 就能一路走通。
>
> ⚠️ **不要**把 `minversion` 用十六进制编辑器改成 4.2 硬开：我们试过——Blender 4.2.9 读 4.5 的
> DNA 会**段错误**（`zsh: segmentation fault`）。版本检查在这儿是保护，不是障碍。

---

## 6. 验证

| 层 | 在哪 | 保证什么 |
| --- | --- | --- |
| 单元 + 端到端 | `crates/io/tests/import.rs`（8 个） | OBJ/ASCII STL/二进制 STL 往返、每对象独立 AABB、截断要说出来、单位提醒、B-rep 与"Blender 读不了"给的错误**有用**、真 Blender 夹具往返 |
| 内核 + 契约 | 同文件的 `assert_import_is_usable` | 导入产物 ①`Scene::from_json` 读得回来 ②每个对象 extras 往返不丢 ③契约校验无未知键 ④side-car 存在且是能读的 GLB |
| 路由 | `crates/serve/tests/http.rs::mesh_sidecar_is_served_byte_for_byte_and_404_is_honest` | `/mesh.glb` 原字节送出、无令牌 401、没有 side-car 时 404 且给出路 |
| 冒烟 | `scripts/smoke.sh` §13 | 上面这些在真二进制、真文件上成立 |
