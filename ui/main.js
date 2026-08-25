const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const PRESETS = {
  deepseek: { name: 'DeepSeek', base_url: 'https://api.deepseek.com/anthropic', key_type: 'api_key' },
  zhipu: { name: '智谱 GLM', base_url: 'https://open.bigmodel.cn/api/anthropic', key_type: 'api_key' },
  kimi: { name: 'Kimi', base_url: 'https://api.kimi.com/coding/', key_type: 'api_key' },
  openrouter: { name: 'OpenRouter', base_url: 'https://openrouter.ai/api/v1', key_type: 'api_key' },
};

const SVG_EDIT = '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 3a2.8 2.8 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5z"/></svg>';
const SVG_DEL = '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/></svg>';
const SVG_CHECK = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';
const SVG_PLAY = '<svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>';
const SVG_TEST = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2 3 14h7l-1 8 10-12h-7z"/></svg>';

let state = null;

const $ = (id) => document.getElementById(id);

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[c]));
}

function badgeColor(name) {
  let h = 0;
  for (const ch of name) h = (h * 31 + ch.codePointAt(0)) % 360;
  return `hsl(${h}, 62%, 46%)`;
}

function hostOf(url) {
  try { return new URL(url).host; } catch { return url; }
}

function applyTheme(t) {
  document.documentElement.dataset.theme = t;
  localStorage.setItem('cs-router-theme', t);
}

/** toast：底部居中浮现，成功与错误带色 */
function toast(message, type = 'info') {
  const el = document.createElement('div');
  el.className = `toast ${type}`;
  el.textContent = message;
  $('toasts').appendChild(el);
  setTimeout(() => el.remove(), 3200);
}

/* ---------- 主界面 ---------- */

function render() {
  renderCards();
  renderDaemonBar();
  const custom = !!(state && state.settings && state.settings.custom_controls);
  $('titlebar').classList.toggle('hidden', !custom);
  document.documentElement.style.setProperty('--titlebar-h', custom ? '34px' : '0px');
}

function renderDaemonBar() {
  if (!state) return;
  const running = !!(state.status && state.status.running);
  const port = state.status && state.status.port ? ` · 端口 ${state.status.port}` : '';
  $('daemon-dot').className = `daemon-dot ${running ? 'running' : 'stopped'}`;
  $('daemon-label').textContent = running ? `守护进程运行中${port}` : '守护进程已停止';
  $('btn-daemon-launch').classList.toggle('hidden', running);
  $('btn-daemon-stop').classList.toggle('hidden', !running);
}

function renderCards() {
  if (!state) return;
  const box = $('cards');
  box.innerHTML = '';
  for (const p of state.providers) {
    const official = !p.base_url;
    const current = p.id === state.current;
    const card = document.createElement('div');
    card.className = `card${current ? ' current' : ''}`;
    const badgeChar = [...p.name][0] || '?';
    const badge = official
      ? '<span style="color:var(--muted-fg)">C</span>'
      : `<span style="color:${badgeColor(p.name)}">${esc(badgeChar)}</span>`;
    const sub = official
      ? '<span class="purl muted">claude.ai · official</span>'
      : `<span class="purl">${esc(hostOf(p.base_url))} · ${(p.models || []).length || 1} 个模型</span>`;
    const mainBtn = current
      ? `<button class="btn sm inuse" disabled>${SVG_CHECK}使用中</button>`
      : `<button class="btn sm primary act-switch">${SVG_PLAY}切换</button>`;
    card.innerHTML = `
      <div class="card-glow"></div>
      <div class="card-row">
        <div class="picon">${badge}</div>
        <div class="pinfo">
          <div class="pname"><span class="nm">${esc(p.name)}</span></div>
          ${sub}
        </div>
        <div class="pside">
          <span class="pactions">
            ${mainBtn}
            ${official ? '' : `<button class="icon-btn act act-edit" title="编辑">${SVG_EDIT}</button>`}
            ${official ? '' : `<button class="btn sm ghost act-test" title="连通测试">${SVG_TEST}测试</button>`}
            <button class="icon-btn act act-del" title="删除">${SVG_DEL}</button>
          </span>
        </div>
      </div>`;
    card.addEventListener('click', (e) => {
      if (e.target.closest('.act-edit')) { openEdit(p); return; }
      if (e.target.closest('.act-del')) { del(p); return; }
      const testBtn = e.target.closest('.act-test');
      if (testBtn) { runTest(p, testBtn); return; }
      if (e.target.closest('.act-switch')) {
        // 乐观切换：界面立即变更为当前项，后台完成守护进程重启
        if (state.current === p.id) return;
        state.current = p.id;
        render();
        invoke('switch_provider', { id: p.id })
          .then(() => {
            if (state.settings && state.settings.manual_daemon) {
              toast(`已切换到 ${p.name}，请手动重启守护进程`, 'info');
            } else {
              toast(`已切换到 ${p.name}`, 'success');
            }
          })
          .catch((err) => { toast(`切换失败：${err}`, 'error'); reload(); });
      }
    });
    box.appendChild(card);
  }
}

async function runTest(p, btn) {
  const original = btn.innerHTML;
  btn.disabled = true;
  btn.innerHTML = `${SVG_TEST}测试中`;
  try {
    const result = await invoke('test_provider', { provider: p });
    toast(`${p.name} · ${result}`, 'success');
  } catch (err) {
    toast(`${p.name} 测试失败：${err}`, 'error');
  }
  setTimeout(() => {
    btn.disabled = false;
    btn.innerHTML = original;
  }, 800);
}

function del(p) {
  if (!confirm(`确认删除供应商「${p.name}」？`)) return;
  invoke('delete_provider', { id: p.id })
    .then(() => { toast('已删除', 'success'); reload(); })
    .catch((err) => toast(`删除失败：${err}`, 'error'));
}

async function reload() {
  state = await invoke('get_state');
  render();
}

/* ---------- 编辑页：currentProvider 是唯一事实源，文本区即其序列化 ---------- */

let currentProvider = null;

function normalizeRole(r) {
  if (typeof r === 'string') return { model: r, display: '' };
  if (r && typeof r === 'object') {
    return { model: typeof r.model === 'string' ? r.model : '', display: typeof r.display === 'string' ? r.display : '' };
  }
  return { model: '', display: '' };
}

function normalizeProvider(p) {
  return {
    id: typeof p.id === 'string' ? p.id : '',
    name: typeof p.name === 'string' ? p.name : '',
    notes: typeof p.notes === 'string' ? p.notes : '',
    website: typeof p.website === 'string' ? p.website : '',
    base_url: typeof p.base_url === 'string' ? p.base_url : '',
    key_type: p.key_type === 'auth_token' ? 'auth_token' : 'api_key',
    api_format: typeof p.api_format === 'string' && p.api_format ? p.api_format : 'anthropic',
    api_key: typeof p.api_key === 'string' ? p.api_key : '',
    model: typeof p.model === 'string' ? p.model : '',
    models: Array.isArray(p.models) ? p.models.filter((m) => typeof m === 'string') : [],
    models_url: typeof p.models_url === 'string' ? p.models_url : '',
    roles: {
      sonnet: normalizeRole(p.roles && p.roles.sonnet),
      opus: normalizeRole(p.roles && p.roles.opus),
      haiku: normalizeRole(p.roles && p.roles.haiku),
      fable: normalizeRole(p.roles && p.roles.fable),
    },
  };
}

function autoSizeJson() {
  const t = $('f-json');
  t.style.height = 'auto';
  t.style.height = `${Math.max(54, t.scrollHeight + 2)}px`;
  const lines = t.value.split('\n').length;
  let nums = '';
  for (let i = 1; i <= lines; i += 1) nums += `${i}\n`;
  $('json-gutter').textContent = nums;
}

/** JSON 语法高亮：按字段切词渲染，键蓝、字符串绿、数字褐、布尔空值红 */
function highlightJson(text) {
  const rx = /("(?:\\.|[^"\\])*")(\s*:)?|\b(true|false|null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g;
  let out = '';
  let last = 0;
  let m;
  while ((m = rx.exec(text)) !== null) {
    out += esc(text.slice(last, m.index));
    if (m[1]) {
      out += m[2]
        ? `<span class="tk-k">${esc(m[1])}</span>${esc(m[2])}`
        : `<span class="tk-s">${esc(m[1])}</span>`;
    } else if (m[3]) {
      out += `<span class="tk-kw">${m[3]}</span>`;
    } else {
      out += `<span class="tk-n">${m[4]}</span>`;
    }
    last = m.index + m[0].length;
  }
  out += esc(text.slice(last));
  return out;
}

function updateJsonHighlight() {
  $('json-hl').innerHTML = `${highlightJson($('f-json').value)}\n`;
}

/** 文本区始终渲染事实源对象 */
function renderJson() {
  $('f-json').value = JSON.stringify(currentProvider, null, 2);
  updateJsonHighlight();
  autoSizeJson();
}

function parseJsonProvider(text) {
  const obj = JSON.parse(text);
  if (typeof obj !== 'object' || obj === null || Array.isArray(obj)) {
    throw new Error('顶层必须是对象');
  }
  return normalizeProvider(obj);
}

function openEdit(p) {
  currentProvider = normalizeProvider(p || {});
  $('edit-overlay').classList.remove('hidden');
  $('btn-back').classList.remove('hidden');
  $('edit-title').textContent = p ? '编辑供应商' : '添加供应商';
  $('f-name').value = currentProvider.name;
  $('f-notes').value = currentProvider.notes;
  $('f-website').value = currentProvider.website;
  $('f-apiformat').value = currentProvider.api_format;
  $('f-url').value = currentProvider.base_url;
  $('f-key').value = currentProvider.api_key;
  $('f-keytype').value = currentProvider.key_type;
  $('f-models-url').value = currentProvider.models_url;
  renderCatalogControls();
  renderJson();
}

function closeEdit() {
  $('edit-overlay').classList.add('hidden');
  $('btn-back').classList.add('hidden');
  currentProvider = null;
}

/** 芯片、角色下拉、兜底模型的统一刷新；改动由各控件写穿事实源后调用 */
function renderCatalogControls() {
  const cur = currentProvider;
  const chips = $('model-chips');
  chips.innerHTML = '';
  for (const m of cur.models) {
    const chip = document.createElement('span');
    chip.className = `mchip${m === cur.model ? ' default' : ''}`;
    chip.title = m === cur.model ? '默认模型' : '点击设为默认';
    const name = document.createElement('span');
    name.textContent = m;
    name.style.cursor = 'pointer';
    name.addEventListener('click', () => pickModel(m));
    const rm = document.createElement('button');
    rm.type = 'button';
    rm.title = '移除';
    rm.textContent = '×';
    rm.addEventListener('click', () => {
      cur.models = cur.models.filter((x) => x !== m);
      if (cur.model === m) cur.model = cur.models[0] || '';
      renderCatalogControls();
      renderJson();
    });
    chip.appendChild(name);
    chip.appendChild(rm);
    chips.appendChild(chip);
  }
  if (!cur.models.length) {
    const empty = document.createElement('span');
    empty.className = 'hint';
    empty.textContent = '清单为空，点右上角获取模型列表';
    chips.appendChild(empty);
  }
  for (const role of ['sonnet', 'opus', 'haiku', 'fable']) {
    const sel = $(`r-${role}`);
    const disp = $(`rd-${role}`);
    const bound = cur.roles[role].model || '';
    sel.innerHTML = '';
    const none = document.createElement('option');
    none.value = '';
    none.textContent = '默认模型';
    sel.appendChild(none);
    for (const m of cur.models) {
      const opt = document.createElement('option');
      opt.value = m;
      opt.textContent = m;
      sel.appendChild(opt);
    }
    sel.value = cur.models.includes(bound) ? bound : '';
    sel.onchange = () => { cur.roles[role].model = sel.value; renderJson(); };
    disp.value = cur.roles[role].display;
    disp.oninput = () => { cur.roles[role].display = disp.value; renderJson(); };
  }
  const defSel = $('f-model');
  defSel.innerHTML = '';
  const ensureOpt = (m) => {
    const o = document.createElement('option');
    o.value = m;
    o.textContent = m;
    defSel.appendChild(o);
  };
  if (cur.model && !cur.models.includes(cur.model)) ensureOpt(cur.model);
  for (const m of cur.models) ensureOpt(m);
  defSel.value = cur.model || '';
  defSel.onchange = () => pickModel(defSel.value);
}

function pickModel(m) {
  currentProvider.model = m || '';
  renderCatalogControls();
  renderJson();
}

async function fetchModels() {
  toast('正在获取模型列表…');
  try {
    const ids = await invoke('fetch_models', {
      baseUrl: $('f-url').value.trim(),
      apiKey: $('f-key').value.trim(),
      keyType: $('f-keytype').value,
      modelsUrl: $('f-models-url').value.trim(),
    });
    currentProvider.models = ids;
    if (!currentProvider.model || !ids.includes(currentProvider.model)) {
      currentProvider.model = ids[0] || '';
    }
    renderCatalogControls();
    renderJson();
    toast(`获得 ${ids.length} 个模型`, 'success');
  } catch (err) {
    toast(String(err), 'error');
  }
}

/** 一键设置：把当前兜底模型填入四角色的实际请求模型 */
function applyRoles() {
  if (!currentProvider.model) return;
  for (const role of ['sonnet', 'opus', 'haiku', 'fable']) {
    currentProvider.roles[role].model = currentProvider.model;
  }
  renderCatalogControls();
  renderJson();
}

/** 保存以文本区为准；解析失败用 toast 拦截 */
function saveProvider() {
  let p;
  try {
    p = parseJsonProvider($('f-json').value);
  } catch (err) {
    toast(`JSON 无法保存：${err.message}`, 'error');
    return;
  }
  if (!p.name) { toast('名称不能为空', 'error'); return; }
  // 接口地址为空即官方式条目；填了地址则密钥必填
  if (p.base_url && !p.api_key) { toast('密钥不能为空', 'error'); return; }
  invoke('save_provider', { provider: p })
    .then(() => { toast('已保存', 'success'); closeEdit(); reload(); })
    .catch((err) => { toast(String(err), 'error'); });
}

/* ---------- 设置 ---------- */

function openSettings() {
  const s = state.settings;
  $('s-close').value = s.close_action || 'tray';
  $('s-autostart').checked = !!s.autostart;
  $('s-fastfail').checked = !!s.fast_fail;
  $('s-controls').checked = s.custom_controls !== false;
  $('s-proxy').value = s.daemon_proxy || '';
  $('s-manual').checked = !!s.manual_daemon;
  $('settings-overlay').classList.remove('hidden');
}

function saveSettings() {
  invoke('save_settings', {
    settings: {
      close_action: $('s-close').value,
      autostart: $('s-autostart').checked,
      fast_fail: $('s-fastfail').checked,
      custom_controls: $('s-controls').checked,
      daemon_proxy: $('s-proxy').value.trim(),
      manual_daemon: $('s-manual').checked,
    },
  })
    .then(() => {
      toast('设置已保存', 'success');
      $('settings-overlay').classList.add('hidden');
      reload();
    })
    .catch((err) => toast(`保存设置失败：${err}`, 'error'));
}

/* ---------- 事件装配 ---------- */

const TEXT_FIELDS = {
  'f-name': 'name',
  'f-notes': 'notes',
  'f-website': 'website',
  'f-url': 'base_url',
  'f-key': 'api_key',
  'f-models-url': 'models_url',
};

window.addEventListener('DOMContentLoaded', () => {
  applyTheme(localStorage.getItem('cs-router-theme')
    || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'));
  $('theme-toggle').addEventListener('click', () => {
    applyTheme(document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark');
  });

  $('btn-add').addEventListener('click', () => openEdit(null));
  $('btn-settings').addEventListener('click', openSettings);

  $('btn-daemon-launch').addEventListener('click', () => {
    $('btn-daemon-launch').disabled = true;
    invoke('launch_daemon')
      .then(() => { toast('守护进程已启动', 'success'); reload(); })
      .catch((err) => { toast(`启动失败：${err}`, 'error'); })
      .finally(() => { $('btn-daemon-launch').disabled = false; });
  });
  $('btn-daemon-stop').addEventListener('click', () => {
    $('btn-daemon-stop').disabled = true;
    invoke('stop_daemon')
      .then(() => { toast('守护进程已停止', 'info'); reload(); })
      .catch((err) => { toast(`停止失败：${err}`, 'error'); })
      .finally(() => { $('btn-daemon-stop').disabled = false; });
  });

  $('btn-back').addEventListener('click', closeEdit);
  $('btn-cancel').addEventListener('click', closeEdit);
  $('btn-save').addEventListener('click', saveProvider);
  $('edit-form').addEventListener('submit', (e) => e.preventDefault());
  $('btn-eye').addEventListener('click', () => {
    const k = $('f-key');
    const show = k.type === 'password';
    k.type = show ? 'text' : 'password';
    $('icon-eye').classList.toggle('hidden', show);
    $('icon-eyeoff').classList.toggle('hidden', !show);
  });

  // 表单写穿事实源：文本输入直接改字段，角色显示名并入 roles
  const fsBody = document.querySelector('.fs-body');
  fsBody.addEventListener('input', (e) => {
    if (!currentProvider) return;
    const id = e.target.id || '';
    if (id === 'f-json') {
      // 文本区手改即事实，同步高亮与高度，保存时校验
      updateJsonHighlight();
      autoSizeJson();
      return;
    }
    if (TEXT_FIELDS[id]) {
      currentProvider[TEXT_FIELDS[id]] = e.target.value.trim();
      renderJson();
      return;
    }
    if (id.startsWith('rd-')) {
      currentProvider.roles[id.slice(3)].display = e.target.value;
      renderJson();
    }
  });
  fsBody.addEventListener('change', (e) => {
    if (!currentProvider) return;
    const id = e.target.id || '';
    if (id === 'f-json') return;
    if (id === 'f-keytype') { currentProvider.key_type = e.target.value; renderJson(); return; }
    if (id === 'f-apiformat') { currentProvider.api_format = e.target.value; renderJson(); }
  });

  $('btn-format-json').addEventListener('click', () => {
    try {
      const obj = JSON.parse($('f-json').value);
      currentProvider = normalizeProvider(obj);
      renderJson();
      toast('格式化成功', 'success');
    } catch (err) {
      toast(`格式化失败：${err.message}`, 'error');
    }
  });
  $('btn-apply-roles').addEventListener('click', applyRoles);
  $('btn-fetch-models').addEventListener('click', fetchModels);

  $('presets').addEventListener('click', (e) => {
    const chip = e.target.closest('.chip');
    if (!chip || !currentProvider) return;
    const preset = PRESETS[chip.dataset.preset];
    if (!preset) return;
    currentProvider.name = preset.name;
    currentProvider.base_url = preset.base_url;
    currentProvider.key_type = preset.key_type;
    $('f-name').value = preset.name;
    $('f-url').value = preset.base_url;
    $('f-keytype').value = preset.key_type;
    renderJson();
  });

  $('s-cancel').addEventListener('click', () => $('settings-overlay').classList.add('hidden'));
  $('s-save').addEventListener('click', saveSettings);

  $('wc-min').addEventListener('click', () => invoke('window_control', { action: 'min' }).catch(() => {}));
  $('wc-max').addEventListener('click', () => invoke('window_control', { action: 'max' }).catch(() => {}));
  $('wc-close').addEventListener('click', () => invoke('window_control', { action: 'close' }).catch(() => {}));

  $('edit-overlay').addEventListener('click', (e) => {
    if (e.target === $('edit-overlay')) closeEdit();
  });
  $('settings-overlay').addEventListener('click', (e) => {
    if (e.target === $('settings-overlay')) $('settings-overlay').classList.add('hidden');
  });

  listen('state', (e) => { state = e.payload; render(); });
  listen('status', (e) => {
    if (state) { state.status = e.payload; renderDaemonBar(); }
  });

  reload();
});
