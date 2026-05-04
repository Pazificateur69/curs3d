.PHONY: help install fmt fmt-check build test coverage snapshot lint clean dry-run \
        deploy-sepolia deploy-base-sepolia serve-dapp gas-report

ifneq (,$(wildcard .env))
include .env
export
endif

help:  ## Show this help.
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'

install:  ## Install OpenZeppelin + forge-std into ./lib (idempotent).
	@test -d lib/openzeppelin-contracts || forge install OpenZeppelin/openzeppelin-contracts --shallow
	@test -d lib/forge-std || forge install foundry-rs/forge-std --shallow

fmt:  ## Run `forge fmt`.
	forge fmt

fmt-check:  ## CI-friendly: fail if anything is unformatted.
	forge fmt --check

build:  ## Compile contracts (must be 0 warnings).
	forge build

test:  ## Run the full test suite with summary.
	forge test --summary -vv

coverage:  ## Print line/branch/func/region coverage. May be slow.
	forge coverage --report summary

snapshot:  ## Refresh the gas snapshot file (.gas-snapshot).
	forge snapshot

gas-report:  ## Write a per-function gas table to .gas-report.
	forge test --gas-report > .gas-report

lint:  ## Strict lint pass (high+medium severity).
	forge lint --severity high --severity med

clean:  ## Remove all build artefacts.
	forge clean

dry-run:  ## Run the deploy script against the in-process EVM (no broadcast).
	PRIVATE_KEY=0x59c6995e998f97a5a004497e5da0f7d9b3b6fbf5e5c6e3d1f1f1c7e6f1111111 \
		forge script script/DeployPortfolio.s.sol:DeployPortfolio

deploy-sepolia:  ## Broadcast deploy to Ethereum Sepolia.
	forge script script/DeployPortfolio.s.sol:DeployPortfolio \
		--rpc-url "$$SEPOLIA_RPC_URL" --broadcast --verify

deploy-base-sepolia:  ## Broadcast deploy to Base Sepolia.
	forge script script/DeployPortfolio.s.sol:DeployPortfolio \
		--rpc-url "$$BASE_SEPOLIA_RPC_URL" --broadcast --verify

serve-dapp:  ## Serve dapp/ on http://localhost:8088.
	python3 -m http.server 8088 --directory dapp
