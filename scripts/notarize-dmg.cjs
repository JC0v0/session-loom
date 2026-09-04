const { existsSync, readdirSync, statSync } = require('node:fs');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');

const projectRoot = join(__dirname, '..');
const config = JSON.parse(require('node:fs').readFileSync(join(projectRoot, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const bundleDir = join(projectRoot, 'target', 'release', 'bundle', 'dmg');
const prefix = `${config.productName}_${config.version}_`;

function run(command, args) {
  const result = spawnSync(command, args, { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function notarizationArgs(dmgPath) {
  if (process.env.APPLE_ID && process.env.APPLE_PASSWORD && process.env.APPLE_TEAM_ID) {
    return [
      '--apple-id', process.env.APPLE_ID,
      '--password', process.env.APPLE_PASSWORD,
      '--team-id', process.env.APPLE_TEAM_ID,
    ];
  }

  if (process.env.APPLE_API_ISSUER && process.env.APPLE_API_KEY && process.env.APPLE_API_KEY_PATH) {
    return [
      '--issuer', process.env.APPLE_API_ISSUER,
      '--key-id', process.env.APPLE_API_KEY,
      '--key', process.env.APPLE_API_KEY_PATH,
    ];
  }

  return null;
}

if (process.platform !== 'darwin') {
  console.log('Skipping DMG notarization on non-macOS host.');
  process.exit(0);
}

const args = notarizationArgs();
if (!args) {
  const message = 'DMG notarization credentials are missing.';
  if (process.env.CI) {
    console.error(message);
    process.exit(1);
  }
  console.warn(`${message} Skipping notarization for this local build.`);
  process.exit(0);
}

if (!existsSync(bundleDir)) {
  console.error(`DMG bundle directory not found: ${bundleDir}`);
  process.exit(1);
}

const dmgFiles = readdirSync(bundleDir)
  .filter((file) => file.endsWith('.dmg') && file.startsWith(prefix))
  .sort((a, b) => statSync(join(bundleDir, b)).mtimeMs - statSync(join(bundleDir, a)).mtimeMs);

if (dmgFiles.length === 0) {
  console.error(`No DMG found for ${prefix}*.dmg in ${bundleDir}`);
  process.exit(1);
}

const dmgPath = join(bundleDir, dmgFiles[0]);
console.log(`Notarizing ${dmgPath}`);
run('xcrun', ['notarytool', 'submit', dmgPath, ...args, '--wait']);
console.log(`Stapling ${dmgPath}`);
run('xcrun', ['stapler', 'staple', dmgPath]);
