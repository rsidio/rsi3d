# glTF 导入导出与 side-car

引擎自己读 **glTF / GLB / OBJ / STL**；**BLEND / FBX / USD** 交给本机 Blender 转一道。
`.stp/.step/.iges/.dwg` 是 B-rep（不是网格）→ 明确拒绝，并告诉你该在 CAD 侧导出网格。

```bash
rsi3d-harness scene import --formats                  # 支持哪些、各走哪条路
rsi3d-harness scene import assets/robot.blend --out robot.scene.json
rsi3d-harness scene import part.step --unit-scale 0.001   # 毫米单位显式声明
rsi3d-harness scene export living.doc.json --out model.gltf
```

## 导出

`scene export` 出**标准 glTF 2.0**（单文件、内嵌 base64、`extras.rsi3d` 带我们的节点 id）。
Blender / FyroxEd / three.js 直接能读。**出口格式只做标准 glTF**，不自造格式。

## 导入

- **几何 side-car**：导入会同时写出 `<场景名>.mesh.glb`，文档里只存 AABB + `mesh_ref`（内容寻址）。
  浏览器拿到 side-car 就画真网格，拿不到就画包围盒代理（**不白屏**，但别把它当真网格）。
  `--no-mesh` 可以不要 side-car（只要 AABB 场景）。
- **坐标系写两个字段**，不要混：
  - `provenance.source_coordinate_system`：源文件惯例（blend/fbx/usd 通常是 RUF）
  - `scene.source_coordinate_system`：场景实际（经 Blender 转轴后是 RDF）
  - OBJ/STL 写 `unknown`——**不猜**。引擎不做轴向转换、不猜单位；单位靠 `--unit-scale` 显式声明。
- **装配体可能上千件**：`--limit` 截断，且会在报告里说明截断了。
- **Blender 路径**：`--blender <PATH>` 或环境变量 `RSI3D_BLENDER`。
  ⚠️ Blender 的**退出码 0 不等于成功**——要查产物是否真的生成。

## 节点 id 与往返

- GLB 节点名 = 我们的节点 id，往返后 id 稳定，所以命令日志在导入产物上照样可用。
- 导入的节点默认带 `provenance`；**可编辑性**由层推导与生产者声明共同决定（见 [discipline.md](./discipline.md)）。

## 第三方验收（重要）

**我们自己的测试证明不了互操作性**。对外格式（glTF / 包清单 / 流协议）必须用**第三方实现**验收一次：

```bash
npx @gltf-transform/cli inspect model.gltf     # 顶点数、包围盒、材质
```

历史教训：`scenes[0].nodes` 写成空数组时，我们的测试与浏览器客户端**全绿**，
但标准加载器读到的是空场景（包围盒 `Infinity`）。所以导出后请用上面这条命令复核一次。
