// The mini overlay: a small always-on-top window with the main readings.
// It asks for readings while it's open, once a second.

const tauri = window.__TAURI__;
const $ = (id) => document.getElementById(id);
let units = { temp: 'c', memory: 'gb', network: 'mbps' };

const temp = (c) => (c == null ? '–' : units.temp === 'f' ? `${Math.round(c * 9 / 5 + 32)}°` : `${Math.round(c)}°`);
const level = (c, warm, hot) => (c == null ? '' : c >= hot ? 'hot' : c >= warm ? 'warn' : '');
const pct = (v) => (v == null ? '–' : `${Math.round(v)}%`);
function rate(b) {
  if (units.network === 'mbs') return b < 1048576 ? `${Math.round(b / 1024)} KB/s` : `${(b / 1048576).toFixed(1)} MB/s`;
  const bits = b * 8;
  return bits < 1e6 ? `${Math.round(bits / 1e3)} Kb` : `${(bits / 1e6).toFixed(bits < 1e8 ? 1 : 0)} Mb`;
}
const gb = (b) => (units.memory === 'mb' ? `${Math.round(b / 1048576)} MB` : `${(b / 1073741824).toFixed(1)} GB`);

function set(id, text, cls = '') {
  const el = $(id);
  if (el.textContent !== text) el.textContent = text;
  if (el.className !== cls) el.className = cls;
}

function show(s) {
  set('cpuT', temp(s.cpu.temp), level(s.cpu.temp, 80, 90));
  set('cpuU', pct(s.cpu.usage));
  set('gpuT', temp(s.gpu?.temp), level(s.gpu?.temp, 75, 85));
  set('gpuU', pct(s.gpu?.usage));
  set('ramU', pct(s.memory.percent), level(s.memory.percent, 85, 95));
  set('ramG', gb(s.memory.used));
  const down = s.net.reduce((a, n) => a + n.down, 0);
  const up = s.net.reduce((a, n) => a + n.up, 0);
  set('down', `↓${rate(down)}`);
  set('up', `↑${rate(up)}`);
}

// --- The temperature chart: liquid, CPU and GPU over the last 5 or 10 min.

const SERIES = [['liquid', 0, 'Liquid', '#8b6cff'], ['cpu', 1, 'CPU', '#c9741a'], ['gpu', 2, 'GPU', '#1fa98f']];
let minutes = 10;
try { minutes = +localStorage.getItem('lumora.overlayMinutes') === 5 ? 5 : 10; } catch { /* default */ }
let points = []; // {at, v: [liquid, cpu, gpu]}
let liquid = null;

function draw() {
  const now = Date.now();
  const span = minutes * 60000;
  const t0 = now - span;
  const shown = points.filter((p) => p.at >= t0 - 10000);
  const present = SERIES.filter(([, i]) => shown.some((p) => p.v[i] != null));
  let lo = Infinity, hi = -Infinity;
  for (const p of shown) for (const [, i] of present) if (p.v[i] != null) { lo = Math.min(lo, p.v[i]); hi = Math.max(hi, p.v[i]); }
  if (!isFinite(lo)) { lo = 20; hi = 80; }
  lo = Math.floor((lo - 2) / 5) * 5;
  hi = Math.max(lo + 15, Math.ceil((hi + 2) / 5) * 5);
  const x = (t) => ((t - t0) / span) * 200;
  const y = (v) => 3 + (1 - (v - lo) / (hi - lo)) * 86;
  let svg = [0.5].map((f) => `<line x1="0" x2="200" y1="${3 + f * 86}" y2="${3 + f * 86}"/>`).join('');
  for (const [, i, , color] of present) {
    let d = '', prev = null;
    for (const p of shown) {
      const v = p.v[i];
      if (v == null) { prev = null; continue; }
      d += `${prev == null || p.at - prev > 20000 ? 'M' : 'L'}${x(p.at).toFixed(1)},${y(v).toFixed(1)}`;
      prev = p.at;
    }
    svg += `<path d="${d}" stroke="${color}"/>`;
  }
  $('chart').innerHTML = svg;
  const keys = present.map(([, i, name, color]) => {
    const last = [...shown].reverse().find((p) => p.v[i] != null);
    return `<span class="key"><i style="background:${color}"></i>${name === 'Liquid' ? 'Liq' : name} ${last ? temp(last.v[i]) : '–'}</span>`;
  }).join('');
  if ($('keys').innerHTML !== keys) $('keys').innerHTML = keys;
  $('range').textContent = `${temp(lo)}–${temp(hi)}`;
  $('ago').textContent = `${minutes} min ago`;
  $('span').textContent = `${minutes}m`;
}

/** A live point each second (history only keeps one every 5 seconds). */
function addLive(s, sensors) {
  const l = sensors?.find((x) => x.name === 'Liquid');
  if (l) liquid = l.value;
  points.push({ at: s.at, v: [liquid, s.cpu.temp ?? null, s.gpu?.temp ?? null] });
  const keep = Date.now() - 10 * 60000 - 15000;
  while (points.length && points[0].at < keep) points.shift();
  draw();
}

async function loadHistory(invoke) {
  try {
    const hist = await invoke('history_range', { minutes: 10 });
    const first = points.length ? points[0].at : Infinity;
    points = [...hist.filter((p) => p.at < first).map((p) => ({ at: p.at, v: [p.v[0], p.v[1], p.v[2]] })), ...points];
    const last = [...hist].reverse().find((p) => p.v[0] != null);
    if (last && liquid == null) liquid = last.v[0];
  } catch { /* live points fill it in */ }
  draw();
}

async function start() {
  if (!tauri) return;
  const { invoke } = tauri.core;
  try {
    const st = await invoke('get_state');
    units = st.settings.units || units;
    document.body.classList.toggle('locked', !!st.settings.overlay?.locked);
  } catch { /* defaults */ }
  await tauri.event.listen('lumora://system', (e) => {
    show(e.payload.sample);
    addLive(e.payload.sample, e.payload.sensors);
  });
  loadHistory(invoke);
  await tauri.event.listen('lumora://overlay-lock', (e) => document.body.classList.toggle('locked', !!e.payload));
  await tauri.event.listen('lumora://settings', (e) => { units = e.payload.units || units; draw(); });
  const snap = await invoke('system_snapshot').catch(() => null);
  if (snap?.sample) show(snap.sample);
  invoke('monitor_watch', { what: 'overlay', on: true });
  $('lock').onclick = () => {
    document.body.classList.add('locked');
    invoke('overlay_lock', { locked: true });
  };
  $('span').onclick = () => {
    minutes = minutes === 10 ? 5 : 10;
    try { localStorage.setItem('lumora.overlayMinutes', String(minutes)); } catch { /* not remembered */ }
    draw();
  };
  $('close').onclick = () => invoke('overlay_toggle', { show: false });
}

start();
