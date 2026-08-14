import { CANONICAL_SCHEMA_VERSION, type CanonicalSession, type Message, type ToolCall } from '../../canonical/types';
import { attachToolOutputs, parseJsonArgs, stringify } from '../common';

interface CodexRecord {
  type?: string;
  timestamp?: string;
  payload?: {
    type?: string;
    role?: string;
    session_id?: string;
    id?: string;
    cwd?: string;
    timestamp?: string;
    name?: string;
    call_id?: string;
    arguments?: string;
    output?: unknown;
    content?: Array<{ type?: string; text?: string }>;
  };
}

export function parseCodexSession(jsonl: string): CanonicalSession {
  const messages: Message[] = [];
  const outputs = new Map<string, string>();
  let sessionId = '';
  let cwd = '';
  let createdAt = '';
  let updatedAt = '';

  for (const line of jsonl.split(/\r?\n/)) {
    if (line.trim() === '') continue;
    let record: CodexRecord;
    try {
      record = JSON.parse(line) as CodexRecord;
    } catch {
      continue;
    }

    if (record.type === 'session_meta') {
      const p = record.payload ?? {};
      sessionId = String(p.session_id ?? p.id ?? sessionId);
      cwd = String(p.cwd ?? cwd);
      createdAt = String(p.timestamp ?? createdAt);
      if (record.timestamp) updatedAt = record.timestamp;
      continue;
    }

    if (record.type !== 'response_item') continue;
    const item = record.payload;
    if (!item) continue;
    if (record.timestamp) updatedAt = record.timestamp;

    if (item.type === 'function_call_output') {
      outputs.set(String(item.call_id ?? ''), stringify(item.output));
      continue;
    }

    if (item.type === 'function_call') {
      const call: ToolCall = {
        id: String(item.call_id ?? ''),
        name: String(item.name ?? ''),
        input: parseJsonArgs(item.arguments),
      };
      const last = messages[messages.length - 1];
      if (last && last.role === 'assistant') {
        last.toolCalls.push(call);
      } else {
        messages.push({ role: 'assistant', text: '', toolCalls: [call] });
      }
      continue;
    }

    if (item.type === 'message') {
      if (item.role !== 'user' && item.role !== 'assistant') continue;
      const text = (item.content ?? [])
        .filter((block) => block.type === 'input_text' || block.type === 'output_text')
        .map((block) => block.text ?? '')
        .join('');
      messages.push({ role: item.role, text, toolCalls: [] });
    }
  }

  attachToolOutputs(messages, outputs);

  return {
    schemaVersion: CANONICAL_SCHEMA_VERSION,
    sourceTool: 'codex',
    sessionId,
    cwd,
    createdAt,
    updatedAt,
    messages,
  };
}
