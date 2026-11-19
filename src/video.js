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
  const ours = location.hostname === HOST;
  const yt = location.hostname === 'www.youtube.com';
  // every message carries this session's token (only this script has it), so a site can't drive the window itself
  const pm = window.chrome.webview.postMessage.bind(window.chrome.webview);
  const post = s => pm('%TOKEN%|' + s);
  // the browser's own versions, taken before any page script runs (a page can't swap them to reach into the bar)
  const apply = Reflect.apply;
  const attachShadow = Element.prototype.attachShadow;
  const listen = EventTarget.prototype.addEventListener;
  const isTrustedOf = Object.getOwnPropertyDescriptor(new Event('x'), 'isTrusted').get; // an own, unforgeable property of real events
  const trusted = e => { try { return apply(isTrustedOf, e, []) === true; } catch (_) { return false; } };
  // only real clicks and keys reach the pill: never a page script's .click() or a look-alike object.
  // Handlers go on through the saved addEventListener, so a page can't even see them
  const on = (el, type, fn) => apply(listen, el, [type, e => { if (trusted(e)) fn(e); }]);
  let btnAction = null;
  let player = null, cur = null, chat = 1, live = false, apiReady = false, pending = null, hideTimer = 0, sideSent = -1, ghost = null, toastTimer = 0;
  let root, ui = {}, path = location.pathname;
  const takeover = () => yt && location.pathname === '/watch'; // a video on YouTube's page fills the window
  const playerPage = () => ours || takeover();

  const CSS = `
  :host { all: initial; }
  #rp { position: fixed; inset: 0; z-index: 2147483600; pointer-events: none; color: #f2f2f2; font: 13px "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif; user-select: none; --side: 0px; }
  #rp * { box-sizing: border-box; }
  #stage { position: absolute; left: 0; top: 0; bottom: 0; right: var(--side); background: #000; pointer-events: auto; }
  #stage iframe, #stage > div { position: absolute; inset: 0; width: 100%; height: 100%; border: 0; }
  #chat { display: none; position: absolute; border: 0; background: #0f0f0f; pointer-events: auto; }
  #rp.side #chat { display: block; right: 0; top: 0; height: 100%; width: 300px; border-left: 1px solid #ffffff14; }
  #rp.over #chat { display: block; right: 10px; top: 46px; height: calc(100% - 104px); width: min(36%, 340px); border-radius: 10px; opacity: .8; box-shadow: 0 4px 24px #0009; }
  #bar { position: absolute; left: 0; right: var(--side); top: 0; height: 44px; padding: 0 8px; display: flex; align-items: center; gap: 4px; background: linear-gradient(#000d, #0000); opacity: 0; pointer-events: none; transition: opacity .18s; }
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

  function toast(text) {
    ui.toast.textContent = text;
    ui.toast.classList.add('on');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => ui.toast.classList.remove('on'), 4000);
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
          h('div', { id: 'tiles' }, tile('YouTube', 'youtube.com'), tile('Twitch', 'twitch.tv'), tile('Google', 'google.com')),
          h('button', { id: 'btn', hidden: true }))) : null,
      h('iframe', { id: 'chat', title: 'Live chat', allow: 'clipboard-write' }),
      h('div', { id: 'toast' }),
      h('div', { id: 'bar' },
        h('button', { id: 'back', title: 'Back', icon: '' }),
        h('input', { id: 'url', placeholder: 'Paste a link, type an address or search', spellcheck: 'false' }),
        h('div', { id: 'live' }, 'LIVE'),
        h('button', { id: 'chatBtn', hidden: true, title: 'Live chat: off, beside the video, or over it', icon: '' }, h('span', { id: 'chatTxt' }, 'Chat')),
        h('button', { id: 'alpha', title: 'See-through: click to cycle', icon: '' }, h('span', { id: 'alphaTxt' }, '100%')),
        h('button', { id: 'ghost', title: "Ghost: clicks go through to what's underneath and it fades while the mouse is over it. Ctrl+Alt+G turns it on and off from anywhere; or hold Ctrl over it to use it.", icon: '' }, 'Ghost'),
        h('button', { id: 'hide', title: 'Hide (Ctrl+Alt+Y brings it back; the sound keeps playing)', icon: '' }),
        h('button', { id: 'close', title: 'Close', icon: '' })),
      ...[10, 11, 12, 15, 13, 14, 16, 17].map(n => h('div', { class: 'e', id: 'e' + n })));
    shadow.append(root);
    document.documentElement.append(host); // outside <body>: some sites replace it
    if (ours) root.classList.add('empty');
    layout();

    window.chrome.webview.addEventListener('message', e => {
      const m = e.data;
      if (m.play && ours) { ui.url.blur(); play(m.play); }
      if (m.err) show("Couldn't open that", m.err);
      if (m.state) {
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
    if (ours) root.querySelectorAll('#tiles button').forEach(b => on(b, 'click', () => post('open:' + b.title)));
    on(ui.btn, 'click', () => btnAction && btnAction());
    on(ui.back, 'click', () => history.back());
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
