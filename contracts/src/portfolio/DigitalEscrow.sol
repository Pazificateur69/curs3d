// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Address} from "@openzeppelin/contracts/utils/Address.sol";

/// @title DigitalEscrow
/// @notice Off-chain-asset escrow with a structured state machine and a single trusted
///         arbitrator for disputes.
/// @dev State machine:
///         Listed  --buyer pays-->  Funded
///         Listed  --seller cancels-->  Cancelled
///         Funded  --buyer cancels (before sellerAccepted)-->  RefundedToBuyer
///         Funded  --seller accepts-->  Accepted (still escrowed)
///         Accepted --buyer releases | timeoutAt elapses-->  PaidToSeller
///         Accepted --either disputes-->  Disputed
///         Disputed --arbitrator rules-->  PaidToSeller | RefundedToBuyer
///
///      Trust model:
///        - The `arbitrator` (set at construction by the contract owner) is the only
///          actor who can resolve disputes. A multisig or DAO is recommended.
///        - The contract `owner()` can pause new listings/buys and rotate the arbitrator.
///        - Funds are held by this contract until a terminal transition.
contract DigitalEscrow is Ownable, Pausable, ReentrancyGuard {
    using Address for address payable;

    /// @notice Default lock window after seller acceptance during which the buyer must
    ///         release or dispute. After it elapses anyone can finalise to the seller.
    uint256 public constant DEFAULT_LOCK_WINDOW = 7 days;
    /// @notice Hard ceiling on per-listing lock window.
    uint256 public constant MAX_LOCK_WINDOW = 60 days;

    enum Status {
        None, // 0 — id not used
        Listed, // 1 — created, no buyer
        Funded, // 2 — buyer paid, seller has not accepted
        Accepted, // 3 — seller accepted, lock window running
        Disputed, // 4 — buyer or seller raised dispute
        PaidToSeller, // 5 — terminal, seller got the money
        RefundedToBuyer, // 6 — terminal, buyer got their money back
        Cancelled // 7 — terminal, seller cancelled an unfunded listing
    }

    error InvalidPrice();
    error ListingMissing();
    error WrongState();
    error WrongPayment();
    error NotSeller();
    error NotBuyer();
    error NotArbitrator();
    error LockWindowTooHigh();
    error LockNotElapsed();
    error ZeroAddress();

    struct Listing {
        address seller;
        address buyer;
        bytes32 itemHash;
        string uri;
        uint256 price;
        uint64 fundedAt;
        uint64 acceptedAt;
        uint64 lockWindow;
        Status status;
    }

    /// @notice Designated arbitrator for disputes.
    address public arbitrator;
    /// @notice Number of listings ever created.
    uint256 public listingCount;
    /// @notice Listings, by id.
    mapping(uint256 => Listing) public listings;

    event Listed(
        uint256 indexed listingId,
        address indexed seller,
        bytes32 itemHash,
        string uri,
        uint256 price
    );
    event Funded(uint256 indexed listingId, address indexed buyer, uint256 price);
    event Accepted(uint256 indexed listingId, uint256 lockUntil);
    event PaidOut(
        uint256 indexed listingId, address indexed recipient, uint256 amount, Status finalStatus
    );
    event Disputed(uint256 indexed listingId, address indexed by);
    event Cancelled(uint256 indexed listingId, Status reason);
    event ArbitratorRotated(address indexed previousArbitrator, address indexed newArbitrator);

    /// @param owner_      Initial contract owner.
    /// @param arbitrator_ Initial arbitrator. May be a multisig.
    constructor(address owner_, address arbitrator_) Ownable(owner_) {
        if (owner_ == address(0) || arbitrator_ == address(0)) revert ZeroAddress();
        arbitrator = arbitrator_;
        emit ArbitratorRotated(address(0), arbitrator_);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Seller flow
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Create a listing. Caller becomes the seller.
    function list(bytes32 itemHash, string calldata uri, uint256 price, uint64 lockWindow)
        external
        whenNotPaused
        returns (uint256 listingId)
    {
        if (price == 0) revert InvalidPrice();
        if (lockWindow > MAX_LOCK_WINDOW) revert LockWindowTooHigh();
        // DEFAULT_LOCK_WINDOW is a hardcoded `7 days` constant, well within uint64.
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 effectiveLock = lockWindow == 0 ? uint64(DEFAULT_LOCK_WINDOW) : lockWindow;

        unchecked {
            listingId = ++listingCount;
        }
        listings[listingId] = Listing({
            seller: msg.sender,
            buyer: address(0),
            itemHash: itemHash,
            uri: uri,
            price: price,
            fundedAt: 0,
            acceptedAt: 0,
            lockWindow: effectiveLock,
            status: Status.Listed
        });
        emit Listed(listingId, msg.sender, itemHash, uri, price);
    }

    /// @notice Seller cancels an unfunded listing.
    function cancelListing(uint256 listingId) external nonReentrant {
        Listing storage l = listings[listingId];
        _requireStatus(l, Status.Listed);
        if (msg.sender != l.seller) revert NotSeller();
        l.status = Status.Cancelled;
        emit Cancelled(listingId, Status.Cancelled);
    }

    /// @notice Seller accepts the funds (signals delivery is in progress).
    function accept(uint256 listingId) external {
        Listing storage l = listings[listingId];
        _requireStatus(l, Status.Funded);
        if (msg.sender != l.seller) revert NotSeller();
        l.status = Status.Accepted;
        l.acceptedAt = uint64(block.timestamp);
        emit Accepted(listingId, block.timestamp + l.lockWindow);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Buyer flow
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Buyer pays the listing price.
    function buy(uint256 listingId) external payable whenNotPaused nonReentrant {
        Listing storage l = listings[listingId];
        _requireStatus(l, Status.Listed);
        if (msg.value != l.price) revert WrongPayment();
        l.buyer = msg.sender;
        l.fundedAt = uint64(block.timestamp);
        l.status = Status.Funded;
        emit Funded(listingId, msg.sender, msg.value);
    }

    /// @notice Buyer cancels a funded listing **before seller acceptance** → full refund.
    function cancelAsBuyer(uint256 listingId) external nonReentrant {
        Listing storage l = listings[listingId];
        _requireStatus(l, Status.Funded);
        if (msg.sender != l.buyer) revert NotBuyer();
        uint256 amount = l.price;
        address payable buyer = payable(l.buyer);
        l.status = Status.RefundedToBuyer;
        emit PaidOut(listingId, buyer, amount, Status.RefundedToBuyer);
        buyer.sendValue(amount);
    }

    /// @notice Buyer releases the funds to the seller (happy path).
    function release(uint256 listingId) external nonReentrant {
        Listing storage l = listings[listingId];
        _requireStatus(l, Status.Accepted);
        if (msg.sender != l.buyer) revert NotBuyer();
        _payout(listingId, l, l.seller, Status.PaidToSeller);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Dispute / timeout
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Either party flags a dispute. Funds stay locked until the arbitrator rules.
    function dispute(uint256 listingId) external {
        Listing storage l = listings[listingId];
        _requireStatus(l, Status.Accepted);
        if (msg.sender != l.buyer && msg.sender != l.seller) revert NotBuyer();
        l.status = Status.Disputed;
        emit Disputed(listingId, msg.sender);
    }

    /// @notice Arbitrator resolves a disputed listing.
    /// @param payToSeller If true, releases to seller; otherwise refunds buyer.
    function resolveDispute(uint256 listingId, bool payToSeller) external nonReentrant {
        if (msg.sender != arbitrator) revert NotArbitrator();
        Listing storage l = listings[listingId];
        _requireStatus(l, Status.Disputed);
        if (payToSeller) {
            _payout(listingId, l, l.seller, Status.PaidToSeller);
        } else {
            _payout(listingId, l, l.buyer, Status.RefundedToBuyer);
        }
    }

    /// @notice After the lock window elapses on an Accepted listing without dispute, anyone
    ///         can push the funds to the seller — protects against an unresponsive buyer.
    function timeoutPayout(uint256 listingId) external nonReentrant {
        Listing storage l = listings[listingId];
        _requireStatus(l, Status.Accepted);
        if (block.timestamp < l.acceptedAt + l.lockWindow) revert LockNotElapsed();
        _payout(listingId, l, l.seller, Status.PaidToSeller);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Admin
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Rotate the arbitrator. Owner only.
    function setArbitrator(address newArbitrator) external onlyOwner {
        if (newArbitrator == address(0)) revert ZeroAddress();
        emit ArbitratorRotated(arbitrator, newArbitrator);
        arbitrator = newArbitrator;
    }

    /// @notice Pause new listings and buys. In-flight escrows are unaffected.
    function pause() external onlyOwner {
        _pause();
    }

    function unpause() external onlyOwner {
        _unpause();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal
    // ─────────────────────────────────────────────────────────────────────────

    function _requireStatus(Listing storage l, Status expected) internal view {
        if (l.status == Status.None) revert ListingMissing();
        if (l.status != expected) revert WrongState();
    }

    /// @dev Effects-then-Interactions: write the terminal state, emit, then send.
    function _payout(uint256 listingId, Listing storage l, address recipient, Status finalStatus)
        internal
    {
        uint256 amount = l.price;
        l.status = finalStatus;
        emit PaidOut(listingId, recipient, amount, finalStatus);
        payable(recipient).sendValue(amount);
    }
}
