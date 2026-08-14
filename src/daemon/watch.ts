import { readdirSync, statSync, type Dirent } from 'node:fs';
import { join, resolve } from 'node:path';
import type { SourceTool } from '../canonical/types';
import { mirrorSession } from './mirror';

export interface WatchTarget {
  sourceTool: SourceTool;
  root: string;
}

export interface WatchState {
  stop: () => void;
}

export function startWatcher(targets: WatchTarget[], options?: { intervalMs?: number }): WatchState {
  const intervalMs = options?.intervalMs ?? 2000;
  const seen = new Map<string, string>();

  const scan = (): void => {
    for (const target of targets) {
      for (const file of listSessionFiles(target.root)) {
        const stat = statSync(file);
        const signature = `${stat.mtimeMs}:${stat.size}`;
        const key = `${target.sourceTool}:${file}`;
        if (seen.get(key) !== signature) {
          seen.set(key, signature);
          try {
            mirrorSession(file, target.sourceTool);
          } catch {
            // A partially written session may fail to parse; retry on the next change.
          }
        }
      }
    }
  };

  scan();
  const timer = setInterval(scan, intervalMs);
  return { stop: () => clearInterval(timer) };
}

function listSessionFiles(root: string): string[] {
  const out: string[] = [];
  walk(root, out);
  return out;
}

function walk(dir: string, out: string[]): void {
  const entries = readdirSafe(dir);
  if (!entries) return;
  for (const entry of entries) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(full, out);
    } else if (entry.isFile() && entry.name.endsWith('.jsonl')) {
      out.push(resolve(full));
    }
  }
}

function readdirSafe(dir: string): Dirent[] | undefined {
  try {
    return readdirSync(dir, { withFileTypes: true });
  } catch {
    return undefined;
  }
}
