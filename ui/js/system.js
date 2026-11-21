// The System pages: an overview, CPU / GPU / memory details, processes,
// startup apps, services, drives & network, sensors, history and PC specs.
//
// Readings are only taken while a page needs them: opening a page tells
// Lumora what to watch, and leaving it (or hiding the window) stops that.

import { drawChart, sparkPath, COLORS } from './syschart.js';

let ctx = null;

const PAGES = [
  ['overview', 'Overview', 'gauge'],
  ['cpu', 'CPU', 'cpu'],
  ['gpu', 'GPU', 'gpu'],
  ['memory', 'Memory', 'ram'],
  ['processes', 'Processes', 'list'],
  ['startup', 'Startup apps', 'rocket'],
  ['services', 'Services', 'cog'],
  ['storage', 'Drives & network', 'storage'],
  ['sensors', 'Sensors', 'thermo'],
  ['history', 'History', 'clock'],
  ['specs', 'PC specs', 'info'],
];

// What each page asks Lumora to sample.
const WATCH = {
  overview: ['overview', 'processes'],
  cpu: ['detail'],
  gpu: ['detail'],
  memory: ['detail'],
  processes: ['processes'],
  storage: ['storage'],
  sensors: ['sensors'],
};

const LIVE_KEEP = 2 * 60 * 1000 + 5000;

const sys = {
  page: null,
  sample: null,
  sensors: [],
  procs: null,
  live: [], // {at, values: {cpu, gpu, ram, down, up}}
  watching: new Set(),
  sort: { key: 'cpu', dir: -1 },
  search: '',
  open: new Set(),
  picked: null,
  startup: null,
  services: null,
  svcFilter: 'all',
  svcSearch: '',
  storage: null,
  specs: null,
  plans: null,
  freed: '',
  history: { minutes: 60, points: [], timer: 0 },
  minmax: new Map(),
  error: {},
};

const $ = (s) => document.querySelector(s);
const esc = (s) => ctx.esc(s ?? '');

export const isSystem = (id) => !!id && id.startsWith('sys:');
const pageOf = (id) => (isSystem(id) ? id.slice(4) : null);

export function initSystem(context) {
  ctx = context;
  ctx.api.listen('lumora://system', (e) => onSample(e.payload));
  ctx.api.listen('lumora://processes', (e) => onProcesses(e.payload));
  // The overlay was shown or hidden from the tray or a hotkey.
  ctx.api.listen('lumora://overlay', (e) => {
    ctx.state.app.settings.overlay = { ...(ctx.state.app.settings.overlay || {}), shown: !!e.payload };
    ctx.rerender();
    const sw = document.getElementById('overlayOn');
    if (sw) sw.checked = !!e.payload;
  });
  document.addEventListener('visibilitychange', syncWatch);
  document.addEventListener('click', onClick);
  document.addEventListener('change', onChange);
  document.addEventListener('input', onInput);
  document.addEventListener('contextmenu', onContextMenu);
  document.addEventListener('dblclick', onDblClick);
  document.addEventListener('keydown', (ev) => {
    if (ev.key === 'Escape') closeMenu();
  });
  window.addEventListener('resize', () => {
    if (sys.page) redrawCharts();
  });
}

// --- Units ------------------------------------------------------------------------

const units = () => ctx.state.app.settings.units || { temp: 'c', memory: 'gb', network: 'mbps' };

export function temp(c, withUnit = true) {
  if (c == null) return '–';
  const f = units().temp === 'f';
  const v = f ? c * 9 / 5 + 32 : c;
  return `${Math.round(v)}${withUnit ? (f ? '°F' : '°C') : '°'}`;
}

export function mem(bytes, digits = 1) {
  if (bytes == null) return '–';
  if (units().memory === 'mb') return `${Math.round(bytes / 1048576).toLocaleString()} MB`;
  return `${(bytes / 1073741824).toFixed(digits)} GB`;
}

/** Process memory: MB under a gigabyte reads better either way. */
function procMem(bytes) {
  if (units().memory === 'gb' && bytes >= 1073741824) return `${(bytes / 1073741824).toFixed(1)} GB`;
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

export function rate(bytesPerSec) {
  if (bytesPerSec == null) return '–';
  if (units().network === 'mbs') {
    if (bytesPerSec < 1048576) return `${(bytesPerSec / 1024).toFixed(bytesPerSec < 10240 ? 1 : 0)} KB/s`;
    return `${(bytesPerSec / 1048576).toFixed(1)} MB/s`;
  }
  const bits = bytesPerSec * 8;
  if (bits < 1e6) return `${(bits / 1e3).toFixed(bits < 1e4 ? 1 : 0)} Kbps`;
  return `${(bits / 1e6).toFixed(bits < 1e8 ? 1 : 0)} Mbps`;
}

const diskRate = (b) => (b < 1048576 ? `${(b / 1024).toFixed(0)} KB/s` : `${(b / 1048576).toFixed(1)} MB/s`);
const gib = (b) => `${Math.round(b / 1073741824)} GB`;
const size = (b) => (b >= 1e12 ? `${(b / 1e12).toFixed(2)} TB` : `${Math.round(b / 1e9)} GB`);
const pct = (v, d = 0) => (v == null ? '–' : `${Number(v).toFixed(d)}%`);
const mhz = (v) => (v == null ? '–' : v >= 1000 ? `${(v / 1000).toFixed(2)} GHz` : `${v} MHz`);
const watts = (v) => (v == null ? '–' : `${Math.round(v)} W`);
function uptime(s) {
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  return d ? `${d}d ${h}h ${m}m` : h ? `${h}h ${m}m` : `${m}m`;
}

// --- Watching -----------------------------------------------------------------------

function syncWatch() {
  const want = new Set(document.hidden || !sys.page ? [] : WATCH[sys.page] || []);
  for (const w of sys.watching) if (!want.has(w)) ctx.api.invoke('monitor_watch', { what: w, on: false });
  for (const w of want) if (!sys.watching.has(w)) ctx.api.invoke('monitor_watch', { what: w, on: true });
  sys.watching = want;
  if (sys.page === 'history') startHistoryTimer();
}

/** Called when another (non-System) page opens. */
export function leave() {
  if (!sys.page) return;
  sys.page = null;
  closeMenu();
  clearInterval(sys.history.timer);
  sys.history.timer = 0;
  syncWatch();
}

function onSample(ev) {
  sys.sample = ev.sample;
  sys.sensors = ev.sensors;
  const s = ev.sample;
  sys.live.push({
    at: s.at,
    values: { cpu: s.cpu.usage, gpu: s.gpu?.usage ?? null, ram: s.memory.percent, down: sum(s.net, 'down'), up: sum(s.net, 'up') },
  });
  while (sys.live.length && sys.live[0].at < s.at - LIVE_KEEP) sys.live.shift();
  if (sys.page && !document.hidden) update();
}

const sum = (list, k) => (list || []).reduce((a, x) => a + x[k], 0);

/** Fills the live graphs from the history when a page opens. */
async function seedLive() {
  if (sys.live.length > 20) return;
  try {
    const pts = await ctx.api.invoke('history_range', { minutes: 3 });
    const since = sys.live.length ? sys.live[0].at : Infinity;
    const seeded = pts.filter((p) => p.at < since).map((p) => ({
      at: p.at, values: { cpu: p.v[3], gpu: p.v[4], ram: p.v[5], down: p.v[8], up: p.v[9] },
    }));
    sys.live = [...seeded, ...sys.live];
    if (sys.page) update();
  } catch { /* the live readings fill it in */ }
}

async function snapshot() {
  try {
    const s = await ctx.api.invoke('system_snapshot');
    if (s.sample && !sys.sample) {
      sys.sample = s.sample;
      sys.sensors = s.sensors;
      if (sys.page) update();
    }
  } catch { /* fine */ }
}

// --- Sidebar ------------------------------------------------------------------------

function collapsed() {
  try { return localStorage.getItem('lumora.systemCollapsed') === '1'; } catch { return false; }
}

export function sidebarHtml() {
  const closed = collapsed() && !isSystem(ctx.state.selected);
  const overlay = !!ctx.state.app.settings.overlay?.shown;
  return `<button class="group-title group-toggle" data-sys-collapse aria-expanded="${!closed}">System<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 10l5 5 5-5"/></svg></button>
    ${closed ? '' : PAGES.map(([id, name, ic]) => `<button class="nav-item" data-device="sys:${id}" aria-current="${ctx.state.selected === 'sys:' + id}">
      <span class="nav-icon">${ctx.icon(ic)}</span><span class="nav-name">${name}</span></button>`).join('')}
    <label class="nav-item nav-switch" title="An always-on-top window with your temperatures and usage (${esc(ctx.state.app.settings.hotkeys?.overlay || 'no hotkey')})">
      <span class="nav-icon">${ctx.icon('overlay')}</span><span class="nav-name">Mini overlay</span>
      <input type="checkbox" class="switch small" data-sys-overlay ${overlay ? 'checked' : ''}>
    </label>`;
}

// --- Page frame ----------------------------------------------------------------------

export function renderHead(id) {
  const page = pageOf(id);
  const [, name] = PAGES.find(([p]) => p === page) || [, 'System'];
  $('#stageTitle').textContent = name;
  const s = sys.sample;
  const sub = {
    overview: 'Live usage and temperatures for your whole PC.',
    cpu: s?.cpu.name || 'Processor',
    gpu: s?.gpu?.name || 'Graphics card',
    memory: s ? `${mem(s.memory.total, 0)} installed` : 'Memory',
    processes: 'Everything running right now. Right-click a process for more.',
    startup: 'Apps that open when you sign in. Switching one off works exactly like Task Manager.',
    services: 'Background services Windows runs.',
    storage: 'Your drives, their health, and network traffic.',
    sensors: 'Every reading Lumora has, with the lowest and highest since it started.',
    history: 'Readings kept for the last 24 hours, with a log saved each day.',
    specs: 'What\'s inside this PC.',
  }[page] || '';
  $('#stageSub').textContent = sub;
  const actions = $('#stageActions');
  actions.innerHTML = page === 'specs'
    ? `<button class="chip" data-sys-copy-specs>${ctx.icon('copy')}Copy specs</button>`
    : page === 'history'
      ? `<button class="chip" data-sys-export>${ctx.icon('download')}Export CSV</button>`
      : page === 'sensors'
        ? `<button class="chip" data-sys-reset-minmax>${ctx.icon('reset')}Reset min/max</button>`
        : page === 'services' || page === 'startup' || page === 'storage'
          ? `<button class="chip" data-sys-refresh>${ctx.icon('reset')}Refresh</button>`
          : '';
}

export function renderPage(id) {
  const page = pageOf(id);
  const changed = sys.page !== page;
  sys.page = page;
  if (changed) {
    closeMenu();
    clearInterval(sys.history.timer);
    sys.history.timer = 0;
  }
  syncWatch();
  const body = $('#stageBody');
  body.innerHTML = `<div class="cool-page sys-page sys-p-${page}">${skeleton(page)}</div>`;
  if (!sys.sample) snapshot();
  if (['overview', 'cpu', 'gpu', 'memory'].includes(page)) seedLive();
  load(page, false);
  update();
}

/** Data a page fetches once when it opens (lists, specs, plans…). */
async function load(page, fresh) {
  const get = async (key, cmd, args) => {
    if (sys[key] && !fresh) return;
    try {
      sys[key] = await ctx.api.invoke(cmd, args);
      sys.error[key] = null;
    } catch (e) {
      sys.error[key] = String(e);
    }
    if (sys.page === page) update();
  };
  if (page === 'overview') get('plans', 'power_plans');
  if (page === 'memory' || page === 'specs') get('specs', 'specs');
  if (page === 'startup') get('startup', 'startup_list');
  if (page === 'services') get('services', 'services_list');
  if (page === 'storage') {
    if (!sys.storage || fresh) {
      try {
        sys.storage = await ctx.api.invoke('storage_info', { fresh: !!fresh });
      } catch (e) { sys.error.storage = String(e); }
      if (sys.page === page) update();
    }
  }
  if (page === 'history') loadHistory();
}

function skeleton(page) {
  switch (page) {
    case 'overview':
      return `<div class="sys-gauges">${['cpu', 'gpu', 'memory'].map((k) => `<button class="cool-card sys-gauge" data-device="sys:${k}" data-gauge="${k}"></button>`).join('')}</div>
        <div class="sys-row">
          <section class="cool-card sys-top"><header><h3>Busiest apps</h3><button class="link-btn" data-device="sys:processes">All processes</button></header><div data-k="top"></div></section>
          <div class="cool-col">
            <section class="cool-card"><header><h3>Network</h3></header><div data-k="net"></div></section>
            <section class="cool-card"><header><h3>Power plan</h3></header><div data-k="plans"></div>
              <div class="sys-free"><button class="btn" data-sys-free>Free up memory</button><span class="hint" data-k="freed">${esc(sys.freed)}</span></div></section>
          </div>
        </div>`;
    case 'cpu':
    case 'gpu':
    case 'memory':
      return `<div class="sys-stats" data-k="stats"></div>
        <section class="cool-card"><header><h3>${page === 'memory' ? 'Memory in use' : 'Usage'} <small>last 2 minutes</small></h3></header><div class="temp-chart" data-chart="detail"></div></section>
        <div data-k="extra"></div>`;
    case 'processes':
      return `<div class="sys-toolbar"><input type="search" class="sys-search" data-sys-search placeholder="Search processes" value="${esc(sys.search)}" spellcheck="false">
        <span class="hint" data-k="pcount"></span><span class="spacer"></span>
        <button class="btn" data-sys-end disabled>End task</button></div>
        <div class="sys-table-wrap"><table class="sys-table proc-table"><thead><tr>
          ${[['name', 'Name'], ['status', 'Status'], ['cpu', 'CPU'], ['memory', 'Memory'], ['disk', 'Disk'], ['gpu', 'GPU']].map(([k, t]) => `<th data-sort="${k}" class="${k === 'name' ? '' : 'num'}">${t}</th>`).join('')}
        </tr></thead><tbody data-k="procs"><tr><td colspan="6" class="sys-empty">Reading the process list…</td></tr></tbody></table></div>`;
    case 'startup':
      return '<div class="sys-stack" data-k="startup"><div class="sys-empty">Reading startup apps…</div></div>';
    case 'services':
      return `<div class="sys-toolbar"><input type="search" class="sys-search" data-sys-svc-search placeholder="Search services" value="${esc(sys.svcSearch)}" spellcheck="false">
        <div class="segmented compact" data-sys-svc-filter>${[['all', 'All'], ['running', 'Running'], ['stopped', 'Stopped']].map(([v, t]) => `<button type="button" data-v="${v}" aria-checked="${sys.svcFilter === v}">${t}</button>`).join('')}</div>
        <span class="hint" data-k="scount"></span></div>
        <div data-k="svcnote"></div>
        <div class="sys-table-wrap"><table class="sys-table svc-table"><thead><tr><th>Service</th><th>Status</th><th>Starts</th><th></th></tr></thead>
        <tbody data-k="services"><tr><td colspan="4" class="sys-empty">Reading services…</td></tr></tbody></table></div>`;
    case 'storage':
      return `<section class="cool-card"><header><h3>Drives</h3></header><div class="sys-volumes" data-k="volumes"></div></section>
        <section class="cool-card"><header><h3>Disk health</h3></header><div data-k="disks"></div></section>
        <section class="cool-card"><header><h3>Network <small>last minute</small></h3><div class="temp-legend" data-k="netlegend"></div></header><div class="temp-chart" data-chart="net"></div><div data-k="adapters"></div></section>`;
    case 'sensors':
      return '<div class="sys-sensors" data-k="sensors"><div class="sys-empty">Reading sensors…</div></div>';
    case 'history':
      return `<div class="sys-toolbar"><div class="segmented compact" data-sys-range>${[[10, '10 min'], [60, '1 hour'], [360, '6 hours'], [1440, '24 hours']].map(([v, t]) => `<button type="button" data-v="${v}" aria-checked="${sys.history.minutes === v}">${t}</button>`).join('')}</div>
        <span class="hint">A log is saved each day too (the last 30 days).</span></div>
        ${[['temps', 'Temperatures'], ['usage', 'Usage'], ['power', 'Power'], ['network', 'Network']].map(([k, t]) => `<section class="cool-card"><header><h3>${t}</h3><div class="temp-legend" data-legend="${k}"></div></header><div class="temp-chart" data-chart="${k}"></div></section>`).join('')}`;
    case 'specs':
      return '<div class="sys-stack" data-k="specs"><div class="sys-empty">Reading your PC\'s specs…</div></div>';
    default:
      return '';
  }
}

const k = (key) => document.querySelector(`#stageBody [data-k="${key}"]`);
function put(key, html) {
  const el = k(key);
  if (el && el._html !== html) {
    el._html = html;
    el.innerHTML = html;
  }
}

/** Redraws the open page's live parts. */
function update() {
  switch (sys.page) {
    case 'overview': return updateOverview();
    case 'cpu': return updateCpu();
    case 'gpu': return updateGpu();
    case 'memory': return updateMemory();
    case 'processes': return updateProcesses();
    case 'startup': return updateStartup();
    case 'services': return updateServices();
    case 'storage': return updateStorage();
    case 'sensors': return updateSensors();
    case 'history': return drawHistory();
    case 'specs': return updateSpecs();
    default:
  }
}

function redrawCharts() {
  if (sys.page === 'history') drawHistory();
  else update();
}

// --- Shared bits ----------------------------------------------------------------------

function helperNote(err, what) {
  if (!err) return '';
  if (err === 'needs-helper' || err === 'helper-old') {
    const old = err === 'helper-old';
    return `<div class="cool-banner sys-helper"><span>${old ? 'Lumora\'s helper needs a quick update' : 'This needs Lumora\'s helper'} to ${esc(what)}. It's the same one-time setup as CPU temperature: Windows asks once.</span>
      <button class="btn primary" data-sys-helper>${old ? 'Update the helper' : 'Set it up'}</button></div>`;
  }
  return `<div class="cool-banner error">${esc(err)}</div>`;
}

const ring = (value, color, label) => {
  const C = 2 * Math.PI * 42;
  const v = value == null ? 0 : Math.max(0, Math.min(100, value));
  return `<svg class="sys-ring" viewBox="0 0 100 100" aria-hidden="true"><circle cx="50" cy="50" r="42" class="sys-ring-bg"/>
    <circle cx="50" cy="50" r="42" class="sys-ring-fg" stroke="${color}" stroke-dasharray="${(v / 100) * C} ${C}"/></svg>
    <div class="sys-ring-label"><b>${value == null ? '–' : Math.round(value)}<small>%</small></b><span>${label}</span></div>`;
};

function stat(label, value, sub = '') {
  return `<div class="readout"><span class="readout-label">${label}</span><span class="readout-value"><b>${value}</b></span>${sub ? `<span class="readout-sub">${sub}</span>` : ''}</div>`;
}

/** A temperature warms its text past a limit. */
function hot(c, warm, high) {
  if (c == null) return '';
  return c >= high ? ' danger' : c >= warm ? ' warn' : '';
}

// --- Overview --------------------------------------------------------------------------

function updateOverview() {
  const s = sys.sample;
  const now = s?.at || Date.now();
  const spark = (key) => `<svg class="sys-spark" viewBox="0 0 200 36" preserveAspectRatio="none" aria-hidden="true"><path d="${sparkPath(sys.live, key, 60000, now, 200, 36)}" stroke="${key === 'cpu' ? COLORS.cpu : key === 'gpu' ? COLORS.gpu : COLORS.third}"/></svg>`;
  for (const el of document.querySelectorAll('#stageBody [data-gauge]')) {
    const g = el.dataset.gauge;
    let html;
    if (!s) html = '<div class="sys-empty">Reading…</div>';
    else if (g === 'cpu') {
      html = `<header><h3>CPU</h3><span class="hint">${esc(short(s.cpu.name))}</span></header>
        <div class="sys-gauge-body">${ring(s.cpu.usage, COLORS.cpu, 'usage')}<dl>
          <div><dt>Temperature</dt><dd class="${hot(s.cpu.temp, 80, 90)}">${temp(s.cpu.temp)}</dd></div>
          <div><dt>Speed</dt><dd>${mhz(s.cpu.clock_mhz)}</dd></div>
          <div><dt>Power</dt><dd>${watts(s.cpu.power)}</dd></div></dl></div>${spark('cpu')}`;
    } else if (g === 'gpu') {
      const gpu = s.gpu;
      html = gpu ? `<header><h3>GPU</h3><span class="hint">${esc(short(gpu.name))}</span></header>
        <div class="sys-gauge-body">${ring(gpu.usage, COLORS.gpu, 'usage')}<dl>
          <div><dt>Temperature</dt><dd class="${hot(gpu.temp, 75, 85)}">${temp(gpu.temp)}</dd></div>
          <div><dt>Clock</dt><dd>${mhz(gpu.core_mhz)}</dd></div>
          <div><dt>Power</dt><dd>${watts(gpu.power)}</dd></div>
          <div><dt>Video memory</dt><dd>${gpu.vram_used == null ? '–' : `${mem(gpu.vram_used)} of ${mem(gpu.vram_total, 0)}`}</dd></div></dl></div>${spark('gpu')}`
        : '<header><h3>GPU</h3></header><div class="sys-empty">Lumora reads NVIDIA graphics cards. None was found.</div>';
    } else {
      const m = s.memory;
      html = `<header><h3>Memory</h3><span class="hint">${mem(m.total, 0)}</span></header>
        <div class="sys-gauge-body">${ring(m.percent, COLORS.third, 'in use')}<dl>
          <div><dt>In use</dt><dd>${mem(m.used)}</dd></div>
          <div><dt>Available</dt><dd>${mem(m.available)}</dd></div>
          <div><dt>Cached</dt><dd>${mem(m.cached)}</dd></div></dl></div>${spark('ram')}`;
    }
    if (el._html !== html) { el._html = html; el.innerHTML = html; }
    el.setAttribute('aria-label', `Open ${g === 'memory' ? 'memory' : g.toUpperCase()} details`);
  }
  if (s) {
    put('net', `<div class="sys-net"><div><span class="hint">Download</span><b>${rate(sum(s.net, 'down'))}</b></div><div><span class="hint">Upload</span><b>${rate(sum(s.net, 'up'))}</b></div></div>`);
  }
  // The five busiest apps, from the process list.
  const top = (sys.procs ? entries(sys.procs).sort((a, b) => b.cpu - a.cpu || b.memory - a.memory).slice(0, 5) : []);
  put('top', !sys.procs ? '<div class="sys-empty">Reading…</div>' : `<table class="sys-table mini"><tbody>${top.map((e) => `<tr><td>${avatar(e)}<span class="proc-name">${esc(e.label)}</span>${e.rows.length > 1 ? ` <span class="hint">(${e.rows.length})</span>` : ''}</td>
    <td class="num">${pct(e.cpu, 1)}</td><td class="num">${procMem(e.memory)}</td></tr>`).join('')}</tbody></table>`);
  const plans = sys.plans;
  put('plans', sys.error.plans ? `<div class="hint">${esc(sys.error.plans)}</div>` : !plans ? '' : !plans.length ? '<div class="hint">Windows didn\'t list any power plans.</div>'
    : `<div class="segmented sys-plans" data-sys-plans>${plans.map((p) => `<button type="button" data-v="${esc(p.id)}" aria-checked="${p.active}">${esc(p.name)}</button>`).join('')}</div>`);
}

const short = (name) => String(name || '').replace(/\(R\)|\(TM\)|NVIDIA |GeForce |AMD | Processor|\s+\d+-Core.*$/gi, '').trim();

// --- Details ---------------------------------------------------------------------------

function detailChart(key, color, name) {
  const box = document.querySelector('#stageBody [data-chart="detail"]');
  drawChart(box, {
    series: [{ key, name, color }],
    points: sys.live,
    span: 120000,
    now: sys.sample?.at || Date.now(),
    min: 0,
    max: 100,
    area: true,
    height: 200,
    fmt: (v) => `${Math.round(v)}%`,
    tipFmt: (v) => `${v.toFixed(1)}%`,
    gap: 12000,
    ticks: [[120000, '2 min ago'], [60000, '1 min'], [0, 'now']],
    label: `${name} over the last 2 minutes`,
  });
}

function updateCpu() {
  const s = sys.sample;
  if (!s) return;
  const c = s.cpu;
  put('stats', [
    stat('Usage', pct(c.usage)),
    stat('Speed', mhz(c.clock_mhz), c.base_mhz ? `Base ${mhz(c.base_mhz)}` : ''),
    stat('Temperature', `<span class="${hot(c.temp, 80, 90)}">${temp(c.temp)}</span>`, c.temp == null ? 'Turn on CPU temperature on a cooler page' : ''),
    stat('Power', watts(c.power), c.power == null ? 'Needs Lumora\'s helper' : 'Package'),
    stat('Processes', c.processes.toLocaleString(), `${c.threads.toLocaleString()} threads · ${c.handles.toLocaleString()} handles`),
    stat('Up time', uptime(c.uptime_s)),
  ].join(''));
  detailChart('cpu', COLORS.cpu, 'CPU usage');
  put('extra', `<section class="cool-card"><header><h3>Each core</h3><span class="hint">${c.cores.length} logical processors</span></header>
    <div class="sys-cores">${c.cores.map((v, i) => `<div class="sys-core" title="Core ${i + 1}: ${Math.round(v)}%"><i style="height:${Math.max(2, Math.min(100, v))}%"></i><span>${Math.round(v)}</span></div>`).join('')}</div></section>`);
}

function updateGpu() {
  const s = sys.sample;
  if (!s) return;
  const g = s.gpu;
  if (!g) {
    put('stats', '<div class="sys-empty">Lumora reads NVIDIA graphics cards. None was found on this PC.</div>');
    return;
  }
  put('stats', [
    stat('Usage', pct(g.usage)),
    stat('Temperature', `<span class="${hot(g.temp, 75, 85)}">${temp(g.temp)}</span>`),
    stat('Core clock', mhz(g.core_mhz), g.mem_mhz ? `Memory ${mhz(g.mem_mhz)}` : ''),
    stat('Power', watts(g.power)),
    stat('Fan', g.fan == null ? '–' : `${g.fan}%`),
    stat('Driver', esc(g.driver || '–')),
  ].join(''));
  detailChart('gpu', COLORS.gpu, 'GPU usage');
  const used = g.vram_used, total = g.vram_total;
  put('extra', used == null ? '' : `<section class="cool-card"><header><h3>Video memory</h3><span class="hint">${mem(used)} of ${mem(total, 0)}</span></header>
    <div class="sys-bar"><i style="width:${(used / total) * 100}%;background:${COLORS.gpu}"></i></div></section>`);
}

function updateMemory() {
  const s = sys.sample;
  if (!s) return;
  const m = s.memory;
  put('stats', [
    stat('In use', mem(m.used), `${pct(m.percent)} of ${mem(m.total, 0)}`),
    stat('Available', mem(m.available)),
    stat('Cached', mem(m.cached)),
    stat('Committed', `${mem(m.committed)}`, `of ${mem(m.commit_limit)}`),
  ].join(''));
  detailChart('ram', COLORS.third, 'Memory in use');
  const sticks = sys.specs?.specs?.ram || [];
  const used = m.used, cached = Math.min(m.cached, m.total - used);
  put('extra', `<section class="cool-card"><header><h3>How it's used</h3><button class="btn" data-sys-free>Free up memory</button></header>
    <div class="sys-bar stacked"><i style="width:${(used / m.total) * 100}%;background:${COLORS.third}" title="In use"></i><i style="width:${(cached / m.total) * 100}%" class="cached" title="Cached"></i></div>
    <div class="sys-legend"><span><i style="background:${COLORS.third}"></i>In use ${mem(used)}</span><span><i class="cached"></i>Cached ${mem(m.cached)}</span><span><i class="free"></i>Free ${mem(Math.max(0, m.total - used - cached))}</span></div>
    <p class="hint" data-k="freed">${esc(sys.freed)}</p></section>
    ${sticks.length ? `<section class="cool-card"><header><h3>Memory sticks</h3></header><table class="sys-table"><thead><tr><th>Slot</th><th>Size</th><th>Type</th><th>Speed</th><th>Maker</th><th>Part</th></tr></thead><tbody>
      ${sticks.map((r) => `<tr><td>${esc(r.slot)}</td><td>${gib(r.size)}</td><td>${esc(r.kind)}</td><td>${r.configured || r.speed} MT/s${r.speed && r.configured && r.speed !== r.configured ? ` <span class="hint">(rated ${r.speed})</span>` : ''}</td><td>${esc(r.maker)}</td><td>${esc(r.part)}</td></tr>`).join('')}
    </tbody></table></section>` : ''}`);
}

// --- Processes --------------------------------------------------------------------------

function onProcesses(rows) {
  sys.procs = rows;
  if (document.hidden) return;
  if (sys.page === 'processes') updateProcesses();
  else if (sys.page === 'overview') updateOverview();
}

const GROUP_NAMES = [['app', 'Apps'], ['background', 'Background processes'], ['windows', 'Windows processes']];

/** Processes grouped by program: "Chrome (14)" like Task Manager. */
function entries(rows) {
  const by = new Map();
  // Every copy of a program with a window counts as part of that app.
  const appNames = new Set(rows.filter((r) => r.group === 'app').map((r) => r.name.toLowerCase()));
  for (const r of rows) {
    const group = appNames.has(r.name.toLowerCase()) ? 'app' : r.group;
    const key = `${group}|${r.name.toLowerCase()}`;
    let e = by.get(key);
    if (!e) {
      e = { key, group, name: r.name, label: r.description || r.name, path: r.path, rows: [], cpu: 0, memory: 0, disk: 0, gpu: 0 };
      by.set(key, e);
    }
    e.rows.push(r);
    e.cpu += r.cpu;
    e.memory += r.memory;
    e.disk += r.disk;
    e.gpu = Math.max(e.gpu, r.gpu);
    if (r.group === 'app' && r.description) e.label = r.description;
    if (!e.path && r.path) e.path = r.path;
  }
  return [...by.values()];
}

const statusOf = (e) => (e.rows.every((r) => r.status === 'suspended') ? 'Suspended' : e.rows.some((r) => r.status === 'efficiency') ? 'Efficiency mode' : '');

function sorter() {
  const { key, dir } = sys.sort;
  if (key === 'name') return (a, b) => dir * a.label.localeCompare(b.label);
  if (key === 'status') return (a, b) => dir * statusOf(a).localeCompare(statusOf(b));
  return (a, b) => dir * (a[key] - b[key]) || a.label.localeCompare(b.label);
}

// Real program icons, asked for once per file and kept for the session.
const icons = new Map(); // path → data URL, or null (no icon)
const wantIcons = new Set();
let iconTimer = 0;
function iconFor(path) {
  if (!path) return null;
  if (icons.has(path)) return icons.get(path);
  wantIcons.add(path);
  if (!iconTimer) {
    iconTimer = setTimeout(async () => {
      iconTimer = 0;
      const paths = [...wantIcons];
      wantIcons.clear();
      for (const p of paths) icons.set(p, null);
      try {
        const got = await ctx.api.invoke('app_icons', { paths });
        for (const [p, url] of Object.entries(got || {})) icons.set(p, url || null);
      } catch { /* letters it is */ }
      if (sys.page) update();
    }, 30);
  }
  return null;
}

function avatar(e) {
  const url = iconFor(e.path);
  if (url) return `<img class="proc-icon" src="${url}" alt="" draggable="false">`;
  const letter = (e.label || e.name || '?').trim()[0]?.toUpperCase() || '?';
  let h = 0;
  for (const ch of e.name) h = (h * 31 + ch.charCodeAt(0)) % 360;
  return `<span class="proc-avatar" style="--h:${h}">${esc(letter)}</span>`;
}

/** Task Manager's heat shading: busier cells glow a little amber. */
const heat = (v, full) => (v > 0.05 ? ` style="--heat:${Math.min(1, v / full).toFixed(2)}"` : '');

function updateProcesses() {
  const rows = sys.procs;
  if (!rows) return;
  const q = sys.search.trim().toLowerCase();
  const all = entries(rows);
  const list = q ? all.filter((e) => e.label.toLowerCase().includes(q) || e.name.toLowerCase().includes(q) || e.rows.some((r) => String(r.pid) === q)) : all;
  const sort = sorter();
  const totalCpu = rows.reduce((a, r) => a + r.cpu, 0);
  put('pcount', `${rows.length} processes · CPU ${pct(Math.min(100, totalCpu))}`);
  const html = GROUP_NAMES.map(([g, title]) => {
    const items = list.filter((e) => e.group === g).sort(sort);
    if (!items.length) return '';
    return `<tr class="proc-group"><td colspan="6">${title} <span class="hint">(${items.length})</span></td></tr>` + items.map((e) => {
      const many = e.rows.length > 1;
      const open = many && (sys.open.has(e.key) || (q && e.rows.length < 6));
      const picked = sys.picked === e.key;
      const head = `<tr class="proc-row${picked ? ' picked' : ''}" data-proc="${esc(e.key)}" aria-selected="${picked}">
        <td><span class="proc-chev${many ? '' : ' none'}" data-proc-toggle="${esc(e.key)}">${many ? (open ? '▾' : '▸') : ''}</span>${avatar(e)}<span class="proc-name">${esc(e.label)}</span>${many ? ` <span class="hint">(${e.rows.length})</span>` : ''}</td>
        <td class="proc-status">${statusOf(e)}</td>
        <td class="num"${heat(e.cpu, 25)}>${pct(e.cpu, 1)}</td>
        <td class="num"${heat(e.memory, 2147483648)}>${procMem(e.memory)}</td>
        <td class="num"${heat(e.disk, 20971520)}>${diskRate(e.disk)}</td>
        <td class="num"${heat(e.gpu, 50)}>${pct(e.gpu, 0)}</td></tr>`;
      if (!open) return head;
      return head + [...e.rows].sort((a, b) => b.cpu - a.cpu || b.memory - a.memory).map((r) => {
        const pk = sys.picked === `pid:${r.pid}`;
        return `<tr class="proc-row child${pk ? ' picked' : ''}" data-proc="pid:${r.pid}" aria-selected="${pk}">
          <td><span class="proc-name">${esc(r.name)}</span> <span class="hint">${r.pid}</span></td>
          <td class="proc-status">${r.status === 'running' ? '' : r.status === 'efficiency' ? 'Efficiency mode' : 'Suspended'}</td>
          <td class="num">${pct(r.cpu, 1)}</td><td class="num">${procMem(r.memory)}</td><td class="num">${diskRate(r.disk)}</td><td class="num">${pct(r.gpu, 0)}</td></tr>`;
      }).join('');
    }).join('');
  }).join('') || `<tr><td colspan="6" class="sys-empty">${q ? 'Nothing matches that search.' : 'No processes.'}</td></tr>`;
  put('procs', html);
  for (const th of document.querySelectorAll('#stageBody th[data-sort]')) {
    th.setAttribute('aria-sort', th.dataset.sort === sys.sort.key ? (sys.sort.dir < 0 ? 'descending' : 'ascending') : 'none');
  }
  const end = document.querySelector('[data-sys-end]');
  if (end) end.disabled = !picked();
}

/** The picked entry or process: { pids, path, name, group, label }. */
function picked(key = sys.picked) {
  if (!key || !sys.procs) return null;
  if (key.startsWith('pid:')) {
    const r = sys.procs.find((x) => `pid:${x.pid}` === key);
    return r && { pids: [r.pid], main: r, name: r.name, label: r.description || r.name, group: r.group };
  }
  const e = entries(sys.procs).find((x) => x.key === key);
  if (!e) return null;
  const main = e.rows.find((r) => r.group === 'app') || [...e.rows].sort((a, b) => a.pid - b.pid)[0];
  return { pids: e.rows.map((r) => r.pid), main, name: e.name, label: e.label, group: e.group };
}

async function processAction(p, action, value) {
  try {
    if (action === 'end' || action === 'tree') {
      for (const pid of p.pids) await ctx.api.invoke('process_action', { pid, action, value: null, path: null });
      ctx.toast(`Ended ${p.label}`);
      sys.picked = null;
    } else if (action === 'search') {
      await ctx.api.invoke('open_external', { url: `https://www.bing.com/search?q=${encodeURIComponent(p.name)}` });
    } else {
      await ctx.api.invoke('process_action', { pid: p.main.pid, action, value: value ?? null, path: p.main.path ?? null });
      if (action === 'priority') ctx.toast(`${p.label}: ${value} priority`);
      if (action === 'efficiency') ctx.toast(`Efficiency mode ${value === 'on' ? 'on' : 'off'} for ${p.label}`);
    }
  } catch (e) {
    ctx.toast(String(e));
  }
}

// The right-click menu.
let menu = null;
function openMenu(x, y, key) {
  closeMenu();
  const p = picked(key);
  if (!p) return;
  const risky = p.group === 'windows';
  const eff = p.main.status === 'efficiency';
  menu = document.createElement('div');
  menu.className = 'menu sys-menu';
  menu.setAttribute('role', 'menu');
  menu.innerHTML = `<div class="menu-label">${esc(p.label)}</div>
    ${risky ? '<div class="menu-note">A Windows process: ending it can make Windows unstable.</div>' : ''}
    <button role="menuitem" data-menu="end">${risky ? 'End task anyway' : 'End task'}</button>
    <button role="menuitem" data-menu="tree">End process tree</button>
    <div class="menu-sep"></div>
    <div class="menu-label">Priority</div>
    <div class="menu-choices">${[['idle', 'Low'], ['below', 'Below normal'], ['normal', 'Normal'], ['above', 'Above normal'], ['high', 'High']].map(([v, t]) => `<button role="menuitem" data-menu="priority" data-v="${v}">${t}</button>`).join('')}</div>
    <button role="menuitemcheckbox" aria-checked="${eff}" data-menu="efficiency" data-v="${eff ? 'off' : 'on'}">${eff ? 'Turn off efficiency mode' : 'Efficiency mode'}</button>
    <div class="menu-sep"></div>
    <button role="menuitem" data-menu="location" ${p.main.path ? '' : 'disabled'}>Open file location</button>
    <button role="menuitem" data-menu="properties" ${p.main.path ? '' : 'disabled'}>Properties</button>
    <button role="menuitem" data-menu="search">Search online</button>`;
  document.body.appendChild(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, innerWidth - r.width - 8)}px`;
  menu.style.top = `${Math.min(y, innerHeight - r.height - 8)}px`;
  menu.dataset.key = key;
  menu.querySelector('button')?.focus();
}

function closeMenu() {
  menu?.remove();
  menu = null;
}

// --- Startup apps ------------------------------------------------------------------------

function updateStartup() {
  const list = sys.startup;
  if (sys.error.startup && !list) return put('startup', helperNote(sys.error.startup, 'read startup apps'));
  if (!list) return;
  const on = list.filter((i) => i.enabled).length;
  put('startup', `${helperNote(sys.error.startupSet, 'switch apps that start for every user')}
    <p class="hint">${on} of ${list.length} start with Windows. Switching one off doesn't uninstall it; it just won't open on its own.</p>
    <div class="sys-list">${list.map((i) => `<label class="sys-item">
      ${avatar({ name: i.name, label: i.description || i.name, path: i.path })}
      <span class="sys-item-text"><strong>${esc(i.description || i.name)}</strong><small title="${esc(i.command)}">${esc(i.path || i.command)}</small></span>
      <span class="nav-badge muted" title="${i.admin ? 'Changing it needs Lumora\'s helper' : ''}">${esc(i.location)}</span>
      <input type="checkbox" class="switch small" data-sys-startup="${esc(i.id)}" ${i.enabled ? 'checked' : ''} aria-label="Start ${esc(i.name)} with Windows">
    </label>`).join('') || '<div class="sys-empty">Nothing starts with Windows.</div>'}</div>`);
}

// --- Services -------------------------------------------------------------------------------

const START_NAMES = { auto: 'Automatic', delayed: 'Automatic (delayed)', manual: 'Manual', disabled: 'Disabled', boot: 'Boot', system: 'System' };

function updateServices() {
  const list = sys.services;
  put('svcnote', helperNote(sys.error.svcAction, 'start, stop and change services'));
  if (sys.error.services && !list) return put('services', `<tr><td colspan="4">${esc(sys.error.services)}</td></tr>`);
  if (!list) return;
  const q = sys.svcSearch.trim().toLowerCase();
  const shown = list.filter((s) => (sys.svcFilter === 'all' || (sys.svcFilter === 'running' ? s.state === 'running' : s.state !== 'running'))
    && (!q || s.display.toLowerCase().includes(q) || s.name.toLowerCase().includes(q)));
  put('scount', `${shown.length} of ${list.length}`);
  put('services', shown.map((s) => {
    const running = s.state === 'running';
    const busy = s.state === 'starting' || s.state === 'stopping';
    const fixed = s.start === 'boot' || s.start === 'system';
    return `<tr title="${esc(s.description)}">
      <td><strong>${esc(s.display)}</strong><small class="hint">${esc(s.name)}${s.description ? ` · ${esc(s.description)}` : ''}</small></td>
      <td><span class="sys-pill ${s.state}">${esc(s.state[0].toUpperCase() + s.state.slice(1))}</span></td>
      <td>${fixed ? START_NAMES[s.start] : `<select class="sys-select" data-sys-svc-start="${esc(s.name)}">${['auto', 'delayed', 'manual', 'disabled'].map((v) => `<option value="${v}" ${s.start === v ? 'selected' : ''}>${START_NAMES[v]}</option>`).join('')}</select>`}</td>
      <td class="sys-actions">${busy ? '<span class="hint">…</span>' : running
        ? `${s.can_stop ? `<button class="btn small" data-sys-svc="stop" data-name="${esc(s.name)}">Stop</button><button class="btn small" data-sys-svc="restart" data-name="${esc(s.name)}">Restart</button>` : ''}`
        : `<button class="btn small" data-sys-svc="start" data-name="${esc(s.name)}" ${s.start === 'disabled' ? 'disabled title="Set it to Manual or Automatic first"' : ''}>Start</button>`}</td></tr>`;
  }).join('') || '<tr><td colspan="4" class="sys-empty">Nothing matches.</td></tr>');
}

// --- Drives & network --------------------------------------------------------------------------

function updateStorage() {
  const st = sys.storage;
  const s = sys.sample;
  const activity = (n) => s?.disks.find((d) => parseInt(d.name, 10) === n);
  if (st) {
    put('volumes', st.volumes.map((v) => {
      const used = v.total - v.free;
      const full = used / v.total;
      return `<div class="sys-volume"><div class="sys-volume-head"><strong>${esc(v.label || (v.kind === 'removable' ? 'USB drive' : 'Local disk'))} (${esc(v.letter)})</strong><span class="hint">${esc(v.filesystem)}${v.kind === 'network' ? ' · network' : ''}</span></div>
        <div class="sys-bar"><i style="width:${full * 100}%" class="${full > 0.9 ? 'danger' : ''}"></i></div>
        <span class="hint">${size(v.free)} free of ${size(v.total)}</span></div>`;
    }).join('') || '<div class="sys-empty">No drives found.</div>');
    const disks = st.disks.length
      ? `<table class="sys-table"><thead><tr><th>Drive</th><th>Health</th><th class="num">Temperature</th><th class="num">Life left</th><th class="num">Active</th><th class="num">Read</th><th class="num">Write</th><th class="num">Powered on</th><th class="num">Written</th></tr></thead><tbody>
        ${st.disks.map((d) => {
          const a = activity(d.number);
          const letters = st.volumes.filter((v) => v.disk === d.number).map((v) => v.letter).join(' ');
          return `<tr><td><strong>${esc(d.model || `Disk ${d.number}`)}</strong><small class="hint">${esc(d.bus)} · ${size(d.size)}${letters ? ` · ${esc(letters)}` : ''}</small></td>
          <td><span class="sys-pill ${d.health === 'good' ? 'running' : d.health === 'warning' ? 'danger' : ''}">${d.health === 'good' ? 'Good' : d.health === 'warning' ? 'Warning' : 'Unknown'}</span></td>
          <td class="num ${hot(d.temp, 55, 70)}">${temp(d.temp)}</td>
          <td class="num">${d.life_left == null ? '<span class="hint" title="Only NVMe drives report this">–</span>' : `${d.life_left}%`}</td>
          <td class="num">${a ? pct(a.active) : '–'}</td><td class="num">${a ? diskRate(a.read) : '–'}</td><td class="num">${a ? diskRate(a.write) : '–'}</td>
          <td class="num">${d.power_on_hours == null ? '–' : `${d.power_on_hours.toLocaleString()} h`}</td>
          <td class="num">${d.written == null ? '–' : size(d.written)}</td></tr>`;
        }).join('')}</tbody></table>`
      : '';
    const activityOnly = !st.disks.length && s?.disks.length
      ? `<table class="sys-table"><thead><tr><th>Disk</th><th class="num">Active</th><th class="num">Read</th><th class="num">Write</th></tr></thead><tbody>${s.disks.map((d) => `<tr><td>Disk ${esc(d.name)}</td><td class="num">${pct(d.active)}</td><td class="num">${diskRate(d.read)}</td><td class="num">${diskRate(d.write)}</td></tr>`).join('')}</tbody></table>`
      : '';
    put('disks', helperNote(st.health_error, 'read drive health and temperatures') + disks + activityOnly);
  }
  if (s) {
    const series = [{ key: 'down', name: 'Download', color: COLORS.third }, { key: 'up', name: 'Upload', color: COLORS.gpu }];
    put('netlegend', series.map((x) => `<span class="temp-key static"><i style="background:${x.color}"></i>${x.name}<b>${rate(sum(s.net, x.key))}</b></span>`).join(''));
    drawChart(document.querySelector('#stageBody [data-chart="net"]'), {
      series, points: sys.live, span: 60000, now: s.at, zero: true, height: 150, minRange: 131072,
      fmt: rate, axis: (v) => (v ? rate(v) : '0'), gap: 6000, left: 64, right: 78,
      ticks: [[60000, '1 min ago'], [0, 'now']], label: 'Network traffic over the last minute',
    });
    put('adapters', s.net.length ? `<table class="sys-table"><thead><tr><th>Adapter</th><th class="num">Download</th><th class="num">Upload</th><th class="num">Link</th></tr></thead><tbody>
      ${s.net.map((n) => `<tr><td>${esc(n.name)}</td><td class="num">${rate(n.down)}</td><td class="num">${rate(n.up)}</td><td class="num">${n.link ? (n.link >= 1e9 ? `${n.link / 1e9} Gbps` : `${Math.round(n.link / 1e6)} Mbps`) : '–'}</td></tr>`).join('')}
    </tbody></table>` : '<div class="sys-empty">No network adapters are connected.</div>');
  }
}

// --- Sensors ------------------------------------------------------------------------------------

function sensorValue(x, v) {
  if (v == null) return '–';
  if (x.unit === '°C') return temp(v);
  if (x.unit === 'MHz') return mhz(Math.round(v));
  if (x.unit === 'rpm') return `${Math.round(v).toLocaleString()} rpm`;
  if (x.unit === 'MB/s') return x.group === 'Network' ? rate(v * 1048576) : `${v.toFixed(1)} MB/s`;
  if (x.unit === 'GB') return mem(v * 1073741824);
  if (x.unit === 'MB') return mem(v * 1048576);
  if (x.unit === 'W') return `${v.toFixed(1)} W`;
  return `${v.toFixed(x.unit === '%' ? 0 : 1)}${x.unit === '%' ? '%' : ` ${x.unit}`}`;
}

function updateSensors() {
  if (!sys.sensors.length) return;
  const groups = new Map();
  for (const x of sys.sensors) {
    const key = `${x.group}\u001f${x.name}`;
    let mm = sys.minmax.get(key);
    if (!mm) sys.minmax.set(key, (mm = { min: x.min, max: x.max }));
    mm.min = Math.min(mm.min, x.value);
    mm.max = Math.max(mm.max, x.value);
    if (!groups.has(x.group)) groups.set(x.group, []);
    groups.get(x.group).push({ ...x, ...mm });
  }
  put('sensors', [...groups].map(([g, list]) => `<section class="cool-card"><header><h3>${esc(g)}</h3></header>
    <table class="sys-table"><thead><tr><th>Sensor</th><th class="num">Now</th><th class="num">Min</th><th class="num">Max</th></tr></thead><tbody>
    ${list.map((x) => `<tr><td>${esc(x.name)}</td><td class="num"><b>${sensorValue(x, x.value)}</b></td><td class="num">${sensorValue(x, x.min)}</td><td class="num">${sensorValue(x, x.max)}</td></tr>`).join('')}
    </tbody></table></section>`).join(''));
}

// --- History ---------------------------------------------------------------------------------------

function startHistoryTimer() {
  clearInterval(sys.history.timer);
  sys.history.timer = document.hidden ? 0 : setInterval(loadHistory, 15000);
}

async function loadHistory() {
  try {
    const pts = await ctx.api.invoke('history_range', { minutes: sys.history.minutes });
    sys.history.points = pts;
  } catch { /* keep what we had */ }
  if (sys.page === 'history') drawHistory();
}

const HISTORY_CHARTS = {
  temps: { series: [['liquid', 0, 'Liquid', COLORS.third], ['cpu', 1, 'CPU', COLORS.cpu], ['gpu', 2, 'GPU', COLORS.gpu]], fmt: (v) => temp(v, false), tip: (v) => temp(v) },
  usage: { series: [['cpu', 3, 'CPU', COLORS.cpu], ['gpu', 4, 'GPU', COLORS.gpu], ['ram', 5, 'Memory', COLORS.third]], fmt: (v) => `${Math.round(v)}%`, min: 0, max: 100 },
  power: { series: [['cpu', 6, 'CPU', COLORS.cpu], ['gpu', 7, 'GPU', COLORS.gpu]], fmt: (v) => `${Math.round(v)} W`, zero: true },
  network: { series: [['down', 8, 'Download', COLORS.third], ['up', 9, 'Upload', COLORS.gpu]], fmt: rate, axis: (v) => (v ? rate(v) : '0'), zero: true, left: 64, right: 78, minRange: 131072 },
};
const hiddenSeries = new Set();

function drawHistory() {
  const pts = sys.history.points;
  const minutes = sys.history.minutes;
  const span = minutes * 60000;
  const ticks = minutes === 10 ? [[600000, '10 min ago'], [300000, '5 min'], [0, 'now']]
    : minutes === 60 ? [[3600000, '1 hour ago'], [1800000, '30 min'], [0, 'now']]
      : minutes === 360 ? [[21600000, '6 hours ago'], [10800000, '3 hours'], [0, 'now']]
        : [[86400000, '24 hours ago'], [43200000, '12 hours'], [0, 'now']];
  for (const [name, c] of Object.entries(HISTORY_CHARTS)) {
    const present = c.series.filter(([, i]) => pts.some((p) => p.v[i] != null));
    const legend = document.querySelector(`#stageBody [data-legend="${name}"]`);
    const latest = (i) => { for (let j = pts.length - 1; j >= 0; j--) if (pts[j].v[i] != null) return pts[j].v[i]; return null; };
    if (legend) {
      const html = present.map(([key, i, label, color]) => `<button class="temp-key" data-hist="${name}:${key}" aria-pressed="${!hiddenSeries.has(`${name}:${key}`)}"><i style="background:${color}"></i>${label}<b>${latest(i) == null ? '–' : esc((c.tip || c.fmt)(latest(i)))}</b></button>`).join('');
      if (legend._html !== html) { legend._html = html; legend.innerHTML = html; }
    }
    const shown = present.filter(([key]) => !hiddenSeries.has(`${name}:${key}`));
    const box = document.querySelector(`#stageBody [data-chart="${name}"]`);
    if (!box) continue;
    if (!pts.length || !shown.length) {
      box.innerHTML = `<div class="temp-empty">${pts.length ? 'Nothing recorded for this yet.' : 'Collecting readings… The charts fill in as Lumora runs.'}</div>`;
      box._opts = null;
      continue;
    }
    drawChart(box, {
      series: shown.map(([key, , label, color]) => ({ key, name: label, color })),
      points: pts.map((p) => ({ at: p.at, values: Object.fromEntries(shown.map(([key, i]) => [key, p.v[i]])) })),
      span, now: Date.now(), min: c.min, max: c.max, zero: c.zero, minRange: c.minRange || 10,
      fmt: c.fmt, tipFmt: c.tip, axis: c.axis || c.fmt, left: c.left, right: c.right, height: 170, gap: Math.max(16000, span / 120),
      ticks, label: `${name} over the last ${minutes >= 60 ? `${minutes / 60} hours` : `${minutes} minutes`}`,
    });
  }
}

// --- Specs ------------------------------------------------------------------------------------------

function updateSpecs() {
  if (sys.error.specs) return put('specs', `<div class="cool-banner error">${esc(sys.error.specs)}</div>`);
  const sp = sys.specs?.specs;
  if (!sp) return;
  const row = (k, v) => (v ? `<div><dt>${k}</dt><dd>${esc(v)}</dd></div>` : '');
  const ramLine = sp.ram.length
    ? `${gib(sp.ram_total)} (${sp.ram.length} × ${gib(sp.ram[0].size)} ${esc(sp.ram[0].kind)} at ${sp.ram[0].configured || sp.ram[0].speed} MT/s)`
    : gib(sp.ram_total);
  put('specs', `<section class="cool-card"><dl class="sys-specs">
    ${row('Windows', sp.os)}${row('Processor', `${sp.cpu} (${sp.cores} cores, ${sp.threads} threads)`)}
    ${row('Motherboard', sp.board)}${row('BIOS', sp.bios)}
    <div><dt>Memory</dt><dd>${ramLine}</dd></div>
    ${row('Graphics', sp.gpu ? `${sp.gpu}${sp.gpu_vram ? ` (${Math.round(sp.gpu_vram / 1073741824)} GB)` : ''}` : '')}${row('Graphics driver', sp.gpu_driver)}
    ${sp.drives.map((d, i) => `<div><dt>${i ? '' : 'Drives'}</dt><dd>${esc(d)}</dd></div>`).join('')}
  </dl></section>
  ${sp.ram.length ? `<section class="cool-card"><header><h3>Memory sticks</h3></header><table class="sys-table"><thead><tr><th>Slot</th><th>Size</th><th>Type</th><th>Speed</th><th>Maker</th><th>Part</th></tr></thead><tbody>
    ${sp.ram.map((r) => `<tr><td>${esc(r.slot)}</td><td>${gib(r.size)}</td><td>${esc(r.kind)}</td><td>${r.configured || r.speed} MT/s</td><td>${esc(r.maker)}</td><td>${esc(r.part)}</td></tr>`).join('')}
  </tbody></table></section>` : ''}`);
}

// --- Events -----------------------------------------------------------------------------------------

async function onClick(ev) {
  const t = ev.target;
  if (menu && !t.closest('.sys-menu')) closeMenu();
  const mi = t.closest('.sys-menu [data-menu]');
  if (mi) {
    const p = picked(menu.dataset.key);
    closeMenu();
    if (p) await processAction(p, mi.dataset.menu, mi.dataset.v);
    return;
  }
  if (t.closest('[data-sys-collapse]')) {
    try { localStorage.setItem('lumora.systemCollapsed', collapsed() ? '0' : '1'); } catch { /* not remembered */ }
    ctx.rerender();
    return;
  }
  if (!sys.page) return;
  const toggle = t.closest('[data-proc-toggle]');
  if (toggle && toggle.dataset.procToggle) {
    const key = toggle.dataset.procToggle;
    if (sys.open.has(key)) sys.open.delete(key); else sys.open.add(key);
    updateProcesses();
    return;
  }
  const row = t.closest('[data-proc]');
  if (row) {
    sys.picked = row.dataset.proc;
    updateProcesses();
    return;
  }
  const th = t.closest('th[data-sort]');
  if (th) {
    const key = th.dataset.sort;
    sys.sort = sys.sort.key === key ? { key, dir: -sys.sort.dir } : { key, dir: key === 'name' || key === 'status' ? 1 : -1 };
    updateProcesses();
    return;
  }
  if (t.closest('[data-sys-end]')) {
    const p = picked();
    if (p) await processAction(p, 'end');
    return;
  }
  const plan = t.closest('[data-sys-plans] [data-v]');
  if (plan) {
    try {
      sys.plans = await ctx.api.invoke('set_power_plan', { id: plan.dataset.v });
      update();
    } catch (e) { ctx.toast(String(e)); }
    return;
  }
  if (t.closest('[data-sys-free]')) return freeMemory(t.closest('[data-sys-free]'));
  if (t.closest('[data-sys-helper]')) return setupHelper(t.closest('[data-sys-helper]'));
  const svc = t.closest('[data-sys-svc]');
  if (svc) {
    svc.disabled = true;
    try {
      await ctx.api.invoke('service_action', { name: svc.dataset.name, action: svc.dataset.sysSvc });
      sys.error.svcAction = null;
      ctx.toast(`${svc.dataset.sysSvc === 'stop' ? 'Stopped' : svc.dataset.sysSvc === 'start' ? 'Started' : 'Restarted'} ${svc.dataset.name}`);
    } catch (e) {
      sys.error.svcAction = String(e);
      if (!/helper/.test(String(e))) ctx.toast(String(e));
    }
    await load('services', true);
    return;
  }
  const range = t.closest('[data-sys-range] [data-v]');
  if (range) {
    sys.history.minutes = +range.dataset.v;
    range.parentElement.querySelectorAll('[data-v]').forEach((b) => b.setAttribute('aria-checked', String(b === range)));
    loadHistory();
    return;
  }
  const hist = t.closest('[data-hist]');
  if (hist) {
    const key = hist.dataset.hist;
    if (hiddenSeries.has(key)) hiddenSeries.delete(key); else hiddenSeries.add(key);
    drawHistory();
    return;
  }
  const filter = t.closest('[data-sys-svc-filter] [data-v]');
  if (filter) {
    sys.svcFilter = filter.dataset.v;
    filter.parentElement.querySelectorAll('[data-v]').forEach((b) => b.setAttribute('aria-checked', String(b === filter)));
    updateServices();
    return;
  }
  if (t.closest('[data-sys-refresh]')) {
    if (sys.page === 'storage') sys.storage = null;
    load(sys.page, true);
    ctx.toast('Refreshed');
    return;
  }
  if (t.closest('[data-sys-reset-minmax]')) {
    sys.minmax.clear();
    for (const x of sys.sensors) sys.minmax.set(`${x.group}\u001f${x.name}`, { min: x.value, max: x.value });
    updateSensors();
    return;
  }
  if (t.closest('[data-sys-copy-specs]')) {
    if (!sys.specs) await load('specs', false);
    if (sys.specs) {
      await copy(sys.specs.text);
      ctx.toast('Specs copied');
    }
    return;
  }
  if (t.closest('[data-sys-export]')) {
    try {
      const path = await ctx.api.invoke('history_export', { minutes: sys.history.minutes });
      ctx.toast(`Saved to ${path.split(/[\\/]/).pop()} in Downloads`);
    } catch (e) { ctx.toast(String(e)); }
  }
}

async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const t = document.createElement('textarea');
    t.value = text;
    t.style.cssText = 'position:fixed;opacity:0';
    document.body.appendChild(t);
    t.select();
    document.execCommand('copy');
    t.remove();
  }
}

async function freeMemory(btn) {
  btn.disabled = true;
  sys.freed = 'Freeing…';
  update();
  try {
    const r = await ctx.api.invoke('free_memory');
    const gained = r.after - r.before;
    sys.freed = gained > 52428800 ? `Freed ${mem(gained)}` : 'Done. Not much was cached.';
  } catch (e) {
    const err = String(e);
    sys.freed = err === 'needs-helper' || err === 'helper-old' ? 'This needs Lumora\'s helper (set it up on the CPU page or a cooler page).' : err;
  }
  btn.disabled = false;
  if (sys.page) update();
  for (const el of document.querySelectorAll('[data-k="freed"]')) el.textContent = sys.freed;
}

async function setupHelper(btn) {
  btn.disabled = true;
  btn.textContent = 'Waiting for Windows…';
  try {
    await ctx.api.invoke('setup_cpu_sensor');
    ctx.toast('Lumora\'s helper is ready');
    sys.error = {};
    sys.storage = null;
    load(sys.page, true);
  } catch (e) {
    ctx.toast(String(e));
    btn.disabled = false;
    btn.textContent = 'Try again';
  }
}

async function onChange(ev) {
  const t = ev.target;
  if (t.matches('[data-sys-overlay]')) {
    try {
      const r = await ctx.api.invoke('overlay_toggle', { show: t.checked });
      ctx.state.app.settings.overlay = { ...(ctx.state.app.settings.overlay || {}), shown: r.shown, locked: r.locked };
    } catch (e) { ctx.toast(String(e)); }
    return;
  }
  if (t.matches('[data-sys-startup]')) {
    const id = t.dataset.sysStartup;
    t.disabled = true;
    try {
      await ctx.api.invoke('startup_set', { id, on: t.checked });
      const item = sys.startup?.find((i) => i.id === id);
      if (item) item.enabled = t.checked;
      sys.error.startupSet = null;
    } catch (e) {
      t.checked = !t.checked;
      sys.error.startupSet = String(e);
      if (!/helper/.test(String(e))) ctx.toast(String(e));
    }
    t.disabled = false;
    updateStartup();
    return;
  }
  if (t.matches('[data-sys-svc-start]')) {
    try {
      await ctx.api.invoke('service_start_type', { name: t.dataset.sysSvcStart, start: t.value });
      sys.error.svcAction = null;
      ctx.toast('Startup type changed');
    } catch (e) {
      sys.error.svcAction = String(e);
      if (!/helper/.test(String(e))) ctx.toast(String(e));
    }
    load('services', true);
  }
}

function onInput(ev) {
  const t = ev.target;
  if (t.matches('[data-sys-search]')) {
    sys.search = t.value;
    updateProcesses();
  } else if (t.matches('[data-sys-svc-search]')) {
    sys.svcSearch = t.value;
    updateServices();
  }
}

function onContextMenu(ev) {
  const row = ev.target.closest('#stageBody [data-proc]');
  if (!row) return;
  ev.preventDefault();
  sys.picked = row.dataset.proc;
  updateProcesses();
  openMenu(ev.clientX, ev.clientY, row.dataset.proc);
}

function onDblClick(ev) {
  const row = ev.target.closest('#stageBody [data-proc]');
  if (!row || row.dataset.proc.startsWith('pid:')) return;
  const key = row.dataset.proc;
  if (sys.open.has(key)) sys.open.delete(key); else sys.open.add(key);
  updateProcesses();
}

/** Settings changed elsewhere (units): redraw with them. */
export function refresh() {
  if (sys.page) update();
}
