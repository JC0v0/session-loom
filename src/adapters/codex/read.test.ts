import { describe, it, expect } from 'vitest';
import { parseCodexSession } from './read';

const fixture = [
  JSON.stringify({ timestamp: '2026-01-01T00:00:00.000Z', type: 'session_meta', payload: { session_id: 's1', cwd: 'C:\\proj', timestamp: '2026-01-01T00:00:00.000Z', base_instructions: { text: 'You are Codex' } } }),
  JSON.stringify({ timestamp: '2026-01-01T00:00:01.000Z', type: 'response_item', payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'do it' }] } }),
  JSON.stringify({ timestamp: '2026-01-01T00:00:02.000Z', type: 'response_item', payload: { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'ok' }] } }),
  JSON.stringify({ timestamp: '2026-01-01T00:00:03.000Z', type: 'response_item', payload: { type: 'function_call', call_id: 'c1', name: 'shell', arguments: '{"cmd":"ls"}' } }),
  JSON.stringify({ timestamp: '2026-01-01T00:00:04.000Z', type: 'response_item', payload: { type: 'function_call_output', call_id: 'c1', output: 'file.txt' } }),
  JSON.stringify({ timestamp: '2026-01-01T00:00:05.000Z', type: 'response_item', payload: { type: 'message', role: 'developer', content: [{ type: 'input_text', text: 'system prompt' }] } }),
].join('\n');

describe('codex read', () => {
  it('parses session metadata and messages without the system prompt', () => {
    const session = parseCodexSession(fixture);
    expect(session.sourceTool).toBe('codex');
    expect(session.sessionId).toBe('s1');
    expect(session.cwd).toBe('C:\\proj');
    expect(session.messages.map((m) => m.role)).toEqual(['user', 'assistant']);
    expect(session.messages.some((m) => m.text.includes('system prompt'))).toBe(false);
  });

  it('maps function calls and attaches outputs', () => {
    const session = parseCodexSession(fixture);
    const assistant = session.messages.find((m) => m.role === 'assistant')!;
    expect(assistant.toolCalls).toHaveLength(1);
    expect(assistant.toolCalls[0]).toMatchObject({ id: 'c1', name: 'shell', input: { cmd: 'ls' }, output: 'file.txt' });
  });
});
