import { describe, it, expect } from 'vitest';
import { parseClaudeSession } from './read';

const fixture = [
  JSON.stringify({ type: 'mode', mode: 'normal', sessionId: 's2' }),
  JSON.stringify({ type: 'user', message: { role: 'user', content: [{ type: 'text', text: 'hello' }] }, sessionId: 's2', cwd: 'C:\\proj', timestamp: '2026-01-01T00:00:00.000Z' }),
  JSON.stringify({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: 'running' }, { type: 'tool_use', id: 'c2', name: 'Bash', input: { command: 'ls' } }] }, sessionId: 's2', timestamp: '2026-01-01T00:00:01.000Z' }),
  JSON.stringify({ type: 'user', message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'c2', content: 'ok' }] }, sessionId: 's2', timestamp: '2026-01-01T00:00:02.000Z' }),
  JSON.stringify({ type: 'file-history-snapshot', messageId: 'm', snapshot: {} }),
].join('\n');

describe('claude read', () => {
  it('ignores mode and file-history entries', () => {
    const session = parseClaudeSession(fixture);
    expect(session.sourceTool).toBe('claude');
    expect(session.sessionId).toBe('s2');
    expect(session.cwd).toBe('C:\\proj');
    expect(session.messages.map((m) => m.role)).toEqual(['user', 'assistant']);
  });

  it('maps tool use and attaches tool results', () => {
    const session = parseClaudeSession(fixture);
    const assistant = session.messages.find((m) => m.role === 'assistant')!;
    expect(assistant.text).toBe('running');
    expect(assistant.toolCalls).toHaveLength(1);
    expect(assistant.toolCalls[0]).toMatchObject({ id: 'c2', name: 'Bash', input: { command: 'ls' }, output: 'ok' });
  });
});
