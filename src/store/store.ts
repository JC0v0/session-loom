import { existsSync, mkdirSync, readdirSync, readFileSync, renameSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { deserialize, serialize } from '../canonical/serialize';
import type { CanonicalSession, SourceTool } from '../canonical/types';

let db: DatabaseSync | undefined;

function storeDir(): string {
  return process.env.SESSION_BRIDGE_STORE ?? join(homedir(), '.session-bridge');
}

function getDb(): DatabaseSync {
  if (db) return db;
  mkdirSync(storeDir(), { recursive: true });
  db = new DatabaseSync(join(storeDir(), 'sessions.db'));
  db.exec(`
    CREATE TABLE IF NOT EXISTS sessions (
      session_id TEXT PRIMARY KEY,
      source_tool TEXT NOT NULL,
      cwd TEXT NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      schema_version INTEGER NOT NULL,
      payload TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);
    CREATE INDEX IF NOT EXISTS idx_sessions_tool ON sessions(source_tool);
  `);
  migrateLegacyJson();
  return db;
}

export function closeStore(): void {
  if (db) {
    db.close();
    db = undefined;
  }
}

function migrateLegacyJson(): void {
  const legacyDir = join(storeDir(), 'store');
  if (!existsSync(legacyDir)) return;
  const { n } = getDb().prepare('SELECT COUNT(*) AS n FROM sessions').get() as { n: number };
  if (n > 0) return;
  for (const tool of ['codex', 'claude'] as SourceTool[]) {
    const dir = join(legacyDir, tool);
    if (!existsSync(dir)) continue;
    for (const entry of readdirSync(dir)) {
      if (!entry.endsWith('.json')) continue;
      try {
        upsert(deserialize(readFileSync(join(dir, entry), 'utf8')));
      } catch {
        // Skip unreadable legacy entries.
      }
    }
  }
  try {
    renameSync(legacyDir, join(storeDir(), 'store.legacy'));
  } catch {
    // Leave the legacy directory in place if the rename fails.
  }
}

function upsert(session: CanonicalSession): void {
  getDb()
    .prepare(`
      INSERT INTO sessions (session_id, source_tool, cwd, created_at, updated_at, schema_version, payload)
      VALUES (?, ?, ?, ?, ?, ?, ?)
      ON CONFLICT(session_id) DO UPDATE SET
        source_tool = excluded.source_tool,
        cwd = excluded.cwd,
        created_at = excluded.created_at,
        updated_at = excluded.updated_at,
        schema_version = excluded.schema_version,
        payload = excluded.payload
    `)
    .run(
      session.sessionId,
      session.sourceTool,
      session.cwd,
      session.createdAt,
      session.updatedAt,
      session.schemaVersion,
      serialize(session),
    );
}

export function storeRoot(): string {
  return storeDir();
}

export function writeSession(session: CanonicalSession): string {
  upsert(session);
  return join(storeDir(), 'sessions.db');
}

export function readSession(_sourceTool: SourceTool, sessionId: string): CanonicalSession {
  const row = getDb().prepare('SELECT payload FROM sessions WHERE session_id = ?').get(sessionId) as
    | { payload: string }
    | undefined;
  if (!row) throw new Error(`session not found: ${sessionId}`);
  return deserialize(row.payload);
}

export function listSessions(filterTool?: SourceTool): CanonicalSession[] {
  const rows = filterTool
    ? (getDb().prepare('SELECT payload FROM sessions WHERE source_tool = ? ORDER BY updated_at DESC').all(filterTool) as { payload: string }[])
    : (getDb().prepare('SELECT payload FROM sessions ORDER BY updated_at DESC').all() as { payload: string }[]);
  return rows.map((row) => deserialize(row.payload));
}

export function searchSessions(query: string): CanonicalSession[] {
  const like = `%${query}%`;
  const rows = getDb()
    .prepare('SELECT payload FROM sessions WHERE payload LIKE ? OR cwd LIKE ? OR source_tool LIKE ? ORDER BY updated_at DESC')
    .all(like, like, like) as { payload: string }[];
  return rows.map((row) => deserialize(row.payload));
}

export function exportSession(sessionId: string): string {
  const row = getDb().prepare('SELECT payload FROM sessions WHERE session_id = ?').get(sessionId) as
    | { payload: string }
    | undefined;
  if (!row) throw new Error(`session not found: ${sessionId}`);
  return row.payload;
}
