# Runbook FR

## Voir la dApp en local

Depuis le dossier `contracts`:

```bash
make serve-dapp
```

Puis ouvre:

```text
http://127.0.0.1:8088
```

Si le port est deja pris:

```bash
python3 -m http.server 8089 --directory dapp
```

## Faire tourner les tests

```bash
make test
```

Resultat attendu:

```text
56 tests passed, 0 failed
```

## Ce qu'il te faut pour que la dApp fonctionne vraiment

Il faut:
- un wallet navigateur, par exemple MetaMask ou Rabby
- un peu de CUR natif sur CURS3D testnet, ou de l'ETH testnet si tu deploies aussi sur Base Sepolia / Sepolia
- une cle testnet uniquement
- le RPC CURS3D `https://rpc.curs3d.fr/eth` (ou un RPC Base Sepolia / Sepolia pour les demos portables)

Important:
- ne mets jamais une cle mainnet ici
- utilise un wallet neuf pour les tests
- garde quelques centimes d'ETH testnet pour payer le gas

## Configurer le deploiement

Dans `contracts`, cree un fichier `.env`:

```bash
cp .env.example .env
```

Remplis au minimum:

```text
CURS3D_RPC_URL=https://rpc.curs3d.fr/eth
BASE_SEPOLIA_RPC_URL=https://sepolia.base.org
```

Pour CURS3D, utilise de preference un keystore Foundry avec `--keystore`,
`--password` et `--sender` plutot qu'une cle privee en clair.

## Deployer sur Base Sepolia

```bash
make deploy-base-sepolia
```

Le script va ecrire les adresses ici:

```text
contracts/deployments/latest.json
```

## Charger les contrats dans la dApp

1. Ouvre `contracts/deployments/latest.json`.
2. Copie tout le JSON.
3. Colle-le dans le champ `Deployment JSON` de la dApp.
4. Clique `Load JSON`.
5. Clique `Connect wallet`.
6. Clique `Base Sepolia`.
7. Clique `Claim test tokens`.

Ensuite tu peux tester:
- issue attestation
- read attestation
- approve + stake
- create proposal
- vote
- escrow list/buy

## Ce que tu montres a un recruteur

Dans un entretien:

```text
J'ai un projet Rust L1, et j'ai ajoute un module Solidity portfolio complet:
token, faucet, staking, governance, attestations, vault, escrow, tests Foundry,
fuzz tests, invariant tests, script de deploiement et dApp testnet.
```

Puis tu montres:
- `contracts/README.md`
- `contracts/SECURITY.md`
- `contracts/PORTFOLIO.md`
- `forge test --summary`
- la dApp avec un vrai deploiement Base Sepolia
