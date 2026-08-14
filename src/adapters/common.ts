import type { Message } from '../canonical/types';

export function stringify(value: unknown): string {
  if (typeof value === 'string') return value;
  if (value === undefined || value === null) return '';
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

export function parseJsonArgs(value: unknown): unknown {
  if (typeof value !== 'string') return value;
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

export function attachToolOutputs(messages: Message[], outputs: Map<string, string>): void {
  for (const message of messages) {
    for (const call of message.toolCalls) {
      const output = outputs.get(call.id);
      if (output !== undefined) call.output = output;
    }
  }
}

export function encodeClaudeProject(cwd: string): string {
  return cwd.replace(/[:\\/]/g, '-');
}
