import { api, decodeFrame } from './bridge.js';
import { build, hexToRgb } from './effects.js';
import { DeviceView } from './render.js';
import { openPicker, closePicker } from './picker.js';
import { icon } from './icons.js';
import * as cooling from './cooling.js';

const $ = (s) => document.querySelector(s);
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

const GROUPS = [
  ['keyboard', 'Keyboards'], ['keypad', 'Keypads'], ['mouse', 'Mice'], ['mousemat', 'Mousepads'],
  ['headset', 'Headsets'], ['headset_stand', 'Headset stands'], ['gamepad', 'Controllers'],
  ['ram', 'Memory'], ['gpu', 'Graphics'], ['cooler', 'Cooling'], ['motherboard', 'Motherboards'],
  ['led_strip', 'Light strips'], ['case', 'Case'], ['light', 'Lights'], ['speaker', 'Speakers'],
  ['microphone', 'Microphones'], ['storage', 'Storage'], ['accessory', 'Accessories'], ['other', 'Other'],
];

const PALETTES = [
  ['Neon', ['#7c5cff', '#00d4ff', '#ff3da8']],
  ['Sunset', ['#ff5a1f', '#ff3d7f', '#7c3dff']],
  ['Ocean', ['#00ffd5', '#00a2ff', '#3d3dff']],
  ['Forest', ['#7dff3d', '#00ff85', '#00a37a']],
  ['Candy', ['#ff7ad9', '#ffd1f0', '#8ae6ff']],
  ['Ember', ['#ff1e00', '#ff8a00', '#ffe066']],
  ['Vapor', ['#ff71ce', '#01cdfe', '#b967ff', '#fffb96']],
  ['Mono', ['#ffffff', '#5c5c70']],
];

const THEMES = [
  ['dark', 'Ink', { bg: '#0f0e17', surface: '#1d1b29', stage: '#0b0a12' }],
  ['oled', 'OLED black', { bg: '#000000', surface: '#0d0d12', stage: '#000000' }],
  ['midnight', 'Midnight', { bg: '#0a1020', surface: '#152038', stage: '#070c19' }],
  ['light', 'Daylight', { bg: '#e9ebf2', surface: '#ffffff', stage: '#15141d' }],
  ['frost', 'Frost', { bg: 'linear-gradient(135deg,#3b2a6e,#0c4a63)', surface: 'rgba(255,255,255,.18)', stage: 'rgba(0,0,0,.45)' }],
];

const ACCENTS = ['#7c5cff', '#3d6bff', '#00b3ff', '#00c9a7', '#3ddc97', '#ffb000', '#ff5a1f', '#ff3d7f'];

// Effects that look wrong in arbitrary colors start from their own palette.
const SIGNATURE_PALETTES = new Set(['fire', 'temperature']);

const DIRECTIONS = [['left', 'Left'], ['right', 'Right'], ['up', 'Up'], ['down', 'Down'], ['out', 'Outward'], ['in', 'Inward']];

const state = {
  app: null,
  selected: '', // '' = all devices
  views: new Map(),
  lastColors: new Map(),
  tiles: [],
};

const snap = () => state.app.snapshot;

/** Display names, numbered when several devices share one (two identical RAM sticks). */
function labels() {
  const count = {}, seen = {}, out = {};
  for (const d of devices()) count[d.name] = (count[d.name] || 0) + 1;
  for (const d of devices()) {
    seen[d.name] = (seen[d.name] || 0) + 1;
    out[d.id] = count[d.name] > 1 && !/\d$/.test(d.name) ? `${d.name} ${seen[d.name]}` : d.name;
  }
  return out;
}
const devices = () => snap().devices;
const selectedDevice = () => devices().find((d) => d.id === state.selected) || null;
const effectById = (id) => state.app.effects.find((e) => e.id === id) || state.app.effects[0];
const currentConfig = () => (state.selected && snap().overrides[state.selected]) || snap().global;

// --- Sending changes --------------------------------------------------------

// Slider drags send at most one update per animation frame.
let pending = null;
function applyConfig(cfg) {
  if (state.selected) snap().overrides[state.selected] = cfg;
  else snap().global = cfg;
  if (!pending) {
    requestAnimationFrame(() => {
      const { device, config } = pending;
      pending = null;
      api.invoke('set_effect', { device, config });
    });
  }
  pending = { device: state.selected || null, config: cfg };
  markProfileModified();
  renderEffects();
  renderInspector();
  renderStageHead();
  renderSidebar();
}

function colorsFor(effect, from) {
  if (!effect.max_colors) return [];
  let colors = (from || []).slice(0, effect.max_colors);
  if (colors.length < effect.min_colors) {
    const fill = effect.default_colors.filter((c) => !colors.includes(c));
    colors = colors.concat(fill).slice(0, Math.max(effect.min_colors, colors.length));
  }
  return colors;
}

function selectEffect(id) {
  const cfg = currentConfig();
  const e = effectById(id);
  // Carry the user's colors over to the new effect where it uses colors.
  const own = SIGNATURE_PALETTES.has(id) && cfg.effect !== id;
  const keep = cfg.colors.length && !own ? cfg.colors : e.default_colors;
  applyConfig({ ...cfg, effect: id, colors: colorsFor(e, keep) });
}

// --- Sidebar ----------------------------------------------------------------

// --- Who controls a device -----------------------------------------------------

/** The vendor app that usually owns this device's lighting, if known. */
function vendorApp(d) {
  return /nzxt/i.test(d.vendor) ? 'NZXT CAM' : null;
}

function handsOffBadge(d, short) {
  const app = vendorApp(d);
  const text = short ? (app ? app.replace(/^NZXT /, '') : 'Off') : app ? 'In ' + app : 'Not controlled';
  return `<span class="nav-badge muted" title="${esc(app ? 'Lighting is left to ' + app : 'Lumora isn\u2019t controlling this device')}">${esc(text)}</span>`;
}

async function setControl(d, on) {
  state.app.settings.device_control = { ...(state.app.settings.device_control || {}), [d.id]: on };
  d.managed = on;
  try {
    state.app.settings = await api.invoke('set_device_control', { device: d.id, on });
  } catch (err) {
    toast(String(err));
  }
  renderAll();
  toast(on ? `Lumora now controls ${labels()[d.id]}` : `${labels()[d.id]} is left to ${vendorApp(d) || 'its own app'}`);
}

function askControl(d) {
  const app = vendorApp(d);
  if (!app) return setControl(d, true);
  $('#controlTitle').textContent = `Control ${labels()[d.id]} with Lumora?`;
  $('#controlText').textContent = `While ${app} is open, it keeps applying its own lighting, so the colors can flicker between the two apps. Quit ${app}, or turn its lighting off, for steady colors.`;
  $('#controlKeep').textContent = `Pump and fan speeds and the screen stay with ${app}. Lumora only changes the lighting, and you can hand it back any time.`;
  $('#controlDialog').showModal();
  $('#controlGo').onclick = () => {
    $('#controlDialog').close();
    setControl(d, true);
  };
}

$('#controlCancel').addEventListener('click', () => $('#controlDialog').close());

function renderSidebar() {
  const list = devices();
  const names = labels();
  $('#deviceCount').textContent = list.length;
  const all = document.querySelector('.nav-all');
  all.setAttribute('aria-current', String(!state.selected));
  all.querySelector('.nav-icon').innerHTML = icon('all');

  const html = GROUPS.map(([kind, label]) => {
    const items = list.filter((d) => d.kind === kind);
    if (!items.length) return '';
    return `<div class="group-title">${label}</div>` + items.map((d) => {
      const custom = snap().overrides[d.id];
      return `<button class="nav-item${d.controllable ? '' : ' unsupported'}" data-device="${esc(d.id)}" aria-current="${state.selected === d.id}" title="${esc(d.vendor ? d.vendor + ' ' + d.name : d.name)}">
        <span class="nav-icon">${icon(d.kind)}</span>
        <span class="nav-name">${esc(names[d.id])}</span>
        ${!d.managed ? handsOffBadge(d, true) : custom ? '<span class="nav-badge">Custom</span>' : ''}
        <span class="nav-glow" data-glow="${esc(d.id)}"></span>
      </button>`;
    }).join('');
  }).join('');
  $('#deviceList').innerHTML = html + cooling.sidebarHtml();
  paintGlows();
}

function paintGlows() {
  for (const el of document.querySelectorAll('[data-glow]')) {
    const c = averageOf(state.lastColors.get(el.dataset.glow));
    if (c) el.style.setProperty('--c', `${c[0] | 0} ${c[1] | 0} ${c[2] | 0}`);
  }
}

function averageOf(colors) {
  if (!colors || !colors.length) return null;
  let r = 0, g = 0, b = 0;
  for (const c of colors) { r += c[0]; g += c[1]; b += c[2]; }
  return [r / colors.length, g / colors.length, b / colors.length];
}

document.addEventListener('click', (e) => {
  const item = e.target.closest('[data-device]');
  if (!item) return;
  select(item.dataset.device);
});

/** Opens the page that was open last time, once its device has shown up
 *  (devices and coolers appear a few seconds after launch). */
function restoreSelection() {
  const id = state.restore;
  if (!id || state.selected) return false;
  const known = cooling.coolerId(id) ? (state.app.coolers || []).some((c) => 'cooler:' + c.id === id) : devices().some((d) => d.id === id);
  if (!known) return false;
  state.restore = '';
  select(id);
  return true;
}

function select(id) {
  state.selected = id;
  try { localStorage.setItem('lumora.selected', id); } catch { /* not remembered */ }
  document.body.classList.toggle('cooler-view', !!cooling.coolerId(id));
  document.body.classList.toggle('device-hands-off', !!selectedDevice() && !selectedDevice().managed);
  closePicker();
  renderSidebar();
  renderStage();
  renderEffects();
  renderInspector();
}

function engineSummary() {
  const n = devices().filter((d) => !d.virtual_device).length;
  if (!n) return 'The hardware engine is running and looking for devices.';
  return `The hardware engine is running and controls ${n === 1 ? 'one device' : n + ' devices'}.`;
}

const engine = () => state.app.engine || { mode: 'external', bundled: false, full_access: 'unsupported' };
const builtin = () => engine().mode !== 'external';

function renderConnection() {
  const c = snap().connection;
  const e = engine();
  const box = $('#connection');
  box.dataset.status = c.status;
  const demo = devices().some((d) => d.virtual_device);
  let title, detail;
  if (builtin()) {
    title = { connected: 'Hardware engine running', connecting: 'Starting hardware engine…', offline: 'Starting hardware engine…' }[c.status];
    const n = devices().filter((d) => !d.virtual_device).length;
    detail = c.status === 'connected'
      ? (n ? c.message : 'Looking for devices…')
      : 'This can take up to a minute';
    // A real error beats "starting": show what went wrong.
    if (c.status === 'offline' && c.message && !/isn't running/.test(c.message)) {
      title = 'Can\u2019t reach the hardware engine';
      detail = c.message;
    }
    if (e.problem) {
      title = 'Hardware engine stopped';
      detail = e.problem;
    }
  } else {
    title = { connected: 'OpenRGB connected', connecting: 'Looking for OpenRGB…', offline: demo ? 'Showing demo devices' : 'OpenRGB offline' }[c.status];
    detail = c.status === 'connected' ? c.message : c.status === 'offline' ? 'Start OpenRGB to connect' : c.server;
  }
  $('#connTitle').textContent = title;
  $('#connDetail').textContent = detail;
  const stuck = c.status === 'connected' && /not responding/.test(c.message);
  $('#connAction').hidden = !stuck && (c.status === 'connected' || builtin());
  $('#connAction').textContent = stuck ? 'Details' : 'Set up';
  const s = $('#settingsStatus');
  if (s) {
    s.textContent = c.status === 'connected'
      ? (builtin() ? engineSummary() : `Connected to ${c.server}. ${c.message}.`)
      : builtin() ? 'The hardware engine is starting.' : c.message || 'Connecting…';
  }
}

$('#connAction').addEventListener('click', () => openSettings(builtin() ? '#reportBtn' : '#host'));

// --- RAM & motherboard (full hardware access) -------------------------------------

function renderHardware() {
  const e = engine();
  const supported = e.full_access !== 'unsupported';
  const card = $('#fullAccessCard');
  card.hidden = !supported;
  card.classList.toggle('on', e.full_access === 'on');
  const btn = $('#fullAccessBtn');
  const text = {
    off: 'Off. Needs a one-time Windows permission.',
    on: 'On. Lumora can reach your RAM and motherboard.',
    needs_update: 'On. A newer hardware engine is ready to install.',
  }[e.full_access] || '';
  $('#fullAccessText').textContent = text;
  btn.textContent = { off: 'Turn on', on: 'Turn off', needs_update: 'Update' }[e.full_access] || 'Turn on';
  btn.classList.toggle('primary', e.full_access !== 'on');
  $('#builtinRow').hidden = !e.bundled;
  $('#externalNote').hidden = builtin();
  $('#getOpenRgb').hidden = builtin();
}

function openFullAccess() {
  $('#fullAccessStatus').hidden = true;
  $('#fullAccessGo').disabled = false;
  $('#fullAccessCancel').disabled = false;
  if ($('#settings').open) $('#settings').close();
  $('#fullAccessDialog').showModal();
}

async function runFullAccess(enable) {
  try {
    state.app.engine = await api.invoke(enable ? 'enable_full_access' : 'disable_full_access');
    renderHardware();
    renderStageHead();
    return null;
  } catch (err) {
    return String(err);
  }
}

$('#fullAccessBtn').addEventListener('click', async () => {
  const e = engine();
  if (e.full_access === 'off') return openFullAccess();
  const btn = $('#fullAccessBtn');
  btn.disabled = true;
  $('#fullAccessText').textContent = 'Waiting for Windows permission…';
  const err = await runFullAccess(e.full_access === 'needs_update');
  btn.disabled = false;
  renderHardware();
  if (err) $('#fullAccessText').textContent = err;
  else toast(e.full_access === 'needs_update' ? 'Hardware engine updated' : 'RAM & motherboard lighting is off');
});

$('#fullAccessCancel').addEventListener('click', () => $('#fullAccessDialog').close());
$('#fullAccessGo').addEventListener('click', async () => {
  $('#fullAccessGo').disabled = true;
  $('#fullAccessCancel').disabled = true;
  const status = $('#fullAccessStatus');
  status.hidden = false;
  status.textContent = 'Waiting for Windows permission…';
  const err = await runFullAccess(true);
  $('#fullAccessCancel').disabled = false;
  if (err) {
    status.textContent = err;
    $('#fullAccessGo').disabled = false;
    $('#fullAccessGo').textContent = 'Try again';
    return;
  }
  $('#fullAccessDialog').close();
  toast('RAM & motherboard lighting is on. Devices appear in a few seconds.');
});

// --- Stage ------------------------------------------------------------------

const resizer = new ResizeObserver((entries) => {
  for (const en of entries) state.views.get(en.target.dataset.view)?.resize();
});

function makeView(canvas, device, opts) {
  canvas.dataset.view = device.id;
  const v = new DeviceView(canvas, device, opts);
  const last = state.lastColors.get(device.id);
  if (last) v.update(last);
  state.views.set(device.id, v);
  resizer.observe(canvas);
  return v;
}

function renderStageHead() {
  const cooler = cooling.selectedCooler();
  if (cooler) return cooling.renderHead(cooler);
  const d = selectedDevice();
  const actions = $('#stageActions');
  if (!d) {
    const all = devices();
    const list = all.filter((x) => x.managed);
    const handsOff = all.filter((x) => !x.managed);
    const custom = list.filter((x) => snap().overrides[x.id]).length;
    $('#stageTitle').textContent = 'All devices';
    const demo = list.some((x) => x.virtual_device);
    $('#stageSub').textContent = demo
      ? (builtin() ? 'These are demo devices. Yours replace them as soon as the hardware engine finds them.' : 'These are demo devices. Connect OpenRGB to light up your own.')
      : !list.length
        ? ''
        : custom
          ? `Effects here apply to every device except the ${custom === 1 ? 'one' : custom} with ${custom === 1 ? 'its' : 'their'} own effect.`
          : `One effect across all ${list.length} devices, in sync.`;
    if (handsOff.length && !demo) {
      const names = labels();
      $('#stageSub').textContent += ` ${handsOff.map((x) => names[x.id]).join(' and ')} ${handsOff.length === 1 ? 'is' : 'are'} left to ${vendorApp(handsOff[0]) || 'its own app'}.`;
    }
    const boardless = !list.some((x) => !x.virtual_device && (x.kind === 'ram' || x.kind === 'motherboard'));
    const offerFull = engine().full_access === 'off' && boardless;
    actions.innerHTML = (offerFull ? `<button class="chip" id="addBoard">${icon('ram')}Add RAM &amp; motherboard</button>` : '')
      + (custom ? `<button class="chip accent" id="syncAll">${icon('reset')}Sync all devices</button>` : '');
    $('#addBoard')?.addEventListener('click', openFullAccess);
    $('#syncAll')?.addEventListener('click', () => {
      api.invoke('sync_all', { config: snap().global });
      snap().overrides = {};
      markProfileModified();
      renderAll();
      toast('Every device now runs the same effect');
    });
    return;
  }
  $('#stageTitle').textContent = labels()[d.id];
  const bits = [d.vendor, `${d.led_count} LED${d.led_count === 1 ? '' : 's'}`, d.zones.length > 1 ? `${d.zones.length} zones` : ''].filter(Boolean);
  $('#stageSub').textContent = d.controllable ? bits.join(', ') : 'OpenRGB has no direct mode for this device, so Lumora can’t animate it.';
  const custom = !!snap().overrides[d.id];
  const control = d.virtual_device ? '' : `<label class="chip control-chip" title="Choose whether Lumora changes this device's lighting">
      <span>Controlled by Lumora</span><input type="checkbox" class="switch small" id="controlSwitch" ${d.managed ? 'checked' : ''}>
    </label>`;
  actions.innerHTML = !d.managed
    ? control
    : (custom
      ? `<span class="chip">Own effect</span><button class="chip" id="useShared">${icon('reset')}Follow all devices</button>`
      : '<span class="chip">Following all devices</span>') + control;
  $('#controlSwitch')?.addEventListener('change', (ev) => {
    if (ev.target.checked) {
      ev.target.checked = false;
      askControl(d);
    } else {
      setControl(d, false);
    }
  });
  $('#useShared')?.addEventListener('click', () => {
    delete snap().overrides[d.id];
    api.invoke('clear_override', { device: d.id });
    markProfileModified();
    renderAll();
  });
}

function renderStage() {
  for (const v of state.views.values()) resizer.unobserve(v.canvas);
  state.views.clear();
  const body = $('#stageBody');
  renderStageHead();
  const cooler = cooling.selectedCooler();
  document.body.classList.toggle('cooler-view', !!cooler);
  if (cooler) return cooling.renderPage(cooler);
  const d = selectedDevice();
  if (d) {
    const sizable = d.zones.map((z, i) => ({ ...z, index: i })).filter((z) => z.resizable);
    const empty = d.led_count === 0;
    body.innerHTML = `<canvas class="stage-single${sizable.length ? ' with-setup' : ''}"></canvas>`
      + (empty ? `<div class="empty stage-hint"><strong>Set up this device's LEDs</strong><span>Headers for fans and strips can't tell how many LEDs are plugged in. Enter a number for each one below and they light up right away.</span></div>` : '')
      + (sizable.length ? `<div class="zone-setup" aria-label="LED counts">
          <div class="zone-setup-head"><strong>LED counts</strong><span>Each fan usually has 8 to 16 LEDs. For chained fans, add them up: 3 fans with 12 LEDs each is 36.</span></div>
          ${sizable.map((z) => `<label class="zone-row"><span>${esc(z.name)}</span>
            <input type="number" inputmode="numeric" min="${z.resizable[0]}" max="${z.resizable[1]}" value="${z.count}" data-zone="${z.index}" aria-label="${esc(z.name)} LED count">
          </label>`).join('')}
        </div>` : '');
    makeView(body.querySelector('canvas'), d, { padding: 0.1 });
    if (!d.managed) {
      body.querySelector('canvas').classList.add('dimmed');
      body.querySelector('.zone-setup')?.remove();
      const app = vendorApp(d);
      body.insertAdjacentHTML('beforeend', `<div class="hands-off">
        <strong>${app ? `Lighting is left to ${esc(app)}` : 'Lumora isn\u2019t controlling this device'}</strong>
        <span>${app ? `Lumora doesn\u2019t touch this device, so ${esc(app)} keeps its lighting, cooling and screen exactly as you set them.` : 'Lumora doesn\u2019t send anything to this device.'}</span>
        <button class="btn primary" id="takeControl">Control with Lumora</button>
      </div>`);
      $('#takeControl').addEventListener('click', () => askControl(d));
    }
    body.querySelectorAll('[data-zone]').forEach((input) => {
      input.addEventListener('change', () => {
        const [min, max] = d.zones[+input.dataset.zone].resizable;
        const size = Math.max(min, Math.min(max, parseInt(input.value, 10) || 0));
        input.value = size;
        api.invoke('resize_zone', { device: d.id, zone: +input.dataset.zone, size });
        toast(`${d.zones[+input.dataset.zone].name} set to ${size} LED${size === 1 ? '' : 's'}`);
      });
    });
    return;
  }
  const list = devices();
  if (!list.length) {
    body.innerHTML = `<div class="empty"><strong>No devices yet</strong><span>OpenRGB is connected but hasn’t detected any RGB hardware. Run a rescan in OpenRGB and your devices appear here.</span></div>`;
    return;
  }
  const names = labels();
  body.innerHTML = `<div class="rig">${list.map((x) => {
    const wide = ['keyboard', 'led_strip', 'gpu', 'cooler'].includes(x.kind) || x.led_count > 40;
    return `<button class="rig-card${wide ? ' wide' : ''}${x.managed ? '' : ' hands-off-card'}" data-device="${esc(x.id)}" aria-label="${esc(names[x.id])}">
      <canvas></canvas>
      <span class="rig-label"><span>${esc(names[x.id])}</span>${!x.managed ? handsOffBadge(x) : x.led_count === 0 ? '<span class="nav-badge">Set up LEDs</span>' : snap().overrides[x.id] ? '<span class="nav-badge">Custom</span>' : ''}</span>
    </button>`;
  }).join('')}</div>`;
  body.querySelectorAll('.rig-card').forEach((card, i) => makeView(card.querySelector('canvas'), list[i], { padding: 0.12, bloom: 0.9 }));
}

// --- Effects shelf -----------------------------------------------------------

const TILE_LEDS = Array.from({ length: 28 }, (_, i) => ({ x: i / 27, y: 0.5 }));

function renderEffects() {
  const cfg = currentConfig();
  const grid = $('#effectGrid');
  if (!grid.children.length) {
    grid.innerHTML = state.app.effects.map((e) => `<button class="effect-tile" role="radio" data-effect="${e.id}" title="${esc(e.description)}">
      <canvas width="168" height="26"></canvas><span>${esc(e.name)}</span></button>`).join('');
    grid.addEventListener('click', (ev) => {
      const t = ev.target.closest('[data-effect]');
      if (t) selectEffect(t.dataset.effect);
    });
  }
  state.tiles = [...grid.children].map((tile) => {
    const e = effectById(tile.dataset.effect);
    tile.setAttribute('aria-checked', String(e.id === cfg.effect));
    const keep = cfg.colors.length && !(SIGNATURE_PALETTES.has(e.id) && cfg.effect !== e.id) ? cfg.colors : e.default_colors;
    const tcfg = { ...cfg, effect: e.id, colors: colorsFor(e, keep), speed: Math.max(cfg.speed, 30), size: 50 };
    return { canvas: tile.querySelector('canvas'), render: build(tcfg, e, TILE_LEDS.length), out: new Array(TILE_LEDS.length).fill([0, 0, 0]) };
  });
}

let tileLast = performance.now();
function animateTiles(now) {
  const t = now / 1000, dt = Math.min((now - tileLast) / 1000, 0.1);
  tileLast = now;
  if (!document.hidden) {
    for (const tile of state.tiles) {
      // Tiles sweep through a fake temperature so the effect reads at a glance.
      tile.render(t, dt, TILE_LEDS, tile.out, 32 + 14 * (0.5 + 0.5 * Math.sin(t * 0.7)));
      const ctx = tile.canvas.getContext('2d');
      const w = tile.canvas.width, h = tile.canvas.height, step = w / TILE_LEDS.length;
      ctx.clearRect(0, 0, w, h);
      tile.out.forEach((c, i) => {
        ctx.fillStyle = `rgb(${c[0] | 0},${c[1] | 0},${c[2] | 0})`;
        ctx.fillRect(i * step, 0, step + 0.6, h);
      });
    }
  }
  requestAnimationFrame(animateTiles);
}

// --- Inspector --------------------------------------------------------------

function setRange(el, value, min = 0, max = 100) {
  el.value = value;
  el.style.setProperty('--p', `${((value - min) / (max - min)) * 100}%`);
}

function tempReading(source) {
  return (state.app.temps || []).find((t) => t.source === source)?.celsius;
}

function renderTempNow() {
  const el = $('#tempNow');
  if (!el || $('#tempControl').hidden) return;
  const source = currentConfig().temp_source || 'liquid';
  const v = tempReading(source);
  if (v == null && source === 'cpu') {
    el.innerHTML = `${esc(cooling.cpuHint())} <button type="button" class="link-btn" data-cpu-setup>Turn on CPU temperature</button>`;
    return;
  }
  el.textContent = v != null
    ? `Now ${v.toFixed(1)} °C`
    : source === 'liquid'
      ? 'No liquid reading yet. Let Lumora run your cooler (in the Cooling section) to use it.'
      : 'No GPU reading. This works with NVIDIA graphics cards.';
}

function renderInspector() {
  const cfg = currentConfig();
  const e = effectById(cfg.effect);
  $('#effectName').textContent = e.name;
  $('#effectDesc').textContent = e.description;

  // Colors
  const colorsCtl = $('#colorsControl');
  colorsCtl.hidden = e.max_colors === 0;
  if (e.max_colors > 0) {
    const row = $('#colorRow');
    const canRemove = cfg.colors.length > Math.max(e.min_colors, 1);
    row.innerHTML = cfg.colors.map((c, i) => `<button class="color-chip" style="--c:${c}" data-i="${i}" aria-label="Color ${i + 1}, ${c}"></button>`).join('')
      + (cfg.colors.length < e.max_colors ? `<button class="color-add" aria-label="Add a color" title="Add a color">${icon('plus')}</button>` : '');
    row.querySelectorAll('.color-chip').forEach((chip) => {
      chip.addEventListener('click', () => {
        const i = +chip.dataset.i;
        openPicker(chip, currentConfig().colors[i], (hex) => {
          const c = currentConfig();
          const colors = c.colors.slice();
          colors[i] = hex;
          chip.style.setProperty('--c', hex);
          applyConfigQuiet({ ...c, colors });
        }, canRemove ? {
          onRemove: () => {
            const c = currentConfig();
            applyConfig({ ...c, colors: c.colors.filter((_, k) => k !== i) });
          },
        } : {});
      });
    });
    row.querySelector('.color-add')?.addEventListener('click', () => {
      const c = currentConfig();
      const next = PALETTES.flatMap((p) => p[1]).find((x) => !c.colors.includes(x)) || '#ffffff';
      applyConfig({ ...c, colors: [...c.colors, next] });
      row.querySelector(`[data-i="${c.colors.length}"]`)?.click();
    });
    const hint = e.min_colors === e.max_colors ? (e.max_colors === 1 ? '' : `${e.max_colors} colors`) : `up to ${e.max_colors}`;
    $('#colorHint').textContent = hint;
    const pal = $('#palettes');
    pal.hidden = e.max_colors < 2;
    if (!pal.children.length) {
      pal.innerHTML = PALETTES.map(([name, cols]) => `<button class="palette" data-p="${name}" title="${name}"><i style="--g:linear-gradient(90deg,${cols.join(',')})"></i><small>${name}</small></button>`).join('');
      pal.addEventListener('click', (ev) => {
        const b = ev.target.closest('[data-p]');
        if (!b) return;
        const c = currentConfig();
        const eff = effectById(c.effect);
        const cols = PALETTES.find((p) => p[0] === b.dataset.p)[1];
        applyConfig({ ...c, colors: colorsFor(eff, cols) });
      });
    }
  }

  $('#speedControl').hidden = !e.uses_speed;
  setRange($('#speed'), cfg.speed);
  $('#speedOut').textContent = cfg.speed;

  $('#sizeControl').hidden = !e.uses_size;
  $('#sizeLabel').textContent = e.size_label || 'Size';
  setRange($('#size'), cfg.size);
  $('#sizeOut').textContent = cfg.size;

  $('#directionControl').hidden = !e.uses_direction;
  $('#tempControl').hidden = !e.uses_temperature;
  if (e.uses_temperature) {
    setRadio($('#tempSource'), cfg.temp_source || 'liquid');
    setRange($('#tempLow'), cfg.temp_low ?? 30, 20, 95);
    setRange($('#tempHigh'), cfg.temp_high ?? 45, 20, 95);
    $('#tempLowOut').textContent = `${cfg.temp_low ?? 30} °C`;
    $('#tempHighOut').textContent = `${cfg.temp_high ?? 45} °C`;
    renderTempNow();
  }
  const dirs = e.id === 'ripple' ? DIRECTIONS.filter(([d]) => d === 'in' || d === 'out') : DIRECTIONS;
  const dirEl = $('#direction');
  const active = e.id === 'ripple' && cfg.direction !== 'in' ? 'out' : cfg.direction;
  dirEl.innerHTML = dirs.map(([d, label]) => `<button role="radio" data-dir="${d}" aria-checked="${active === d}" title="${label}" aria-label="${label}">${icon(d)}</button>`).join('');

  setRange($('#brightness'), snap().brightness);
  $('#brightnessOut').textContent = snap().brightness + '%';
}

// Live picker drags: update the lights without re-rendering the picker away.
function applyConfigQuiet(cfg) {
  if (state.selected) snap().overrides[state.selected] = cfg;
  else snap().global = cfg;
  if (!pending) {
    requestAnimationFrame(() => {
      const { device, config } = pending;
      pending = null;
      api.invoke('set_effect', { device, config });
      renderEffects();
    });
  }
  pending = { device: state.selected || null, config: cfg };
  markProfileModified();
}

for (const key of ['speed', 'size']) {
  $('#' + key).addEventListener('input', (ev) => {
    const v = +ev.target.value;
    setRange(ev.target, v);
    $(`#${key}Out`).textContent = v;
    applyConfigQuiet({ ...currentConfig(), [key]: v });
  });
  $('#' + key).addEventListener('change', () => renderEffects());
}

$('#tempSource').addEventListener('click', (ev) => {
  const b = ev.target.closest('[data-v]');
  if (!b) return;
  // Sensible ranges per source: liquid runs cool, CPUs and GPUs run hot.
  const range = { gpu: [40, 80], cpu: [40, 85] }[b.dataset.v] || [30, 45];
  applyConfig({ ...currentConfig(), temp_source: b.dataset.v, temp_low: range[0], temp_high: range[1] });
});

for (const [id, key] of [['#tempLow', 'temp_low'], ['#tempHigh', 'temp_high']]) {
  $(id).addEventListener('input', (ev) => {
    const v = +ev.target.value;
    setRange(ev.target, v, 20, 95);
    $(id + 'Out').textContent = `${v} °C`;
    applyConfigQuiet({ ...currentConfig(), [key]: v });
  });
}

$('#direction').addEventListener('click', (ev) => {
  const b = ev.target.closest('[data-dir]');
  if (b) applyConfig({ ...currentConfig(), direction: b.dataset.dir });
});

let brightnessTimer = null;
$('#brightness').addEventListener('input', (ev) => {
  const v = +ev.target.value;
  setRange(ev.target, v);
  $('#brightnessOut').textContent = v + '%';
  snap().brightness = v;
  if (!brightnessTimer) {
    brightnessTimer = setTimeout(() => {
      brightnessTimer = null;
      api.invoke('set_brightness', { value: snap().brightness });
    }, 40);
  }
  markProfileModified();
});

// --- Power ------------------------------------------------------------------

function renderPower() {
  const on = snap().enabled;
  $('#powerBtn').setAttribute('aria-checked', String(on));
  $('#powerLabel').textContent = on ? 'Lights on' : 'Lights off';
}

$('#powerBtn').addEventListener('click', () => {
  snap().enabled = !snap().enabled;
  api.invoke('set_enabled', { on: snap().enabled });
  renderPower();
});

// --- Profiles ---------------------------------------------------------------

let profileModified = false;
function markProfileModified() {
  if (state.app.active_profile && !profileModified) {
    profileModified = true;
    renderProfileName();
  }
}

function renderProfileName() {
  const name = state.app.active_profile;
  $('#profileName').textContent = name ? name + (profileModified ? ' (edited)' : '') : 'Unsaved';
}

function profileGradient(p) {
  const cols = p.global.colors.length ? p.global.colors : ['#ff2d55', '#ffd400', '#00ff85', '#00d4ff', '#7c5cff'];
  return cols.length === 1 ? cols[0] : `linear-gradient(135deg,${cols.join(',')})`;
}

function renderProfileMenu() {
  const menu = $('#profileMenu');
  const active = state.app.active_profile;
  menu.innerHTML = state.app.profiles.map((p) => `<div class="menu-item" role="menuitemradio" tabindex="0" aria-checked="${p.name === active}" data-load="${esc(p.name)}">
      <span class="menu-swatch" style="--g:${profileGradient(p)}"></span>
      <span class="menu-text">${esc(p.name)}</span>
      <button class="menu-del" data-del="${esc(p.name)}" aria-label="Delete ${esc(p.name)}" title="Delete">${icon('trash')}</button>
    </div>`).join('')
    + (state.app.profiles.length ? '<div class="menu-sep"></div>' : '')
    + (active && profileModified ? `<button class="menu-item" data-update="${esc(active)}">${icon('save')}<span class="menu-text">Update ${esc(active)}</span></button>` : '')
    + `<form class="menu-save" id="saveForm"><input id="saveName" placeholder="Save current lighting as…" maxlength="40" aria-label="Profile name"><button class="btn primary">Save</button></form>`;
}

async function saveProfile(name) {
  try {
    state.app.profiles = await api.invoke('save_profile', { name });
    state.app.active_profile = name.trim();
    profileModified = false;
    renderProfileName();
    closeMenu();
    toast(`Saved ${name.trim()}`);
  } catch (err) {
    toast(String(err));
  }
}

function closeMenu() {
  $('#profileMenu').hidden = true;
  $('#profileBtn').setAttribute('aria-expanded', 'false');
}

$('#profileBtn').addEventListener('click', () => {
  const menu = $('#profileMenu');
  if (!menu.hidden) return closeMenu();
  renderProfileMenu();
  menu.hidden = false;
  $('#profileBtn').setAttribute('aria-expanded', 'true');
});

$('#profileMenu').addEventListener('click', async (ev) => {
  const del = ev.target.closest('[data-del]');
  if (del) {
    ev.stopPropagation();
    state.app.profiles = await api.invoke('delete_profile', { name: del.dataset.del });
    if (state.app.active_profile === del.dataset.del) state.app.active_profile = null;
    renderProfileName();
    renderProfileMenu();
    return;
  }
  const upd = ev.target.closest('[data-update]');
  if (upd) return saveProfile(upd.dataset.update);
  const load = ev.target.closest('[data-load]');
  if (load) {
    await api.invoke('load_profile', { name: load.dataset.load });
    state.app.active_profile = load.dataset.load;
    profileModified = false;
    renderProfileName();
    closeMenu();
    await refreshSnapshot();
  }
});

$('#profileMenu').addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter' && ev.target.dataset.load) ev.target.click();
});

$('#profileMenu').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const name = $('#saveName').value.trim();
  if (name) saveProfile(name);
});

document.addEventListener('pointerdown', (ev) => {
  if (!ev.target.closest('.profile-menu')) closeMenu();
});
document.addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') closeMenu();
});

// --- Theme & accent -------------------------------------------------------------

let followed = null;
function applyTheme() {
  const s = state.app.settings;
  document.documentElement.dataset.theme = s.theme;
  if (!s.accent_follows_lighting) document.documentElement.style.setProperty('--accent', s.accent);
  for (const v of state.views.values()) v.refreshTheme();
}

// Every few frames, tint the stage (and optionally the accent) from the lights.
let lastAmbient = 0;
function updateAmbient(now) {
  if (now - lastAmbient < 120) return;
  lastAmbient = now;
  const ids = state.selected ? [state.selected] : [...state.views.keys()];
  let r = 0, g = 0, b = 0, n = 0;
  let vivid = null, vividScore = 0;
  for (const id of ids) {
    const cols = state.lastColors.get(id);
    if (!cols) continue;
    for (const c of cols) {
      r += c[0]; g += c[1]; b += c[2]; n++;
      const max = Math.max(...c), min = Math.min(...c);
      const score = max * (max - min);
      if (score > vividScore) { vividScore = score; vivid = c; }
    }
  }
  if (!n) return;
  const avg = [r / n, g / n, b / n];
  // Boost so dim scenes still read, without blowing out bright ones.
  const peak = Math.max(...avg, 1);
  const k = Math.min(255 / peak, 3) * 0.9;
  document.documentElement.style.setProperty('--ambient', avg.map((v) => Math.round(Math.min(v * k, 255))).join(' '));
  if (state.app.settings.accent_follows_lighting && vivid && vividScore > 4000) {
    followed = followed ? followed.map((v, i) => v + (vivid[i] - v) * 0.25) : vivid.slice();
    const m = Math.max(...followed, 1), boost = 230 / m;
    document.documentElement.style.setProperty('--accent', `rgb(${followed.map((v) => Math.round(Math.min(v * boost, 255))).join(',')})`);
  }
}

// --- Settings ---------------------------------------------------------------

async function saveSettings(patch) {
  Object.assign(state.app.settings, patch);
  applyTheme();
  try {
    state.app.settings = await api.invoke('update_settings', { settings: state.app.settings });
  } catch (err) {
    toast(String(err));
  }
  renderSettings();
}

function setRadio(container, value) {
  container.querySelectorAll('[data-v]').forEach((b) => b.setAttribute('aria-checked', String(b.dataset.v === String(value))));
}

function renderSettings() {
  const s = state.app.settings;
  $('#themeGrid').innerHTML = THEMES.map(([id, name, t]) => `<button type="button" class="theme-card" role="radio" data-theme-id="${id}" aria-checked="${s.theme === id}" style="--t-bg:${t.bg};--t-surface:${t.surface};--t-stage:${t.stage}"><span class="thumb"></span><span>${name}</span></button>`).join('');
  const custom = !ACCENTS.includes(s.accent);
  $('#accentRow').innerHTML = ACCENTS.map((c) => `<button type="button" class="accent-dot" style="--c:${c}" data-accent="${c}" role="radio" aria-checked="${!s.accent_follows_lighting && s.accent === c}" aria-label="Accent ${c}"></button>`).join('')
    + `<button type="button" class="accent-dot custom" id="accentCustom" style="--c:${s.accent}" aria-checked="${!s.accent_follows_lighting && custom}" aria-label="Custom accent">${icon('plus')}</button>`;
  $('#accentFollow').checked = s.accent_follows_lighting;
  if (document.activeElement !== $('#host')) $('#host').value = s.host;
  if (document.activeElement !== $('#port')) $('#port').value = s.port;
  setRadio($('#fps'), s.fps);
  setRadio($('#demoMode'), s.demo_devices);
  $('#startMin').checked = s.start_minimized;
  $('#closeTray').checked = s.close_to_tray;
  $('#autostart').checked = state.app.launch_at_login;
  $('#autoUpdate').checked = s.check_for_updates;
  $('#builtinEngine').checked = s.builtin_engine;
  renderHardware();
  renderUpdateStatus();
  $('#about').textContent = `Lumora ${state.app.version}${api.inApp ? '' : ' (browser preview with demo devices)'}`;
  renderConnection();
}

function openSettings(focus) {
  renderSettings();
  $('#settings').showModal();
  if (focus) $(focus).focus();
}

$('#settingsBtn').addEventListener('click', () => openSettings());
$('#themeGrid').addEventListener('click', (ev) => {
  const b = ev.target.closest('[data-theme-id]');
  if (b) saveSettings({ theme: b.dataset.themeId });
});
$('#accentRow').addEventListener('click', (ev) => {
  const dot = ev.target.closest('[data-accent]');
  if (dot) return saveSettings({ accent: dot.dataset.accent, accent_follows_lighting: false });
  const custom = ev.target.closest('#accentCustom');
  if (custom) {
    openPicker(custom, state.app.settings.accent, (hex) => {
      state.app.settings.accent = hex;
      state.app.settings.accent_follows_lighting = false;
      document.documentElement.style.setProperty('--accent', hex);
    });
    const done = () => {
      if (!document.querySelector('.picker')) {
        saveSettings({});
        document.removeEventListener('pointerup', done);
      }
    };
    setTimeout(() => document.addEventListener('pointerup', done), 0);
  }
});
$('#accentFollow').addEventListener('change', (ev) => {
  followed = null;
  saveSettings({ accent_follows_lighting: ev.target.checked });
});
$('#fps').addEventListener('click', (ev) => {
  const b = ev.target.closest('[data-v]');
  if (b) saveSettings({ fps: +b.dataset.v });
});
$('#demoMode').addEventListener('click', (ev) => {
  const b = ev.target.closest('[data-v]');
  if (b) saveSettings({ demo_devices: b.dataset.v });
});
$('#host').addEventListener('change', (ev) => saveSettings({ host: ev.target.value.trim() || '127.0.0.1' }));
$('#port').addEventListener('change', (ev) => {
  const port = parseInt(ev.target.value, 10);
  if (port > 0 && port < 65536) saveSettings({ port });
  else ev.target.value = state.app.settings.port;
});
$('#reconnectBtn').addEventListener('click', () => api.invoke('reconnect'));
async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const t = document.createElement('textarea');
    t.value = text;
    t.style.cssText = 'position:fixed;opacity:0';
    document.body.appendChild(t);
    t.select();
    const ok = document.execCommand('copy');
    t.remove();
    return ok;
  }
}

$('#reportBtn').addEventListener('click', async () => {
  const btn = $('#reportBtn');
  btn.disabled = true;
  btn.textContent = 'Gathering…';
  try {
    const report = await api.invoke('diagnostic_report');
    toast((await copyText(report)) ? 'Report copied. Paste it into a message.' : 'Couldn\u2019t reach the clipboard');
  } catch (err) {
    toast(String(err));
  }
  btn.disabled = false;
  btn.textContent = 'Copy diagnostic report';
});

// "Copy the report" buttons elsewhere (cooler pages) do the same.
document.addEventListener('click', (e) => {
  if (e.target.closest('[data-copy-report]')) $('#reportBtn').click();
});

$('#rescanBtn').addEventListener('click', () => {
  api.invoke('rescan');
  toast('Looking for devices again. This takes a few seconds.');
});
$('#builtinEngine').addEventListener('change', (ev) => saveSettings({ builtin_engine: ev.target.checked }));
$('#startMin').addEventListener('change', (ev) => saveSettings({ start_minimized: ev.target.checked }));
$('#closeTray').addEventListener('change', (ev) => saveSettings({ close_to_tray: ev.target.checked }));
$('#autostart').addEventListener('change', async (ev) => {
  try {
    state.app.launch_at_login = await api.invoke('set_launch_at_login', { on: ev.target.checked });
  } catch (err) {
    toast(String(err));
  }
  ev.target.checked = state.app.launch_at_login;
});
$('#getOpenRgb').addEventListener('click', () => api.invoke('open_external', { url: 'https://openrgb.org/' }));
$('#settings').addEventListener('close', closePicker);
$('#settings').addEventListener('click', (ev) => {
  if (ev.target === $('#settings')) $('#settings').close();
});

// --- Updates ------------------------------------------------------------------

let update = null;

function setUpdate(info) {
  update = info;
  $('#updatePill').hidden = !info;
  renderUpdateStatus();
}

function renderUpdateStatus(text) {
  const v = state.app.version;
  $('#updateStatus').textContent = text || (update ? `Lumora ${update.version} is available. You're on ${v}.` : `You're on Lumora ${v}.`);
  $('#checkUpdateBtn').textContent = update ? 'Install update' : 'Check for updates';
}

function formatMB(bytes) {
  return (bytes / 1048576).toFixed(1) + ' MB';
}

function openUpdate() {
  if (!update) return;
  $('#updateVersions').textContent = `Version ${update.version}${update.date ? `, released ${update.date}` : ''}. You have ${update.current_version}.`;
  $('#updateNotes').textContent = update.notes || '';
  $('#updateProgress').hidden = true;
  $('#updateProgressText').hidden = true;
  $('#updateInstall').disabled = false;
  $('#updateLater').disabled = false;
  if ($('#settings').open) $('#settings').close();
  $('#updateDialog').showModal();
}

$('#updatePill').addEventListener('click', openUpdate);
$('#updateLater').addEventListener('click', () => $('#updateDialog').close());

$('#checkUpdateBtn').addEventListener('click', async () => {
  if (update) return openUpdate();
  const btn = $('#checkUpdateBtn');
  btn.disabled = true;
  renderUpdateStatus('Checking for updates…');
  try {
    const info = await api.invoke('check_update');
    setUpdate(info);
    if (info) openUpdate();
    else renderUpdateStatus(`You're up to date on Lumora ${state.app.version}.`);
  } catch (err) {
    renderUpdateStatus(String(err));
  }
  btn.disabled = false;
});

$('#updateInstall').addEventListener('click', async () => {
  $('#updateInstall').disabled = true;
  $('#updateLater').disabled = true;
  const bar = $('#updateProgress');
  bar.hidden = false;
  bar.classList.add('indeterminate');
  const text = $('#updateProgressText');
  text.hidden = false;
  text.textContent = 'Starting download…';
  try {
    await api.invoke('install_update');
    // The app restarts on its own once the new version is in place.
    bar.classList.remove('indeterminate');
    bar.style.setProperty('--p', '100%');
    text.textContent = 'Installed. Restarting Lumora…';
  } catch (err) {
    bar.hidden = true;
    text.textContent = String(err);
    $('#updateLater').disabled = false;
    // The failed download is spent; look again so "Try again" has something to install.
    try {
      const info = await api.invoke('check_update');
      setUpdate(info);
      $('#updateInstall').disabled = !info;
      $('#updateInstall').textContent = 'Try again';
    } catch {
      /* keep the error message showing */
    }
  }
});

function onUpdateProgress({ downloaded, total }) {
  const bar = $('#updateProgress');
  const text = $('#updateProgressText');
  if (total) {
    bar.classList.remove('indeterminate');
    const pct = Math.min(100, (downloaded / total) * 100);
    bar.style.setProperty('--p', pct + '%');
    text.textContent = pct >= 100 ? 'Installing… Lumora restarts in a moment.' : `Downloading ${formatMB(downloaded)} of ${formatMB(total)}`;
  } else {
    text.textContent = `Downloading ${formatMB(downloaded)}`;
  }
}

$('#autoUpdate').addEventListener('change', (ev) => saveSettings({ check_for_updates: ev.target.checked }));

// --- Toast --------------------------------------------------------------------

let toastTimer;
function toast(text) {
  const t = $('#toast');
  t.textContent = text;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), 2200);
}

// --- Window controls ------------------------------------------------------------

$('#winMin').addEventListener('click', () => api.window.minimize());
$('#winMax').addEventListener('click', () => api.window.toggleMaximize());
$('#winClose').addEventListener('click', () => api.window.close());

// --- Backend events --------------------------------------------------------------

function renderAll() {
  document.body.classList.toggle('device-hands-off', !!selectedDevice() && !selectedDevice().managed);
  renderSidebar();
  renderStage();
  renderEffects();
  renderInspector();
  renderPower();
  renderConnection();
}

function deviceSignature(list) {
  return list.map((d) => d.id + ':' + d.led_count).join('|');
}

function onSnapshot(next) {
  const devicesChanged = deviceSignature(next.devices) !== deviceSignature(snap().devices);
  // Ignore echoes of edits still in flight from this window.
  if (pending) {
    next.global = snap().global;
    next.overrides = snap().overrides;
  }
  state.app.snapshot = next;
  // Coolers aren't lighting devices, so they're never in this list.
  if (state.selected && !cooling.coolerId(state.selected) && !next.devices.some((d) => d.id === state.selected)) state.selected = '';
  if (devicesChanged) {
    restoreSelection();
    renderAll();
  } else {
    renderConnection();
    renderPower();
    renderStageHead();
    if (!document.querySelector('.picker') && document.activeElement?.type !== 'range') {
      renderInspector();
      renderEffects();
    }
  }
}

async function refreshSnapshot() {
  onSnapshot(await api.invoke('get_snapshot'));
  renderAll();
}

let glowTimer = 0;
function onFrames(frames) {
  for (const [id, b64] of Object.entries(frames)) {
    const colors = decodeFrame(b64);
    state.lastColors.set(id, colors);
    state.views.get(id)?.update(colors);
  }
  const now = performance.now();
  updateAmbient(now);
  if (now - glowTimer > 150) {
    glowTimer = now;
    paintGlows();
  }
}

async function init() {
  if (!api.inApp) document.body.classList.add('browser');
  state.app = await api.invoke('get_state');
  cooling.initCooling({
    api, state, toast, esc, icon,
    rerender: () => { renderSidebar(); renderStage(); },
    // Lighting on other devices, for the cooler page's fan lighting card.
    lighting: {
      devices: () => devices(),
      labels: () => labels(),
      effects: () => state.app.effects,
      configOf: (id) => snap().overrides[id] || snap().global,
      setEffect: (id, cfg) => {
        snap().overrides[id] = cfg;
        api.invoke('set_effect', { device: id, config: cfg });
        markProfileModified();
        renderSidebar();
      },
      colorsFor: (effect, from) => colorsFor(effect, from),
      setControl: (d, on) => setControl(d, on),
      open: (id) => select(id),
    },
  });
  applyTheme();
  try { state.restore = localStorage.getItem('lumora.selected') || ''; } catch { state.restore = ''; }
  restoreSelection();
  renderAll();
  renderProfileName();
  requestAnimationFrame(animateTiles);

  await api.listen('lumora://changed', (e) => onSnapshot(e.payload));
  await api.listen('lumora://frames', (e) => onFrames(e.payload));
  await api.listen('lumora://engine', (e) => {
    state.app.engine = e.payload;
    renderConnection();
    renderHardware();
    renderStageHead();
  });
  await api.listen('lumora://coolers', (e) => {
    const prev = state.app.coolers || [];
    const shape = (list) => list.map((c) => `${c.id}|${c.state}|${c.takeover}|${c.owner_running}|${c.message}|${c.firmware}`).join(',');
    if (cooling.isBusy()) {
      // Keep local edits; only take the live readings.
      for (const c of e.payload) {
        const mine = prev.find((p) => p.id === c.id);
        if (mine) mine.status = c.status;
      }
    } else {
      state.app.coolers = e.payload;
    }
    if (restoreSelection()) return;
    if (shape(prev) !== shape(state.app.coolers) || prev.length !== e.payload.length) {
      renderSidebar();
      if (cooling.selectedCooler() || cooling.coolerId(state.selected)) renderStage();
    } else {
      cooling.refreshSidebarTemps();
      const c = cooling.selectedCooler();
      if (c) cooling.refreshLive(c);
    }
  });
  await api.listen('lumora://temps', (e) => {
    state.app.temps = e.payload;
    renderTempNow();
  });
  await api.listen('lumora://update-progress', (e) => onUpdateProgress(e.payload));
  await api.listen('lumora://update-available', (e) => {
    const fresh = !update;
    setUpdate(e.payload);
    if (fresh) toast(`Lumora ${e.payload.version} is available`);
  });
  const preview = () => api.invoke('set_preview', { on: !document.hidden });
  document.addEventListener('visibilitychange', preview);
  window.addEventListener('focus', preview);
  preview();
  window.addEventListener('resize', () => closePicker());
}

init().catch((err) => {
  document.body.innerHTML = `<div class="empty"><strong>Lumora couldn't start</strong><span>${esc(err?.message || err)}</span></div>`;
});

// Exposed for quick inspection from devtools.
window.lumora = { state, hexToRgb };
