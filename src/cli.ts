import { daemonCommand } from './cli/daemon';
import { restoreCommand } from './cli/restore';
import type { SourceTool } from './canonical/types';
import { exportSession, listSessions, searchSessions } from './store/store';

const VERSION = '0.1.0';

export function main(argv: string[]): void {
  const [command, ...rest] = argv;
  switch (command) {
    case 'restore':
      runRestore(rest);
      return;
    case 'list':
      runList(rest);
      return;
    case 'search':
      runSearch(rest);
      return;
    case 'export':
      runExport(rest);
      return;
    case 'daemon':
      daemonCommand(rest);
      return;
    case '--version':
    case '-V':
      console.log(VERSION);
      return;
    case '--help':
    case '-h':
    case undefined:
      printHelp();
      return;
    default:
      console.error(`unknown command: ${command}`);
      printHelp();
      process.exitCode = 2;
  }
}

function runRestore(args: string[]): void {
  let target: SourceTool | undefined;
  let sessionId: string | undefined;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--to') {
      target = args[++i] as SourceTool;
    } else if (!sessionId) {
      sessionId = args[i];
    }
  }
  if (target !== 'codex' && target !== 'claude') {
    console.error('restore requires --to <codex|claude>');
    process.exitCode = 2;
    return;
  }
  const result = restoreCommand(target, sessionId);
  if (result.ok) {
    console.log(result.message);
  } else {
    console.error(result.message);
    process.exitCode = 1;
  }
}

function runList(args: string[]): void {
  let tool: SourceTool | undefined;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--tool') {
      tool = args[++i] as SourceTool;
    }
  }
  for (const session of listSessions(tool)) {
    console.log(`${session.sessionId}\t${session.sourceTool}\t${session.cwd}\t${session.updatedAt}`);
  }
}

function runSearch(args: string[]): void {
  const query = args.join(' ').trim();
  if (!query) {
    console.error('search requires a query');
    process.exitCode = 2;
    return;
  }
  for (const session of searchSessions(query)) {
    console.log(`${session.sessionId}\t${session.sourceTool}\t${session.cwd}\t${session.updatedAt}`);
  }
}

function runExport(args: string[]): void {
  const sessionId = args[0];
  if (!sessionId) {
    console.error('export requires a session id');
    process.exitCode = 2;
    return;
  }
  try {
    process.stdout.write(exportSession(sessionId));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

function printHelp(): void {
  console.log('session-loom - mirror coding-agent sessions into a canonical format');
  console.log();
  console.log('Usage: ssl <command> [options]');
  console.log();
  console.log('Commands:');
  console.log('  daemon [start|stop|status]  Run the background mirror (default: start)');
  console.log('  restore --to <codex|claude> [session-id]  Restore a canonical session');
  console.log('  list [--tool <codex|claude>]  List canonical sessions');
  console.log('  search <query>  Search sessions by text or path');
  console.log('  export <session-id>  Export a canonical session as JSON');
}

main(process.argv.slice(2));
