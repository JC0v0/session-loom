import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CANONICAL_SCHEMA_VERSION, type CanonicalSession } from '../canonical/types';
import { closeStore, writeSession } from '../store/store';
import { restoreCommand } from './restore';

let store: string;
let codexRoot: string;
let claudeRoot: string;

beforeEach(() => {
  store = mkdtempSync(join(tmpdir(), 'sb-store-'));
  codexRoot = mkdtempSync(join(tmpdir(), 'sb-codex-'));
  claudeRoot = mkdtempSync(join(tmpdir(), 'sb-claude-'));
  process.env.SESSION_LOOM_STORE = store;
  process.env.CODEX_SESSIONS_ROOT = codexRoot;
  process.env.CLAUDE_ROOT = claudeRoot;
});

afterEach(() => {
  closeStore();
  rmSync(store, { recursive: true, force: true });
  rmSync(codexRoot, { recursive: true, force: true });
  rmSync(claudeRoot, { recursive: true, force: true });
});

const session: CanonicalSession = {
  schemaVersion: CANONICAL_SCHEMA_VERSION,
  sourceTool: 'codex',
  sessionId: 's1',
  cwd: 'C:\\proj',
  createdAt: '2026-01-01T00:00:00.000Z',
  updatedAt: '2026-01-01T00:00:01.000Z',
  messages: [
    { role: 'user', text: 'hello', toolCalls: [] },
    { role: 'assistant', text: 'ok', toolCalls: [] },
  ],
};

describe('restore command', () => {
  it('restores a canonical session to Codex', () => {
    writeSession(session);
    const result = restoreCommand('codex', 's1');
    expect(result.ok).toBe(true);
    const files = walkJsonl(codexRoot);
    expect(files).toHaveLength(1);
    expect(readFileSync(files[0], 'utf8')).toContain('"session_meta"');
  });

  it('restores a canonical session to Claude Code', () => {
    writeSession(session);
    const result = restoreCommand('claude', 's1');
    expect(result.ok).toBe(true);
    expect(existsSync(join(claudeRoot, 'projects', 'C--proj'))).toBe(true);
    expect(readFileSync(join(claudeRoot, 'history.jsonl'), 'utf8').trim().length).toBeGreaterThan(0);
  });

  it('reports failure for a missing session', () => {
    expect(restoreCommand('codex', 'missing').ok).toBe(false);
  });
});

function walkJsonl(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walkJsonl(full));
    else if (entry.name.endsWith('.jsonl')) out.push(full);
  }
  return out;
}
