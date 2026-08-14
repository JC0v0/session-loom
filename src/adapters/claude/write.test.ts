import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CANONICAL_SCHEMA_VERSION, type CanonicalSession } from '../../canonical/types';
import { writeClaudeSession } from './write';

let dir: string;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'sb-claude-'));
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

const session: CanonicalSession = {
  schemaVersion: CANONICAL_SCHEMA_VERSION,
  sourceTool: 'claude',
  sessionId: 's2',
  cwd: 'C:\\proj',
  createdAt: '2026-01-01T00:00:00.000Z',
  updatedAt: '2026-01-01T00:00:01.000Z',
  messages: [
    { role: 'user', text: 'hello', toolCalls: [] },
    { role: 'assistant', text: 'running', toolCalls: [{ id: 'c2', name: 'Bash', input: { command: 'ls' }, output: 'ok' }] },
  ],
};

describe('claude write', () => {
  it('writes a session file and a history entry', () => {
    const result = writeClaudeSession(session, { root: dir });
    expect(result.sessionFile.endsWith('.jsonl')).toBe(true);
    const history = readFileSync(join(dir, 'history.jsonl'), 'utf8');
    const entry = JSON.parse(history.trim().split('\n')[0]) as { sessionId: string; project: string; display: string };
    expect(entry).toMatchObject({ sessionId: result.sessionId, project: 'C:\\proj', display: 'hello' });
  });

  it('encodes the project directory from the cwd', () => {
    const result = writeClaudeSession({ ...session, cwd: 'F:\\xiaoyou\\xiaoyou' }, { root: dir });
    expect(result.sessionFile).toContain('F--xiaoyou-xiaoyou');
  });
});
