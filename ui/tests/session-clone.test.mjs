import assert from 'node:assert/strict';
import test from 'node:test';
import {
  assetsForStep, beginRequest, canApply, canRetryPartial, captureAssetIds,
  classifyResults, createMigrationState, decisionList, diffRows, initialDraft,
  invalidateRequests, isCurrent, loadRememberedRun, rememberRun, resultsSummary,
  retryPreviewId, setDecision, setDiscovery, setPreview, startClone, toggleAsset,
} from '../session-clone-state.js';

import { createCloneFlow } from '../session-clone.js';

const source = { sessionId: 'native-a', title: '修复登录', cwd: '/tmp/project', sourceTool: 'codex' };
function discoveryFixture() {
  return {
    run: { id: 'run-1', revision: 1, project: { root: '/tmp/project' }, target: { tool: 'claude', projectRoot: '/tmp/project' } },
    report: { assets: [
      { id: 'project-skill', kind: 'skill', name: 'shared', scope: 'project', selectedByDefault: true, sourceActive: true },
      { id: 'global-memory', kind: 'memory', name: 'global.md', scope: 'user', selectedByDefault: false, sourceActive: true },
      { id: 'shadowed', kind: 'skill', scope: 'user', sourceActive: false },
    ] },
  };
}
function previewFixture(overrides = {}) {
  return {
    id: 'preview-1', readyToApply: false,
    conflicts: [{ id: 'conflict-1', kind: 'targetChanged', targetPath: '/tmp/project/.claude/skills/shared/SKILL.md', resolved: false,
      items: [{ decisionId: 'decision-1', itemId: 'project-skill', contributionKind: 'wholeFile',
        diff: { kind: 'text', diff: { lines: [
          { kind: 'removed', oldLine: 1, text: 'old\n' },
          { kind: 'added', newLine: 1, text: 'new\n' },
          { kind: 'context', oldLine: 2, newLine: 2, text: 'same\n' },
        ] } },
      }],
    }], ...overrides,
  };
}

test('clone starts from the clicked native session and target without a project wizard', () => {
  const state = createMigrationState();
  startClone(state, source, 'claude', 'restore');
  assert.equal(state.step, 'items');
  assert.equal(state.source.sessionId, 'native-a');
  assert.equal(state.targetTool, 'claude');
  assert.deepEqual(captureAssetIds(state), []);
  assert.equal(state.project, undefined);
  assert.equal(state.selectedSessions, undefined);
});

test('all attachments are optional including default project items', () => {
  const state = createMigrationState();
  startClone(state, source, 'claude');
  setDiscovery(state, discoveryFixture());
  assert.deepEqual(captureAssetIds(state), []);
  const assets = assetsForStep(state);
  assert.equal(assets.length, 2);
  assert.ok(assets.every((asset) => !asset.selected && !asset.locked));
  toggleAsset(state, 'project-skill');
  toggleAsset(state, 'global-memory');
  assert.deepEqual(captureAssetIds(state), ['project-skill', 'global-memory']);
  toggleAsset(state, 'project-skill');
  assert.deepEqual(captureAssetIds(state), ['global-memory']);
  assert.throws(() => toggleAsset(state, 'forged'), /unknown/);
  assert.throws(() => toggleAsset(state, 'shadowed'), /inactive/);
});

test('opening another clone or closing the dialog invalidates late scans and clears old choices', () => {
  const state = createMigrationState();
  startClone(state, source, 'claude');
  const token = beginRequest(state);
  setDiscovery(state, discoveryFixture());
  toggleAsset(state, 'global-memory');
  startClone(state, { ...source, sessionId: 'native-b' }, 'pi');
  assert.equal(isCurrent(state, token), false);
  assert.deepEqual(captureAssetIds(state), []);
  assert.equal(state.run, null);
  const second = beginRequest(state);
  invalidateRequests(state);
  assert.equal(isCurrent(state, second), false);
});

test('changing attachments or conflict decisions immediately invalidates apply', () => {
  const state = createMigrationState();
  startClone(state, source, 'claude');
  setDiscovery(state, discoveryFixture());
  setPreview(state, previewFixture({ readyToApply: true, conflicts: [] }));
  assert.equal(canApply(state), true);
  toggleAsset(state, 'project-skill');
  assert.equal(canApply(state), false);
  const resolved = previewFixture({ readyToApply: true, conflicts: [] });
  setPreview(state, resolved);
  const item = previewFixture().conflicts[0].items[0];
  setDecision(state, item, 'replace');
  assert.equal(canApply(state), false, 'old preview must not apply while recalculation is pending');
  setPreview(state, resolved);
  assert.equal(canApply(state), true);
});

test('merge validates config JSON, binary capability and redaction without losing drafts', () => {
  const state = createMigrationState();
  const item = previewFixture().conflicts[0].items[0];
  assert.throws(() => setDecision(state, item, 'merge', ''), /合并内容不能为空/);
  assert.throws(() => setDecision(state, item, 'merge', 'value [redacted]'), /脱敏/);
  assert.throws(() => setDecision(state, { ...item, diff: { kind: 'binary' } }, 'merge', 'text'), /二进制/);
  const config = { ...item, decisionId: 'config-1', contributionKind: 'configValue' };
  assert.throws(() => setDecision(state, config, 'merge', 'not json'), /有效 JSON/);
  setDecision(state, config, 'merge', '{"command":"fixture"}');
  assert.deepEqual(decisionList(state)[0].mergedValue, { command: 'fixture' });
  assert.equal(initialDraft(item), 'new\nsame\n');
});

test('results distinguish the cloned session from attachments needing configuration or failing', () => {
  const report = { state: 'partial', items: [
    { itemId: 'native-a', readiness: 'available' },
    { itemId: 'tool', readiness: 'needsConfiguration' },
    { itemId: 'other', readiness: 'unsupported' },
    { itemId: 'bad', readiness: 'failed' },
  ] };
  assert.equal(classifyResults(report).failed.length, 1);
  assert.deepEqual(resultsSummary(report), { total: 4, available: 1, needsConfiguration: 1, unsupported: 1, failed: 1, ok: false });
});

test('diff rows are text data and include binary/missing summaries', () => {
  const item = previewFixture().conflicts[0].items[0];
  assert.deepEqual(diffRows(item).map((row) => row.kind), ['removed', 'added', 'context']);
  assert.match(diffRows({ diff: { kind: 'binary', oldSize: 1, newSize: 2 } })[0].text, /二进制/);
  assert.match(diffRows({ diff: { kind: 'missing', addedBytes: 7 } })[0].text, /7 字节/);
});

test('resume is scoped to this native session and destination and stores only a durable run pointer', () => {
  const data = new Map();
  const storage = { setItem: (key, value) => data.set(key, value), getItem: (key) => data.get(key), removeItem: (key) => data.delete(key) };
  const state = createMigrationState();
  startClone(state, source, 'claude');
  state.run = { id: 'run-1' };
  state.mergeDrafts = { private: 'sensitive merge body' };
  rememberRun(state, storage);
  assert.equal(loadRememberedRun(source.sessionId, 'claude', storage).runId, 'run-1');
  assert.equal(loadRememberedRun(source.sessionId, 'pi', storage), null);
  assert.equal(loadRememberedRun('native-b', 'claude', storage), null);
  assert.ok(![...data.values()].join('').includes('sensitive merge body'));
  const blocked = { getItem() { throw new Error('blocked'); }, setItem() { throw new Error('blocked'); } };
  assert.doesNotThrow(() => rememberRun(state, blocked));
  assert.equal(loadRememberedRun(source.sessionId, 'claude', blocked), null);
});

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

test('session-only cloning works while a scan is pending and ignores its late response', async () => {
  const scan = deferred();
  const calls = [];
  const flow = createCloneFlow(async (command, args) => {
    calls.push({ command, args });
    if (command === 'sessions_clone_prepare') return scan.promise;
    if (command === 'sessions_restore') return { ok: true };
    throw new Error(`unexpected ${command}`);
  }, undefined, undefined, null);
  const opening = flow.open(source, 'claude');
  assert.equal(flow.state.loading, true);
  await flow.sessionOnly();
  scan.resolve(discoveryFixture());
  await opening;
  assert.equal(flow.state.results.sessionOnly, true);
  assert.equal(flow.state.step, 'results');
  assert.equal(flow.state.run, null);
  assert.deepEqual(calls.map((call) => call.command), ['sessions_clone_prepare', 'sessions_restore']);
  assert.deepEqual(calls[1].args, { sessionId: 'native-a', target: 'claude' });
});

test('scan failure does not block session-only cloning and cancellation writes nothing', async () => {
  const calls = [];
  const flow = createCloneFlow(async (command) => {
    calls.push(command);
    if (command === 'sessions_clone_prepare') throw new Error('project missing');
    return { ok: true };
  }, undefined, undefined, null);
  await flow.open(source, 'claude');
  assert.equal(flow.state.scanError, 'project missing');
  flow.close();
  assert.deepEqual(calls, ['sessions_clone_prepare']);
  await flow.open(source, 'pi', 'sync');
  await flow.sessionOnly();
  assert.equal(calls.at(-1), 'sessions_sync');
});

test('only checked attachments are previewed and applying never duplicates an in-flight write', async () => {
  const writing = deferred();
  const calls = [];
  const flow = createCloneFlow(async (command, args) => {
    calls.push({ command, args });
    if (command === 'sessions_clone_prepare') return discoveryFixture();
    if (command === 'sessions_clone_preview') return previewFixture({ readyToApply: true, conflicts: [] });
    if (command === 'migration_apply') return writing.promise;
    throw new Error(command);
  }, undefined, undefined, null);
  await flow.open(source, 'claude');
  flow.toggle('global-memory');
  await flow.preview();
  assert.deepEqual(calls[1].args.assets, ['global-memory']);
  assert.equal(calls[1].args.target, undefined, 'target is resolved by the core');
  const first = flow.apply();
  await flow.apply();
  flow.close();
  assert.equal(calls.filter((call) => call.command === 'migration_apply').length, 1);
  writing.resolve({ state: 'partial', items: [], sessions: [] });
  await first;
  assert.equal(flow.state.open, false, 'finishing a write must not reopen a hidden dialog');
  assert.equal(flow.state.results.state, 'partial');
});

test('reopening the same clone restores results and retries the stored preview rather than preparing a new copy', async () => {
  const data = new Map();
  const storage = { getItem: (key) => data.get(key), setItem: (key, value) => data.set(key, value) };
  const before = createMigrationState();
  startClone(before, source, 'claude');
  before.run = { id: 'durable-run' };
  rememberRun(before, storage);
  const calls = [];
  const flow = createCloneFlow(async (command, args) => {
    calls.push({ command, args });
    if (command === 'migration_status') return {
      id: 'durable-run', sessionIds: ['native-a'], target: { tool: 'claude' },
      assets: [], discoveredAssets: [], preview: { id: 'durable-preview' },
      state: 'partial', applyReport: { state: 'partial', items: [], sessions: [] },
    };
    if (command === 'migration_preview_get') return { id: 'durable-preview', decisions: [] };
    if (command === 'migration_apply') return { state: 'partial', items: [], sessions: [] };
    throw new Error('must not prepare a second clone');
  }, undefined, undefined, storage);
  await flow.open(source, 'claude');
  assert.equal(flow.state.step, 'results');
  await flow.apply();
  assert.deepEqual(calls.at(-1).args, { runId: 'durable-run', previewId: 'durable-preview' });
});

test('closing during discovery prevents a late response from opening or populating the dialog', async () => {
  const scan = deferred();
  const flow = createCloneFlow(() => scan.promise, undefined, undefined, null);
  const opening = flow.open(source, 'claude');
  flow.close();
  scan.resolve(discoveryFixture());
  await opening;
  assert.equal(flow.state.open, false);
  assert.equal(flow.state.run, null);
});

test('partial execution can resume the same preview and completed results cannot retry', () => {
  const state = createMigrationState();
  assert.equal(retryPreviewId(state), null);
  state.run = { preview: { id: 'preview-1' } };
  assert.equal(retryPreviewId(state), 'preview-1');
  assert.equal(canRetryPartial({ state: 'partial' }, retryPreviewId(state)), true);
  assert.equal(canRetryPartial({ state: 'completed' }, retryPreviewId(state)), false);
});
