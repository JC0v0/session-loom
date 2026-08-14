import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { deserialize, serialize } from '../canonical/serialize';
import type { CanonicalSession, SourceTool } from '../canonical/types';

export function storeRoot(): string {
  return process.env.SESSION_BRIDGE_STORE ?? join(homedir(), '.session-bridge', 'store');
}

function fileFor(sourceTool: SourceTool, sessionId: string): string {
  return join(storeRoot(), sourceTool, `${sessionId}.json`);
}

export function writeSession(session: CanonicalSession): string {
  const file = fileFor(session.sourceTool, session.sessionId);
  mkdirSync(dirname(file), { recursive: true });
  const content = serialize(session);
  if (existsSync(file) && readFileSync(file, 'utf8') === content) {
    return file;
  }
  writeFileSync(file, content, 'utf8');
  return file;
}

export function readSession(sourceTool: SourceTool, sessionId: string): CanonicalSession {
  return deserialize(readFileSync(fileFor(sourceTool, sessionId), 'utf8'));
}

export function listSessions(filterTool?: SourceTool): CanonicalSession[] {
  const root = storeRoot();
  const tools: SourceTool[] = filterTool ? [filterTool] : ['codex', 'claude'];
  const result: CanonicalSession[] = [];
  for (const tool of tools) {
    const dir = join(root, tool);
    if (!existsSync(dir)) continue;
    for (const entry of readdirSync(dir)) {
      if (!entry.endsWith('.json')) continue;
      try {
        result.push(deserialize(readFileSync(join(dir, entry), 'utf8')));
      } catch {
        // Skip unreadable entries.
      }
    }
  }
  result.sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
  return result;
}
