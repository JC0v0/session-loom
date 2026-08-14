import { CANONICAL_SCHEMA_VERSION, type CanonicalSession } from './types';

export function serialize(session: CanonicalSession): string {
  return JSON.stringify(session, null, 2);
}

export function deserialize(input: string): CanonicalSession {
  let parsed: unknown;
  try {
    parsed = JSON.parse(input);
  } catch {
    throw new Error('Invalid canonical JSON');
  }
  if (!isObject(parsed) || parsed.schemaVersion !== CANONICAL_SCHEMA_VERSION) {
    throw new Error('Unsupported canonical schema version');
  }
  return parsed as unknown as CanonicalSession;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}
