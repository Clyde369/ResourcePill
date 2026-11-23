// The window under the pill (src/video.rs injects this into every page it loads):
//  - ours (https://%HOST%/player.html): the start page, and YouTube / Twitch players with live chat
//  - www.youtube.com: a video page shows just its player, stretched over the window (videos whose
//    owner turned off embedding play here); the rest of YouTube is ordinary browsing
//  - any other site: ordinary browsing (sign in to anything; it stays signed in)
// Every page gets the same hover bar and resize edges, in a closed shadow DOM so a site's styles
// and security rules don't touch them. Talks to the pill with short strings:
//  page → pill: ready:embed|site, open:<text>, watch:<id>|<list>|<start>, chat:<n>, side:<0|1>,
//               alpha, ghost, hide, close, ext:<url>, size:<hit-test>
//  pill → page: {play:{kind:"yt",id,list,start,channel} | {kind:"tw",channel,video,clip}},
//               {state:{chat,alpha,ghost}}, {hover:0|1}, {err:"…"}
(() => {
  const HOST = '%HOST%';
  if (window.top !== window || !window.chrome || !window.chrome.webview) return;
  const ours = location.hostname === HOST && location.pathname === '/player.html'; // the player page (lists.html is lists.js)
  const yt = location.hostname === 'www.youtube.com';
  // every message carries this session's token (only this script has it), so a site can't drive the window itself
  const pm = window.chrome.webview.postMessage.bind(window.chrome.webview);
  // the token lives only in this closure: never written out in a function a page could call .toString() on
  const TOKEN = '%TOKEN%';
  const post = s => pm(TOKEN + '|' + s);
  // the browser's own versions, taken before any page script runs (a page can't swap them to reach into the bar)
  const apply = Reflect.apply;
  const attachShadow = Element.prototype.attachShadow;
  const listen = EventTarget.prototype.addEventListener;
  const isTrustedOf = Object.getOwnPropertyDescriptor(new Event('x'), 'isTrusted').get; // an own, unforgeable property of real events
  const trusted = e => { try { return apply(isTrustedOf, e, []) === true; } catch (_) { return false; } };
  // only real clicks and keys reach the pill: never a page script's .click() or a look-alike object.
  // Handlers go on through the saved addEventListener, so a page can't even see them
  const on = (el, type, fn) => apply(listen, el, [type, e => { if (trusted(e)) fn(e); }]);
  // what the tab strip is drawn with: the browser's own versions, so a site can't watch the titles go in
  const N = {
    create: Document.prototype.createElement, append: Node.prototype.appendChild, attr: Element.prototype.setAttribute,
    text: Object.getOwnPropertyDescriptor(Node.prototype, 'textContent').set, clear: Element.prototype.replaceChildren,
  };
  const isArray = Array.isArray, Str = String, Num = Number; // saved too: a site can't swap them to see the titles
  const el = (tag, cls, text) => {
    const e = apply(N.create, document, [tag]);
    if (cls) apply(N.attr, e, ['class', cls]);
    if (text != null) apply(N.text, e, [text]);
    return e;
  };
  // the tab strip arrives by a direct call from the pill (never as a page message, which every script
  // on the page could read); defined read-only now, before any site script runs
  let pendingTabs = null;
  Object.defineProperty(window, '%TABSFN%', {
    value: (token, data) => {
      if (token !== TOKEN || !data) return;
      if (isArray(data.tabs)) { if (root) renderTabs(data.tabs); else pendingTabs = data.tabs; }
      if (typeof data.toast === 'string' && root) toast(Str(data.toast)); // notes for you, not for the page
    },
    writable: false, configurable: false, enumerable: false,
  });
  let btnAction = null;
  let player = null, cur = null, chat = 1, live = false, apiReady = false, pending = null, hideTimer = 0, sideSent = -1, ghost = null, toastTimer = 0;
  let root, ui = {}, path = location.pathname;
  const takeover = () => yt && location.pathname === '/watch'; // a video on YouTube's page fills the window
  const playerPage = () => ours || takeover();

  const CSS = `
  :host { all: initial; }
  #rp { position: fixed; inset: 0; z-index: 2147483647; pointer-events: none; color: #f2f2f2; font: 13px "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif; user-select: none; --side: 0px; }
  #rp * { box-sizing: border-box; }
  #stage { position: absolute; left: 0; top: 0; bottom: 0; right: var(--side); background: #000; pointer-events: auto; }
  #stage iframe, #stage > div { position: absolute; inset: 0; width: 100%; height: 100%; border: 0; }
  #chat { display: none; position: absolute; border: 0; background: #0f0f0f; pointer-events: auto; }
  #rp.side #chat { display: block; right: 0; top: 0; height: 100%; width: 300px; border-left: 1px solid #ffffff14; }
  #rp.over #chat { display: block; right: 10px; top: 84px; height: calc(100% - 142px); width: min(36%, 340px); border-radius: 10px; opacity: .8; box-shadow: 0 4px 24px #0009; }
  #bar { position: absolute; left: 0; right: var(--side); top: 0; padding: 4px 8px 8px; display: flex; flex-direction: column; gap: 4px; background: linear-gradient(#000e, #000a 70%, #0000); opacity: 0; pointer-events: none; transition: opacity .18s; }
  #ctrl { display: flex; align-items: center; gap: 4px; height: 30px; }
  #tabrow { display: flex; align-items: center; gap: 4px; height: 28px; overflow-x: auto; scrollbar-width: none; }
  #tabrow:empty { display: none; }
  #tabrow .tab { max-width: 190px; min-width: 60px; height: 26px; padding: 0 4px 0 10px; gap: 6px; justify-content: space-between; background: #1f2126cc; }
  #tabrow .tab.on { background: #3a3f48; box-shadow: inset 0 -2px 0 #5ac8fa; }
  #tabrow .tab.asleep .t { opacity: .55; }
  #tabrow .t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }
  #tabrow .x { width: 18px; height: 18px; border-radius: 9px; display: flex; align-items: center; justify-content: center; font-size: 13px; opacity: .6; }
  #tabrow .x:hover { opacity: 1; background: #ffffff22; }
  #tabrow .plus { min-width: 26px; width: 26px; height: 26px; padding: 0; font-size: 16px; }
  #rp.browse #bar { top: auto; bottom: 0; right: 0; background: linear-gradient(#0000, #000d); } /* the site's own top bar stays usable */
  #rp.ui #bar, #rp.empty #bar, #rp.err #bar { opacity: 1; pointer-events: auto; }
  #url { flex: 1; min-width: 60px; height: 28px; border-radius: 14px; border: 1px solid #ffffff22; background: #1c1c1fcc; color: inherit; padding: 0 12px; font: inherit; outline: none; user-select: text; }
  #url:focus { border-color: #5ac8fa; background: #000d; }
  button { height: 28px; min-width: 28px; padding: 0 9px; border: 0; border-radius: 14px; background: #2a2a2ecc; color: inherit; font: inherit; display: flex; align-items: center; justify-content: center; gap: 6px; cursor: pointer; white-space: nowrap; flex: none; }
  button:hover { background: #3a3a40; }
  button.on { background: #5ac8fa38; color: #9be7ff; }
  [hidden] { display: none !important; }
  .i { font-family: "Segoe Fluent Icons", "Segoe MDL2 Assets"; font-size: 12px; }
  #live { display: none; align-items: center; gap: 5px; font-size: 11px; font-weight: 600; letter-spacing: .04em; color: #ff6b6b; padding: 0 4px; }
  #rp.live #live { display: flex; }
  #live::before { content: ""; width: 7px; height: 7px; border-radius: 4px; background: #ff453a; }
  #msg { position: absolute; inset: 0; display: none; flex-direction: column; align-items: center; justify-content: center; gap: 8px; text-align: center; padding: 56px 24px 24px; background: radial-gradient(circle at 50% 40%, #1d1d24, #000 70%); pointer-events: auto; }
  #rp.empty #msg, #rp.err #msg { display: flex; }
  #msg b { font-size: 15px; font-weight: 600; }
  #msg span { opacity: .62; max-width: 460px; line-height: 1.45; }
  #msg button { margin-top: 6px; }
  #tiles { display: flex; gap: 10px; margin-top: 10px; }
  #rp.err #tiles { display: none; }
  #tiles button { height: 38px; padding: 0 16px; border-radius: 19px; font-weight: 600; }
  #crop { position: absolute; inset: 0; cursor: crosshair; background: #0004; pointer-events: auto; outline: none; }
  #crop .box { position: absolute; border: 2px solid #5ac8fa; background: #5ac8fa22; border-radius: 4px; }
  #crop .hint { position: absolute; left: 50%; top: 14px; transform: translateX(-50%); background: #000d; padding: 8px 14px; border-radius: 16px; font-size: 12.5px; white-space: nowrap; }
  #toast { position: absolute; left: 50%; bottom: 18px; transform: translateX(-50%); max-width: 90%; padding: 8px 14px; border-radius: 16px; background: #000c; font-size: 12.5px; text-align: center; opacity: 0; transition: opacity .25s; }
  #rp.browse #toast { bottom: 56px; }
  #toast.on { opacity: 1; }
  .e { position: absolute; pointer-events: auto; }
  #e10 { left: 0; top: 12px; bottom: 12px; width: 6px; cursor: ew-resize; }
  #e11 { right: 0; top: 12px; bottom: 12px; width: 6px; cursor: ew-resize; }
  #e12 { top: 0; left: 12px; right: 12px; height: 4px; cursor: ns-resize; }
  #e15 { bottom: 0; left: 12px; right: 12px; height: 6px; cursor: ns-resize; }
  #e13 { left: 0; top: 0; width: 12px; height: 12px; cursor: nwse-resize; }
  #e14 { right: 0; top: 0; width: 12px; height: 12px; cursor: nesw-resize; }
  #e16 { left: 0; bottom: 0; width: 12px; height: 12px; cursor: nesw-resize; }
  #e17 { right: 0; bottom: 0; width: 12px; height: 12px; cursor: nwse-resize; }
  `;
  // on YouTube's own video page: its player over the whole window, everything else hidden
  const PAGE_CSS = yt ? `
  html.rp-take, html.rp-take body { overflow: hidden !important; background: #000 !important; }
  html.rp-take #movie_player { position: fixed !important; left: 0 !important; top: 0 !important; width: calc(100vw - var(--rp-side, 0px)) !important; height: 100vh !important; z-index: 2147483500 !important; background: #000; }
  ` : ours ? `html, body { margin: 0; height: 100%; overflow: hidden; background: #000; }` : '';

  // tiny DOM builder: h('button', {id: 'x', title: '…'}, 'text', child…) — no HTML strings, so no
  // site's Trusted Types or CSP gets in the way
  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (k === 'icon') el.append(h('span', { class: 'i' }, v));
      else if (v !== false) el.setAttribute(k, v === true ? '' : v);
    }
    for (const k of kids) if (k != null) el.append(k);
    if (attrs && attrs.id) ui[attrs.id] = el;
    return el;
  }
  const sheet = css => { const s = new CSSStyleSheet(); s.replaceSync(css); return s; }; // CSP doesn't apply to these

  function show(title, text, btn, action) {
    root.classList.remove('empty');
    root.classList.add('err');
    ui.title.textContent = title;
    ui.text.textContent = text;
    ui.btn.hidden = !btn;
    ui.btn.textContent = btn || '';
    btnAction = action || null;
  }

  function clearStage() {
    if (player && player.destroy) { try { player.destroy(); } catch (e) {} }
    player = null;
    [...ui.stage.children].forEach(c => c !== ui.msg && c.remove());
  }

  function play(t) {
    cur = t;
    live = false;
    root.classList.remove('empty', 'err');
    document.title = t.kind === 'tw' ? 'Twitch · ' + (t.channel || 'video') : 'YouTube'; // the tab's name until the video's own title is known
    if (t.kind === 'tw') {
      clearStage();
      const src = t.clip ? 'https://clips.twitch.tv/embed?clip=' + t.clip : 'https://player.twitch.tv/?' + (t.video ? 'video=v' + t.video : 'channel=' + t.channel);
      ui.stage.prepend(h('iframe', { src: src + '&parent=' + HOST + '&autoplay=true&muted=false', allow: 'autoplay; fullscreen; encrypted-media', allowfullscreen: true }));
      live = !!t.channel; // a channel is live (or about to be): its chat is worth showing
      return refresh();
    }
    refresh();
    if (!apiReady) { pending = t; return; }
    clearStage();
    const host = h('div', { id: 'ytplayer' });
    ui.stage.prepend(host);
    const vars = { autoplay: 1, playsinline: 1, rel: 0, origin: location.origin };
    if (t.start) vars.start = t.start;
    if (t.list) { vars.list = t.list; vars.listType = 'playlist'; }
    if (t.channel) vars.channel = t.channel;
    player = new YT.Player(host, {
      host: 'https://www.youtube.com',
      videoId: t.channel ? 'live_stream' : (t.id || undefined),
      playerVars: vars,
      events: { onReady: check, onStateChange: check, onError: e => onError(e.data) },
    });
  }

  function data() {
    try { return (player && player.getVideoData && player.getVideoData()) || {}; } catch (e) { return {}; }
  }
  const videoId = () => data().video_id || (cur && cur.id) || '';

  function check() {
    if (location.pathname !== path) {
      path = location.pathname;
      layout();
    }
    if (yt) player = document.getElementById('movie_player');
    if (!ui.url.matches(':focus') && !ours) ui.url.value = location.href;
    if (!(cur && cur.kind === 'tw')) live = playerPage() ? !!data().isLive : false;
    if (ours && data().title && document.title !== data().title) document.title = data().title; // name the tab after the video
    refresh();
  }

  function layout() {
    const take = takeover();
    if (document.documentElement.classList.contains('rp-take') !== take) {
      document.documentElement.classList.toggle('rp-take', take);
      setTimeout(() => window.dispatchEvent(new Event('resize')), 50); // the player fits itself to its new size
    }
    if (!root) return;
    root.classList.toggle('browse', !playerPage());
    ui.back.hidden = ours;
    ui.addItem.hidden = location.hostname === HOST; // our own pages have nothing to add
  }

  function onError(code) {
    const id = videoId();
    // the owner allows YouTube only: play it on YouTube's own page, in this window
    if ((code == 101 || code == 150 || code == 153) && id) {
      let t = 0;
      try { t = Math.floor(player.getCurrentTime()) || (cur && cur.start) || 0; } catch (e) {}
      post('watch:' + id + '|' + (cur && cur.list || '') + '|' + t);
      return;
    }
    const open = id ? () => post('ext:https://www.youtube.com/watch?v=' + id) : null;
    if (code == 100) show('Video not found', 'It may be private or removed.', id && 'Open on YouTube', open);
    else show('YouTube could not play this', 'Error ' + code + '. Try another link.', id && 'Open on YouTube', open);
  }

  // live chat (YouTube live streams, Twitch channels); the pill widens the window while it sits beside the video
  function refresh() {
    const tw = cur && cur.kind === 'tw' && ours;
    const id = tw ? cur.channel : videoId();
    const mode = live && id && playerPage() ? chat : 0;
    root.classList.toggle('live', live && playerPage());
    root.classList.toggle('side', mode == 1);
    root.classList.toggle('over', mode == 2);
    ui.chatBtn.hidden = !(live && playerPage());
    ui.chatBtn.classList.toggle('on', chat > 0);
    ui.chatTxt.textContent = ['Chat off', 'Chat beside', 'Chat over'][chat];
    const src = !mode ? 'about:blank'
      : tw ? 'https://www.twitch.tv/embed/' + id + '/chat?parent=' + HOST + '&darkpopout'
      : 'https://www.youtube.com/live_chat?v=' + id + '&embed_domain=' + location.hostname + '&dark_theme=1';
    if (ui.chat.dataset.src !== src) { ui.chat.dataset.src = src; ui.chat.src = src; }
    const side = mode == 1 ? 1 : 0;
    root.style.setProperty('--side', side ? '300px' : '0px');
    document.documentElement.style.setProperty('--rp-side', side ? '300px' : '0px');
    if (side !== sideSent) {
      sideSent = side;
      post('side:' + side);
      if (yt) setTimeout(() => window.dispatchEvent(new Event('resize')), 50);
    }
  }

  // the tab strip (from the pill): title, a speaker while it plays sound, dim while asleep (unloaded)
  let activeTab = -1;
  function renderTabs(tabs) {
    const row = ui.tabrow;
    apply(N.clear, row, []);
    // a counted loop, not for...of: a site can swap the array iterator, not plain indexing of our own data
    for (let i = 0, n = tabs.length; i < n; i++) {
      const t = tabs[i];
      if (t.active) activeTab = Num(t.id);
      const title = Str(t.title || 'Tab');
      const tab = el('button', 'tab' + (t.active ? ' on' : '') + (t.asleep ? ' asleep' : ''));
      apply(N.attr, tab, ['title', title + (t.asleep ? ' (asleep: loads when you open it)' : '')]);
      if (t.audio) apply(N.append, tab, [el('span', 'i', '')]);
      apply(N.append, tab, [el('span', 't', title)]);
      const close = el('span', 'x', '×');
      apply(N.attr, close, ['title', 'Close tab (Ctrl+W)']);
      apply(N.append, tab, [close]);
      const id = Num(t.id);
      on(tab, 'click', e => post(e.target === close ? 'tab-close:' + id : 'tab:' + id));
      on(tab, 'auxclick', e => { if (e.button === 1) post('tab-close:' + id); }); // middle-click closes, like a browser
      apply(N.append, row, [tab]);
    }
    const plus = el('button', 'plus', '+');
    apply(N.attr, plus, ['title', 'New tab (Ctrl+T)']);
    on(plus, 'click', () => post('tab-new'));
    apply(N.append, row, [plus]);
  }

  // ---------- Add to list ----------
  // The page's own product data (most shops publish it: schema.org JSON-LD, or product meta tags):
  // exact name and price, no AI needed.
  function pageProduct() {
    const out = { name: '', price: null, currency: '' };
    const visit = o => {
      if (!o || typeof o !== 'object' || out.price != null) return;
      if (isArray(o)) { for (let i = 0; i < o.length; i++) visit(o[i]); return; }
      const type = [].concat(o['@type'] || []).join(' ');
      if (/Product/i.test(type)) {
        const offers = [].concat(o.offers || []);
        for (let i = 0; i < offers.length; i++) {
          const x = offers[i] || {};
          const p = x.price ?? x.lowPrice ?? (x.priceSpecification && x.priceSpecification.price);
          if (p != null) { out.name = Str(o.name || ''); out.price = p; out.currency = Str(x.priceCurrency || ''); return; }
        }
        if (!out.name) out.name = Str(o.name || '');
      }
      visit(o['@graph']);
    };
    document.querySelectorAll('script[type="application/ld+json"]').forEach(s => { try { visit(JSON.parse(s.textContent)); } catch (e) {} });
    const meta = n => { const m = document.querySelector(`meta[property="${n}"],meta[name="${n}"],meta[itemprop="${n}"]`); return m && m.getAttribute('content'); };
    if (out.price == null) {
      const p = meta('product:price:amount') || meta('og:price:amount');
      if (p) { out.price = p; out.currency = meta('product:price:currency') || meta('og:price:currency') || ''; out.name = out.name || meta('og:title') || ''; }
    }
    return out;
  }

  // The words inside a dragged box: every bit of text (and image description) it covers.
  function cropText(r) {
    const parts = [];
    let len = 0;
    const hit = b => b.width > 0 && b.right > r.left && b.left < r.right && b.bottom > r.top && b.top < r.bottom;
    document.querySelectorAll('img[alt]').forEach(img => { const a = img.alt.trim(); if (a && hit(img.getBoundingClientRect())) parts.push(a); });
    const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const range = document.createRange();
    for (let n = walk.nextNode(); n && len < 4000; n = walk.nextNode()) {
      const t = n.nodeValue.trim();
      if (!t) continue;
      range.selectNodeContents(n);
      if (hit(range.getBoundingClientRect())) { parts.push(t); len += t.length; }
    }
    return parts.join('\n').slice(0, 4000);
  }

  function sendAdd(text, whole) {
    post('list-add:' + JSON.stringify({ url: location.href, title: document.title, text, product: whole ? pageProduct() : {} }));
  }

  // Add: highlighted text goes straight in; otherwise drag a box around the item (Enter: this page's
  // product, Esc: never mind)
  function startAdd() {
    const sel = Str(window.getSelection() || '').trim();
    if (sel) return sendAdd(sel.slice(0, 4000), false);
    const crop = h('div', { id: 'crop', tabindex: '-1' }, h('div', { class: 'hint' }, 'Drag a box around the item  ·  Enter: use this page  ·  Esc: cancel'));
    root.append(crop);
    crop.focus();
    let start = null, box = null;
    const rect = e => ({ left: Math.min(start.x, e.clientX), top: Math.min(start.y, e.clientY), right: Math.max(start.x, e.clientX), bottom: Math.max(start.y, e.clientY) });
    on(crop, 'mousedown', e => { e.preventDefault(); start = { x: e.clientX, y: e.clientY }; box = h('div', { class: 'box' }); crop.append(box); });
    on(crop, 'mousemove', e => {
      if (!start) return;
      const r = rect(e);
      box.style.cssText = `left:${r.left}px;top:${r.top}px;width:${r.right - r.left}px;height:${r.bottom - r.top}px`;
    });
    on(crop, 'mouseup', e => {
      if (!start) return;
      const r = rect(e);
      crop.remove();
      if (r.right - r.left < 8 || r.bottom - r.top < 8) return sendAdd('', true); // a click, not a box: the page's product
      const text = cropText(r);
      sendAdd(text, !text);
    });
    on(crop, 'keydown', e => {
      if (e.key === 'Escape') crop.remove();
      if (e.key === 'Enter') { crop.remove(); sendAdd('', true); }
    });
  }

  // drawn with the saved browser functions: a site can't swap textContent to read your notes
  function toast(text) {
    apply(N.text, ui.toast, [Str(text)]);
    apply(N.attr, ui.toast, ['class', 'on']);
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => apply(N.attr, ui.toast, ['class', '']), 4000);
  }

  function showBar(on) {
    clearTimeout(hideTimer);
    if (on) {
      root.classList.add('ui');
      hideTimer = setTimeout(() => showBar(false), 2200);
    } else if (!ui.url.matches(':focus')) {
      root.classList.remove('ui');
    }
  }

  function build() {
    if (root) return;
    const host = document.createElement('resourcepill-ui');
    const shadow = apply(attachShadow, host, [{ mode: 'closed' }]);
    shadow.adoptedStyleSheets = [sheet(CSS)];
    if (PAGE_CSS) document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet(PAGE_CSS)];

    const tile = (label, link) => h('button', { title: link }, label);
    root = h('div', { id: 'rp' },
      ours ? h('div', { id: 'stage' },
        h('div', { id: 'msg' },
          h('b', { id: 'title' }, 'Watch and browse under the pill'),
          h('span', { id: 'text' }, 'Paste a YouTube or Twitch link to watch it here, with live chat beside or over it. Or type any address or search: sign in to sites and they stay signed in.'),
          h('div', { id: 'tiles' }, tile('YouTube', 'youtube.com'), tile('Twitch', 'twitch.tv'), tile('Google', 'google.com'), tile('Shopping lists', '/lists.html')),
          h('button', { id: 'btn', hidden: true }))) : null,
      h('iframe', { id: 'chat', title: 'Live chat', allow: 'clipboard-write' }),
      h('div', { id: 'toast' }),
      h('div', { id: 'bar' },
        h('div', { id: 'tabrow' }),
        h('div', { id: 'ctrl' },
        h('button', { id: 'back', title: 'Back', icon: '' }),
        h('button', { id: 'fwd', title: 'Forward', icon: '' }),
        h('button', { id: 'reload', title: 'Reload (F5)', icon: '' }),
        h('input', { id: 'url', placeholder: 'Paste a link, type an address or search', spellcheck: 'false' }),
        h('div', { id: 'live' }, 'LIVE'),
        h('button', { id: 'chatBtn', hidden: true, title: 'Live chat: off, beside the video, or over it', icon: '' }, h('span', { id: 'chatTxt' }, 'Chat')),
        h('button', { id: 'addItem', title: 'Add an item to your shopping list: drag a box around it (or highlight it first)', icon: '' }, 'Add'),
        h('button', { id: 'listBtn', title: 'Your shopping list beside the page', icon: '' }, 'List'),
        h('button', { id: 'alpha', title: 'See-through: click to cycle', icon: '' }, h('span', { id: 'alphaTxt' }, '100%')),
        h('button', { id: 'ghost', title: "Ghost: clicks go through to what's underneath and it fades while the mouse is over it. Ctrl+Alt+G turns it on and off from anywhere; or hold Ctrl over it to use it.", icon: '' }, 'Ghost'),
        h('button', { id: 'hide', title: 'Hide (Ctrl+Alt+Y brings it back; the sound keeps playing)', icon: '' }),
        h('button', { id: 'close', title: 'Close', icon: '' }))),
      ...[10, 11, 12, 15, 13, 14, 16, 17].map(n => h('div', { class: 'e', id: 'e' + n })));
    shadow.append(root);
    document.documentElement.append(host); // outside <body>: some sites replace it
    if (ours) root.classList.add('empty');
    if (pendingTabs) { renderTabs(pendingTabs); pendingTabs = null; }
    layout();

    window.chrome.webview.addEventListener('message', e => {
      const m = e.data;
      if (m.play && ours) { ui.url.blur(); play(m.play); }
      if (m.err) show("Couldn't open that", m.err);

      if (m.state) {
        sideSent = -1; // say again whether chat sits beside the video (the window may have changed tab)
        chat = m.state.chat;
        ui.alphaTxt.textContent = m.state.alpha + '%';
        ui.alpha.classList.toggle('on', m.state.alpha < 100);
        ui.ghost.classList.toggle('on', m.state.ghost);
        if (m.state.ghost && ghost !== true) toast('Ghost on: clicks go through. Ctrl+Alt+G turns it off (or hold Ctrl over the window).');
        ghost = m.state.ghost;
        refresh();
      }
      if ('hover' in m) m.hover ? showBar(true) : setTimeout(() => showBar(false), 400);
    });

    const url = ui.url;
    const open = () => { if (url.value.trim()) post('open:' + url.value.trim()); };
    on(url, 'keydown', e => {
      e.stopPropagation(); // not the site's keyboard shortcuts
      if (e.key === 'Enter') open();
      if (e.key === 'Escape') url.blur();
    });
    url.addEventListener('keyup', e => e.stopPropagation());
    on(url, 'paste', () => setTimeout(() => { if (ours) open(); }, 0)); // on the start page a pasted link opens at once
    url.addEventListener('focus', () => url.select());
    url.addEventListener('blur', () => showBar(true));
    if (ours) root.querySelectorAll('#tiles button').forEach(b => on(b, 'click', () => b.title.startsWith('/') ? (location.href = b.title) : post('open:' + b.title)));
    on(ui.btn, 'click', () => btnAction && btnAction());
    on(ui.back, 'click', () => history.back());
    on(ui.fwd, 'click', () => history.forward());
    on(ui.reload, 'click', () => location.reload());
    on(ui.listBtn, 'click', () => post('side-list'));
    on(ui.addItem, 'click', startAdd);
    // browser keys: Ctrl+T new tab, Ctrl+W close it, Ctrl+(Shift+)Tab next/previous, Ctrl+L the address bar
    apply(listen, window, ['keydown', e => {
      if (!trusted(e) || !e.ctrlKey || e.altKey) return;
      const k = e.key.toLowerCase();
      const act = k === 't' ? 'tab-new' : k === 'w' ? 'tab-close:' + activeTab : k === 'tab' ? (e.shiftKey ? 'tab-prev' : 'tab-next') : null;
      if (act) { e.preventDefault(); e.stopPropagation(); post(act); }
      if (k === 'l') { e.preventDefault(); showBar(true); ui.url.focus(); }
    }, true]);
    on(ui.chatBtn, 'click', () => post('chat:' + (chat + 1) % 3));
    on(ui.alpha, 'click', () => post('alpha'));
    on(ui.ghost, 'click', () => post('ghost'));
    on(ui.hide, 'click', () => post('hide'));
    on(ui.close, 'click', () => post('close'));
    root.querySelectorAll('.e').forEach(el => on(el, 'mousedown', e => {
      if (e.button === 0) { e.preventDefault(); post('size:' + el.id.slice(1)); }
    }));

    if (ours) {
      window.onYouTubeIframeAPIReady = () => { apiReady = true; if (pending) { const t = pending; pending = null; play(t); } };
      const tag = document.createElement('script');
      tag.src = 'https://www.youtube.com/iframe_api';
      document.head.append(tag);
    }
    setInterval(check, 1500); // live or not, and where a site moved on to without reloading
    if (yt) window.addEventListener('yt-navigate-finish', check);
    check();
    post(ours ? 'ready:embed' : 'ready:site');
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', build);
  else build();
})();
