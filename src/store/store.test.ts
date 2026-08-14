import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CANONICAL_SCHEMA_VERSION, type CanonicalSession } from '../canonical/types';
import { closeStore, exportSession, listSessions, readSession, searchSessions, writeSession } from './store';

let dir: string;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'sb-store-'));
  process.env.SESSION_BRIDGE_STORE = dir;
});

afterEach(() => {
  closeStore();
  rmSync(dir, { recursive: true, force: true });
});

const sample: CanonicalSession = {
  schemaVersion: CANONICAL_SCHEMA_VERSION,
  sourceTool: 'codex',
  sessionId: 's1',
  cwd: 'C:\\proj',
  createdAt: '2026-01-01T00:00:00.000Z',
  updatedAt: '2026-01-01T00:00:00.000Z',
  messages: [{ role: 'user', text: '帮我修一下这个 bug', toolCalls: [] }],
};

describe('sqlite store', () => {
  it('writes and reads back the same session', () => {
    writeSession(sample);
    expect(readSession('codex', 's1')).toEqual(sample);
  });

  it('re-writing an unchanged session does not duplicate rows', () => {
    writeSession(sample);
    writeSession(sample);
    expect(listSessions()).toHaveLength(1);
  });

  it('lists sessions newest first', () => {
    writeSession(sample);
    writeSession({ ...sample, sessionId: 's2', sourceTool: 'claude', updatedAt: '2026-01-02T00:00:00.000Z' });
    expect(listSessions().map((s) => s.sessionId)).toEqual(['s2', 's1']);
  });

  it('searches sessions by text and cwd', () => {
    writeSession(sample);
    expect(searchSessions('修一下').map((s) => s.sessionId)).toContain('s1');
    expect(searchSessions('C:\\proj').map((s) => s.sessionId)).toContain('s1');
    expect(searchSessions('不存在的词')).toHaveLength(0);
  });

  it('exports a session as canonical JSON', () => {
    writeSession(sample);
    expect(JSON.parse(exportSession('s1')).sessionId).toBe('s1');
  });
});
