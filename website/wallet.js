// ==========================================================================
// CURS3D — Browser wallet logic.
//
// Loaded as <script type="module">. Talks to:
//   - /wallet-wasm/curs3d_wallet_wasm.js   (Dilithium-L5 + Argon2id + AES-GCM)
//   - https://api.curs3d.fr/api/...        (read balances, post signed txs)
//
// Security posture:
//   - Private key bytes only live inside the WASM module's memory. The JS
//     side keeps a `KeyPair` instance handle + the address string — never
//     raw secret bytes.
//   - Locking the wallet drops the JS-side reference to the KeyPair so the
//     WASM memory is freed at the next GC sweep.
//   - The password is recomputed with Argon2id every time the user unlocks;
//     it is never stored or echoed.
//   - Encrypted JSON is the only thing in localStorage.
//   - No analytics, no third-party JS at runtime besides the QR code
//     library (qrcode.js, hash-pinned to a specific version on unpkg.com).
//
// Fallback: if the WASM bundle fails to load, the page enters a read-only
// mode where the user can still view any address's balance / history.
// ==========================================================================

const API_BASE = "https://api.curs3d.fr";
const EXPLORER_BASE = "https://explorer.curs3d.fr";
const WASM_URL = "/wallet-wasm/curs3d_wallet_wasm.js";
const STORAGE_KEY = "curs3d.wallet.v1";
const HISTORY_PAGE_SIZE = 20;

const ADDR_REGEX = /^(CUR|0x)?[0-9a-fA-F]{40}$/;
const MICRO_PER_CUR = 1_000_000n;

// ──────────────────────────────────────────────────────────────────────
// DOM helpers
// ──────────────────────────────────────────────────────────────────────
const $ = (id) => document.getElementById(id);
const t = (en, fr) =>
  (document.documentElement.getAttribute("data-current-lang") || "en") === "fr" ? fr : en;

function setText(el, text) { if (el) el.textContent = text; }
function show(el, on = true) { if (el) el.style.display = on ? "" : "none"; }
function setError(el, msg) { if (el) el.textContent = msg || ""; }

// Strict 40-hex normalization. Returns lowercase hex (no prefix) or null.
function normalizeAddress(input) {
  if (typeof input !== "string") return null;
  const cleaned = input.trim();
  if (!ADDR_REGEX.test(cleaned)) return null;
  return cleaned.replace(/^(CUR|0x)/, "").toLowerCase();
}

// "CUR" + first hex char in input (preserve case if user used checksum).
function withCurPrefix(rawHex) {
  return "CUR" + rawHex;
}

// Convert decimal CUR string -> microtokens BigInt. Returns null on parse error
// or if more than 6 decimals are provided.
function curToMicrotokens(input) {
  if (typeof input !== "string") return null;
  const s = input.trim();
  if (!/^\d+(\.\d{1,6})?$/.test(s)) return null;
  const [intPart, fracPart = ""] = s.split(".");
  const padded = (fracPart + "000000").slice(0, 6);
  try {
    return BigInt(intPart) * MICRO_PER_CUR + BigInt(padded);
  } catch {
    return null;
  }
}

function microToCur(micro) {
  if (micro == null) return "0";
  let n;
  try {
    n = typeof micro === "bigint" ? micro : BigInt(micro);
  } catch {
    return String(micro);
  }
  const whole = n / MICRO_PER_CUR;
  const frac = n % MICRO_PER_CUR;
  if (frac === 0n) return whole.toString();
  const fracStr = frac.toString().padStart(6, "0").replace(/0+$/, "");
  return `${whole.toString()}.${fracStr}`;
}

function shortHash(hash) {
  if (!hash) return "—";
  return hash.length > 14 ? hash.slice(0, 8) + "…" + hash.slice(-6) : hash;
}

function formatTimestamp(unixSeconds) {
  if (!unixSeconds) return "—";
  const diff = Math.floor(Date.now() / 1000 - Number(unixSeconds));
  if (diff < 0) return t("just now", "à l'instant");
  if (diff < 60) return diff + "s";
  if (diff < 3600) return Math.floor(diff / 60) + "m";
  if (diff < 86400) return Math.floor(diff / 3600) + "h";
  return Math.floor(diff / 86400) + "d";
}

function downloadJson(filename, data) {
  const blob = new Blob([data], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(() => URL.revokeObjectURL(url), 2000);
}

// ──────────────────────────────────────────────────────────────────────
// Banner stack (banners are appended dynamically — insecure connection,
// WASM read-only, and ad-hoc info messages).
// ──────────────────────────────────────────────────────────────────────
function pushBanner(severity, htmlContent, opts = {}) {
  const stack = $("walletBanners");
  if (!stack) return null;
  const div = document.createElement("div");
  div.className = "callout " + (severity === "warn" ? "warning" : severity === "err" ? "danger" : "");
  if (opts.id) div.id = opts.id;
  div.innerHTML = htmlContent;
  stack.appendChild(div);
  return div;
}

function clearBannerById(id) {
  const el = $(id);
  if (el && el.parentNode) el.parentNode.removeChild(el);
}

// ──────────────────────────────────────────────────────────────────────
// WASM bundle load
// ──────────────────────────────────────────────────────────────────────
let wasm = null;        // module exports
let wasmReady = false;  // truthy if KeyPair etc. are usable
let wasmError = null;

async function loadWasm() {
  // Show a transient "loading" banner; clear it once the import resolves.
  const loadingBanner = pushBanner(
    "info",
    `<p>${t(
      "WASM module loading… signing will be available shortly.",
      "Chargement du module WASM… la signature sera disponible dans un instant."
    )}</p>`,
    { id: "wasmLoadingBanner" }
  );
  try {
    const mod = await import(/* webpackIgnore: true */ WASM_URL);
    // Most wasm-bindgen output exports a default init() that fetches the
    // .wasm file. Call it if present.
    if (typeof mod.default === "function") {
      try { await mod.default(); } catch { /* some bundles auto-init */ }
    }
    wasm = mod;
    wasmReady = !!(mod.KeyPair && typeof mod.build_transfer_tx === "function");
    if (!wasmReady) throw new Error("WASM bundle missing KeyPair / build_transfer_tx exports");
  } catch (err) {
    wasmError = err;
  } finally {
    clearBannerById("wasmLoadingBanner");
  }
}

// ──────────────────────────────────────────────────────────────────────
// Insecure-connection banner
// ──────────────────────────────────────────────────────────────────────
function maybeShowInsecureBanner() {
  if (window.location.protocol === "http:" && window.location.hostname !== "localhost" && window.location.hostname !== "127.0.0.1") {
    pushBanner(
      "warn",
      `<p><strong>${t("Insecure connection.", "Connexion non sécurisée.")}</strong> ${t(
        "Do not enter your password on http://. Use the https:// version of this page.",
        "Ne saisissez pas votre mot de passe sur http://. Utilisez la version https:// de cette page."
      )}</p>`
    );
  }
}

// ──────────────────────────────────────────────────────────────────────
// API helpers
// ──────────────────────────────────────────────────────────────────────
async function apiGet(path) {
  const res = await fetch(API_BASE + path, { mode: "cors" });
  if (!res.ok) throw new Error("HTTP " + res.status);
  const json = await res.json().catch(() => ({}));
  if (json && json.ok === true && json.data !== undefined) return json.data;
  if (json && json.ok === false) throw new Error(json.error || "API error");
  return json;
}

async function apiPostJson(path, body) {
  const res = await fetch(API_BASE + path, {
    method: "POST",
    mode: "cors",
    headers: { "Content-Type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
  const json = await res.json().catch(() => ({}));
  if (!res.ok || (json && json.ok === false)) {
    throw new Error((json && json.error) || ("HTTP " + res.status));
  }
  return json && json.data !== undefined ? json.data : json;
}

// ──────────────────────────────────────────────────────────────────────
// Storage
// ──────────────────────────────────────────────────────────────────────
function loadStoredWallet() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function saveStoredWallet(encryptedJson) {
  // The encrypted JSON itself is what we keep — never the password.
  localStorage.setItem(STORAGE_KEY, encryptedJson);
}

function deleteStoredWallet() {
  localStorage.removeItem(STORAGE_KEY);
}

function storedWalletAddress() {
  const obj = loadStoredWallet();
  if (!obj || typeof obj !== "object") return null;
  return obj.address || null;
}

// ──────────────────────────────────────────────────────────────────────
// In-memory wallet state (lifecycle: create / unlock / lock)
// ──────────────────────────────────────────────────────────────────────
let keypair = null;       // wasm.KeyPair instance handle
let walletAddress = null; // CUR... string

function setUnlocked(kp, address) {
  keypair = kp;
  walletAddress = address;
  renderUnlocked();
}

function lockWallet() {
  // Drop reference. WASM memory will be reclaimed at next GC.
  // wasm-bindgen also exposes `.free()` on heap objects; call it if present.
  if (keypair && typeof keypair.free === "function") {
    try { keypair.free(); } catch { /* idempotent */ }
  }
  keypair = null;
  walletAddress = null;
  renderInitial();
}

// ──────────────────────────────────────────────────────────────────────
// Renderers (pick which view to show based on state)
// ──────────────────────────────────────────────────────────────────────
function renderInitial() {
  const stored = loadStoredWallet();

  if (!wasmReady) {
    // Read-only fallback path
    show($("lockedView"), false);
    show($("unlockedView"), false);
    show($("readonlyView"), true);
    if (!$("readonlyBanner")) {
      pushBanner(
        "warn",
        `<p><strong>${t("Wallet bundle failed to load.", "Le module wallet n'a pas pu se charger.")}</strong> ${t(
          "The wallet is read-only. You can still inspect any CURS3D address below.",
          "Le wallet est en lecture seule. Vous pouvez tout de même inspecter une adresse CURS3D ci-dessous."
        )}</p>`,
        { id: "readonlyBanner" }
      );
    }
    return;
  }

  // Locked state (with or without an existing stored wallet)
  show($("readonlyView"), false);
  show($("unlockedView"), false);
  show($("lockedView"), true);

  if (stored && stored.address) {
    show($("unlockCard"), true);
    setText($("unlockAddr"), stored.address);
  } else {
    show($("unlockCard"), false);
  }
}

function renderUnlocked() {
  show($("lockedView"), false);
  show($("readonlyView"), false);
  show($("unlockedView"), true);

  setText($("addrText"), walletAddress);
  setText($("qrAddrText"), walletAddress);
  const link = $("explorerLink");
  if (link) link.href = `${EXPLORER_BASE}/address/${walletAddress}`;

  // Reset balance / history skeletons then refresh
  refreshBalance();
  refreshHistory(0);
}

// ──────────────────────────────────────────────────────────────────────
// Create flow
// ──────────────────────────────────────────────────────────────────────
async function handleCreate(e) {
  e.preventDefault();
  setError($("createErr"), "");
  if (!wasmReady) {
    setError($("createErr"), t("Wallet module not ready.", "Module wallet indisponible."));
    return;
  }
  const p1 = $("createPwd1").value;
  const p2 = $("createPwd2").value;
  if (p1.length < 8) {
    setError($("createErr"), t("Password must be at least 8 characters.", "Le mot de passe doit comporter au moins 8 caractères."));
    return;
  }
  if (p1 !== p2) {
    setError($("createErr"), t("Passwords do not match.", "Les mots de passe ne correspondent pas."));
    return;
  }

  const btn = $("createBtn");
  btn.disabled = true;
  try {
    const kp = wasm.KeyPair.generate();
    const address = kp.address();
    const encryptedJson = kp.save_encrypted(p1);
    // Persist + download backup before unlocking.
    saveStoredWallet(encryptedJson);
    const filename = `curs3d-wallet-${address}.json`;
    downloadJson(filename, encryptedJson);
    // Wipe password fields immediately.
    $("createPwd1").value = "";
    $("createPwd2").value = "";
    setUnlocked(kp, address);
  } catch (err) {
    setError($("createErr"), (err && err.message) || String(err));
  } finally {
    btn.disabled = false;
  }
}

// ──────────────────────────────────────────────────────────────────────
// Import flow
// ──────────────────────────────────────────────────────────────────────
async function handleImport(e) {
  e.preventDefault();
  setError($("importErr"), "");
  if (!wasmReady) {
    setError($("importErr"), t("Wallet module not ready.", "Module wallet indisponible."));
    return;
  }
  const json = $("importJson").value.trim();
  const pwd = $("importPwd").value;
  if (!json) {
    setError($("importErr"), t("Paste the encrypted JSON or upload a file.", "Collez le JSON chiffré ou téléversez un fichier."));
    return;
  }
  if (!pwd) {
    setError($("importErr"), t("Enter the password.", "Entrez le mot de passe."));
    return;
  }
  // Sanity-check JSON shape before sending to WASM (cheap parse failure).
  try { JSON.parse(json); } catch {
    setError($("importErr"), t("Invalid JSON.", "JSON invalide."));
    return;
  }

  const btn = $("importBtn");
  btn.disabled = true;
  try {
    const kp = wasm.KeyPair.load_encrypted(json, pwd);
    const address = kp.address();
    saveStoredWallet(json);
    $("importJson").value = "";
    $("importPwd").value = "";
    if ($("importFile")) $("importFile").value = "";
    setUnlocked(kp, address);
  } catch (err) {
    const msg = (err && err.message) || String(err);
    setError($("importErr"), /password|decrypt/i.test(msg)
      ? t("Wrong password or corrupted backup.", "Mot de passe incorrect ou backup corrompu.")
      : msg);
  } finally {
    btn.disabled = false;
  }
}

function handleImportFile(e) {
  const file = e.target.files && e.target.files[0];
  if (!file) return;
  const reader = new FileReader();
  reader.onload = () => {
    $("importJson").value = String(reader.result || "");
  };
  reader.onerror = () => {
    setError($("importErr"), t("Could not read file.", "Impossible de lire le fichier."));
  };
  reader.readAsText(file);
}

// ──────────────────────────────────────────────────────────────────────
// Unlock flow (existing localStorage wallet)
// ──────────────────────────────────────────────────────────────────────
async function handleUnlock(e) {
  e.preventDefault();
  setError($("unlockErr"), "");
  if (!wasmReady) {
    setError($("unlockErr"), t("Wallet module not ready.", "Module wallet indisponible."));
    return;
  }
  const stored = localStorage.getItem(STORAGE_KEY);
  if (!stored) {
    setError($("unlockErr"), t("No saved wallet on this device.", "Aucun wallet sauvegardé sur cet appareil."));
    return;
  }
  const pwd = $("unlockPwd").value;
  if (!pwd) {
    setError($("unlockErr"), t("Enter the password.", "Entrez le mot de passe."));
    return;
  }
  const btn = $("unlockBtn");
  btn.disabled = true;
  try {
    const kp = wasm.KeyPair.load_encrypted(stored, pwd);
    const address = kp.address();
    $("unlockPwd").value = "";
    setUnlocked(kp, address);
  } catch (err) {
    const msg = (err && err.message) || String(err);
    setError($("unlockErr"), /password|decrypt/i.test(msg)
      ? t("Wrong password.", "Mot de passe incorrect.")
      : msg);
  } finally {
    btn.disabled = false;
  }
}

function handleForget() {
  const yes = confirm(
    t(
      "Delete the saved wallet from this browser? Make sure you still have the backup file — without it, you cannot recover the funds.",
      "Supprimer le wallet sauvegardé de ce navigateur ? Assurez-vous d'avoir le fichier de backup — sans lui, les fonds sont irrécupérables."
    )
  );
  if (!yes) return;
  deleteStoredWallet();
  renderInitial();
}

// ──────────────────────────────────────────────────────────────────────
// Balance + nonce
// ──────────────────────────────────────────────────────────────────────
let cachedAccount = null;

async function refreshBalance() {
  if (!walletAddress) return;
  try {
    const data = await apiGet("/api/account/" + encodeURIComponent(walletAddress));
    cachedAccount = data || {};
    setText($("balLiquid"), microToCur(data.balance ?? 0n) + " CUR");
    setText($("balLiquidMicro"), (data.balance ?? 0) + " µCUR");
    setText($("balStaked"), microToCur(data.staked_balance ?? 0n) + " CUR");
    setText($("balStakedMicro"), (data.staked_balance ?? 0) + " µCUR");

    // pending_unstakes shape: array of {amount, unlock_height} OR a sum field
    let pendingTotal = 0n;
    if (Array.isArray(data.pending_unstakes)) {
      for (const u of data.pending_unstakes) {
        try { pendingTotal += BigInt(u.amount ?? 0); } catch {}
      }
    } else if (data.pending_unstakes != null) {
      try { pendingTotal = BigInt(data.pending_unstakes); } catch {}
    }
    setText($("balPendingUnstake"), microToCur(pendingTotal) + " CUR");
    setText($("balPendingUnstakeMicro"), pendingTotal.toString() + " µCUR");

    setText($("balNonce"), String(data.nonce ?? 0));
    const valState = data.validator_active_from_height
      ? t("validator (active)", "validateur (actif)")
      : (data.jailed_until_height ? t("jailed", "en jail") : "—");
    setText($("balValidatorState"), valState);

    setText($("estNonce"), String(data.nonce ?? 0));
    updateSendSummary();
  } catch (err) {
    setText($("balLiquid"), "—");
    setText($("balLiquidMicro"), t("API unreachable", "API inaccessible"));
  }
}

// ──────────────────────────────────────────────────────────────────────
// Send transaction
// ──────────────────────────────────────────────────────────────────────
let currentTxType = "transfer"; // transfer | stake | unstake

function bindTxTypeSelector() {
  document.querySelectorAll(".tx-type-select [data-txtype]").forEach((btn) => {
    btn.addEventListener("click", () => {
      document.querySelectorAll(".tx-type-select [data-txtype]").forEach((b) => b.classList.remove("active"));
      btn.classList.add("active");
      currentTxType = btn.getAttribute("data-txtype");
      const recipientField = $("sendToField");
      // Stake/unstake transactions have no separate recipient — value
      // is staked from / unstaked to the sender's own account.
      if (currentTxType === "transfer") {
        show(recipientField, true);
      } else {
        show(recipientField, false);
        $("sendTo").value = "";
      }
      updateSendSummary();
    });
  });
}

function updateSendSummary() {
  const fee = BigInt($("sendFee").value || 0);
  const amountMicro = curToMicrotokens($("sendAmount").value || "0") || 0n;
  setText($("estFee"), `${fee.toString()} µCUR (${microToCur(fee)} CUR)`);
  const total = amountMicro + fee;
  setText($("estTotal"), `${total.toString()} µCUR (${microToCur(total)} CUR)`);
}

async function handleSend(e) {
  e.preventDefault();
  setError($("sendErr"), "");
  setError($("sendOk"), "");
  if (!wasmReady || !keypair) {
    setError($("sendErr"), t("Wallet locked.", "Wallet verrouillé."));
    return;
  }

  let toClean;
  if (currentTxType === "transfer") {
    toClean = normalizeAddress($("sendTo").value);
    if (!toClean) {
      setError($("sendErr"), t("Invalid recipient address.", "Adresse destinataire invalide."));
      return;
    }
  } else {
    // For stake / unstake the protocol still expects a `to` field — set
    // it to the sender's own address as a no-op placeholder. The actual
    // tx kind is encoded inside the build_*_tx call.
    const me = normalizeAddress(walletAddress);
    if (!me) {
      setError($("sendErr"), t("Invalid wallet address state.", "État d'adresse de wallet invalide."));
      return;
    }
    toClean = me;
  }

  const amountMicro = curToMicrotokens($("sendAmount").value);
  if (amountMicro == null || amountMicro <= 0n) {
    setError($("sendErr"), t("Amount must be a positive number with up to 6 decimals.", "Le montant doit être un nombre positif avec au plus 6 décimales."));
    return;
  }
  const fee = BigInt($("sendFee").value || 0);
  const gas = BigInt($("sendGas").value || 0);
  if (fee < 0n || gas < 21000n) {
    setError($("sendErr"), t("Gas limit must be at least 21000.", "Le gas limit doit être d'au moins 21000."));
    return;
  }
  // Don't allow sending more than (liquid balance - fee) for transfers.
  const liquid = BigInt((cachedAccount && cachedAccount.balance) || 0);
  const totalCost = amountMicro + fee;
  if (currentTxType === "transfer" && totalCost > liquid) {
    setError($("sendErr"), t(
      `Insufficient balance. Liquid ${microToCur(liquid)} CUR, need ${microToCur(totalCost)}.`,
      `Solde insuffisant. Liquide ${microToCur(liquid)} CUR, requis ${microToCur(totalCost)}.`
    ));
    return;
  }
  if (currentTxType === "stake" && amountMicro > liquid) {
    setError($("sendErr"), t("Cannot stake more than your liquid balance.", "Impossible de staker plus que le solde liquide."));
    return;
  }
  const stakedTotal = BigInt((cachedAccount && cachedAccount.staked_balance) || 0);
  if (currentTxType === "unstake" && amountMicro > stakedTotal) {
    setError($("sendErr"), t("Cannot unstake more than your staked balance.", "Impossible de retirer plus que le stake actuel."));
    return;
  }

  const nonce = BigInt((cachedAccount && cachedAccount.nonce) || 0);

  const btn = $("sendBtn");
  btn.disabled = true;
  try {
    let signedJson;
    if (currentTxType === "transfer") {
      signedJson = wasm.build_transfer_tx(keypair, toClean, amountMicro, fee, nonce);
    } else if (currentTxType === "stake" && typeof wasm.build_stake_tx === "function") {
      signedJson = wasm.build_stake_tx(keypair, amountMicro, fee, nonce);
    } else if (currentTxType === "unstake" && typeof wasm.build_unstake_tx === "function") {
      signedJson = wasm.build_unstake_tx(keypair, amountMicro, fee, nonce);
    } else {
      throw new Error(t(
        "This transaction kind is not yet supported by the WASM bundle.",
        "Ce type de transaction n'est pas encore supporté par le bundle WASM."
      ));
    }

    const result = await apiPostJson("/api/tx/submit", signedJson);
    const txHash = (result && (result.tx_hash || result.hash)) || "";
    if (txHash) {
      const link = `${EXPLORER_BASE}/tx/${txHash}`;
      $("sendOk").innerHTML = `${t("Submitted.", "Transaction soumise.")} <a href="${link}" target="_blank" rel="noopener noreferrer" style="color:var(--accent);">${shortHash(txHash)} ↗</a>`;
    } else {
      $("sendOk").textContent = t("Submitted.", "Transaction soumise.");
    }
    $("sendAmount").value = "";
    if (currentTxType === "transfer") $("sendTo").value = "";
    updateSendSummary();
    setTimeout(refreshBalance, 1200);
    setTimeout(() => refreshHistory(0), 2500);
  } catch (err) {
    setError($("sendErr"), (err && err.message) || String(err));
  } finally {
    btn.disabled = false;
  }
}

// ──────────────────────────────────────────────────────────────────────
// History
// ──────────────────────────────────────────────────────────────────────
let historyPage = 0;
let lastHistoryLength = 0;

async function refreshHistory(page = 0) {
  if (!walletAddress) return;
  historyPage = Math.max(0, page);
  const offset = historyPage * HISTORY_PAGE_SIZE;
  const tbody = $("historyBody");
  if (!tbody) return;
  // Skeleton
  tbody.innerHTML =
    `<tr><td colspan="6" class="history-empty"><span class="skel" style="width:120px;"></span></td></tr>`;
  try {
    const data = await apiGet(
      `/api/account/${encodeURIComponent(walletAddress)}/transactions?limit=${HISTORY_PAGE_SIZE}&offset=${offset}`
    );
    const items = Array.isArray(data) ? data : (data && data.transactions) || [];
    lastHistoryLength = items.length;
    if (items.length === 0) {
      tbody.innerHTML =
        `<tr><td colspan="6" class="history-empty">${t("No transactions yet.", "Aucune transaction.")}</td></tr>`;
    } else {
      tbody.innerHTML = "";
      for (const tx of items) {
        const tr = document.createElement("tr");
        tr.className = "history-row";

        const hash = tx.hash || "";
        const kind = (tx.kind || tx.type || "transfer").toLowerCase();
        const amount = tx.amount ?? 0;
        const me = walletAddress.toLowerCase();
        const fromMe = (tx.from || "").toLowerCase() === me;
        const counter = fromMe ? (tx.to || "—") : (tx.from || "—");
        const status = tx.status || (tx.success === false ? "fail" : (tx.included ? "ok" : "ok"));
        const sign = fromMe ? "-" : "+";
        const ts = tx.timestamp || tx.block_time || tx.time || 0;

        const tds = [
          // hash
          (() => {
            const td = document.createElement("td");
            const a = document.createElement("a");
            a.className = "hash-link";
            a.href = `${EXPLORER_BASE}/tx/${hash}`;
            a.target = "_blank";
            a.rel = "noopener noreferrer";
            a.textContent = shortHash(hash);
            td.appendChild(a);
            return td;
          })(),
          // kind
          (() => {
            const td = document.createElement("td");
            const span = document.createElement("span");
            span.className = "kind-pill " + kind;
            span.textContent = kind;
            td.appendChild(span);
            return td;
          })(),
          // counterparty
          (() => {
            const td = document.createElement("td");
            td.className = "mono";
            td.textContent = shortHash(counter);
            return td;
          })(),
          // amount
          (() => {
            const td = document.createElement("td");
            td.className = "mono";
            td.textContent = sign + microToCur(amount) + " CUR";
            return td;
          })(),
          // status
          (() => {
            const td = document.createElement("td");
            const wrap = document.createElement("span");
            wrap.className = "status-dot-cell";
            const dot = document.createElement("span");
            dot.className = "d " + (status === "fail" ? "fail" : status === "pending" ? "pending" : "ok");
            wrap.appendChild(dot);
            const lbl = document.createElement("span");
            lbl.textContent = status;
            wrap.appendChild(lbl);
            td.appendChild(wrap);
            return td;
          })(),
          // when
          (() => {
            const td = document.createElement("td");
            td.className = "mono";
            td.textContent = formatTimestamp(ts);
            return td;
          })(),
        ];
        tds.forEach((td) => tr.appendChild(td));
        tbody.appendChild(tr);
      }
    }
    setText($("historyPageInfo"),
      `${t("page", "page")} ${historyPage + 1} · ${items.length} ${t("rows", "lignes")}`);
  } catch (err) {
    tbody.innerHTML =
      `<tr><td colspan="6" class="history-empty">${t("Failed to load history: ", "Échec du chargement : ")}${(err.message || err)}</td></tr>`;
  }
}

// ──────────────────────────────────────────────────────────────────────
// Read-only mode (WASM unavailable)
// ──────────────────────────────────────────────────────────────────────
async function handleReadonly(e) {
  e.preventDefault();
  setError($("readonlyErr"), "");
  const cleaned = normalizeAddress($("readonlyAddr").value);
  if (!cleaned) {
    setError($("readonlyErr"), t("Invalid CURS3D address.", "Adresse CURS3D invalide."));
    return;
  }
  walletAddress = withCurPrefix(cleaned);
  // Show the unlocked-style read-only panels: address, balance, history.
  // Hide controls that need a private key (send, lock, export, delete).
  show($("unlockedView"), true);
  show($("sendPanel"), false);
  show($("walletActionsRow"), false);
  setText($("addrText"), walletAddress);
  setText($("qrAddrText"), walletAddress);
  const link = $("explorerLink");
  if (link) link.href = `${EXPLORER_BASE}/address/${walletAddress}`;
  refreshBalance();
  refreshHistory(0);
}

// ──────────────────────────────────────────────────────────────────────
// QR modal
// ──────────────────────────────────────────────────────────────────────
function openQrModal() {
  if (!walletAddress) return;
  const modal = $("qrModal");
  const canvasHost = $("qrCanvas");
  canvasHost.innerHTML = "";
  setText($("qrAddrText"), walletAddress);
  // Render the QR. qrcode@1.5.x exposes a global `QRCode` with .toCanvas()
  if (typeof QRCode !== "undefined" && QRCode && QRCode.toCanvas) {
    const canvas = document.createElement("canvas");
    canvasHost.appendChild(canvas);
    QRCode.toCanvas(canvas, walletAddress, { width: 240, margin: 1 }, (err) => {
      if (err) {
        canvasHost.textContent = t("Could not render QR code.", "Impossible de générer le QR code.");
      }
    });
  } else {
    // Fallback if the CDN library is blocked: show address big.
    canvasHost.textContent = walletAddress;
  }
  modal.classList.add("open");
}

function closeQrModal() {
  $("qrModal").classList.remove("open");
}

// ──────────────────────────────────────────────────────────────────────
// Export / delete
// ──────────────────────────────────────────────────────────────────────
function handleExport() {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (!stored) return;
  const filename = `curs3d-wallet-${walletAddress || "backup"}.json`;
  downloadJson(filename, stored);
}

function handleDelete() {
  const yes = confirm(
    t(
      "Permanently delete this wallet from this browser? You will need the backup file (and password) to restore access. This cannot be undone.",
      "Supprimer définitivement ce wallet de ce navigateur ? Vous aurez besoin du backup (et du mot de passe) pour le restaurer. Action irréversible."
    )
  );
  if (!yes) return;
  deleteStoredWallet();
  lockWallet();
}

// ──────────────────────────────────────────────────────────────────────
// Copy address
// ──────────────────────────────────────────────────────────────────────
async function handleCopyAddr() {
  if (!walletAddress) return;
  try {
    await navigator.clipboard.writeText(walletAddress);
    const btn = $("copyAddrBtn");
    btn.classList.add("done");
    const orig = btn.textContent;
    btn.textContent = t("Copied", "Copié");
    setTimeout(() => {
      btn.classList.remove("done");
      btn.textContent = orig;
    }, 1400);
  } catch {
    // ignore — clipboard API may be denied
  }
}

// ──────────────────────────────────────────────────────────────────────
// Wire up
// ──────────────────────────────────────────────────────────────────────
function wireUp() {
  $("createForm").addEventListener("submit", handleCreate);
  $("importForm").addEventListener("submit", handleImport);
  $("importFile").addEventListener("change", handleImportFile);

  $("unlockForm").addEventListener("submit", handleUnlock);
  $("forgetBtn").addEventListener("click", handleForget);

  $("readonlyForm").addEventListener("submit", handleReadonly);

  $("sendForm").addEventListener("submit", handleSend);
  $("sendAmount").addEventListener("input", updateSendSummary);
  $("sendFee").addEventListener("input", updateSendSummary);

  $("refreshBalanceBtn").addEventListener("click", refreshBalance);
  $("refreshHistoryBtn").addEventListener("click", () => refreshHistory(historyPage));
  $("historyPrev").addEventListener("click", () => refreshHistory(Math.max(0, historyPage - 1)));
  $("historyNext").addEventListener("click", () => {
    if (lastHistoryLength === HISTORY_PAGE_SIZE) refreshHistory(historyPage + 1);
  });

  $("copyAddrBtn").addEventListener("click", handleCopyAddr);
  $("qrBtn").addEventListener("click", openQrModal);
  $("qrCloseBtn").addEventListener("click", closeQrModal);
  $("qrModal").addEventListener("click", (e) => {
    if (e.target === $("qrModal")) closeQrModal();
  });

  $("lockBtn").addEventListener("click", lockWallet);
  $("exportBtn").addEventListener("click", handleExport);
  $("deleteBtn").addEventListener("click", handleDelete);

  bindTxTypeSelector();
}

// ──────────────────────────────────────────────────────────────────────
// Boot
// ──────────────────────────────────────────────────────────────────────
async function main() {
  maybeShowInsecureBanner();
  wireUp();
  await loadWasm();
  renderInitial();
}

main().catch((err) => {
  // Last-resort error banner — should never fire.
  pushBanner(
    "err",
    `<p>${t("Wallet failed to initialize: ", "Échec d'initialisation du wallet : ")}${(err && err.message) || err}</p>`
  );
});
