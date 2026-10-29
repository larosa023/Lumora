// JavaScript port of crates/lumora-core/src/effects.rs.
//
// The real lights are always driven by the Rust engine. This port renders the
// little live previews on the effect tiles, and powers the browser-only dev
// preview (`npm run preview`). Keep the maths in step with the Rust version.

export const hexToRgb = (h) => {
  const s = h.replace('#', '');
  const v = s.length === 3 ? s.split('').map((c) => c + c).join('') : s;
  return [parseInt(v.slice(0, 2), 16), parseInt(v.slice(2, 4), 16), parseInt(v.slice(4, 6), 16)];
};

export const rgbToHex = ([r, g, b]) =>
  '#' + [r, g, b].map((v) => Math.round(Math.max(0, Math.min(255, v))).toString(16).padStart(2, '0')).join('');

const clamp01 = (v) => Math.max(0, Math.min(1, v));
const fract = (v) => v - Math.floor(v);
const mod = (a, n) => ((a % n) + n) % n;
export const smooth = (t) => {
  t = clamp01(t);
  return t * t * (3 - 2 * t);
};

export function hsv(h, s, v) {
  h = mod(h, 360) / 60;
  const c = v * s;
  const x = c * (1 - Math.abs((h % 2) - 1));
  const [r, g, b] = [[c, x, 0], [x, c, 0], [0, c, x], [0, x, c], [x, 0, c], [c, 0, x]][Math.floor(h) % 6];
  const m = v - c;
  return [(r + m) * 255, (g + m) * 255, (b + m) * 255];
}

const lerp = (a, b, t) => {
  t = clamp01(t);
  return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
};
const scale = (c, f) => [c[0] * clamp01(f), c[1] * clamp01(f), c[2] * clamp01(f)];
const BLACK = [0, 0, 0];

export function sample(colors, t, wrap) {
  const n = colors.length;
  if (!n) return BLACK;
  if (n === 1) return colors[0];
  if (wrap) {
    const u = mod(t, 1) * n;
    const i = Math.floor(u) % n;
    return lerp(colors[i], colors[(i + 1) % n], smooth(fract(u)));
  }
  const u = clamp01(t) * (n - 1);
  const i = Math.min(Math.floor(u), n - 2);
  return lerp(colors[i], colors[i + 1], u - i);
}

export const speedFactor = (s) => 0.15 * Math.pow(20, Math.min(s, 100) / 100);
export const sizeFactor = (s) => 0.25 * Math.pow(16, Math.min(s, 100) / 100);

export function along(p, dir) {
  const center = () => Math.min(Math.hypot(p.x - 0.5, p.y - 0.5) / 0.7071, 1);
  switch (dir) {
    case 'left': return 1 - p.x;
    case 'down': return p.y;
    case 'up': return 1 - p.y;
    case 'out': return center();
    case 'in': return 1 - center();
    default: return p.x;
  }
}

function hash(x, y) {
  let h = (Math.imul(x, 0x27d4eb2d) ^ Math.imul(y, 0x165667b1)) >>> 0;
  h = Math.imul(h ^ (h >>> 15), 0x85ebca6b) >>> 0;
  h = (h ^ (h >>> 13)) >>> 0;
  return (h & 0xffff) / 65535;
}

export function noise(x, y) {
  const xi = Math.floor(x), yi = Math.floor(y);
  const xf = smooth(x - xi), yf = smooth(y - yi);
  const a = hash(xi, yi) + (hash(xi + 1, yi) - hash(xi, yi)) * xf;
  const b = hash(xi, yi + 1) + (hash(xi + 1, yi + 1) - hash(xi, yi + 1)) * xf;
  return a + (b - a) * yf;
}

/** Builds a renderer: `(t, dt, leds, out) => void`, writing [r,g,b] per LED. */
export function build(cfg, desc, ledCount) {
  let colors = (cfg.colors || []).map(hexToRgb);
  if (desc) {
    if (colors.length < desc.min_colors) {
      const fill = desc.default_colors.map(hexToRgb);
      colors = colors.concat(fill.slice(0, desc.min_colors - colors.length));
    }
    if (desc.max_colors > 0) colors = colors.slice(0, desc.max_colors);
  }
  if (!colors.length) colors = [[255, 255, 255]];
  const speed = speedFactor(cfg.speed ?? 40);
  const size = sizeFactor(cfg.size ?? 50);
  const dir = cfg.direction || 'right';
  const n = colors.length;

  switch (cfg.effect) {
    case 'gradient':
      return (t, dt, leds, out) => leds.forEach((p, i) => (out[i] = sample(colors, along(p, dir), false)));
    case 'breathing':
      return (t, dt, leds, out) => {
        const phase = t * speed * 0.5;
        const c = colors[mod(Math.floor(phase), n)];
        const level = Math.pow(0.5 - 0.5 * Math.cos(fract(phase) * Math.PI * 2), 1.6);
        out.fill(scale(c, level));
      };
    case 'spectrum':
      return (t, dt, leds, out) => out.fill(hsv(t * speed * 40, 1, 1));
    case 'rainbow_wave':
      return (t, dt, leds, out) => {
        const shift = t * speed * 0.5;
        leds.forEach((p, i) => (out[i] = hsv((along(p, dir) / size - shift) * 360, 1, 1)));
      };
    case 'color_wave':
      return (t, dt, leds, out) => {
        const shift = t * speed * 0.4;
        leds.forEach((p, i) => (out[i] = sample(colors, along(p, dir) / size - shift, true)));
      };
    case 'ripple': {
      const d = dir === 'in' ? 'in' : 'out';
      return (t, dt, leds, out) => {
        leds.forEach((p, i) => {
          const v = (along(p, d) * 2) / size - t * speed * 0.8;
          const w = Math.pow(Math.max(Math.sin(fract(v) * Math.PI * 2), 0), 1.5);
          out[i] = scale(colors[mod(Math.floor(v), n)], w);
        });
      };
    }
    case 'comet': {
      const tail = Math.max(0.04, Math.min(0.95, 0.18 * size));
      const bg = colors[1] || BLACK;
      return (t, dt, leds, out) => {
        const head = mod(t * speed * 0.6, 1);
        leds.forEach((p, i) => {
          const dist = mod(head - along(p, dir), 1);
          const k = dist < tail ? Math.pow(1 - dist / tail, 2) : 0;
          out[i] = lerp(bg, colors[0], k);
        });
      };
    }
    case 'marquee': {
      const dashes = Math.max(6 / size, 1);
      return (t, dt, leds, out) => {
        leds.forEach((p, i) => {
          const v = along(p, dir) * dashes - t * speed * 1.5;
          const on = smooth(1 - Math.max(Math.abs(fract(v) - 0.25) * 4 - 0.6, 0) * 2.5);
          out[i] = scale(colors[mod(Math.floor(v), n)], on);
        });
      };
    }
    case 'twinkle': {
      const level = new Float32Array(ledCount);
      const rising = new Uint8Array(ledCount);
      const pick = new Uint8Array(ledCount);
      return (t, dt, leds, out) => {
        const rate = speed * 2.5;
        const spawn = Math.min(0.35 * size * speed * dt, 1);
        for (let i = 0; i < Math.min(out.length, ledCount); i++) {
          if (rising[i]) {
            level[i] += rate * dt;
            if (level[i] >= 1) { level[i] = 1; rising[i] = 0; }
          } else if (level[i] > 0) {
            level[i] = Math.max(level[i] - rate * 0.6 * dt, 0);
          } else if (Math.random() < spawn) {
            rising[i] = 1;
            pick[i] = Math.floor(Math.random() * n) % n;
          }
          out[i] = scale(colors[pick[i]], smooth(level[i]));
        }
      };
    }
    case 'aurora':
      return (t, dt, leds, out) => {
        const tt = t * speed * 0.35;
        leds.forEach((p, i) => {
          const x = (p.x / size) * 2;
          const hue = noise(x + tt * 0.6, p.y * 1.5 + tt * 0.2) * 1.4 + tt * 0.05;
          const glow = 0.35 + 0.65 * smooth(noise(x * 1.7 - tt, p.y * 2 + 7));
          out[i] = scale(sample(colors, hue, true), glow);
        });
      };
    case 'fire': {
      const palette = [BLACK, colors[0], colors[1 % n], colors[2 % n]];
      return (t, dt, leds, out) => {
        const tt = t * speed * 1.6;
        leds.forEach((p, i) => {
          const height = along(p, dir);
          const across = dir === 'up' || dir === 'down' ? p.x : p.y;
          let f = noise(across * 6 + i * 0.37, tt - height * 2);
          f = f * 0.7 + 0.3 * noise(across * 13, tt * 2.3);
          const reach = 0.35 + 0.45 * Math.min(size, 2);
          out[i] = sample(palette, clamp01(f * 1.25 - height / reach), false);
        });
      };
    }
    default:
      return (t, dt, leds, out) => out.fill(colors[0]);
  }
}

/** Mirror of `DeviceInfo::led_positions` in Rust. */
export function ledPositions(dev) {
  const out = Array.from({ length: dev.led_count }, () => ({ x: 0.5, y: 0.5 }));
  const linearZones = Math.max(dev.zones.filter((z) => z.kind !== 'matrix').length, 1);
  let row = 0;
  for (const z of dev.zones) {
    if (z.kind === 'matrix' && z.matrix) {
      const { width: w, height: h, map } = z.matrix;
      map.forEach((led, cell) => {
        if (led == null) return;
        const c = cell % w, r = Math.floor(cell / w);
        out[z.start + led] = { x: w > 1 ? c / (w - 1) : 0.5, y: h > 1 ? r / (h - 1) : 0.5 };
      });
    } else {
      const y = linearZones > 1 ? row / (linearZones - 1) : 0.5;
      for (let i = 0; i < z.count; i++) out[z.start + i] = { x: z.count > 1 ? i / (z.count - 1) : 0.5, y };
      row++;
    }
  }
  return out;
}
