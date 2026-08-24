import { fileURLToPath } from 'node:url';

export function hello() {
  return 'hello xiaoyou code';
}

if (process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url)) {
  console.log(hello());
}
