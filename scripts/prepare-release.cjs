const { execFileSync } = require('node:child_process');
const { readFileSync } = require('node:fs');
const { join, resolve } = require('node:path');

const projectRoot = join(__dirname, '..');
const args = process.argv.slice(2);
const rawTag = args[0] || process.env.RELEASE_TAG || '';
const tag = rawTag.startsWith('v') ? rawTag : `v${rawTag}`;
const notesFileIndex = args.indexOf('--notes-file');
const notesFile = notesFileIndex >= 0 ? args[notesFileIndex + 1] : process.env.RELEASE_NOTES_FILE;

if (!/^v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(tag)) {
  throw new Error('Usage: npm run release:prepare -- v0.1.2 [--notes-file path]');
}

function run(command, commandArgs) {
  execFileSync(command, commandArgs, {
    cwd: projectRoot,
    stdio: 'inherit',
  });
}

function output(command, commandArgs) {
  return execFileSync(command, commandArgs, {
    cwd: projectRoot,
    encoding: 'utf8',
  }).trim();
}

function succeeds(command, commandArgs) {
  try {
    execFileSync(command, commandArgs, {
      cwd: projectRoot,
      stdio: 'ignore',
    });
    return true;
  } catch {
    return false;
  }
}

if (output('git', ['status', '--porcelain'])) {
  throw new Error('Release preparation requires a clean working tree.');
}

const branch = output('git', ['branch', '--show-current']);
if (branch !== 'main') {
  throw new Error(`Release preparation must run on main, currently on ${branch || 'detached HEAD'}.`);
}

if (succeeds('git', ['show-ref', '--verify', '--quiet', `refs/tags/${tag}`])) {
  throw new Error(`Tag ${tag} already exists locally.`);
}

run(process.execPath, [join(projectRoot, 'scripts', 'sync-release-version.cjs'), tag]);
run('git', ['diff', '--check']);
run('cargo', ['fmt', '--all', '--', '--check']);
run('cargo', ['test', '--workspace']);
run('cargo', ['clippy', '--workspace', '--all-targets', '--', '-D', 'warnings']);

const versionFiles = [
  'package.json',
  'package-lock.json',
  'Cargo.toml',
  'Cargo.lock',
  'src-tauri/Cargo.toml',
  'src-tauri/tauri.conf.json',
];
run('git', ['add', ...versionFiles]);

if (succeeds('git', ['diff', '--cached', '--quiet'])) {
  console.log(`No version file changes; tagging current HEAD as ${tag}.`);
} else {
  run('git', ['commit', '-m', `chore(release): prepare ${tag}`]);
}

let notes = process.env.RELEASE_NOTES?.trim() || '';
if (notesFile) {
  notes = readFileSync(resolve(projectRoot, notesFile), 'utf8').trim();
}
if (!notes) {
  notes = `Release ${tag}. GitHub Actions will generate release notes from the commits.`;
}

run('git', ['tag', '-a', tag, '-m', `Session-Loom ${tag}`, '-m', notes]);

const commit = output('git', ['rev-parse', 'HEAD']);
console.log(`Prepared ${tag} at ${commit}.`);
console.log(`Next: git push origin main && git push origin ${tag}`);
