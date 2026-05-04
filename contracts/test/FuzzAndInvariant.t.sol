// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {TestBase, Vm} from "./TestBase.sol";
import {Curs3dToken} from "../src/Curs3dToken.sol";
import {Curs3dFaucet} from "../src/Curs3dFaucet.sol";
import {Curs3dStaking} from "../src/Curs3dStaking.sol";
import {Curs3dVault} from "../src/portfolio/Curs3dVault.sol";

// ─────────────────────────────────────────────────────────────────────────────
// Token fuzz
// ─────────────────────────────────────────────────────────────────────────────

contract TokenFuzzTest is TestBase {
    Curs3dToken internal token;

    function setUp() public {
        token = new Curs3dToken(0, 1_000_000 ether, address(this));
        token.mint(alice, 10_000 ether);
    }

    /// @dev Bounded amount, conservation invariant.
    function testFuzzTransferConservesSupply(uint256 rawAmount) public {
        uint256 amount = clamp(rawAmount, 0, token.balanceOf(alice));
        uint256 supplyBefore = token.totalSupply();

        vm.prank(alice);
        assertTrue(token.transfer(bob, amount), "transfer ok");

        assertEq(token.totalSupply(), supplyBefore, "supply changed");
        assertEq(token.balanceOf(alice) + token.balanceOf(bob), supplyBefore, "balances changed");
    }

    /// @dev Tightened bounds: any amount > remaining cap MUST revert with `CapExceeded`.
    ///      Previously this fuzzed only above 1M which was overly loose.
    function testFuzzMintCannotExceedCap(uint256 rawAmount) public {
        uint256 remaining = token.cap() - token.totalSupply();
        // Anything strictly greater than `remaining` must revert.
        uint256 amount = clamp(rawAmount, remaining + 1, type(uint128).max);
        vm.expectRevert(Curs3dToken.CapExceeded.selector);
        token.mint(alice, amount);
    }

    /// @dev Transfer to zero must always revert (OZ ERC20 default).
    function testFuzzTransferToZeroReverts(uint256 amount) public {
        vm.assume(amount > 0 && amount <= token.balanceOf(alice));
        vm.prank(alice);
        vm.expectRevert();
        // forge-lint: disable-next-line(erc20-unchecked-transfer)
        token.transfer(address(0), amount);
    }

    /// @dev Burning more than balance must always revert.
    function testFuzzBurnMoreThanBalanceReverts(uint256 extra) public {
        uint256 balance = token.balanceOf(alice);
        vm.assume(extra > 0 && extra < type(uint128).max - balance);
        vm.prank(alice);
        vm.expectRevert();
        token.burn(balance + extra);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Faucet handler + invariants
// ─────────────────────────────────────────────────────────────────────────────

contract FaucetHandler {
    Curs3dToken public token;
    Curs3dFaucet public faucet;

    address public userOne = address(0x1111);
    address public userTwo = address(0x2222);
    uint256 public ghostClaims;

    constructor(Curs3dToken token_, Curs3dFaucet faucet_) {
        token = token_;
        faucet = faucet_;
    }

    function claim(uint256 actorSeed, uint256 timeJump) external {
        address actor = actorSeed % 2 == 0 ? userOne : userTwo;
        uint256 jump = (timeJump % 2 hours) + 1 hours;
        Vm(address(uint160(uint256(keccak256("hevm cheat code"))))).warp(block.timestamp + jump);
        Vm(address(uint160(uint256(keccak256("hevm cheat code"))))).prank(actor);
        faucet.claim();
        ghostClaims++;
    }
}

interface IInvariantConfig {
    function targetContract(address newTargetedContract) external;
    function excludeSender(address sender) external;
}

contract FaucetInvariantTest is TestBase {
    Curs3dToken internal token;
    Curs3dFaucet internal faucet;
    FaucetHandler internal handler;

    function setUp() public {
        token = new Curs3dToken(0, 1_000_000 ether, address(this));
        faucet = new Curs3dFaucet(token, 100 ether, 1 hours, address(this));
        token.setMinter(address(faucet), true);
        handler = new FaucetHandler(token, faucet);
    }

    /// @notice Foundry uses functions starting with `targetContracts` / `excludeContracts`
    ///         (returning address[]) to scope invariant fuzzing — see
    ///         https://book.getfoundry.sh/forge/invariant-testing.
    function targetContracts() external view returns (address[] memory addrs) {
        addrs = new address[](1);
        addrs[0] = address(handler);
    }

    /// @notice Total supply never exceeds the cap.
    function invariantTokenSupplyNeverExceedsCap() public view {
        assertTrue(token.totalSupply() <= token.cap(), "cap respected");
    }

    /// @notice Total minted via faucet equals exactly `claimAmount * ghostClaims`.
    function invariantFaucetAccountingExact() public view {
        assertEq(
            token.totalSupply(),
            faucet.claimAmount() * handler.ghostClaims(),
            "faucet supply = drips * claims"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Staking invariants
// ─────────────────────────────────────────────────────────────────────────────

contract StakingHandler {
    Curs3dToken public token;
    Curs3dStaking public staking;
    address[] public actors;

    constructor(Curs3dToken token_, Curs3dStaking staking_, address[] memory actors_) {
        token = token_;
        staking = staking_;
        actors = actors_;
    }

    function stake(uint256 actorSeed, uint256 amount) external {
        address actor = actors[actorSeed % actors.length];
        uint256 bal = token.balanceOf(actor);
        if (bal == 0) return;
        amount = (amount % bal) + 1;
        Vm v = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
        v.startPrank(actor);
        token.approve(address(staking), amount);
        staking.stake(amount);
        v.stopPrank();
    }

    function unstake(uint256 actorSeed, uint256 amount) external {
        address actor = actors[actorSeed % actors.length];
        uint256 staked = staking.stakedBalance(actor);
        if (staked == 0) return;
        amount = (amount % staked) + 1;
        Vm v = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
        v.prank(actor);
        staking.unstake(amount);
    }
}

contract StakingInvariantTest is TestBase {
    Curs3dToken internal token;
    Curs3dStaking internal staking;
    StakingHandler internal handler;
    address[] internal actors;

    function setUp() public {
        token = new Curs3dToken(0, 100_000_000 ether, address(this));
        staking = new Curs3dStaking(token, 0, address(this)); // rate = 0 to keep invariant pure
        token.setMinter(address(staking), true);
        actors = new address[](3);
        actors[0] = alice;
        actors[1] = bob;
        actors[2] = carol;
        token.mint(alice, 10_000 ether);
        token.mint(bob, 10_000 ether);
        token.mint(carol, 10_000 ether);
        handler = new StakingHandler(token, staking, actors);
    }

    function targetContracts() external view returns (address[] memory addrs) {
        addrs = new address[](1);
        addrs[0] = address(handler);
    }

    /// @notice totalStaked == sum of individual stakes (the canonical invariant).
    function invariantTotalEqualsSumOfBalances() public view {
        uint256 sum;
        for (uint256 i = 0; i < actors.length; i++) {
            sum += staking.stakedBalance(actors[i]);
        }
        assertEq(staking.totalStaked(), sum, "totalStaked invariant");
    }

    /// @notice Token held by the staking contract >= totalStaked (slack only goes up via rewards).
    function invariantSolvent() public view {
        assertTrue(token.balanceOf(address(staking)) >= staking.totalStaked(), "solvency");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Vault fuzz
// ─────────────────────────────────────────────────────────────────────────────

contract VaultFuzzTest is TestBase {
    Curs3dToken internal token;
    Curs3dVault internal vault;

    function setUp() public {
        token = new Curs3dToken(0, 100_000_000 ether, address(this));
        vault = new Curs3dVault(token, address(this));
        token.mint(alice, 10_000_000 ether);
    }

    /// @notice Round-trip deposit then withdraw should never let a user extract more value
    ///         than they put in. Tolerance for the inflation-defence rounding offset.
    function testFuzzDepositWithdrawNoFreeMoney(uint256 amount) public {
        amount = clamp(amount, 1 ether, token.balanceOf(alice));
        uint256 balBefore = token.balanceOf(alice);

        vm.startPrank(alice);
        token.approve(address(vault), amount);
        vault.deposit(amount, alice);
        // Withdraw the maximum that the user can extract.
        uint256 maxOut = vault.convertToAssets(vault.balanceOf(alice));
        if (maxOut > 0) vault.withdraw(maxOut, alice, alice);
        vm.stopPrank();

        // Result must be <= original balance — never more.
        assertTrue(token.balanceOf(alice) <= balBefore, "no free money");
    }
}
