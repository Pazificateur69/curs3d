use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

// ─── Governance Constants ───────────────────────────────────────────

pub const VOTING_PERIOD_EPOCHS: u64 = 2;
pub const QUORUM_PERCENT: u64 = 50;
pub const APPROVAL_PERCENT: u64 = 67;
pub const MIN_EXECUTION_DELAY_BLOCKS: u64 = 32;

// ─── Types ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProposalKind {
    ProtocolUpgrade { version: u32, description: String },
    ParameterChange { parameter: String, new_value: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProposalStatus {
    Active,
    Passed,
    Rejected,
    Executed,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum VoteChoice {
    For,
    Against,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub id: Vec<u8>,
    pub proposer: Vec<u8>,
    pub kind: ProposalKind,
    pub status: ProposalStatus,
    pub created_at_height: u64,
    pub voting_deadline_height: u64,
    pub execution_height: Option<u64>,
    pub votes_for: u64,
    pub votes_against: u64,
    pub voters: HashSet<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitProposalParams {
    pub kind: ProposalKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceVoteParams {
    pub proposal_id: Vec<u8>,
    pub vote: VoteChoice,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GovernanceState {
    pub proposals: HashMap<Vec<u8>, Proposal>,
    pub executed_upgrades: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GovernanceError {
    #[error("proposal not found")]
    ProposalNotFound,
    #[error("proposal is not active")]
    ProposalNotActive,
    #[error("already voted on this proposal")]
    AlreadyVoted,
    #[error("invalid proposal parameters")]
    InvalidParams,
    #[error("voting period has ended")]
    VotingEnded,
    #[error("unknown parameter: {0}")]
    UnknownParameter(String),
}

impl GovernanceState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit_proposal(
        &mut self,
        proposer: &[u8],
        params: &SubmitProposalParams,
        current_height: u64,
        epoch_length: u64,
    ) -> Result<Vec<u8>, GovernanceError> {
        // Validate proposal kind
        match &params.kind {
            ProposalKind::ProtocolUpgrade { version, .. } => {
                if *version == 0 {
                    return Err(GovernanceError::InvalidParams);
                }
            }
            ProposalKind::ParameterChange { parameter, .. } => {
                let valid_params = [
                    "block_gas_limit",
                    "minimum_stake",
                    "block_reward",
                    "epoch_length",
                    "unstake_delay_blocks",
                    "jail_duration_blocks",
                ];
                if !valid_params.contains(&parameter.as_str()) {
                    return Err(GovernanceError::UnknownParameter(parameter.clone()));
                }
            }
        }

        // Derive proposal ID
        let mut seed = proposer.to_vec();
        seed.extend_from_slice(&current_height.to_le_bytes());
        seed.extend_from_slice(&bincode::serialize(&params.kind).unwrap_or_default());
        let id = crate::crypto::hash::sha3_hash(&seed);

        let voting_deadline = current_height + (VOTING_PERIOD_EPOCHS * epoch_length);

        let proposal = Proposal {
            id: id.clone(),
            proposer: proposer.to_vec(),
            kind: params.kind.clone(),
            status: ProposalStatus::Active,
            created_at_height: current_height,
            voting_deadline_height: voting_deadline,
            execution_height: None,
            votes_for: 0,
            votes_against: 0,
            voters: HashSet::new(),
        };

        self.proposals.insert(id.clone(), proposal);
        Ok(id)
    }

    pub fn vote(
        &mut self,
        voter: &[u8],
        proposal_id: &[u8],
        choice: &VoteChoice,
        voter_stake: u64,
        current_height: u64,
    ) -> Result<(), GovernanceError> {
        let proposal = self
            .proposals
            .get_mut(proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Active {
            return Err(GovernanceError::ProposalNotActive);
        }

        if current_height > proposal.voting_deadline_height {
            return Err(GovernanceError::VotingEnded);
        }

        if proposal.voters.contains(voter) {
            return Err(GovernanceError::AlreadyVoted);
        }

        proposal.voters.insert(voter.to_vec());
        match choice {
            VoteChoice::For => proposal.votes_for += voter_stake,
            VoteChoice::Against => proposal.votes_against += voter_stake,
        }

        Ok(())
    }

    /// Check proposals at each block height. Returns list of proposals that should be executed.
    pub fn process_block(
        &mut self,
        current_height: u64,
        total_stake: u64,
        epoch_length: u64,
    ) -> Vec<Proposal> {
        let mut to_execute = Vec::new();

        for proposal in self.proposals.values_mut() {
            match proposal.status {
                ProposalStatus::Active => {
                    if current_height > proposal.voting_deadline_height {
                        let total_votes = proposal.votes_for + proposal.votes_against;
                        let quorum_met =
                            total_stake > 0 && total_votes * 100 >= total_stake * QUORUM_PERCENT;
                        let approved = total_votes > 0
                            && proposal.votes_for * 100 >= total_votes * APPROVAL_PERCENT;

                        if quorum_met && approved {
                            proposal.status = ProposalStatus::Passed;
                            proposal.execution_height = Some(
                                proposal
                                    .voting_deadline_height
                                    .saturating_add(MIN_EXECUTION_DELAY_BLOCKS)
                                    .max(
                                        // Align to next epoch boundary
                                        ((proposal.voting_deadline_height / epoch_length) + 1)
                                            * epoch_length,
                                    ),
                            );
                        } else {
                            proposal.status = ProposalStatus::Rejected;
                        }
                    }
                }
                ProposalStatus::Passed => {
                    if let Some(exec_height) = proposal.execution_height
                        && current_height >= exec_height
                    {
                        proposal.status = ProposalStatus::Executed;
                        self.executed_upgrades.push(proposal.id.clone());
                        to_execute.push(proposal.clone());
                    }
                }
                _ => {}
            }
        }

        to_execute
    }

    pub fn get_proposal(&self, id: &[u8]) -> Option<&Proposal> {
        self.proposals.get(id)
    }

    pub fn list_proposals(&self) -> Vec<&Proposal> {
        self.proposals.values().collect()
    }

    #[allow(dead_code)]
    pub fn active_proposals(&self) -> Vec<&Proposal> {
        self.proposals
            .values()
            .filter(|p| p.status == ProposalStatus::Active)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_validator() -> Vec<u8> {
        vec![1u8; 20]
    }

    fn test_validator2() -> Vec<u8> {
        vec![2u8; 20]
    }

    #[test]
    fn test_submit_proposal() {
        let mut gov = GovernanceState::new();
        let params = SubmitProposalParams {
            kind: ProposalKind::ParameterChange {
                parameter: "block_gas_limit".to_string(),
                new_value: 20_000_000,
            },
        };
        let id = gov
            .submit_proposal(&test_validator(), &params, 100, 32)
            .unwrap();
        let proposal = gov.get_proposal(&id).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Active);
        assert_eq!(proposal.voting_deadline_height, 100 + 2 * 32);
    }

    #[test]
    fn test_vote() {
        let mut gov = GovernanceState::new();
        let params = SubmitProposalParams {
            kind: ProposalKind::ParameterChange {
                parameter: "minimum_stake".to_string(),
                new_value: 2_000_000_000,
            },
        };
        let id = gov
            .submit_proposal(&test_validator(), &params, 100, 32)
            .unwrap();
        gov.vote(&test_validator(), &id, &VoteChoice::For, 1000, 100)
            .unwrap();
        let proposal = gov.get_proposal(&id).unwrap();
        assert_eq!(proposal.votes_for, 1000);
    }

    #[test]
    fn test_double_vote_rejected() {
        let mut gov = GovernanceState::new();
        let params = SubmitProposalParams {
            kind: ProposalKind::ParameterChange {
                parameter: "minimum_stake".to_string(),
                new_value: 2_000_000_000,
            },
        };
        let id = gov
            .submit_proposal(&test_validator(), &params, 100, 32)
            .unwrap();
        gov.vote(&test_validator(), &id, &VoteChoice::For, 1000, 100)
            .unwrap();
        let result = gov.vote(&test_validator(), &id, &VoteChoice::Against, 1000, 100);
        assert_eq!(result, Err(GovernanceError::AlreadyVoted));
    }

    #[test]
    fn test_proposal_passes_and_executes() {
        let mut gov = GovernanceState::new();
        let params = SubmitProposalParams {
            kind: ProposalKind::ParameterChange {
                parameter: "block_gas_limit".to_string(),
                new_value: 20_000_000,
            },
        };
        let id = gov
            .submit_proposal(&test_validator(), &params, 100, 32)
            .unwrap();
        // Vote with supermajority
        gov.vote(&test_validator(), &id, &VoteChoice::For, 700, 100)
            .unwrap();
        gov.vote(&test_validator2(), &id, &VoteChoice::Against, 300, 100)
            .unwrap();

        // Process after voting deadline
        let deadline = 100 + 2 * 32;
        let executed = gov.process_block(deadline + 1, 1000, 32);
        assert!(executed.is_empty()); // Passed but not yet at execution height

        let proposal = gov.get_proposal(&id).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Passed);

        // Process at execution height
        let exec_height = proposal.execution_height.unwrap();
        let executed = gov.process_block(exec_height, 1000, 32);
        assert_eq!(executed.len(), 1);
        assert_eq!(
            gov.get_proposal(&id).unwrap().status,
            ProposalStatus::Executed
        );
    }

    #[test]
    fn test_proposal_rejected_no_quorum() {
        let mut gov = GovernanceState::new();
        let params = SubmitProposalParams {
            kind: ProposalKind::ParameterChange {
                parameter: "block_gas_limit".to_string(),
                new_value: 20_000_000,
            },
        };
        let id = gov
            .submit_proposal(&test_validator(), &params, 100, 32)
            .unwrap();
        // Only 10% of stake votes
        gov.vote(&test_validator(), &id, &VoteChoice::For, 100, 100)
            .unwrap();

        let deadline = 100 + 2 * 32;
        let _ = gov.process_block(deadline + 1, 1000, 32);
        assert_eq!(
            gov.get_proposal(&id).unwrap().status,
            ProposalStatus::Rejected
        );
    }

    #[test]
    fn test_proposal_rejected_not_enough_approval() {
        let mut gov = GovernanceState::new();
        let params = SubmitProposalParams {
            kind: ProposalKind::ParameterChange {
                parameter: "block_gas_limit".to_string(),
                new_value: 20_000_000,
            },
        };
        let id = gov
            .submit_proposal(&test_validator(), &params, 100, 32)
            .unwrap();
        // 50/50 split (needs 67%)
        gov.vote(&test_validator(), &id, &VoteChoice::For, 500, 100)
            .unwrap();
        gov.vote(&test_validator2(), &id, &VoteChoice::Against, 500, 100)
            .unwrap();

        let deadline = 100 + 2 * 32;
        let _ = gov.process_block(deadline + 1, 1000, 32);
        assert_eq!(
            gov.get_proposal(&id).unwrap().status,
            ProposalStatus::Rejected
        );
    }

    #[test]
    fn test_invalid_parameter() {
        let mut gov = GovernanceState::new();
        let params = SubmitProposalParams {
            kind: ProposalKind::ParameterChange {
                parameter: "invalid_param".to_string(),
                new_value: 42,
            },
        };
        let result = gov.submit_proposal(&test_validator(), &params, 100, 32);
        assert!(matches!(result, Err(GovernanceError::UnknownParameter(_))));
    }

    #[test]
    fn test_vote_after_deadline() {
        let mut gov = GovernanceState::new();
        let params = SubmitProposalParams {
            kind: ProposalKind::ProtocolUpgrade {
                version: 2,
                description: "v2 upgrade".to_string(),
            },
        };
        let id = gov
            .submit_proposal(&test_validator(), &params, 100, 32)
            .unwrap();
        let deadline = 100 + 2 * 32;
        let result = gov.vote(&test_validator(), &id, &VoteChoice::For, 1000, deadline + 1);
        assert_eq!(result, Err(GovernanceError::VotingEnded));
    }
}
