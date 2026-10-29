# Third-party software in Lumora

Lumora itself is MIT-licensed. The Windows build also ships these programs,
unmodified, as separate executables that Lumora starts and talks to.

## OpenRGB

The hardware engine that detects and controls RGB devices.

- License: GNU General Public License v2.0 (included next to the binaries as `LICENSE`)
- Project and complete source code: https://codeberg.org/OpenRGB/OpenRGB
- The exact version bundled is in `engine/VERSION`, and its source is published
  at https://codeberg.org/OpenRGB/OpenRGB/releases under the matching
  `release_<version>` tag. A copy of that source is also attached to every
  Lumora release on GitHub.

## PawnIO

Signed driver installer, run only if you turn on RAM & motherboard lighting.

- Project, license and source code: https://pawnio.eu and https://github.com/namazso/PawnIO.Setup

## UsbDk

Signed driver installer from Red Hat / Daynix, run only if you set up a Corsair Hydro Pro cooler (one Windows permission prompt). It lets Lumora reach the cooler without replacing its driver, so iCUE keeps working.

- Project, license (Apache-2.0) and source code: https://github.com/daynix/UsbDk

## libusb

Built into Lumora for talking to USB coolers. LGPL-2.1-or-later. Lumora's complete source is public, so you can rebuild it against your own copy of libusb.

- Project and source code: https://libusb.info and https://github.com/libusb/libusb

## Cooler protocol research

Lumora's cooler drivers are its own code (MIT). The USB message layouts they use were learned from these open projects, whose authors documented the devices; no code was copied from them:

- liquidctl (GPL-3.0): https://github.com/liquidctl/liquidctl
- Linux `nzxt-kraken3` hwmon driver (GPL-2.0): https://github.com/torvalds/linux/blob/master/drivers/hwmon/nzxt-kraken3.c

## Sora typeface

- License: SIL Open Font License 1.1 (`ui/fonts/OFL.txt`)
