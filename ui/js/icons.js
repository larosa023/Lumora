// Line icons, 24×24, drawn with the current text color.

const p = (d) => `<svg viewBox="0 0 24 24" aria-hidden="true">${d}</svg>`;

export const ICONS = {
  all: p('<rect x="3" y="3" width="7.5" height="7.5" rx="2"/><rect x="13.5" y="3" width="7.5" height="7.5" rx="2"/><rect x="3" y="13.5" width="7.5" height="7.5" rx="2"/><rect x="13.5" y="13.5" width="7.5" height="7.5" rx="2"/>'),
  keyboard: p('<rect x="2" y="6" width="20" height="12" rx="2.5"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>'),
  keypad: p('<rect x="5" y="3" width="14" height="18" rx="2.5"/><path d="M9 8h.01M12 8h.01M15 8h.01M9 12h.01M12 12h.01M15 12h.01M9 16h6"/>'),
  mouse: p('<rect x="6" y="2.5" width="12" height="19" rx="6"/><path d="M12 6.5v3.5"/>'),
  mousemat: p('<rect x="2.5" y="5" width="19" height="14" rx="3"/><path d="M6 15h4"/>'),
  ram: p('<rect x="2" y="7" width="20" height="9" rx="1.5"/><path d="M5 10h2v3H5zM9 10h2v3H9zM13 10h2v3h-2zM17 10h2v3h-2zM4 16v2M8 16v2M16 16v2M20 16v2"/>'),
  gpu: p('<rect x="2" y="6" width="20" height="11" rx="2"/><circle cx="8" cy="11.5" r="3"/><circle cx="16" cy="11.5" r="3"/><path d="M4 17v2"/>'),
  cooler: p('<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2"/><path d="M12 10c0-3 1-5 3.5-5.5M14 12c3 0 5 1 5.5 3.5M12 14c0 3-1 5-3.5 5.5M10 12c-3 0-5-1-5.5-3.5"/>'),
  motherboard: p('<rect x="3" y="3" width="18" height="18" rx="2"/><rect x="7" y="7" width="6" height="6" rx="1"/><path d="M16 7v2M16 12v5M7 17h5"/>'),
  led_strip: p('<path d="M3 15c3-6 6 6 9 0s6 6 9 0"/><path d="M5 9h.01M12 9h.01M19 9h.01"/>'),
  light: p('<path d="M9 18h6M10 21h4M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2.1h5c0-.9.4-1.6 1-2.1A6 6 0 0 0 12 3z"/>'),
  headset: p('<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4" height="6" rx="1.5"/><rect x="17" y="14" width="4" height="6" rx="1.5"/>'),
  headset_stand: p('<path d="M8 21h8M12 21V8M8 8a4 4 0 0 1 8 0"/>'),
  gamepad: p('<path d="M7 9h4M9 7v4M15 10h.01M18 8h.01"/><path d="M6.5 5h11a4 4 0 0 1 4 4.5l-.7 6A3 3 0 0 1 15.5 17l-1.5-2h-4l-1.5 2a3 3 0 0 1-5.3-1.5l-.7-6A4 4 0 0 1 6.5 5z"/>'),
  speaker: p('<rect x="5" y="2.5" width="14" height="19" rx="2.5"/><circle cx="12" cy="14" r="3.5"/><path d="M12 7h.01"/>'),
  storage: p('<rect x="3" y="6" width="18" height="12" rx="2"/><path d="M7 14h.01M11 14h6"/>'),
  case: p('<rect x="6" y="2.5" width="12" height="19" rx="2"/><circle cx="12" cy="14" r="3"/><path d="M9 6h6"/>'),
  microphone: p('<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>'),
  accessory: p('<path d="M12 3l2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.4 6.8 19.1l1-5.8L3.5 9.2l5.9-.9z"/>'),
  other: p('<circle cx="12" cy="12" r="8"/><path d="M12 8v4M12 16h.01"/>'),
  right: p('<path d="M5 12h14M13 6l6 6-6 6"/>'),
  left: p('<path d="M19 12H5M11 6l-6 6 6 6"/>'),
  up: p('<path d="M12 19V5M6 11l6-6 6 6"/>'),
  down: p('<path d="M12 5v14M6 13l6 6 6-6"/>'),
  out: p('<circle cx="12" cy="12" r="2"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3"/>'),
  in: p('<circle cx="12" cy="12" r="8"/><path d="M12 7v2.5M12 14.5V17M7 12h2.5M14.5 12H17"/>'),
  plus: p('<path d="M12 5v14M5 12h14"/>'),
  trash: p('<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>'),
  save: p('<path d="M5 3h11l3 3v15H5z"/><path d="M8 3v5h7M8 21v-7h8v7"/>'),
  reset: p('<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/>'),
  check: p('<path d="M5 12.5l4.5 4.5L19 7.5"/>'),
};

export const icon = (name) => ICONS[name] || ICONS.other;
