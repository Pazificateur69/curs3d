/* =========================================================
   CURS3D — script
   - Animated chain canvas
   - Live testnet stats (curs3d.fr API + graceful fallback)
   - EN/FR toggle
   - Code copy buttons
   - Scroll reveals
   - Tweaks panel
   ========================================================= */

(() => {
  'use strict';

  /* ---------- 1. Language toggle ---------- */
  const LANG_KEY = 'curs3d.lang';
  function setLang(lang) {
    document.documentElement.setAttribute('lang', lang);
    document.querySelectorAll('[data-lang]').forEach(el => {
      el.classList.toggle('lang-show', el.dataset.lang === lang);
    });
    document.querySelectorAll('.lang-toggle button').forEach(btn => {
      btn.classList.toggle('active', btn.dataset.langSet === lang);
    });
    try { localStorage.setItem(LANG_KEY, lang); } catch (e) {}
  }
  document.addEventListener('click', (e) => {
    const t = e.target.closest('[data-lang-set]');
    if (t) setLang(t.dataset.langSet);
  });
  const initialLang = (() => {
    try { return localStorage.getItem(LANG_KEY) || 'en'; } catch (e) { return 'en'; }
  })();
  setLang(initialLang);

  /* ---------- 1b. Mobile navigation ---------- */
  const header = document.querySelector('.nav');
  const burger = document.querySelector('.nav-burger');
  const primaryNav = document.querySelector('.nav-links');
  if (header && burger && primaryNav) {
    const mobileNav = document.createElement('nav');
    mobileNav.className = 'mobile-nav';
    mobileNav.setAttribute('aria-label', 'mobile');
    mobileNav.innerHTML = primaryNav.innerHTML;
    header.appendChild(mobileNav);
    burger.setAttribute('aria-expanded', 'false');

    const setMobileNav = (open) => {
      header.classList.toggle('mobile-open', open);
      burger.setAttribute('aria-expanded', String(open));
    };

    burger.addEventListener('click', () => {
      setMobileNav(!header.classList.contains('mobile-open'));
    });
    mobileNav.addEventListener('click', (e) => {
      if (e.target.closest('a')) setMobileNav(false);
    });
    window.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') setMobileNav(false);
    });
    window.addEventListener('resize', () => {
      if (window.innerWidth > 880) setMobileNav(false);
    }, { passive: true });
  }

  /* ---------- 2. Code copy ---------- */
  document.querySelectorAll('.code-copy').forEach(btn => {
    btn.addEventListener('click', async () => {
      const code = btn.closest('.code-block').querySelector('.code-body');
      const text = code.innerText;
      try {
        await navigator.clipboard.writeText(text);
      } catch (e) {
        const ta = document.createElement('textarea');
        ta.value = text; document.body.appendChild(ta); ta.select();
        try { document.execCommand('copy'); } catch (_) {}
        document.body.removeChild(ta);
      }
      const orig = btn.textContent;
      btn.classList.add('done');
      btn.textContent = btn.dataset.done || 'COPIED';
      setTimeout(() => {
        btn.classList.remove('done');
        btn.textContent = orig;
      }, 1400);
    });
  });

  /* ---------- 3. Scroll reveal ---------- */
  const io = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (e.isIntersecting) {
        e.target.classList.add('in');
        io.unobserve(e.target);
      }
    }
  }, { threshold: 0.12, rootMargin: '0px 0px -40px 0px' });
  document.querySelectorAll('.reveal, [data-stagger]').forEach(el => io.observe(el));

  /* ---------- 4. Live stats ----------
     Tries the real API. If it answers, we paint LIVE numbers.
     If not, we DO NOT show fake numbers as if they were real:
     - statusbar / hero stats stay as "—"
     - <b data-live-source> flips to "DEMO" with a clear tooltip
     - any element marked .stat-num gets a .sim-mode class so CSS
       can grey them out and add a "(simulated)" cue
     The simulated counter is still shown for visual life — but it is
     unmistakably labelled and visibly different from real data.
  */
  const ENDPOINTS = [
    'https://api.curs3d.fr/api/status',
    'https://curs3d.fr/api/status',
  ];
  // Initial display values are dashes — never seed with fake data.
  const PLACEHOLDER = '—';
  // Internal state used only AFTER we either confirmed LIVE or fell back to DEMO.
  const stats = {
    block: null,
    epoch: null,
    validators: null,
    validatorsTotal: null,
    txTotal: null,
    tps: null,
    finality: null,
    chainId: 'curs3d-public-testnet',
    version: PLACEHOLDER,
  };
  let mode = 'init'; // 'init' | 'live' | 'demo'

  function fmt(n) {
    if (n === null || n === undefined) return PLACEHOLDER;
    return Number(n).toLocaleString('en-US');
  }
  function setStat(id, value) {
    document.querySelectorAll(`[data-stat="${id}"]`).forEach(el => {
      el.textContent = value;
    });
  }

  function applyModeClass() {
    // Toggle a body-level class so CSS can grey out simulated values.
    document.body.classList.toggle('stats-demo', mode === 'demo');
    document.body.classList.toggle('stats-live', mode === 'live');
    // Update every <b data-live-source> with the right label and tooltip.
    document.querySelectorAll('[data-live-source]').forEach(e => {
      if (mode === 'live') {
        e.textContent = 'LIVE';
        e.title = 'Live data from https://api.curs3d.fr/api/status';
        e.style.color = '';
      } else if (mode === 'demo') {
        e.textContent = 'DEMO';
        e.title = 'API unreachable — these numbers are simulated, not real chain state.';
        e.style.color = '#d4af37';
      } else {
        e.textContent = '—';
        e.title = 'Connecting to the chain…';
      }
    });
  }

  function paintStats() {
    setStat('block', fmt(stats.block));
    setStat('epoch', fmt(stats.epoch));
    if (stats.validators !== null) {
      const total = stats.validatorsTotal ?? stats.validators;
      setStat('validators', `${stats.validators}/${total}`);
    } else {
      setStat('validators', PLACEHOLDER);
    }
    setStat('tps', stats.tps !== null ? stats.tps.toFixed(1) : PLACEHOLDER);
    setStat('finality', stats.finality !== null ? stats.finality.toFixed(1) + 's' : PLACEHOLDER);
    setStat('txtotal', fmt(stats.txTotal));
    setStat('chainid', stats.chainId);
    setStat('version', stats.version);
  }
  // Initial paint = all dashes, mode = init.
  paintStats();
  applyModeClass();

  // Try real endpoints (silent on failure — CORS/offline are expected).
  (async () => {
    for (const url of ENDPOINTS) {
      try {
        const res = await fetch(url, { mode: 'cors', cache: 'no-store' });
        if (!res.ok) continue;
        const body = await res.json();
        // CURS3D wraps responses as {ok, data}. Be tolerant of either shape.
        const j = (body && body.data) ? body.data : body;
        if (typeof j.height === 'number') stats.block = j.height;
        else if (typeof j.block_height === 'number') stats.block = j.block_height;
        if (typeof j.epoch === 'number') stats.epoch = j.epoch;
        if (typeof j.validators_active === 'number') stats.validators = j.validators_active;
        else if (Array.isArray(j.validators)) stats.validators = j.validators.length;
        if (typeof j.validators_total === 'number') stats.validatorsTotal = j.validators_total;
        if (typeof j.tps === 'number') stats.tps = j.tps;
        if (typeof j.protocol_version !== 'undefined') stats.version = 'v' + j.protocol_version;
        if (typeof j.chain_id === 'string') stats.chainId = j.chain_id;
        mode = 'live';
        paintStats();
        applyModeClass();
        return;
      } catch (_) { /* swallow */ }
    }
    // API unreachable — switch to DEMO mode. Numbers will be simulated,
    // CSS will grey them out, and data-live-source will read "DEMO".
    mode = 'demo';
    stats.block = 184_726;
    stats.epoch = 412;
    stats.validators = 2;
    stats.validatorsTotal = 2;
    stats.txTotal = 2_184_902;
    stats.tps = 18.4;
    stats.finality = 1.2;
    stats.version = 'v4 (DEMO)';
    paintStats();
    applyModeClass();
    setInterval(() => {
      stats.block += 1;
      stats.txTotal += Math.floor(Math.random() * 8) + 1;
      stats.tps = +(16 + Math.random() * 6).toFixed(1);
      stats.finality = +(0.9 + Math.random() * 0.6).toFixed(1);
      if (stats.block % 256 === 0) stats.epoch += 1;
      paintStats();
    }, 1800);
  })();

  /* ---------- 5. Animated chain canvas ----------
     A premium hero animation:
     - Constellation of stars w/ proximity-linked threads (parallax)
     - Curved ribbon path (bezier wave) snaking through scene
     - Cubic blocks with isometric depth, drifting along the ribbon
     - Glowing data packets streaming along the path
     - Soft nebula glow + vignette
  */
  const canvas = document.getElementById('hero-canvas');
  if (canvas) {
    const ctx = canvas.getContext('2d');
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    let W = 0, H = 0;
    let stars = [];
    let blocks = [];
    let packets = [];
    let nebula = null;
    let t0 = performance.now();
    let raf = 0;
    let mouseX = 0.5, mouseY = 0.5;

    // ---------- ribbon path ----------
    // returns a point along a smooth horizontal bezier wave (param u in 0..1)
    function pathPoint(u, t) {
      const x = u * W;
      // two layered sine waves for organic motion
      const y = H * 0.55
        + Math.sin(u * Math.PI * 2.2 + t * 0.35) * H * 0.10
        + Math.sin(u * Math.PI * 4.1 - t * 0.6)  * H * 0.04;
      // tangent (approximate)
      const dy = Math.cos(u * Math.PI * 2.2 + t * 0.35) * H * 0.10 * Math.PI * 2.2 / W
               + Math.cos(u * Math.PI * 4.1 - t * 0.6) * H * 0.04 * Math.PI * 4.1 / W;
      return { x, y, ang: Math.atan(dy) };
    }

    function resize() {
      const r = canvas.getBoundingClientRect();
      W = r.width; H = r.height;
      canvas.width = W * dpr;
      canvas.height = H * dpr;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      seed();
    }

    function seed() {
      // stars / particles
      stars = [];
      const SN = Math.floor((W * H) / 12000);
      for (let i = 0; i < SN; i++) {
        const depth = Math.random();
        stars.push({
          x: Math.random() * W,
          y: Math.random() * H,
          z: depth, // 0 = far, 1 = near
          vx: (Math.random() - 0.5) * (0.04 + depth * 0.18),
          vy: (Math.random() - 0.5) * (0.04 + depth * 0.18),
          r: 0.4 + depth * 1.6,
          tw: Math.random() * Math.PI * 2,
        });
      }

      // blocks — each has a base position u along the path
      blocks = [];
      const BN = W < 700 ? 7 : 11;
      for (let i = 0; i < BN; i++) {
        blocks.push({
          u: i / (BN - 1), // 0..1 along path
          baseU: i / (BN - 1),
          phase: i * 0.7,
          size: W < 700 ? 16 : 22,
          hash: '0x' + Math.random().toString(16).slice(2, 10),
          birth: -i * 0.4,
        });
      }

      // data packets — flow along the ribbon
      packets = [];
      const PN = W < 700 ? 4 : 8;
      for (let i = 0; i < PN; i++) {
        packets.push({
          u: Math.random(),
          speed: 0.06 + Math.random() * 0.08,
          life: Math.random(),
          tone: Math.random() < 0.18 ? 'gold' : 'cyan',
        });
      }

      // pre-render nebula bg (offscreen) for cheap blur glow
      const off = document.createElement('canvas');
      off.width = Math.max(1, Math.floor(W));
      off.height = Math.max(1, Math.floor(H));
      const oc = off.getContext('2d');
      // vignette
      const vg = oc.createRadialGradient(W*0.5, H*0.55, 0, W*0.5, H*0.55, Math.max(W, H) * 0.7);
      vg.addColorStop(0, 'rgba(34,211,238,0.10)');
      vg.addColorStop(0.45, 'rgba(34,211,238,0.025)');
      vg.addColorStop(1, 'rgba(0,0,0,0)');
      oc.fillStyle = vg;
      oc.fillRect(0, 0, W, H);
      // gold blob upper-right
      const gg = oc.createRadialGradient(W*0.85, H*0.25, 0, W*0.85, H*0.25, W*0.45);
      gg.addColorStop(0, 'rgba(212,175,55,0.07)');
      gg.addColorStop(1, 'rgba(0,0,0,0)');
      oc.fillStyle = gg;
      oc.fillRect(0, 0, W, H);
      nebula = off;
    }

    // mouse parallax
    canvas.addEventListener('pointermove', (e) => {
      const r = canvas.getBoundingClientRect();
      mouseX = (e.clientX - r.left) / r.width;
      mouseY = (e.clientY - r.top) / r.height;
    });

    // ---- helpers ----
    function lerp(a, b, t) { return a + (b - a) * t; }

    function drawCubeBlock(cx, cy, s, alpha, isLatest, t, ang) {
      // isometric-ish 3D cube projection (shear)
      const dx = s * 0.32, dy = -s * 0.22; // depth offset
      ctx.save();
      ctx.translate(cx, cy);
      ctx.rotate(ang * 0.2);

      // soft ground shadow
      ctx.fillStyle = 'rgba(0,0,0,0.45)';
      ctx.beginPath();
      ctx.ellipse(dx*0.4, s*0.9, s*1.1, s*0.25, 0, 0, Math.PI*2);
      ctx.fill();

      // back face (top)
      const cyan = `rgba(34,211,238,${alpha})`;
      const cyanFill = `rgba(34,211,238,${alpha * 0.18})`;
      const cyanFillDk = `rgba(34,211,238,${alpha * 0.07})`;
      const accent = isLatest ? `rgba(212,175,55,${alpha})` : cyan;

      // top face
      ctx.beginPath();
      ctx.moveTo(-s, -s);
      ctx.lineTo(-s + dx, -s + dy);
      ctx.lineTo( s + dx, -s + dy);
      ctx.lineTo( s, -s);
      ctx.closePath();
      ctx.fillStyle = `rgba(34,211,238,${alpha * 0.28})`;
      ctx.fill();
      ctx.strokeStyle = cyan;
      ctx.lineWidth = 1;
      ctx.stroke();

      // right face
      ctx.beginPath();
      ctx.moveTo( s, -s);
      ctx.lineTo( s + dx, -s + dy);
      ctx.lineTo( s + dx,  s + dy);
      ctx.lineTo( s,  s);
      ctx.closePath();
      ctx.fillStyle = cyanFillDk;
      ctx.fill();
      ctx.strokeStyle = `rgba(34,211,238,${alpha * 0.5})`;
      ctx.stroke();

      // front face — main
      if (isLatest) {
        const pulse = 0.55 + 0.45 * Math.sin(t * 3);
        ctx.shadowBlur = 22 + pulse * 14;
        ctx.shadowColor = 'rgba(34,211,238,0.85)';
      }
      ctx.fillStyle = cyanFill;
      ctx.fillRect(-s, -s, s * 2, s * 2);
      ctx.strokeStyle = isLatest ? 'rgba(34,211,238,1)' : cyan;
      ctx.lineWidth = isLatest ? 1.4 : 1;
      ctx.strokeRect(-s, -s, s * 2, s * 2);
      ctx.shadowBlur = 0;

      // inner accent square
      ctx.strokeStyle = accent;
      ctx.lineWidth = 0.7;
      ctx.strokeRect(-s * 0.55, -s * 0.55, s * 1.1, s * 1.1);

      // tiny center glyph dot
      ctx.fillStyle = accent;
      ctx.fillRect(-1.5, -1.5, 3, 3);

      ctx.restore();
    }

    function draw() {
      const t = (performance.now() - t0) / 1000;

      // base clear w/ slight trail (motion blur feel)
      ctx.fillStyle = 'rgba(8,9,11,0.28)';
      ctx.fillRect(0, 0, W, H);

      // nebula
      if (nebula) ctx.drawImage(nebula, 0, 0, W, H);

      // parallax offset (mouse + slow drift)
      const px = (mouseX - 0.5) * 24;
      const py = (mouseY - 0.5) * 14;

      // ---- stars + constellation ----
      for (const s of stars) {
        s.x += s.vx; s.y += s.vy;
        if (s.x < -10) s.x = W + 10; else if (s.x > W + 10) s.x = -10;
        if (s.y < -10) s.y = H + 10; else if (s.y > H + 10) s.y = -10;
      }
      // links
      const linkDist = W < 700 ? 90 : 130;
      ctx.lineWidth = 0.6;
      for (let i = 0; i < stars.length; i++) {
        const a = stars[i];
        const ax = a.x + px * a.z, ay = a.y + py * a.z;
        for (let j = i + 1; j < stars.length; j++) {
          const b = stars[j];
          const bx = b.x + px * b.z, by = b.y + py * b.z;
          const dx = ax - bx, dy = ay - by;
          const d2 = dx*dx + dy*dy;
          if (d2 < linkDist * linkDist) {
            const a01 = 1 - Math.sqrt(d2) / linkDist;
            ctx.strokeStyle = `rgba(34,211,238,${a01 * 0.10 * Math.min(a.z + b.z, 1)})`;
            ctx.beginPath();
            ctx.moveTo(ax, ay); ctx.lineTo(bx, by);
            ctx.stroke();
          }
        }
      }
      // dots
      for (const s of stars) {
        const tw = 0.6 + 0.4 * Math.sin(t * 2 + s.tw);
        ctx.fillStyle = `rgba(232,228,216,${0.18 * tw + s.z * 0.25})`;
        ctx.beginPath();
        ctx.arc(s.x + px * s.z, s.y + py * s.z, s.r, 0, Math.PI * 2);
        ctx.fill();
      }

      // ---- ribbon path (multi-pass for glow) ----
      const SAMPLES = 80;
      for (let pass = 0; pass < 3; pass++) {
        ctx.beginPath();
        for (let i = 0; i <= SAMPLES; i++) {
          const u = i / SAMPLES;
          const p = pathPoint(u, t);
          if (i === 0) ctx.moveTo(p.x, p.y);
          else ctx.lineTo(p.x, p.y);
        }
        if (pass === 0) {
          ctx.strokeStyle = 'rgba(34,211,238,0.06)';
          ctx.lineWidth = 18;
        } else if (pass === 1) {
          ctx.strokeStyle = 'rgba(34,211,238,0.18)';
          ctx.lineWidth = 4;
        } else {
          ctx.strokeStyle = 'rgba(34,211,238,0.7)';
          ctx.lineWidth = 1;
        }
        ctx.lineCap = 'round';
        ctx.stroke();
      }

      // ---- data packets streaming along path ----
      for (const pk of packets) {
        pk.u += pk.speed * (1/60);
        if (pk.u > 1.05) { pk.u = -0.05; pk.life = Math.random(); pk.tone = Math.random() < 0.18 ? 'gold' : 'cyan'; }
        if (pk.u < 0 || pk.u > 1) continue;
        const p = pathPoint(pk.u, t);
        const tone = pk.tone === 'gold' ? '212,175,55' : '34,211,238';
        // trail
        for (let k = 1; k <= 6; k++) {
          const tu = Math.max(0, pk.u - k * 0.012);
          const tp = pathPoint(tu, t);
          const a = (1 - k / 6) * 0.5;
          ctx.fillStyle = `rgba(${tone},${a})`;
          ctx.beginPath();
          ctx.arc(tp.x, tp.y, 2 - k * 0.2, 0, Math.PI * 2);
          ctx.fill();
        }
        // head — bright
        ctx.shadowBlur = 16;
        ctx.shadowColor = `rgba(${tone},0.95)`;
        ctx.fillStyle = `rgba(${tone},1)`;
        ctx.beginPath();
        ctx.arc(p.x, p.y, 2.4, 0, Math.PI * 2);
        ctx.fill();
        ctx.shadowBlur = 0;
        // halo
        ctx.fillStyle = `rgba(${tone},0.18)`;
        ctx.beginPath();
        ctx.arc(p.x, p.y, 8, 0, Math.PI * 2);
        ctx.fill();
      }

      // ---- blocks drifting along path ----
      // sort by depth (u) so latest renders last
      const sorted = blocks
        .map((b, idx) => ({ b, idx }))
        .sort((a, b) => a.b.u - b.b.u);

      for (let k = 0; k < sorted.length; k++) {
        const b = sorted[k].b;
        // gently drift the block along path; "latest" is rightmost
        b.u = b.baseU + Math.sin(t * 0.5 + b.phase) * 0.012;
        const p = pathPoint(b.u, t);
        // depth scale: rightmost = bigger / brighter
        const depth = Math.pow(b.u, 0.85);
        const scale = 0.55 + depth * 0.65;
        const alpha = 0.25 + depth * 0.7;
        const isLatest = k === sorted.length - 1;
        const float = Math.sin(t * 1.2 + b.phase) * 3;
        drawCubeBlock(p.x, p.y + float, b.size * scale, alpha, isLatest, t, p.ang);

        // labels (only on bigger ones, far enough apart)
        if (W >= 760 && depth > 0.35) {
          ctx.font = '10px JetBrains Mono, monospace';
          ctx.textAlign = 'center';
          ctx.fillStyle = isLatest ? 'rgba(34,211,238,0.95)' : `rgba(122,125,133,${0.4 + depth * 0.2})`;
          ctx.fillText(b.hash, p.x, p.y + b.size * scale + 22);
          if (isLatest) {
            ctx.fillStyle = 'rgba(212,175,55,0.95)';
            ctx.fillText('// LATEST', p.x, p.y - b.size * scale - 16);
            // bracket marks
            ctx.strokeStyle = 'rgba(212,175,55,0.7)';
            ctx.lineWidth = 1;
            const bw = b.size * scale * 1.6;
            ctx.beginPath();
            ctx.moveTo(p.x - bw, p.y - b.size * scale - 8);
            ctx.lineTo(p.x - bw, p.y - b.size * scale - 4);
            ctx.lineTo(p.x - bw + 6, p.y - b.size * scale - 4);
            ctx.moveTo(p.x + bw, p.y - b.size * scale - 8);
            ctx.lineTo(p.x + bw, p.y - b.size * scale - 4);
            ctx.lineTo(p.x + bw - 6, p.y - b.size * scale - 4);
            ctx.stroke();
          }
        }
      }

      // ---- soft top/bottom fade ----
      const fadeTop = ctx.createLinearGradient(0, 0, 0, H * 0.25);
      fadeTop.addColorStop(0, 'rgba(8,9,11,0.7)');
      fadeTop.addColorStop(1, 'rgba(8,9,11,0)');
      ctx.fillStyle = fadeTop;
      ctx.fillRect(0, 0, W, H * 0.25);
      const fadeBot = ctx.createLinearGradient(0, H * 0.75, 0, H);
      fadeBot.addColorStop(0, 'rgba(8,9,11,0)');
      fadeBot.addColorStop(1, 'rgba(8,9,11,0.85)');
      ctx.fillStyle = fadeBot;
      ctx.fillRect(0, H * 0.75, W, H * 0.25);

      raf = requestAnimationFrame(draw);
    }

    const ro = new ResizeObserver(resize);
    ro.observe(canvas);
    resize();
    draw();

    // pause when offscreen
    const visIO = new IntersectionObserver(([entry]) => {
      if (entry.isIntersecting && !raf) { draw(); }
      else if (!entry.isIntersecting && raf) { cancelAnimationFrame(raf); raf = 0; }
    }, { threshold: 0 });
    visIO.observe(canvas);

    // periodically rotate hashes (new block event) — feels like ledger advancing
    setInterval(() => {
      if (!blocks.length) return;
      // shift hashes left, generate new at end
      for (let i = 0; i < blocks.length - 1; i++) {
        blocks[i].hash = blocks[i + 1].hash;
      }
      blocks[blocks.length - 1].hash = '0x' + Math.random().toString(16).slice(2, 10);
      // gold flash on the latest
      blocks[blocks.length - 1].phase += 0.5;
    }, 2400);
  }

  /* ---------- 6. Status bar ticker ---------- */
  const ticker = document.querySelector('.statusbar-ticker');
  if (ticker) {
    const items = [
      'TESTNET-1 // OPERATIONAL',
      'CONSENSUS // BFT-PoS',
      'SIG // DILITHIUM-L5',
      'HASH // SHA3-256',
      'VM // WASM',
    ];
    let idx = 0;
    setInterval(() => {
      idx = (idx + 1) % items.length;
      ticker.querySelector('.t-dyn').textContent = items[idx];
    }, 3200);
  }

  /* ---------- 7. Tweaks panel ---------- */
  const TWEAKS = /*EDITMODE-BEGIN*/{
    "accent": "cyan",
    "density": "comfortable",
    "grid": true
  }/*EDITMODE-END*/;

  const tweaksEl = document.getElementById('tweaks');

  function applyTweaks(t) {
    // accent
    const accentMap = {
      cyan: '#22d3ee',
      gold: '#d4af37',
      mint: '#7ee5c8',
    };
    document.documentElement.style.setProperty('--accent', accentMap[t.accent] || accentMap.cyan);

    // density
    document.documentElement.style.setProperty(
      '--pad-x',
      t.density === 'compact' ? 'clamp(16px, 3vw, 40px)' : 'clamp(20px, 4vw, 64px)'
    );

    // grid
    document.body.classList.toggle('no-grid', !t.grid);
  }
  applyTweaks(TWEAKS);

  // build controls
  if (tweaksEl) {
    const groups = [
      { key: 'accent', label: 'Accent', opts: ['cyan', 'gold', 'mint'] },
      { key: 'density', label: 'Density', opts: ['comfortable', 'compact'] },
      { key: 'grid', label: 'Grid BG', opts: [true, false], labels: ['ON', 'OFF'] },
    ];
    const body = tweaksEl.querySelector('.tweak-body');
    body.innerHTML = groups.map(g => `
      <div class="tweak-row">
        <label>${g.label}</label>
        <div class="tweak-options" data-tweak="${g.key}">
          ${g.opts.map((o, i) => {
            const v = String(o);
            const lab = (g.labels && g.labels[i]) || String(o).toUpperCase();
            return `<button data-val="${v}" class="${TWEAKS[g.key] === o ? 'active' : ''}">${lab}</button>`;
          }).join('')}
        </div>
      </div>
    `).join('');

    body.addEventListener('click', (e) => {
      const btn = e.target.closest('[data-val]');
      if (!btn) return;
      const group = btn.parentElement.dataset.tweak;
      const raw = btn.dataset.val;
      const val = raw === 'true' ? true : raw === 'false' ? false : raw;
      TWEAKS[group] = val;
      applyTweaks(TWEAKS);
      btn.parentElement.querySelectorAll('button').forEach(b => b.classList.toggle('active', b === btn));
      try {
        window.parent.postMessage({ type: '__edit_mode_set_keys', edits: { [group]: val } }, window.location.origin);
      } catch (_) {}
    });

    tweaksEl.querySelector('.tweak-close').addEventListener('click', () => {
      tweaksEl.classList.remove('open');
      try { window.parent.postMessage({ type: '__edit_mode_dismissed' }, window.location.origin); } catch (_) {}
    });
  }

  window.addEventListener('message', (e) => {
    if (e.origin !== window.location.origin) return;
    const d = e.data || {};
    if (d.type === '__activate_edit_mode') tweaksEl?.classList.add('open');
    if (d.type === '__deactivate_edit_mode') tweaksEl?.classList.remove('open');
  });
  try { window.parent.postMessage({ type: '__edit_mode_available' }, window.location.origin); } catch (_) {}

})();


/* =========================================================
   PREMIUM ADDITIONS — runs after main IIFE
   ========================================================= */
(() => {
  'use strict';

  /* --- custom cursor --- */
  if (matchMedia('(hover: hover) and (pointer: fine)').matches) {
    const dot = document.createElement('div'); dot.className = 'cursor-dot';
    const ring = document.createElement('div'); ring.className = 'cursor-ring';
    document.body.append(dot, ring);
    let mx = innerWidth/2, my = innerHeight/2;
    let rx = mx, ry = my;
    addEventListener('pointermove', (e) => { mx = e.clientX; my = e.clientY; });
    function loop() {
      rx += (mx - rx) * 0.18;
      ry += (my - ry) * 0.18;
      dot.style.transform = `translate(${mx}px, ${my}px) translate(-50%,-50%)`;
      ring.style.transform = `translate(${rx}px, ${ry}px) translate(-50%,-50%)`;
      requestAnimationFrame(loop);
    }
    loop();
    document.addEventListener('pointerover', (e) => {
      const t = e.target;
      if (t.closest && t.closest('a, button, [data-magnetic], .stack-cell, .use-case')) {
        ring.classList.add('hover');
      } else {
        ring.classList.remove('hover');
      }
    });
  }

  /* --- magnetic buttons --- */
  document.querySelectorAll('[data-magnetic]').forEach(el => {
    el.addEventListener('pointermove', (e) => {
      const r = el.getBoundingClientRect();
      const x = e.clientX - (r.left + r.width/2);
      const y = e.clientY - (r.top + r.height/2);
      el.style.transform = `translate(${x*0.25}px, ${y*0.35}px)`;
    });
    el.addEventListener('pointerleave', () => { el.style.transform = ''; });
  });

  /* --- scroll progress --- */
  const sp = document.querySelector('.scroll-progress');
  if (sp) {
    addEventListener('scroll', () => {
      const h = document.documentElement.scrollHeight - innerHeight;
      const p = h > 0 ? scrollY / h : 0;
      sp.style.transform = `scaleX(${p})`;
    }, { passive: true });
  }

  /* --- letter reveal on hero h1 --- */
  document.querySelectorAll('[data-letters]').forEach(el => {
    if (el.dataset.split) return;
    el.dataset.split = '1';
    const txt = el.textContent;
    el.textContent = '';
    let delay = 0;
    txt.split(' ').forEach((word, wi) => {
      const wsp = document.createElement('span');
      wsp.className = 'word';
      [...word].forEach((ch, ci) => {
        const cs = document.createElement('span');
        cs.className = 'char';
        cs.textContent = ch;
        cs.style.animationDelay = (delay + ci * 0.025) + 's';
        wsp.append(cs);
      });
      delay += word.length * 0.025 + 0.06;
      el.append(wsp);
      el.append(document.createTextNode(' '));
    });
  });

  /* --- count-up numbers --- */
  const countIO = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      const el = e.target;
      const target = parseFloat(el.dataset.count);
      const dur = 1400;
      const t0 = performance.now();
      const start = 0;
      const suffix = el.dataset.suffix || '';
      const decimals = (el.dataset.count.split('.')[1] || '').length;
      function step(t) {
        const k = Math.min(1, (t - t0) / dur);
        const eased = 1 - Math.pow(1 - k, 3);
        const v = start + (target - start) * eased;
        el.textContent = decimals
          ? v.toFixed(decimals) + suffix
          : Math.floor(v).toLocaleString('en-US') + suffix;
        if (k < 1) requestAnimationFrame(step);
      }
      requestAnimationFrame(step);
      countIO.unobserve(el);
    }
  }, { threshold: 0.4 });
  document.querySelectorAll('[data-count]').forEach(el => countIO.observe(el));

  /* --- code tabs --- */
  document.querySelectorAll('.code-tabs').forEach(tabs => {
    const wrap = tabs.closest('.code-block');
    tabs.addEventListener('click', (e) => {
      const b = e.target.closest('button[data-tab]');
      if (!b) return;
      tabs.querySelectorAll('button').forEach(x => x.classList.toggle('active', x === b));
      wrap.querySelectorAll('.code-panel').forEach(p => {
        p.classList.toggle('active', p.dataset.tab === b.dataset.tab);
      });
    });
  });

  /* --- performance chart --- */
  const pc = document.getElementById('perf-chart');
  if (pc) {
    const cx = pc.getContext('2d');
    const dpr = Math.min(devicePixelRatio || 1, 2);
    let w = 0, h = 0;
    let series = []; // {tps, finality}
    function resize() {
      const r = pc.getBoundingClientRect();
      w = r.width; h = r.height;
      pc.width = w * dpr; pc.height = h * dpr;
      cx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }
    new ResizeObserver(resize).observe(pc);
    resize();
    // seed history
    for (let i = 0; i < 60; i++) {
      series.push({ tps: 16 + Math.random() * 6, fin: 0.9 + Math.random() * 0.6 });
    }
    function tick() {
      series.push({ tps: 16 + Math.random() * 6, fin: 0.9 + Math.random() * 0.6 });
      if (series.length > 60) series.shift();
      // update readouts
      const cur = series[series.length - 1];
      const r1 = document.querySelector('[data-perf="tps"]');
      const r2 = document.querySelector('[data-perf="fin"]');
      const r3 = document.querySelector('[data-perf="tps-avg"]');
      const r4 = document.querySelector('[data-perf="fin-p99"]');
      if (r1) r1.textContent = cur.tps.toFixed(1);
      if (r2) r2.textContent = cur.fin.toFixed(2) + 's';
      if (r3) r3.textContent = (series.reduce((a,b)=>a+b.tps,0)/series.length).toFixed(1);
      if (r4) r4.textContent = Math.max(...series.map(s=>s.fin)).toFixed(2) + 's';
    }
    tick();
    setInterval(tick, 1500);

    function draw() {
      cx.clearRect(0, 0, w, h);
      // grid
      cx.strokeStyle = 'rgba(255,255,255,0.04)';
      cx.lineWidth = 1;
      cx.beginPath();
      for (let i = 1; i < 5; i++) {
        const y = (h / 5) * i;
        cx.moveTo(0, y); cx.lineTo(w, y);
      }
      cx.stroke();

      // y-axis labels
      cx.font = '10px JetBrains Mono, monospace';
      cx.fillStyle = 'rgba(122,125,133,0.6)';
      cx.textAlign = 'left';
      for (let i = 0; i <= 5; i++) {
        const y = (h / 5) * i;
        const v = 25 - (i * 5);
        if (i < 5) cx.fillText(v + ' tps', 6, y - 4);
      }

      const N = series.length;
      const stepX = w / (N - 1);

      // tps area + line
      cx.beginPath();
      for (let i = 0; i < N; i++) {
        const x = i * stepX;
        const y = h - (series[i].tps / 25) * h;
        if (i === 0) cx.moveTo(x, y); else cx.lineTo(x, y);
      }
      cx.lineTo(w, h); cx.lineTo(0, h); cx.closePath();
      const grad = cx.createLinearGradient(0, 0, 0, h);
      grad.addColorStop(0, 'rgba(34,211,238,0.35)');
      grad.addColorStop(1, 'rgba(34,211,238,0)');
      cx.fillStyle = grad;
      cx.fill();

      cx.beginPath();
      for (let i = 0; i < N; i++) {
        const x = i * stepX;
        const y = h - (series[i].tps / 25) * h;
        if (i === 0) cx.moveTo(x, y); else cx.lineTo(x, y);
      }
      cx.strokeStyle = 'rgba(34,211,238,1)';
      cx.lineWidth = 1.5;
      cx.shadowBlur = 10;
      cx.shadowColor = 'rgba(34,211,238,0.6)';
      cx.stroke();
      cx.shadowBlur = 0;

      // finality (gold) — scale 0..3s
      cx.beginPath();
      for (let i = 0; i < N; i++) {
        const x = i * stepX;
        const y = h - (series[i].fin / 3) * h;
        if (i === 0) cx.moveTo(x, y); else cx.lineTo(x, y);
      }
      cx.strokeStyle = 'rgba(212,175,55,0.85)';
      cx.lineWidth = 1.2;
      cx.setLineDash([4, 3]);
      cx.stroke();
      cx.setLineDash([]);

      // current point
      const cur = series[N - 1];
      const cxX = (N-1) * stepX;
      const cxY = h - (cur.tps / 25) * h;
      cx.fillStyle = 'rgba(34,211,238,1)';
      cx.shadowBlur = 14;
      cx.shadowColor = 'rgba(34,211,238,1)';
      cx.beginPath(); cx.arc(cxX, cxY, 3.2, 0, Math.PI*2); cx.fill();
      cx.shadowBlur = 0;

      requestAnimationFrame(draw);
    }
    draw();
  }

  /* --- crypto vis canvases --- */
  document.querySelectorAll('[data-crypto-vis]').forEach(el => {
    const kind = el.dataset.cryptoVis;
    const c = document.createElement('canvas');
    el.appendChild(c);
    const cx = c.getContext('2d');
    const dpr = Math.min(devicePixelRatio || 1, 2);
    let w = 0, h = 0;
    function resize() {
      const r = el.getBoundingClientRect();
      w = r.width; h = r.height;
      c.width = w * dpr; c.height = h * dpr;
      c.style.width = w + 'px'; c.style.height = h + 'px';
      cx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }
    new ResizeObserver(resize).observe(el);
    resize();
    let t0 = performance.now();
    function draw() {
      const t = (performance.now() - t0) / 1000;
      cx.clearRect(0, 0, w, h);

      if (kind === 'ecc') {
        // ===== Proper elliptic curve y² = x³ - x + 1 =====
        // canonical "infinity loop" shape
        const cxx = w/2, cyy = h/2;
        const scale = Math.min(w, h*2.4) * 0.18;

        // axes (subtle)
        cx.strokeStyle = 'rgba(248,113,113,0.08)';
        cx.lineWidth = 1;
        cx.beginPath();
        cx.moveTo(0, cyy); cx.lineTo(w, cyy);
        cx.moveTo(cxx, 0); cx.lineTo(cxx, h);
        cx.stroke();

        // build curve points
        const pts = []; // [x, yPos, yNeg]
        for (let i = 0; i <= 400; i++) {
          const u = -2 + (i/400) * 4;          // x range -2..2
          const v2 = u*u*u - u + 1;            // y² = x³ - x + 1
          if (v2 < 0) continue;
          const v = Math.sqrt(v2);
          const px = cxx + u * scale;
          const py1 = cyy - v * scale;
          const py2 = cyy + v * scale;
          if (px < 0 || px > w) continue;
          pts.push([px, py1, py2]);
        }

        // top branch
        cx.strokeStyle = 'rgba(248,113,113,0.85)';
        cx.lineWidth = 1.5;
        cx.shadowBlur = 8;
        cx.shadowColor = 'rgba(248,113,113,0.4)';
        cx.beginPath();
        for (let i = 0; i < pts.length; i++) {
          if (i === 0) cx.moveTo(pts[i][0], pts[i][1]);
          else cx.lineTo(pts[i][0], pts[i][1]);
        }
        // bottom branch (reverse)
        for (let i = pts.length - 1; i >= 0; i--) {
          cx.lineTo(pts[i][0], pts[i][2]);
        }
        cx.closePath();
        cx.stroke();
        cx.shadowBlur = 0;

        // P + Q = R demo: choose two points, draw secant, reflect
        const ai = Math.floor(pts.length * 0.35);
        const bi = Math.floor(pts.length * 0.78);
        if (pts[ai] && pts[bi]) {
          const Ax = pts[ai][0], Ay = pts[ai][1];
          const Bx = pts[bi][0], By = pts[bi][2];

          // secant line (animated dash)
          cx.strokeStyle = 'rgba(248,113,113,0.5)';
          cx.lineWidth = 1;
          cx.setLineDash([4, 4]);
          cx.lineDashOffset = -t * 12;
          cx.beginPath();
          // extend line across canvas
          const dx = Bx - Ax, dy = By - Ay;
          const t1 = -100, t2 = 100;
          cx.moveTo(Ax + dx*t1, Ay + dy*t1);
          cx.lineTo(Ax + dx*t2, Ay + dy*t2);
          cx.stroke();
          cx.setLineDash([]);
          cx.lineDashOffset = 0;

          // P, Q, R points
          [[Ax, Ay, 'P'], [Bx, By, 'Q']].forEach(([x, y, lab]) => {
            cx.fillStyle = 'rgba(248,113,113,1)';
            cx.shadowBlur = 10; cx.shadowColor = 'rgba(248,113,113,0.8)';
            cx.beginPath(); cx.arc(x, y, 3.5, 0, Math.PI*2); cx.fill();
            cx.shadowBlur = 0;
            cx.fillStyle = 'rgba(232,228,216,0.7)';
            cx.font = '10px JetBrains Mono, monospace';
            cx.fillText(lab, x + 6, y - 4);
          });
        }

      } else {
        // ===== Lattice — proper 2D lattice with basis vectors and a target =====
        const cxx = w/2, cyy = h/2;
        const bx1 = w * 0.085, by1 = -h * 0.18;
        const bx2 = w * 0.04,  by2 = h * 0.32;

        // draw lattice points (linear combinations of basis)
        const range = 7;
        for (let i = -range; i <= range; i++) {
          for (let j = -range; j <= range; j++) {
            const x = cxx + i * bx1 + j * bx2;
            const y = cyy + i * by1 + j * by2;
            if (x < -4 || x > w+4 || y < -4 || y > h+4) continue;
            // jitter very subtle
            const jx = Math.sin(t * 0.6 + i * 0.7 + j * 1.1) * 0.6;
            const jy = Math.cos(t * 0.5 + i * 1.1 + j * 0.7) * 0.6;
            const dist = Math.hypot(i, j);
            const alpha = Math.max(0.15, 0.7 - dist * 0.06);
            cx.fillStyle = `rgba(34,211,238,${alpha})`;
            cx.beginPath(); cx.arc(x + jx, y + jy, 1.5, 0, Math.PI*2); cx.fill();
          }
        }

        // basis vectors from origin (cxx, cyy)
        cx.strokeStyle = 'rgba(34,211,238,0.95)';
        cx.lineWidth = 1.5;
        cx.shadowBlur = 6;
        cx.shadowColor = 'rgba(34,211,238,0.5)';

        function arrow(fromX, fromY, toX, toY) {
          cx.beginPath();
          cx.moveTo(fromX, fromY); cx.lineTo(toX, toY);
          cx.stroke();
          // head
          const ang = Math.atan2(toY - fromY, toX - fromX);
          const aLen = 6;
          cx.beginPath();
          cx.moveTo(toX, toY);
          cx.lineTo(toX - aLen * Math.cos(ang - 0.4), toY - aLen * Math.sin(ang - 0.4));
          cx.moveTo(toX, toY);
          cx.lineTo(toX - aLen * Math.cos(ang + 0.4), toY - aLen * Math.sin(ang + 0.4));
          cx.stroke();
        }
        arrow(cxx, cyy, cxx + bx1, cyy + by1);
        arrow(cxx, cyy, cxx + bx2, cyy + by2);
        cx.shadowBlur = 0;

        // origin point
        cx.fillStyle = 'rgba(232,228,216,0.9)';
        cx.beginPath(); cx.arc(cxx, cyy, 2.5, 0, Math.PI*2); cx.fill();

        // CVP target — gold point near a lattice point but not on it
        const ti = 2, tj = -1;
        const tgX = cxx + ti * bx1 + tj * bx2 + 14 + Math.sin(t * 0.8) * 3;
        const tgY = cyy + ti * by1 + tj * by2 + 8 + Math.cos(t * 0.8) * 3;
        // dashed line from target to nearest lattice point
        const nearX = cxx + ti * bx1 + tj * bx2;
        const nearY = cyy + ti * by1 + tj * by2;
        cx.strokeStyle = 'rgba(212,175,55,0.5)';
        cx.setLineDash([3, 3]);
        cx.beginPath(); cx.moveTo(tgX, tgY); cx.lineTo(nearX, nearY); cx.stroke();
        cx.setLineDash([]);
        cx.fillStyle = 'rgba(212,175,55,1)';
        cx.shadowBlur = 12; cx.shadowColor = 'rgba(212,175,55,0.8)';
        cx.beginPath(); cx.arc(tgX, tgY, 3.2, 0, Math.PI*2); cx.fill();
        cx.shadowBlur = 0;
      }
      requestAnimationFrame(draw);
    }
    draw();
  });

  /* --- world map (proper continent shapes via path data) --- */
  const wm = document.getElementById('world-map');
  if (wm) {
    const wctx = wm.getContext('2d');
    const dpr = Math.min(devicePixelRatio || 1, 2);
    let W = 0, H = 0;

    // Real continent SVG paths (low-poly), normalized 0..360 lon, 0..180 lat space
    // We'll use a stylized dotted grid with a continent mask defined by simple polygons.
    // Source: simplified Natural Earth low-res, expressed as longitude/latitude polygons.
    const continents = [
      // North America
      [[-170,68],[-130,70],[-100,73],[-80,72],[-60,60],[-55,50],[-65,45],[-75,42],[-80,30],[-95,28],[-100,22],[-110,22],[-118,30],[-125,38],[-130,52],[-140,58],[-160,62],[-170,68]],
      // South America
      [[-80,12],[-72,9],[-62,8],[-50,0],[-42,-5],[-38,-15],[-40,-25],[-50,-35],[-60,-45],[-72,-52],[-75,-45],[-72,-30],[-78,-15],[-80,-5],[-80,12]],
      // Europe + west Asia
      [[-10,58],[5,60],[20,68],[30,68],[45,55],[55,45],[40,40],[28,38],[18,40],[10,42],[0,45],[-8,50],[-10,58]],
      // Africa
      [[-15,32],[0,35],[12,32],[24,32],[35,30],[42,18],[50,12],[42,0],[40,-12],[32,-25],[20,-35],[10,-32],[5,-15],[-5,5],[-15,15],[-15,32]],
      // Asia (main + india)
      [[40,72],[80,75],[130,72],[155,65],[160,55],[145,45],[135,35],[125,30],[122,22],[110,15],[105,5],[100,2],[95,8],[88,22],[78,18],[72,22],[68,28],[60,32],[55,40],[50,48],[45,55],[42,62],[40,72]],
      // SE Asia / Indonesia (rough)
      [[95,5],[105,2],[120,-2],[135,-5],[140,-8],[130,-10],[115,-9],[100,-2],[95,5]],
      // Australia
      [[115,-12],[135,-12],[148,-18],[152,-28],[145,-38],[130,-35],[118,-32],[112,-22],[115,-12]],
      // UK + Ireland
      [[-10,55],[-2,58],[2,55],[0,50],[-6,50],[-10,55]],
      // Greenland
      [[-50,82],[-20,82],[-15,72],[-30,60],[-50,65],[-55,75],[-50,82]],
    ];

    function projection(lon, lat) {
      // simple equirectangular, fit box
      // lon: -180..180 → 0..1
      // lat: 85..-60 → 0..1 (skip antarctic mostly)
      const u = (lon + 180) / 360;
      const v = (85 - lat) / 145;
      return [u * W, v * H];
    }

    function resize() {
      const r = wm.getBoundingClientRect();
      W = r.width; H = r.height;
      wm.width = W * dpr; wm.height = H * dpr;
      wctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      drawMap();
    }
    new ResizeObserver(resize).observe(wm);
    resize();

    function pointInPolygon(px, py, poly) {
      let inside = false;
      for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
        const [xi, yi] = projection(poly[i][0], poly[i][1]);
        const [xj, yj] = projection(poly[j][0], poly[j][1]);
        const intersect = ((yi > py) !== (yj > py)) &&
          (px < (xj - xi) * (py - yi) / (yj - yi + 1e-9) + xi);
        if (intersect) inside = !inside;
      }
      return inside;
    }

    function drawMap() {
      wctx.clearRect(0, 0, W, H);

      // dotted land using real continent polygons as mask
      const cell = 7;
      for (let x = 0; x < W; x += cell) {
        for (let y = 0; y < H; y += cell) {
          let isLand = false;
          for (const poly of continents) {
            if (pointInPolygon(x, y, poly)) { isLand = true; break; }
          }
          if (isLand) {
            // distance-to-edge fade for nice look (skip — just uniform)
            wctx.fillStyle = 'rgba(122,125,133,0.4)';
            wctx.beginPath();
            wctx.arc(x, y, 1, 0, Math.PI * 2);
            wctx.fill();
          }
        }
      }

      // Pin lon/lat data
      const pins = [
        { lon: 2.35,   lat: 48.85, id: 'val-01' }, // Paris
        { lon: 8.68,   lat: 50.11, id: 'val-02' }, // Frankfurt
        { lon: -74.0,  lat: 40.71, id: 'val-03' }, // NYC
        { lon: 103.85, lat: 1.35,  id: 'val-04' }, // Singapore
      ];
      const pinXY = pins.map(p => projection(p.lon, p.lat));

      // arcs between pins
      for (let i = 0; i < pinXY.length; i++) {
        for (let j = i+1; j < pinXY.length; j++) {
          const [ax, ay] = pinXY[i], [bx, by] = pinXY[j];
          const mx = (ax+bx)/2, my = (ay+by)/2 - Math.abs(bx-ax)*0.18;
          const grad = wctx.createLinearGradient(ax, ay, bx, by);
          grad.addColorStop(0, 'rgba(34,211,238,0.4)');
          grad.addColorStop(0.5, 'rgba(34,211,238,0.15)');
          grad.addColorStop(1, 'rgba(34,211,238,0.4)');
          wctx.strokeStyle = grad;
          wctx.lineWidth = 1;
          wctx.setLineDash([3, 3]);
          wctx.beginPath();
          wctx.moveTo(ax, ay);
          wctx.quadraticCurveTo(mx, my, bx, by);
          wctx.stroke();
          wctx.setLineDash([]);
        }
      }

      // Update HTML pin positions to match projection
      pins.forEach((p, i) => {
        const pinEl = document.querySelector(`.world-pin[data-id="${p.id}"]`);
        if (pinEl) {
          const [x, y] = pinXY[i];
          pinEl.style.left = (x / W * 100) + '%';
          pinEl.style.top = (y / H * 100) + '%';
        }
      });
    }
  }

})();
