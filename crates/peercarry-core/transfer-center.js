/* Persistent transfers UI. Downloads never apply or open files. */
(function (root) {
  'use strict';

  const terminal = new Set(['completed', 'failed', 'cancelled', 'expired']);
  const states = {
    queued: ['排队中', 'Queued'],
    running: ['进行中', 'In progress'],
    pending: ['等待中', 'Pending'],
    downloading: ['下载中', 'Downloading'],
    waiting_for_peer: ['等待重试', 'Waiting to retry'],
    paused: ['已暂停', 'Paused'],
    failed: ['失败', 'Failed'],
    cancelled: ['已取消', 'Cancelled'],
    expired: ['已过期', 'Expired'],
  };

  function label(job, chinese) {
    if (job.status === 'completed') {
      return job.operation === 'send'
        ? (chinese ? '条目已送达' : 'Entry delivered')
        : (chinese ? '文件已下载' : 'Files downloaded');
    }
    return (states[job.status] || ['未知状态', 'Unknown status'])[chinese ? 0 : 1];
  }

  function statusClass(status) {
    return status === 'completed' || states[status] ? `transfer-status--${status}` : 'transfer-status--unknown';
  }

  function title(job, chinese) {
    const target = job.target_name || job.node_id || '';
    return job.operation === 'send'
      ? `${chinese ? '发送至' : 'Send to'}${target ? ` ${target}` : ''}`
      : `${chinese ? '下载文件' : 'Download files'}${target ? ` · ${target}` : ''}`;
  }

  function visibleFiles(job) {
    return job.operation === 'download' && Array.isArray(job.files) ? job.files : [];
  }

  function shortError(value, max = 120) {
    const text = String(value || '').replace(/[\u0000-\u001f\u007f]/g, ' ').trim();
    if (text.length <= max) return text;
    return `${text.slice(0, Math.max(0, max - 1)).trimEnd()}…`;
  }

  function progress(job) {
    const files = visibleFiles(job);
    const bytes = files.reduce((sum, file) => sum + Math.max(0, Number(file.bytes) || 0), 0);
    const known = files.length > 0 && files.every(file => Number.isFinite(file.total_bytes) && file.total_bytes >= 0);
    const total = known ? files.reduce((sum, file) => sum + file.total_bytes, 0) : null;
    return { bytes, total, fraction: total > 0 ? Math.min(1, bytes / total) : null };
  }

  function date(value, locale) {
    if (value === undefined || value === null) return '';
    const parsed = new Date(typeof value === 'number' && value < 1e12 ? value * 1000 : value);
    return Number.isNaN(parsed.getTime()) ? '' : parsed.toLocaleString(locale || undefined);
  }

  function actions(job) {
    if (job.status === 'completed' || job.status === 'expired') return ['delete'];
    if (job.status === 'failed' || job.status === 'cancelled') return ['retry', 'delete'];
    if (job.status === 'paused') return ['retry', 'cancel'];
    if (['queued', 'running', 'waiting_for_peer'].includes(job.status)) return ['cancel'];
    return [];
  }

  const view = { label, statusClass, title, visibleFiles, shortError, progress, actions, date };
  if (typeof module !== 'undefined' && module.exports) module.exports = view;
  if (!root.document) return;

  const document = root.document;
  const $ = id => document.getElementById(id);
  if (!$('transfers')) return;
  const chinese = root.PeerCarryLocale.isChinese;
  const dateLocale = root.PeerCarryLocale.locale;
  const t = (cn, en) => chinese ? cn : en;
  const userError = error => String(error?.message || error || '');
  const peerSelect = $('transfer-peer');
  const jobsElement = $('transfer-jobs');
  let visible = false, timer = null, loading = false, submitting = false, selected = null;
  let jobs = [], peers = [], firstLoad = true;
  const samples = new Map(), announced = new Set(), pendingActions = new Set();
  const detailState = new Map();
  let pendingActionFocus = null;

  function el(tag, text, className) {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    if (className) node.className = className;
    return node;
  }
  function setText(id, text) {
    const node = $(id);
    if (node) node.textContent = text;
  }
  function size(bytes) {
    const n = Math.max(0, Number(bytes) || 0);
    if (n < 1024) return `${n} B`;
    if (n < 1048576) return `${(n / 1024).toFixed(1)} KiB`;
    if (n < 1073741824) return `${(n / 1048576).toFixed(1)} MiB`;
    return `${(n / 1073741824).toFixed(2)} GiB`;
  }
  function status(text, error = false) {
    const node = $('transfer-status');
    if (!node) return;
    node.textContent = text;
    node.className = error ? 'transfer-error' : 'transfer-note';
  }
  async function request(path, body, method = 'POST') {
    const response = await fetch(path, {
      method,
      headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const raw = await response.text();
    if (!response.ok) {
      let message = raw;
      try { message = JSON.parse(raw).error || raw; } catch (_) { /* keep response text */ }
      throw new Error(`${response.status} ${String(message).slice(0, 240)}`);
    }
    return raw ? JSON.parse(raw) : null;
  }
  function controls() {
    const send = $('transfer-send');
    if (send) {
      send.disabled = submitting || !peerSelect?.value;
      send.textContent = selected ? t('发送所选条目', 'Send selected entry') : t('发送当前剪贴板', 'Send current clipboard');
    }
    if (peerSelect) peerSelect.disabled = submitting || !peers.length;
    const ttl = $('transfer-ttl');
    if (ttl) ttl.disabled = submitting;
    const clear = $('transfer-clear-selection');
    if (clear) { clear.hidden = !selected; clear.disabled = submitting; }
    setText('transfer-selection', selected
      ? `${t('已选：', 'Selected: ')}${selected.preview || selected.id}`
      : t('发送时采集一次；重试使用这次内容。', 'Sending captures once; retries reuse that entry.'));
  }
  async function loadPeers() {
    if (!peerSelect) return;
    const previous = peerSelect.value;
    try {
      const data = await request('/v1/peers', undefined, 'GET');
      peers = (Array.isArray(data) ? data : []).filter(peer => typeof peer.node_id === 'string' && peer.node_id);
      peerSelect.replaceChildren();
      const placeholder = el('option', t('选择目标电脑…', 'Choose a target computer…'));
      placeholder.value = '';
      peerSelect.append(placeholder);
      for (const peer of peers) {
        const state = peer.online ? t('在线', 'online') : t('离线，可排队', 'offline, can queue');
        const option = el('option', `${peer.host || peer.node_id} · ${state}`);
        option.value = peer.node_id;
        peerSelect.append(option);
      }
      if (peers.some(peer => peer.node_id === previous)) peerSelect.value = previous;
    } catch (error) {
      status(`${t('读取设备失败：', 'Could not load devices: ')}${userError(error)}`, true);
    }
    controls();
  }
  function details(job, send) {
    const box = el('details', undefined, 'transfer-details');
    box.dataset.jobId = job.id;
    box.open = detailState.get(job.id) || false;
    box.addEventListener('toggle', () => {
      if (box.isConnected) detailState.set(job.id, box.open);
    });
    box.append(el('summary', t('详情', 'Details')));
    const meta = el('div', undefined, 'transfer-detail-list');
    for (const [name, value] of [
      [t('创建时间', 'Created'), date(job.created_at, dateLocale)],
      [t('到期时间', 'Expires'), date(job.expires_at, dateLocale)],
      [t('任务编号', 'Task ID'), job.id],
      [t('条目编号', 'Entry ID'), job.entry_id],
    ]) {
      if (!value) continue;
      const row = el('div', undefined, 'transfer-detail-row');
      row.append(el('span', name, 'transfer-detail-label'), el('span', value, 'transfer-detail-value'));
      meta.append(row);
    }
    box.append(meta);
    if (job.error) box.append(el('p', job.error, 'transfer-error transfer-error-detail'));
    if (!send) {
      const files = visibleFiles(job);
      if (files.length) {
        const list = el('div', undefined, 'transfer-file-list');
        for (const file of files) {
          const row = el('div', undefined, 'transfer-file-row');
          const info = el('div', undefined, 'transfer-file-info');
          info.append(el('span', file.name || '', 'transfer-file-name'));
          info.append(el('span', label({ operation: 'download', status: file.status }, chinese), `transfer-status-badge ${statusClass(file.status)}`));
          row.append(info);
          if (file.total_bytes !== undefined || file.bytes) {
            row.append(el('span', `${size(file.bytes)}${file.total_bytes !== undefined ? ` / ${size(file.total_bytes)}` : ''}`, 'transfer-file-size'));
          }
          if (file.path) row.append(el('div', file.path, 'transfer-file-path'));
          if (file.error) row.append(el('div', `${t('错误：', 'Error: ')}${file.error}`, 'transfer-error transfer-error-detail'));
          list.append(row);
        }
        box.append(list);
      }
    }
    return box;
  }
  function render() {
    if (!jobsElement) return;
    for (const item of jobsElement.querySelectorAll('details[data-job-id]')) {
      detailState.set(item.dataset.jobId, item.open);
    }
    const active = document.activeElement;
    if (active && jobsElement.contains(active) && active.dataset?.jobId && active.dataset?.action) {
      pendingActionFocus = { id: active.dataset.jobId, action: active.dataset.action };
    } else if (pendingActionFocus && active && active !== document.body && !jobsElement.contains(active)) {
      pendingActionFocus = null;
    }
    const filter = $('transfer-filter')?.value || 'all';
    const shown = jobs.filter(job => filter === 'all' || (filter === 'active' ? !terminal.has(job.status) : job.operation === filter));
    jobsElement.replaceChildren();
    if (!shown.length) jobsElement.append(el('p', t('暂无传输任务', 'No transfers'), 'empty'));
    const now = Date.now();
    for (const job of shown) {
      const send = job.operation === 'send';
      const card = el('article', undefined, 'transfer-card');
      const heading = el('div', undefined, 'transfer-card-heading');
      heading.append(el('strong', title(job, chinese), 'transfer-card-title'));
      heading.append(el('span', label(job, chinese), `transfer-status-badge ${statusClass(job.status)}`));
      card.append(heading);

      if (job.status === 'failed' && job.error) {
        card.append(el('p', `${t('错误：', 'Error: ')}${shortError(job.error)}`, 'transfer-error-short'));
      }
      if (!send) {
        const amount = progress(job);
        const bar = el('progress');
        bar.max = 1;
        bar.setAttribute('aria-label', t('下载进度', 'Download progress'));
        if (amount.fraction !== null) bar.value = amount.fraction;
        else if (job.status !== 'running') bar.value = job.status === 'completed' ? 1 : 0;
        card.append(bar);
        const previous = samples.get(job.id);
        const speed = previous && now > previous.time && amount.bytes >= previous.bytes && job.status === 'running'
          ? (amount.bytes - previous.bytes) * 1000 / (now - previous.time) : null;
        samples.set(job.id, { bytes: amount.bytes, time: now });
        card.append(el('div', `${size(amount.bytes)}${amount.total !== null ? ` / ${size(amount.total)}` : ''}${speed !== null ? ` · ${size(speed)}/s` : ''}`, 'transfer-progress-label'));
      }
      card.append(details(job, send));

      const buttons = el('div', undefined, 'transfer-card-actions');
      for (const action of actions(job)) {
        const text = action === 'retry' ? t('重试', 'Retry') : action === 'delete' ? t('删除记录', 'Remove record') : t('取消', 'Cancel');
        const button = el('button', text, action === 'retry' ? 'transfer-action-primary' : 'transfer-action-secondary');
        button.dataset.jobId = job.id;
        button.dataset.action = action;
        button.disabled = pendingActions.has(job.id);
        button.onclick = () => change(job, action);
        buttons.append(button);
      }
      card.append(buttons);
      jobsElement.append(card);
    }
    const currentIds = new Set(jobs.map(job => job.id));
    for (const id of samples.keys()) if (!currentIds.has(id)) samples.delete(id);
    for (const id of detailState.keys()) if (!currentIds.has(id)) detailState.delete(id);
    if (pendingActionFocus) {
      const target = Array.from(jobsElement.querySelectorAll('button[data-job-id][data-action]'))
        .find(button => button.dataset.jobId === pendingActionFocus.id && button.dataset.action === pendingActionFocus.action);
      if (!target) {
        if (!pendingActions.has(pendingActionFocus.id)) pendingActionFocus = null;
      } else if (!target.disabled && document.activeElement === document.body) {
        target.focus();
        pendingActionFocus = null;
      }
    }
  }
  async function refresh() {
    if (loading) return;
    loading = true;
    try {
      const data = await request('/v1/transfers', undefined, 'GET');
      jobs = Array.isArray(data) ? data : [];
      for (const job of jobs) {
        if (terminal.has(job.status) && !announced.has(job.id)) {
          announced.add(job.id);
          if (!firstLoad) {
            status(`${label(job, chinese)} · ${job.target_name || job.entry_id || ''}`, job.status === 'failed');
            if (typeof root.show === 'function') root.show(label(job, chinese), job.status === 'completed' ? 'ok' : 'err');
          }
        }
      }
      firstLoad = false;
      render();
    } catch (error) {
      status(`${t('读取任务失败：', 'Could not load transfers: ')}${userError(error)}`, true);
    } finally { loading = false; }
  }
  async function change(job, action) {
    if (pendingActions.has(job.id)) return;
    pendingActions.add(job.id);
    render();
    try {
      const path = `/v1/transfers/${encodeURIComponent(job.id)}${action === 'delete' ? '' : `/${action}`}`;
      await request(path, undefined, action === 'delete' ? 'DELETE' : 'POST');
      if (action === 'retry') announced.delete(job.id);
      if (action === 'delete') samples.delete(job.id);
      status(action === 'delete' ? t('已移除任务记录，文件保留。', 'Record removed; downloaded files are kept.') : t('操作已保存。', 'Action saved.'));
      await refresh();
    } catch (error) { status(`${t('操作失败：', 'Action failed: ')}${userError(error)}`, true); }
    finally { pendingActions.delete(job.id); render(); }
  }
  async function enqueue(body) {
    if (submitting) return;
    submitting = true;
    controls();
    status(t('正在保存…', 'Saving…'));
    try {
      await request('/v1/transfers', body);
      status(t('已加入队列。', 'Added to the queue.'));
      await refresh();
    } catch (error) {
      status(`${t('创建任务失败：', 'Could not create transfer: ')}${userError(error)}`, true);
    } finally { submitting = false; controls(); }
  }

  setText('transfer-title', t('传输中心', 'Transfers'));
  setText('transfer-help', t('发送文件时，请保持源电脑在线。下载的文件不会覆盖剪贴板。', 'Keep the source computer online for file transfers. Downloads leave your clipboard unchanged.'));
  setText('transfer-refresh', t('刷新', 'Refresh'));
  setText('transfer-clear-selection', t('改用当前剪贴板', 'Use current clipboard'));
  setText('transfer-compose-title', t('新建发送', 'New send'));
  setText('transfer-options-title', t('更多选项', 'More options'));
  setText('transfer-ttl-label', t('等待多久', 'Keep queued for'));
  setText('transfer-list-title', t('全部记录', 'Activity'));
  peerSelect?.setAttribute('aria-label', t('目标电脑', 'Target computer'));
  $('transfer-ttl')?.setAttribute('aria-label', t('任务有效期', 'Transfer expiry'));
  $('transfer-filter')?.setAttribute('aria-label', t('筛选传输任务', 'Filter transfers'));
  const ttl = $('transfer-ttl');
  if (ttl?.options?.length >= 3) {
    ttl.options[0].textContent = t('1 小时', '1 hour');
    ttl.options[1].textContent = t('24 小时', '24 hours');
    ttl.options[2].textContent = t('7 天', '7 days');
  }
  for (const [value, cn, en] of [['all', '全部', 'All'], ['active', '进行中', 'Active'], ['send', '发送', 'Sends'], ['download', '下载', 'Downloads']]) {
    const option = el('option', t(cn, en));
    option.value = value;
    $('transfer-filter')?.append(option);
  }
  if ($('transfer-filter')) $('transfer-filter').onchange = render;
  if (peerSelect) peerSelect.onchange = controls;
  if ($('transfer-refresh')) $('transfer-refresh').onclick = () => { loadPeers(); refresh(); };
  if ($('transfer-clear-selection')) $('transfer-clear-selection').onclick = () => { selected = null; controls(); };
  if ($('transfer-send')) $('transfer-send').onclick = () => {
    if (!peerSelect?.value) return;
    const ttlValue = Number($('transfer-ttl')?.value || 86400);
    const body = { operation: 'send', node_id: peerSelect.value, expires_in_secs: ttlValue };
    if (selected) body.entry_id = selected.id;
    enqueue(body);
  };
  root.PeerCarryTransfers = {
    setVisible(value) {
      visible = value;
      clearInterval(timer);
      timer = null;
      if (value) {
        loadPeers(); refresh();
        timer = setInterval(() => { if (visible && !document.hidden) refresh(); }, 2500);
      }
    },
    selectEntry(entry) {
      if (!entry || submitting) return;
      selected = { id: entry.id, preview: entry.preview };
      if ($('transfer-compose')) $('transfer-compose').open = true;
      controls();
      if (typeof root.showTab === 'function') root.showTab('transfers');
    },
    download(entry) {
      if (!entry || entry.kind !== 'files' || submitting) return;
      if (typeof root.showTab === 'function') root.showTab('transfers');
      enqueue({ operation: 'download', entry_id: entry.id, expires_in_secs: 86400 });
    },
  };
  document.addEventListener('visibilitychange', () => { if (visible && !document.hidden) refresh(); });
  controls();
})(typeof window !== 'undefined' ? window : globalThis);
