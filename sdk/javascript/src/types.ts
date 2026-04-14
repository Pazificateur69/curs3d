// CURS3D SDK Types

export interface ApiResponse<T> {
  ok: boolean;
  data?: T;
  error?: string;
}

export interface ChainStatus {
  chain_id: string;
  chain_name: string;
  height: number;
  finalized_height: number;
  epoch: number;
  epoch_start_height: number;
  pending_transactions: number;
  active_validators: number;
  genesis_hash: string;
  protocol_version: number;
}

export interface Block {
  height: number;
  hash: string;
  prev_hash: string;
  timestamp: number;
  validator: string;
  tx_count: number;
  gas_used: number;
  base_fee_per_gas: number;
  transactions: Transaction[];
}

export interface BlockSummary {
  height: number;
  hash: string;
  timestamp: number;
  validator: string;
  tx_count: number;
}

export interface Account {
  address: string;
  balance: number;
  nonce: number;
  staked_balance: number;
}

export interface Transaction {
  hash: string;
  kind: string;
  from: string;
  to: string;
  amount: number;
  fee: number;
  max_fee_per_gas: number;
  max_priority_fee_per_gas: number;
  gas_limit: number;
  nonce: number;
}

export interface Receipt {
  tx_hash: string;
  success: boolean;
  gas_used: number;
  effective_gas_price: number;
  priority_fee_paid: number;
  base_fee_burned: number;
  gas_refunded: number;
  logs: LogEntry[];
  return_data: string;
  contract_address?: string;
}

export interface LogEntry {
  contract: string;
  topics: string[];
  data: string;
}

export interface Validator {
  address: string;
  public_key: string;
  stake: number;
}

export interface CUR20Token {
  address: string;
  name: string;
  symbol: string;
  decimals: number;
  total_supply: number;
  creator: string;
  created_at_height: number;
}

export interface Proposal {
  id: string;
  proposer: string;
  kind: string;
  status: string;
  created_at_height: number;
  voting_deadline_height: number;
  execution_height?: number;
  votes_for: number;
  votes_against: number;
  voter_count: number;
}

export interface WsEvent {
  type: string;
  data: Record<string, unknown>;
}

export interface WsSubscription {
  events: string[];
}

export interface TransactionEstimate {
  next_block_height: number;
  base_fee_per_gas: number;
  gas_used: number;
  effective_gas_price: number;
  priority_fee_paid: number;
  base_fee_burned: number;
  total_fee_charged: number;
  gas_refunded: number;
  max_total_fee: number;
  would_replace_pending: boolean;
}
