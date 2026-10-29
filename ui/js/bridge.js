// One small surface between the UI and the backend: the Rust engine inside the
// app, or a JavaScript stand-in when the UI is opened in a normal browser.

const tauri = window.__TAURI__;

async function load() {
  if (tauri) {
    const win = tauri.window.getCurrentWindow();
    return {
      inApp: true,
      invoke: tauri.core.invoke,
      listen: tauri.event.listen,
      window: {
        minimize: () => win.minimize(),
        toggleMaximize: () => win.toggleMaximize(),
        close: () => win.close(),
        startDragging: () => win.startDragging(),
      },
    };
  }
  const { mock } = await import('./mock.js');
  return { inApp: false, ...mock };
}

export const api = await load();

/** Decodes a base64 `r g b r g b …` frame into an array of [r, g, b]. */
export function decodeFrame(b64) {
  const bin = atob(b64);
  const out = new Array(bin.length / 3);
  for (let i = 0, j = 0; i < bin.length; i += 3, j++) {
    out[j] = [bin.charCodeAt(i), bin.charCodeAt(i + 1), bin.charCodeAt(i + 2)];
  }
  return out;
}
