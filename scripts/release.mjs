// Bumps the version everywhere, commits, tags and pushes. GitHub Actions then
// builds the installers and publishes the update.
//   npm run release -- 0.2.0
import { readFileSync, writeFileSync } from 'node:fs';
import { execSync } from 'node:child_process';

const version = process.argv[2];
if (!/^\d+\.\d+\.\d+$/.test(version || '')) {
  console.error('Usage: npm run release -- <major.minor.patch>   e.g. npm run release -- 0.2.0');
  process.exit(1);
}

const edit = (file, fn) => writeFileSync(file, fn(readFileSync(file, 'utf8')));
edit('package.json', (s) => s.replace(/"version": "[^"]+"/, `"version": "${version}"`));
edit('src-tauri/tauri.conf.json', (s) => s.replace(/"version": "[^"]+"/, `"version": "${version}"`));
edit('Cargo.toml', (s) => s.replace(/(\[workspace\.package\]\s*\nversion = )"[^"]+"/, `$1"${version}"`));

const run = (cmd) => execSync(cmd, { stdio: 'inherit' });
run('cargo update -w');
run('git add package.json src-tauri/tauri.conf.json Cargo.toml Cargo.lock');
run(`git commit -m "Release v${version}"`);
run(`git tag v${version}`);
run('git push');
run(`git push origin v${version}`);
console.log(`\nv${version} pushed. Installers and the update appear on GitHub Releases in about 15 minutes.`);
