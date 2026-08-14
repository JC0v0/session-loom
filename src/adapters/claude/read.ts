import { CANONICAL_SCHEMA_VERSION, type CanonicalSession, type Message } from '../../canonical/types';
import { attachToolOutputs, stringify } from '../common';

interface ClaudeBlock {
  type?: string;
  text?: string;
  id?: string;
  name?: string;
  input?: unknown;
  tool_use_id?: string;
  content?: unknown;
}

interface ClaudeRecord {
  type?: string;
  timestamp?: string;
  sessionId?: string;
  cwd?: string;
  message?: {
    role?: string;
    content?: ClaudeBlock[];
  };
}

export function parseClaudeSession(jsonl: string): CanonicalSession {
  const messages: Message[] = [];
  const outputs = new Map<string, string>();
  let sessionId = '';
  let cwd = '';
  let createdAt = '';
  let updatedAt = '';

  for (const line of jsonl.split(/\r?\n/)) {
    if (line.trim() === '') continue;
    let record: ClaudeRecord;
    try {
      record = JSON.parse(line) as ClaudeRecord;
    } catch {
      continue;
    }

    if (record.sessionId) sessionId = record.sessionId;
    if (record.cwd) cwd = record.cwd;
    if (record.timestamp) {
      if (!createdAt) createdAt = record.timestamp;
      updatedAt = record.timestamp;
    }

    if (record.type !== 'user' && record.type !== 'assistant') continue;
    const content = record.message?.content;
    if (!Array.isArray(content)) continue;

    const role = record.message?.role === 'assistant' ? 'assistant' : 'user';
    const message: Message = { role, text: '', toolCalls: [] };

    for (const block of content) {
      if (block.type === 'text') {
        message.text += block.text ?? '';
      } else if (block.type === 'tool_use') {
        message.toolCalls.push({
          id: String(block.id ?? ''),
          name: String(block.name ?? ''),
          input: block.input,
        });
      } else if (block.type === 'tool_result') {
        outputs.set(String(block.tool_use_id ?? ''), stringify(block.content));
      }
    }

    if (message.text.length > 0 || message.toolCalls.length > 0) {
      messages.push(message);
    }
  }

  attachToolOutputs(messages, outputs);

  return {
    schemaVersion: CANONICAL_SCHEMA_VERSION,
    sourceTool: 'claude',
    sessionId,
    cwd,
    createdAt,
    updatedAt,
    messages,
  };
}
