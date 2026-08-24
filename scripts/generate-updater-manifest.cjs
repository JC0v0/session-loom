const {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} = require('node:fs');
const { basename, join, relative, resolve } = require('node:path');

const projectRoot = join(__dirname, '..');
const artifactsRoot = resolve(
  projectRoot,
  process.env.UPDATER_ARTIFACTS_ROOT || process.argv[2] || 'artifacts',
);
const outputPath = resolve(
  projectRoot,
  process.env.UPDATER_MANIFEST_PATH || join(artifactsRoot, 'latest.json'),
);
const rawTag = process.env.RELEASE_TAG || process.argv[3] || '';
const tag = rawTag.startsWith('v') ? rawTag : `v${rawTag}`;

if (!/^v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(tag)) {
  throw new Error(`Expected a release tag such as v0.1.2, received: ${rawTag}`);
}

function walk(directory) {
  const paths = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      paths.push(...walk(path));
    } else if (entry.isFile()) {
      paths.push(path);
    }
  }
  return paths;
}

function artifactForSignature(signaturePath) {
  const artifactPath = signaturePath.slice(0, -'.sig'.length);
  if (!existsSync(artifactPath)) {
    throw new Error(`Missing updater artifact for ${signaturePath}`);
  }
  return artifactPath;
}

function updaterBundle(path) {
  const name = basename(path);
  if (name.endsWith('.nsis.zip')) return 'nsis';
  if (name.endsWith('.app.tar.gz')) return 'app';
  return null;
}

function artifactGroup(path) {
  const parts = relative(artifactsRoot, path).split(/[\\/]/);
  return parts[0] || '';
}

function updaterPlatform(path, allPaths) {
  const group = artifactGroup(path);
  const groupPaths = allPaths.filter((candidate) => artifactGroup(candidate) === group);
  const hints = [group, ...groupPaths.map((candidate) => basename(candidate))]
    .join(' ')
    .toLowerCase();

  let os;
  if (hints.includes('windows')) {
    os = 'windows';
  } else if (hints.includes('macos') || hints.includes('darwin')) {
    os = 'darwin';
  } else {
    throw new Error(`Could not determine updater OS for ${path}`);
  }

  let arch;
  if (/aarch64|arm64/.test(hints)) {
    arch = 'aarch64';
  } else if (/x86_64|x64|amd64|x86-64/.test(hints)) {
    arch = 'x86_64';
  } else if (/i686|i386/.test(hints)) {
    arch = 'i686';
  } else {
    throw new Error(`Could not determine updater architecture for ${path}`);
  }

  return `${os}-${arch}`;
}

const allPaths = walk(artifactsRoot);
const signatures = allPaths
  .filter((path) => path.endsWith('.sig'))
  .map((signaturePath) => {
    const artifactPath = artifactForSignature(signaturePath);
    return {
      artifactPath,
      bundle: updaterBundle(artifactPath),
      signaturePath,
    };
  })
  .filter(({ bundle }) => bundle);

if (signatures.length === 0) {
  throw new Error(
    `No signed updater artifacts found under ${artifactsRoot}. Expected .nsis.zip.sig or .app.tar.gz.sig files.`,
  );
}

const server = (process.env.GITHUB_SERVER_URL || 'https://github.com').replace(/\/$/, '');
const repository = process.env.GITHUB_REPOSITORY || 'JC0v0/session-loom';
const platforms = {};
const selected = new Map();

for (const candidate of signatures) {
  const platform = updaterPlatform(candidate.artifactPath, allPaths);
  const score = candidate.bundle === 'nsis' || candidate.bundle === 'app' ? 10 : 0;
  const current = selected.get(platform);
  if (current && current.score >= score) continue;
  selected.set(platform, { ...candidate, platform, score });
}

for (const candidate of selected.values()) {
  const signature = readFileSync(candidate.signaturePath, 'utf8').trim();
  if (!signature) throw new Error(`Empty updater signature: ${candidate.signaturePath}`);
  const fileName = basename(candidate.artifactPath);
  const payload = {
    signature,
    url: `${server}/${repository}/releases/download/${tag}/${encodeURIComponent(fileName)}`,
  };
  platforms[candidate.platform] = payload;
  platforms[`${candidate.platform}-${candidate.bundle}`] = payload;
}

mkdirSync(resolve(outputPath, '..'), { recursive: true });
writeFileSync(
  outputPath,
  `${JSON.stringify(
    {
      version: tag.slice(1),
      notes: process.env.RELEASE_NOTES?.trim() || `Session-Loom ${tag}`,
      pub_date: new Date().toISOString(),
      platforms,
    },
    null,
    2,
  )}\n`,
);

console.log(
  `Generated ${relative(projectRoot, outputPath)} for ${Object.keys(platforms).join(', ')}`,
);
