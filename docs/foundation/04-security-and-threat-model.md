# Security And Threat Model

## Position honnête

`CURS3D` doit être présenté comme un protocole qui prend le risque post-quantique au sérieux, pas comme un protocole qui "résout définitivement la sécurité".

## Menaces considérées

- compromission future des schémas de signature classiques
- mauvaise adéquation entre durée de vie de l'actif et hypothèses crypto
- dette de migration sur chaînes historiques
- bugs de protocole / consensus / VM
- erreurs d'implémentation
- capture gouvernance
- compromission de l'infrastructure opérateur

## Menaces non résolues automatiquement

- bugs logiques d'applications
- compromission d'opsec
- erreurs humaines
- failles économiques
- attaques sociales
- risques réglementaires

## Ce qu'il faut toujours dire

- la cryptographie ne remplace pas les audits
- la sécurité ne se réduit pas à la primitive de signature
- la durabilité vient aussi de la gouvernance et de l'upgradabilité

## Exigences minimales de crédibilité sécurité

- audit externe
- testnet public
- bug bounty
- threat model public
- politique de disclosure
- process de release rigoureux

## Positionnement communication

Bon:

`CURS3D is designed around post-quantum security assumptions.`

Mauvais:

`CURS3D is impossible to break.`
