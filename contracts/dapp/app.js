/* CURS3D Solidity Portfolio dApp — front end logic.
 *
 * Design notes for reviewers:
 *  - No `innerHTML` with user-controlled data anywhere. Every dynamic node is built via
 *    `createElement` + `textContent`, so an attacker who controls a contract event payload
 *    cannot inject HTML.
 *  - No `eval()`, no `new Function()`, no inline `onclick` — everything wires up through
 *    `addEventListener` so a strict CSP can be enforced server-side.
 *  - Wallet UX: pending tx spinner + toast notifications + persisted account reconnect.
 *  - All numbers go through ethers.formatUnits / parseUnits — no manual decimal math.
 *  - Addresses are EIP-55 checksummed via ethers.getAddress before display.
 */

const NETWORKS = {
  curs3d: {
    chainIdNum: 1800329576,
    chainId: "0x6b4ed968",
    chainName: "CURS3D Public Testnet",
    explorer: "https://explorer.curs3d.fr/tx/",
    nativeCurrency: {name: "CURS3D", symbol: "CUR", decimals: 18},
    rpcUrls: ["https://rpc.curs3d.fr/eth"],
    blockExplorerUrls: ["https://explorer.curs3d.fr"]
  },
  sepolia: {
    chainIdNum: 11155111,
    chainId: "0xaa36a7",
    chainName: "Sepolia",
    explorer: "https://sepolia.etherscan.io/tx/",
    nativeCurrency: {name: "Sepolia ETH", symbol: "ETH", decimals: 18},
    rpcUrls: ["https://rpc.sepolia.org"],
    blockExplorerUrls: ["https://sepolia.etherscan.io"]
  },
  baseSepolia: {
    chainIdNum: 84532,
    chainId: "0x14a34",
    chainName: "Base Sepolia",
    explorer: "https://sepolia.basescan.org/tx/",
    nativeCurrency: {name: "Base Sepolia ETH", symbol: "ETH", decimals: 18},
    rpcUrls: ["https://sepolia.base.org"],
    blockExplorerUrls: ["https://sepolia.basescan.org"]
  }
};

const TOKEN_ABI = [
  "function balanceOf(address account) view returns (uint256)",
  "function decimals() view returns (uint8)",
  "function symbol() view returns (string)",
  "function approve(address spender, uint256 amount) returns (bool)"
];

const FAUCET_ABI = ["function claim()"];

const STAKING_ABI = [
  "function stake(uint256 amount)",
  "function claimRewards() returns (uint256)"
];

const GOVERNANCE_ABI = [
  "event ProposalCreated(uint256 indexed proposalId, address indexed proposer, string description, uint256 startTime, uint256 endTime, uint256 executionDeadline)",
  "function createProposal(string description) returns (uint256)",
  "function vote(uint256 proposalId, bool support)",
  "function execute(uint256 proposalId)"
];

const ATTESTATIONS_ABI = [
  "event AttestationIssued(bytes32 indexed attestationId, address indexed issuer, address indexed subject, bytes32 dataHash, string uri)",
  "function issue(address subject, bytes32 dataHash, string uri) returns (bytes32)",
  "function attestations(bytes32 attestationId) view returns (address issuer, address subject, bytes32 dataHash, string uri, uint64 issuedAt, bool revoked)"
];

const ESCROW_ABI = [
  "event Listed(uint256 indexed listingId, address indexed seller, bytes32 itemHash, string uri, uint256 price)",
  "function list(bytes32 itemHash, string uri, uint256 price, uint64 lockWindow) returns (uint256)",
  "function buy(uint256 listingId) payable"
];

const STORAGE_KEYS = {
  account: "curs3d-solidity-account",
  addresses: "curs3d-solidity-addresses",
  network: "curs3d-solidity-network"
};

let provider;
let signer;
let account;
let currentNetworkKey = "curs3d";
let currentExplorer = NETWORKS.curs3d.explorer;

const $ = (id) => document.getElementById(id);

// ─── UI helpers ──────────────────────────────────────────────────────────────

function setText(id, value, className = "") {
  const el = $(id);
  if (!el) return;
  el.textContent = value;
  el.className = className;
}

/** Show a toast. `tone` is "ok" | "warn" | "error". role="status" + aria-live="polite"
 *  is set on the element itself in HTML so screen readers announce it. */
function showToast(message, tone = "ok") {
  const toast = $("toast");
  if (!toast) return;
  toast.textContent = message;
  toast.classList.remove("show", "ok", "warn", "error");
  toast.classList.add("show", tone);
  // Click to dismiss.
  toast.onclick = () => toast.classList.remove("show");
  window.clearTimeout(showToast.timeout);
  showToast.timeout = window.setTimeout(() => {
    toast.classList.remove("show");
  }, 4500);
}

/** Always display addresses in EIP-55 checksum form. */
function displayAddress(address) {
  try {
    return ethers.getAddress(address);
  } catch {
    return address;
  }
}

// ─── Address book persistence ────────────────────────────────────────────────

function getAddresses() {
  return {
    token: $("tokenAddress").value.trim(),
    faucet: $("faucetAddress").value.trim(),
    attestations: $("attestationsAddress").value.trim(),
    staking: $("stakingAddress").value.trim(),
    governance: $("governanceAddress").value.trim(),
    escrow: $("escrowAddress").value.trim()
  };
}

function saveAddresses() {
  localStorage.setItem(STORAGE_KEYS.addresses, JSON.stringify(getAddresses()));
}

function loadAddresses() {
  const raw = localStorage.getItem(STORAGE_KEYS.addresses);
  if (!raw) return;
  let addresses;
  try {
    addresses = JSON.parse(raw);
  } catch {
    return; // Corrupt entry — silently ignore.
  }
  $("tokenAddress").value = addresses.token || "";
  $("faucetAddress").value = addresses.faucet || "";
  $("attestationsAddress").value = addresses.attestations || "";
  $("stakingAddress").value = addresses.staking || "";
  $("governanceAddress").value = addresses.governance || "";
  $("escrowAddress").value = addresses.escrow || "";
}

/**
 * Auto-load addresses from /dapp/deployments.json (written by contracts/deploy.sh).
 * Runs at boot — local saves still win when present and non-empty so a user can
 * pin to specific addresses by hitting "Save". Silent on missing file (typical
 * when the dApp is opened before any deploy has happened yet).
 */
async function autoLoadDeploymentsFile() {
  try {
    const res = await fetch("./deployments.json", {cache: "no-store"});
    if (!res.ok) return;
    const data = await res.json();
    const filled = (id) => $(id) && $(id).value.trim().length > 0;
    if (!filled("tokenAddress")) $("tokenAddress").value = data.token || "";
    if (!filled("faucetAddress")) $("faucetAddress").value = data.faucet || "";
    if (!filled("attestationsAddress")) $("attestationsAddress").value = data.attestations || "";
    if (!filled("stakingAddress")) $("stakingAddress").value = data.staking || "";
    if (!filled("governanceAddress")) $("governanceAddress").value = data.governance || "";
    if (!filled("escrowAddress")) $("escrowAddress").value = data.escrow || "";
  } catch {
    /* deployments.json not served — fall back to manual entry */
  }
}

function loadDeploymentJson() {
  const raw = $("deploymentJsonInput").value.trim();
  if (!raw) throw new Error("Deployment JSON is empty");
  let deployment;
  try {
    deployment = JSON.parse(raw);
  } catch (e) {
    throw new Error("Deployment JSON is not valid JSON");
  }
  $("tokenAddress").value = deployment.token || "";
  $("faucetAddress").value = deployment.faucet || "";
  $("attestationsAddress").value = deployment.attestations || "";
  $("stakingAddress").value = deployment.staking || "";
  $("governanceAddress").value = deployment.governance || "";
  $("escrowAddress").value = deployment.escrow || "";
  saveAddresses();
  showToast("Deployment loaded", "ok");
}

function requireAddress(address, label) {
  if (!ethers.isAddress(address)) {
    throw new Error(`${label} address is missing or invalid`);
  }
}

// ─── Wallet connect / network ───────────────────────────────────────────────

async function connect() {
  if (!window.ethereum) throw new Error("No browser wallet found (install MetaMask)");
  provider = new ethers.BrowserProvider(window.ethereum);
  await provider.send("eth_requestAccounts", []);
  signer = await provider.getSigner();
  account = await signer.getAddress();
  localStorage.setItem(STORAGE_KEYS.account, account);
  setText("walletStatus", displayAddress(account), "ok");
  await refreshNetwork();
  await refreshBalance();
  showToast("Wallet connected", "ok");
}

/** Reconnect silently on page load if the wallet is still authorised. */
async function tryReconnect() {
  if (!window.ethereum) return;
  const persisted = localStorage.getItem(STORAGE_KEYS.account);
  if (!persisted) return;
  try {
    provider = new ethers.BrowserProvider(window.ethereum);
    const accounts = await provider.send("eth_accounts", []); // does NOT prompt
    if (!accounts || accounts.length === 0) return;
    signer = await provider.getSigner();
    account = await signer.getAddress();
    setText("walletStatus", displayAddress(account), "ok");
    await refreshNetwork();
    await refreshBalance();
  } catch (err) {
    console.warn("Silent reconnect failed:", err);
  }
}

async function refreshNetwork() {
  if (!provider) return;
  const network = await provider.getNetwork();
  const chainId = Number(network.chainId);
  if (chainId === NETWORKS.curs3d.chainIdNum) {
    currentNetworkKey = "curs3d";
    currentExplorer = NETWORKS.curs3d.explorer;
    setText("networkStatus", "CURS3D Testnet", "ok");
  } else if (chainId === NETWORKS.sepolia.chainIdNum) {
    currentNetworkKey = "sepolia";
    currentExplorer = NETWORKS.sepolia.explorer;
    setText("networkStatus", "Sepolia", "ok");
  } else if (chainId === NETWORKS.baseSepolia.chainIdNum) {
    currentNetworkKey = "baseSepolia";
    currentExplorer = NETWORKS.baseSepolia.explorer;
    setText("networkStatus", "Base Sepolia", "ok");
  } else {
    setText("networkStatus", `Unsupported (${chainId})`, "warn");
  }
  localStorage.setItem(STORAGE_KEYS.network, currentNetworkKey);
}

async function switchNetwork(networkKey) {
  if (!window.ethereum) throw new Error("No browser wallet found");
  const network = NETWORKS[networkKey];
  if (!network) throw new Error("Unknown network: " + networkKey);
  try {
    await window.ethereum.request({
      method: "wallet_switchEthereumChain",
      params: [{chainId: network.chainId}]
    });
  } catch (error) {
    // 4902 = chain not added. We add it then re-request the switch.
    if (error.code !== 4902) throw error;
    await window.ethereum.request({
      method: "wallet_addEthereumChain",
      params: [
        {
          chainId: network.chainId,
          chainName: network.chainName,
          nativeCurrency: network.nativeCurrency,
          rpcUrls: network.rpcUrls,
          blockExplorerUrls: network.blockExplorerUrls
        }
      ]
    });
  }
  await connect();
}

async function refreshBalance() {
  if (!signer || !account) return;
  const {token} = getAddresses();
  if (!ethers.isAddress(token)) {
    setText("balanceStatus", "Token address missing", "warn");
    return;
  }
  try {
    const contract = new ethers.Contract(token, TOKEN_ABI, provider);
    const [balance, decimals, symbol] = await Promise.all([
      contract.balanceOf(account),
      contract.decimals(),
      contract.symbol()
    ]);
    setText("balanceStatus", `${ethers.formatUnits(balance, decimals)} ${symbol}`, "ok");
  } catch (err) {
    console.error(err);
    setText("balanceStatus", "Read failed", "warn");
  }
}

// ─── Tx tracking ────────────────────────────────────────────────────────────

function addTx(label, hash) {
  const txList = $("txList");
  if (!txList) return;
  const empty = txList.querySelector(".empty-state");
  if (empty) empty.remove();

  const li = document.createElement("li");
  const span = document.createElement("span");
  span.textContent = label;

  const link = document.createElement("a");
  link.href = `${currentExplorer}${hash}`;
  link.target = "_blank";
  link.rel = "noreferrer noopener";
  link.textContent = `${hash.slice(0, 10)}...${hash.slice(-8)}`;

  const status = document.createElement("span");
  status.textContent = "pending";
  status.className = "tx-status pending";
  li.dataset.hash = hash;

  li.append(span, link, status);
  txList.prepend(li);
  return li;
}

function markTx(li, ok) {
  if (!li) return;
  const status = li.querySelector(".tx-status");
  if (!status) return;
  status.textContent = ok ? "ok" : "failed";
  status.className = `tx-status ${ok ? "ok" : "failed"}`;
}

// ─── Action runner with spinner + toast ─────────────────────────────────────

async function runAction(action, button, label) {
  if (!button) return;
  const original = button.textContent;
  try {
    button.disabled = true;
    button.classList.add("busy");
    button.textContent = label ? `${label}…` : "Submitting…";
    await action();
  } catch (error) {
    console.error(error);
    const msg = error.shortMessage || error.reason || error.message || String(error);
    showToast(msg, "error");
  } finally {
    button.disabled = false;
    button.classList.remove("busy");
    button.textContent = original;
  }
}

async function withTx(label, txPromiseFn) {
  let li;
  try {
    const tx = await txPromiseFn();
    li = addTx(label, tx.hash);
    const receipt = await tx.wait();
    markTx(li, receipt.status === 1);
    return receipt;
  } catch (e) {
    if (li) markTx(li, false);
    throw e;
  }
}

// ─── Contract actions ───────────────────────────────────────────────────────

async function claim() {
  const {faucet} = getAddresses();
  requireAddress(faucet, "Faucet");
  const contract = new ethers.Contract(faucet, FAUCET_ABI, signer);
  await withTx("Faucet claim", () => contract.claim());
  await refreshBalance();
}

async function approveStake() {
  const {token, staking} = getAddresses();
  requireAddress(token, "Token");
  requireAddress(staking, "Staking");
  const amount = ethers.parseUnits($("stakeAmountInput").value || "0", 18);
  if (amount === 0n) throw new Error("Amount must be > 0");
  const contract = new ethers.Contract(token, TOKEN_ABI, signer);
  await withTx("Approve staking", () => contract.approve(staking, amount));
}

async function stake() {
  const {staking} = getAddresses();
  requireAddress(staking, "Staking");
  const amount = ethers.parseUnits($("stakeAmountInput").value || "0", 18);
  if (amount === 0n) throw new Error("Amount must be > 0");
  const contract = new ethers.Contract(staking, STAKING_ABI, signer);
  await withTx("Stake tokens", () => contract.stake(amount));
  await refreshBalance();
}

async function claimRewards() {
  const {staking} = getAddresses();
  requireAddress(staking, "Staking");
  const contract = new ethers.Contract(staking, STAKING_ABI, signer);
  await withTx("Claim staking rewards", () => contract.claimRewards());
  await refreshBalance();
}

async function issueAttestation() {
  const {attestations} = getAddresses();
  requireAddress(attestations, "Attestations");
  const subject = $("subjectInput").value.trim();
  requireAddress(subject, "Subject");
  const data = $("dataInput").value.trim();
  const uri = $("uriInput").value.trim();
  if (!data) throw new Error("Data to hash is required");

  const dataHash = ethers.keccak256(ethers.toUtf8Bytes(data));
  const contract = new ethers.Contract(attestations, ATTESTATIONS_ABI, signer);
  const receipt = await withTx("Issue attestation", () => contract.issue(subject, dataHash, uri));

  const iface = new ethers.Interface(ATTESTATIONS_ABI);
  for (const log of receipt.logs) {
    try {
      const parsed = iface.parseLog(log);
      if (parsed && parsed.name === "AttestationIssued") {
        $("attestationIdInput").value = parsed.args.attestationId;
        await readAttestation();
        break;
      }
    } catch {
      /* ignore foreign logs */
    }
  }
}

async function readAttestation() {
  const {attestations} = getAddresses();
  requireAddress(attestations, "Attestations");
  const attestationId = $("attestationIdInput").value.trim();
  if (!ethers.isHexString(attestationId, 32)) {
    throw new Error("Attestation id must be a bytes32 hex value");
  }
  const contract = new ethers.Contract(attestations, ATTESTATIONS_ABI, provider);
  const result = await contract.attestations(attestationId);
  $("attestationOutput").textContent = JSON.stringify(
    {
      issuer: displayAddress(result.issuer),
      subject: displayAddress(result.subject),
      dataHash: result.dataHash,
      uri: result.uri,
      issuedAt: Number(result.issuedAt),
      revoked: result.revoked
    },
    null,
    2
  );
}

async function createProposal() {
  const {governance} = getAddresses();
  requireAddress(governance, "Governance");
  const description = $("proposalInput").value.trim();
  if (!description) throw new Error("Proposal description is required");
  const contract = new ethers.Contract(governance, GOVERNANCE_ABI, signer);
  const receipt = await withTx("Create proposal", () => contract.createProposal(description));
  const iface = new ethers.Interface(GOVERNANCE_ABI);
  for (const log of receipt.logs) {
    try {
      const parsed = iface.parseLog(log);
      if (parsed && parsed.name === "ProposalCreated") {
        $("proposalIdInput").value = parsed.args.proposalId.toString();
        break;
      }
    } catch {
      /* ignore */
    }
  }
}

async function voteForProposal() {
  const {governance} = getAddresses();
  requireAddress(governance, "Governance");
  const proposalId = BigInt($("proposalIdInput").value || "0");
  if (proposalId === 0n) throw new Error("Proposal id is required");
  const contract = new ethers.Contract(governance, GOVERNANCE_ABI, signer);
  await withTx("Vote for proposal", () => contract.vote(proposalId, true));
}

async function executeProposal() {
  const {governance} = getAddresses();
  requireAddress(governance, "Governance");
  const proposalId = BigInt($("proposalIdInput").value || "0");
  if (proposalId === 0n) throw new Error("Proposal id is required");
  const contract = new ethers.Contract(governance, GOVERNANCE_ABI, signer);
  await withTx("Execute proposal", () => contract.execute(proposalId));
}

async function listEscrowItem() {
  const {escrow} = getAddresses();
  requireAddress(escrow, "Escrow");
  const item = $("escrowItemInput").value.trim();
  const uri = $("escrowUriInput").value.trim();
  const price = ethers.parseEther($("escrowPriceInput").value || "0");
  if (!item) throw new Error("Item text is required");
  if (price === 0n) throw new Error("Price must be greater than zero");
  const itemHash = ethers.keccak256(ethers.toUtf8Bytes(item));
  const contract = new ethers.Contract(escrow, ESCROW_ABI, signer);
  // lockWindow = 0 → contract uses DEFAULT_LOCK_WINDOW (7 days).
  const receipt = await withTx("List escrow item", () => contract.list(itemHash, uri, price, 0));
  const iface = new ethers.Interface(ESCROW_ABI);
  for (const log of receipt.logs) {
    try {
      const parsed = iface.parseLog(log);
      if (parsed && parsed.name === "Listed") {
        $("listingIdInput").value = parsed.args.listingId.toString();
        break;
      }
    } catch {
      /* ignore */
    }
  }
}

async function buyEscrowItem() {
  const {escrow} = getAddresses();
  requireAddress(escrow, "Escrow");
  const listingId = BigInt($("listingIdInput").value || "0");
  const price = ethers.parseEther($("escrowPriceInput").value || "0");
  if (listingId === 0n) throw new Error("Listing id is required");
  if (price === 0n) throw new Error("Price is required");
  const contract = new ethers.Contract(escrow, ESCROW_ABI, signer);
  await withTx("Buy escrow item", () => contract.buy(listingId, {value: price}));
}

// ─── Wire up ────────────────────────────────────────────────────────────────

loadAddresses();
autoLoadDeploymentsFile();
tryReconnect();

$("connectBtn").addEventListener("click", (e) => runAction(connect, e.currentTarget, "Connecting"));
const curs3dBtn = $("curs3dBtn");
if (curs3dBtn) {
  curs3dBtn.addEventListener("click", (e) =>
    runAction(() => switchNetwork("curs3d"), e.currentTarget, "Switching")
  );
}
$("sepoliaBtn").addEventListener("click", (e) =>
  runAction(() => switchNetwork("sepolia"), e.currentTarget, "Switching")
);
$("baseSepoliaBtn").addEventListener("click", (e) =>
  runAction(() => switchNetwork("baseSepolia"), e.currentTarget, "Switching")
);
$("saveAddressesBtn").addEventListener("click", () => {
  saveAddresses();
  showToast("Addresses saved", "ok");
});
$("loadDeploymentBtn").addEventListener("click", (e) =>
  runAction(loadDeploymentJson, e.currentTarget, "Loading")
);
$("refreshBtn").addEventListener("click", (e) =>
  runAction(refreshBalance, e.currentTarget, "Refreshing")
);
$("claimBtn").addEventListener("click", (e) => runAction(claim, e.currentTarget, "Claiming"));
$("issueBtn").addEventListener("click", (e) =>
  runAction(issueAttestation, e.currentTarget, "Issuing")
);
$("readBtn").addEventListener("click", (e) =>
  runAction(readAttestation, e.currentTarget, "Reading")
);
$("approveStakeBtn").addEventListener("click", (e) =>
  runAction(approveStake, e.currentTarget, "Approving")
);
$("stakeBtn").addEventListener("click", (e) => runAction(stake, e.currentTarget, "Staking"));
$("claimRewardsBtn").addEventListener("click", (e) =>
  runAction(claimRewards, e.currentTarget, "Claiming")
);
$("createProposalBtn").addEventListener("click", (e) =>
  runAction(createProposal, e.currentTarget, "Creating")
);
$("voteForBtn").addEventListener("click", (e) =>
  runAction(voteForProposal, e.currentTarget, "Voting")
);
$("executeProposalBtn").addEventListener("click", (e) =>
  runAction(executeProposal, e.currentTarget, "Executing")
);
$("listEscrowBtn").addEventListener("click", (e) =>
  runAction(listEscrowItem, e.currentTarget, "Listing")
);
$("buyEscrowBtn").addEventListener("click", (e) =>
  runAction(buyEscrowItem, e.currentTarget, "Buying")
);

if (window.ethereum) {
  window.ethereum.on("accountsChanged", (accounts) => {
    if (accounts.length === 0) {
      account = undefined;
      signer = undefined;
      localStorage.removeItem(STORAGE_KEYS.account);
      setText("walletStatus", "Not connected", "");
    } else {
      connect().catch((e) => showToast(e.message || "Reconnect failed", "error"));
    }
  });
  window.ethereum.on("chainChanged", () => {
    // EIP-1193 best practice: just reload after a chain switch.
    window.location.reload();
  });
}
