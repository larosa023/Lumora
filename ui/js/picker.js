// Color picker popover: saturation/value field, hue rail, hex entry,
// curated swatches, recent colors and (where the webview supports it) an
// eyedropper for grabbing a color from anywhere on screen.

import { hexToRgb, rgbToHex, hsv } from './effects.js';

export const SWATCHES = [
  '#ff2d55', '#ff3da8', '#d63dff', '#7c5cff', '#3d6bff', '#00a2ff', '#00d4ff', '#00ffd5',
  '#00ff85', '#7dff3d', '#e6ff3d', '#ffd400', '#ff9500', '#ff5a1f', '#ffd7a8', '#ffffff',
];

const RECENT_KEY = 'lumora.recentColors';

function recent() {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY)) || [];
  } catch {
    return [];
  }
}

function remember(hex) {
  try {
    const list = [hex, ...recent().filter((c) => c !== hex)].slice(0, 8);
    localStorage.setItem(RECENT_KEY, JSON.stringify(list));
  } catch {
    /* storage unavailable: recent colors just won't persist */
  }
}

function toHsv([r, g, b]) {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b), d = max - min;
  let h = 0;
  if (d) {
    if (max === r) h = ((g - b) / d) % 6;
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
  }
  return { h: (h * 60 + 360) % 360, s: max ? d / max : 0, v: max };
}

let open = null;

export function closePicker() {
  if (open) open.close();
}

/** Opens the picker next to `anchor`. `onChange(hex)` fires live while dragging. */
export function openPicker(anchor, initial, onChange, { onRemove } = {}) {
  closePicker();
  let { h, s, v } = toHsv(hexToRgb(initial));
  const el = document.createElement('div');
  el.className = 'picker';
  el.setAttribute('role', 'dialog');
  el.setAttribute('aria-label', 'Choose a color');
  el.innerHTML = `
    <div class="picker-field" tabindex="0" aria-label="Saturation and brightness"><div class="picker-knob"></div></div>
    <div class="picker-hue" tabindex="0" aria-label="Hue"><div class="picker-knob"></div></div>
    <div class="picker-row">
      <span class="picker-chip"></span>
      <input class="picker-hex" spellcheck="false" maxlength="7" aria-label="Hex color">
      ${'EyeDropper' in window ? '<button class="icon-btn picker-drop" title="Pick a color from the screen" aria-label="Pick from screen"><svg viewBox="0 0 24 24"><path d="M19.4 4.6a2.1 2.1 0 0 0-3 0l-2.3 2.3-1-1-1.4 1.4 1 1-7 7V19h3.7l7-7 1 1 1.4-1.4-1-1 2.3-2.3a2.1 2.1 0 0 0 .3-2.7z"/></svg></button>' : ''}
    </div>
    <div class="picker-swatches"></div>
    <div class="picker-recent-wrap"><span class="picker-sub">Recent</span><div class="picker-swatches picker-recent"></div></div>
    ${onRemove ? '<button class="text-btn picker-remove">Remove color</button>' : ''}
  `;
  document.body.appendChild(el);

  const field = el.querySelector('.picker-field');
  const hue = el.querySelector('.picker-hue');
  const hex = el.querySelector('.picker-hex');
  const chip = el.querySelector('.picker-chip');

  const current = () => rgbToHex(hsv(h, s, v));
  const paint = (emit = true) => {
    const c = current();
    field.style.setProperty('--hue', rgbToHex(hsv(h, 1, 1)));
    field.querySelector('.picker-knob').style.cssText = `left:${s * 100}%;top:${(1 - v) * 100}%;background:${c}`;
    hue.querySelector('.picker-knob').style.cssText = `left:${(h / 360) * 100}%;background:${rgbToHex(hsv(h, 1, 1))}`;
    chip.style.background = c;
    if (document.activeElement !== hex) hex.value = c;
    if (emit) onChange(c);
  };

  const drag = (target, move) => {
    const handle = (e) => {
      const r = target.getBoundingClientRect();
      move(Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)), Math.max(0, Math.min(1, (e.clientY - r.top) / r.height)));
      paint();
    };
    target.addEventListener('pointerdown', (e) => {
      target.setPointerCapture(e.pointerId);
      handle(e);
      const up = () => {
        target.removeEventListener('pointermove', handle);
        target.removeEventListener('pointerup', up);
        remember(current());
      };
      target.addEventListener('pointermove', handle);
      target.addEventListener('pointerup', up);
    });
  };
  drag(field, (x, y) => { s = x; v = 1 - y; });
  drag(hue, (x) => { h = x * 359.9; });

  field.addEventListener('keydown', (e) => {
    const k = { ArrowLeft: [-0.02, 0], ArrowRight: [0.02, 0], ArrowUp: [0, 0.02], ArrowDown: [0, -0.02] }[e.key];
    if (!k) return;
    e.preventDefault();
    s = Math.max(0, Math.min(1, s + k[0]));
    v = Math.max(0, Math.min(1, v + k[1]));
    paint();
  });
  hue.addEventListener('keydown', (e) => {
    const k = { ArrowLeft: -4, ArrowRight: 4 }[e.key];
    if (!k) return;
    e.preventDefault();
    h = (h + k + 360) % 360;
    paint();
  });

  const setHex = (value) => {
    const m = value.trim().match(/^#?([0-9a-f]{6}|[0-9a-f]{3})$/i);
    if (!m) return false;
    ({ h, s, v } = toHsv(hexToRgb(m[1])));
    paint();
    return true;
  };
  hex.addEventListener('input', () => setHex(hex.value));
  hex.addEventListener('change', () => {
    if (setHex(hex.value)) remember(current());
    else hex.value = current();
  });

  const swatchRow = (container, list) => {
    container.innerHTML = list.map((c) => `<button class="swatch" style="--c:${c}" data-c="${c}" aria-label="${c}"></button>`).join('');
    container.addEventListener('click', (e) => {
      const c = e.target.closest('.swatch')?.dataset.c;
      if (c) {
        setHex(c);
        remember(c);
      }
    });
  };
  swatchRow(el.querySelector('.picker-swatches'), SWATCHES);
  const rec = recent();
  if (rec.length) swatchRow(el.querySelector('.picker-recent'), rec);
  else el.querySelector('.picker-recent-wrap').remove();

  el.querySelector('.picker-drop')?.addEventListener('click', async () => {
    try {
      const { sRGBHex } = await new window.EyeDropper().open();
      setHex(sRGBHex);
      remember(current());
    } catch {
      /* cancelled */
    }
  });
  el.querySelector('.picker-remove')?.addEventListener('click', () => {
    close();
    onRemove();
  });

  // Position beside the anchor, kept on screen.
  const a = anchor.getBoundingClientRect();
  const pw = el.offsetWidth, ph = el.offsetHeight;
  let left = a.left + a.width / 2 - pw / 2;
  let top = a.bottom + 10;
  if (top + ph > window.innerHeight - 8) top = a.top - ph - 10;
  left = Math.max(8, Math.min(left, window.innerWidth - pw - 8));
  top = Math.max(8, top);
  el.style.left = left + 'px';
  el.style.top = top + 'px';

  const outside = (e) => {
    if (!el.contains(e.target) && !anchor.contains(e.target)) close();
  };
  const esc = (e) => {
    if (e.key === 'Escape') {
      close();
      anchor.focus();
    }
  };
  setTimeout(() => document.addEventListener('pointerdown', outside), 0);
  document.addEventListener('keydown', esc);

  function close() {
    document.removeEventListener('pointerdown', outside);
    document.removeEventListener('keydown', esc);
    el.remove();
    remember(current());
    open = null;
  }
  open = { close };
  paint(false);
  field.focus();
  return close;
}
