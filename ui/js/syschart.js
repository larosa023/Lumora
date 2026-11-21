// Line charts for the System pages: a few series over a time window, a
// crosshair with a tooltip on hover, and direct labels at the right edge.
// Drawn as one SVG string; a redraw is a single innerHTML swap.

// The same three hues as the cooler chart (checked for color-blind
// separation on the dark card): CPU orange, GPU teal, the third purple.
export const COLORS = { cpu: '#c9741a', gpu: '#1fa98f', third: '#8b6cff' };

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

/**
 * box: the element to draw in (keeps its hover state between draws).
 * opts: { series: [{key, name, color}], points: [{at, values}], span (ms),
 *   now (ms), height, min, max (fixed axis, else fitted), fmt(v) for labels,
 *   axis(v) for the y axis, gap (ms: further apart = a break), label (aria) }
 */
export function drawChart(box, opts) {
  if (!box) return;
  box._opts = opts;
  render(box);
  if (!box._bound) {
    box._bound = true;
    box.addEventListener('pointermove', (ev) => {
      box._hover = ev.clientX - box.getBoundingClientRect().left;
      render(box);
    });
    box.addEventListener('pointerleave', () => {
      box._hover = null;
      render(box);
    });
  }
}

function render(box) {
  const o = box._opts;
  const { series, points } = o;
  const fmt = o.fmt || ((v) => Math.round(v));
  const axis = o.axis || fmt;
  const W = Math.max(240, Math.round(box.clientWidth || 600));
  const H = o.height || 170;
  const L = o.left ?? 40, R = o.right ?? 48, T = 10, B = 22;
  const now = o.now || (points.length ? points[points.length - 1].at : Date.now());
  const span = o.span;
  const t0 = now - span;
  const gap = o.gap || Math.max(16000, span / 60);
  let lo = o.min, hi = o.max;
  if (lo == null || hi == null) {
    let a = Infinity, b = -Infinity;
    for (const p of points) {
      if (p.at < t0) continue;
      for (const s of series) {
        const v = p.values[s.key];
        if (v != null) { a = Math.min(a, v); b = Math.max(b, v); }
      }
    }
    if (!isFinite(a)) { a = 0; b = 1; }
    if (lo == null) lo = o.zero ? 0 : niceFloor(a - (b - a) * 0.1);
    if (hi == null) hi = niceCeil(b + (b - a) * 0.1 || 1);
    if (hi - lo < (o.minRange || 1)) hi = lo + (o.minRange || 1);
  }
  const x = (t) => L + ((t - t0) / span) * (W - L - R);
  const y = (v) => T + (1 - (Math.min(hi, Math.max(lo, v)) - lo) / (hi - lo)) * (H - T - B);

  let grid = '';
  for (let i = 0; i <= 3; i++) {
    const v = lo + ((hi - lo) * i) / 3;
    grid += `<line class="tc-grid" x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}"/><text class="tc-axis" x="${L - 6}" y="${y(v) + 3.5}" text-anchor="end">${esc(axis(v))}</text>`;
  }
  const ticks = (o.ticks || []).map(([ms, text], i, all) => {
    const tx = x(now - ms);
    const anchor = i === 0 ? 'start' : i === all.length - 1 ? 'end' : 'middle';
    return `<text class="tc-axis" x="${tx}" y="${H - 5}" text-anchor="${anchor}">${esc(text)}</text>`;
  }).join('');

  const latest = (key) => {
    for (let i = points.length - 1; i >= 0; i--) if (points[i].values[key] != null) return points[i].values[key];
    return null;
  };
  const lines = series.map((s) => {
    let d = '', prev = null;
    for (const p of points) {
      const v = p.values[s.key];
      if (v == null || p.at < t0 - gap) { prev = null; continue; }
      d += `${prev == null || p.at - prev > gap ? 'M' : 'L'}${x(p.at).toFixed(1)},${y(v).toFixed(1)}`;
      prev = p.at;
    }
    const area = o.area && series.length === 1 && d
      ? `<path class="sc-area" d="${d}L${x(points[points.length - 1].at).toFixed(1)},${H - B}L${x(Math.max(t0, points.find((p) => p.values[s.key] != null)?.at ?? t0)).toFixed(1)},${H - B}Z" fill="${s.color}"/>`
      : '';
    return `${area}<path class="tc-line" d="${d}" stroke="${s.color}"/>`;
  }).join('');

  const ends = series.map((s) => ({ s, v: latest(s.key) })).filter((e) => e.v != null)
    .map((e) => ({ ...e, ly: y(e.v) })).sort((a, b) => a.ly - b.ly);
  for (let i = 1; i < ends.length; i++) ends[i].ly = Math.max(ends[i].ly, ends[i - 1].ly + 13);
  const labels = ends.map((e) => `<text class="tc-end" x="${W - R + 6}" y="${e.ly + 4}">${esc(fmt(e.v))}</text>`).join('');

  let hover = '', tip = '';
  const hx0 = box._hover;
  if (hx0 != null && hx0 >= L && hx0 <= W - R && points.length) {
    const t = t0 + ((hx0 - L) / (W - L - R)) * span;
    let best = null;
    for (const p of points) if (!best || Math.abs(p.at - t) < Math.abs(best.at - t)) best = p;
    if (best && Math.abs(best.at - t) < gap) {
      const hx = x(best.at);
      const dots = series.map((s) => {
        const v = best.values[s.key];
        return v == null ? '' : `<circle class="tc-dot" cx="${hx}" cy="${y(v)}" r="4" fill="${s.color}"/>`;
      }).join('');
      hover = `<line class="tc-cross" x1="${hx}" x2="${hx}" y1="${T}" y2="${H - B}"/>${dots}`;
      const when = new Date(best.at).toLocaleTimeString([], span > 6 * 3600e3 ? { hour: 'numeric', minute: '2-digit' } : { hour: 'numeric', minute: '2-digit', second: span <= 600e3 ? '2-digit' : undefined });
      const rows = series.map((s) => {
        const v = best.values[s.key];
        return v == null ? '' : `<div><i style="background:${s.color}"></i>${esc(s.name)}<b>${esc((o.tipFmt || fmt)(v))}</b></div>`;
      }).join('');
      const left = hx > W / 2;
      tip = `<div class="tc-tip" style="${left ? `right:${W - hx + 10}px` : `left:${hx + 10}px`}"><span>${esc(when)}</span>${rows}</div>`;
    }
  }
  const clip = box._clip || (box._clip = `sc${Math.random().toString(36).slice(2, 8)}`);
  const summary = ends.map((e) => `${e.s.name} ${fmt(e.v)}`).join(', ');
  box.innerHTML = `<svg viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" aria-label="${esc(`${o.label || 'Chart'}. ${summary}`)}"><defs><clipPath id="${clip}"><rect x="${L}" y="0" width="${W - L - R}" height="${H - B}"/></clipPath></defs>${grid}${ticks}<g clip-path="url(#${clip})">${lines}</g>${labels}${hover}</svg>${tip}`;
}

function niceFloor(v) {
  const step = magnitude(v);
  return Math.floor(v / step) * step;
}
function niceCeil(v) {
  const step = magnitude(v);
  return Math.ceil(v / step) * step;
}
function magnitude(v) {
  const a = Math.abs(v) || 1;
  const m = 10 ** Math.floor(Math.log10(a));
  return a / m >= 5 ? m : m / 2;
}

/** A tiny no-axis sparkline path over a fixed window (for the gauges). */
export function sparkPath(points, key, span, now, w, h, max = 100) {
  let d = '', prev = null;
  const t0 = now - span;
  for (const p of points) {
    const v = p.values[key];
    if (v == null || p.at < t0 - 6000) { prev = null; continue; }
    const px = ((p.at - t0) / span) * w;
    const py = h - 1 - (Math.min(max, Math.max(0, v)) / max) * (h - 2);
    d += `${prev == null || p.at - prev > 12000 ? 'M' : 'L'}${px.toFixed(1)},${py.toFixed(1)}`;
    prev = p.at;
  }
  return d;
}
