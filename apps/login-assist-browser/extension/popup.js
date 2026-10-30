import { qrcode } from './vendor/qrcode.mjs';
const $ = id => document.getElementById(id);
let shownTicket = '';
async function update() {
  const state = await chrome.runtime.sendMessage({ type: 'status' });
  $('origin').textContent = state.origin || '未连接';
  $('ticket').value = state.ticket ? JSON.stringify(state.ticket) : '';
  if ($('ticket').value !== shownTicket) {
    shownTicket = $('ticket').value;
    $('qr').hidden = !shownTicket;
    if (shownTicket) {
      const qr = qrcode(0, 'M');
      qr.addData(shownTicket); qr.make();
      $('qr').src = qr.createDataURL(4, 16);
    } else { $('qr').removeAttribute('src'); }
  }
  $('start').disabled = state.connected || !$('confirm').checked;
  $('stop').disabled = !state.connected;
  $('status').textContent = state.error || (state.ticket ? '等待手机配对，有效期 60 秒' : state.connected ? '接管中' : '未开始');
}
$('confirm').onchange = update;
$('start').onclick = async () => {
  $('start').disabled = true;
  const result = await chrome.runtime.sendMessage({ type: 'start', bind: $('bind').value.trim() });
  if (result.error) $('status').textContent = result.error;
  else await update();
};
$('stop').onclick = async () => { await chrome.runtime.sendMessage({ type: 'stop' }); await update(); };
await update();
setInterval(update, 1000);
