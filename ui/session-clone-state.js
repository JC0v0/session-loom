// Optional attachments for the existing single-session clone action.
export const READINESS_LABELS = {
  available: '已经可用', needsConfiguration: '需要配置', unsupported: '暂不支持', failed: '失败',
};
export const KIND_LABELS = {
  memory: '记忆', instruction: '项目指令', skill: '技能', mcp: '工具连接', script: '脚本', command: '命令',
};
const REDACTED_MARKERS = ['[redacted]', '[redacted-url]'];

export function createMigrationState() {
  return {
    open: false, step: 'items', busy: false, loading: false, error: '', scanError: '',
    requestSeq: 0, source: null, targetTool: '', action: 'restore',
    discovery: null, run: null, assetSelection: new Set(),
    preview: null, previewDirty: false, decisions: {}, results: null, mergeDrafts: {},
  };
}

export function startClone(state, source, targetTool, action = 'restore') {
  const seq = state.requestSeq + 1;
  Object.assign(state, createMigrationState(), { open: true, source, targetTool, action, requestSeq: seq });
}
export function beginRequest(state) { return { seq: ++state.requestSeq }; }
export function isCurrent(state, token) { return Boolean(token) && token.seq === state.requestSeq; }
export function invalidateRequests(state) { state.requestSeq += 1; }
export function assetsForStep(state) {
  return (state.discovery?.report.assets || [])
    .filter((asset) => asset.sourceActive !== false)
    .map((asset) => ({ ...asset, selected: state.assetSelection.has(asset.id), locked: false }));
}
export function toggleAsset(state, id) {
  const asset = state.discovery?.report.assets.find((entry) => entry.id === id);
  if (!asset) throw new Error('unknown environment item');
  if (asset.sourceActive === false) throw new Error('inactive environment item');
  if (state.assetSelection.has(id)) state.assetSelection.delete(id);
  else state.assetSelection.add(id);
  // A change of selection must never apply a previous preview.
  state.previewDirty = true;
  state.decisions = {};
  state.mergeDrafts = {};
  return state.assetSelection.has(id);
}
export function captureAssetIds(state) { return [...state.assetSelection]; }
export function setDiscovery(state, discovery) {
  state.discovery = discovery;
  state.run = discovery.run;
  state.assetSelection = new Set();
  state.preview = null;
  state.previewDirty = false;
  state.decisions = {};
  state.mergeDrafts = {};
  state.results = null;
}
export function setPreview(state, preview) {
  state.preview = preview;
  state.previewDirty = false;
  if (state.run) state.run.preview = { id: preview.id };
}
export function unresolvedCount(preview) {
  return (preview?.conflicts || []).filter((entry) => !entry.resolved).length;
}
export function canApply(state) {
  return !state.previewDirty && state.preview?.readyToApply === true && unresolvedCount(state.preview) === 0;
}
export function setDecision(state, item, choice, merged) {
  if (!['keep', 'replace', 'merge'].includes(choice)) throw new Error('无效的处理方式');
  const decision = { decisionId: item.decisionId, itemId: item.itemId, choice };
  if (choice === 'merge') {
    if (item.diff?.kind === 'binary') throw new Error('二进制文件只支持保留或替换');
    const content = typeof merged === 'string' ? merged : '';
    if (!content.trim()) throw new Error('合并内容不能为空');
    if (REDACTED_MARKERS.some((marker) => content.includes(marker))) throw new Error('脱敏占位符不能作为最终内容');
    if (item.contributionKind === 'configValue') {
      try { decision.mergedValue = JSON.parse(content); }
      catch { throw new Error('配置合并内容必须是有效 JSON'); }
    } else decision.mergedContent = content;
    state.mergeDrafts[item.decisionId] = content;
  }
  state.decisions[item.decisionId] = decision;
  state.previewDirty = true;
  return decision;
}
export function decisionList(state) { return Object.values(state.decisions); }
export function initialDraft(item) {
  if (item.diff?.kind !== 'text') return '';
  return item.diff.diff.lines.filter((line) => line.kind !== 'removed').map((line) => line.text).join('');
}
export function diffRows(item) {
  const diff = item?.diff;
  if (!diff) return [];
  if (diff.kind === 'text') return diff.diff.lines;
  if (diff.kind === 'binary') return [{ kind: 'binary', text: `二进制内容 ${diff.oldSize} → ${diff.newSize} 字节` }];
  return [{ kind: 'missing', text: `目标尚不存在，将新增 ${diff.addedBytes} 字节` }];
}
export function classifyResults(report) {
  const groups = { available: [], needsConfiguration: [], unsupported: [], failed: [] };
  for (const item of report?.items || []) (groups[item.readiness] || groups.failed).push(item);
  return groups;
}
export function retryPreviewId(state) { return state.preview?.id || state.run?.preview?.id || null; }
export function canRetryPartial(report, previewId) { return report?.state === 'partial' && Boolean(previewId); }
export function resultsSummary(report) {
  const groups = classifyResults(report);
  return {
    total: Object.values(groups).reduce((sum, items) => sum + items.length, 0),
    ...Object.fromEntries(Object.entries(groups).map(([key, items]) => [key, items.length])),
    ok: report?.state === 'completed',
  };
}

// Store only a pointer to the durable run, scoped to the clicked native
// session and destination. Never keep merge text or configuration in storage.
function resumeKey(sessionId, targetTool) { return `loomClone:${sessionId}:${targetTool}`; }
function browserStorage(storage) {
  try { return storage === undefined ? globalThis.localStorage : storage; }
  catch { return null; }
}
export function rememberRun(state, storage) {
  if (!state.source || !state.run?.id) return;
  try {
    browserStorage(storage)?.setItem(resumeKey(state.source.sessionId, state.targetTool), JSON.stringify({ runId: state.run.id }));
  } catch { /* Storage is optional; the durable run remains in the core. */ }
}
export function forgetRun(sessionId, targetTool, storage) {
  try { browserStorage(storage)?.removeItem(resumeKey(sessionId, targetTool)); } catch { /* best effort */ }
}
export function loadRememberedRun(sessionId, targetTool, storage) {
  try {
    const stored = JSON.parse(browserStorage(storage)?.getItem(resumeKey(sessionId, targetTool)) || 'null');
    return typeof stored?.runId === 'string' ? stored : null;
  } catch { return null; }
}
