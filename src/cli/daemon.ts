import { spawn } from 'node:child_process';
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { startWatcher } from '../daemon/watch';
import { claudeSessionsRoot, codexSessionsRoot, sessionStoreRoot } from '../paths';

const pidFile = join(sessionStoreRoot(), 'daemon.pid');

export function daemonCommand(args: string[]): void {
  switch (args[0] ?? 'run') {
    case 'run':
      runForeground();
      return;
    case 'start':
      startDetached();
      return;
    case 'stop':
      stop();
      return;
    case 'status':
      status();
      return;
    default:
      console.error(`unknown daemon subcommand: ${args[0]}`);
      process.exitCode = 2;
  }
}

function runForeground(): void {
  console.log('session-loom daemon running');
  startWatcher([
    { sourceTool: 'codex', root: codexSessionsRoot() },
    { sourceTool: 'claude', root: claudeSessionsRoot() },
  ]);
}

function startDetached(): void {
  if (isRunning()) {
    console.log('already running');
    return;
  }
  const cliPath = join(dirname(fileURLToPath(import.meta.url)), '..', 'cli.ts');
  const child = spawn(process.execPath, ['--import', 'tsx', cliPath, 'daemon', 'run'], { detached: true, stdio: 'ignore' });
  child.unref();
  writeFileSync(pidFile, String(child.pid), 'utf8');
  console.log(`daemon started (pid ${child.pid})`);
}

function stop(): void {
  if (!existsSync(pidFile)) {
    console.log('stopped');
    return;
  }
  const pid = Number(readFileSync(pidFile, 'utf8').trim());
  try {
    process.kill(pid);
  } catch {
    // Process is already gone.
  }
  rmSync(pidFile, { force: true });
  console.log('stopped');
}

function status(): void {
  console.log(isRunning() ? 'running' : 'stopped');
}

function isRunning(): boolean {
  if (!existsSync(pidFile)) return false;
  const pid = Number(readFileSync(pidFile, 'utf8').trim());
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}
