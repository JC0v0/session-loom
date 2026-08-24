const { readFileSync, writeFileSync } = require('node:fs');
const { join } = require('node:path');

const projectRoot = join(__dirname, '..');
const args = process.argv.slice(2);
const checkOnly = args.includes('--check');
const suppliedTag = args.find((arg) => !arg.startsWith('--')) || '';
const tag = suppliedTag || process.env.RELEASE_TAG || '';
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

function readText(path) {
  return readFileSync(path, 'utf8');
}

function writeText(path, value) {
  writeFileSync(path, value);
}

function findTomlVersion(text, section, path) {
  const lines = text.split(/\r?\n/);
  let currentSection = '';

  for (const line of lines) {
    const sectionMatch = line.match(/^\s*\[([^\]]+)\]\s*$/);
    if (sectionMatch) {
      currentSection = sectionMatch[1];
    }

    if (currentSection !== section) {
      continue;
    }

    const versionMatch = line.match(/^\s*version\s*=\s*"([^"]+)"/);
    if (versionMatch) {
      return versionMatch[1];
    }
  }

  throw new Error(`Could not find version in [${section}] in ${path}`);
}

function setTomlVersion(text, section, version, path) {
  const newline = text.includes('\r\n') ? '\r\n' : '\n';
  const lines = text.split(/\r?\n/);
  let currentSection = '';
  let replaced = false;

  for (let index = 0; index < lines.length; index += 1) {
    const sectionMatch = lines[index].match(/^\s*\[([^\]]+)\]\s*$/);
    if (sectionMatch) {
      currentSection = sectionMatch[1];
    }

    if (currentSection !== section) {
      continue;
    }

    const versionMatch = lines[index].match(/^(\s*version\s*=\s*")([^"]+)(".*)$/);
    if (versionMatch) {
      lines[index] = `${versionMatch[1]}${version}${versionMatch[3]}`;
      replaced = true;
      break;
    }
  }

  if (!replaced) {
    throw new Error(`Could not update version in [${section}] in ${path}`);
  }

  return lines.join(newline);
}

function findCargoLockVersions(text, packageNames, path) {
  const versions = new Map();
  const lines = text.split(/\r?\n/);
  let packageName = null;

  for (const line of lines) {
    if (line.trim() === '[[package]]') {
      packageName = null;
      continue;
    }

    const nameMatch = line.match(/^\s*name\s*=\s*"([^"]+)"/);
    if (nameMatch) {
      packageName = nameMatch[1];
      continue;
    }

    if (!packageNames.has(packageName) || versions.has(packageName)) {
      continue;
    }

    const versionMatch = line.match(/^\s*version\s*=\s*"([^"]+)"/);
    if (versionMatch) {
      versions.set(packageName, versionMatch[1]);
    }
  }

  for (const packageNameToFind of packageNames) {
    if (!versions.has(packageNameToFind)) {
      throw new Error(`Could not find package ${packageNameToFind} in ${path}`);
    }
  }

  return versions;
}

function setCargoLockVersions(text, packageNames, version, path) {
  const newline = text.includes('\r\n') ? '\r\n' : '\n';
  const lines = text.split(/\r?\n/);
  let packageName = null;
  const updated = new Set();

  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === '[[package]]') {
      packageName = null;
      continue;
    }

    const nameMatch = lines[index].match(/^\s*name\s*=\s*"([^"]+)"/);
    if (nameMatch) {
      packageName = nameMatch[1];
      continue;
    }

    if (!packageNames.has(packageName) || updated.has(packageName)) {
      continue;
    }

    const versionMatch = lines[index].match(/^(\s*version\s*=\s*")([^"]+)(".*)$/);
    if (versionMatch) {
      lines[index] = `${versionMatch[1]}${version}${versionMatch[3]}`;
      updated.add(packageName);
    }
  }

  for (const packageNameToFind of packageNames) {
    if (!updated.has(packageNameToFind)) {
      throw new Error(`Could not update package ${packageNameToFind} in ${path}`);
    }
  }

  return lines.join(newline);
}

const packagePath = join(projectRoot, 'package.json');
const packageJson = readJson(packagePath);

const lockPath = join(projectRoot, 'package-lock.json');
const lockJson = readJson(lockPath);
const lockRoot = lockJson.packages?.[''];

const cargoPath = join(projectRoot, 'Cargo.toml');
const cargoText = readText(cargoPath);
const cargoVersion = findTomlVersion(cargoText, 'workspace.package', cargoPath);

const cargoLockPath = join(projectRoot, 'Cargo.lock');
const cargoLockText = readText(cargoLockPath);
const cargoPackageNames = new Set(['session-loom', 'session-loom-cli', 'session-loom-core']);
const cargoLockVersions = findCargoLockVersions(cargoLockText, cargoPackageNames, cargoLockPath);

const tauriCargoPath = join(projectRoot, 'src-tauri', 'Cargo.toml');
const tauriCargoText = readText(tauriCargoPath);
const tauriCargoVersion = findTomlVersion(tauriCargoText, 'package', tauriCargoPath);

const tauriPath = join(projectRoot, 'src-tauri', 'tauri.conf.json');
const tauriJson = readJson(tauriPath);

const versions = new Map([
  ['package.json', packageJson.version],
  ['package-lock.json', lockJson.version],
  ['package-lock.json packages[""].version', lockRoot?.version],
  ['Cargo.toml [workspace.package]', cargoVersion],
  ['Cargo.lock session-loom', cargoLockVersions.get('session-loom')],
  ['Cargo.lock session-loom-cli', cargoLockVersions.get('session-loom-cli')],
  ['Cargo.lock session-loom-core', cargoLockVersions.get('session-loom-core')],
  ['src-tauri/Cargo.toml [package]', tauriCargoVersion],
  ['src-tauri/tauri.conf.json', tauriJson.version],
]);

if (checkOnly) {
  const mismatches = [...versions.entries()]
    .filter(([, actualVersion]) => actualVersion !== version)
    .map(([file, actualVersion]) => `${file}=${actualVersion ?? '<missing>'}`);

  if (mismatches.length > 0) {
    throw new Error(`Release tag ${tag} does not match project versions:\n- ${mismatches.join('\n- ')}`);
  }

  console.log(`Verified Session-Loom ${version}`);
  return;
}

if (packageJson.version !== version) {
  packageJson.version = version;
  writeJson(packagePath, packageJson);
}

if (lockJson.version !== version || lockRoot?.version !== version) {
  lockJson.version = version;
  lockJson.packages ??= {};
  lockJson.packages[''] ??= {};
  lockJson.packages[''].version = version;
  writeJson(lockPath, lockJson);
}

if (cargoVersion !== version) {
  writeText(cargoPath, setTomlVersion(cargoText, 'workspace.package', version, cargoPath));
}

if ([...cargoLockVersions.values()].some((cargoLockVersion) => cargoLockVersion !== version)) {
  writeText(cargoLockPath, setCargoLockVersions(cargoLockText, cargoPackageNames, version, cargoLockPath));
}

if (tauriCargoVersion !== version) {
  writeText(tauriCargoPath, setTomlVersion(tauriCargoText, 'package', version, tauriCargoPath));
}

if (tauriJson.version !== version) {
  tauriJson.version = version;
  writeJson(tauriPath, tauriJson);
}

console.log(`Synchronized Session-Loom ${version}`);
