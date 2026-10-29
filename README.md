<p align="center"><img src="assets/logo.svg" width="96" alt=""></p>

<h1 align="center">Lumora</h1>

<p align="center">One lightweight app for all your RGB: RAM, motherboard, GPU, coolers, fans, keyboards, mice and more.</p>

---

Lumora replaces the pile of vendor apps (iCUE, Armoury Crate, Mystic Light, Synapse, G Hub…) with one clean window. Pick an effect, pick your colors, and every device lights up in sync. Or give any single device its own look.

## Features

- **Every device in one place.** Lumora drives hardware through [OpenRGB](https://openrgb.org), which already supports hundreds of RAM, motherboard, GPU, cooler, fan, LED strip, keyboard, mouse and headset controllers.
- **12 effects:** Static, Gradient, Breathing, Spectrum, Aurora, Rainbow, Color Wave, Ripple, Comet, Marquee, Starfield and Fire, each with speed, size and direction controls.
- **Colors your way:** a full color picker, hex entry, curated swatches, recent colors, screen eyedropper (Windows), up to six colors per effect and one-click palettes.
- **Sync or customize:** one effect across your whole rig, or an override for any device.
- **Profiles:** save a lighting setup and switch back to it in one click. Five starter profiles are included.
- **Themes:** Ink, OLED black, Midnight, Daylight and Frost (glass), plus accent colors or an accent that follows your lighting.
- **Live previews:** devices are drawn as glowing objects that mirror your real LEDs, and the window picks up their color.
- **Built-in updater:** Lumora checks for new versions, then downloads, verifies and installs them in one click.
- **Lightweight:** a small native Rust core and the system webview (no bundled Chromium). Static colors cost no CPU at all; animated effects only send LEDs that changed.
- **Runs quietly:** lives in the tray, starts with Windows if you like, and restores your lighting on boot.
- **Demo devices** appear until your hardware is connected, so you can try everything right away.

## Getting started

1. Install [OpenRGB](https://openrgb.org) and open it once so it detects your devices.
2. In OpenRGB, open the **SDK Server** tab and click **Start Server**. (Turn on *Start server on launch* in OpenRGB's settings so it's always ready.)
3. Install Lumora from the [latest release](https://github.com/larosa023/lumora/releases/latest) and open it. It connects on its own.

> Close vendor RGB apps (iCUE, Armoury Crate, etc.) while using Lumora so they don't fight over the same lights.

## Building from source

You need [Rust](https://rustup.rs), [Node.js](https://nodejs.org) 20+, and on Windows the WebView2 runtime (already on Windows 10/11).

```sh
npm install
npm run dev            # run the app
npm run mock           # (separate terminal) fake OpenRGB server with a sample rig
npm run preview        # UI only, in your browser, with demo devices
cargo test -p lumora-core
```

## Releasing an update

Releases are built and published by GitHub Actions, and installed copies of Lumora update themselves from them.

```sh
npm run release -- 0.2.0
```

That bumps the version, tags `v0.2.0` and pushes. About 15 minutes later the installers are on the Releases page and everyone's Lumora offers the update.

Updates are signed. The workflow needs the signing key in the repo's secrets as `TAURI_SIGNING_PRIVATE_KEY` (and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if the key has one). The matching public key lives in `src-tauri/tauri.conf.json`. Keep a backup of the private key: without it you can't ship updates to existing installs. To start over with a new key, run `npx tauri signer generate -w lumora.key`, put the new public key in `tauri.conf.json`, and update the secret.

## How it works

```
crates/lumora-core   Rust engine: OpenRGB SDK client, effects, render loop, profiles
src-tauri            Desktop shell: window, tray, updater, settings on disk
ui                   Interface: plain HTML, CSS and JavaScript (no framework, no bundler)
```

The engine renders every effect from one shared clock, so devices on different buses stay in phase. It sends only frames that changed, idles when nothing animates, and keeps trying to reconnect if OpenRGB restarts.

## License

MIT. The bundled Sora font is under the SIL Open Font License (`ui/fonts/OFL.txt`).
