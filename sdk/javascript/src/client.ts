import type {
  Account,
  ApiResponse,
  Block,
  BlockSummary,
  CUR20Token,
  ChainStatus,
  Proposal,
  Receipt,
  Transaction,
  TransactionEstimate,
  Validator,
  WsEvent,
  WsSubscription,
} from "./types";

export class CursClient {
  private baseUrl: string;
  private ws: WebSocket | null = null;
  private wsListeners: Map<string, Set<(event: WsEvent) => void>> = new Map();

  constructor(nodeUrl: string) {
    this.baseUrl = nodeUrl.replace(/\/$/, "");
  }

  // ─── HTTP API ──────────────────────────────────────────────────

  private async request<T>(path: string, options?: RequestInit): Promise<T> {
    const url = `${this.baseUrl}${path}`;
    const response = await fetch(url, options);
    const json: ApiResponse<T> = await response.json();
    if (!json.ok || json.data === undefined) {
      throw new Error(json.error || "Unknown API error");
    }
    return json.data;
  }

  async getStatus(): Promise<ChainStatus> {
    return this.request<ChainStatus>("/api/status");
  }

  async getHealth(): Promise<Record<string, unknown>> {
    return this.request("/api/healthz");
  }

  async getBlock(height: number): Promise<Block> {
    return this.request<Block>(`/api/block/${height}`);
  }

  async getBlocks(from?: number, limit?: number): Promise<BlockSummary[]> {
    const params = new URLSearchParams();
    if (from !== undefined) params.set("from", from.toString());
    if (limit !== undefined) params.set("limit", limit.toString());
    const query = params.toString();
    return this.request<BlockSummary[]>(
      `/api/blocks${query ? `?${query}` : ""}`
    );
  }

  async getAccount(address: string): Promise<Account> {
    return this.request<Account>(`/api/account/${address}`);
  }

  async getTransaction(txHash: string): Promise<Transaction> {
    return this.request<Transaction>(`/api/tx/${txHash}`);
  }

  async getReceipt(txHash: string): Promise<Receipt> {
    return this.request<Receipt>(`/api/receipt/${txHash}`);
  }

  async getValidators(): Promise<Validator[]> {
    return this.request<Validator[]>("/api/validators");
  }

  async getPending(): Promise<Transaction[]> {
    return this.request<Transaction[]>("/api/pending");
  }

  async requestFaucet(address: string): Promise<Record<string, unknown>> {
    return this.request("/api/faucet/request", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ address }),
    });
  }

  async submitTransaction(
    signedTx: Record<string, unknown>
  ): Promise<Record<string, unknown>> {
    return this.request("/api/tx/submit", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(signedTx),
    });
  }

  async estimateGas(tx: Record<string, unknown>): Promise<TransactionEstimate> {
    return this.request<TransactionEstimate>("/api/tx/estimate", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(tx),
    });
  }

  // ─── CUR-20 Tokens ────────────────────────────────────────────

  async getTokens(): Promise<CUR20Token[]> {
    return this.request<CUR20Token[]>("/api/tokens");
  }

  async getToken(tokenAddress: string): Promise<CUR20Token> {
    return this.request<CUR20Token>(`/api/token/${tokenAddress}`);
  }

  async getTokenBalance(
    tokenAddress: string,
    ownerAddress: string
  ): Promise<{ balance: number }> {
    return this.request(`/api/token/${tokenAddress}/balance/${ownerAddress}`);
  }

  // ─── Governance ────────────────────────────────────────────────

  async getProposals(): Promise<Proposal[]> {
    return this.request<Proposal[]>("/api/governance/proposals");
  }

  async getProposal(proposalId: string): Promise<Proposal> {
    return this.request<Proposal>(`/api/governance/proposal/${proposalId}`);
  }

  // ─── WebSocket ─────────────────────────────────────────────────

  subscribe(
    events: string[],
    callback: (event: WsEvent) => void
  ): { unsubscribe: () => void } {
    const wsUrl = this.baseUrl.replace(/^http/, "ws") + "/ws";

    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      this.ws = new WebSocket(wsUrl);

      this.ws.onopen = () => {
        const sub: WsSubscription = { events };
        this.ws!.send(JSON.stringify(sub));
      };

      this.ws.onmessage = (msg) => {
        try {
          const event: WsEvent = JSON.parse(msg.data);
          const listeners = this.wsListeners.get(event.type);
          if (listeners) {
            for (const listener of listeners) {
              listener(event);
            }
          }
          // Also notify "all" subscribers
          const allListeners = this.wsListeners.get("*");
          if (allListeners) {
            for (const listener of allListeners) {
              listener(event);
            }
          }
        } catch {
          // Ignore non-JSON messages
        }
      };

      this.ws.onclose = () => {
        this.ws = null;
      };
    }

    // Register listeners
    for (const eventType of events) {
      if (!this.wsListeners.has(eventType)) {
        this.wsListeners.set(eventType, new Set());
      }
      this.wsListeners.get(eventType)!.add(callback);
    }
    if (events.length === 0) {
      if (!this.wsListeners.has("*")) {
        this.wsListeners.set("*", new Set());
      }
      this.wsListeners.get("*")!.add(callback);
    }

    return {
      unsubscribe: () => {
        for (const eventType of events.length === 0 ? ["*"] : events) {
          this.wsListeners.get(eventType)?.delete(callback);
        }
      },
    };
  }

  disconnect(): void {
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
    this.wsListeners.clear();
  }
}
