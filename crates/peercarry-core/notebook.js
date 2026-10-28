(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root) root.PeerCarryNotebook = api;
})(typeof window !== 'undefined' ? window : globalThis, function () {
  'use strict';

  const id = () => (
    typeof crypto !== 'undefined' && crypto.randomUUID
      ? crypto.randomUUID()
      : 'task-' + Date.now().toString(36) + Math.random().toString(36).slice(2)
  );
  const normalize = value => (
    Array.isArray(value) ? value : (value && Array.isArray(value.items) ? value.items : [])
  );
  const limitCodepoints = (value, max) => Array.from(String(value)).slice(0, max).join('');
  const limitUtf8 = (value, maxBytes) => {
    let out = '';
    let bytes = 0;
    for (const codepoint of String(value)) {
      const size = typeof TextEncoder === 'function'
        ? new TextEncoder().encode(codepoint).length
        : unescape(encodeURIComponent(codepoint)).length;
      if (bytes + size > maxBytes) break;
      out += codepoint;
      bytes += size;
    }
    return out;
  };

  function mount(root, options) {
    options = options || {};
    if (!root || !root.ownerDocument) throw new Error('notebook root required');
    const doc = root.ownerDocument;
    const zh = !!options.isChinese;
    const locale = options.locale || (zh ? 'zh-CN' : 'en');
    const openSettings = typeof options.onOpenSettings === 'function' ? options.onOpenSettings : null;
    const fetcher = options.fetchImpl || (typeof fetch === 'function' ? fetch.bind(globalThis) : null);
    if (!fetcher) throw new Error('fetch implementation required');
    const text = (en, cn) => zh ? cn : en;
    const state = {
      section: 'notes',
      items: [],
      item: null,
      peers: [],
      entries: [],
      targetId: '',
      trash: false,
      search: '',
      dirty: false,
      version: 0,
      savedVersion: 0,
      selectionSequence: 0,
      listSequence: 0,
      searchTimer: null,
      hasLoaded: false,
      saving: false,
      conflict: false,
      conflictKind: '',
      syncConflictTarget: null,
      conflictVersions: null,
      timer: null,
      busy: false,
      composing: false,
      visible: false,
      syncTimer: null,
      sync: {
        loaded: false,
        available: null,
        unsupported: false,
        loading: false,
        saving: false,
        running: false,
        enabled: false,
        peerIds: [],
        discovered: [],
        status: null,
        message: '',
        discoveryFailed: false,
        detailsOpen: false,
      },
    };
    const el = (tag, cls, label) => {
      const node = doc.createElement(tag);
      if (cls) node.className = cls;
      if (label != null) node.textContent = label;
      return node;
    };
    const button = (label, action, cls) => {
      const node = el('button', cls || '', label);
      node.type = 'button';
      node.addEventListener('click', action);
      return node;
    };
    const storageKey = section => 'peercarry.notebook.last.' + section;
    const remember = (section, itemId) => {
      if (section === 'clipboard') return;
      try {
        const storage = doc.defaultView?.localStorage;
        if (itemId) storage?.setItem(storageKey(section), itemId);
        else storage?.removeItem(storageKey(section));
      } catch (_) {}
    };
    const recalled = section => {
      try { return doc.defaultView?.localStorage?.getItem(storageKey(section)) || ''; }
      catch (_) { return ''; }
    };
    const showEditor = value => root.classList.toggle('notebook-show-editor', value);
    const focusTitle = () => root.querySelector('.notebook-title-input')?.focus?.();
    const request = async (path, method, body) => {
      const response = await fetcher(path, {
        method: method || 'GET',
        headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      let data;
      try {
        data = await response.json();
      } catch (_) {
        let raw = '';
        try {
          raw = await response.clone().text();
        } catch (__) {}
        data = raw ? { raw: raw.slice(0, 200) } : {};
      }
      if (!response.ok) {
        const message = data.message || data.error || data.raw || (
          response.status === 409 ? 'revision_conflict' : 'Request failed (' + response.status + ')'
        );
        const error = new Error(message);
        error.status = response.status;
        error.data = data;
        throw error;
      }
      return data;
    };
    const status = (message, cls) => {
      const node = root.querySelector('.notebook-status');
      if (!node) return;
      node.textContent = message || '';
      node.className = 'notebook-status' + (cls ? ' ' + cls : '');
      const retry = root.querySelector('.notebook-retry');
      if (retry) retry.hidden = cls !== 'error' || !state.dirty || state.conflict;
    };
    const titleOf = item => item.title || text('Untitled', '未命名');
    function shell() {
      root.replaceChildren();
      const shell = el('div', 'notebook-shell');
      const panel = el('section','notebook-panel notebook-list');
      panel.append(el('div', 'notebook-list-header'));
      panel.append(el('div', 'notebook-list-items'));
      const syncCard = el('section', 'notebook-sync-card');
      syncCard.addEventListener('focusout', () => {
        const view = doc.defaultView;
        if (view?.setTimeout) view.setTimeout(() => {
          if (!syncCard.contains(doc.activeElement)) renderSyncPanel();
        }, 0);
      });
      panel.append(syncCard);
      const editor = el('section', 'notebook-editor');
      shell.append(panel, editor);
      root.append(shell);
    }
    function syncStateLabel(value) {
      const labels = {
        disabled: text('Off', '未开启'),
        offline: text('Offline · will retry', '离线 · 将自动重试'),
        checking: text('Checking…', '检查中…'),
        checked: text('Checked', '已检查'),
        pending: text('Waiting for the other computer to enable sync', '等待另一台电脑开启同步'),
        unsupported: text('Not supported on this computer', '此电脑版本暂不支持同步'),
        denied: text('Device authorization is needed', '需要双向设备授权'),
        error: text('Check failed · will retry', '检查失败 · 将稍后重试'),
        quota_exceeded: text('Sync storage limit reached · local copy kept', '同步空间已满 · 本机副本保留'),
      };
      return labels[value] || text('Status unavailable', '状态暂不可用');
    }
    function syncErrorText(error) {
      const code = String(error?.data?.error || error?.message || '').toLowerCase();
      if (error?.status === 404) return text(
        'Automatic sync is unavailable in this service version.',
        '当前服务版本暂不支持自动同步。',
      );
      if (error?.status === 401 || error?.status === 403 || /needs?_authentication|authentication_required|auth_required/.test(code)) {
        return text(
          'Configure device authentication in Settings before enabling sync.',
          '请先在“设置”中配置设备认证，再开启自动同步。',
        );
      }
      if (/denied|not_authorized|unselected_peer/.test(code)) return text(
        'Both computers must enable sync and select each other.',
        '两台电脑都需要开启同步并相互选择。',
      );
      if (/quota/.test(code)) return syncStateLabel('quota_exceeded');
      return text('Could not check automatic sync. Local notes are still available.', '无法检查自动同步状态，本机笔记仍可使用。');
    }
    function statusPeers() {
      return Array.isArray(state.sync.status?.peers) ? state.sync.status.peers : [];
    }
    function displayPeerName(peer) {
      for (const value of [peer?.name, peer?.host]) {
        if (typeof value === 'string' && value.trim()) return value;
      }
      return text('Another computer', '另一台电脑');
    }
    function peerName(nodeId) {
      const candidates = [
        ...state.sync.discovered,
        ...statusPeers(),
      ];
      const peer = candidates.find(candidate => candidate?.node_id === nodeId);
      return displayPeerName(peer);
    }
    function formatSyncTime(value) {
      if (!value) return '';
      const date = new Date(value);
      if (!Number.isFinite(date.getTime())) return '';
      return text('Last local read/merge: ', '上次本地读取/合并：') + date.toLocaleString(locale);
    }
    function syncSummary() {
      const sync = state.sync;
      if (sync.unsupported) return syncErrorText({ status: 404 });
      if (sync.message) return sync.message;
      if (sync.status?.needs_authentication) return syncErrorText({ status: 401 });
      if (!sync.loaded) return text('Checking availability…', '正在检查可用状态…');
      if (!sync.enabled) return text('Automatic sync is off', '自动同步未开启');
      if (!sync.peerIds.length) return text('Choose computers to sync with', '请选择要同步的电脑');
      const peerStates = statusPeers()
        .filter(peer => sync.peerIds.includes(peer?.node_id))
        .map(peer => peer.state);
      const priority = ['quota_exceeded', 'denied', 'offline', 'unsupported', 'error', 'checking', 'pending'];
      const stateName = priority.find(name => peerStates.includes(name));
      if (stateName) return syncStateLabel(stateName);
      if (sync.status?.ready === false) return syncStateLabel('pending');
      return text(
        `Automatic sync is on · ${sync.peerIds.length} selected`,
        `自动同步已开启 · 已选 ${sync.peerIds.length} 台电脑`,
      );
    }
    function renderSyncPanel() {
      const card = root.querySelector('.notebook-sync-card');
      if (!card) return;
      card.hidden = state.section === 'clipboard';
      if (card.hidden) return;
      const active = doc.activeElement;
      if (active && card.contains(active)) return;
      const previousDetails = card.querySelector('.notebook-sync-options');
      const detailsOpen = previousDetails ? !!previousDetails.open : state.sync.detailsOpen;
      card.replaceChildren();
      const sync = state.sync;
      const summary = el('div', 'notebook-sync-summary');
      const title = el('strong', '', text('Automatic sync', '自动同步'));
      const summaryText = el('small', 'notebook-sync-state', syncSummary());
      summary.append(title, summaryText);
      const check = button(text('Check now', '立即检查'), manualSync, 'notebook-sync-check');
      check.disabled = sync.available !== true || !sync.enabled || sync.running || sync.loading;
      check.setAttribute('aria-label', text('Check sync now', '立即检查同步状态'));
      summary.append(check);
      card.append(summary);

      if (sync.status?.conflicted_items > 0) {
        card.append(el('p', 'notebook-sync-conflict-count', text(
          `${sync.status.conflicted_items} notes have versions to review.`,
          `${sync.status.conflicted_items} 篇笔记有待处理的版本冲突。`,
        )));
      }

      const optionsPanel = el('details', 'notebook-sync-options');
      optionsPanel.open = detailsOpen;
      state.sync.detailsOpen = detailsOpen;
      optionsPanel.addEventListener('toggle', () => {
        state.sync.detailsOpen = !!optionsPanel.open;
        root.querySelector('.notebook-list')?.classList.toggle('notebook-sync-open', state.sync.detailsOpen);
      });
      const optionsSummary = el('summary', '', text('Devices and setup', '设备与设置'));
      optionsPanel.append(optionsSummary);
      const setup = el('p', 'notebook-sync-help', text(
        'Select the same computers on each side. Both computers must opt in. Turning sync off keeps local notes and stops future sync requests.',
        '在每台电脑上相互选择并开启同步。关闭后本机笔记仍会保留，也会停止后续同步请求。',
      ));
      optionsPanel.append(setup);
      if (sync.status?.needs_authentication || (sync.message && /authentication|認證|认证/i.test(sync.message))) {
        const auth = el('p', 'notebook-sync-auth', text(
          'Device authentication is required. Configure it in Settings; this panel never asks for or displays the token.',
          '需要先配置设备认证。请前往“设置”完成配置；此处不会索取或显示认证令牌。',
        ));
        optionsPanel.append(auth);
        if (openSettings) optionsPanel.append(button(text('Open Settings', '打开设置'), openSettings, 'notebook-sync-settings-link'));
      }

      const enableLabel = el('label', 'notebook-sync-enable');
      const enabled = el('input', 'notebook-sync-enable-control');
      enabled.type = 'checkbox';
      enabled.checked = sync.enabled;
      enabled.disabled = sync.available !== true || sync.loading || sync.saving || (sync.discoveryFailed && !sync.enabled);
      enabled.setAttribute('aria-label', text('Enable automatic sync on this computer', '在此电脑开启自动同步'));
      enabled.addEventListener('change', () => saveSyncSettings(enabled.checked, sync.peerIds));
      enableLabel.append(enabled, el('span', '', text('Enable on this computer', '在此电脑开启')));
      optionsPanel.append(enableLabel);

      const peers = el('div', 'notebook-sync-peers');
      const known = new Set();
      const discoverable = sync.discovered.filter(peer => {
        if (!peer || typeof peer.node_id !== 'string' || known.has(peer.node_id)) return false;
        known.add(peer.node_id);
        return true;
      }).slice(0, 16);
      if (!discoverable.length) {
        peers.append(el('p', 'notebook-sync-empty', sync.discoveryFailed
          ? text('Could not list discovered computers. Try again later.', '暂时无法读取已发现的电脑，请稍后重试。')
          : text('No discovered computers yet.', '尚未发现其他电脑。')));
      } else {
        for (const peer of discoverable) {
          const row = el('label', 'notebook-sync-peer');
          const choice = el('input', 'notebook-sync-peer-choice');
          choice.type = 'checkbox';
          choice.value = peer.node_id;
          choice.checked = sync.peerIds.includes(peer.node_id);
          choice.disabled = sync.available !== true || sync.discoveryFailed || sync.loading || sync.saving;
          choice.addEventListener('change', () => {
            const selected = new Set(sync.peerIds);
            if (choice.checked) selected.add(peer.node_id);
            else selected.delete(peer.node_id);
            const ids = Array.from(selected).slice(0, 16);
            return saveSyncSettings(sync.enabled && ids.length > 0, ids);
          });
          const info = el('span', 'notebook-sync-peer-info');
          info.append(el('span', 'notebook-sync-peer-name', displayPeerName(peer)));
          const peerStatus = statusPeers().find(candidate => candidate?.node_id === peer.node_id);
          if (peerStatus) {
            const detail = [syncStateLabel(peerStatus.state), formatSyncTime(peerStatus.last_success_at)].filter(Boolean).join(' · ');
            info.append(el('small', '', detail));
          }
          row.append(choice, info);
          peers.append(row);
        }
      }
      if (sync.discovered.length > 16) {
        peers.append(el('small', 'notebook-sync-limit', text('Choose up to 16 computers.', '最多可选择 16 台电脑。')));
      }
      optionsPanel.append(peers);
      card.append(optionsPanel);
      root.querySelector('.notebook-list')?.classList.toggle('notebook-sync-open', detailsOpen);
    }
    async function refreshSync(options) {
      options = options || {};
      const sync = state.sync;
      if (state.section === 'clipboard' || sync.loading || (sync.unsupported && !options.manual)) return false;
      sync.loading = true;
      sync.message = '';
      renderSyncPanel();
      try {
        const settings = await request('/v1/notebook/sync/settings');
        const statusInfo = await request('/v1/notebook/sync/status');
        let discovered = [];
        let discoveryFailed = false;
        try {
          discovered = normalize(await request('/v1/peers'))
            .filter(peer => peer && typeof peer.node_id === 'string');
        } catch (_) {
          discoveryFailed = true;
        }
        sync.available = true;
        sync.unsupported = false;
        sync.loaded = true;
        sync.enabled = !!settings.enabled;
        sync.peerIds = Array.isArray(settings.peer_ids)
          ? Array.from(new Set(settings.peer_ids.filter(value => typeof value === 'string'))).slice(0, 16)
          : [];
        sync.status = statusInfo && typeof statusInfo === 'object' ? statusInfo : {};
        sync.discovered = discovered;
        sync.discoveryFailed = discoveryFailed;
        if (options.refreshList && canRefreshRemoteList()) await loadList({ preserveEditor: true });
        return true;
      } catch (error) {
        sync.loaded = true;
        if (error?.status === 404) {
          sync.available = false;
          sync.unsupported = true;
          sync.status = null;
          sync.message = syncErrorText(error);
        } else {
          sync.message = syncErrorText(error);
          if (error?.status === 401 || error?.status === 403) {
            sync.status = { ...(sync.status || {}), needs_authentication: true };
          }
        }
        return false;
      } finally {
        sync.loading = false;
        renderSyncPanel();
        updateSyncRefreshTimer();
      }
    }
    async function saveSyncSettings(enabled, peerIds) {
      const sync = state.sync;
      if (sync.available !== true || sync.saving) return false;
      const selected = Array.from(new Set(peerIds.filter(value => typeof value === 'string'))).slice(0, 16);
      if (enabled && !selected.length) {
        sync.message = text('Choose at least one computer first.', '请先至少选择一台电脑。');
        renderSyncPanel();
        return false;
      }
      if (enabled && sync.discoveryFailed) {
        sync.message = text('Cannot verify discovered computers right now. Try again later.', '暂时无法确认已发现的电脑，请稍后重试。');
        renderSyncPanel();
        return false;
      }
      const previous = { enabled: sync.enabled, peerIds: sync.peerIds.slice() };
      sync.enabled = !!enabled;
      sync.peerIds = selected;
      sync.message = '';
      sync.saving = true;
      renderSyncPanel();
      try {
        const updated = await request('/v1/notebook/sync/settings', 'POST', {
          enabled: sync.enabled,
          peer_ids: selected,
        });
        sync.enabled = !!updated.enabled;
        sync.peerIds = Array.isArray(updated.peer_ids)
          ? Array.from(new Set(updated.peer_ids.filter(value => typeof value === 'string'))).slice(0, 16)
          : selected;
        await refreshSync();
        return true;
      } catch (error) {
        sync.enabled = previous.enabled;
        sync.peerIds = previous.peerIds;
        sync.message = syncErrorText(error);
        if (error?.status === 404) {
          sync.available = false;
          sync.unsupported = true;
        }
        if (error?.status === 401 || error?.status === 403 || /needs?_authentication|authentication_required|auth_required/i.test(String(error?.data?.error || ''))) {
          sync.status = { ...(sync.status || {}), needs_authentication: true };
        }
        return false;
      } finally {
        sync.saving = false;
        renderSyncPanel();
      }
    }
    async function manualSync() {
      const sync = state.sync;
      if (sync.available !== true || !sync.enabled || sync.running) return false;
      sync.running = true;
      sync.message = '';
      renderSyncPanel();
      try {
        await request('/v1/notebook/sync/run', 'POST', {});
        await refreshSync({ refreshList: true });
        return true;
      } catch (error) {
        sync.message = syncErrorText(error);
        if (error?.status === 404) {
          sync.available = false;
          sync.unsupported = true;
        }
        return false;
      } finally {
        sync.running = false;
        renderSyncPanel();
        updateSyncRefreshTimer();
      }
    }
    function canRefreshRemoteList() {
      if (state.dirty || state.saving || state.composing || state.conflict || state.section === 'clipboard') return false;
      const editor = root.querySelector('.notebook-editor');
      const active = doc.activeElement;
      return !(editor && active && active !== editor && editor.contains(active));
    }
    function updateSyncRefreshTimer() {
      const view = doc.defaultView;
      if (!view) return;
      const shouldRun = state.visible && state.section !== 'clipboard' && !state.sync.unsupported;
      if (shouldRun && state.syncTimer == null && typeof view.setInterval === 'function') {
        state.syncTimer = view.setInterval(() => refreshSync({ refreshList: true }), 15000);
      } else if (!shouldRun && state.syncTimer != null && typeof view.clearInterval === 'function') {
        view.clearInterval(state.syncTimer);
        state.syncTimer = null;
      }
    }
    function headerAction(label, fn) {
      return button(label, fn);
    }
    async function loadList(options) {
      options = options || {};
      if (state.section === 'clipboard') return loadClipboard();
      const sequence = ++state.listSequence;
      const section = state.section, search = state.search, trash = state.trash;
      const kind = state.section === 'todo' ? 'todo' : 'markdown';
      const qs = new URLSearchParams({ kind, trash: String(state.trash) });
      if (state.search) qs.set('q', state.search);
      try {
        const items = normalize(await request('/v1/notebook?' + qs));
        if (sequence !== state.listSequence || section !== state.section || search !== state.search || trash !== state.trash) return;
        state.items = items;
        state.hasLoaded = true;
        renderList(options);
        return true;
      } catch (error) {
        if (sequence !== state.listSequence || section !== state.section || search !== state.search || trash !== state.trash) return;
        state.items = [];
        renderList(options);
        status(text('Could not load notes: ', '读取笔记失败：') + error.message, 'error');
        return false;
      }
    }
    async function loadClipboard() {
      const editor = root.querySelector('.notebook-editor');
      editor.className = 'notebook-editor notebook-clipboard';
      try {
        const [peers, entries] = await Promise.all([request('/v1/peers'), request('/v1/timeline?limit=200&max=1000')]);
        state.peers = normalize(peers).filter(p => p && typeof p.node_id === 'string');
        state.entries = normalize(entries).filter(entry => (
          entry && entry.origin && typeof entry.origin.node_id === 'string' &&
          (!state.targetId || entry.origin.node_id === state.targetId)
        ));
        renderClipboard();
        const header = root.querySelector('.notebook-editor-header');
        const selectedPeer = state.peers.find(peer => peer.node_id === state.targetId);
        const fetchButton = button(
          text('Fetch selected device clipboard', '获取所选电脑剪贴板'),
          async () => {
          if (!selectedPeer) {
            status(text('Choose a device first.', '请先选择设备。'), 'error');
            return;
          }
          fetchButton.disabled = true;
          try {
            await request('/v1/actions/fetch-clipboard', 'POST', { node_id: selectedPeer.node_id });
            await loadClipboard();
            status(text(
              'History refreshed. Click Copy to apply an entry.',
              '历史已刷新。需要使用时请单独点击“复制”。',
            ));
          } catch (error) {
            const message = error.status === 403
              ? text('Clipboard access is not allowed by that device.', '对方设备未允许获取剪贴板。')
              : text('Could not fetch clipboard: ', '获取剪贴板失败：') + error.message;
            status(message, 'error');
          } finally {
            fetchButton.disabled = false;
          }
        });
        fetchButton.disabled = !selectedPeer;
        header.append(fetchButton);
        status('');
      } catch (error) {
        state.peers = [];
        state.entries = [];
        renderClipboard();
        status(text('Could not load device history: ', '读取设备历史失败：') + error.message, 'error');
      }
    }
    function renderList(options) {
      options = options || {};
      const panel = root.querySelector('.notebook-panel');
      const head = panel.querySelector('.notebook-list-header');
      const list = panel.querySelector('.notebook-list-items');
      list.replaceChildren();
      const trashLabel = state.trash ? text('Notes', '笔记') : text('Recently Deleted', '最近删除');
      let search = head.querySelector('.notebook-search');
      if (!search) {
        search = el('input', 'notebook-search');
        search.type = 'search';
        search.placeholder = text('Search', '搜索');
        search.value = state.search;
        search.addEventListener('input', () => {
          state.search = search.value;
          ++state.listSequence;
          ++state.selectionSequence;
          clearTimeout(state.searchTimer);
          state.searchTimer = setTimeout(() => loadList({ preserveEditor: true }), 250);
        });
        const trashButton = headerAction(trashLabel, async () => {
          if (!(await flush())) return;
          ++state.selectionSequence;
          state.trash = !state.trash;
          await loadList({ preserveEditor: true });
        });
        trashButton.className = 'notebook-trash-toggle';
        head.append(search, headerAction(text('New', '新建'), createItem), trashButton);
      }
      head.querySelector('.notebook-trash-toggle').textContent = trashLabel;
      for (const item of state.items) {
        const row = el('article', 'notebook-item');
        row.dataset.id = item.id;
        const open = button(titleOf(item), async () => {
          if (!(await flush())) return;
          await selectItem(item.id);
        });
        row.append(
          open,
          el('p', '', item.preview || ''),
          el('small', '', item.trashed ? text('In Recently Deleted', '已回收') : ''),
        );
        if (Number(item.conflict_count) > 0) {
          row.append(el('small', 'notebook-conflict-badge', text(
            `${item.conflict_count} versions`,
            `${item.conflict_count} 个版本`,
          )));
        }
        if (item.trashed) {
          row.append(button(text('Restore', '恢复'), () => restoreItem(item)));
        } else {
          const pinLabel = item.pinned ? text('Unpin', '取消置顶') : text('Pin', '置顶');
          row.append(button(pinLabel, async () => {
            if (!(await selectItem(item.id))) return;
            state.item.pinned = !state.item.pinned;
            changed();
            await flush();
            await loadList();
          }));
          row.append(button(text('Delete', '回收'), async () => {
            if (!(await flush())) return;
            try {
              const fresh = await request('/v1/notebook/' + encodeURIComponent(item.id));
              await request('/v1/notebook/' + encodeURIComponent(item.id), 'DELETE', {
                expected_revision: fresh.revision,
              });
              if (state.item?.id === item.id) { state.item = null; remember(state.section, ''); showEditor(false); }
              await loadList();
              renderEditor();
              status(text('Moved to Recently Deleted', '已移入回收站'));
            } catch (error) {
              await showError(error, item.id);
            }
          }));
        }
        list.append(row);
      }
      if (state.item) {
        const summary = state.items.find(item => item.id === state.item.id);
        if (summary) {
          if (Number.isFinite(summary.conflict_count)) state.item.conflict_count = summary.conflict_count;
          if (summary.version_id) state.item.version_id = summary.version_id;
        }
      }
      if (!options.preserveEditor && !state.item) renderEditor();
      renderSyncPanel();
    }
    async function selectItem(itemId) {
      const sequence = ++state.selectionSequence;
      if (!(await flush()) || sequence !== state.selectionSequence) return false;
      const previousId = state.item?.id;
      const version = state.version;
      const section = state.section;
      try {
        const selected = await request('/v1/notebook/' + encodeURIComponent(itemId));
        if (sequence !== state.selectionSequence || section !== state.section ||
            previousId !== state.item?.id || version !== state.version || state.dirty || state.composing) {
          return false;
        }
        state.item = selected;
        if (!selected.trashed) remember(section, selected.id);
        showEditor(true);
        state.conflict = false;
        state.conflictKind = '';
        state.syncConflictTarget = null;
        state.conflictVersions = null;
        state.dirty = false;
        state.version = state.savedVersion = 0;
        renderEditor();
        return true;
      } catch (error) {
        if (sequence === state.selectionSequence) await showError(error, itemId);
        return false;
      }
    }
    async function createItem() {
      const sequence = ++state.selectionSequence;
      if (!(await flush()) || sequence !== state.selectionSequence) return;
      const previousId = state.item?.id;
      const version = state.version;
      const section = state.section;
      try {
        const kind = state.section === 'todo' ? 'todo' : 'markdown';
        const item = await request('/v1/notebook', 'POST', {
          kind,
          title: '',
          markdown: '',
          tasks: [],
          pinned: false,
        });
        if (sequence !== state.selectionSequence || section !== state.section ||
            previousId !== state.item?.id || version !== state.version || state.dirty || state.composing) {
          await loadList({ preserveEditor: true });
          status(text('New note created. Your current draft is kept.', '已新建笔记，当前编辑草稿已保留。'));
          return;
        }
        state.item = item;
        remember(section, item.id);
        showEditor(true);
        state.dirty = false;
        state.conflict = false;
        state.conflictKind = '';
        state.syncConflictTarget = null;
        state.conflictVersions = null;
        state.version = state.savedVersion = 0;
        await loadList();
        renderEditor();
        focusTitle();
      } catch (error) {
        showError(error);
      }
    }
    const snapshot = () => ({
      kind: state.item.kind,
      title: state.item.title || '',
      markdown: state.item.kind === 'markdown' ? state.item.markdown || '' : '',
      tasks: state.item.kind === 'todo'
        ? (state.item.tasks || []).map(task => ({
          id: task.id,
          text: task.text,
          done: !!task.done,
        }))
        : [],
      pinned: !!state.item.pinned,
    });
    async function saveLoop() {
      if (state.saving || !state.item || !state.dirty || state.conflict || state.composing) return;
      state.saving = true;
      status(text('Saving…', '保存中…'));
      try {
        while (state.item && state.dirty && !state.conflict && !state.composing) {
          const version = state.version, body = { ...snapshot(), expected_revision: state.item.revision };
          const saved = await request('/v1/notebook/' + encodeURIComponent(state.item.id), 'PUT', body);
          state.item.revision = saved.revision; state.item.updated_at = saved.updated_at;
          state.savedVersion = version;
          if (state.version === version) {
            state.dirty = false;
            status(text('Saved', '已保存'));
            loadList({ preserveEditor: true });
          }
        }
      } catch (error) {
        if (error.status === 409) {
          state.conflict = true;
          if (error.data?.error === 'sync_conflict') {
            state.conflictKind = 'sync';
            state.syncConflictTarget = { id: state.item?.id, revision: state.item?.revision };
            status(text(
              'Another computer has a concurrent version. Your draft is kept.',
              '另一台电脑上有并发版本，当前草稿已保留。',
            ), 'conflict');
            renderEditor();
          } else {
            state.conflictKind = 'revision';
            status(text(
              'Revision conflict: your draft is kept. Reload or save a copy.',
              '本机版本冲突：草稿已保留。可重新载入或另存副本。',
            ), 'conflict');
            renderConflict();
          }
        } else {
          status(text('Save failed. Your draft is kept. ', '保存失败，草稿已保留。') + error.message, 'error');
        }
      } finally {
        state.saving = false;
      }
      return !state.dirty && !state.conflict;
    }
    async function flush() {
      clearTimeout(state.timer);
      if (state.composing) return false;
      if (!state.dirty) return !state.conflict;
      while (state.saving) await new Promise(resolve => setTimeout(resolve, 0));
      if (state.dirty) await saveLoop();
      return !state.dirty && !state.conflict;
    }
    function changed() {
      state.version++;
      state.dirty = true;
      status(text('Waiting to save automatically', '等待自动保存'));
      clearTimeout(state.timer);
      if (!state.composing) state.timer = setTimeout(() => saveLoop(), 350);
      if (state.item?.kind === 'markdown') renderPreview();
    }
    function renderConflict() {
      const editor = root.querySelector('.notebook-editor');
      let box = editor.querySelector('.notebook-conflict');
      if (box) return;

      box = el('div', 'notebook-conflict');
      box.append(
        button(text('Reload server version', '重新载入服务器版本'), async () => {
          try {
            state.item = await request('/v1/notebook/' + encodeURIComponent(state.item.id));
            state.conflict = false;
            state.conflictKind = '';
            state.dirty = false;
            renderEditor();
          } catch (error) {
            await showError(error, state.item?.id);
          }
        }),
        button(text('Save as copy', '另存副本'), async () => {
          const previous = state.item;
          try {
            const copy = await request('/v1/notebook', 'POST', snapshot());
            copy.title = (previous.title || '') + text(' (copy)', '（副本）');
            state.item = copy;
            state.conflict = false;
            state.conflictKind = '';
            state.syncConflictTarget = null;
            state.dirty = true;
            state.version++;
            renderEditor();
            await flush();
            await loadList();
          } catch (error) {
            showError(error);
          }
        }),
      );
      editor.append(box);
    }
    function renderSyncConflict() {
      const editor = root.querySelector('.notebook-editor');
      const box = el('div', 'notebook-conflict notebook-sync-conflict');
      box.append(el('p', '', text(
        state.dirty
          ? 'This note has concurrent versions. Your draft is kept until you choose what to do.'
          : 'This note has concurrent versions that need an explicit choice.',
        state.dirty
          ? '这篇笔记存在并发版本。完成选择前，当前草稿会保留。'
          : '这篇笔记存在并发版本，需要明确选择要保留的版本。',
      )));
      box.append(button(text('View conflict versions', '查看冲突版本'), () => viewConflicts(state.syncConflictTarget?.id || state.item?.id)));
      if (state.dirty) box.append(button(text('Save draft as copy', '将草稿另存为新笔记'), saveDraftAsCopyForSyncConflict));
      editor.append(box);
    }
    function renderConflictVersions() {
      const details = state.conflictVersions;
      if (!details || details.id !== state.item?.id) return;
      const panel = el('section', 'notebook-conflict-versions');
      panel.append(el('h3', '', text('Choose a version to keep', '选择要保留的版本')));
      panel.append(el('p', '', text(
        'Only an explicit choice resolves these versions. Device names are shown when known.',
        '需要明确选择一个版本才能完成处理；可识别的电脑将显示其名称。',
      )));
      if (details.loading) {
        panel.append(el('p', '', text('Loading versions…', '正在读取版本…')));
      } else if (details.error) {
        panel.append(el('p', 'notebook-conflict-error', details.error));
      } else if (!details.versions.length) {
        panel.append(el('p', '', text('No concurrent versions remain.', '当前没有待处理的并发版本。')));
      } else {
        for (const version of details.versions) {
          const card = el('article', 'notebook-conflict-version');
          const name = version.title || text('Untitled', '未命名');
          card.append(el('strong', '', name));
          card.append(el('small', '', peerName(version.device_id)));
          if (version.updated_at) {
            const date = new Date(version.updated_at);
            if (Number.isFinite(date.getTime())) card.append(el('small', '', date.toLocaleString(locale)));
          }
          const preview = el('details', 'notebook-conflict-preview');
          preview.append(el('summary', '', text('Preview content', '预览内容')));
          if (version.kind === 'todo') {
            const tasks = el('ul');
            for (const task of Array.isArray(version.tasks) ? version.tasks : []) {
              tasks.append(el('li', '', `${task?.done ? '✓ ' : ''}${String(task?.text || '')}`));
            }
            preview.append(tasks);
          } else {
            preview.append(el('pre', '', String(version.markdown || '')));
          }
          const keep = button(text('Keep this version', '保留此版本'), () => resolveConflictVersion(version.version_id), 'primary');
          keep.disabled = state.dirty || state.saving || !version.version_id;
          card.append(preview, keep);
          panel.append(card);
        }
      }
      root.querySelector('.notebook-editor').append(panel);
    }
    async function viewConflicts(itemId) {
      const targetId = itemId || state.item?.id;
      if (!targetId) return;
      if (state.dirty) {
        state.conflict = true;
        state.conflictKind = 'sync';
        state.syncConflictTarget = { id: targetId, revision: state.item?.revision };
        status(text(
          'Save your draft as a copy before resolving versions.',
          '请先将草稿另存为新笔记，再处理冲突版本。',
        ), 'conflict');
        renderEditor();
        return;
      }
      state.conflictVersions = { id: targetId, loading: true, versions: [], error: '' };
      state.syncConflictTarget = { id: targetId, revision: state.item?.revision };
      renderEditor();
      try {
        const fresh = await request('/v1/notebook/' + encodeURIComponent(targetId));
        const response = await request('/v1/notebook/' + encodeURIComponent(targetId) + '/conflicts');
        if (state.item?.id === targetId) state.item = fresh;
        state.syncConflictTarget = { id: targetId, revision: fresh.revision };
        state.conflictVersions = {
          id: targetId,
          loading: false,
          versions: Array.isArray(response.versions) ? response.versions : [],
          error: '',
        };
        renderEditor();
      } catch (error) {
        state.conflictVersions = {
          id: targetId,
          loading: false,
          versions: [],
          error: error.status === 404
            ? text('Conflict versions are unavailable in this service version.', '当前服务版本暂不支持读取冲突版本。')
            : syncErrorText(error),
        };
        renderEditor();
      }
    }
    async function saveDraftAsCopyForSyncConflict() {
      if (!state.item || !state.dirty || state.saving) return false;
      const targetId = state.syncConflictTarget?.id || state.item.id;
      const draft = snapshot();
      draft.title = (draft.title || '') + text(' (copy)', '（副本）');
      let savedCopy;
      try {
        savedCopy = await request('/v1/notebook', 'POST', draft);
      } catch (error) {
        status(text(
          'Could not save the draft as a copy; the original draft is still kept. ',
          '另存副本失败，原草稿仍已保留。',
        ) + error.message, 'error');
        return false;
      }
      state.item = savedCopy;
      state.dirty = false;
      state.conflict = false;
      state.conflictKind = '';
      state.version = state.savedVersion = 0;
      state.syncConflictTarget = null;
      state.conflictVersions = null;
      await loadList({ preserveEditor: true });
      renderEditor();
      let original;
      try {
        original = await request('/v1/notebook/' + encodeURIComponent(targetId));
      } catch (error) {
        status(text(
          'Draft saved as a separate note, but the original could not be reloaded yet.',
          '草稿已另存为新笔记，但暂时无法重新读取原笔记。',
        ), 'error');
        return true;
      }
      try {
        state.item = original;
        state.dirty = false;
        state.version = state.savedVersion = 0;
        state.syncConflictTarget = { id: targetId, revision: original.revision };
        state.conflictVersions = null;
        await loadList({ preserveEditor: true });
        renderEditor();
        status(text(
          'Draft saved as a separate note. Choose a version to resolve the original.',
          '草稿已另存为新笔记。现在请选择原笔记要保留的版本。',
        ));
        await viewConflicts(targetId);
        return true;
      } catch (error) {
        status(text('Draft saved as a copy, but conflict versions could not be loaded. ', '草稿已另存，但暂时无法读取冲突版本。') + error.message, 'error');
        return false;
      }
    }
    async function resolveConflictVersion(versionId) {
      const target = state.syncConflictTarget;
      if (!target || !versionId) return false;
      if (state.dirty) {
        status(text('Save the draft as a copy before resolving.', '请先将草稿另存为新笔记，再处理冲突。'), 'conflict');
        return false;
      }
      try {
        const resolved = await request('/v1/notebook/' + encodeURIComponent(target.id) + '/resolve', 'POST', {
          expected_revision: state.item?.id === target.id ? state.item.revision : target.revision,
          version_id: versionId,
        });
        state.item = resolved;
        state.dirty = false;
        state.conflict = false;
        state.conflictKind = '';
        state.syncConflictTarget = null;
        state.conflictVersions = null;
        await loadList({ preserveEditor: true });
        renderEditor();
        status(text('Versions resolved. The chosen content is now the local note.', '冲突已处理，所选内容已成为本机笔记。'));
        return true;
      } catch (error) {
        if (error.status === 409) {
          status(text(
            'The conflict changed. Review the latest versions and choose again.',
            '冲突版本已变化，请刷新后重新选择。',
          ), 'conflict');
          await viewConflicts(target.id);
          return false;
        }
        status(text('Could not resolve versions. ', '处理冲突失败。') + error.message, 'error');
        return false;
      }
    }
    function renderEditor() {
      const editor = root.querySelector('.notebook-editor'); editor.replaceChildren();
      if (!state.item || state.section === 'clipboard') {
        editor.append(el('p', '', text('Select or create a note.', '选择或新建笔记。')));
        return;
      }

      const header = el('header', 'notebook-editor-header');
      header.append(button(text('Back to list', '返回列表'), async () => {
        if (await flush()) {
          showEditor(false);
          loadList({ preserveEditor: true });
        }
      }, 'notebook-back'));
      const title = el('input', 'notebook-title-input');
      title.value = state.item.title || '';
      title.placeholder = text('Title', '标题');
      title.addEventListener('input', () => {
        title.value = limitCodepoints(title.value, 200);
        state.item.title = title.value;
        changed();
      });
      header.append(title);
      if (state.item.kind === 'markdown') {
        const toggle = button(text('Preview', '预览'), () => {
          const input = root.querySelector('.notebook-markdown-input');
          const preview = root.querySelector('.notebook-preview');
          const showingPreview = !preview.hidden;
          input.hidden = !showingPreview;
          preview.hidden = showingPreview;
          toggle.textContent = showingPreview ? text('Preview', '预览') : text('Edit', '编辑');
          toggle.setAttribute('aria-pressed', String(!showingPreview));
          if (!showingPreview) renderPreview();
          else input.focus?.();
        });
        toggle.setAttribute('aria-pressed', 'false');
        header.append(toggle);
      }
      if (state.item.kind === 'todo') {
        header.append(
          button(text('Add task', '添加任务'), addTask),
          button(text('Save', '保存'), flush, 'notebook-save'),
        );
      }
      const more = el('details', 'notebook-more');
      more.append(el('summary', '', text('More', '更多')),
        button(text('Export .md', '导出 .md'), exportMarkdown),
        button(text('Send to device', '发送到设备'), share));
      header.append(more);
      editor.append(header);
      if (state.item.kind === 'markdown') {
        const bar = el('div', 'notebook-toolbar');
        const input = el('textarea', 'notebook-markdown-input');
        input.value = state.item.markdown || '';
        input.addEventListener('input', () => {
          state.item.markdown = input.value;
          changed();
        });
        const preview = el('div', 'notebook-preview');
        preview.hidden = true;
        bar.append(button(text('Save', '保存'), flush, 'notebook-save'));
        editor.append(bar, input, preview);
        renderPreview();
      } else {
        const tasks = el('div', 'notebook-tasks');
        (state.item.tasks || []).forEach((task, index) => {
          const row = el('label', 'notebook-task-row');
          const checkbox = el('input');
          checkbox.type = 'checkbox';
          checkbox.checked = !!task.done;
          checkbox.addEventListener('change', () => {
            task.done = checkbox.checked;
            changed();
          });
          const field = el('input', 'notebook-task-input');
          field.value = task.text;
          field.addEventListener('input', () => {
            field.value = limitUtf8(field.value, 4096);
            task.text = field.value;
            changed();
          });
          field.addEventListener('keydown', event => {
            if (event.key !== 'Enter' || event.repeat || event.isComposing || event.keyCode === 229 || state.composing || !task.text.trim()) return;
            event.preventDefault();
            addTask();
          });
          row.append(
            checkbox,
            field,
            button(text('Remove', '删除'), () => {
              state.item.tasks.splice(index, 1);
              changed();
              renderEditor();
            }),
          );
          tasks.append(row);
        });
        editor.append(tasks);
      }
      const saveStatus = el('div', 'notebook-status');
      saveStatus.setAttribute('role', 'status');
      saveStatus.setAttribute('aria-live', 'polite');
      const retry = button(text('Retry save', '重试保存'), flush, 'notebook-retry');
      retry.hidden = true;
      editor.append(saveStatus, retry);
      if (state.conflictKind === 'revision') renderConflict();
      else if (state.conflictKind === 'sync') renderSyncConflict();
      else if (Number(state.item.conflict_count) > 0) {
        const note = el('div', 'notebook-sync-conflict');
        note.append(
          el('p', '', text(
            `${state.item.conflict_count} concurrent versions need a choice before they can be reconciled.`,
            `${state.item.conflict_count} 个并发版本需要明确选择后才能合并。`,
          )),
          button(text('Review versions', '查看版本'), () => viewConflicts(state.item.id)),
        );
        editor.append(note);
      }
      renderConflictVersions();
    }
    function addTask() {
      if (state.composing || !state.item || state.item.kind !== 'todo') return;
      state.item.tasks.push({ id: id(), text: '', done: false });
      changed();
      renderEditor();
      const fields = root.querySelectorAll('.notebook-task-input');
      fields[fields.length - 1]?.focus?.();
    }
    function appendInline(parent, value) {
      const pattern = /(`[^`]+`|\*\*[^*]+\*\*|__[^_]+__|\*[^*\n]+\*|_[^_\n]+_|\[[^\]]+\]\([^)]+\))/g;
      let position = 0;
      let match;
      while ((match = pattern.exec(value))) {
        if (match.index > position) {
          parent.append(doc.createTextNode(value.slice(position, match.index)));
        }
        const raw = match[0];
        let node;
        if (raw[0] === '`') {
          node = el('code', '', raw.slice(1, -1));
        } else if (raw.startsWith('**') || raw.startsWith('__')) {
          node = el('strong', '', raw.slice(2, -2));
        } else if (raw[0] === '*' || raw[0] === '_') {
          node = el('em', '', raw.slice(1, -1));
        } else {
          const link = /^\[([^\]]+)\]\(([^)]+)\)$/.exec(raw);
          let href = '';
          try {
            const url = new URL(link[2]);
            if (url.protocol === 'http:' || url.protocol === 'https:') href = url.href;
          } catch (_) {}
          if (href) {
            node = el('a', '', link[1]);
            node.href = href;
            node.target = '_blank';
            node.rel = 'noopener noreferrer';
          } else {
            node = doc.createTextNode(raw);
          }
        }
        parent.append(node);
        position = pattern.lastIndex;
      }
      if (position < value.length) parent.append(doc.createTextNode(value.slice(position)));
    }
    function renderMarkdown(container, source) {
      container.replaceChildren();
      const lines = String(source || '').split(/\r?\n/);
      let index = 0;
      while (index < lines.length) {
        const line = lines[index];
        if (!line.trim()) {
          index++;
          continue;
        }
        const fence = line.match(/^\s*```/);
        if (fence) {
          const code = [];
          index++;
          while (index < lines.length && !/^\s*```/.test(lines[index])) code.push(lines[index++]);
          index++;
          container.append(el('pre', '', code.join('\n')));
          continue;
        }
        const heading = line.match(/^(#{1,6})\s+(.*)$/);
        if (heading) {
          const node = el('h' + heading[1].length);
          appendInline(node, heading[2]);
          container.append(node);
          index++;
          continue;
        }
        if (/^\s*>/.test(line)) {
          const quote = el('blockquote');
          appendInline(quote, line.replace(/^\s*>\s?/, ''));
          container.append(quote);
          index++;
          continue;
        }
        if (/^\s*([-*+] |\d+\. )/.test(line)) {
          const ordered = /^\s*\d+\./.test(line);
          const list = el(ordered ? 'ol' : 'ul');
          while (index < lines.length && /^\s*([-*+] |\d+\. )/.test(lines[index])) {
            const item = el('li');
            appendInline(item, lines[index++].replace(/^\s*([-*+] |\d+\. )/, ''));
            list.append(item);
          }
          container.append(list);
          continue;
        }
        const paragraph = el('p');
        appendInline(paragraph, line);
        container.append(paragraph);
        index++;
      }
    }
    function renderPreview() {
      const preview = root.querySelector('.notebook-preview');
      if (preview && state.item) renderMarkdown(preview, state.item.markdown || '');
    }
    function exportText(item) {
      const title = item.title || '';
      const content = item.kind === 'todo'
        ? (item.tasks || []).map(task => '- [' + (task.done ? 'x' : ' ') + '] ' + task.text).join('\n')
        : (item.markdown || '');
      return (title ? '# ' + title + '\n\n' : '') + content;
    }
    function exportMarkdown() {
      if (!state.item) return;
      const blob = new Blob([exportText(state.item)], { type: 'text/markdown;charset=utf-8' });
      const url = URL.createObjectURL(blob);
      const anchor = el('a');
      anchor.href = url;
      anchor.download = (state.item.title || 'note').replace(/[\\/:*?"<>|]/g, '_') + '.md';
      anchor.click();
      setTimeout(() => URL.revokeObjectURL(url), 0);
    }
    function renderClipboard() {
      const editor = root.querySelector('.notebook-editor');
      editor.replaceChildren();
      const header = el('header', 'notebook-editor-header');
      header.append(el('h2', '', text('Device clipboard history', '设备剪贴板历史')));

      const filter = el('select', 'notebook-peer-filter');
      const allDevices = el('option');
      allDevices.value = '';
      allDevices.textContent = text('All devices', '全部设备');
      filter.append(allDevices);
      for (const peer of state.peers) {
        const option = el('option');
        option.value = peer.node_id;
        option.textContent = peer.name || peer.host || peer.node_id;
        filter.append(option);
      }
      filter.value = state.targetId;
      filter.addEventListener('change', () => {
        state.targetId = filter.value;
        loadClipboard();
      });
      header.append(filter, button(text('Refresh', '刷新'), loadClipboard));
      editor.append(header);

      const list = el('div', 'notebook-clipboard-list');
      for (const entry of state.entries) {
        const row = el('article', 'notebook-clipboard-item');
        row.append(
          el('p', '', entry.preview || ''),
          el('small', '', entry.origin.host || entry.origin.node_id),
        );
        const actions = el('div', 'notebook-clipboard-actions');
        if (entry.kind !== 'files') {
          actions.append(button(text('Copy', '复制'), async () => {
            try {
              await request('/v1/actions/apply', 'POST', { id: entry.id });
              status(text('Copied', '已复制'));
            } catch (error) {
              showError(error);
            }
          }));
        }
        if (entry.kind === 'files') {
          actions.append(button(text('Download', '下载'), () => {
            if (globalThis.PeerCarryTransfers?.download) {
              globalThis.PeerCarryTransfers.download(entry);
            } else {
              status(text('Download is unavailable', '下载不可用'), 'error');
            }
          }));
        }
        for (const peer of state.peers) {
          const label = text('Send to ', '发送到 ') + (peer.name || peer.host || peer.node_id);
          const send = button(label, () => sendEntry(peer.node_id, entry.id));
          send.dataset.nodeId = peer.node_id;
          actions.append(send);
        }
        row.append(actions);
        list.append(row);
      }
      editor.append(list, el('div', 'notebook-status'));
    }
    async function sendEntry(nodeId, entryId) {
      if (state.busy) return;
      state.busy = true;
      try {
        await request('/v1/transfers', 'POST', {
          operation: 'send',
          node_id: nodeId,
          entry_id: entryId,
          expires_in_secs: 86400,
        });
        status(text('Queued for sending', '已加入发送队列'));
      } catch (error) {
        status(text('Send failed; it was not delivered. ', '发送失败，未确认送达。') + error.message, 'error');
      } finally {
        state.busy = false;
      }
    }
    async function share() {
      if (!state.item || state.busy) return;
      try {
        state.peers = normalize(await request('/v1/peers'))
          .filter(peer => peer && typeof peer.node_id === 'string');
      } catch (error) {
        showError(error);
        return;
      }

      const select = el('select', 'notebook-peer-filter');
      select.setAttribute('aria-label', text('Choose exact device', '选择设备'));
      const placeholder = el('option');
      placeholder.value = '';
      placeholder.textContent = text('Choose a device', '选择设备');
      select.append(placeholder);
      for (const peer of state.peers) {
        const option = el('option');
        option.value = peer.node_id;
        option.textContent = peer.name || peer.host || peer.node_id;
        select.append(option);
      }

      const send = button(text('Confirm send', '确认发送'), async () => {
        const target = select.value;
        if (!target) {
          status(text('Choose a device first.', '请先选择设备。'), 'error');
          return;
        }
        if (state.busy) return;
        state.busy = true;
        send.disabled = true;
        let published = false;
        try {
          if (!(await flush())) return;
          const publication = await request(
            '/v1/notebook/' + encodeURIComponent(state.item.id) + '/publish',
            'POST',
            { expected_revision: state.item.revision },
          );
          if (!publication.entry_id) throw new Error('Missing entry_id');
          published = true;
          await request('/v1/transfers', 'POST', {
            operation: 'send',
            node_id: target,
            entry_id: publication.entry_id,
            expires_in_secs: 86400,
          });
          status(text('Queued for sending', '已加入发送队列'));
        } catch (error) {
          const message = published
            ? text(
              'Export entered local history; send failed and was not delivered. ',
              '内容已导出到本机历史；发送失败，未确认送达。',
            )
            : text('Share failed before export; not delivered. ', '导出前分享失败，未确认送达。');
          status(message + error.message, 'error');
        } finally {
          state.busy = false;
          send.disabled = false;
          dialog.remove();
        }
      });
      const dialog = el('div', 'notebook-share-dialog');
      dialog.append(
        select,
        send,
        button(text('Cancel', '取消'), () => dialog.remove()),
      );
      root.append(dialog);
    }
    async function restoreItem(item) {
      const sequence = ++state.selectionSequence;
      if (!(await flush()) || sequence !== state.selectionSequence) return false;
      const previousId = state.item?.id;
      const version = state.version;
      const section = state.section;
      try {
        const updated = await request(
          '/v1/notebook/' + encodeURIComponent(item.id) + '/restore',
          'POST',
          { expected_revision: item.revision },
        );
        if (sequence !== state.selectionSequence || section !== state.section ||
            previousId !== state.item?.id || version !== state.version || state.dirty || state.composing) {
          await loadList({ preserveEditor: true });
          status(text('Note restored. Your current draft is kept.', '笔记已恢复，当前编辑草稿已保留。'));
          return false;
        }
        state.item = updated;
        remember(section, updated.id);
        showEditor(true);
        state.trash = false;
        state.conflict = false;
        state.conflictKind = '';
        state.syncConflictTarget = null;
        state.conflictVersions = null;
        state.dirty = false;
        state.version = state.savedVersion = 0;
        renderEditor();
        await loadList({ preserveEditor: true });
        return true;
      } catch (error) {
        if (sequence === state.selectionSequence) await showError(error, item.id);
        return false;
      }
    }
    async function showError(error, itemId) {
      if (error?.data?.error === 'sync_conflict' && itemId) {
        const keepDraft = state.dirty && state.item?.id === itemId;
        state.syncConflictTarget = { id: itemId, revision: state.item?.id === itemId ? state.item.revision : undefined };
        state.conflictKind = 'sync';
        state.conflict = keepDraft;
        state.conflictVersions = null;
        if (!keepDraft) {
          try {
            state.item = await request('/v1/notebook/' + encodeURIComponent(itemId));
            state.dirty = false;
            state.version = state.savedVersion = 0;
          } catch (_) {}
        }
        renderEditor();
        status(text(
          keepDraft ? 'Another computer has a concurrent version. Your draft is kept.' : 'This note has concurrent versions to review.',
          keepDraft ? '另一台电脑上有并发版本，当前草稿已保留。' : '这篇笔记存在需要处理的并发版本。',
        ), 'conflict');
        return;
      }
      if (error?.status === 409) {
        status(text('This note changed on the server. Reload it and try again.', '笔记版本已变化，请重新载入后再试。'), 'conflict');
        return;
      }
      status(text('Action failed: ', '操作失败：') + error.message, 'error');
    }
    function setSection(section) {
      if (!['notes', 'todo', 'clipboard'].includes(section)) return Promise.resolve(false);
      return (async () => {
        if (section === state.section) {
          if (!state.hasLoaded && section !== 'clipboard') {
            const loaded = await loadList();
            if (loaded) await restoreLast(section);
            await refreshSync();
            updateSyncRefreshTimer();
          }
          return true;
        }
        if (!(await flush())) return false;
        ++state.selectionSequence;
        ++state.listSequence;
        clearTimeout(state.searchTimer);
        state.section = section;
        state.hasLoaded = false;
        state.item = null;
        state.trash = false;
        state.search = '';
        const search = root.querySelector('.notebook-search');
        if (search) search.value = '';
        showEditor(section === 'clipboard');
        const editor = root.querySelector('.notebook-editor');
        editor.className = 'notebook-editor';
        renderSyncPanel();
        const loaded = await loadList();
        if (section !== 'clipboard' && loaded) await restoreLast(section);
        if (section !== 'clipboard') await refreshSync();
        updateSyncRefreshTimer();
        return true;
      })();
    }
    function setVisible(visible) {
      state.visible = !!visible;
      if (state.visible && state.section !== 'clipboard' && !state.sync.loaded && !state.sync.loading) {
        refreshSync();
      }
      updateSyncRefreshTimer();
    }
    async function load() {
      const loaded = await loadList();
      if (state.section !== 'clipboard' && loaded) await restoreLast(state.section);
      if (state.section !== 'clipboard') await refreshSync();
      return api;
    }
    async function restoreLast(section) {
      const itemId = recalled(section);
      if (!itemId || state.section !== section || state.item || state.search || state.trash) return;
      if (!state.items.some(item => item.id === itemId && !item.trashed)) {
        remember(section, '');
        return;
      }
      await selectItem(itemId);
    }
    const api = {
      setSection,
      setVisible,
      load,
      whenIdle: async () => {
        if (state.dirty && !state.conflict) await flush();
        while (state.saving) await new Promise(resolve => setTimeout(resolve, 0));
        return !state.dirty && !state.conflict;
      },
    };

    doc.addEventListener('keydown', event => {
      const saveKey = (event.ctrlKey || event.metaKey) && !event.shiftKey && !event.altKey &&
        event.key?.toLowerCase() === 's';
      const composing = event.isComposing || event.keyCode === 229;
      if (!saveKey || composing || root.hidden || !root.contains(event.target)) return;
      event.preventDefault();
      flush();
    });
    doc.addEventListener('compositionstart', event => {
      if (root.contains(event.target)) {
        state.composing = true;
        clearTimeout(state.timer);
      }
    });
    doc.addEventListener('compositionend', event => {
      if (root.contains(event.target)) {
        state.composing = false;
        if (state.dirty && !state.conflict) state.timer = setTimeout(() => saveLoop(), 350);
      }
    });

    shell();
    renderSyncPanel();
    return api;
  }
  return { mount };
});
