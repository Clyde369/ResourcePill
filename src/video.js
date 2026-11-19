// The YouTube window's page (src/video.rs injects it into every page it loads). Two places:
//  - embed: our own page (https://%HOST%/player.html) with YouTube's embedded player
//  - youtube.com itself: for browsing (and signing in), and for videos whose owner turned off
//    embedding. On a watch page the player is stretched over the window and everything else hidden.
// All get the same hover bar, live chat and resize edges. Talks to the pill with short strings:
//  page → pill: ready:embed|youtube, open:<text>, watch:<id>|<list>|<start>, chat:<n>, side:<0|1>,
//               alpha, ghost, hide, close, ext:<url>, size:<hit-test>
//  pill → page: {play:{id,list,start,channel}}, {state:{chat,alpha,ghost}}, {hover:0|1}, {err:"…"}
(() => {
  const HOST = '%HOST%';
  const yt = location.hostname === 'www.youtube.com';
  if (window.top !== window || !window.chrome || !window.chrome.webview || !(yt || location.hostname === HOST)) return;
  const post = s => window.chrome.webview.postMessage(s);
  let player = null, cur = null, chat = 1, live = false, apiReady = false, pending = null, hideTimer = 0, sideSent = -1, root, ghost = null, toastTimer = 0;
  let path = location.pathname;
  const takeover = () => yt && location.pathname === '/watch'; // a video on YouTube's page fills the window
  const $ = id => root.querySelector('#rp-' + id);

  const CSS = `
  #rp { position: fixed; inset: 0; z-index: 2147483600; pointer-events: none; color: #f2f2f2; font: 13px "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif; user-select: none; --side: 0px; }
  #rp * { box-sizing: border-box; }
  #rp > * { pointer-events: auto; }
  #rp-stage { position: absolute; left: 0; top: 0; bottom: 0; right: var(--side); background: #000; }
  #rp-stage iframe, #rp-player { position: absolute; inset: 0; width: 100%; height: 100%; border: 0; }
  #rp-chat { display: none; position: absolute; border: 0; background: #0f0f0f; }
  #rp.side #rp-chat { display: block; right: 0; top: 0; height: 100%; width: 300px; border-left: 1px solid #ffffff14; }
  #rp.over #rp-chat { display: block; right: 10px; top: 46px; height: calc(100% - 104px); width: min(36%, 340px); border-radius: 10px; opacity: .8; box-shadow: 0 4px 24px #0009; }
  #rp-bar { position: absolute; left: 0; right: var(--side); top: 0; height: 44px; padding: 0 8px; display: flex; align-items: center; gap: 4px; background: linear-gradient(#000d, #0000); opacity: 0; pointer-events: none; transition: opacity .18s; }
  #rp.ui #rp-bar, #rp.empty #rp-bar, #rp.err #rp-bar { opacity: 1; pointer-events: auto; }
  #rp-url { flex: 1; min-width: 60px; height: 28px; border-radius: 14px; border: 1px solid #ffffff22; background: #ffffff14; color: inherit; padding: 0 12px; font: inherit; outline: none; user-select: text; }
  #rp-url:focus { border-color: #5ac8fa; background: #000b; }
  #rp button { height: 28px; min-width: 28px; padding: 0 9px; border: 0; border-radius: 14px; background: #ffffff16; color: inherit; font: inherit; display: flex; align-items: center; justify-content: center; gap: 6px; cursor: pointer; white-space: nowrap; flex: none; }
  #rp button:hover { background: #ffffff2e; }
  #rp button.on { background: #5ac8fa38; color: #9be7ff; }
  #rp button[hidden] { display: none; }
  #rp .i { font-family: "Segoe Fluent Icons", "Segoe MDL2 Assets"; font-size: 12px; }
  #rp-live { display: none; align-items: center; gap: 5px; font-size: 11px; font-weight: 600; letter-spacing: .04em; color: #ff6b6b; padding: 0 4px; }
  #rp.live #rp-live { display: flex; }
  #rp-live::before { content: ""; width: 7px; height: 7px; border-radius: 4px; background: #ff453a; }
  #rp-msg { position: absolute; inset: 0; display: none; flex-direction: column; align-items: center; justify-content: center; gap: 8px; text-align: center; padding: 56px 24px 24px; background: radial-gradient(circle at 50% 40%, #1d1d24, #000 70%); }
  #rp.empty #rp-msg, #rp.err #rp-msg { display: flex; }
  #rp-msg b { font-size: 15px; font-weight: 600; }
  #rp-msg span { opacity: .62; max-width: 440px; line-height: 1.45; }
  #rp-msg button { margin-top: 6px; }
  #rp-toast { position: absolute; left: 50%; bottom: 18px; transform: translateX(-50%); max-width: 90%; padding: 8px 14px; border-radius: 16px; background: #000c; font-size: 12.5px; text-align: center; opacity: 0; transition: opacity .25s; pointer-events: none !important; }
  #rp-toast.on { opacity: 1; }
  #rp .e { position: absolute; }
  #rp-e10 { left: 0; top: 12px; bottom: 12px; width: 6px; cursor: ew-resize; }
  #rp-e11 { right: 0; top: 12px; bottom: 12px; width: 6px; cursor: ew-resize; }
  #rp-e12 { top: 0; left: 12px; right: 12px; height: 4px; cursor: ns-resize; }
  #rp-e15 { bottom: 0; left: 12px; right: 12px; height: 6px; cursor: ns-resize; }
  #rp-e13 { left: 0; top: 0; width: 12px; height: 12px; cursor: nwse-resize; }
  #rp-e14 { right: 0; top: 0; width: 12px; height: 12px; cursor: nesw-resize; }
  #rp-e16 { left: 0; bottom: 0; width: 12px; height: 12px; cursor: nesw-resize; }
  #rp-e17 { right: 0; bottom: 0; width: 12px; height: 12px; cursor: nwse-resize; }
  ` + (yt ? `
  html.rp-take, html.rp-take body { overflow: hidden !important; background: #000 !important; }
  html.rp-take #movie_player { position: fixed !important; left: 0 !important; top: 0 !important; width: calc(100vw - var(--rp-side, 0px)) !important; height: 100vh !important; z-index: 2147483500 !important; background: #000; }
  #rp-stage, #rp-msg { display: none !important; }
  html:not(.rp-take) #rp-chat { display: none !important; }
  html:not(.rp-take) #rp-bar { top: auto; bottom: 0; right: 0; background: linear-gradient(#0000, #000d); } /* YouTube's own top bar stays usable */
  ` : `html, body { margin: 0; height: 100%; overflow: hidden; background: #000; }`);

  const HTML = `
  <div id="rp-stage">
    <div id="rp-player"></div>
    <div id="rp-msg">
      <b id="rp-title">Watch YouTube under the pill</b>
      <span id="rp-text">Paste a video, live stream, Short or playlist link in the box above. Live streams can show their chat beside or over the video.</span>
      <button id="rp-btn" hidden></button>
    </div>
  </div>
  <iframe id="rp-chat" title="Live chat"></iframe>
  <div id="rp-toast"></div>
  <div id="rp-bar">
    <input id="rp-url" placeholder="Paste a YouTube link and press Enter" spellcheck="false">
    <div id="rp-live">LIVE</div>
    <button id="rp-browse" title="Browse YouTube: search, your subscriptions, sign in"><span class="i">&#xE80F;</span>Browse</button>
    <button id="rp-chatBtn" hidden title="Live chat: off, beside the video, or over it"><span class="i">&#xE8BD;</span><span id="rp-chatTxt">Chat</span></button>
    <button id="rp-alpha" title="See-through: click to cycle"><span class="i">&#xE706;</span><span id="rp-alphaTxt">100%</span></button>
    <button id="rp-ghost" title="Ghost: clicks go through to what's underneath and it fades while the mouse is over it. Ctrl+Alt+G turns it on and off from anywhere; or hold Ctrl over it to use it."><span class="i">&#xE890;</span>Ghost</button>
    <button id="rp-hide" title="Hide (Ctrl+Alt+Y brings it back; the sound keeps playing)"><span class="i">&#xE921;</span></button>
    <button id="rp-close" title="Close the video"><span class="i">&#xE8BB;</span></button>
  </div>
  ${[10, 11, 12, 15, 13, 14, 16, 17].map(n => `<div class="e" id="rp-e${n}"></div>`).join('')}`;

  function show(title, text, btn, action) {
    root.classList.remove('empty');
    root.classList.add('err');
    $('title').textContent = title;
    $('text').textContent = text;
    $('btn').hidden = !btn;
    $('btn').textContent = btn || '';
    $('btn').onclick = action || null;
  }

  function play(t) {
    cur = t;
    live = false;
    root.classList.remove('empty', 'err');
    refresh();
    if (!apiReady) { pending = t; return; }
    if (player) { try { player.destroy(); } catch (e) {} }
    const host = document.createElement('div');
    host.id = 'rp-player';
    $('stage').prepend(host);
    const vars = { autoplay: 1, playsinline: 1, rel: 0, origin: location.origin };
    if (t.start) vars.start = t.start;
    if (t.list) { vars.list = t.list; vars.listType = 'playlist'; }
    if (t.channel) vars.channel = t.channel;
    player = new YT.Player('rp-player', {
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
    if (yt) {
      if (location.pathname !== path) {
        path = location.pathname;
        layout();
      }
      player = document.getElementById('movie_player');
    }
    live = takeover() || !yt ? !!data().isLive : false;
    refresh();
  }

  function layout() {
    const take = takeover();
    if (document.documentElement.classList.contains('rp-take') !== take) {
      document.documentElement.classList.toggle('rp-take', take);
      setTimeout(() => window.dispatchEvent(new Event('resize')), 50); // the player fits itself to its new size
    }
    if (!root) return;
    // already browsing everywhere else on YouTube
    $('browse').hidden = yt && !take;
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

  // chat only exists for live streams; the pill widens the window while it sits beside the video
  function refresh() {
    const id = videoId();
    const mode = live && id ? chat : 0;
    root.classList.toggle('live', live);
    root.classList.toggle('side', mode == 1);
    root.classList.toggle('over', mode == 2);
    $('chatBtn').hidden = !live;
    $('chatBtn').classList.toggle('on', chat > 0);
    $('chatTxt').textContent = ['Chat off', 'Chat beside', 'Chat over'][chat];
    const src = mode ? 'https://www.youtube.com/live_chat?v=' + id + '&embed_domain=' + location.hostname + '&dark_theme=1' : 'about:blank';
    if ($('chat').dataset.src !== src) { $('chat').dataset.src = src; $('chat').src = src; }
    const side = mode == 1 ? 1 : 0;
    root.style.setProperty('--side', side ? '300px' : '0px');
    document.documentElement.style.setProperty('--rp-side', side ? '300px' : '0px');
    if (side !== sideSent) {
      sideSent = side;
      post('side:' + side);
      if (yt) setTimeout(() => window.dispatchEvent(new Event('resize')), 50); // the player fits itself to its new size
    }
  }

  function toast(text) {
    $('toast').textContent = text;
    $('toast').classList.add('on');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => $('toast').classList.remove('on'), 4000);
  }

  function showBar(on) {
    clearTimeout(hideTimer);
    if (on) {
      root.classList.add('ui');
      hideTimer = setTimeout(() => showBar(false), 2200);
    } else if (document.activeElement !== $('url')) {
      root.classList.remove('ui');
    }
  }

  function build() {
    if (document.getElementById('rp')) return;
    const style = document.createElement('style');
    style.textContent = CSS;
    (document.head || document.documentElement).append(style);
    root = document.createElement('div');
    root.id = 'rp';
    // YouTube's page only takes HTML through a Trusted Types policy (ours is the fixed markup above)
    const tt = window.trustedTypes && window.trustedTypes.createPolicy('resourcepill', { createHTML: s => s });
    root.innerHTML = tt ? tt.createHTML(HTML) : HTML;
    document.body.append(root);
    if (!yt) root.classList.add('empty');
    layout();

    window.chrome.webview.addEventListener('message', e => {
      const m = e.data;
      if (m.play && !yt) { $('url').blur(); play(m.play); }
      if (m.err) show("That isn't a YouTube link", m.err);
      if (m.state) {
        chat = m.state.chat;
        $('alphaTxt').textContent = m.state.alpha + '%';
        $('alpha').classList.toggle('on', m.state.alpha < 100);
        $('ghost').classList.toggle('on', m.state.ghost);
        if (m.state.ghost && ghost !== true) toast('Ghost on: clicks go through. Ctrl+Alt+G turns it off (or hold Ctrl over the video).');
        ghost = m.state.ghost;
        refresh();
      }
      if ('hover' in m) m.hover ? showBar(true) : setTimeout(() => showBar(false), 400);
    });

    const url = $('url');
    const open = () => { if (url.value.trim()) post('open:' + url.value.trim()); };
    url.addEventListener('keydown', e => {
      e.stopPropagation(); // not YouTube's keyboard shortcuts
      if (e.key === 'Enter') open();
      if (e.key === 'Escape') url.blur();
    });
    url.addEventListener('paste', () => setTimeout(open, 0));
    url.addEventListener('focus', () => url.select());
    url.addEventListener('blur', () => showBar(true));
    $('chatBtn').onclick = () => post('chat:' + (chat + 1) % 3);
    $('alpha').onclick = () => post('alpha');
    $('ghost').onclick = () => post('ghost');
    $('hide').onclick = () => post('hide');
    $('close').onclick = () => post('close');
    $('browse').onclick = () => { location.href = 'https://www.youtube.com/'; };
    root.querySelectorAll('.e').forEach(el => el.addEventListener('mousedown', e => {
      if (e.button === 0) { e.preventDefault(); post('size:' + el.id.slice(4)); }
    }));

    if (!yt) {
      window.onYouTubeIframeAPIReady = () => { apiReady = true; if (pending) { const t = pending; pending = null; play(t); } };
      const tag = document.createElement('script');
      tag.src = 'https://www.youtube.com/iframe_api';
      tag.onerror = () => show('Could not reach YouTube', 'Check your internet connection, then paste the link again.');
      document.head.append(tag);
    }
    setInterval(check, 1500); // live or not (and, on YouTube's page, the video it moved on to)
    if (yt) window.addEventListener('yt-navigate-finish', check); // YouTube changes pages without reloading
    post(yt ? 'ready:youtube' : 'ready:embed');
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', build);
  else build();
})();
