import { describe, it, expect } from 'vitest';
import { CANONICAL_SCHEMA_VERSION, type CanonicalSession } from '../../canonical/types';
import { writeCodexSession } from './write';

const session: CanonicalSession = {
  schemaVersion: CANONICAL_SCHEMA_VERSION,
  sourceTool: 'codex',
  sessionId: 's1',
  cwd: 'C:\\proj',
  createdAt: '2026-01-01T00:00:00.000Z',
  updatedAt: '2026-01-01T00:00:01.000Z',
  messages: [
    { role: 'user', text: 'hi', toolCalls: [] },
    { role: 'assistant', text: 'ok', toolCalls: [{ id: 'c1', name: 'shell', input: { cmd: 'ls' }, output: 'file.txt' }] },
  ],
};

describe('codex write', () => {
  it('emits session_meta, message, function_call, and function_call_output records', () => {
    const lines = writeCodexSession(session).trim().split('\n').map((l) => JSON.parse(l) as { type?: string; payload?: { type?: string } });
    expect(lines[0].type).toBe('session_meta');
    const types = lines.map((l) => l.payload?.type);
    expect(types).toContain('message');
    expect(types).toContain('function_call');
    expect(types).toContain('function_call_output');
  });
});
