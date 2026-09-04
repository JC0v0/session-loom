const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');
const test = require('node:test');

const projectRoot = join(__dirname, '..');
const workflow = readFileSync(
  join(projectRoot, '.github', 'workflows', 'release.yml'),
  'utf8',
);
const macosJob = workflow.slice(
  workflow.indexOf('  build-macos:'),
  workflow.indexOf('  publish:'),
);

test('macOS releases require a Developer ID certificate and Apple notarization', () => {
  assert.match(macosJob, /node --test scripts\/release-security\.test\.cjs/);
  assert.doesNotMatch(macosJob, /APPLE_SIGNING_IDENTITY:\s*['"]?-['"]?/);
  assert.match(macosJob, /APPLE_CERTIFICATE:\s*\$\{\{ secrets\.APPLE_CERTIFICATE \}\}/);
  assert.match(
    macosJob,
    /APPLE_CERTIFICATE_PASSWORD:\s*\$\{\{ secrets\.APPLE_CERTIFICATE_PASSWORD \}\}/,
  );
  assert.match(macosJob, /KEYCHAIN_PASSWORD:\s*\$\{\{ secrets\.KEYCHAIN_PASSWORD \}\}/);
  assert.match(macosJob, /security import/);
  assert.match(macosJob, /Developer ID Application/);

  for (const secret of ['APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID']) {
    assert.match(macosJob, new RegExp(`${secret}:\\s*\\$\\{\\{ secrets\\.${secret} \\}\\}`));
  }
});

test('macOS release artifacts must pass signature, notarization, and Gatekeeper checks', () => {
  const verificationStep = macosJob.indexOf(
    '- name: Verify macOS signature, notarization, and Gatekeeper acceptance',
  );
  const uploadStep = macosJob.indexOf('- name: Upload macOS packages and updater artifacts');

  assert.ok(verificationStep >= 0, 'macOS artifact verification step is required');
  assert.ok(uploadStep > verificationStep, 'artifacts must be verified before upload');
  assert.match(macosJob, /DMG_FILES=\(target\/release\/bundle\/dmg\/Session-Loom_\*\.dmg\)/);
  assert.match(macosJob, /\$\{#DMG_FILES\[@\]\} -ne 1/);
  assert.doesNotMatch(macosJob, /src-tauri\/target\/release\/bundle/);
  assert.match(macosJob, /codesign --verify --deep --strict/);
  assert.match(macosJob, /xcrun stapler validate/);
  assert.match(macosJob, /spctl --assess --type execute/);
  assert.match(macosJob, /spctl --assess --type open/);
});

test('CI fails closed when notarization credentials are missing', () => {
  const env = { ...process.env, CI: 'true' };
  for (const name of [
    'APPLE_ID',
    'APPLE_PASSWORD',
    'APPLE_TEAM_ID',
    'APPLE_API_ISSUER',
    'APPLE_API_KEY',
    'APPLE_API_KEY_PATH',
  ]) {
    delete env[name];
  }

  const result = spawnSync(process.execPath, [join(projectRoot, 'scripts', 'notarize-dmg.cjs')], {
    cwd: projectRoot,
    encoding: 'utf8',
    env,
  });

  assert.notEqual(result.status, 0, 'CI must not publish an unnotarized DMG');
  assert.match(result.stderr, /notarization credentials/i);
});
