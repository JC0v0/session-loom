import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { appendFileSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startWatcher, type WatchState } from './watch';

let fixture: string;
let store: string;
let watcher: WatchState | undefined;

beforeEach(() => {
  fixture = mkdtempSync(join(tmpdir(), 'sb-fixture-'));
  store = mkdtempSync(join(tmpdir(), 'sb-store-'));
  process.env.SESSION_BRIDGE_STORE = store;
});

afterEach(() => {
  watcher?.stop();
  rmSync(fixture, { recursive: true, force: true });
  rmSync(store, { recursive: true, force: true });
});

const codexSession = [
  JSON.stringify({ timestamp: '2026-01-01T00:00:00.000Z', type: 'session_meta', payload: { session_id: 's1', cwd: 'C:\\proj', timestamp: '2026-01-01T00:00:00.000Z' } }),
  JSON.stringify({ timestamp: '2026-01-01T00:00:01.000Z', type: 'response_item', payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'hello' }] } }),
].join('\n');

describe('daemon watcher', () => {
  it('mirrors a new session file and re-mirrors when it grows', async () => {
    const file = join(fixture, 's1.jsonl');
    writeFileSync(file, codexSession, 'utf8');
    watcher = startWatcher([{ sourceTool: 'codex', root: fixture }], { intervalMs: 30 });

    const storeFile = join(store, 'codex', 's1.json');
    await waitFor(() => existsSync(storeFile));
    expect(readFileSync(storeFile, 'utf8')).toContain('hello');

    appendFileSync(file, `\n${JSON.stringify({ timestamp: '2026-01-01T00:00:02.000Z', type: 'response_item', payload: { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'world' }] } })}`, 'utf8');
    await waitFor(() => readFileSync(storeFile, 'utf8').includes('world'));
  });

  it('does not write back into the watched directory', async () => {
    const file = join(fixture, 's1.jsonl');
    writeFileSync(file, codexSession, 'utf8');
    watcher = startWatcher([{ sourceTool: 'codex', root: fixture }], { intervalMs: 30 });
    await waitFor(() => existsSync(join(store, 'codex', 's1.json')));
    expect(readdirSync(fixture)).toEqual(['s1.jsonl']);
  });
});

async function waitFor(condition: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (!condition()) {
    if (Date.now() - start > timeoutMs) throw new Error('timed out waiting for condition');
    await new Promise((r) => setTimeout(r, 20));
  }
}
