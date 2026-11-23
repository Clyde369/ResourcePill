// Smart lists: shopping lists with running totals, a calculator and AI price comparison. Runs only
// on our own page (https://%HOST%/lists.html); src/lists.rs keeps the lists on disk and asks the AI.
//  page → pill: list-load, list-save:<json>, list-prices:<json>, list-share:<whatsapp|email>|<text>
//  pill → page: {lists: <saved data or null>, region: "ZA"}, {prices: {...}}, {pricesErr: "…"}
// Everything shown comes from the user or the AI, so it only ever goes in as text, never as HTML.
(() => {
  if (window.top !== window || !window.chrome || !window.chrome.webview) return;
  if (location.hostname !== '%HOST%' || location.pathname !== '/lists.html') return;
  const pm = window.chrome.webview.postMessage.bind(window.chrome.webview);
  const post = s => pm('%TOKEN%|' + s);

  const CURRENCY = { ZA: 'ZAR', US: 'USD', GB: 'GBP', AU: 'AUD', CA: 'CAD', NZ: 'NZD', IN: 'INR', NG: 'NGN', KE: 'KES', BW: 'BWP', NA: 'NAD', ZW: 'USD', JP: 'JPY', CH: 'CHF', SE: 'SEK', NO: 'NOK', DK: 'DKK', BR: 'BRL', MX: 'MXN', AE: 'AED' };
  const EURO = 'AT BE CY DE EE ES FI FR GR HR IE IT LT LU LV MT NL PT SI SK'.split(' ');
  let data = null, region = 'US', saveTimer = 0, busy = false, results = null;
  const uid = () => Math.random().toString(36).slice(2, 10);
  const cur = () => data.lists.find(l => l.id === data.current) || data.lists[0];

  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else if (k === 'value') el.value = v;
      else if (k === 'checked') el.checked = v;
      else if (v !== false && v != null) el.setAttribute(k, v === true ? '' : v);
    }
    for (const k of kids) if (k != null && k !== false) el.append(k);
    return el;
  }
  const money = (n, c) => {
    if (!isFinite(n)) return '–';
    try { return new Intl.NumberFormat(undefined, { style: 'currency', currency: c || cur().currency }).format(n); } catch (e) { return n.toFixed(2); }
  };
  const num = s => { const v = parseFloat(String(s).replace(',', '.').replace(/[^0-9.\-]/g, '')); return isFinite(v) ? v : null; };

  // ---------- calculator: + − × ÷ ( ) and %, no eval ----------
  // "200 - 10%" takes 10% of 200 off, like a pocket calculator; "15% * 80" is 12
  function calc(src) {
    const s = src.replace(/×|x(?=\s*[\d(.])/gi, '*').replace(/÷/g, '/').replace(/,/g, '.').replace(/\s+/g, '');
    let i = 0;
    const peek = () => s[i];
    function primary() {
      if (peek() === '(') { i++; const v = expr(); if (s[i++] !== ')') throw 0; return { v, pct: false }; }
      const m = /^\d*\.?\d+/.exec(s.slice(i));
      if (!m) throw 0;
      i += m[0].length;
      let v = parseFloat(m[0]), pct = false;
      if (peek() === '%') { i++; pct = true; }
      return { v, pct };
    }
    function unary() { if (peek() === '-') { i++; const p = unary(); return { v: -p.v, pct: p.pct }; } if (peek() === '+') { i++; } return primary(); }
    function term() {
      let a = unary(), v = a.pct ? a.v / 100 : a.v, pct = a.pct;
      while (peek() === '*' || peek() === '/') {
        const op = s[i++], b = unary(), bv = b.pct ? b.v / 100 : b.v;
        v = op === '*' ? v * bv : v / bv; pct = false;
      }
      return { v, pct, raw: a.v };
    }
    function expr() {
      let a = term(), v = a.v;
      while (peek() === '+' || peek() === '-') {
        const op = s[i++], b = term();
        const bv = b.pct ? v * b.raw / 100 : b.v; // "x + 15%" adds 15% of x
        v = op === '+' ? v + bv : v - bv;
      }
      return v;
    }
    const v = expr();
    if (i !== s.length || !isFinite(v)) throw 0;
    return Math.round(v * 1e6) / 1e6;
  }

  // ---------- data ----------
  function fresh() {
    const c = EURO.includes(region) ? 'EUR' : CURRENCY[region] || 'USD';
    const id = uid();
    return { version: 1, current: id, lists: [{ id, name: 'Shopping', currency: c, budget: null, stores: '', items: [] }] };
  }
  function save() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => post('list-save:' + JSON.stringify(data)), 300);
  }
  // "2x milk 1L", "milk x2", "3 apples" → qty + name
  function parseItem(text) {
    let t = text.trim(), qty = 1, m;
    if ((m = /^(\d+(?:[.,]\d+)?)\s*(?:x|×|\*)?\s+(.+)$/i.exec(t))) { qty = num(m[1]); t = m[2]; }
    else if ((m = /^(.+?)\s*(?:x|×|\*)\s*(\d+(?:[.,]\d+)?)$/i.exec(t))) { t = m[1]; qty = num(m[2]); }
    return { id: uid(), name: t.slice(0, 120), qty: qty || 1, price: null, store: '', done: false };
  }
  const lineTotal = it => (it.price == null ? 0 : it.price * (it.qty || 1));

  // ---------- AI price comparison ----------
  function compare() {
    const list = cur();
    const items = list.items.filter(it => !it.done).map(it => ({ name: it.name, qty: it.qty }));
    if (!items.length || busy) return;
    busy = true; results = null; render();
    post('list-prices:' + JSON.stringify({ items, stores: list.stores || '', currency: list.currency }));
  }
  function useOffer(name, offer) {
    const it = cur().items.find(i => i.name.toLowerCase() === name.toLowerCase());
    if (!it) return;
    it.price = offer.price; it.store = offer.store;
    save(); render();
  }
  function useStore(store) {
    for (const r of results.items) {
      const o = r.offers.find(o => o.store === store);
      if (o) useOffer(r.name, o);
    }
  }
  // per store: what the items it has would cost (with your quantities), and how many it has
  function storeTotals() {
    const qty = name => (cur().items.find(i => i.name.toLowerCase() === name.toLowerCase()) || {}).qty || 1;
    const by = {};
    for (const r of results.items) for (const o of r.offers) {
      const s = (by[o.store] ||= { store: o.store, total: 0, count: 0 });
      s.total += o.price * qty(r.name); s.count++;
    }
    let mixed = 0, found = 0;
    for (const r of results.items) if (r.offers.length) { mixed += Math.min(...r.offers.map(o => o.price)) * qty(r.name); found++; }
    return { stores: Object.values(by).sort((a, b) => b.count - a.count || a.total - b.total), mixed, found };
  }

  // ---------- export ----------
  function asText(withPrices) {
    const list = cur();
    const lines = list.items.map(it => `${it.done ? '☑' : '☐'} ${it.qty !== 1 ? it.qty + '× ' : ''}${it.name}${withPrices && it.price != null ? ' — ' + money(lineTotal(it)) + (it.store ? ' @ ' + it.store : '') : ''}`);
    const total = list.items.reduce((a, it) => a + lineTotal(it), 0);
    return `${list.name}\n${lines.join('\n')}${withPrices && total ? '\n\nTotal: ' + money(total) : ''}`;
  }
  async function copy(text, note) {
    try { await navigator.clipboard.writeText(text); toast(note || 'Copied'); } catch (e) { toast("Couldn't copy"); }
  }

  // ---------- view ----------
  const root = h('div', { id: 'app' });
  let toastEl, calcOut;
  function toast(t) { toastEl.textContent = t; toastEl.classList.add('on'); setTimeout(() => toastEl.classList.remove('on'), 2200); }

  function render() {
    const list = cur();
    const total = list.items.reduce((a, it) => a + lineTotal(it), 0);
    const inCart = list.items.filter(i => i.done).reduce((a, it) => a + lineTotal(it), 0);
    const priced = list.items.filter(i => i.price != null).length;
    const focus = document.activeElement && document.activeElement.dataset.k;

    const add = h('input', { class: 'add', 'data-k': 'add', placeholder: 'Add an item… (2x milk 1L, bread, 6 eggs) and press Enter', onkeydown: e => {
      if (e.key !== 'Enter' || !e.target.value.trim()) return;
      for (const part of e.target.value.split(/[,;\n]/).filter(p => p.trim())) list.items.push(parseItem(part));
      save(); render();
    } });

    const rows = list.items.map(it => h('div', { class: 'row' + (it.done ? ' done' : '') },
      h('input', { type: 'checkbox', checked: it.done, title: 'In the trolley', onchange: e => { it.done = e.target.checked; save(); render(); } }),
      h('input', { class: 'qty', value: String(it.qty), title: 'Quantity', onchange: e => { it.qty = num(e.target.value) || 1; save(); render(); } }),
      h('input', { class: 'name', value: it.name, onchange: e => { it.name = e.target.value.trim().slice(0, 120) || it.name; save(); render(); } }),
      h('input', { class: 'price', value: it.price == null ? '' : String(it.price), placeholder: 'price', title: 'Price each (you can type a sum, like 3*12.99)', onchange: e => {
        const v = e.target.value.trim();
        try { it.price = v ? calc(v) : null; } catch (_) { it.price = num(v); }
        save(); render();
      } }),
      h('span', { class: 'line' }, it.price == null ? '' : money(lineTotal(it))),
      h('span', { class: 'store', title: it.store }, it.store || ''),
      h('button', { class: 'x', title: 'Remove', onclick: () => { list.items = list.items.filter(i => i !== it); save(); render(); } }, '×')));

    const budget = list.budget;
    const left = budget != null ? budget - total : null;
    const totals = h('div', { class: 'totals' },
      h('div', {}, h('small', {}, 'List total'), h('b', {}, money(total)), priced < list.items.length ? h('small', {}, `${list.items.length - priced} without a price`) : null),
      h('div', {}, h('small', {}, 'In trolley'), h('b', {}, money(inCart))),
      h('div', {}, h('small', {}, 'Still to get'), h('b', {}, money(total - inCart))),
      h('div', {}, h('small', {}, 'Budget'), h('input', { class: 'budget', value: budget == null ? '' : String(budget), placeholder: 'none', onchange: e => { list.budget = num(e.target.value); save(); render(); } }),
        left != null ? h('small', { class: left < 0 ? 'over' : 'ok' }, left < 0 ? money(-left) + ' over' : money(left) + ' left') : null));

    const calcIn = h('input', { class: 'calc', 'data-k': 'calc', placeholder: 'Calculator: 3*24.99 - 10%', onkeyup: e => {
      const v = e.target.value.trim();
      if (!v) { calcOut.textContent = ''; return; }
      try { calcOut.textContent = '= ' + calc(v); calcOut.classList.remove('bad'); } catch (_) { calcOut.textContent = '…'; calcOut.classList.add('bad'); }
    } });
    calcOut = h('span', { class: 'calcOut', title: 'Click to copy', onclick: () => calcOut.textContent.startsWith('=') && copy(calcOut.textContent.slice(2)) });

    let ai;
    if (busy) ai = h('div', { class: 'ai muted' }, 'Looking up current prices… (searching the web, about 20–60 seconds)');
    else if (results) {
      const t = storeTotals();
      ai = h('div', { class: 'ai' },
        h('div', { class: 'aihead' }, h('b', {}, results.estimated ? 'Estimated prices' : 'Prices found'), results.note ? h('span', { class: 'muted' }, results.note) : null, h('button', { onclick: () => { results = null; render(); } }, 'Close')),
        results.estimated ? h('div', { class: 'warn' }, 'Estimates, not live prices: your free Gemini key has no Google Search quota right now, so these are typical prices Gemini knows of. Check them in the shop.') : null,
        h('div', { class: 'sum' },
          h('div', {}, h('small', {}, `Cheapest, mixing stores (${t.found} items)`), h('b', {}, money(t.mixed, results.currency))),
          ...t.stores.slice(0, 4).map(s => h('div', {}, h('small', {}, `${s.store} (${s.count} of ${results.items.length})`), h('b', {}, money(s.total, results.currency)),
            h('button', { class: 'mini', title: 'Use this store\'s prices on the list', onclick: () => useStore(s.store) }, 'Use')))),
        ...results.items.map(r => h('div', { class: 'item' },
          h('div', { class: 'iname' }, r.name),
          r.offers.length ? h('div', { class: 'offers' }, ...r.offers.map((o, n) => h('div', { class: 'offer' + (n === 0 ? ' best' : '') },
            h('b', {}, money(o.price, results.currency)), h('span', {}, o.store), o.size ? h('span', { class: 'muted' }, o.size) : null,
            o.url ? h('a', { href: o.url, title: o.url }, 'view') : null,
            h('button', { class: 'mini', onclick: () => useOffer(r.name, o) }, 'Use'))))
            : h('div', { class: 'muted' }, 'No price found'))),
        h('div', { class: 'muted small' }, 'Prices come from a web search by AI and can be out of date or wrong: check before you buy.'));
    }

    const header = h('div', { class: 'head' },
      h('select', { onchange: e => { data.current = e.target.value; results = null; save(); render(); } },
        ...data.lists.map(l => h('option', { value: l.id, selected: l.id === list.id }, `${l.name} (${l.items.length})`))),
      h('button', { onclick: () => { const id = uid(); data.lists.push({ id, name: 'New list', currency: list.currency, budget: null, stores: '', items: [] }); data.current = id; results = null; save(); render(); } }, '+ New list'),
      h('input', { class: 'rename', value: list.name, title: 'List name', onchange: e => { list.name = e.target.value.trim().slice(0, 60) || list.name; save(); render(); } }),
      h('select', { title: 'Currency', onchange: e => { list.currency = e.target.value; save(); render(); } },
        ...[...new Set([list.currency, 'ZAR', 'USD', 'EUR', 'GBP', 'AUD', 'CAD', 'INR', 'NGN', 'KES'])].map(c => h('option', { value: c, selected: c === list.currency }, c))),
      h('button', { class: 'danger', title: 'Delete this list', onclick: () => {
        if (!confirm(`Delete "${list.name}"?`)) return;
        data.lists = data.lists.filter(l => l !== list);
        if (!data.lists.length) data = fresh();
        data.current = data.lists[0].id; save(); render();
      } }, 'Delete'));

    const tools = h('div', { class: 'tools' },
      h('button', { class: 'primary', disabled: busy || !list.items.some(i => !i.done), onclick: compare, title: 'Search current prices for the items not yet ticked off' }, busy ? 'Comparing…' : '✨ Compare prices with AI'),
      h('input', { class: 'stores', value: list.stores || '', placeholder: 'Stores to check (optional): Checkers, Pick n Pay, Woolworths…', onchange: e => { list.stores = e.target.value.slice(0, 200); save(); } }),
      h('span', { class: 'grow' }),
      h('button', { onclick: () => copy(asText(true), 'List copied') }, 'Copy'),
      h('button', { title: 'Opens WhatsApp with the list: send it to yourself and it is on your phone', onclick: () => post('list-share:whatsapp|' + asText(false)) }, 'WhatsApp'),
      h('button', { onclick: () => post('list-share:email|' + asText(true)) }, 'Email'),
      h('button', { title: 'Copies the list and opens Google Keep: paste it into a new note (⋮ → Show checkboxes)', onclick: async () => { await copy(asText(false), 'Copied: paste it into a new Keep note'); setTimeout(() => { location.href = 'https://keep.google.com/'; }, 600); } }, 'Google Keep'));

    root.replaceChildren(header, add, h('div', { class: 'rows' }, ...rows, rows.length ? null : h('div', { class: 'empty muted' }, 'Nothing on this list yet. Type items above: “2x milk, bread, 6 eggs”.')),
      totals, h('div', { class: 'calcrow' }, calcIn, calcOut), tools, ai, toastEl);
    if (focus) { const el = root.querySelector(`[data-k="${focus}"]`); if (el) el.focus(); }
  }

  function start() {
    document.head.append(h('style', {}, CSS));
    toastEl = h('div', { class: 'toast' });
    document.body.append(root);
    window.chrome.webview.addEventListener('message', e => {
      const m = e.data;
      if ('lists' in m) {
        region = m.region || region;
        data = m.lists && Array.isArray(m.lists.lists) && m.lists.lists.length ? m.lists : fresh();
        render();
      }
      if (m.prices) { busy = false; results = m.prices; render(); }
      if (m.pricesErr) { busy = false; render(); toast(m.pricesErr); }
    });
    post('list-load');
  }

  const CSS = `
  * { box-sizing: border-box; }
  html, body { margin: 0; height: 100%; background: #111215; color: #eee; font: 13px "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif; }
  #app { padding: 12px 14px 70px; display: flex; flex-direction: column; gap: 10px; max-width: 980px; margin: 0 auto; }
  input, select, button { font: inherit; color: inherit; background: #1d1f24; border: 1px solid #2c2f36; border-radius: 8px; height: 30px; padding: 0 10px; outline: none; }
  input:focus, select:focus { border-color: #5ac8fa; }
  button { cursor: pointer; background: #262930; } button:hover { background: #31353d; } button:disabled { opacity: .5; cursor: default; }
  button.primary { background: #0a84ff; border-color: #0a84ff; font-weight: 600; } button.primary:hover { background: #2a95ff; }
  button.danger:hover { background: #5a1f1f; } button.mini { height: 24px; padding: 0 8px; font-size: 12px; }
  .head, .tools, .calcrow { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .rename { flex: 1; min-width: 120px; font-weight: 600; } .add { height: 36px; font-size: 14px; }
  .rows { display: flex; flex-direction: column; gap: 4px; }
  .row { display: grid; grid-template-columns: 22px 52px 1fr 90px 90px minmax(0, 120px) 28px; gap: 6px; align-items: center; }
  .row.done .name { text-decoration: line-through; opacity: .55; }
  .row input[type=checkbox] { width: 18px; height: 18px; accent-color: #30d158; }
  .qty { text-align: center; padding: 0 4px; } .price { text-align: right; }
  .line { text-align: right; font-variant-numeric: tabular-nums; } .store { opacity: .6; font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .x { height: 26px; width: 26px; padding: 0; background: none; border: 0; opacity: .5; font-size: 16px; } .x:hover { opacity: 1; background: #3a1e1e; }
  .totals { display: grid; grid-template-columns: repeat(4, 1fr); gap: 8px; }
  .totals > div, .sum > div { background: #1a1c21; border: 1px solid #262930; border-radius: 10px; padding: 8px 10px; display: flex; flex-direction: column; gap: 2px; }
  .totals b, .sum b { font-size: 17px; font-variant-numeric: tabular-nums; } small { opacity: .6; font-size: 11.5px; }
  .budget { height: 26px; } .over { color: #ff6b6b; opacity: 1; } .ok { color: #30d158; opacity: 1; }
  .calc { flex: 1; font-variant-numeric: tabular-nums; } .calcOut { min-width: 90px; font-weight: 600; font-size: 15px; cursor: pointer; } .calcOut.bad { opacity: .4; }
  .stores { flex: 1; min-width: 200px; } .grow { flex: 0; }
  .ai { background: #15171b; border: 1px solid #2c2f36; border-radius: 12px; padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; }
  .aihead { display: flex; gap: 10px; align-items: center; } .aihead span { flex: 1; }
  .sum { display: grid; grid-template-columns: repeat(auto-fill, minmax(170px, 1fr)); gap: 6px; } .sum .mini { align-self: flex-start; margin-top: 2px; }
  .item { display: grid; grid-template-columns: 160px 1fr; gap: 8px; border-top: 1px solid #23262c; padding-top: 6px; }
  .iname { font-weight: 600; } .offers { display: flex; flex-direction: column; gap: 3px; }
  .offer { display: flex; gap: 8px; align-items: center; } .offer.best b { color: #30d158; } .offer b { min-width: 80px; font-variant-numeric: tabular-nums; }
  .warn { background: #3a2c10; border: 1px solid #6b4e17; color: #ffd27a; border-radius: 8px; padding: 6px 10px; font-size: 12.5px; }
  a { color: #5ac8fa; } .muted { opacity: .6; } .small { font-size: 11.5px; } .empty { padding: 14px 4px; }
  .toast { position: fixed; left: 50%; bottom: 60px; transform: translateX(-50%); background: #000d; padding: 8px 14px; border-radius: 14px; opacity: 0; transition: opacity .2s; pointer-events: none; }
  .toast.on { opacity: 1; }
  @media (max-width: 640px) { .row { grid-template-columns: 22px 44px 1fr 74px 28px; } .line, .store { display: none; } .totals { grid-template-columns: 1fr 1fr; } .item { grid-template-columns: 1fr; } }
  `;

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start); else start();
})();
