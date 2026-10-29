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

## Sora typeface

- License: SIL Open Font License 1.1 (`ui/fonts/OFL.txt`)
