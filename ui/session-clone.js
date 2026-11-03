import {
  KIND_LABELS, READINESS_LABELS, assetsForStep, beginRequest, canApply,
  canRetryPartial, captureAssetIds, createMigrationState, decisionList, diffRows,
  forgetRun, initialDraft, invalidateRequests, isCurrent, loadRememberedRun,
  rememberRun, retryPreviewId, setDecision, setDiscovery, setPreview, startClone, toggleAsset,
} from './session-clone-state.js';

const TOOLS = { codex: 'Codex', claude: 'Claude', opencode: 'OpenCode', dsh: 'DSH', pi: 'Pi' };
const SCOPES = { project: '项目', user: '全局', profile: 'Profile' };
const errorMessage = (error) => error?.message || String(error);

// The API is the only external boundary. The same flow is exercised without
// a DOM in tests, including slow scans, cancellation and repeated submits.
export function createCloneFlow(api, changed = () => {}, completed = () => {}, storage) {
  const state = createMigrationState();
  let choices = {};
  const notify = () => changed(state);
  async function open(source, target, action = 'restore') {
    if (state.busy) {
      if (state.source?.sessionId !== source.sessionId || state.targetTool !== target) {
        throw new Error('当前复刻尚未结束，请稍后再试');
      }
      state.open = true;
      notify();
      return;
    }
    startClone(state, source, target, action);
    choices = {};
    state.loading = true;
    const token = beginRequest(state);
    notify();
    const remembered = loadRememberedRun(source.sessionId, target, storage);
    if (remembered) {
      try {
        const run = await api('migration_status', { runId: remembered.runId });
        if (!isCurrent(state, token)) return;
        if (run.sessionIds.length !== 1 || run.sessionIds[0] !== source.sessionId || run.target?.tool !== target) {
          throw new Error('复刻记录不匹配');
        }
        setDiscovery(state, { run, report: { assets: run.discoveredAssets || [], diagnostics: run.diagnostics || [] } });
        state.assetSelection = new Set((run.assets || []).map((asset) => asset.asset.id));
        if (run.preview) {
          const preview = await api('migration_preview_get', { runId: run.id });
          if (!isCurrent(state, token)) return;
          setPreview(state, preview);
          state.decisions = Object.fromEntries((preview.decisions || []).map((decision) => [decision.decisionId, decision]));
          choices = preview.environment || {};
        }
        state.results = run.applyReport || null;
        state.step = state.results ? 'results' : run.preview ? 'review' : 'items';
        if (run.state === 'applying' || run.state === 'interrupted') state.step = 'results';
        state.loading = false;
        notify();
        return;
      } catch {
        if (!isCurrent(state, token)) return;
        forgetRun(source.sessionId, target, storage);
      }
    }
    try {
      const prepared = await api('sessions_clone_prepare', { sessionId: source.sessionId, target });
      if (!isCurrent(state, token)) return;
      setDiscovery(state, prepared);
    } catch (error) {
      if (!isCurrent(state, token)) return;
      state.scanError = errorMessage(error);
    }
    if (isCurrent(state, token)) { state.loading = false; notify(); }
  }
  function close() {
    state.open = false;
    // Let an actual write finish and keep its result, but discard late scans
    // and previews that must never reopen a cancelled dialog.
    if (!state.busy) { invalidateRequests(state); state.loading = false; }
    notify();
  }
  async function preview() {
    if (state.busy || !state.run || !state.assetSelection.size) return;
    state.busy = true;
    state.error = '';
    state.previewDirty = true;
    const token = beginRequest(state);
    notify();
    try {
      const result = await api('sessions_clone_preview', {
        runId: state.run.id, assets: captureAssetIds(state), choices, decisions: decisionList(state),
      });
      if (!isCurrent(state, token)) return;
      setPreview(state, result);
      state.step = 'review';
      rememberRun(state, storage);
    } catch (error) {
      if (isCurrent(state, token)) state.error = errorMessage(error);
    } finally {
      if (isCurrent(state, token)) { state.busy = false; notify(); }
    }
  }
  async function sessionOnly() {
    if (state.busy || state.results) return;
    state.busy = true;
    state.loading = false;
    state.error = '';
    const token = beginRequest(state); // invalidates an outstanding scan
    notify();
    try {
      const result = await api(state.action === 'sync' ? 'sessions_sync' : 'sessions_restore', {
        sessionId: state.source.sessionId, target: state.targetTool,
      });
      if (!isCurrent(state, token)) return;
      if (!result.ok) throw new Error(result.message);
      state.results = { state: 'completed', items: [], sessions: [], sessionOnly: true };
      state.step = 'results';
      forgetRun(state.source.sessionId, state.targetTool, storage);
      await completed(result);
    } catch (error) {
      if (isCurrent(state, token)) state.error = errorMessage(error);
    } finally {
      if (isCurrent(state, token)) { state.busy = false; notify(); }
    }
  }
  async function apply() {
    const id = retryPreviewId(state);
    const recovering = canRetryPartial(state.results, id)
      || ['applying', 'interrupted'].includes(state.run?.state);
    if (state.busy || !state.run || !id || (!recovering && !canApply(state))) return;
    state.busy = true;
    state.error = '';
    rememberRun(state, storage);
    const token = beginRequest(state);
    notify();
    try {
      const report = await api('migration_apply', { runId: state.run.id, previewId: id });
      if (!isCurrent(state, token)) return;
      state.results = report;
      state.step = 'results';
      state.run.state = report.state;
      await completed(report);
    } catch (error) {
      if (isCurrent(state, token)) state.error = errorMessage(error);
    } finally {
      if (isCurrent(state, token)) { state.busy = false; notify(); }
    }
  }
  return {
    state, open, close, preview, apply, sessionOnly,
    get choices() { return choices; },
    toggle(id) { if (!state.busy) { toggleAsset(state, id); notify(); } },
    decide(item, choice, content) { if (!state.busy) { setDecision(state, item, choice, content); notify(); } },
    configure(key, value) { choices[key] = value; state.previewDirty = true; },
    back() { if (!state.busy) { state.step = 'items'; state.error = ''; notify(); } },
  };
}

const escapeHtml = (value) => String(value ?? '').replace(/[&<>"']/g, (character) => ({
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
}[character]));
const button = (text, action, primary = false, disabled = false) =>
  `<button type="button" class="btn${primary ? ' primary' : ''}" data-clone-action="${action}" ${disabled ? 'disabled' : ''}>${text}</button>`;
let flow;
let nodes;
let origin;
let onCompleted;
let mergeOpen;
let globalOpen = false;
let filesOpen = false;

export function openSessionClone(source, target, action, completed, trigger) {
  if (!flow) bind();
  if (flow.state.busy && (flow.state.source?.sessionId !== source.sessionId || flow.state.targetTool !== target)) {
    throw new Error('当前复刻尚未结束，请稍后再试');
  }
  origin = trigger || document.activeElement;
  onCompleted = completed;
  mergeOpen = null;
  globalOpen = false;
  filesOpen = false;
  return flow.open(source, target, action);
}

function itemName(id) {
  if (id === flow.state.source?.sessionId) return '会话';
  return flow.state.discovery?.report.assets.find((asset) => asset.id === id)?.name || '附带内容';
}
function assetRows(assets) {
  return assets.map((asset) => `<label class="clone-asset">
    <input type="checkbox" data-asset="${escapeHtml(asset.id)}" ${asset.selected ? 'checked' : ''} ${flow.state.busy ? 'disabled' : ''} />
    <span class="clone-asset-copy"><strong>${escapeHtml(asset.name)}</strong><small>${escapeHtml(KIND_LABELS[asset.kind] || asset.kind)}${asset.scope !== 'project' ? ` · ${escapeHtml(SCOPES[asset.scope] || asset.scope)}` : ''}</small></span>
    ${asset.diagnostics?.length ? '<span class="clone-note">需核对</span>' : ''}
  </label>`).join('');
}
function optionsContent() {
  const state = flow.state;
  const assets = assetsForStep(state);
  const local = assets.filter((asset) => asset.scope === 'project');
  const global = assets.filter((asset) => asset.scope !== 'project');
  let content = '<p class="clone-lead">是否也带上这条会话的记忆和工具？</p><p class="clone-muted">按需勾选；不选则只复刻对话。</p>';
  if (state.loading) content += '<div class="clone-empty" role="status"><span class="clone-spinner"></span>正在查找可附带的内容…</div>';
  else if (state.scanError) content += `<div class="clone-empty">暂时无法读取附带内容，你仍可仅复刻会话。<details><summary>查看原因</summary><p>${escapeHtml(state.scanError)}</p></details></div>`;
  else if (!assets.length) content += '<div class="clone-empty">没有发现可附带的记忆或工具。</div>';
  else {
    content += local.length ? `<div class="clone-group-head"><h3>项目内容 <span>${local.length}</span></h3>${button(local.every((asset) => asset.selected) ? '取消全选' : '全选', 'select-project')}</div>${assetRows(local)}` : '<p class="clone-muted">未发现项目内容。</p>';
    if (global.length) content += `<details class="clone-global" ${globalOpen ? 'open' : ''}><summary>全局记忆与工具 <span>${global.length}</span><small>仅勾选的内容会被附带</small></summary>${assetRows(global)}</details>`;
    const chosenMcp = assets.some((asset) => asset.selected && asset.kind === 'mcp');
    if (chosenMcp && ['opencode', 'pi', 'dsh'].includes(state.targetTool)) {
      content += '<details class="clone-settings"><summary>连接兼容设置</summary>';
      const options = (key, entries) => entries.map(([value, text]) => `<option value="${value}" ${flow.choices[key] === value || (!value && !flow.choices[key]) ? 'selected' : ''}>${text}</option>`).join('');
      if (state.targetTool === 'opencode') content += `<label>OpenCode 配置格式<select data-setting="opencodeVersion">${options('opencodeVersion', [['', '自动识别'], ['v1', 'V1'], ['v2', 'V2']])}</select></label>`;
      if (state.targetTool === 'pi') content += `<label>Pi 的 MCP 支持<select data-setting="piMcp">${options('piMcp', [['', '尚未确认'], ['supported', '此版本支持原生 MCP'], ['unsupported', '此版本不支持']])}</select></label>`;
      if (state.targetTool === 'dsh') content += `<label>目标 Profile<input data-setting="dshProfile" value="${escapeHtml(flow.choices.dshProfile || '')}" placeholder="已配置的 profile 名称" /></label>`;
      content += '</details>';
    }
  }
  return content;
}
function diffMarkup(item) {
  return `<div class="clone-diff">${diffRows(item).map((row) => `<div class="clone-diff-line ${escapeHtml(row.kind)}"><span class="clone-line-no">${row.oldLine ?? ''}</span><span class="clone-line-no">${row.newLine ?? ''}</span><code>${escapeHtml(row.text)}</code></div>`).join('')}</div>`;
}
function reviewContent() {
  const { preview, decisions, mergeDrafts, previewDirty } = flow.state;
  const conflicts = preview?.conflicts || [];
  let content = `<p class="clone-lead">确认后，所选内容将随会话一并复刻。</p><p class="clone-muted">${conflicts.some((entry) => !entry.resolved) ? '目标已有不同内容，请逐项选择处理方式。' : '请核对目标位置与最终差异。'}</p>`;
  for (const conflict of conflicts) {
    content += `<section class="clone-conflict"><h3>${conflict.kind === 'sourceSource' ? '来源内容重名' : '目标已有内容'}${conflict.resolved ? ' · 已处理' : ''}</h3><p class="clone-path">${escapeHtml(conflict.targetPath)}</p>`;
    for (const item of conflict.items) {
      const selected = decisions[item.decisionId]?.choice;
      const showMerge = mergeOpen === item.decisionId;
      content += `<div class="clone-conflict-item"><strong>${escapeHtml(itemName(item.itemId))}</strong>${diffMarkup(item)}<div class="clone-choices">${[['keep', '保留现有'], ['replace', '使用迁入内容'], ['merge', '手动合并']].map(([choice, text]) => `<button type="button" class="btn${choice === selected ? ' chosen' : ''}" data-choice="${choice}" data-decision="${escapeHtml(item.decisionId)}" aria-pressed="${choice === selected}" ${flow.state.busy || (choice === 'merge' && item.diff.kind === 'binary') ? 'disabled' : ''}>${text}</button>`).join('')}</div>`;
      if (showMerge) content += `<div class="clone-merge"><label for="cloneMergeText">最终内容${item.contributionKind === 'configValue' ? '（JSON）' : ''}</label><textarea id="cloneMergeText" data-draft="${escapeHtml(item.decisionId)}" spellcheck="false">${escapeHtml(mergeDrafts[item.decisionId] ?? initialDraft(item))}</textarea>${button('保存并更新预览', `merge:${escapeHtml(item.decisionId)}`, false, flow.state.busy)}</div>`;
      content += '</div>';
    }
    content += '</section>';
  }
  const notices = (preview?.items || []).filter((item) => item.readiness !== 'available');
  if (notices.length) content += `<div class="clone-notices">${notices.map((item) => `<p><strong>${escapeHtml(itemName(item.itemId))}</strong> · ${escapeHtml(READINESS_LABELS[item.readiness])}<span>${escapeHtml(item.message)}</span></p>`).join('')}</div>`;
  content += `<details class="clone-files" ${filesOpen ? 'open' : ''}><summary>查看最终文件差异 <span>${preview?.files.length || 0}</span></summary>${(preview?.files || []).map((file) => `<section><p class="clone-path">${escapeHtml(file.targetPath)}</p>${diffMarkup(file)}</section>`).join('')}</details>`;
  if (previewDirty) content += '<p class="clone-muted" role="status">处理方式已改变，请更新预览后再复刻。</p>';
  return content;
}
function resultsContent() {
  const state = flow.state;
  if (!state.results) return '<p class="clone-lead">上次复刻未完成。</p><p class="clone-muted">可以使用原来的记录继续，不会额外创建会话。</p>';
  if (state.results.sessionOnly) return `<div class="clone-success-mark">✓</div><p class="clone-lead">会话已复刻到 ${TOOLS[state.targetTool]}。</p><p class="clone-muted">在目标工具中打开当前项目即可继续。</p>`;
  const sessionOk = state.results.sessions?.some((session) => session.readiness === 'available');
  let content = `<p class="clone-lead">${sessionOk ? '会话已复刻' : '会话尚未复刻成功'}${state.results.state === 'partial' ? '，部分附带内容仍需处理。' : '。'}</p>`;
  content += (state.results.items || []).map((item) => `<div class="clone-result"><span class="clone-status ${escapeHtml(item.readiness)}">${escapeHtml(READINESS_LABELS[item.readiness] || '待核对')}</span><div><strong>${escapeHtml(itemName(item.itemId))}</strong><p>${escapeHtml(item.message)}</p>${(item.nextSteps || []).map((step) => `<small>${escapeHtml(step)}</small>`).join('')}</div></div>`).join('');
  return content;
}
function render(state) {
  if (!nodes) return;
  const wasOpen = nodes.modal.classList.contains('open');
  nodes.modal.classList.toggle('open', state.open);
  nodes.modal.setAttribute('aria-hidden', String(!state.open));
  if (!state.open) {
    if (wasOpen && origin?.isConnected) origin.focus();
    return;
  }
  nodes.title.textContent = `复刻到 ${TOOLS[state.targetTool] || state.targetTool}`;
  nodes.subtitle.textContent = state.source?.title || '当前会话';
  nodes.project.textContent = state.source?.cwd || '沿用原会话的工作目录';
  nodes.icon.src = `icons/${state.targetTool}.svg`;
  nodes.error.hidden = !state.error;
  nodes.error.textContent = state.error;
  nodes.close.textContent = state.busy ? '隐藏' : '关闭';
  nodes.body.setAttribute('aria-busy', String(state.loading || state.busy));
  const scroll = nodes.body.scrollTop;
  const activeId = document.activeElement?.dataset.asset;
  nodes.body.innerHTML = state.step === 'items' ? optionsContent() : state.step === 'review' ? reviewContent() : resultsContent();
  nodes.body.scrollTop = scroll;
  if (state.step === 'items') {
    const count = state.assetSelection.size;
    nodes.count.textContent = count ? `已选 ${count} 项附带内容` : '仅复刻会话';
    nodes.actions.innerHTML = (count ? button('仅复刻会话', 'session-only', false, state.busy) : '')
      + button(state.busy ? '正在处理…' : count ? '查看预览' : '复刻会话', count ? 'preview' : 'session-only', true, state.busy);
  } else if (state.step === 'review') {
    nodes.count.textContent = '复刻前确认';
    nodes.actions.innerHTML = button('返回选择', 'back', false, state.busy)
      + (state.previewDirty ? button('更新预览', 'preview', false, state.busy) : '')
      + button(state.busy ? '正在复刻…' : '确认复刻', 'apply', true, state.busy || !canApply(state));
  } else {
    nodes.count.textContent = '复刻结果';
    const retry = canRetryPartial(state.results, retryPreviewId(state)) || (!state.results && retryPreviewId(state));
    nodes.actions.innerHTML = (retry ? button('重试未完成项', 'apply', false, state.busy) : '')
      + button('再次复刻', 'again', false, state.busy) + button('完成', 'close', true);
  }
  if (activeId) [...nodes.body.querySelectorAll('[data-asset]')].find((input) => input.dataset.asset === activeId)?.focus();
  if (!wasOpen) nodes.close.focus();
}
function findConflict(id) { return flow.state.preview?.conflicts.flatMap((conflict) => conflict.items).find((item) => item.decisionId === id); }
function bind() {
  const get = (id) => document.getElementById(id);
  nodes = { modal: get('cloneModal'), panel: get('clonePanel'), title: get('cloneTitle'), subtitle: get('cloneSubtitle'),
    icon: get('cloneIcon'), project: get('cloneProject'), body: get('cloneBody'), actions: get('cloneActions'),
    count: get('cloneCount'), error: get('cloneError'), close: get('cloneClose') };
  flow = createCloneFlow((command, args) => window.__TAURI__.core.invoke(command, args), render,
    async (result) => { try { await onCompleted?.(result); } catch { /* refresh failures must not re-run a clone */ } });
  nodes.close.addEventListener('click', () => flow.close());
  nodes.modal.querySelector('[data-clone-close]').addEventListener('click', () => flow.close());
  nodes.modal.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); flow.close(); }
    if (event.key !== 'Tab') return;
    const focusable = [...nodes.panel.querySelectorAll('button, input, select, textarea, summary')]
      .filter((node) => !node.disabled && node.getClientRects().length);
    const first = focusable[0]; const last = focusable.at(-1);
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
  });
  nodes.body.addEventListener('toggle', (event) => {
    if (event.target.classList.contains('clone-global')) globalOpen = event.target.open;
    if (event.target.classList.contains('clone-files')) filesOpen = event.target.open;
  }, true);
  nodes.body.addEventListener('change', (event) => {
    if (event.target.dataset.asset) flow.toggle(event.target.dataset.asset);
    if (event.target.dataset.setting) flow.configure(event.target.dataset.setting, event.target.value || null);
  });
  nodes.body.addEventListener('input', (event) => {
    if (event.target.dataset.draft) flow.state.mergeDrafts[event.target.dataset.draft] = event.target.value;
  });
  nodes.modal.addEventListener('click', (event) => {
    const choice = event.target.closest('[data-choice]');
    if (choice && !flow.state.busy) {
      const item = findConflict(choice.dataset.decision);
      if (!item) return;
      if (choice.dataset.choice === 'merge') { mergeOpen = item.decisionId; render(flow.state); get('cloneMergeText')?.focus(); }
      else { mergeOpen = null; flow.decide(item, choice.dataset.choice); flow.preview(); }
      return;
    }
    const target = event.target.closest('[data-clone-action]');
    if (!target || target.disabled) return;
    const action = target.dataset.cloneAction;
    if (action === 'close') return flow.close();
    if (flow.state.busy) return;
    if (action === 'session-only') return void flow.sessionOnly();
    if (action === 'preview') return void flow.preview();
    if (action === 'apply') return void flow.apply();
    if (action === 'back') return flow.back();
    if (action === 'again') {
      forgetRun(flow.state.source.sessionId, flow.state.targetTool);
      return void flow.open(flow.state.source, flow.state.targetTool, flow.state.action);
    }
    if (action === 'select-project') {
      const local = assetsForStep(flow.state).filter((asset) => asset.scope === 'project');
      const select = !local.every((asset) => asset.selected);
      for (const asset of local) if (asset.selected !== select) toggleAsset(flow.state, asset.id);
      render(flow.state);
    }
    if (action.startsWith('merge:')) {
      const id = action.slice(6); const item = findConflict(id);
      try { flow.decide(item, 'merge', flow.state.mergeDrafts[id] ?? initialDraft(item)); mergeOpen = null; flow.preview(); }
      catch (error) { flow.state.error = errorMessage(error); render(flow.state); get('cloneMergeText')?.focus(); }
    }
  });
}
