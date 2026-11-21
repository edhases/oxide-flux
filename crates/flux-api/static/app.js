'use strict';

const $ = (id) => document.getElementById(id);
const STORAGE_KEY = 'oxideflux.token';

let token = localStorage.getItem(STORAGE_KEY) || '';
let socket = null;
let reconnectTimer = null;
let nextId = 1;
let torrents = [];
let selectedId = null;
let connected = false;

function log(message) {
  const box = $('log');
  box.classList.remove('hidden');
  box.textContent = `${new Date().toLocaleTimeString()}  ${message}\n${box.textContent}`;
}

function formatBytes(value) {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let n = Number(value) || 0;
  let unit = 0;
  while (n >= 1024 && unit < units.length - 1) {
    n /= 1024;
    unit += 1;
  }
  return `${n.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

async function rpc(method, params) {
  const body = { jsonrpc: '2.0', id: nextId++, method };
  if (params !== undefined) body.params = params;
  const headers = { 'Content-Type': 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const response = await fetch('/rpc', {
    method: 'POST',
    headers,
    body: JSON.stringify(body),
  });
  if (response.status === 401) throw new Error('unauthorized: check the token');
  if (response.status === 413) throw new Error('request too large');
  const payload = await response.json().catch(() => null);
  if (!payload) throw new Error(`HTTP ${response.status}`);
  if (payload.error) throw new Error(`${payload.error.message} (${payload.error.code})`);
  return payload.result;
}

function setStatus(text, bad = false) {
  const node = $('status');
  node.textContent = text;
  node.style.color = bad ? 'var(--bad)' : '';
}

async function connect() {
  token = $('token').value.trim();
  if (token) localStorage.setItem(STORAGE_KEY, token);
  else localStorage.removeItem(STORAGE_KEY);
  try {
    const info = await rpc('session.info');
    connected = true;
    setStatus(`${info.name} ${info.version} · api v${info.apiVersion}`);
    openSocket();
    await refresh();
  } catch (error) {
    connected = false;
    setStatus(String(error.message || error), true);
    log(String(error.message || error));
  }
}

function openSocket() {
  if (socket) {
    socket.onclose = null;
    socket.close();
  }
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const query = token ? `?token=${encodeURIComponent(token)}` : '';
  socket = new WebSocket(`${protocol}//${location.host}/ws${query}`);
  socket.onopen = () => log('events connected');
  socket.onmessage = (event) => {
    let message;
    try {
      message = JSON.parse(event.data);
    } catch {
      return;
    }
    if (message.method === 'torrent.updated') {
      const summary = message.params;
      const index = torrents.findIndex((item) => item.id === summary.id);
      if (index >= 0) torrents[index] = summary;
      else torrents.push(summary);
      renderTorrents();
      if (selectedId === summary.id) loadFiles(selectedId);
    } else if (message.method === 'torrent.removed') {
      torrents = torrents.filter((item) => item.id !== message.params.id);
      if (selectedId === message.params.id) closeFiles();
      renderTorrents();
    } else if (message.method === 'session.shutdown') {
      log('session shutting down');
    }
  };
  socket.onclose = () => {
    if (!connected) return;
    log('events disconnected, retrying…');
    clearTimeout(reconnectTimer);
    reconnectTimer = setTimeout(openSocket, 2000);
  };
}

async function refresh() {
  const [list, stats] = await Promise.all([rpc('torrent.list'), rpc('session.stats')]);
  torrents = list || [];
  renderTorrents();
  $('stats').textContent =
    `${stats.torrents} torrents · ${stats.active} active · ${stats.seeding} seeding · ` +
    `${stats.downloading} downloading · ↓ ${formatBytes(stats.bytesDownloaded)} · ↑ ${formatBytes(stats.bytesUploaded)}`;
}

function badge(state) {
  const span = document.createElement('span');
  span.className = `badge ${state}`;
  span.textContent = state;
  return span;
}

function actionButton(label, onClick) {
  const button = document.createElement('button');
  button.type = 'button';
  button.textContent = label;
  button.addEventListener('click', onClick);
  return button;
}

function renderTorrents() {
  const list = $('torrents');
  list.replaceChildren();
  $('empty').classList.toggle('hidden', torrents.length > 0);

  for (const torrent of torrents) {
    const item = document.createElement('li');
    item.className = 'torrent';

    const head = document.createElement('div');
    head.className = 'torrent-head';
    const name = document.createElement('span');
    name.className = 'torrent-name';
    name.textContent = torrent.name;
    const right = document.createElement('span');
    right.className = 'torrent-meta';
    right.append(badge(torrent.state));
    if (torrent.private) {
      const priv = document.createElement('span');
      priv.className = 'badge';
      priv.textContent = 'private';
      right.append(' ', priv);
    }
    head.append(name, right);

    const bar = document.createElement('div');
    bar.className = 'bar';
    const fill = document.createElement('span');
    const percent = Math.max(0, Math.min(1, Number(torrent.progress) || 0)) * 100;
    fill.style.width = `${percent.toFixed(1)}%`;
    bar.append(fill);

    const meta = document.createElement('div');
    meta.className = 'torrent-meta';
    meta.textContent =
      `${percent.toFixed(1)}% · ${torrent.piecesHave}/${torrent.piecesTotal} pieces · ` +
      formatBytes(torrent.totalBytes);

    const actions = document.createElement('div');
    actions.className = 'actions';
    if (torrent.state === 'paused') {
      actions.append(actionButton('Resume', () => mutate('torrent.resume', { id: torrent.id })));
    } else {
      actions.append(actionButton('Pause', () => mutate('torrent.pause', { id: torrent.id })));
    }
    actions.append(
      actionButton('Files', () => loadFiles(torrent.id)),
      actionButton('Remove', () => removeTorrent(torrent.id, false)),
      actionButton('Remove + files', () => removeTorrent(torrent.id, true)),
    );

    item.append(head, bar, meta, actions);
    list.append(item);
  }
}

async function mutate(method, params) {
  try {
    await rpc(method, params);
    await refresh();
  } catch (error) {
    log(`${method}: ${error.message || error}`);
  }
}

async function removeTorrent(id, deleteFiles) {
  const question = deleteFiles
    ? 'Remove the torrent and delete its files?'
    : 'Remove the torrent and keep its files?';
  if (!window.confirm(question)) return;
  await mutate('torrent.remove', { id, deleteFiles });
}

async function addFile() {
  const input = $('torrent-file');
  const file = input.files && input.files[0];
  if (!file) {
    log('choose a .torrent file first');
    return;
  }
  try {
    log(`reading ${file.name}…`);
    const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
    const summary = await rpc('torrent.add', { kind: 'metainfo', bytes });
    $('json-result').value = `added ${summary.name}`;
    input.value = '';
    await refresh();
  } catch (error) {
    log(String(error.message || error));
  }
}

async function addMagnet() {
  const uri = $('magnet').value.trim();
  if (!uri) return;
  try {
    log('fetching metadata from the swarm…');
    const summary = await rpc('torrent.add', { kind: 'magnet', uri });
    $('json-result').value = `added ${summary.name}`;
    $('magnet').value = '';
    await refresh();
  } catch (error) {
    log(String(error.message || error));
  }
}

async function loadFiles(id) {
  try {
    const detail = await rpc('torrent.get', { id });
    selectedId = id;
    $('files-panel').classList.remove('hidden');
    $('files-title').textContent = `Files · ${detail.name}`;
    const list = $('files');
    list.replaceChildren();
    for (const file of detail.files || []) {
      const item = document.createElement('li');
      item.className = 'file';

      const path = document.createElement('span');
      path.className = 'file-path';
      path.textContent = file.path;
      const size = document.createElement('small');
      size.textContent = ` ${formatBytes(file.length)}`;
      path.append(size);

      const select = document.createElement('select');
      for (const value of ['skip', 'low', 'normal', 'high']) {
        const option = document.createElement('option');
        option.value = value;
        option.textContent = value;
        if (value === file.priority) option.selected = true;
        if (value === 'low') option.disabled = true;
        if (value === 'low' && file.priority !== 'low') option.textContent = 'low (unavailable)';
        select.append(option);
      }
      select.addEventListener('change', async () => {
        try {
          await rpc('torrent.setFilePriorities', {
            id,
            priorities: [{ index: file.index, priority: select.value }],
          });
          await refresh();
        } catch (error) {
          log(`priority: ${error.message || error}`);
          await loadFiles(id);
        }
      });

      item.append(path, select);
      list.append(item);
    }
  } catch (error) {
    log(String(error.message || error));
  }
}

function closeFiles() {
  selectedId = null;
  $('files-panel').classList.add('hidden');
}

$('token').value = token;
$('connect').addEventListener('click', connect);
$('token').addEventListener('keydown', (event) => {
  if (event.key === 'Enter') connect();
});
$('add-file').addEventListener('click', addFile);
$('add-magnet').addEventListener('click', addMagnet);
$('magnet').addEventListener('keydown', (event) => {
  if (event.key === 'Enter') addMagnet();
});
$('files-close').addEventListener('click', closeFiles);
$('json-form').addEventListener('submit', (event) => event.preventDefault());

// A token saved from a previous session connects immediately.
if (token) connect();
