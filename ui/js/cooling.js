// The cooling page: live readings, pump and fan curves, the pump screen, and
// handing a cooler over from its vendor app (NZXT CAM, iCUE) to Lumora.

const PRESETS = [
  ['silent', 'Silent'],
  ['balanced', 'Balanced'],
  ['performance', 'Performance'],
];
const T0 = 20, T1 = 59, TMAX = 60;
const LIGHT_MODES = [['static', 'Static'], ['shift', 'Color shift'], ['pulse', 'Pulse'], ['blink', 'Blink'], ['sync', 'Match Lumora']];
const SCREEN_MODES = [['temperature', 'Temperature'], ['liquid', 'NZXT built-in'], ['color', 'Colors'], ['image', 'Picture'], ['unchanged', 'Leave as is']];
const PALETTE = ['#7c5cff', '#00d2ff', '#ff3d8b', '#ffd400', '#3ddc97'];
// Quick picks on the fan lighting card; everything else is one click away.
const FAN_EFFECTS = ['static', 'breathing', 'spectrum', 'color_wave', 'rainbow_wave', 'temperature'];
const LIGHT_SPEEDS = [['slower', 'Slow'], ['normal', 'Normal'], ['faster', 'Fast']];
const PUMP_MODE_NAMES = { quiet: 'Silent mode', balanced: 'Balanced mode', performance: 'Performance mode' };

let ctx = null;
let pendingSave = null;
// Small previews of chosen pictures, by cooler (kept for this session, and in
// browser storage when it's available).
const pictures = new Map();
const loadPreview = (id) => {
  if (pictures.has(id)) return pictures.get(id);
  try {
    const p = localStorage.getItem('lumora.screen.' + id);
    if (p) pictures.set(id, p);
    return p;
  } catch {
    return null;
  }
};

// CPU temperature comes from a small helper that needs a one-time setup.
let cpu = { state: 'needs_setup', message: '' };
const READINGS = [['liquid', 'Liquid'], ['cpu', 'CPU'], ['gpu', 'GPU']];

/** One line on why there's no CPU reading. */
export function cpuHint() {
  if (cpu.state === 'error') return `CPU temperature isn't working: ${cpu.message}.`;
  if (cpu.state === 'unsupported') return cpu.message || 'CPU temperature isn\'t available on this system.';
  return 'CPU temperature needs a one-time setup (one Windows permission prompt).';
}

async function refreshCpu() {
  try {
    const next = await ctx.api.invoke('cpu_sensor');
    const changed = next.state !== cpu.state || next.message !== cpu.message;
    cpu = next;
    if (changed && selectedCooler()) render();
  } catch { /* preview without a backend */ }
}

async function setupCpu(btn) {
  if (btn) {
    btn.disabled = true;
    btn.textContent = 'Setting up…';
  }
  try {
    await ctx.api.invoke('setup_cpu_sensor');
    ctx.toast('CPU temperature is on. The first reading arrives in a few seconds.');
  } catch (err) {
    ctx.toast(`Couldn't turn on CPU temperature: ${err}`);
  }
  await refreshCpu();
  render();
}

export function initCooling(context) {
  ctx = context;
  refreshCpu();
  setInterval(refreshCpu, 4000);
  document.addEventListener('click', (ev) => {
    const b = ev.target.closest('[data-cpu-setup]');
    if (b) setupCpu(b);
  });
  document.addEventListener('click', onClick);
  document.addEventListener('input', onInput);
  document.addEventListener('change', onChange);
  $('#coolerDialogCancel').addEventListener('click', () => $('#coolerDialog').close());
}

const coolers = () => ctx.state.app.coolers || [];
export const coolerId = (sel) => (sel && sel.startsWith('cooler:') ? sel.slice(7) : null);
export const selectedCooler = () => coolers().find((c) => c.id === coolerId(ctx.state.selected)) || null;
const fmt = (v, digits = 0) => (v == null ? '–' : Number(v).toFixed(digits));

// --- Sidebar -----------------------------------------------------------------

export function sidebarHtml() {
  const list = coolers();
  if (!list.length) return '';
  return '<div class="group-title">Cooler control</div>' + list.map((c) => {
    const live = c.state === 'active' && c.status?.liquid_c != null;
    const badge = live
      ? `<span class="nav-temp" data-cooler-temp="${ctx.esc(c.id)}">${fmt(c.status.liquid_c, 1)}°</span>`
      : c.takeover && c.needs_restart
        ? '<span class="nav-badge muted">Restart</span>'
        : c.takeover && c.needs_setup
        ? '<span class="nav-badge muted">Setup</span>'
        : c.takeover
        ? '<span class="nav-badge muted">…</span>'
        : `<span class="nav-badge muted">${ctx.esc((c.owner_app || 'Off').replace(/^(NZXT|Corsair) /, ''))}</span>`;
    return `<button class="nav-item" data-device="cooler:${ctx.esc(c.id)}" aria-current="${ctx.state.selected === 'cooler:' + c.id}" title="${ctx.esc(c.vendor + ' ' + c.name)}">
      <span class="nav-icon">${ctx.icon('cooler')}</span>
      <span class="nav-name">${ctx.esc(c.name.replace(/ \(\d{4}\)$/, ''))}</span>${badge}
    </button>`;
  }).join('');
}

// --- Saving --------------------------------------------------------------------

function configOf(c) {
  return {
    takeover: c.takeover,
    speeds: Object.fromEntries(c.channels.map((ch) => [ch.id, ch.setting])),
    screen: c.screen ? c.screen.settings : undefined,
    light: c.light || undefined,
  };
}

/** Applies a change locally right away and saves it shortly after. */
function update(c, mutate, { now = false, wait = 350 } = {}) {
  mutate(c);
  const send = async () => {
    pendingSave = null;
    try {
      ctx.state.app.coolers = await ctx.api.invoke('set_cooler_config', { id: c.id, config: configOf(c) });
    } catch (err) {
      ctx.toast(String(err));
    }
    render();
  };
  clearTimeout(pendingSave);
  if (now) send();
  else pendingSave = setTimeout(send, wait);
}

// --- Header & takeover ------------------------------------------------------------

export function renderHead(c) {
  $('#stageTitle').textContent = c.name;
  const bits = [c.vendor, c.firmware ? `firmware ${c.firmware}` : '', c.screen ? `${c.screen.width}×${c.screen.height} screen` : ''].filter(Boolean);
  $('#stageSub').textContent = bits.join(', ');
  $('#stageActions').innerHTML = `<label class="chip control-chip" title="Choose who runs this cooler's pump, fans and screen">
    <span>Run by Lumora</span><input type="checkbox" class="switch small" id="takeoverSwitch" ${c.takeover ? 'checked' : ''}>
  </label>`;
}

/** What Lumora takes over on this cooler, in words. */
const parts = (c) => (c.screen ? 'pump, fans and screen' : c.light ? 'pump, fans and lighting' : 'pump and fans');

function askTakeover(c) {
  const app = c.owner_app || 'its own app';
  const go = $('#coolerDialogGo');
  $('#coolerDialogTitle').textContent = c.needs_setup ? `Set up ${c.vendor} ${c.name}?` : `Let Lumora run ${c.name}?`;
  const running = `${app} is running right now. Quit it from the system tray first (and turn off its "start with Windows" option), or the two apps will keep overriding each other.`;
  if (c.needs_setup) {
    $('#coolerDialogText').textContent = `Windows needs a small access driver (UsbDk) before Lumora can reach this cooler. It's installed once, after one Windows permission prompt, and it doesn't replace the cooler's own driver, so ${app} still works whenever Lumora isn't running the cooler.`;
    $('#coolerDialogKeep').textContent = c.owner_running ? running : `Then Lumora runs the ${parts(c)}. You can hand it back any time.`;
    go.textContent = 'Set up and run it';
  } else {
    $('#coolerDialogText').textContent = c.owner_running ? running : `Lumora will run the ${parts(c)}. ${app} isn't running, so nothing will get in the way.`;
    $('#coolerDialogKeep').textContent = 'Your cooler stores its fan curves, so it keeps following them even when Lumora is closed. You can hand it back any time.';
    go.textContent = 'Let Lumora run it';
  }
  go.disabled = false;
  $('#coolerDialog').showModal();
  go.onclick = async () => {
    if (c.needs_setup) {
      go.disabled = true;
      go.textContent = 'Setting up…';
      try {
        await ctx.api.invoke('setup_cooler_access');
      } catch (err) {
        go.disabled = false;
        go.textContent = 'Try again';
        // Keep the reason on screen (a toast is gone before anyone can read it).
        $('#coolerDialogKeep').textContent = `Setup didn't finish: ${String(err)}`;
        return;
      }
      ctx.toast('Access driver installed. If the cooler doesn\'t connect within a minute, restart your PC once.');
    }
    $('#coolerDialog').close();
    update(c, (x) => (x.takeover = true), { now: true });
  };
}

// --- Page -------------------------------------------------------------------------

function readouts(c) {
  const s = c.status || {};
  const stat = (key, label, value, unit, sub) => `<div class="readout" data-readout="${key}">
      <span class="readout-label">${label}</span>
      <span class="readout-value"><b>${value}</b><small>${unit}</small></span>
      <span class="readout-sub">${sub}</span>
    </div>`;
  const hasFan = c.channels.some((ch) => ch.id === 'fan');
  return `<div class="readouts">
    <div class="readout liquid" data-readout="liquid">
      <span class="readout-label">Liquid</span>
      <span class="readout-value"><b>${fmt(s.liquid_c, 1)}</b><small>°C</small></span>
      <span class="readout-sub">${s.fault ? 'The cooler reports a fault. Check its SATA power cable.' : 'Your CPU heats the liquid; the radiator cools it.'}</span>
    </div>
    ${stat('pump', 'Pump', fmt(s.pump_rpm), 'rpm', pumpSub(s))}
    ${hasFan ? stat('fan', 'Radiator fans', fmt(s.fan_rpm), 'rpm', fanSub(s)) : ''}
  </div>`;
}

function pumpSub(s) {
  if (s.pump_duty != null) return `${s.pump_duty}% duty`;
  return PUMP_MODE_NAMES[s.pump_mode] || '';
}

function fanSub(s) {
  if (s.fan_duty != null) return `${s.fan_duty}% duty`;
  if (s.fan_rpms && s.fan_rpms.length > 1) return s.fan_rpms.map((r, i) => `Fan ${i + 1}: ${r}`).join(' · ');
  return '';
}

function curvePath(table, w, h, pad) {
  const x = (t) => pad + ((t - T0) / (TMAX - T0)) * (w - pad * 2);
  const y = (d) => h - pad - (d / 100) * (h - pad * 2);
  const pts = table.map((d, i) => [x(T0 + i), y(d)]);
  pts.push([x(TMAX), y(100)]);
  const line = pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(' ');
  const area = `${line} L${x(TMAX).toFixed(1)},${(h - pad).toFixed(1)} L${x(T0).toFixed(1)},${(h - pad).toFixed(1)} Z`;
  return { line, area, x, y };
}

function seedPoints(ch) {
  // Five handles on the current curve, spread across the useful range.
  return [25, 32, 39, 46, 53].map((t) => [t, ch.table[t - T0]]);
}

function curveSvg(c, ch) {
  const W = 560, H = 220, P = 26;
  const custom = ch.setting.mode === 'custom';
  const table = ch.table;
  const { line, area, x, y } = curvePath(table, W, H, P);
  const grid = [];
  for (let t = 20; t <= 60; t += 10) grid.push(`<line x1="${x(t)}" y1="${P}" x2="${x(t)}" y2="${H - P}"/><text x="${x(t)}" y="${H - 8}" text-anchor="middle">${t}°</text>`);
  for (let d = 0; d <= 100; d += 25) grid.push(`<line x1="${P}" y1="${y(d)}" x2="${W - P}" y2="${y(d)}"/><text x="${P - 6}" y="${y(d) + 4}" text-anchor="end">${d}</text>`);
  const liquid = c.status?.liquid_c;
  const now = liquid != null && c.state === 'active'
    ? (() => {
      const t = Math.max(T0, Math.min(TMAX, liquid));
      const d = table[Math.max(0, Math.min(39, Math.round(t) - T0))];
      return `<g class="curve-now"><line x1="${x(t)}" y1="${P}" x2="${x(t)}" y2="${H - P}"/><circle cx="${x(t)}" cy="${y(d)}" r="5"/></g>`;
    })()
    : '';
  const handles = custom
    ? ch.setting.points.map(([t, d], i) => `<circle class="curve-handle" data-ch="${ch.id}" data-i="${i}" cx="${x(t)}" cy="${y(d)}" r="8" tabindex="0" aria-label="${t}°C, ${d}%"><title>${t}°C → ${d}%</title></circle>`).join('')
    : '';
  return `<svg class="curve${custom ? ' editable' : ''}" viewBox="0 0 ${W} ${H}" data-curve="${ch.id}" role="img" aria-label="${ch.name} speed by liquid temperature">
    <g class="curve-grid">${grid.join('')}</g>
    <path class="curve-area" d="${area}"/><path class="curve-line" d="${line}"/>
    ${now}${handles}
  </svg>`;
}

function channelHtml(c, ch) {
  const mode = ch.setting.mode;
  const active = mode === 'preset' ? ch.setting.preset : mode;
  if (ch.kind === 'modes') {
    const seg = PRESETS.map(([v, l]) => `<button role="radio" data-speed="${ch.id}" data-v="${v}" aria-checked="${active === v}">${l}</button>`).join('');
    return `<section class="cool-card">
      <header><h3>${ch.name}</h3><div class="segmented compact" role="radiogroup" aria-label="${ch.name} speed">${seg}</div></header>
      <p class="hint">This pump has three built-in speeds, and the cooler fine-tunes within each one by itself.</p>
    </section>`;
  }
  const seg = [...PRESETS, ['fixed', 'Fixed'], ['custom', 'Custom']]
    .map(([v, l]) => `<button role="radio" data-speed="${ch.id}" data-v="${v}" aria-checked="${active === v}">${l}</button>`).join('');
  const fixed = mode === 'fixed'
    ? `<div class="control fixed-row"><label class="control-label" for="fixed-${ch.id}"><span>Speed</span><output>${ch.setting.duty}%</output></label>
        <input type="range" id="fixed-${ch.id}" data-fixed="${ch.id}" min="${ch.min_duty}" max="100" value="${ch.setting.duty}" style="--p:${ch.setting.duty}%"></div>`
    : '';
  const hint = mode === 'custom'
    ? 'Drag the points. The cooler follows this curve on its own, even with Lumora closed.'
    : ch.id === 'pump' ? `Never below ${ch.min_duty}%, so the pump can't stall. 100% from 59 °C, always.` : 'Full speed from 59 °C, always.';
  return `<section class="cool-card">
    <header><h3>${ch.name}</h3><div class="segmented compact" role="radiogroup" aria-label="${ch.name} speed">${seg}</div></header>
    ${fixed}
    ${curveSvg(c, ch)}
    <p class="hint">${hint}</p>
  </section>`;
}

/** Color swatches with remove buttons and an add button. */
function swatchesHtml(colors, max, kind) {
  const list = colors.slice(0, max);
  const sw = list.map((col, i) => `<label class="light-swatch" style="--c:${ctx.esc(col)}" title="Color ${i + 1}">
      <input type="color" data-${kind}-color="${i}" value="${ctx.esc(col)}" aria-label="Color ${i + 1}">
      ${list.length > 1 ? `<button type="button" class="light-remove" data-${kind}-remove="${i}" aria-label="Remove color ${i + 1}">×</button>` : ''}
    </label>`).join('');
  const add = list.length < max ? `<button type="button" class="light-add" data-${kind}-add aria-label="Add a color">+</button>` : '';
  return `<div class="light-colors">${sw}${add}</div>`;
}

/** The readings Lumora draws, as the preview shows them. */
function readingsFace(c, s) {
  const list = (s.readings && s.readings.length ? s.readings : [s.source || 'liquid']).slice(0, 2);
  const value = (src) => {
    const t = src === 'liquid' ? c.status?.liquid_c : ctx.state.app.temps?.find((x) => x.source === src)?.celsius;
    return t == null ? '--' : Math.round(t);
  };
  const names = { liquid: 'LIQUID', cpu: 'CPU', gpu: 'GPU' };
  return `<div class="screen-face${list.length > 1 ? ' two' : ''}" style="color:${ctx.esc(s.text_color || '#fff')}">
    ${list.map((src) => `<div class="screen-reading"><span class="screen-num"><b>${value(src)}</b><sup>°C</sup></span><small>${names[src] || ''}</small></div>`).join('')}
  </div>`;
}

// The preview shows the screen as it looks on the cooler, so it isn't
// rotated: rotation only turns the picture to match how the pump is mounted.
function screenPreview(c) {
  const s = c.screen.settings;
  const blend = (cols, fallback) => {
    const list = (cols || []).slice(0, 3);
    return list.length > 1 ? `linear-gradient(135deg, ${list.join(', ')})` : list[0] || fallback;
  };
  const overlay = s.overlay ? readingsFace(c, s) : '';
  if (s.mode === 'temperature') {
    return `<div class="screen-preview" style="background:${ctx.esc(blend(s.background, '#000'))}">${readingsFace(c, s)}</div>`;
  }
  if (s.mode === 'color') return `<div class="screen-preview" style="background:${ctx.esc(blend(s.colors, '#7c5cff'))}">${overlay}</div>`;
  if (s.mode === 'image') {
    const p = loadPreview(c.id);
    return p
      ? `<div class="screen-preview"><img src="${ctx.esc(p)}" alt="">${overlay}</div>`
      : `<div class="screen-preview empty">${overlay || '<span>Your picture</span>'}</div>`;
  }
  if (s.mode === 'liquid') {
    return `<div class="screen-preview builtin"><div class="screen-face"><div class="screen-reading">
      <span class="screen-num"><b>${c.status?.liquid_c == null ? '--' : Math.round(c.status.liquid_c)}</b><sup>°C</sup></span>
    </div></div></div>`;
  }
  return '<div class="screen-preview empty"><span>Unchanged</span></div>';
}

/** Which readings to draw (pick up to two), and their color. */
function readingsHtml(s) {
  const list = s.readings && s.readings.length ? s.readings : [s.source || 'liquid'];
  const chips = READINGS.map(([v, l]) => `<button role="checkbox" data-reading="${v}" aria-checked="${list.includes(v)}">${l}</button>`).join('');
  const needCpu = list.includes('cpu') && cpu.state !== 'ok';
  return `<div class="control">
      <div class="control-label"><span>Shows</span><small>pick up to 2</small></div>
      <div class="chips" role="group" aria-label="Temperatures shown">${chips}</div>
      ${needCpu ? `<div class="cool-banner compact">${ctx.esc(cpuHint())}${cpu.state === 'unsupported' ? '' : ' <button type="button" class="link-btn" data-cpu-setup>' + (cpu.state === 'error' ? 'Try the setup again' : 'Turn it on') + '</button>'}</div>` : ''}
    </div>
    <div class="control">
      <div class="control-label"><span>Text color</span></div>
      <div class="light-colors"><label class="light-swatch" style="--c:${ctx.esc(s.text_color || '#ffffff')}">
        <input type="color" data-screen-text value="${ctx.esc(s.text_color || '#ffffff')}" aria-label="Text color"></label></div>
    </div>`;
}

function screenHtml(c) {
  if (!c.screen) return '';
  const s = c.screen.settings;
  const modes = SCREEN_MODES.map(([v, l]) => `<button role="radio" data-screen-mode="${v}" aria-checked="${s.mode === v}">${l}</button>`).join('');
  const rot = [0, 90, 180, 270].map((r) => `<button role="radio" data-rotate="${r}" aria-checked="${s.rotation === r}">${r}°</button>`).join('');
  const overlayToggle = `<label class="toggle-row"><span>Show temperatures on top</span>
      <input type="checkbox" class="switch small" data-screen-overlay ${s.overlay ? 'checked' : ''}></label>`;
  const extra = s.mode === 'temperature'
    ? `${readingsHtml(s)}
      <div class="control">
        <div class="control-label"><span>Background</span><small>up to 3, blended</small></div>
        ${swatchesHtml(s.background || ['#000000'], 3, 'screen-bg')}
      </div>`
    : s.mode === 'color'
    ? `<div class="control">
        <div class="control-label"><span>Colors</span><small>up to 3, blended</small></div>
        ${swatchesHtml(s.colors || [], 3, 'screen')}
      </div>
      ${overlayToggle}${s.overlay ? readingsHtml(s) : ''}`
    : s.mode === 'image'
      ? `<div class="control">
          <button type="button" class="btn" data-screen-pick>${s.image ? 'Choose another picture…' : 'Choose a picture…'}</button>
        </div>
        ${overlayToggle}${s.overlay ? readingsHtml(s) : ''}`
      : '';
  const note = s.mode === 'temperature'
    ? 'Lumora draws this in your colors and redraws it when a number changes (at most every 5 seconds). When Lumora closes, the screen goes back to the built-in display.'
    : (s.mode === 'image' || s.mode === 'color') && s.overlay
      ? 'Lumora redraws it when a number changes (at most every 5 seconds). When Lumora closes, the screen goes back to the built-in display.'
    : s.mode === 'image' || s.mode === 'color'
    ? 'It stays on the screen even with Lumora closed. Still pictures for now; animated GIFs are coming.'
    : s.mode === 'liquid' ? 'The cooler draws this itself, live, even with Lumora closed. Its text can\'t be recolored; every other option can.' : 'Lumora leaves the screen showing whatever is on it.';
  return `<section class="cool-card">
    <header><h3>Screen</h3></header>
    <input type="file" id="screenFile" accept="image/png,image/jpeg,image/webp,image/bmp,image/gif" hidden>
    <div class="screen-row">
      ${screenPreview(c)}
      <div class="screen-controls">
        <div class="chips" role="radiogroup" aria-label="Screen shows">${modes}</div>
        ${extra}
      </div>
    </div>
    <div class="control">
      <label class="control-label" for="screenBrightness"><span>Brightness</span><output id="screenBrightnessOut">${s.brightness}%</output></label>
      <input type="range" id="screenBrightness" min="0" max="100" value="${s.brightness}" style="--p:${s.brightness}%" ${s.mode === 'unchanged' ? 'disabled' : ''}>
    </div>
    <div class="control">
      <div class="control-label"><span>Rotation</span></div>
      <div class="segmented" role="radiogroup" aria-label="Screen rotation">${rot}</div>
    </div>
    <p class="hint">${note}</p>
  </section>`;
}

/** Lighting of the vendor's other devices (an NZXT Kraken's fans run through
 *  the RGB & Fan Controller), right on the cooler page. */
function fanLightingHtml(c) {
  const L = ctx.lighting;
  if (!L) return '';
  const vendor = new RegExp(c.vendor, 'i');
  const list = L.devices().filter((d) => vendor.test(d.vendor) && d.led_count > 0);
  if (!list.length) return '';
  const names = L.labels();
  const effects = L.effects();
  const rows = list.map((d) => {
    if (!d.managed) {
      return `<div class="fan-light">
        <div class="fan-light-head"><b>${ctx.esc(names[d.id])}</b><span class="nav-badge muted">Not controlled</span></div>
        <p class="hint">Lumora leaves this one to ${ctx.esc(c.owner_app || 'its own app')}.</p>
        <button class="btn" data-fan-control="${ctx.esc(d.id)}">Let Lumora light it</button>
      </div>`;
    }
    const cfg = L.configOf(d.id);
    const fx = effects.find((e) => e.id === cfg.effect);
    const quick = FAN_EFFECTS.map((id) => effects.find((e) => e.id === id)).filter(Boolean);
    if (fx && !quick.includes(fx)) quick.push(fx);
    const chips = quick.map((e) => `<button role="radio" data-fan-effect="${ctx.esc(e.id)}" data-dev="${ctx.esc(d.id)}" aria-checked="${cfg.effect === e.id}">${ctx.esc(e.name)}</button>`).join('');
    const max = fx?.max_colors || 0;
    const colors = max
      ? `<div class="light-colors">${cfg.colors.slice(0, max).map((col, i) => `<label class="light-swatch" style="--c:${ctx.esc(col)}">
          <input type="color" data-fan-color="${i}" data-dev="${ctx.esc(d.id)}" value="${ctx.esc(col)}" aria-label="Color ${i + 1}"></label>`).join('')}
          ${cfg.colors.length < max ? `<button type="button" class="light-add" data-fan-add="${ctx.esc(d.id)}" aria-label="Add a color">+</button>` : ''}</div>`
      : '';
    return `<div class="fan-light">
      <div class="fan-light-head"><b>${ctx.esc(names[d.id])}</b><button class="link-btn" data-fan-open="${ctx.esc(d.id)}">All options →</button></div>
      <div class="chips" role="radiogroup" aria-label="Effect">${chips}</div>
      ${colors}
    </div>`;
  }).join('');
  return `<section class="cool-card">
    <header><h3>Fan lighting</h3></header>
    ${rows}
    <p class="hint">Your radiator fans light up through ${list.length > 1 ? 'these' : 'this'}. Changes here are the same as on the device's own page.</p>
  </section>`;
}

function lightHtml(c) {
  if (!c.light) return '';
  const l = c.light;
  const modes = LIGHT_MODES.map(([v, t]) => `<button role="radio" data-light-mode="${v}" aria-checked="${l.mode === v}">${t}</button>`).join('');
  const max = l.mode === 'static' ? 1 : 4;
  const sync = l.mode === 'sync';
  const speeds = LIGHT_SPEEDS.map(([v, t]) => `<button role="radio" data-light-speed="${v}" aria-checked="${l.speed === v}">${t}</button>`).join('');
  return `<section class="cool-card">
    <header><h3>Pump lighting</h3></header>
    <div class="control">
      <div class="control-label"><span>Effect</span></div>
      <div class="chips" role="radiogroup" aria-label="Pump lighting effect">${modes}</div>
    </div>
    ${sync ? '' : `<div class="control">
      <div class="control-label"><span>${max > 1 ? 'Colors' : 'Color'}</span>${max > 1 ? '<small>up to 4</small>' : ''}</div>
      ${swatchesHtml(l.colors, max, 'light')}
    </div>`}
    ${l.mode === 'static' || sync ? '' : `<div class="control">
      <div class="control-label"><span>Speed</span></div>
      <div class="segmented" role="radiogroup" aria-label="Pump lighting speed">${speeds}</div>
    </div>`}
    <p class="hint">${sync ? 'Takes the color of your main effect, updated about once a second.' : 'The cooler runs this effect by itself, so it costs no CPU.'}</p>
  </section>`;
}

function setupHtml(c) {
  const app = c.owner_app || 'its own app';
  return `<div class="cool-page"><div class="hands-off static">
    <strong>One-time setup needed</strong>
    <span>Windows needs a small access driver (UsbDk) before Lumora can reach this cooler. It doesn't replace the cooler's own driver, so ${ctx.esc(app)} still works whenever Lumora isn't running it.</span>
    <button class="btn primary" data-takeover="on">Set up</button>
  </div></div>`;
}

export function renderPage(c) {
  const body = $('#stageBody');
  if (!c.takeover) {
    const app = c.owner_app || 'its own app';
    body.innerHTML = `<div class="cool-page"><div class="hands-off static">
      <strong>${ctx.esc(app)} runs this cooler</strong>
      <span>Lumora isn't touching it, so ${ctx.esc(app)} keeps the ${parts(c)} exactly as you set them. Let Lumora run it to see live temperatures and set your own curves here.</span>
      <button class="btn primary" data-takeover="on">Let Lumora run this cooler</button>
    </div></div>`;
    return;
  }
  if (c.needs_setup) {
    body.innerHTML = setupHtml(c);
    return;
  }
  if (c.needs_restart && c.message && !/^Setup is done/.test(c.message)) {
    body.innerHTML = `<div class="cool-page"><div class="hands-off static">
      <strong>Lumora can't reach this cooler yet</strong>
      <span>${ctx.esc(c.message)}</span>
      <button class="btn" data-copy-report>Copy the troubleshooting report</button>
    </div></div>`;
    return;
  }
  if (c.needs_restart) {
    body.innerHTML = `<div class="cool-page"><div class="hands-off static">
      <strong>One restart to finish</strong>
      <span>The access driver is installed. Windows connects it to the cooler at the next start, so restart your PC once. After that Lumora finds the cooler on its own${c.owner_app ? `, and ${ctx.esc(c.owner_app)} keeps working whenever Lumora isn't running it` : ''}.</span>
    </div></div>`;
    return;
  }
  const owner = c.owner_running
    ? `<div class="cool-banner">${ctx.esc(c.owner_app)} is running and will keep overriding Lumora. Quit it fully: right-click its icon in the system tray and choose Exit.</div>`
    : '';
  const banner = c.state === 'error'
    ? `<div class="cool-banner error">${ctx.esc(c.message)}</div>${owner}`
    : owner || (c.state === 'connecting' ? '<div class="cool-banner">Connecting to the cooler…</div>' : '');
  const notice = c.notice ? `<div class="cool-banner">${ctx.esc(c.notice)}</div>` : '';
  body.innerHTML = `<div class="cool-page">
    ${banner}${notice}
    ${readouts(c)}
    <div class="cool-grid">
      <div class="cool-col">${c.channels.map((ch) => channelHtml(c, ch)).join('')}</div>
      <div class="cool-col">${screenHtml(c)}${lightHtml(c)}${fanLightingHtml(c)}</div>
    </div>
  </div>`;
}

/** Live numbers only, so dragging a curve isn't interrupted. */
export function refreshLive(c) {
  if (!c || !document.querySelector('.readouts')) return render();
  const s = c.status || {};
  const set = (key, value, sub) => {
    const el = document.querySelector(`[data-readout="${key}"]`);
    if (!el) return;
    el.querySelector('b').textContent = value;
    if (sub != null) el.querySelector('.readout-sub').textContent = sub;
  };
  set('liquid', fmt(s.liquid_c, 1));
  set('pump', fmt(s.pump_rpm), pumpSub(s));
  set('fan', fmt(s.fan_rpm), fanSub(s));
  if (!dragging) {
    for (const ch of c.channels) {
      const svg = document.querySelector(`svg[data-curve="${ch.id}"]`);
      if (svg) svg.outerHTML = curveSvg(c, ch);
    }
  }
}

export function refreshSidebarTemps() {
  for (const c of coolers()) {
    const el = document.querySelector(`[data-cooler-temp="${CSS.escape(c.id)}"]`);
    if (el && c.status?.liquid_c != null) el.textContent = `${fmt(c.status.liquid_c, 1)}°`;
  }
}

function render() {
  ctx.rerender();
}

/** True while the user is mid-edit, so live events don't overwrite it. */
export const isBusy = () => !!dragging || !!pendingSave;

// --- Interaction --------------------------------------------------------------------

function onClick(ev) {
  const c = selectedCooler();
  if (!c) return;
  if (ev.target.closest('[data-takeover="on"]')) return askTakeover(c);
  const sp = ev.target.closest('[data-speed]');
  if (sp) {
    const ch = c.channels.find((x) => x.id === sp.dataset.speed);
    const v = sp.dataset.v;
    update(c, () => {
      if (v === 'fixed') ch.setting = { mode: 'fixed', duty: Math.max(ch.min_duty, ch.table[15]) };
      else if (v === 'custom') ch.setting = { mode: 'custom', points: seedPoints(ch) };
      else ch.setting = { mode: 'preset', preset: v };
    }, { now: true });
    return render();
  }
  const sm = ev.target.closest('[data-screen-mode]');
  if (sm) {
    const mode = sm.dataset.screenMode;
    // A picture mode needs a picture first.
    if (mode === 'image' && !c.screen.settings.image) {
      $('#screenFile')?.click();
      return;
    }
    update(c, (x) => {
      x.screen.settings.mode = mode;
      if (mode === 'color' && !(x.screen.settings.colors || []).length) x.screen.settings.colors = PALETTE.slice(0, 2);
    }, { now: true });
    return render();
  }
  if (ev.target.closest('[data-screen-pick]')) return $('#screenFile').click();
  const rd = ev.target.closest('[data-reading]');
  if (rd) {
    update(c, (x) => {
      const st = x.screen.settings;
      let list = st.readings && st.readings.length ? [...st.readings] : [st.source || 'liquid'];
      const v = rd.dataset.reading;
      if (list.includes(v)) list = list.length > 1 ? list.filter((r) => r !== v) : list;
      else list = [...list, v].slice(-2);
      st.readings = list;
    }, { now: true });
    return render();
  }
  if (ev.target.closest('[data-screen-overlay]')) {
    update(c, (x) => (x.screen.settings.overlay = ev.target.checked), { now: true });
    return render();
  }
  if (ev.target.closest('[data-screen-bg-add]')) {
    update(c, (x) => {
      const bg = x.screen.settings.background || (x.screen.settings.background = ['#000000']);
      bg.push(['#1a1040', '#0b2a3a', '#2a0a1a'].find((p) => !bg.includes(p)) || '#202020');
    }, { now: true });
    return render();
  }
  const brm = ev.target.closest('[data-screen-bg-remove]');
  if (brm) {
    ev.preventDefault();
    update(c, (x) => x.screen.settings.background.splice(+brm.dataset.screenBgRemove, 1), { now: true });
    return render();
  }
  if (ev.target.closest('[data-screen-add]')) {
    update(c, (x) => x.screen.settings.colors.push(PALETTE.find((p) => !x.screen.settings.colors.includes(p)) || '#ffffff'), { now: true });
    return render();
  }
  const srm = ev.target.closest('[data-screen-remove]');
  if (srm) {
    ev.preventDefault();
    update(c, (x) => x.screen.settings.colors.splice(+srm.dataset.screenRemove, 1), { now: true });
    return render();
  }
  // Fan lighting card (other devices' lighting).
  const L = ctx.lighting;
  const fe = ev.target.closest('[data-fan-effect]');
  if (fe && L) {
    const effect = L.effects().find((e) => e.id === fe.dataset.fanEffect);
    const cfg = L.configOf(fe.dataset.dev);
    L.setEffect(fe.dataset.dev, { ...cfg, effect: effect.id, colors: L.colorsFor(effect, cfg.colors.length ? cfg.colors : effect.default_colors) });
    return render();
  }
  const fa = ev.target.closest('[data-fan-add]');
  if (fa && L) {
    const cfg = L.configOf(fa.dataset.fanAdd);
    L.setEffect(fa.dataset.fanAdd, { ...cfg, colors: [...cfg.colors, PALETTE.find((p) => !cfg.colors.includes(p)) || '#ffffff'] });
    return render();
  }
  const fo = ev.target.closest('[data-fan-open]');
  if (fo && L) return L.open(fo.dataset.fanOpen);
  const fc = ev.target.closest('[data-fan-control]');
  if (fc && L) {
    const d = L.devices().find((x) => x.id === fc.dataset.fanControl);
    if (d) L.setControl(d, true).then(render);
    return;
  }
  const rt = ev.target.closest('[data-rotate]');
  if (rt) {
    update(c, (x) => (x.screen.settings.rotation = +rt.dataset.rotate), { now: true });
    return render();
  }
  const lm = ev.target.closest('[data-light-mode]');
  if (lm) {
    update(c, (x) => {
      x.light.mode = lm.dataset.lightMode;
      if (x.light.mode !== 'static' && x.light.colors.length < 2) x.light.colors.push('#00d2ff');
    }, { now: true });
    return render();
  }
  const ls = ev.target.closest('[data-light-speed]');
  if (ls) {
    update(c, (x) => (x.light.speed = ls.dataset.lightSpeed), { now: true });
    return render();
  }
  if (ev.target.closest('[data-light-add]')) {
    update(c, (x) => x.light.colors.push(PALETTE.find((p) => !x.light.colors.includes(p)) || '#ffffff'), { now: true });
    return render();
  }
  const rm = ev.target.closest('[data-light-remove]');
  if (rm) {
    ev.preventDefault();
    update(c, (x) => x.light.colors.splice(+rm.dataset.lightRemove, 1), { now: true });
    return render();
  }
}

function onInput(ev) {
  const c = selectedCooler();
  if (!c) return;
  const t = ev.target;
  if (t.dataset.fixed) {
    const ch = c.channels.find((x) => x.id === t.dataset.fixed);
    t.style.setProperty('--p', `${t.value}%`);
    t.closest('.control').querySelector('output').textContent = `${t.value}%`;
    update(c, () => (ch.setting = { mode: 'fixed', duty: +t.value }));
  } else if (t.dataset.screenText != null || t.dataset.screenBgColor != null) {
    t.closest('.light-swatch').style.setProperty('--c', t.value);
    update(c, (x) => {
      if (t.dataset.screenText != null) x.screen.settings.text_color = t.value;
      else x.screen.settings.background[+t.dataset.screenBgColor] = t.value;
    }, { wait: 900 });
    const pv = document.querySelector('.screen-preview');
    if (pv) pv.outerHTML = screenPreview(c);
  } else if (t.dataset.screenColor != null) {
    t.closest('.light-swatch').style.setProperty('--c', t.value);
    update(c, (x) => (x.screen.settings.colors[+t.dataset.screenColor] = t.value), { wait: 900 });
    const pv = document.querySelector('.screen-preview');
    if (pv) pv.outerHTML = screenPreview(c);
  } else if (t.dataset.fanColor != null && ctx.lighting) {
    t.closest('.light-swatch').style.setProperty('--c', t.value);
    const cfg = ctx.lighting.configOf(t.dataset.dev);
    const colors = [...cfg.colors];
    colors[+t.dataset.fanColor] = t.value;
    queueFan(t.dataset.dev, { ...cfg, colors });
  } else if (t.dataset.lightColor != null) {
    t.closest('.light-swatch').style.setProperty('--c', t.value);
    update(c, (x) => (x.light.colors[+t.dataset.lightColor] = t.value));
  } else if (t.id === 'screenBrightness') {
    t.style.setProperty('--p', `${t.value}%`);
    $('#screenBrightnessOut').textContent = `${t.value}%`;
    update(c, (x) => (x.screen.settings.brightness = +t.value));
  }
}

// Color drags send at most one lighting change per frame.
let fanPending = null;
function queueFan(id, cfg) {
  if (!fanPending) requestAnimationFrame(() => {
    const p = fanPending;
    fanPending = null;
    ctx.lighting.setEffect(p.id, p.cfg);
  });
  fanPending = { id, cfg };
}

/** Fits a picture to the screen (filling it, centered), saves it and shows it. */
async function pickPicture(c, file) {
  if (!file) return;
  const size = c.screen.width;
  try {
    const bmp = await createImageBitmap(file);
    const cv = document.createElement('canvas');
    cv.width = cv.height = size;
    const g = cv.getContext('2d');
    const k = Math.max(size / bmp.width, size / bmp.height);
    const w = bmp.width * k, h = bmp.height * k;
    g.fillStyle = '#000';
    g.fillRect(0, 0, size, size);
    g.drawImage(bmp, (size - w) / 2, (size - h) / 2, w, h);
    const data = g.getImageData(0, 0, size, size).data;
    let bin = '';
    for (let i = 0; i < data.length; i += 0x8000) bin += String.fromCharCode.apply(null, data.subarray(i, i + 0x8000));
    const path = await ctx.api.invoke('save_cooler_picture', { id: c.id, rgba: btoa(bin) });
    const thumb = document.createElement('canvas');
    thumb.width = thumb.height = 160;
    thumb.getContext('2d').drawImage(cv, 0, 0, 160, 160);
    const url = thumb.toDataURL('image/jpeg', 0.82);
    pictures.set(c.id, url);
    try { localStorage.setItem('lumora.screen.' + c.id, url); } catch { /* preview only */ }
    update(c, (x) => {
      x.screen.settings.mode = 'image';
      x.screen.settings.image = path;
    }, { now: true });
    ctx.toast('Sending the picture to the screen…');
    render();
  } catch (err) {
    ctx.toast(`Couldn't use that picture: ${err}`);
  }
}

function onChange(ev) {
  if (ev.target.id === 'screenFile') {
    const c = selectedCooler();
    const file = ev.target.files && ev.target.files[0];
    ev.target.value = '';
    if (c && file) pickPicture(c, file);
    return;
  }
  if (ev.target.id !== 'takeoverSwitch') return;
  const c = selectedCooler();
  if (!c) return;
  if (ev.target.checked) {
    ev.target.checked = false;
    askTakeover(c);
  } else {
    update(c, (x) => (x.takeover = false), { now: true });
    ctx.toast(`${c.name} is back with ${c.owner_app || 'its own app'}`);
  }
}

// Dragging curve points.
let dragging = null;
document.addEventListener('pointerdown', (ev) => {
  const h = ev.target.closest?.('.curve-handle');
  const c = h && selectedCooler();
  if (!c) return;
  const svg = h.ownerSVGElement;
  const ch = c.channels.find((x) => x.id === h.dataset.ch);
  dragging = { c, ch, i: +h.dataset.i, svg };
  svg.setPointerCapture(ev.pointerId);
  ev.preventDefault();
});
document.addEventListener('pointermove', (ev) => {
  if (!dragging) return;
  const { c, ch, i, svg } = dragging;
  const box = svg.viewBox.baseVal, r = svg.getBoundingClientRect();
  const sx = ((ev.clientX - r.left) / r.width) * box.width, sy = ((ev.clientY - r.top) / r.height) * box.height;
  const P = 26;
  const pts = ch.setting.points;
  const lo = i > 0 ? pts[i - 1][0] + 1 : T0, hi = i < pts.length - 1 ? pts[i + 1][0] - 1 : T1;
  const t = Math.round(T0 + ((sx - P) / (box.width - P * 2)) * (TMAX - T0));
  const d = Math.round(((box.height - P - sy) / (box.height - P * 2)) * 100);
  pts[i] = [Math.max(lo, Math.min(hi, t)), Math.max(ch.min_duty, Math.min(100, d))];
  // Duty never drops as temperature rises: drag neighbours along.
  for (let k = i + 1; k < pts.length; k++) pts[k][1] = Math.max(pts[k][1], pts[i][1]);
  for (let k = i - 1; k >= 0; k--) pts[k][1] = Math.min(pts[k][1], pts[i][1]);
  ch.table = localTable(pts, ch.min_duty);
  svg.outerHTML = curveSvg(c, ch);
  dragging.svg = document.querySelector(`svg[data-curve="${ch.id}"]`);
});
document.addEventListener('pointerup', () => {
  if (!dragging) return;
  const { c } = dragging;
  dragging = null;
  update(c, () => {});
});

/** Same maths as the Rust side, for instant feedback while dragging. */
export function localTable(points, min) {
  const pts = [...points.map((p) => [p[0], Math.min(100, p[1])]), [59, 100]].sort((a, b) => a[0] - b[0] || b[1] - a[1]);
  const norm = [];
  for (const [t, d] of pts) {
    const last = norm[norm.length - 1];
    if (last && t === last[0]) continue;
    if (last && last[1] >= 100) break;
    norm.push([t, last ? Math.max(d, last[1]) : d]);
  }
  const out = [];
  for (let t = T0; t <= T1; t++) {
    let v = norm[norm.length - 1][1];
    if (t <= norm[0][0]) v = norm[0][1];
    else {
      for (let k = 0; k < norm.length - 1; k++) {
        const [t0, d0] = norm[k], [t1, d1] = norm[k + 1];
        if (t >= t0 && t <= t1) { v = Math.round(d0 + ((t - t0) / (t1 - t0)) * (d1 - d0)); break; }
      }
    }
    out.push(Math.max(min, Math.min(100, v)));
  }
  return out;
}

function $(s) {
  return document.querySelector(s);
}
