// The "last 30 minutes" temperature chart on the cooler page. Lumora keeps
// the history itself (a reading every 5 seconds), so the chart is full as
// soon as the page opens.

const SERIES = [
  // Colors checked for color-blind separation against the dark card surface.
  { key: 'liquid', name: 'Liquid', color: '#8b6cff' },
  { key: 'cpu', name: 'CPU', color: '#c9741a' },
  { key: 'gpu', name: 'GPU', color: '#1fa98f' },
];
const SPAN = 30 * 60 * 1000;
const FETCH_EVERY = 5000;
// Readings further apart than this leave a gap in the line.
const GAP = 16000;

let api = null;
let points = [];
let fetchedAt = 0;
let fetching = false;
const hidden = new Set();
let hoverX = null;
let resizeBound = false;

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

export function initTempChart(invoker) {
  api = invoker;
}

/** The card, drawn with whatever history is already here. */
export function tempChartHtml() {
  return `<section class="cool-card temp-chart-card">
    <header><h3>Temperatures <small>last 30 minutes</small></h3><div class="temp-legend" role="group" aria-label="Show or hide a temperature"></div></header>
    <div class="temp-chart" id="tempChart"></div>
  </section>`;
}

/** Fetches new readings (at most every few seconds) and redraws. */
export async function refreshTempChart(force = false) {
  const box = document.getElementById('tempChart');
  if (!box) return;
  if (!resizeBound) {
    resizeBound = true;
    window.addEventListener('resize', () => draw());
  }
  draw();
  if (!api || fetching || (!force && Date.now() - fetchedAt < FETCH_EVERY)) return;
  fetching = true;
  try {
    points = (await api.invoke('temp_history')) || [];
    fetchedAt = Date.now();
  } catch {
    // Keep what we had; the next refresh tries again.
  } finally {
    fetching = false;
  }
  draw();
}

const present = () => SERIES.filter((s) => points.some((p) => p.values?.[s.key] != null));

function latest(key) {
  for (let i = points.length - 1; i >= 0; i--) {
    const v = points[i].values?.[key];
    if (v != null) return v;
  }
  return null;
}

function draw() {
  const box = document.getElementById('tempChart');
  if (!box) return;
  const card = box.closest('.temp-chart-card');
  const series = present();
  const legend = card.querySelector('.temp-legend');
  legend.innerHTML = series.map((s) => {
    const v = latest(s.key);
    return `<button class="temp-key" data-temp-series="${s.key}" aria-pressed="${!hidden.has(s.key)}">
      <i style="background:${s.color}"></i>${s.name}<b>${v == null ? '–' : `${Math.round(v)}°`}</b></button>`;
  }).join('');
  legend.querySelectorAll('[data-temp-series]').forEach((b) => {
    b.onclick = () => {
      const k = b.dataset.tempSeries;
      if (hidden.has(k)) hidden.delete(k);
      else if (series.filter((s) => !hidden.has(s.key)).length > 1) hidden.add(k);
      draw();
    };
  });

  const shown = series.filter((s) => !hidden.has(s.key));
  if (!points.length || !shown.length) {
    box.innerHTML = `<div class="temp-empty">Collecting readings… The chart fills in as Lumora runs.</div>`;
    return;
  }

  const W = Math.max(320, Math.round(box.clientWidth || 800));
  const H = 190;
  const L = 34, R = 44, T = 10, B = 24;
  const now = points[points.length - 1].at;
  const t0 = now - SPAN;
  let lo = Infinity, hi = -Infinity;
  for (const p of points) {
    for (const s of shown) {
      const v = p.values?.[s.key];
      if (v != null) { lo = Math.min(lo, v); hi = Math.max(hi, v); }
    }
  }
  lo = Math.floor((lo - 3) / 5) * 5;
  hi = Math.ceil((hi + 3) / 5) * 5;
  if (hi - lo < 15) hi = lo + 15;
  const x = (t) => L + ((t - t0) / SPAN) * (W - L - R);
  const y = (v) => T + (1 - (v - lo) / (hi - lo)) * (H - T - B);

  const step = (hi - lo) > 40 ? 20 : 10;
  const grid = [];
  for (let v = Math.ceil(lo / step) * step; v <= hi; v += step) {
    grid.push(`<line class="tc-grid" x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}"/><text class="tc-axis" x="${L - 6}" y="${y(v) + 3.5}" text-anchor="end">${v}°</text>`);
  }
  const ticks = [30, 20, 10, 0].map((m) => {
    const tx = x(now - m * 60000);
    return `<text class="tc-axis" x="${tx}" y="${H - 6}" text-anchor="${m === 0 ? 'end' : m === 30 ? 'start' : 'middle'}">${m === 0 ? 'now' : `${m} min ago`}</text>`;
  }).join('');

  const lines = shown.map((s) => {
    let d = '', prev = null;
    for (const p of points) {
      const v = p.values?.[s.key];
      if (v == null || p.at < t0) { prev = null; continue; }
      d += `${prev == null || p.at - prev > GAP ? 'M' : 'L'}${x(p.at).toFixed(1)},${y(v).toFixed(1)}`;
      prev = p.at;
    }
    return `<path class="tc-line" d="${d}" stroke="${s.color}"/>`;
  }).join('');

  // Direct labels at the right edge, nudged apart so they never overlap.
  const ends = shown.map((s) => ({ s, v: latest(s.key) })).filter((e) => e.v != null)
    .map((e) => ({ ...e, ly: y(e.v) })).sort((a, b) => a.ly - b.ly);
  for (let i = 1; i < ends.length; i++) ends[i].ly = Math.max(ends[i].ly, ends[i - 1].ly + 13);
  const labels = ends.map((e) => `<text class="tc-end" x="${W - R + 6}" y="${e.ly + 4}">${Math.round(e.v)}°</text>`).join('');

  let hover = '', tip = '';
  if (hoverX != null && hoverX >= L && hoverX <= W - R) {
    const t = t0 + ((hoverX - L) / (W - L - R)) * SPAN;
    let best = null;
    for (const p of points) if (!best || Math.abs(p.at - t) < Math.abs(best.at - t)) best = p;
    if (best && Math.abs(best.at - t) < GAP) {
      const hx = x(best.at);
      const dots = shown.map((s) => {
        const v = best.values?.[s.key];
        return v == null ? '' : `<circle class="tc-dot" cx="${hx}" cy="${y(v)}" r="4" fill="${s.color}"/>`;
      }).join('');
      hover = `<line class="tc-cross" x1="${hx}" x2="${hx}" y1="${T}" y2="${H - B}"/>${dots}`;
      const ago = Math.round((now - best.at) / 60000);
      const when = new Date(best.at).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
      const rows = shown.map((s) => {
        const v = best.values?.[s.key];
        return v == null ? '' : `<div><i style="background:${s.color}"></i>${s.name}<b>${v.toFixed(1)}°C</b></div>`;
      }).join('');
      const left = hx > W / 2;
      tip = `<div class="tc-tip" style="${left ? `right:${W - hx + 10}px` : `left:${hx + 10}px`}"><span>${when}${ago ? ` · ${ago} min ago` : ''}</span>${rows}</div>`;
    }
  }

  const summary = shown.map((s) => `${s.name} ${latest(s.key) == null ? 'no reading' : `${Math.round(latest(s.key))} °C now`}`).join(', ');
  box.innerHTML = `<svg viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" aria-label="${esc(`Temperatures over the last 30 minutes. ${summary}.`)}">
    ${grid.join('')}${ticks}${lines}${labels}${hover}
  </svg>${tip}`;
  // Handlers live on the box, which survives redraws.
  box.onpointermove = (ev) => {
    hoverX = ev.clientX - box.getBoundingClientRect().left;
    draw();
  };
  box.onpointerleave = () => {
    hoverX = null;
    draw();
  };
}
