const { readFileSync, writeFileSync } = require('node:fs');
const { join } = require('node:path');

const projectRoot = join(__dirname, '..');
const tag = process.env.RELEASE_TAG || process.argv[2] || '';
const version = tag.replace(/^v/, '');

if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error(`Expected a semantic version tag such as v0.1.0, received: ${tag}`);
}

function readJson(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

const packagePath = join(projectRoot, 'package.json');
const packageJson = readJson(packagePath);
packageJson.version = version;
writeJson(packagePath, packageJson);

const lockPath = join(projectRoot, 'package-lock.json');
const lockJson = readJson(lockPath);
lockJson.packages ??= {};
lockJson.packages[''] ??= {};
lockJson.packages[''].version = version;
writeJson(lockPath, lockJson);

const tauriPath = join(projectRoot, 'src-tauri', 'tauri.conf.json');
const tauriJson = readJson(tauriPath);
tauriJson.version = version;
writeJson(tauriPath, tauriJson);

console.log(`Building Session-Loom ${version}`);
