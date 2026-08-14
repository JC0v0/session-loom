import { describe, it, expect } from 'vitest';
import { deserialize, serialize } from './serialize';
import { CANONICAL_SCHEMA_VERSION, type CanonicalSession } from './types';

const sample: CanonicalSession = {
  schemaVersion: CANONICAL_SCHEMA_VERSION,
  sourceTool: 'codex',
  sessionId: 's1',
  cwd: 'C:\\proj',
  createdAt: '2026-01-01T00:00:00.000Z',
  updatedAt: '2026-01-01T00:00:01.000Z',
  messages: [
    { role: 'user', text: 'hello', toolCalls: [] },
    {
      role: 'assistant',
      text: '',
      toolCalls: [{ id: 'c1', name: 'shell', input: { cmd: 'ls' }, output: 'ok' }],
    },
  ],
};

describe('canonical serialize', () => {
  it('round-trips every field', () => {
    expect(deserialize(serialize(sample))).toEqual(sample);
  });

  it('rejects an unknown schema version', () => {
    const bad = JSON.stringify({ ...sample, schemaVersion: 99 });
    expect(() => deserialize(bad)).toThrow(/schema version/i);
  });
});
