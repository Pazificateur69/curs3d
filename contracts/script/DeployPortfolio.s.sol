// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Curs3dToken} from "../src/Curs3dToken.sol";
import {Curs3dFaucet} from "../src/Curs3dFaucet.sol";
import {Curs3dStaking} from "../src/Curs3dStaking.sol";
import {Curs3dGovernance} from "../src/Curs3dGovernance.sol";
import {Curs3dAttestations} from "../src/Curs3dAttestations.sol";
import {Curs3dVault} from "../src/portfolio/Curs3dVault.sol";
import {DigitalEscrow} from "../src/portfolio/DigitalEscrow.sol";

/// @dev Minimal Vm interface used by this script. Mirrors the Foundry cheatcodes we need.
interface ScriptVm {
    function envUint(string calldata key) external returns (uint256);
    function envOr(string calldata key, address defaultValue) external returns (address);
    function envOr(string calldata key, bool defaultValue) external returns (bool);
    function addr(uint256 privateKey) external returns (address);
    function startBroadcast() external;
    function startBroadcast(uint256 privateKey) external;
    function stopBroadcast() external;
    function serializeAddress(string calldata objectKey, string calldata valueKey, address value)
        external
        returns (string memory json);
    function serializeUint(string calldata objectKey, string calldata valueKey, uint256 value)
        external
        returns (string memory json);
    function serializeString(
        string calldata objectKey,
        string calldata valueKey,
        string calldata value
    ) external returns (string memory json);
    function writeJson(string calldata json, string calldata path) external;
    function exists(string calldata path) external returns (bool);
    function toString(uint256 value) external returns (string memory);
}

/// @title DeployPortfolio
/// @notice One-shot deploy script for the full CURS3D Solidity portfolio.
/// @dev Authentication options (any one of):
///        1. `--private-key 0x...` flag on the forge command
///        2. `--keystore <file> --password <pw>` flag pair
///        3. `PRIVATE_KEY` env variable (legacy path)
///      Reads:
///        - ARBITRATOR (optional)     — DigitalEscrow arbitrator (defaults to deployer)
///        - FORCE (optional)          — set true to overwrite an existing deployments file
///      Writes:
///        - ./deployments/<chainId>.json — atomic JSON via vm.writeJson
contract DeployPortfolio {
    ScriptVm internal constant vm =
        ScriptVm(address(uint160(uint256(keccak256("hevm cheat code")))));

    struct Deployment {
        Curs3dToken token;
        Curs3dFaucet faucet;
        Curs3dStaking staking;
        Curs3dGovernance governance;
        Curs3dAttestations attestations;
        Curs3dVault vault;
        DigitalEscrow escrow;
    }

    function run() external returns (Deployment memory deployed) {
        bool force = vm.envOr("FORCE", false);

        string memory deploymentPath =
            string.concat("./deployments/", vm.toString(block.chainid), ".json");
        if (vm.exists(deploymentPath) && !force) {
            revert(
                string.concat(
                    "deployment file already exists: ",
                    deploymentPath,
                    " (set FORCE=true to overwrite)"
                )
            );
        }

        // ╔══════════════════════════════════════════════════════════════════╗
        // ║  CURS3D PORTFOLIO — deploying 7 contracts                        ║
        // ╚══════════════════════════════════════════════════════════════════╝
        // Use the broadcast configuration (--keystore / --private-key /
        // --account) provided to forge instead of pinning to PRIVATE_KEY env.
        vm.startBroadcast();
        address deployer = msg.sender;
        address arbitrator = vm.envOr("ARBITRATOR", deployer);

        deployed.token = new Curs3dToken(1_000_000 ether, 100_000_000 ether, deployer);
        deployed.faucet = new Curs3dFaucet(deployed.token, 100 ether, 1 hours, deployer);
        deployed.staking = new Curs3dStaking(deployed.token, 0.001 ether, deployer);
        deployed.governance =
            new Curs3dGovernance(deployed.token, 1 minutes, 3 days, 10_000 ether, deployer);
        deployed.attestations = new Curs3dAttestations(deployer);
        deployed.vault = new Curs3dVault(deployed.token, deployer);
        deployed.escrow = new DigitalEscrow(deployer, arbitrator);

        deployed.token.setMinter(address(deployed.faucet), true);
        deployed.token.setMinter(address(deployed.staking), true);

        vm.stopBroadcast();

        // Atomic write of the deployment manifest.
        string memory object = "deployment";
        vm.serializeUint(object, "chainId", block.chainid);
        vm.serializeAddress(object, "deployer", deployer);
        vm.serializeAddress(object, "arbitrator", arbitrator);
        vm.serializeAddress(object, "token", address(deployed.token));
        vm.serializeAddress(object, "faucet", address(deployed.faucet));
        vm.serializeAddress(object, "staking", address(deployed.staking));
        vm.serializeAddress(object, "governance", address(deployed.governance));
        vm.serializeAddress(object, "attestations", address(deployed.attestations));
        vm.serializeAddress(object, "vault", address(deployed.vault));
        string memory json = vm.serializeAddress(object, "escrow", address(deployed.escrow));
        vm.writeJson(json, deploymentPath);
    }
}
