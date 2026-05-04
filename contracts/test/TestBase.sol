// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/// @notice Slim Foundry cheatcode interface. We import the exact subset our tests need so the
///         portfolio test base file compiles even on stripped-down Foundry installs.
interface Vm {
    function addr(uint256 privateKey) external returns (address);
    function warp(uint256 newTimestamp) external;
    function deal(address account, uint256 newBalance) external;
    function prank(address msgSender) external;
    function startPrank(address msgSender) external;
    function stopPrank() external;
    function expectRevert(bytes4 selector) external;
    function expectRevert(bytes calldata revertData) external;
    function expectRevert() external;
    function expectEmit(bool checkTopic1, bool checkTopic2, bool checkTopic3, bool checkData)
        external;
    function expectEmit() external;
    function recordLogs() external;
    function getRecordedLogs() external returns (Log[] memory);
    function label(address account, string calldata newLabel) external;
    function assume(bool condition) external pure;
    function load(address target, bytes32 slot) external view returns (bytes32);
    function store(address target, bytes32 slot, bytes32 value) external;

    struct Log {
        bytes32[] topics;
        bytes data;
        address emitter;
    }
}

/// @title TestBase
/// @notice Common test fixtures + tiny in-house assertion helpers.
/// @dev Kept dependency-free of forge-std on purpose so an upstream forge-std API churn cannot
///      break the portfolio test suite.
abstract contract TestBase {
    Vm internal constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    address internal alice = address(0xA11CE);
    address internal bob = address(0xB0B);
    address internal carol = address(0xCA201);
    address internal dave = address(0xDA15);

    function assertEq(uint256 actual, uint256 expected, string memory message) internal pure {
        if (actual != expected) revert(string.concat(message, ": uint mismatch"));
    }

    function assertEq(uint256 actual, uint256 expected) internal pure {
        if (actual != expected) revert("uint mismatch");
    }

    function assertEq(address actual, address expected, string memory message) internal pure {
        if (actual != expected) revert(string.concat(message, ": address mismatch"));
    }

    function assertEq(address actual, address expected) internal pure {
        if (actual != expected) revert("address mismatch");
    }

    function assertEq(bytes32 actual, bytes32 expected, string memory message) internal pure {
        if (actual != expected) revert(string.concat(message, ": bytes32 mismatch"));
    }

    function assertTrue(bool value, string memory message) internal pure {
        if (!value) revert(message);
    }

    function assertTrue(bool value) internal pure {
        if (!value) revert("assertion failed");
    }

    function assertFalse(bool value, string memory message) internal pure {
        if (value) revert(message);
    }

    function assertGt(uint256 actual, uint256 boundary, string memory message) internal pure {
        if (actual <= boundary) revert(string.concat(message, ": not greater"));
    }

    function assertLt(uint256 actual, uint256 boundary, string memory message) internal pure {
        if (actual >= boundary) revert(string.concat(message, ": not less"));
    }

    function clamp(uint256 value, uint256 min, uint256 max) internal pure returns (uint256) {
        if (value < min) return min;
        if (value > max) return max;
        return value;
    }

    /// @notice Approx-equal helper for fuzz tests where rounding can introduce 1-wei drift.
    function assertApproxEq(
        uint256 actual,
        uint256 expected,
        uint256 tolerance,
        string memory message
    ) internal pure {
        uint256 diff = actual > expected ? actual - expected : expected - actual;
        if (diff > tolerance) revert(string.concat(message, ": approx mismatch"));
    }
}
