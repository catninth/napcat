// NapCat WebHID naplózó.
// A gyártó webes driverének oldalán, a DevTools Console-ba illesztve naplóz minden HID
// forgalmat (kimenő reportok, feature reportok, bejövő reportok). Semmit nem küld magától.
// Mentés: __hidsave('r75')  -> letölt egy .txt fájlt.
(() => {
  if (window.__hidlog) { console.log('[hidlog] már fut'); return; }
  const log = window.__hidlog = [];
  const t0 = performance.now();
  const bytes = (d) => d instanceof ArrayBuffer ? new Uint8Array(d) : new Uint8Array(d.buffer, d.byteOffset, d.byteLength);
  const hex = (d) => Array.from(bytes(d), (b) => b.toString(16).padStart(2, '0').toUpperCase()).join(' ');
  const h4 = (n) => n.toString(16).padStart(4, '0').toUpperCase();
  const name = (dev) => `${h4(dev.vendorId)}:${h4(dev.productId)} ${dev.productName}`;
  const push = (line) => { log.push(line); console.log('[hidlog]', line); };
  const add = (dir, dev, id, data) =>
    push(`${((performance.now() - t0) / 1000).toFixed(3).padStart(8)}s ${dir} [${name(dev)}] id=0x${id.toString(16).padStart(2, '0')} ${hex(data)}`);

  const watched = new WeakSet();
  const watch = (dev) => {
    if (watched.has(dev)) return;
    watched.add(dev);
    const pages = dev.collections.map((c) => `0x${h4(c.usagePage)}/0x${c.usage.toString(16)}`).join(', ');
    push(`DEV [${name(dev)}] collections: ${pages}`);
    dev.addEventListener('inputreport', (e) => add('IN   ', e.device, e.reportId, e.data));
  };

  const P = HIDDevice.prototype;
  const origSend = P.sendReport;
  P.sendReport = function (id, data) { add('OUT  ', this, id, data); return origSend.call(this, id, data); };
  const origSendF = P.sendFeatureReport;
  P.sendFeatureReport = function (id, data) { add('FEAT>', this, id, data); return origSendF.call(this, id, data); };
  const origRecvF = P.receiveFeatureReport;
  P.receiveFeatureReport = async function (id) { const r = await origRecvF.call(this, id); add('FEAT<', this, id, r); return r; };
  const origOpen = P.open;
  P.open = function () { watch(this); return origOpen.call(this); };

  const origReq = navigator.hid.requestDevice.bind(navigator.hid);
  navigator.hid.requestDevice = async (opts) => { const ds = await origReq(opts); ds.forEach(watch); return ds; };
  navigator.hid.getDevices().then((ds) => ds.forEach(watch));
  navigator.hid.addEventListener('connect', (e) => watch(e.device));

  window.__hidnote = (text) => push(`NOTE ${text}`);
  window.__hidsave = (n = 'hid') => {
    const a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([log.join('\n') + '\n'], { type: 'text/plain' }));
    a.download = `${n}-${new Date().toISOString().replace(/[:.]/g, '-')}.txt`;
    a.click();
  };
  console.log('[hidlog] fut. Jegyzet: __hidnote("akku 73%"), mentés: __hidsave("r75")');
})();
