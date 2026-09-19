// rsi3d-harness 浏览器客户端：两条流并排，把设计意图直接摆在屏幕上。
//
//   左：场景流（glTF 快照 + 增量）→ 本地 three.js 渲染，随便转，不花服务端算力
//   右：图像流（PNG 帧）        → 服务端渲染的权威观测，旁边挂着遮挡率等测量值
//
// 为什么两路都连：它们不是"二选一"，而是两种代价/两种用途。
// 场景流带得走（资产到了客户端），图像流带得走结论（可复现、能当证据）。

const qs = new URLSearchParams(location.search);
let TOKEN = qs.get('token') || '';
let view = qs.get('view') || 'iso-sw';

const $ = (id) => document.getElementById(id);
const hex = (s) => (s || '').slice(0, 8);

// ---------------------------------------------------------------- 令牌
if (!TOKEN) $('gate').classList.add('show');
$('token-go').onclick = () => {
  TOKEN = $('token-input').value.trim();
  if (!TOKEN) return;
  history.replaceState(null, '', '?token=' + encodeURIComponent(TOKEN));
  $('gate').classList.remove('show');
  boot();
};
$('token-input').addEventListener('keydown', (e) => { if (e.key === 'Enter') $('token-go').click(); });

// ---------------------------------------------------------------- 上行
async function post(path, body) {
  const res = await fetch(`${path}?token=${encodeURIComponent(TOKEN)}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  let out = {};
  try { out = await res.json(); } catch { /* 空体 */ }
  return { status: res.status, out };
}

// 命令一律带 reason：它会进内核的归因表（谁改的、为什么改）。
const ACTIONS = {
  'move-out': () => ({ op: 'transform', target: 'obj:sofa_01',
    params: { translate: [0, 0, 1.3] }, reason: '沙发挡住窗户采光，挪出挡光带' }),
  'move-back': () => ({ op: 'transform', target: 'obj:sofa_01',
    params: { translate: [0, 0, -1.3] }, reason: '挪回窗下（对照实验）' }),
  'dim': () => ({ op: 'set_light', target: 'sun', params: { intensity: 1.0 },
    reason: '主光过曝，压到 1.0' }),
  'undo': () => ({ op: 'checkout', params: { rev: Math.max(0, localRev - 1) },
    reason: '撤销一步（跳到上一版的完整状态）' }),
  'rollback': () => ({ op: 'checkout', params: { rev: 1 },
    reason: '回滚到 rev 1：只挪沙发那一版' }),
};

// ---------------------------------------------------------------- 场景流
let localRev = 0;
let patches = 0, snapshots = 0, frames = 0;
let three = null, renderer = null, scene3 = null, camera3 = null, meshes = new Map();
let windowPlane = null;
let room = [4.2, 2.8, 6.0];
let orbit = { az: -0.9, el: 0.45, dist: 8 };

async function loadThree() {
  // 从 CDN 取；取不到（离线/被拦）就**明确降级**到图像流（而不是白屏）。
  // 一定要带超时：CDN 卡住时不能把整个客户端拖死。
  const timeout = new Promise((_, reject) => setTimeout(() => reject(new Error('timeout')), 4000));
  try {
    const mod = await Promise.race([
      import('https://unpkg.com/three@0.160.0/build/three.module.js'),
      timeout,
    ]);
    return mod;
  } catch (e) {
    console.warn('three.js 不可用：', e.message);
    return null;
  }
}

function cameraFromOrbit() {
  const d = orbit.dist;
  const x = d * Math.cos(orbit.el) * Math.cos(orbit.az);
  const y = d * Math.sin(orbit.el);
  const z = d * Math.cos(orbit.el) * Math.sin(orbit.az);
  camera3.position.set(x, y, z);
  camera3.lookAt(0, 1.0, 0);
}

function colorOf(node) {
  const c = node?.extras?.rsi3d?.color;
  if (Array.isArray(c) && c.length === 3) {
    return (c[0] << 16) | (c[1] << 8) | c[2];
  }
  return 0x9aa4b2;
}

function upsertNode(node) {
  if (!three || !scene3) return;
  const id = node.name;
  const [x, y, z] = node.translation;
  const [sx, sy, sz] = node.scale;
  let mesh = meshes.get(id);
  if (!mesh) {
    mesh = new three.Mesh(
      new three.BoxGeometry(1, 1, 1),
      new three.MeshStandardMaterial({ color: colorOf(node), roughness: 0.85, metalness: 0 })
    );
    scene3.add(mesh);
    meshes.set(id, mesh);
  }
  mesh.position.set(x, y, z);
  mesh.scale.set(sx, sy, sz);
  mesh.material.color.setHex(colorOf(node));
}

function removeNode(id) {
  const mesh = meshes.get(id);
  if (mesh) { scene3.remove(mesh); meshes.delete(id); }
}

function buildScene() {
  if (!three || renderer) return;
  const canvas = $('c');
  renderer = new three.WebGLRenderer({ canvas, antialias: true, alpha: false });
  renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
  renderer.setSize(canvas.clientWidth, canvas.clientHeight, false);
  scene3 = new three.Scene();
  scene3.background = new three.Color(0x0e1116);
  camera3 = new three.PerspectiveCamera(45, canvas.clientWidth / canvas.clientHeight, 0.1, 200);

  scene3.add(new three.HemisphereLight(0xdfe8f5, 0x2a3038, 1.1));
  const dir = new three.DirectionalLight(0xfff0dd, 1.4);
  dir.position.set(3, 6, -4);
  scene3.add(dir);

  // 地面网格：给"房间"一个可读的空间参照
  scene3.add(new three.GridHelper(12, 24, 0x2b3340, 0x1c222a));
  cameraFromOrbit();
  renderLoop();

  // 本地轨道：拖拽/滚轮只动本地相机——这正是"场景流不花服务端算力"的意思
  let dragging = false, last = [0, 0];
  canvas.addEventListener('pointerdown', (e) => { dragging = true; last = [e.clientX, e.clientY]; });
  addEventListener('pointerup', () => { dragging = false; });
  addEventListener('pointermove', (e) => {
    if (!dragging) return;
    orbit.az -= (e.clientX - last[0]) * 0.008;
    orbit.el = Math.max(-0.2, Math.min(1.4, orbit.el + (e.clientY - last[1]) * 0.006));
    last = [e.clientX, e.clientY];
    cameraFromOrbit();
  });
  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    orbit.dist = Math.max(2, Math.min(40, orbit.dist * (1 + Math.sign(e.deltaY) * 0.1)));
    cameraFromOrbit();
  }, { passive: false });

  addEventListener('resize', () => {
    if (!renderer) return;
    renderer.setSize(canvas.clientWidth, canvas.clientHeight, false);
    camera3.aspect = canvas.clientWidth / canvas.clientHeight;
    camera3.updateProjectionMatrix();
  });
}

function renderLoop() {
  requestAnimationFrame(renderLoop);
  if (renderer && scene3 && camera3) renderer.render(scene3, camera3);
}

/// 窗（只画位置：房间是一层壳，看得见朝向就够）。
/// 注意数据来源：**快照的 extras**，不是 welcome（welcome 里没有场景元数据）。
function drawWindow(ex) {
  if (!three || !scene3 || !ex || !ex.window) return;
  const win = ex.window;
  if (!Array.isArray(win.spanX)) return;
  if (windowPlane) { scene3.remove(windowPlane); windowPlane = null; }
  const w = Math.abs(win.spanX[1] - win.spanX[0]);
  const h = win.height || 1.6;
  windowPlane = new three.Mesh(
    new three.PlaneGeometry(w, h),
    new three.MeshBasicMaterial({ color: 0x8ecdf7, transparent: true, opacity: 0.55,
      side: three.DoubleSide })
  );
  windowPlane.position.set((win.spanX[0] + win.spanX[1]) / 2, h / 2, win.z);
  scene3.add(windowPlane);
}

function applySnapshot(gltf) {
  // three 还没就绪（正在加载或加载失败）：先存着，等能画了再画。
  // **不能因此丢掉数据**——流已经收到了，丢的就只是显示。
  if (!three || !scene3) {
    pendingSnapshot = gltf;
    return;
  }
  for (const id of [...meshes.keys()]) removeNode(id);
  for (const n of gltf.nodes || []) upsertNode(n);
  const ex = gltf?.extras?.rsi3d;
  if (Array.isArray(ex?.room?.size)) room = ex.room.size;
  drawWindow(ex);
  if (ex && Array.isArray(ex.window_blockers)) {
    blockers = ex.window_blockers;
  }
  orbit.dist = Math.min(orbit.dist, Math.max(6, Math.max(...room) * 1.6));
  cameraFromOrbit();
}

function applyPatch(changes) {
  for (const id of changes.nodes_remove || []) removeNode(id);
  for (const n of changes.nodes_upsert || []) upsertNode(n);
  // 挡窗者这类**场景级事实**随增量一起更新（不能只看几何）
  if (Array.isArray(changes.blockers)) blockers = changes.blockers;
}

function connectScene() {
  const es = new EventSource(`/stream/scene?token=${encodeURIComponent(TOKEN)}`);
  const dot = $('scene-dot');
  es.onopen = () => { dot.className = 'dot on'; };
  es.onerror = () => {
    // EventSource 会自己重连，并带上 Last-Event-ID —— 服务端据此发"差的那一段"
    dot.className = 'dot err';
    note('连接中断，正在自动重连（会带 Last-Event-ID 续传）…');
  };
  es.addEventListener('welcome', (ev) => {
    const w = JSON.parse(ev.data);
    serverRev = w.revision;
    buildScene();
    // 续传成功时**不重建**场景：客户端已经有底子了
    note(w.resumed
      ? `已续传：你看到 rev ${localRev}，服务端在 rev ${w.revision}，只补差量`
      : `几何档次：${w.geometry}（H0 还没有真网格，节点是包围盒代理）`);
    paint();
  });

  es.addEventListener('snapshot', (ev) => {
    const m = JSON.parse(ev.data);
    applySnapshot(m.gltf);
    localRev = m.revision;
    snapshots++;
    $('snapshots').textContent = snapshots;
    paint();
  });
  es.addEventListener('patch', (ev) => {
    const m = JSON.parse(ev.data);
    applyPatch(m.changes || {});
    localRev = m.to;
    patches++;
    $('patches').textContent = patches;
    paint();
  });
  return es;
}

// ---------------------------------------------------------------- 图像流
let frameES = null;
function connectFrame() {
  if (frameES) frameES.close();
  frameES = new EventSource(
    `/stream/frame?token=${encodeURIComponent(TOKEN)}&view=${encodeURIComponent(view)}`
  );
  const dot = $('frame-dot');
  frameES.onopen = () => { dot.className = 'dot on'; };
  frameES.onerror = () => { dot.className = 'dot err'; };
  frameES.addEventListener('frame', (ev) => {
    const m = JSON.parse(ev.data);
    $('frame').src = 'data:image/png;base64,' + m.png_base64;
    frames++;
    $('f-view').textContent = m.view;
    $('f-rev').textContent = m.revision;
    $('f-hash').textContent = hex(m.image_hash);
    $('f-count').textContent = frames;
    $('f-occ').textContent = m.band_occlusion == null
      ? '–' : (m.band_occlusion * 100).toFixed(1) + '%';
    $('f-occ').className = m.band_occlusion > 0.2 ? 'bad' : 'ok';
    serverRev = m.revision;
    paint();
  });
}

// ---------------------------------------------------------------- 显示
let serverRev = 0;
function paint() {
  $('rev').textContent = `${localRev} / ${serverRev}`;
  const behind = localRev !== serverRev;
  $('rev').className = behind ? 'warn' : '';
  $('hash').textContent = hex(lastHash);
}

let lastHash = '';
let blockers = [];
/// three 未就绪时暂存的快照（就绪后补画）
let pendingSnapshot = null;
/// three.js 彻底不可用？——那是要一直显示在屏幕上的事实，不能被后续文案盖掉
let threeFailed = false;

function note(text) {
  if (threeFailed) return; // 已经明确降级了，别再糊上别的字
  $('scene-note').textContent = text;
}
setInterval(async () => {
  try {
    const r = await fetch('/healthz');
    const h = await r.json();
    lastHash = h.scene_hash;
    $('conns').textContent = `连接 ${h.connections} · 已发 ${h.messages_sent} · 帧 ${h.frames_sent}`;
    if (localRev === 0) localRev = h.revision;
    $('rev').textContent = `${localRev === h.revision ? '' : localRev + ' / '}${h.revision}`;
    $('hash').textContent = hex(h.scene_hash);
  } catch { /* 服务停了 */ }
  try {
    // 浏览器里看到的告警必须和 Agent 看到的一致（同一个 observe）
    const r = await fetch(`/observe?token=${encodeURIComponent(TOKEN)}`);
    const o = await r.json();
    const n = (o.warnings || []).length;
    $('alerts').textContent = n ? `告警 ${n}` : '无告警';
    $('alerts').className = 'stat ' + (n ? 'warn' : 'ok');
    const bl = o.window_blockers || [];
    $('scene-note').title = bl.length ? `挡窗者：${bl.join(', ')}` : '无挡窗者';
  } catch { /* ignore */ }
}, 2000);

// ---------------------------------------------------------------- 启动
document.querySelectorAll('[data-view]').forEach((b) => {
  b.onclick = () => {
    view = b.dataset.view;
    connectFrame();
  };
});

for (const [id, mk] of Object.entries(ACTIONS)) {
  $(id).onclick = async () => {
    const { status, out } = await post('/command', mk());
    if (status >= 400) {
      $('err').textContent = `${out.error || status}: ${out.message || ''}`;
      $('err').className = 'stat bad';
    } else {
      $('err').textContent = '';
    }
  };
}

async function boot() {
  // 顺序很重要：**先把两条流接上**，再去拿渲染库。
  // 流是本服务自己的东西（永远在），three.js 是 CDN 上的第三方（可能拉不到、可能很慢）；
  // 把后者放到前面 await，等于让第三方决定我们能不能工作。
  connectScene();
  connectFrame();

  three = await loadThree();
  if (three) {
    buildScene();
    if (pendingSnapshot) {
      applySnapshot(pendingSnapshot);
      pendingSnapshot = null;
    }
  } else {
    threeFailed = true;
    $('scene-note').textContent =
      'three.js 没能加载（离线/CDN 不可达）：场景流仍在接收，但左侧无法渲染——请看右侧服务端渲染的图像流';
  }
}

if (TOKEN) boot();
