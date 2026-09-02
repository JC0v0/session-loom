const { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, rmSync } = require('node:fs');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');

const projectRoot = join(__dirname, '..');
const release = process.argv.includes('--release');
const profile = release ? 'release' : 'debug';
const executable = process.platform === 'win32' ? 'ssl.exe' : 'ssl';
const args = ['build', '-p', 'session-loom-cli'];
if (release) args.push('--release');

const result = spawnSync('cargo', args, { cwd: projectRoot, stdio: 'inherit' });
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);

const source = join(projectRoot, 'target', profile, executable);
const destinationDir = join(projectRoot, 'src-tauri', 'binaries');
const destination = join(destinationDir, executable);
mkdirSync(destinationDir, { recursive: true });
for (const stale of ['ssl', 'ssl.exe']) {
  const path = join(destinationDir, stale);
  if (path !== destination && existsSync(path)) rmSync(path);
}
copyFileSync(source, destination);
if (process.platform !== 'win32') chmodSync(destination, 0o755);

if (process.platform === 'darwin' && release) {
  const macosConfig = JSON.parse(
    readFileSync(join(projectRoot, 'src-tauri', 'tauri.macos.conf.json'), 'utf8'),
  );
  const identity = process.env.APPLE_SIGNING_IDENTITY || macosConfig?.bundle?.macOS?.signingIdentity;

  if (identity && identity !== '-') {
    const signing = spawnSync(
      'codesign',
      ['--force', '--options', 'runtime', '--timestamp', '--sign', identity, destination],
      { stdio: 'inherit' },
    );
    if (signing.error) throw signing.error;
    if (signing.status !== 0) process.exit(signing.status ?? 1);
  }
}
