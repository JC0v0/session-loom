import type { CanonicalSession } from '../../canonical/types';

export function writeCodexSession(session: CanonicalSession): string {
  const lines: string[] = [];

  lines.push(
    JSON.stringify({
      timestamp: session.updatedAt,
      type: 'session_meta',
      payload: {
        session_id: session.sessionId,
        id: session.sessionId,
        timestamp: session.createdAt,
        cwd: session.cwd,
        originator: 'codex-tui',
        source: 'cli',
        thread_source: 'user',
        cli_version: 'session-loom',
        history_mode: 'legacy',
      },
    }),
  );

  for (const message of session.messages) {
    lines.push(
      JSON.stringify({
        timestamp: session.updatedAt,
        type: 'response_item',
        payload: {
          type: 'message',
          role: message.role,
          content: [{ type: message.role === 'user' ? 'input_text' : 'output_text', text: message.text }],
        },
      }),
    );

    for (const call of message.toolCalls) {
      lines.push(
        JSON.stringify({
          timestamp: session.updatedAt,
          type: 'response_item',
          payload: {
            type: 'function_call',
            call_id: call.id,
            name: call.name,
            arguments: typeof call.input === 'string' ? call.input : JSON.stringify(call.input),
          },
        }),
      );
      if (call.output !== undefined) {
        lines.push(
          JSON.stringify({
            timestamp: session.updatedAt,
            type: 'response_item',
            payload: {
              type: 'function_call_output',
              call_id: call.id,
              output: call.output,
            },
          }),
        );
      }
    }
  }

  return `${lines.join('\n')}\n`;
}
