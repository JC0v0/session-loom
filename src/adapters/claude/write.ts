import { randomUUID } from 'node:crypto';
import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import type { CanonicalSession, Message } from '../../canonical/types';
import { encodeClaudeProject } from '../common';

export interface ClaudeWriteResult {
  sessionFile: string;
  sessionId: string;
}

export function writeClaudeSession(session: CanonicalSession, options?: { root?: string }): ClaudeWriteResult {
  const root = options?.root ?? join(homedir(), '.claude');
  const project = encodeClaudeProject(session.cwd);
  const sessionId = randomUUID();
  const dir = join(root, 'projects', project);
  mkdirSync(dir, { recursive: true });
  const sessionFile = join(dir, `${sessionId}.jsonl`);

  const lines: string[] = [JSON.stringify({ type: 'mode', mode: 'normal', sessionId })];

  for (const message of session.messages) {
    if (message.role === 'assistant') {
      lines.push(
        JSON.stringify({
          type: 'assistant',
          message: { role: 'assistant', content: contentBlocks(message) },
          sessionId,
          cwd: session.cwd,
          timestamp: session.updatedAt,
        }),
      );
      for (const call of message.toolCalls) {
        if (call.output !== undefined) {
          lines.push(
            JSON.stringify({
              type: 'user',
              message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: call.id, content: call.output }] },
              sessionId,
              cwd: session.cwd,
              timestamp: session.updatedAt,
            }),
          );
        }
      }
    } else {
      lines.push(
        JSON.stringify({
          type: 'user',
          message: { role: 'user', content: contentBlocks(message) },
          sessionId,
          cwd: session.cwd,
          timestamp: session.updatedAt,
        }),
      );
    }
  }

  writeFileSync(sessionFile, `${lines.join('\n')}\n`, 'utf8');
  appendHistory(root, session, sessionId);
  return { sessionFile, sessionId };
}

function contentBlocks(message: Message): unknown[] {
  const blocks: unknown[] = [];
  if (message.text.length > 0) {
    blocks.push({ type: 'text', text: message.text });
  }
  for (const call of message.toolCalls) {
    blocks.push({ type: 'tool_use', id: call.id, name: call.name, input: call.input });
  }
  return blocks;
}

function appendHistory(root: string, session: CanonicalSession, sessionId: string): void {
  const entry = {
    display: firstUserText(session).slice(0, 200),
    pastedContents: {},
    timestamp: Date.parse(session.createdAt) || Date.now(),
    project: session.cwd,
    sessionId,
  };
  appendFileSync(join(root, 'history.jsonl'), `${JSON.stringify(entry)}\n`, 'utf8');
}

function firstUserText(session: CanonicalSession): string {
  return session.messages.find((m) => m.role === 'user')?.text ?? '';
}
