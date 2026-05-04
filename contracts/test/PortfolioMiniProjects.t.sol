// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {TestBase} from "./TestBase.sol";
import {Curs3dToken} from "../src/Curs3dToken.sol";
import {Curs3dVault} from "../src/portfolio/Curs3dVault.sol";
import {DigitalEscrow} from "../src/portfolio/DigitalEscrow.sol";

/// @notice Tests for the two portfolio mini-projects: Curs3dVault and DigitalEscrow.
contract PortfolioMiniProjectsTest is TestBase {
    Curs3dToken internal token;
    Curs3dVault internal vault;
    DigitalEscrow internal escrow;

    address internal arbitrator = address(0xA787);

    function setUp() public {
        token = new Curs3dToken(0, 10_000_000 ether, address(this));
        vault = new Curs3dVault(token, address(this));
        escrow = new DigitalEscrow(address(this), arbitrator);
        token.mint(alice, 1_000 ether);
        vm.deal(bob, 10 ether);
    }

    // ─── Vault ─────────────────────────────────────────────────────────────

    function testVaultDepositAndWithdraw() public {
        vm.startPrank(alice);
        token.approve(address(vault), 100 ether);
        vault.deposit(100 ether, alice);
        // The inflation-protected formula introduces a tiny rounding offset; check that the
        // user can withdraw essentially the same value back.
        uint256 sharesAlice = vault.balanceOf(alice);
        assertGt(sharesAlice, 0, "shares minted");

        vault.withdraw(50 ether, alice, alice);
        vm.stopPrank();

        // Alice spent 100 ether, withdrew 50 ether → balance = 1000 - 100 + 50 = 950 ether
        // (might be 949...999 wei due to rounding offset).
        assertApproxEq(token.balanceOf(alice), 950 ether, 1e6, "returned assets approx");
    }

    function testVaultRejectsZeroDeposit() public {
        vm.prank(alice);
        vm.expectRevert(Curs3dVault.ZeroAmount.selector);
        vault.deposit(0, alice);
    }

    function testVaultPauseBlocksDepositButNotWithdraw() public {
        vm.startPrank(alice);
        token.approve(address(vault), 100 ether);
        vault.deposit(100 ether, alice);
        vm.stopPrank();

        vault.pause();

        // Deposit blocked.
        vm.prank(alice);
        token.approve(address(vault), 50 ether);
        vm.prank(alice);
        // OZ Pausable.EnforcedPause selector
        vm.expectRevert(0xd93c0665);
        vault.deposit(50 ether, alice);

        // Withdraw allowed.
        vm.prank(alice);
        vault.withdraw(10 ether, alice, alice);
    }

    function testVaultRejectsNonOwnerWithdraw() public {
        vm.startPrank(alice);
        token.approve(address(vault), 100 ether);
        vault.deposit(100 ether, alice);
        vm.stopPrank();

        vm.prank(bob);
        vm.expectRevert(Curs3dVault.NotShareOwner.selector);
        vault.withdraw(10 ether, bob, alice);
    }

    function testVaultZeroAddressReceiverReverts() public {
        vm.prank(alice);
        token.approve(address(vault), 50 ether);
        vm.prank(alice);
        vm.expectRevert(Curs3dVault.ZeroAddress.selector);
        vault.deposit(50 ether, address(0));
    }

    // ─── Escrow ────────────────────────────────────────────────────────────

    function testEscrowHappyPath() public {
        uint256 sellerBefore = alice.balance;

        vm.prank(alice);
        uint256 listingId =
            escrow.list(keccak256("certificate.pdf"), "ipfs://certificate", 1 ether, 0);

        vm.prank(bob);
        escrow.buy{value: 1 ether}(listingId);

        vm.prank(alice);
        escrow.accept(listingId);

        vm.prank(bob);
        escrow.release(listingId);

        assertEq(alice.balance, sellerBefore + 1 ether, "seller paid");
    }

    function testEscrowRejectsWrongPayment() public {
        vm.prank(alice);
        uint256 listingId = escrow.list(keccak256("artifact"), "ipfs://artifact", 1 ether, 0);

        vm.prank(bob);
        vm.expectRevert(DigitalEscrow.WrongPayment.selector);
        escrow.buy{value: 0.5 ether}(listingId);
    }

    function testEscrowBuyerCancelBeforeAcceptance() public {
        uint256 buyerBefore = bob.balance;

        vm.prank(alice);
        uint256 listingId = escrow.list(keccak256("x"), "ipfs://x", 1 ether, 0);

        vm.prank(bob);
        escrow.buy{value: 1 ether}(listingId);

        vm.prank(bob);
        escrow.cancelAsBuyer(listingId);

        assertEq(bob.balance, buyerBefore, "refunded");
    }

    function testEscrowSellerCannotCancelAfterFunded() public {
        vm.prank(alice);
        uint256 listingId = escrow.list(keccak256("x"), "ipfs://x", 1 ether, 0);
        vm.prank(bob);
        escrow.buy{value: 1 ether}(listingId);
        vm.prank(alice);
        vm.expectRevert(DigitalEscrow.WrongState.selector);
        escrow.cancelListing(listingId);
    }

    function testEscrowDisputeResolvedToBuyer() public {
        vm.prank(alice);
        uint256 listingId = escrow.list(keccak256("d"), "ipfs://d", 1 ether, 0);
        vm.prank(bob);
        escrow.buy{value: 1 ether}(listingId);
        vm.prank(alice);
        escrow.accept(listingId);
        vm.prank(bob);
        escrow.dispute(listingId);

        uint256 buyerBefore = bob.balance;
        vm.prank(arbitrator);
        escrow.resolveDispute(listingId, false);
        assertEq(bob.balance, buyerBefore + 1 ether, "buyer refunded");
    }

    function testEscrowOnlyArbitratorCanResolve() public {
        vm.prank(alice);
        uint256 listingId = escrow.list(keccak256("d"), "ipfs://d", 1 ether, 0);
        vm.prank(bob);
        escrow.buy{value: 1 ether}(listingId);
        vm.prank(alice);
        escrow.accept(listingId);
        vm.prank(bob);
        escrow.dispute(listingId);

        vm.prank(alice);
        vm.expectRevert(DigitalEscrow.NotArbitrator.selector);
        escrow.resolveDispute(listingId, true);
    }

    function testEscrowTimeoutPayoutToSeller() public {
        uint256 sellerBefore = alice.balance;
        vm.prank(alice);
        uint256 listingId = escrow.list(keccak256("t"), "ipfs://t", 1 ether, 1 days);
        vm.prank(bob);
        escrow.buy{value: 1 ether}(listingId);
        vm.prank(alice);
        escrow.accept(listingId);

        vm.warp(block.timestamp + 1 days + 1);
        // Anyone can finalise.
        vm.prank(carol);
        escrow.timeoutPayout(listingId);
        assertEq(alice.balance, sellerBefore + 1 ether, "seller paid via timeout");
    }

    function testEscrowRejectsZeroPrice() public {
        vm.prank(alice);
        vm.expectRevert(DigitalEscrow.InvalidPrice.selector);
        escrow.list(keccak256("z"), "ipfs://z", 0, 0);
    }
}
