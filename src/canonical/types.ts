export const CANONICAL_SCHEMA_VERSION = 1;

export type SourceTool = 'codex' | 'claude';

export type Role = 'user' | 'assistant';

export interface ToolCall {
  id: string;
  name: string;
  input: unknown;
  output?: string;
}

export interface Message {
  role: Role;
  text: string;
  toolCalls: ToolCall[];
}

export interface CanonicalSession {
  schemaVersion: number;
  sourceTool: SourceTool;
  sessionId: string;
  cwd: string;
  createdAt: string;
  updatedAt: string;
  messages: Message[];
}
