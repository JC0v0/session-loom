import { daemonCommand } from './cli/daemon';
import { restoreCommand } from './cli/restore';
import type { SourceTool } from './canonical/types';
import { listSessions } from './store/store';

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

function printHelp(): void {
  console.log('session-bridge - mirror coding-agent sessions into a canonical format');
  console.log();
  console.log('Usage: session-bridge <command> [options]');
  console.log();
  console.log('Commands:');
  console.log('  daemon [start|stop|status]  Run the background mirror (default: start)');
  console.log('  restore --to <codex|claude> [session-id]  Restore a canonical session');
  console.log('  list [--tool <codex|claude>]  List canonical sessions');
}

main(process.argv.slice(2));
