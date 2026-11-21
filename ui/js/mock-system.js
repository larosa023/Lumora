// Pretend readings for the System pages in the browser preview: a Ryzen 9
// 7900X with an RTX 4090, 32 GB of DDR5, two NVMe drives and some apps.

export function systemMock(emit, getSettings) {
  const watching = new Set();
  const t0 = Date.now();
  let tick = 0;
  const cores = Array.from({ length: 24 }, () => 5);
  let ram = 13.4;
  const wave = (p, a = 1) => (Math.sin(Date.now() / p) + 1) / 2 * a;

  function sample() {
    tick++;
    const load = 0.25 + wave(13000, 0.55);
    for (let i = 0; i < cores.length; i++) {
      cores[i] = Math.max(0, Math.min(100, cores[i] * 0.6 + (Math.random() * 40 + load * 50) * (i % 2 ? 0.5 : 1) * 0.4));
    }
    const usage = cores.reduce((a, b) => a + b, 0) / cores.length;
    ram = Math.max(11, Math.min(20, ram + (Math.random() - 0.5) * 0.15));
    const gpuUse = Math.round(load * 92);
    return {
      at: Date.now(),
      cpu: {
        name: 'AMD Ryzen 9 7900X 12-Core Processor', usage, cores: [...cores],
        clock_mhz: Math.round(4700 + load * 650), base_mhz: 4700, temp: 48 + load * 30, power: 45 + load * 110,
        processes: 287, threads: 4120, handles: 141233, uptime_s: 3 * 3600 + 1234 + Math.round((Date.now() - t0) / 1000),
      },
      memory: {
        total: 32 * 1073741824, used: ram * 1073741824, available: (32 - ram) * 1073741824, percent: (ram / 32) * 100,
        committed: (ram + 5) * 1073741824, commit_limit: 38 * 1073741824, cached: 9.6 * 1073741824,
      },
      gpu: {
        name: 'NVIDIA GeForce RTX 4090', usage: gpuUse, temp: 41 + load * 28, vram_used: (3.2 + load * 9) * 1073741824, vram_total: 24 * 1073741824,
        core_mhz: Math.round(1200 + load * 1500), mem_mhz: 10501, power: 40 + load * 330, fan: Math.round(30 + load * 35), driver: '566.36',
      },
      net: [{ name: 'Ethernet', down: (wave(4000) * 6 + 0.1) * 1048576, up: wave(7000) * 380000 + 9000, link: 2.5e9 }],
      disks: watching.has('storage') ? [
        { name: '0 C:', active: wave(3000, 18), read: wave(3000) * 42e6, write: wave(5000) * 8e6 },
        { name: '1 D:', active: wave(9000, 4), read: wave(9000) * 3e6, write: 0 },
      ] : [],
    };
  }

  const minmax = new Map();
  function sensors(s) {
    const out = [];
    const add = (group, name, value, unit) => {
      if (value == null) return;
      const k = group + name;
      const m = minmax.get(k) || { min: value, max: value };
      m.min = Math.min(m.min, value); m.max = Math.max(m.max, value);
      minmax.set(k, m);
      out.push({ group, name, value, unit, ...m });
    };
    const c = s.cpu.name;
    add(c, 'Temperature', s.cpu.temp, '°C');
    add(c, 'Package power', s.cpu.power, 'W');
    add(c, 'Usage', s.cpu.usage, '%');
    add(c, 'Clock', s.cpu.clock_mhz, 'MHz');
    const g = s.gpu.name;
    add(g, 'Temperature', s.gpu.temp, '°C');
    add(g, 'Usage', s.gpu.usage, '%');
    add(g, 'Power', s.gpu.power, 'W');
    add(g, 'Core clock', s.gpu.core_mhz, 'MHz');
    add(g, 'Memory clock', s.gpu.mem_mhz, 'MHz');
    add(g, 'Fan', s.gpu.fan, '%');
    add('Memory', 'Used', s.memory.used / 1073741824, 'GB');
    add('Memory', 'Load', s.memory.percent, '%');
    add('NZXT Kraken Elite', 'Liquid', 31 + wave(20000, 4), '°C');
    add('NZXT Kraken Elite', 'Pump', 2100 + wave(20000, 200), 'rpm');
    add('Samsung SSD 990 PRO 2TB', 'Temperature', 38 + wave(30000, 6), '°C');
    add('Samsung SSD 990 PRO 2TB', 'Life left', 98, '%');
    add('Network', 'Ethernet download', s.net[0].down / 1048576, 'MB/s');
    add('Network', 'Ethernet upload', s.net[0].up / 1048576, 'MB/s');
    return out;
  }

  let latest = sample();
  const live = () => [...watching].some((w) => w !== 'processes');
  setInterval(() => {
    latest = sample();
    if (live() && !document.hidden) emit('lumora://system', { sample: latest, sensors: sensors(latest) });
  }, 1000);
  setInterval(() => {
    if (watching.has('processes') && !document.hidden) emit('lumora://processes', processes());
  }, 2000);

  const APPS = [
    ['chrome.exe', 'Google Chrome', 'app', 14, 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe'],
    ['Discord.exe', 'Discord', 'app', 6, 'C:\\Users\\Anthony\\AppData\\Local\\Discord\\app-1.0.9170\\Discord.exe'],
    ['steam.exe', 'Steam', 'app', 1, 'C:\\Program Files (x86)\\Steam\\steam.exe'],
    ['cs2.exe', 'Counter-Strike 2', 'app', 1, 'D:\\SteamLibrary\\steamapps\\common\\Counter-Strike Global Offensive\\game\\bin\\win64\\cs2.exe'],
    ['Lumora.exe', 'Lumora', 'app', 1, 'C:\\Program Files\\Lumora\\Lumora.exe'],
    ['explorer.exe', 'Windows Explorer', 'app', 1, 'C:\\Windows\\explorer.exe'],
    ['steamwebhelper.exe', 'Steam Client WebHelper', 'background', 7, 'C:\\Program Files (x86)\\Steam\\bin\\cef\\cef.win7x64\\steamwebhelper.exe'],
    ['ArmouryCrate.Service.exe', 'Armoury Crate Service', 'background', 1, 'C:\\Program Files\\ASUS\\Armoury Crate Service\\ArmouryCrate.Service.exe'],
    ['NVDisplay.Container.exe', 'NVIDIA Container', 'background', 2, 'C:\\Windows\\System32\\DriverStore\\FileRepository\\nv_dispi.inf_amd64\\Display.NvContainer\\NVDisplay.Container.exe'],
    ['OneDrive.exe', 'Microsoft OneDrive', 'background', 1, 'C:\\Users\\Anthony\\AppData\\Local\\Microsoft\\OneDrive\\OneDrive.exe'],
    ['lumora-sensors.exe', 'Lumora sensors', 'background', 1, 'C:\\Program Files\\Lumora\\lumora-sensors.exe'],
    ['svchost.exe', 'Service Host', 'windows', 82, 'C:\\Windows\\System32\\svchost.exe'],
    ['dwm.exe', 'Desktop Window Manager', 'windows', 1, 'C:\\Windows\\System32\\dwm.exe'],
    ['csrss.exe', 'Client Server Runtime Process', 'windows', 2, null],
    ['System', 'System', 'windows', 1, null],
    ['MsMpEng.exe', 'Antimalware Service Executable', 'windows', 1, 'C:\\ProgramData\\Microsoft\\Windows Defender\\Platform\\MsMpEng.exe'],
  ];
  function processes() {
    const rows = [];
    let pid = 400;
    for (const [name, description, group, n, path] of APPS) {
      const parent = pid;
      for (let i = 0; i < n; i++) {
        const busy = name === 'cs2.exe' ? 9 + wave(5000, 6) : name === 'chrome.exe' ? wave(3000 + i * 400, 1.4) : name === 'dwm.exe' ? 1.2 : Math.random() < 0.15 ? Math.random() * 0.8 : 0;
        rows.push({
          pid: (pid += 4 + i), parent: i ? parent : 4, name, description, path, group: i && group === 'app' ? 'background' : group,
          cpu: busy, memory: (name === 'cs2.exe' ? 2.3e9 : name === 'chrome.exe' ? 9e7 + i * 2e7 : 2e7 + (name.length * 1e6)),
          disk: name === 'cs2.exe' ? wave(4000) * 2e6 : 0, gpu: name === 'cs2.exe' ? 70 + wave(5000, 20) : name === 'dwm.exe' ? 3 : 0,
          threads: 12 + i, status: name === 'OneDrive.exe' ? 'efficiency' : 'running',
        });
      }
    }
    return rows;
  }

  let plans = [
    { id: '381b4222-f694-41f0-9685-ff5bb260df2e', name: 'Balanced', active: true },
    { id: '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c', name: 'High performance', active: false },
    { id: 'e9a42b02-d5df-448d-aa00-03f14749eb61', name: 'Ultimate Performance', active: false },
  ];
  const startup = [
    ['hkcu|Discord', 'Discord', 'Discord', 'C:\\Users\\Anthony\\AppData\\Local\\Discord\\Update.exe --processStart Discord.exe', 'Registry (you)', true, false],
    ['hkcu|Steam', 'Steam', 'Steam', '"C:\\Program Files (x86)\\Steam\\steam.exe" -silent', 'Registry (you)', true, false],
    ['hklm|ArmouryCrate', 'ArmouryCrate', 'Armoury Crate', '"C:\\Program Files\\ASUS\\ArmouryCrate\\ArmouryCrate.exe"', 'Registry (all users)', false, true],
    ['hkcu|OneDrive', 'OneDrive', 'Microsoft OneDrive', '"C:\\Users\\Anthony\\AppData\\Local\\Microsoft\\OneDrive\\OneDrive.exe" /background', 'Registry (you)', true, false],
    ['hklm|NZXT CAM', 'NZXT CAM', 'NZXT CAM', '"C:\\Program Files\\NZXT CAM\\NZXT CAM.exe" --startup', 'Registry (all users)', false, true],
    ['folder|Spotify.lnk', 'Spotify', 'Spotify', 'C:\\Users\\Anthony\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\Spotify.lnk', 'Startup folder (you)', false, false],
  ].map(([id, name, description, command, location, enabled, admin]) => ({ id, name, description, command, path: command.replace(/^"([^"]+)".*$/, '$1').split(' --')[0], location, enabled, admin }));
  const services = [
    ['AudioSrv', 'Windows Audio', 'Manages audio for Windows-based programs.', 'running', 'auto'],
    ['BITS', 'Background Intelligent Transfer Service', 'Transfers files in the background using idle network bandwidth.', 'stopped', 'manual'],
    ['Dnscache', 'DNS Client', 'Caches Domain Name System (DNS) names.', 'running', 'auto'],
    ['ArmouryCrateService', 'ARMOURY CRATE Service', 'ASUS ARMOURY CRATE Service', 'running', 'auto'],
    ['LightingService', 'LightingService', 'ASUS AURA lighting service', 'running', 'auto'],
    ['NVDisplay.ContainerLocalSystem', 'NVIDIA Display Container LS', 'Container service for NVIDIA root features', 'running', 'auto'],
    ['Spooler', 'Print Spooler', 'Spools print jobs and handles interaction with the printer.', 'running', 'auto'],
    ['SysMain', 'SysMain', 'Maintains and improves system performance over time.', 'running', 'auto'],
    ['WSearch', 'Windows Search', 'Provides content indexing, property caching, and search results.', 'running', 'delayed'],
    ['wuauserv', 'Windows Update', 'Enables the detection, download, and installation of updates.', 'stopped', 'manual'],
    ['XblAuthManager', 'Xbox Live Auth Manager', 'Provides authentication and authorization services.', 'stopped', 'manual'],
    ['Fax', 'Fax', 'Enables you to send and receive faxes.', 'stopped', 'disabled'],
  ].map(([name, display, description, state, start], i) => ({ name, display, description, state, start, pid: state === 'running' ? 1000 + i * 8 : 0, can_stop: true }));
  const helper = location.search.includes('helper');

  const specs = {
    os: 'Windows 11 Pro 24H2 (build 26100)', cpu: 'AMD Ryzen 9 7900X 12-Core Processor', cores: 12, threads: 24,
    board: 'ASUSTeK COMPUTER INC. TUF GAMING B650E-PLUS WIFI', bios: 'American Megatrends Inc. 3287 (2025-03-10)', ram_total: 32 * 1073741824,
    ram: [{ slot: 'DIMM_A2', size: 16 * 1073741824, kind: 'DDR5', speed: 6000, configured: 6000, maker: 'G Skill', part: 'F5-6000J3038F16G' },
      { slot: 'DIMM_B2', size: 16 * 1073741824, kind: 'DDR5', speed: 6000, configured: 6000, maker: 'G Skill', part: 'F5-6000J3038F16G' }],
    gpu: 'NVIDIA GeForce RTX 4090', gpu_driver: '566.36', gpu_vram: 24 * 1073741824,
    drives: ['Samsung SSD 990 PRO 2TB (NVMe, 2000 GB)', 'WD_BLACK SN850X 2000GB (NVMe, 2000 GB)'],
  };

  return {
    monitor_watch: ({ what, on }) => {
      if (on) watching.add(what); else watching.delete(what);
      if (on && what === 'processes') setTimeout(() => emit('lumora://processes', processes()), 200);
    },
    system_snapshot: () => ({ sample: latest, sensors: sensors(latest) }),
    process_action: () => {},
    startup_list: () => structuredClone(startup),
    startup_set: ({ id, on }) => {
      const i = startup.find((x) => x.id === id);
      if (i.admin && !helper) throw 'needs-helper';
      i.enabled = on;
    },
    services_list: () => structuredClone(services),
    service_action: ({ name, action }) => {
      if (!helper) throw 'needs-helper';
      const s = services.find((x) => x.name === name);
      s.state = action === 'stop' ? 'stopped' : 'running';
    },
    service_start_type: ({ name, start }) => {
      if (!helper) throw 'needs-helper';
      services.find((x) => x.name === name).start = start;
    },
    storage_info: () => ({
      volumes: [
        { letter: 'C:', label: 'Windows', filesystem: 'NTFS', total: 2e12, free: 0.81e12, kind: 'fixed', disk: 0 },
        { letter: 'D:', label: 'Games', filesystem: 'NTFS', total: 2e12, free: 0.12e12, kind: 'fixed', disk: 1 },
        { letter: 'E:', label: 'USB', filesystem: 'exFAT', total: 64e9, free: 40e9, kind: 'removable', disk: 2 },
      ],
      disks: helper ? [
        { number: 0, model: 'Samsung SSD 990 PRO 2TB', bus: 'NVMe', size: 2e12, temp: 41, life_left: 98, health: 'good', power_on_hours: 3120, written: 28.4e12 },
        { number: 1, model: 'WD_BLACK SN850X 2000GB', bus: 'NVMe', size: 2e12, temp: 38, life_left: 95, health: 'good', power_on_hours: 6210, written: 61e12 },
      ] : [],
      health_error: helper ? null : 'needs-helper',
    }),
    specs: () => ({ specs, text: `Windows: ${specs.os}\nCPU: ${specs.cpu}\n…` }),
    power_plans: () => structuredClone(plans),
    set_power_plan: ({ id }) => {
      plans = plans.map((p) => ({ ...p, active: p.id === id }));
      return structuredClone(plans);
    },
    free_memory: async () => {
      await new Promise((r) => setTimeout(r, 600));
      if (!helper) throw 'needs-helper';
      return { before: 18e9, after: 24.3e9 };
    },
    history_range: ({ minutes }) => {
      const now = Date.now();
      const n = Math.min(720, minutes * 12);
      const step = (minutes * 60000) / n;
      return Array.from({ length: n }, (_, i) => {
        const at = now - (n - 1 - i) * step;
        const load = Math.max(0, Math.sin((i / n) * Math.PI * 2.2));
        const r = (k) => Math.sin(i * 0.37 + k) * 0.8;
        return { at, v: [31 + load * 4 + r(1) * 0.3, 47 + load * 25 + r(2) * 3, 40 + load * 30 + r(3) * 2, 8 + load * 40 + r(4) * 4, load * 90, 40 + load * 12, 45 + load * 100, 40 + load * 320, (1 + load * 5) * 1048576, 200000 + load * 100000] };
      });
    },
    history_export: () => 'C:\\Users\\Anthony\\Downloads\\Lumora history 2026-11-21 10-04-00.csv',
    overlay_toggle: ({ show }) => {
      const s = getSettings();
      s.overlay = { ...s.overlay, shown: show ?? !s.overlay.shown };
      return { shown: s.overlay.shown, locked: s.overlay.locked };
    },
    overlay_lock: ({ locked }) => {
      const s = getSettings();
      s.overlay = { ...s.overlay, locked };
      return { shown: s.overlay.shown, locked };
    },
  };
}
