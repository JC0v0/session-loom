import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CANONICAL_SCHEMA_VERSION, type CanonicalSession } from '../canonical/types';
import { listSessions, readSession, writeSession } from './store';

let dir: string;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'sb-store-'));
  process.env.SESSION_BRIDGE_STORE = dir;
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

const sample: CanonicalSession = {
  schemaVersion: CANONICAL_SCHEMA_VERSION,
  sourceTool: 'codex',
  sessionId: 's1',
  cwd: 'C:\\proj',
  createdAt: '2026-01-01T00:00:00.000Z',
  updatedAt: '2026-01-01T00:00:00.000Z',
  messages: [{ role: 'user', text: 'hi', toolCalls: [] }],
};

describe('canonical store', () => {
  it('writes and reads back the same session', () => {
    const file = writeSession(sample);
    expect(readSession('codex', 's1')).toEqual(sample);
    expect(file).toMatch(/codex[\\/]s1\.json$/);
  });

  it('re-mirroring an unchanged session is a no-op', () => {
    const file = writeSession(sample);
    const before = readFileSync(file, 'utf8');
    writeSession(sample);
    expect(readFileSync(file, 'utf8')).toBe(before);
  });

  it('lists sessions newest first', () => {
    writeSession(sample);
    writeSession({ ...sample, sessionId: 's2', sourceTool: 'claude', updatedAt: '2026-01-02T00:00:00.000Z' });
    expect(listSessions().map((s) => s.sessionId)).toEqual(['s2', 's1']);
  });
});
