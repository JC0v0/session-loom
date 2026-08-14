import { readFileSync } from 'node:fs';
import { parseCodexSession } from '../adapters/codex/read';
import { parseClaudeSession } from '../adapters/claude/read';
import type { CanonicalSession, SourceTool } from '../canonical/types';
import { writeSession } from '../store/store';

export function mirrorSession(file: string, sourceTool: SourceTool): CanonicalSession {
  const text = readFileSync(file, 'utf8');
  const session = sourceTool === 'codex' ? parseCodexSession(text) : parseClaudeSession(text);
  writeSession(session);
  return session;
}
