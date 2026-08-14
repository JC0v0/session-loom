import { app, BrowserWindow, ipcMain } from 'electron';
import { spawn } from 'node:child_process';
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

const electronDir = dirname(fileURLToPath(import.meta.url));
const projectRoot = join(electronDir, '..');
const storeDir = process.env.SESSION_BRIDGE_STORE ?? join(homedir(), '.session-bridge');
const dbFile = join(storeDir, 'sessions.db');
const pidFile = join(storeDir, 'daemon.pid');

let db;

function getDb() {
  if (db) return db;
  db = new DatabaseSync(dbFile);
  try {
    db.exec('PRAGMA busy_timeout = 3000');
    db.exec('PRAGMA journal_mode = WAL');
  } catch {
    // Keep default journal mode if the daemon holds the database.
  }
  return db;
}

function parsePayload(payload) {
  try {
    return JSON.parse(payload);
  } catch {
    return undefined;
  }
}

function sessionTitle(session) {
  const firstUser = (session?.messages ?? []).find(
    (m) => m.role === 'user' && typeof m.text === 'string' && m.text.trim(),
  );
  if (!firstUser) return '(空会话)';
  const text = firstUser.text.trim().replace(/\s+/g, ' ');
  return text.length > 80 ? `${text.slice(0, 80)}…` : text;
}

function runCli(args) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, ['--import', 'tsx', join(projectRoot, 'src', 'cli.ts'), ...args], {
      cwd: projectRoot,
      env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' },
    });
    let out = '';
    let err = '';
    child.stdout.on('data', (d) => {
      out += d;
    });
    child.stderr.on('data', (d) => {
      err += d;
    });
    child.on('error', (e) => resolve({ ok: false, message: String(e) }));
    child.on('close', (code) => {
      const message = (out + err).trim();
      resolve({ ok: code === 0, message: message || `exit code ${code}` });
    });
  });
}

function daemonState() {
  if (!existsSync(pidFile)) return { running: false };
  const pid = Number(readFileSync(pidFile, 'utf8').trim());
  if (!Number.isFinite(pid)) return { running: false };
  try {
    process.kill(pid, 0);
    return { running: true, pid };
  } catch {
    return { running: false };
  }
}

ipcMain.handle('sessions:list', (_event, filter = {}) => {
  if (!existsSync(dbFile)) return [];
  const { tool, query } = filter;
  const clauses = [];
  const params = [];
  if (tool === 'codex' || tool === 'claude') {
    clauses.push('source_tool = ?');
    params.push(tool);
  }
  if (typeof query === 'string' && query.trim()) {
    clauses.push('(payload LIKE ? OR cwd LIKE ?)');
    params.push(`%${query.trim()}%`, `%${query.trim()}%`);
  }
  const where = clauses.length ? ` WHERE ${clauses.join(' AND ')}` : '';
  const rows = getDb()
    .prepare(`SELECT session_id, source_tool, cwd, created_at, updated_at, payload FROM sessions${where} ORDER BY updated_at DESC`)
    .all(...params);
  return rows.map((row) => {
    const session = parsePayload(row.payload);
    return {
      sessionId: row.session_id,
      sourceTool: row.source_tool,
      cwd: row.cwd,
      createdAt: row.created_at,
      updatedAt: row.updated_at,
      title: sessionTitle(session),
      messageCount: session?.messages?.length ?? 0,
    };
  });
});

ipcMain.handle('sessions:get', (_event, sessionId) => {
  if (!existsSync(dbFile)) throw new Error('store not found');
  const row = getDb().prepare('SELECT payload FROM sessions WHERE session_id = ?').get(sessionId);
  if (!row) throw new Error(`session not found: ${sessionId}`);
  return JSON.parse(row.payload);
});

ipcMain.handle('sessions:delete', (_event, sessionId) => {
  getDb().prepare('DELETE FROM sessions WHERE session_id = ?').run(sessionId);
  return { ok: true };
});

ipcMain.handle('sessions:restore', (_event, sessionId, target) => {
  if (target !== 'claude' && target !== 'codex') {
    return { ok: false, message: `unknown target: ${target}` };
  }
  return runCli(['restore', '--to', target, sessionId]);
});

ipcMain.handle('daemon:status', () => daemonState());

ipcMain.handle('daemon:toggle', () => {
  const state = daemonState();
  if (state.running) {
    try {
      process.kill(state.pid);
    } catch {
      // Process already exited.
    }
    rmSync(pidFile, { force: true });
    return { running: false };
  }
  const child = spawn(
    process.execPath,
    ['--import', 'tsx', join(projectRoot, 'src', 'cli.ts'), 'daemon', 'run'],
    {
      cwd: projectRoot,
      detached: true,
      stdio: 'ignore',
      env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' },
    },
  );
  child.unref();
  writeFileSync(pidFile, String(child.pid), 'utf8');
  return { running: true, pid: child.pid };
});

function createWindow() {
  const win = new BrowserWindow({
    width: 1280,
    height: 840,
    minWidth: 980,
    minHeight: 620,
    backgroundColor: '#0F172A',
    autoHideMenuBar: true,
    title: 'session-loom',
    webPreferences: {
      preload: join(electronDir, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  win.loadFile(join(electronDir, 'index.html'));
}

app.whenReady().then(() => {
  createWindow();
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});
