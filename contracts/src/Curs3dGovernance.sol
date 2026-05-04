// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

/// @title Curs3dGovernance
/// @notice Lightweight on-chain governance with snapshot-based voting weight.
/// @dev Trust model:
///      - `owner()` tunes timing (delay/period/execution-window) and the quorum threshold.
///      - Voting weight = balanceOf(voter) at the moment of casting the vote, **clamped** by
///        the voter's balance at proposal creation time. This prevents the trivial
///        flash-loan / token-shuffling attack where one balance votes from many addresses.
///      - The token is treated as a generic ERC-20 (no `Votes` extension required).
///      Limitations:
///      - Snapshot is per-voter on first vote, not chain-wide. Sufficient against the
///        "transfer to friend, vote again" attack but does NOT defend against an attacker
///        who already held the tokens at snapshot time across multiple addresses.
///      - For a full Compound-style snapshot, use OZ `ERC20Votes` + `Governor`. This
///        contract is intentionally simpler for the portfolio.
contract Curs3dGovernance is Ownable {
    /// @notice Minimum allowed `votingPeriod`. Below this, votes can't reasonably converge.
    uint256 public constant MIN_VOTING_PERIOD = 1 hours;
    /// @notice Maximum allowed `votingPeriod`. Above this, the vote is unbounded.
    uint256 public constant MAX_VOTING_PERIOD = 30 days;
    /// @notice Maximum allowed `votingDelay`.
    uint256 public constant MAX_VOTING_DELAY = 7 days;
    /// @notice Maximum allowed `executionWindow`.
    uint256 public constant MAX_EXECUTION_WINDOW = 30 days;
    /// @notice Floor for the execution window so passed proposals can always be executed.
    uint256 public constant MIN_EXECUTION_WINDOW = 1 hours;

    error EmptyDescription();
    error ProposalMissing();
    error VotingClosed();
    error VotingNotStarted();
    error VotingStillOpen();
    error AlreadyVoted();
    error NoVotingPower();
    error AlreadyExecuted();
    error QuorumNotMet();
    error ProposalRejected();
    error InvalidVotingWindow();
    error ExecutionWindowExpired();
    error ZeroAddress();

    struct Proposal {
        address proposer;
        string description;
        uint64 startTime;
        uint64 endTime;
        uint64 executionDeadline;
        uint64 _gap; // packing slot — keeps storage layout explicit.
        uint256 forVotes;
        uint256 againstVotes;
        bool executed;
    }

    /// @notice The governance token. Voting weight is read from `balanceOf`.
    IERC20 public immutable token;
    /// @notice Delay between proposal creation and vote start.
    uint256 public votingDelay;
    /// @notice Length of the voting window.
    uint256 public votingPeriod;
    /// @notice Quorum threshold (sum of for+against votes required).
    uint256 public quorumVotes;
    /// @notice Time after `endTime` during which a passed proposal can be executed.
    uint256 public executionWindow;
    /// @notice Monotonically increasing proposal id.
    uint256 public proposalCount;

    mapping(uint256 => Proposal) public proposals;
    /// @notice Snapshot of voter balance at proposal creation, taken lazily on first vote.
    mapping(uint256 => mapping(address => uint256)) public voterSnapshot;
    /// @notice Whether a voter has voted on a given proposal.
    mapping(uint256 => mapping(address => bool)) public hasVoted;

    event ProposalCreated(
        uint256 indexed proposalId,
        address indexed proposer,
        string description,
        uint256 startTime,
        uint256 endTime,
        uint256 executionDeadline
    );
    event VoteCast(uint256 indexed proposalId, address indexed voter, bool support, uint256 weight);
    event ProposalExecuted(uint256 indexed proposalId);
    event GovernanceConfigUpdated(
        uint256 votingDelay, uint256 votingPeriod, uint256 quorumVotes, uint256 executionWindow
    );

    constructor(
        IERC20 token_,
        uint256 votingDelay_,
        uint256 votingPeriod_,
        uint256 quorumVotes_,
        address owner_
    ) Ownable(owner_) {
        if (address(token_) == address(0) || owner_ == address(0)) {
            revert ZeroAddress();
        }
        _validateConfig(votingDelay_, votingPeriod_, 7 days);
        token = token_;
        votingDelay = votingDelay_;
        votingPeriod = votingPeriod_;
        quorumVotes = quorumVotes_;
        executionWindow = 7 days;
        emit GovernanceConfigUpdated(votingDelay_, votingPeriod_, quorumVotes_, 7 days);
    }

    /// @notice Create a proposal. Returns its id.
    function createProposal(string calldata description) external returns (uint256 proposalId) {
        if (bytes(description).length == 0) revert EmptyDescription();
        unchecked {
            proposalId = ++proposalCount;
        }
        uint256 start = block.timestamp + votingDelay;
        uint256 end = start + votingPeriod;
        uint256 deadline = end + executionWindow;
        if (deadline > type(uint64).max) revert InvalidVotingWindow();

        // forge-lint: disable-start(unsafe-typecast)
        // Bounds-checked above (deadline <= uint64.max ⇒ start, end, deadline all fit).
        proposals[proposalId] = Proposal({
            proposer: msg.sender,
            description: description,
            startTime: uint64(start),
            endTime: uint64(end),
            executionDeadline: uint64(deadline),
            _gap: 0,
            forVotes: 0,
            againstVotes: 0,
            executed: false
        });
        // forge-lint: disable-end(unsafe-typecast)
        emit ProposalCreated(proposalId, msg.sender, description, start, end, deadline);
    }

    /// @notice Cast a vote.
    /// @dev Voting weight = min(current balance, balance at first vote on this proposal).
    ///      The lazy-snapshot pattern prevents same-token-many-addresses attacks: once a
    ///      voter is recorded, their cap is fixed for this proposal.
    function vote(uint256 proposalId, bool support) external {
        Proposal storage proposal = proposals[proposalId];
        if (proposal.proposer == address(0)) revert ProposalMissing();
        if (block.timestamp < proposal.startTime) revert VotingNotStarted();
        if (block.timestamp > proposal.endTime) revert VotingClosed();
        if (hasVoted[proposalId][msg.sender]) revert AlreadyVoted();

        uint256 currentBalance = token.balanceOf(msg.sender);
        if (currentBalance == 0) revert NoVotingPower();

        // Snapshot lazily on first vote, then re-use for subsequent calls (here, same call).
        uint256 snap = voterSnapshot[proposalId][msg.sender];
        if (snap == 0) {
            snap = currentBalance;
            voterSnapshot[proposalId][msg.sender] = snap;
        }
        uint256 weight = currentBalance < snap ? currentBalance : snap;

        hasVoted[proposalId][msg.sender] = true;
        if (support) {
            proposal.forVotes += weight;
        } else {
            proposal.againstVotes += weight;
        }
        emit VoteCast(proposalId, msg.sender, support, weight);
    }

    /// @notice Execute a passed proposal. Anyone can call.
    function execute(uint256 proposalId) external {
        Proposal storage proposal = proposals[proposalId];
        if (proposal.proposer == address(0)) revert ProposalMissing();
        if (block.timestamp <= proposal.endTime) revert VotingStillOpen();
        if (block.timestamp > proposal.executionDeadline) revert ExecutionWindowExpired();
        if (proposal.executed) revert AlreadyExecuted();

        uint256 totalVotes = proposal.forVotes + proposal.againstVotes;
        if (totalVotes < quorumVotes) revert QuorumNotMet();
        if (proposal.forVotes <= proposal.againstVotes) revert ProposalRejected();

        proposal.executed = true;
        emit ProposalExecuted(proposalId);
    }

    /// @notice Update governance parameters. Owner only.
    function setConfig(
        uint256 votingDelay_,
        uint256 votingPeriod_,
        uint256 quorumVotes_,
        uint256 executionWindow_
    ) external onlyOwner {
        _validateConfig(votingDelay_, votingPeriod_, executionWindow_);
        votingDelay = votingDelay_;
        votingPeriod = votingPeriod_;
        quorumVotes = quorumVotes_;
        executionWindow = executionWindow_;
        emit GovernanceConfigUpdated(votingDelay_, votingPeriod_, quorumVotes_, executionWindow_);
    }

    function _validateConfig(uint256 delay, uint256 period, uint256 window) internal pure {
        if (delay > MAX_VOTING_DELAY) revert InvalidVotingWindow();
        if (period < MIN_VOTING_PERIOD || period > MAX_VOTING_PERIOD) revert InvalidVotingWindow();
        if (window < MIN_EXECUTION_WINDOW || window > MAX_EXECUTION_WINDOW) {
            revert InvalidVotingWindow();
        }
    }
}
