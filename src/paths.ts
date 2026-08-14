import { homedir } from 'node:os';
import { join } from 'node:path';

export function codexSessionsRoot(): string {
  return process.env.CODEX_SESSIONS_ROOT ?? join(homedir(), '.codex', 'sessions');
}

export function claudeSessionsRoot(): string {
  return process.env.CLAUDE_ROOT ?? join(homedir(), '.claude', 'projects');
}
