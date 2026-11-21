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

async function start() {
  if (!tauri) return;
  const { invoke } = tauri.core;
  try {
    const st = await invoke('get_state');
    units = st.settings.units || units;
    document.body.classList.toggle('locked', !!st.settings.overlay?.locked);
  } catch { /* defaults */ }
  await tauri.event.listen('lumora://system', (e) => show(e.payload.sample));
  await tauri.event.listen('lumora://overlay-lock', (e) => document.body.classList.toggle('locked', !!e.payload));
  await tauri.event.listen('lumora://settings', (e) => { units = e.payload.units || units; });
  const snap = await invoke('system_snapshot').catch(() => null);
  if (snap?.sample) show(snap.sample);
  invoke('monitor_watch', { what: 'overlay', on: true });
  $('#lock').onclick = () => {
    document.body.classList.add('locked');
    invoke('overlay_lock', { locked: true });
  };
  $('#close').onclick = () => invoke('overlay_toggle', { show: false });
}

start();
