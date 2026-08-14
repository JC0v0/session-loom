const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const html = fs.readFileSync(path.join(root, 'ui', 'index.html'), 'utf8');

const { DatabaseSync } = require('node:sqlite');
const dbPath = process.env.SESSION_BRIDGE_STORE
  ? path.join(process.env.SESSION_BRIDGE_STORE, 'sessions.db')
  : path.join(process.env.USERPROFILE, '.session-bridge', 'sessions.db');
const db = new DatabaseSync(dbPath);
const row = db
  .prepare('SELECT payload FROM sessions WHERE session_id = ?')
  .get(process.argv[2] ?? '019ffeb7-ab30-78e3-bb00-2ef609cf4624');
db.close();
if (!row) {
  console.error('session not found in db');
  process.exit(1);
}

const sessionDetail = JSON.parse(row.payload);

const unusedShape = {
  schemaVersion: 1,
  sourceTool: 'codex',
  sessionId: 'demo',
  cwd: 'F:/demo',
  createdAt: '2026-08-14T00:00:00Z',
  updatedAt: '2026-08-14T01:00:00Z',
  messages: [
    {
      role: 'user',
      text: '我想做一个可以统一 Claude Code, codex, OpenCode 这些 agent 会话的工具，效果就 Claude Code 的会话可以导入到 codex 中反之也可以',
      toolCalls: [],
    },
    {
      role: 'assistant',
      text: '好的，这是一个很有价值的方向。我先了解一下两个工具的会话格式。',
      toolCalls: [
        {
          id: 't1',
          name: 'shell_command',
          input: { command: 'Get-ChildItem ~/.claude/projects' },
          output: 'Mode  Name\nd----  projects',
        },
      ],
    },
    { role: 'user', text: '续接任务优先', toolCalls: [] },
    { role: 'assistant', text: '收到，优先保证会话可以在对端工具里续接。', toolCalls: [] },
  ],
};

const embeddedB64 = Buffer.from(JSON.stringify(sessionDetail), 'utf8').toString('base64');
const mock = [
  'const sessionDetail = JSON.parse(new TextDecoder().decode(Uint8Array.from(atob("' + embeddedB64 + '"), (c) => c.charCodeAt(0))));',
  'window.__TAURI__ = { core: { invoke: async (cmd, args) => {',
  '  if (cmd === "sessions_list") return [{ sessionId: "demo", sourceTool: "codex", cwd: "F:/demo", createdAt: "2026-08-14T00:00:00Z", updatedAt: "2026-08-14T01:00:00Z", title: "demo session", messageCount: 4 }];',
  '  if (cmd === "sessions_get") return sessionDetail;',
  '  if (cmd === "daemon_status") return { running: false, pid: null };',
  '  return null;',
  '} } };',
].join('\n');

const probe = [
  'setTimeout(() => {',
  '  document.querySelector(".card").click();',
  '  setTimeout(() => {',
  '    const pick = (sel) => {',
  '      const el = document.querySelector(sel);',
  '      if (!el) return sel + " MISSING";',
  '      const r = el.getBoundingClientRect();',
  '      return sel + " x=" + Math.round(r.x) + " w=" + Math.round(r.width) + " h=" + Math.round(r.height);',
  '    };',
  '    const body = document.querySelector("#detailBody");',
  '    const conv = document.querySelector("#conversation");',
  '    const lines = [',
  '      pick("main"), pick("#detail"), pick("#detailBody"), pick(".detail-head"),',
  '      pick("#conversation"), pick(".msg"), pick(".msg .text"),',
  '      "body scrollWidth=" + document.body.scrollWidth + " clientWidth=" + document.body.clientWidth,',
  '      "detailBody display=" + getComputedStyle(body).display + " direction=" + getComputedStyle(body).flexDirection + " flex=" + getComputedStyle(body).flex,',
  '      "conversation display=" + getComputedStyle(conv).display + " overflowX=" + getComputedStyle(conv).overflowX,',
  '    ];',
  '    const pre = document.createElement("pre");',
  '    pre.id = "diag";',
  '    pre.textContent = lines.join("\\n");',
  '    document.body.appendChild(pre);',
  '  }, 800);',
  '}, 800);',
].join('\n');

const out = html
  .replace('<script>', '<script>\n' + mock + '\n')
  .replace('</body>', '<script>\n' + probe + '\n</scr' + 'ipt>\n</body>');

fs.writeFileSync(path.join(root, '.debug-ui.html'), out);

const block = out.match(/<script>([\s\S]*?)<\/script>/);
fs.writeFileSync(path.join(root, '.debug-script.js'), block ? block[1] : '');
console.log('debug page written, script block chars =', block ? block[1].length : 0);
