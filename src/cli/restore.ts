import { randomUUID } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { writeClaudeSession } from '../adapters/claude/write';
import { writeCodexSession } from '../adapters/codex/write';
import type { CanonicalSession, SourceTool } from '../canonical/types';
import { codexSessionsRoot } from '../paths';
import { listSessions } from '../store/store';

export interface RestoreResult {
  ok: boolean;
  message: string;
}

export function restoreCommand(target: SourceTool, sessionId?: string): RestoreResult {
  const sessions = listSessions();
  const session = sessionId ? sessions.find((s) => s.sessionId === sessionId) : sessions[0];

  if (!session) {
    return { ok: false, message: sessionId ? `session not found: ${sessionId}` : 'no canonical sessions available' };
  }

  if (target === 'claude') {
    const result = writeClaudeSession(session, { root: process.env.CLAUDE_ROOT });
    return { ok: true, message: `restored to Claude Code: ${result.sessionId} (${result.sessionFile})` };
  }

  return restoreToCodex(session);
}

function restoreToCodex(session: CanonicalSession): RestoreResult {
  const sessionId = randomUUID();
  const now = new Date();
  const y = String(now.getFullYear());
  const m = String(now.getMonth() + 1).padStart(2, '0');
  const d = String(now.getDate()).padStart(2, '0');
  const dir = join(codexSessionsRoot(), y, m, d);
  mkdirSync(dir, { recursive: true });
  const ts = `${y}-${m}-${d}T${pad(now.getHours())}-${pad(now.getMinutes())}-${pad(now.getSeconds())}`;
  const file = join(dir, `rollout-${ts}-${sessionId}.jsonl`);
  writeFileSync(file, writeCodexSession({ ...session, sessionId }), 'utf8');
  return { ok: true, message: `restored to Codex: ${sessionId} (${file})` };
}

function pad(n: number): string {
  return String(n).padStart(2, '0');
}
