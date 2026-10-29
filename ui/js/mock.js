// Stand-in backend for `npm run preview`, so the UI can be worked on in a plain
// browser. Inside the app, bridge.js talks to the real Rust engine instead.

import { build, ledPositions } from './effects.js';
import { localTable } from './cooling.js';

const fx = (id, name, description, category, [min, max], speed, size, direction, colors) => ({
  id, name, description, category, min_colors: min, max_colors: max,
  uses_speed: speed, uses_size: !!size, uses_direction: direction, size_label: size || '', default_colors: colors,
});

const EFFECTS = [
  fx('static', 'Static', 'One solid color', 'Solid', [1, 1], false, null, false, ['#7c5cff']),
  fx('gradient', 'Gradient', 'A still blend across your LEDs', 'Solid', [2, 6], false, null, true, ['#ff3d7f', '#7c5cff', '#00d4ff']),
  fx('breathing', 'Breathing', 'Slow fade in and out, cycling colors', 'Calm', [1, 6], true, null, false, ['#00d4ff', '#7c5cff']),
  fx('spectrum', 'Spectrum', 'Everything drifts through the rainbow', 'Calm', [0, 0], true, null, false, []),
  fx('aurora', 'Aurora', 'Soft, shifting northern lights', 'Calm', [2, 6], true, 'Scale', false, ['#00ffa3', '#00b3ff', '#8a5cff', '#ff4fd8']),
  fx('rainbow_wave', 'Rainbow', 'The classic rolling rainbow', 'Wave', [0, 0], true, 'Wavelength', true, []),
  fx('color_wave', 'Color Wave', 'Your colors rolling across', 'Wave', [2, 6], true, 'Wavelength', true, ['#7c5cff', '#00d4ff', '#ff3da8']),
  fx('ripple', 'Ripple', 'Rings pulsing out from the center', 'Wave', [1, 6], true, 'Ring width', true, ['#00d4ff', '#ff3da8']),
  fx('comet', 'Comet', 'A bright head with a fading tail', 'Motion', [1, 2], true, 'Tail length', true, ['#ffffff', '#2a0f5c']),
  fx('marquee', 'Marquee', 'Theater-style chasing dashes', 'Motion', [1, 4], true, 'Dash size', true, ['#ffb000', '#ff3d7f']),
  fx('twinkle', 'Starfield', 'Random sparkles fading in and out', 'Motion', [1, 6], true, 'Density', false, ['#ffffff', '#9ad7ff', '#c7a6ff']),
  fx('fire', 'Fire', 'Flickering flames', 'Motion', [3, 3], true, 'Flame height', true, ['#ff1e00', '#ff8a00', '#ffe066']),
  { ...fx('temperature', 'Temperature', 'Colors follow your liquid or GPU temperature', 'Live', [2, 6], false, null, false, ['#00a2ff', '#00ffa3', '#ffd400', '#ff2d2d']), uses_temperature: true },
];

// --- A pretend Kraken and Corsair H115i Pro for the cooling page ------------------------------------------
const PRESET_POINTS = {
  pump: { silent: [[20, 50], [35, 60], [45, 80], [50, 100]], balanced: [[20, 60], [35, 70], [45, 90], [50, 100]], performance: [[20, 80], [30, 90], [40, 100]] },
  fan: { silent: [[20, 25], [30, 30], [36, 45], [42, 70], [48, 100]], balanced: [[20, 30], [30, 40], [36, 60], [42, 85], [48, 100]], performance: [[20, 50], [30, 65], [36, 85], [40, 100]] },
};
function tableFor(ch, setting) {
  const pts = setting.mode === 'preset' ? PRESET_POINTS[ch][setting.preset]
    : setting.mode === 'fixed' ? [[20, setting.duty], [58, setting.duty]] : setting.points;
  return localTable(pts, ch === 'pump' ? 20 : 0);
}
const kraken = {
  id: 'hid:1e71:300c:DEMO', name: 'Kraken Elite (2023)', vendor: 'NZXT', owner_app: 'NZXT CAM', owner_running: false,
  takeover: location.search.includes('cooler'), state: 'idle', message: '', firmware: '2.4.0', status: null,
  channels: [
    { id: 'pump', name: 'Pump', min_duty: 20, kind: 'curve', setting: { mode: 'preset', preset: 'balanced' } },
    { id: 'fan', name: 'Radiator fans', min_duty: 0, kind: 'curve', setting: { mode: 'preset', preset: 'balanced' } },
  ],
  screen: { width: 640, height: 640, settings: { mode: 'liquid', brightness: 80, rotation: 0 } },
  light: null, needs_setup: false,
};
// `?corsair-setup` shows the one-time setup step.
const hydro = {
  id: 'usb:1b1c:0c13:0', name: 'H115i Pro', vendor: 'Corsair', owner_app: 'Corsair iCUE', owner_running: false,
  takeover: location.search.includes('corsair'), state: 'idle', message: '', firmware: '1.1.15.4', status: null,
  channels: [
    { id: 'pump', name: 'Pump', min_duty: 0, kind: 'modes', setting: { mode: 'preset', preset: 'balanced' } },
    { id: 'fan', name: 'Radiator fans', min_duty: 0, kind: 'curve', setting: { mode: 'preset', preset: 'balanced' } },
  ],
  screen: null,
  light: { mode: 'static', colors: ['#7c5cff', '#00d2ff'], speed: 'normal' },
  needs_setup: location.search.includes('corsair-setup'),
};
[kraken, hydro].forEach((k) => k.channels.forEach((c) => (c.table = tableFor(c.id, c.setting))));
let liquid = 33.2;

function linear(zones) {
  let start = 0;
  return zones.map(([name, count]) => {
    const z = { name, kind: count === 1 ? 'single' : 'linear', start, count };
    start += count;
    return z;
  });
}

function keyboard() {
  const rows = ['1111111111111111', '1111111111111011', '1111111111111111', '1111111111110101', '1111111111110111', '1110001001111111'];
  const map = [];
  let n = 0;
  for (const r of rows) for (const c of r) map.push(c === '1' ? n++ : null);
  return [{ name: 'Keyboard', kind: 'matrix', start: 0, count: n, matrix: { width: 16, height: 6, map } }];
}

const dev = (id, name, kind, zones) => ({
  id: 'demo:' + id, name, vendor: 'Lumora', kind, location: 'Demo device', zones,
  led_count: zones.reduce((a, z) => a + z.count, 0), controllable: true, virtual_device: true, managed: true,
});

const DEVICES = [
  dev('kbd', 'Demo Keyboard 75%', 'keyboard', keyboard()),
  dev('mouse', 'Demo Mouse', 'mouse', linear([['Logo', 1], ['Scroll Wheel', 1], ['Underglow', 10]])),
  dev('ram1', 'Demo DDR5 Stick 1', 'ram', linear([['DRAM', 12]])),
  dev('ram2', 'Demo DDR5 Stick 2', 'ram', linear([['DRAM', 12]])),
  dev('gpu', 'Demo Graphics Card', 'gpu', linear([['Light Bar', 18]])),
  dev('aio', 'Demo AIO Cooler', 'cooler', linear([['Pump', 16], ['Fan 1', 12], ['Fan 2', 12], ['Fan 3', 12]])),
  dev('strip', 'Demo Case Strip', 'led_strip', linear([['Strip', 40]])),
];

const preset = (name, effect, colors, speed, size, direction) => ({
  name, global: { effect, colors, speed, size, direction }, overrides: {}, brightness: 100,
});

const state = {
  snapshot: {
    connection: { status: 'offline', server: '127.0.0.1:6742', message: "OpenRGB isn't running, or its SDK server is off", protocol: 0 },
    devices: DEVICES,
    global: { effect: 'color_wave', colors: ['#7c5cff', '#00d4ff', '#ff3da8'], speed: 40, size: 50, direction: 'right' },
    overrides: {},
    brightness: 100,
    enabled: true,
  },
  effects: EFFECTS,
  settings: {
    host: '127.0.0.1', port: 6742, fps: 30, theme: 'dark', accent: '#7c5cff', accent_follows_lighting: false,
    demo_devices: 'auto', start_minimized: false, close_to_tray: true, check_for_updates: true, builtin_engine: true,
  },
  profiles: [
    preset('Neon Drift', 'color_wave', ['#7c5cff', '#00d4ff', '#ff3da8'], 40, 50, 'right'),
    preset('Arctic', 'aurora', ['#e8fbff', '#00d4ff', '#3d6bff'], 25, 60, 'right'),
    preset('Ember', 'fire', ['#ff1e00', '#ff8a00', '#ffe066'], 45, 55, 'up'),
    preset('Focus', 'static', ['#ffd7a8'], 0, 50, 'right'),
    preset('Rainbow', 'rainbow_wave', [], 45, 50, 'right'),
  ],
  active_profile: null,
  coolers: [kraken, hydro],
  temps: [],
  launch_at_login: false,
  engine: { mode: 'standard', bundled: true, full_access: 'off', version: '1.1-8e16de2', problem: null },
  version: '0.1.0-preview',
};

const listeners = {};
const emit = (ev, payload) => (listeners[ev] || []).forEach((cb) => cb({ payload }));
const changed = () => emit('lumora://changed', structuredClone(state.snapshot));
const clone = (v) => structuredClone(v);

// --- Render loop: same idea as the Rust engine ------------------------------
const runtimes = DEVICES.map((d) => ({ dev: d, leds: ledPositions(d), cfg: null, render: null, out: [] }));
let preview = false;
let fade = 1;
const start = performance.now();
let last = start;
setInterval(() => {
  const now = performance.now();
  const t = (now - start) / 1000, dt = Math.min((now - last) / 1000, 0.25);
  last = now;
  const s = state.snapshot;
  const target = s.enabled ? 1 : 0;
  fade = fade < target ? Math.min(fade + dt * 3, 1) : Math.max(fade - dt * 3, 0);
  const level = (s.brightness / 100) ** 2 * fade;
  const frames = {};
  for (const r of runtimes) {
    const cfg = s.overrides[r.dev.id] || s.global;
    const key = JSON.stringify(cfg);
    if (r.cfg !== key) {
      r.cfg = key;
      r.render = build(cfg, EFFECTS.find((e) => e.id === cfg.effect), r.dev.led_count);
      r.out = new Array(r.dev.led_count).fill([0, 0, 0]);
    }
    r.render(t, dt, r.leds, r.out, mockTemp(cfg.temp_source || 'liquid'));
    if (!preview) continue;
    let bin = '';
    for (const c of r.out) bin += String.fromCharCode(...c.map((v) => Math.round(v * level)));
    frames[r.dev.id] = btoa(bin);
  }
  if (preview) emit('lumora://frames', frames);
}, 1000 / 30);

function mockTemp(source) {
  return state.temps.find((x) => x.source === source)?.celsius ?? null;
}

setInterval(() => {
  liquid += (Math.random() - 0.45) * 0.4;
  liquid = Math.max(29, Math.min(44, liquid));
  const k = state.coolers[0];
  k.state = k.takeover ? 'active' : 'idle';
  const at = (ch) => ch.table[Math.max(0, Math.min(39, Math.round(liquid) - 20))];
  if (k.takeover) {
    const [pump, fan] = k.channels;
    k.status = { liquid_c: Math.round(liquid * 10) / 10, pump_duty: at(pump), pump_rpm: 1000 + at(pump) * 18, fan_duty: at(fan), fan_rpm: at(fan) * 18, fault: false };
  } else {
    k.status = null;
  }
  const h = state.coolers[1];
  h.state = !h.takeover ? 'idle' : h.needs_setup ? 'needs_setup' : 'active';
  if (h.state === 'active') {
    const modes = { silent: 'quiet', balanced: 'balanced', performance: 'performance' };
    const fan = at(h.channels[1]) * 20;
    h.status = { liquid_c: Math.round((liquid - 1.6) * 10) / 10, pump_rpm: 2350, pump_duty: null, pump_mode: modes[h.channels[0].setting.preset] || 'balanced', fan_rpm: fan + 10, fan_duty: null, fan_rpms: [fan, fan + 20], fault: false };
  } else {
    h.status = null;
  }
  state.temps = [
    ...(k.takeover ? [{ source: 'liquid', celsius: k.status.liquid_c }] : []),
    { source: 'gpu', celsius: 54 + Math.round(Math.sin(performance.now() / 9000) * 80) / 10 },
  ];
  emit('lumora://coolers', structuredClone(state.coolers));
  emit('lumora://temps', structuredClone(state.temps));
}, 1000);

const commands = {
  get_state: () => clone({ ...state, snapshot: state.snapshot }),
  get_snapshot: () => clone(state.snapshot),
  set_effect: ({ device, config }) => {
    if (device) state.snapshot.overrides[device] = config;
    else state.snapshot.global = config;
    changed();
  },
  clear_override: ({ device }) => { delete state.snapshot.overrides[device]; changed(); },
  sync_all: ({ config }) => { state.snapshot.global = config; state.snapshot.overrides = {}; changed(); },
  set_brightness: ({ value }) => { state.snapshot.brightness = value; changed(); },
  set_enabled: ({ on }) => { state.snapshot.enabled = on; changed(); },
  set_preview: ({ on }) => { preview = on; },
  rescan: () => {},
  set_cooler_config: ({ id, config }) => {
    const k = state.coolers.find((c) => c.id === id);
    k.takeover = config.takeover;
    for (const ch of k.channels) {
      if (config.speeds[ch.id]) ch.setting = config.speeds[ch.id];
      ch.table = tableFor(ch.id, ch.setting);
    }
    if (config.screen && k.screen) k.screen.settings = config.screen;
    if (config.light && k.light) k.light = config.light;
    return structuredClone(state.coolers);
  },
  setup_cooler_access: async () => {
    await new Promise((r) => setTimeout(r, 900));
    state.coolers.forEach((c) => (c.needs_setup = false));
  },
  diagnostic_report: () => 'Lumora preview\n(no hardware in the browser preview)',
  set_device_control: ({ device, on }) => {
    state.settings.device_control = { ...(state.settings.device_control || {}), [device]: on };
    const d = state.snapshot.devices.find((x) => x.id === device);
    if (d) d.managed = on;
    changed();
    return structuredClone(state.settings);
  },
  resize_zone: () => {},
  save_profile: ({ name }) => {
    const p = { name, global: clone(state.snapshot.global), overrides: clone(state.snapshot.overrides), brightness: state.snapshot.brightness };
    const i = state.profiles.findIndex((x) => x.name.toLowerCase() === name.toLowerCase());
    if (i >= 0) state.profiles[i] = p; else state.profiles.push(p);
    state.active_profile = name;
    return clone(state.profiles);
  },
  load_profile: ({ name }) => {
    const p = state.profiles.find((x) => x.name === name);
    Object.assign(state.snapshot, { global: clone(p.global), overrides: clone(p.overrides), brightness: p.brightness, enabled: true });
    state.active_profile = name;
    changed();
  },
  delete_profile: ({ name }) => {
    state.profiles = state.profiles.filter((p) => p.name !== name);
    if (state.active_profile === name) state.active_profile = null;
    return clone(state.profiles);
  },
  update_settings: ({ settings }) => { state.settings = settings; return clone(settings); },
  reconnect: () => {
    state.snapshot.connection.status = 'connecting';
    changed();
    setTimeout(() => { state.snapshot.connection.status = 'offline'; changed(); }, 900);
  },
  set_launch_at_login: ({ on }) => (state.launch_at_login = on),
  enable_full_access: () => new Promise((resolve) => setTimeout(() => resolve(Object.assign(state.engine, { mode: 'full', full_access: 'on' })), 1200)),
  disable_full_access: () => new Promise((resolve) => setTimeout(() => resolve(Object.assign(state.engine, { mode: 'standard', full_access: 'off' })), 800)),
  open_external: ({ url }) => { window.open(url, '_blank'); },
  // Add ?update to the preview URL to try the update flow.
  check_update: () => new Promise((resolve) => setTimeout(() => resolve(location.search.includes('update') ? {
    version: '0.2.0', current_version: state.version, date: '2026-11-12',
    notes: 'New Audio Visualizer effect\nFaster device detection on startup\nFixed RAM sticks occasionally showing the wrong color',
  } : null), 700)),
  install_update: () => new Promise((resolve) => {
    const total = 9.4 * 1048576;
    let downloaded = 0;
    const tick = setInterval(() => {
      downloaded = Math.min(total, downloaded + total / 25);
      emit('lumora://update-progress', { downloaded, total });
      if (downloaded >= total) { clearInterval(tick); setTimeout(resolve, 600); }
    }, 80);
  }),
};

export const mock = {
  invoke: async (cmd, args = {}) => {
    if (!commands[cmd]) throw new Error('Unknown command ' + cmd);
    return commands[cmd](args);
  },
  listen: async (ev, cb) => {
    (listeners[ev] ||= []).push(cb);
    return () => (listeners[ev] = listeners[ev].filter((x) => x !== cb));
  },
  window: {
    minimize() {}, toggleMaximize() {}, close() {}, startDragging() {},
  },
};
