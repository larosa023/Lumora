// Draws devices as glowing objects on a canvas: keycaps for keyboards, a
// light bar for RAM, rings for fans and pumps, an outline for mice.
//
// Each layout works in its own unit space; the view scales it to fit.

const RING = /fan|ring|pump|halo|loop|circle/i;

function roundRect(ctx, x, y, w, h, r) {
  r = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

// --- Layout pieces ----------------------------------------------------------

function ringLeds(zone, cx, cy, radius, ledR) {
  const leds = [];
  for (let i = 0; i < zone.count; i++) {
    const a = -Math.PI / 2 + (i / zone.count) * Math.PI * 2;
    leds.push({ i: zone.start + i, x: cx + Math.cos(a) * radius, y: cy + Math.sin(a) * radius, r: ledR });
  }
  return leds;
}

function ringDecor(cx, cy, radius, hub) {
  return (ctx, pal) => {
    ctx.fillStyle = pal.body;
    ctx.beginPath();
    ctx.arc(cx, cy, radius * 1.18, 0, Math.PI * 2);
    ctx.fill();
    ctx.strokeStyle = pal.edge;
    ctx.lineWidth = radius * 0.03;
    ctx.stroke();
    if (!hub) return;
    // Fan blades, drawn faintly.
    ctx.fillStyle = pal.detail;
    for (let k = 0; k < 7; k++) {
      ctx.save();
      ctx.translate(cx, cy);
      ctx.rotate((k / 7) * Math.PI * 2);
      ctx.beginPath();
      ctx.ellipse(radius * 0.42, 0, radius * 0.38, radius * 0.13, 0.5, 0, Math.PI * 2);
      ctx.fill();
      ctx.restore();
    }
    ctx.fillStyle = pal.body;
    ctx.beginPath();
    ctx.arc(cx, cy, radius * 0.22, 0, Math.PI * 2);
    ctx.fill();
  };
}

function stripLeds(zone, x0, x1, y, ledR) {
  const leds = [];
  for (let i = 0; i < zone.count; i++) {
    const t = zone.count > 1 ? i / (zone.count - 1) : 0.5;
    leds.push({ i: zone.start + i, x: x0 + (x1 - x0) * t, y, r: ledR });
  }
  return leds;
}

function keyboardLayout(dev, zone) {
  const { width: W, height: H, map } = zone.matrix;
  const gap = 0.14, pad = 0.45;
  const leds = [];
  for (let r = 0; r < H; r++) {
    for (let c = 0; c < W; c++) {
      const led = map[r * W + c];
      if (led == null) continue;
      let span = 1;
      while (c + span < W && map[r * W + c + span] == null && r * W + c + span < map.length) span++;
      leds.push({
        i: zone.start + led, key: true,
        x: pad + c + gap / 2, y: pad + r + gap / 2, w: span - gap, h: 1 - gap,
      });
    }
  }
  const w = W + pad * 2, h = H + pad * 2;
  return {
    w, h, leds,
    decor(ctx, pal) {
      ctx.fillStyle = pal.body;
      roundRect(ctx, 0, 0, w, h, 0.5);
      ctx.fill();
      ctx.strokeStyle = pal.edge;
      ctx.lineWidth = 0.04;
      ctx.stroke();
    },
  };
}

function ramLayout(dev) {
  const w = 13.3, h = 3.4;
  const zoneLeds = dev.zones.flatMap((z) => stripLeds(z, 0.7, w - 0.7, 0.55, 0.32));
  return {
    w, h, leds: zoneLeds, bar: { x: 0.25, y: 0.1, w: w - 0.5, h: 0.9 },
    decor(ctx, pal) {
      ctx.fillStyle = pal.body;
      roundRect(ctx, 0, 0, w, h - 0.35, 0.2);
      ctx.fill();
      ctx.strokeStyle = pal.edge;
      ctx.lineWidth = 0.05;
      ctx.stroke();
      ctx.fillStyle = pal.detail;
      for (let k = 0; k < 8; k++) {
        roundRect(ctx, 0.6 + k * 1.53, 1.35, 1.2, 1.2, 0.08);
        ctx.fill();
      }
      ctx.fillStyle = pal.contact;
      for (let k = 0; k < 40; k++) ctx.fillRect(0.4 + k * 0.315, h - 0.3, 0.2, 0.3);
    },
  };
}

function gpuLayout(dev) {
  const w = 16, h = 5.4;
  const leds = dev.zones.flatMap((z, zi) => stripLeds(z, 1, w - 1, 0.55 + zi * 0.5, 0.3));
  return {
    w, h, leds, bar: { x: 0.5, y: 0.15, w: w - 1, h: 0.8 },
    decor(ctx, pal) {
      ctx.fillStyle = pal.body;
      roundRect(ctx, 0, 0, w, h, 0.45);
      ctx.fill();
      ctx.strokeStyle = pal.edge;
      ctx.lineWidth = 0.05;
      ctx.stroke();
      for (const cx of [4.2, 11.8]) ringDecor(cx, 3.1, 1.75, true)(ctx, pal);
    },
  };
}

function mouseLayout(dev) {
  const w = 6, h = 9.6;
  const path = (ctx) => {
    ctx.beginPath();
    ctx.moveTo(3, 0.2);
    ctx.bezierCurveTo(5.4, 0.2, 5.8, 3, 5.8, 5.2);
    ctx.bezierCurveTo(5.8, 8, 4.6, 9.4, 3, 9.4);
    ctx.bezierCurveTo(1.4, 9.4, 0.2, 8, 0.2, 5.2);
    ctx.bezierCurveTo(0.2, 3, 0.6, 0.2, 3, 0.2);
    ctx.closePath();
  };
  // Points around the outline for underglow-style strips.
  const outline = (t) => {
    const a = Math.PI / 2 + t * Math.PI * 2;
    const rx = 3.15, ry = 4.85;
    return { x: 3 + Math.cos(a) * rx, y: 4.8 + Math.sin(a) * ry * (Math.sin(a) > 0 ? 0.95 : 1) };
  };
  const leds = [];
  let other = 0;
  for (const z of dev.zones) {
    if (z.count === 1) {
      const name = z.name.toLowerCase();
      const pos = /scroll|wheel/.test(name) ? { x: 3, y: 2.1 } : /logo/.test(name) ? { x: 3, y: 6.6 } : { x: 3, y: 4.2 + other++ * 0.9 };
      leds.push({ i: z.start, ...pos, r: /scroll|wheel/.test(name) ? 0.42 : 0.6, pill: /scroll|wheel/.test(name) });
    } else {
      for (let i = 0; i < z.count; i++) leds.push({ i: z.start + i, ...outline((i + 0.5) / z.count), r: 0.28 });
    }
  }
  return {
    w, h, leds,
    decor(ctx, pal) {
      ctx.fillStyle = pal.body;
      path(ctx);
      ctx.fill();
      ctx.strokeStyle = pal.edge;
      ctx.lineWidth = 0.06;
      ctx.stroke();
      ctx.strokeStyle = pal.detail;
      ctx.lineWidth = 0.07;
      ctx.beginPath();
      ctx.moveTo(3, 0.3);
      ctx.lineTo(3, 3.4);
      ctx.stroke();
    },
  };
}

function genericLayout(dev) {
  // Rings side by side on one row, strips and dots stacked below.
  const rings = dev.zones.filter((z) => z.kind !== 'matrix' && z.count >= 6 && (RING.test(z.name) || dev.kind === 'cooler'));
  const rest = dev.zones.filter((z) => !rings.includes(z));
  const leds = [];
  const decor = [];
  let w = 0, y = 0;
  if (rings.length) {
    const R = 2.4;
    let x = 0;
    for (const z of rings) {
      const r = z.count >= 14 && /pump/i.test(z.name) ? R * 0.8 : R;
      const cx = x + R * 1.25, cy = R * 1.25;
      decor.push(ringDecor(cx, cy, r, !/pump/i.test(z.name)));
      leds.push(...ringLeds(z, cx, cy, r, Math.min(0.36, (r * Math.PI) / z.count * 0.55)));
      x += R * 2.6;
    }
    w = x;
    y = R * 2.6;
  }
  const longest = Math.max(10, ...rest.map((z) => z.count * 0.9));
  w = Math.max(w, Math.min(longest, 26));
  for (const z of rest) {
    if (z.kind === 'matrix' && z.matrix) {
      const sub = keyboardLayout(dev, z);
      const s = Math.min(w / sub.w, 1);
      sub.leds.forEach((l) => leds.push({ ...l, x: l.x * s, y: y + l.y * s, w: l.w * s, h: l.h * s }));
      y += sub.h * s + 0.6;
    } else if (z.count === 1) {
      leds.push({ i: z.start, x: w / 2, y: y + 0.8, r: 0.6 });
      y += 1.8;
    } else {
      const x0 = 0.6, x1 = w - 0.6;
      const yy = y + 0.6;
      decor.push((ctx, pal) => {
        ctx.fillStyle = pal.body;
        roundRect(ctx, x0 - 0.5, yy - 0.5, x1 - x0 + 1, 1, 0.5);
        ctx.fill();
      });
      leds.push(...stripLeds(z, x0, x1, yy, Math.min(0.32, ((x1 - x0) / z.count) * 0.4)));
      y += 1.6;
    }
  }
  return { w: Math.max(w, 1), h: Math.max(y, 1), leds, decor: (ctx, pal) => decor.forEach((d) => d(ctx, pal)) };
}

export function layoutFor(dev) {
  const matrix = dev.zones.find((z) => z.kind === 'matrix' && z.matrix);
  if (matrix && dev.zones.length === 1) return keyboardLayout(dev, matrix);
  if (dev.kind === 'ram') return ramLayout(dev);
  if (dev.kind === 'gpu' && dev.zones.every((z) => z.kind !== 'matrix' && !RING.test(z.name))) return gpuLayout(dev);
  if (dev.kind === 'mouse') return mouseLayout(dev);
  return genericLayout(dev);
}

// --- View -------------------------------------------------------------------

function palette() {
  const css = getComputedStyle(document.documentElement);
  const v = (n) => css.getPropertyValue(n).trim();
  return { body: v('--device-body'), edge: v('--device-edge'), detail: v('--device-detail'), contact: v('--device-contact'), off: v('--led-off') };
}

export class DeviceView {
  constructor(canvas, device, { padding = 0.08, bloom = 1 } = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.device = device;
    this.layout = layoutFor(device);
    this.padding = padding;
    this.bloom = bloom;
    this.glow = document.createElement('canvas');
    this.colors = new Array(device.led_count).fill([0, 0, 0]);
    this.pal = palette();
    this.resize();
  }

  refreshTheme() {
    this.pal = palette();
    this.draw();
  }

  resize() {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const { width, height } = this.canvas.getBoundingClientRect();
    if (!width || !height) return;
    this.canvas.width = Math.round(width * dpr);
    this.canvas.height = Math.round(height * dpr);
    const { w, h } = this.layout;
    const pw = this.canvas.width * (1 - this.padding * 2), ph = this.canvas.height * (1 - this.padding * 2);
    this.scale = Math.min(pw / w, ph / h);
    this.ox = (this.canvas.width - w * this.scale) / 2;
    this.oy = (this.canvas.height - h * this.scale) / 2;
    this.glow.width = Math.max(1, Math.round(this.canvas.width / 10));
    this.glow.height = Math.max(1, Math.round(this.canvas.height / 10));
    this.draw();
  }

  update(colors) {
    this.colors = colors;
    this.draw();
  }

  /** Average color of the device right now, for ambient tinting. */
  average() {
    let r = 0, g = 0, b = 0;
    for (const c of this.colors) { r += c[0]; g += c[1]; b += c[2]; }
    const n = Math.max(this.colors.length, 1);
    return [r / n, g / n, b / n];
  }

  drawLeds(ctx, keyAlpha) {
    for (const l of this.layout.leds) {
      const c = this.colors[l.i] || [0, 0, 0];
      const lum = Math.max(c[0], c[1], c[2]);
      const fill = `rgb(${c[0]},${c[1]},${c[2]})`;
      if (l.key) {
        ctx.fillStyle = this.pal.detail;
        roundRect(ctx, l.x, l.y, l.w, l.h, 0.16);
        ctx.fill();
        if (lum > 2) {
          ctx.globalAlpha = keyAlpha;
          ctx.fillStyle = fill;
          roundRect(ctx, l.x + 0.08, l.y + 0.08, l.w - 0.16, l.h - 0.16, 0.12);
          ctx.fill();
          ctx.globalAlpha = 1;
        }
      } else if (l.pill) {
        ctx.fillStyle = lum > 2 ? fill : this.pal.off;
        roundRect(ctx, l.x - l.r * 0.6, l.y - l.r * 1.4, l.r * 1.2, l.r * 2.8, l.r * 0.6);
        ctx.fill();
      } else {
        ctx.fillStyle = lum > 2 ? fill : this.pal.off;
        ctx.beginPath();
        ctx.arc(l.x, l.y, l.r, 0, Math.PI * 2);
        ctx.fill();
      }
    }
  }

  draw() {
    const { ctx, canvas, layout } = this;
    if (!canvas.width) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);

    // Bloom: render the LEDs tiny, then stretch the result back up. Bilinear
    // upscaling is a cheap, good-looking blur on every webview.
    if (this.bloom > 0) {
      const g = this.glow.getContext('2d');
      g.setTransform(1, 0, 0, 1, 0, 0);
      g.clearRect(0, 0, this.glow.width, this.glow.height);
      const k = this.glow.width / canvas.width;
      g.setTransform(this.scale * k, 0, 0, this.scale * k, this.ox * k, this.oy * k);
      this.drawLeds(g, 1);
      ctx.save();
      ctx.globalCompositeOperation = 'lighter';
      ctx.globalAlpha = 0.9 * this.bloom;
      ctx.imageSmoothingEnabled = true;
      ctx.imageSmoothingQuality = 'high';
      const spread = canvas.width * 0.04;
      ctx.drawImage(this.glow, -spread, -spread, canvas.width + spread * 2, canvas.height + spread * 2);
      ctx.restore();
    }

    ctx.setTransform(this.scale, 0, 0, this.scale, this.ox, this.oy);
    layout.decor(ctx, this.pal);
    // Keys glow onto the gaps around them: a second, softer bloom over the body.
    if (this.bloom > 0 && layout.leds[0]?.key) {
      ctx.save();
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.globalCompositeOperation = 'lighter';
      ctx.globalAlpha = 0.35 * this.bloom;
      ctx.drawImage(this.glow, 0, 0, canvas.width, canvas.height);
      ctx.restore();
      ctx.setTransform(this.scale, 0, 0, this.scale, this.ox, this.oy);
    }
    if (layout.bar) {
      ctx.fillStyle = this.pal.detail;
      roundRect(ctx, layout.bar.x, layout.bar.y, layout.bar.w, layout.bar.h, layout.bar.h / 2);
      ctx.fill();
    }
    this.drawLeds(ctx, 0.92);
  }
}
