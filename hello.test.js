import { test } from 'node:test';
import assert from 'node:assert/strict';
import { hello } from './hello.js';

test('hello returns "hello xiaoyou code"', () => {
  assert.equal(hello(), 'hello xiaoyou code');
});
