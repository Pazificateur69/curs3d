# CURS3D Website Audit

Date: 2026-04-29

## Scope

This audit covers the public static website in `website/`, with priority on the homepage, shared design system, accessibility basics, performance safety and documentation clarity.

## Current strengths

- The website is already bilingual, with persistent language preference and translated metadata.
- The information architecture is credible for a protocol project: docs, stack, governance, tokenomics, whitepaper, examples and explorer are separate pages.
- The visual foundation already matches the brand direction: obsidian base, cyan signal color, gold premium accent, technical typography and glass surfaces.
- The homepage contains concrete proof points instead of only marketing claims: tests, endpoints, validators, Dilithium 5, WASM, governance and SDKs.

## Issues found

- The original hero had good content but not enough hierarchy. The first screen did not immediately feel like a premium protocol landing page.
- The live panel relied on a simple SVG orbit and did not communicate the stack strongly enough.
- Important reader paths were present in navigation but not framed as explicit journeys for developers, validators and issuers.
- The canvas resize logic used repeated `ctx.scale`, which can accumulate transforms after multiple resizes.
- Motion did not fully respect `prefers-reduced-motion`.
- Mobile navigation visually worked, but the toggle did not expose `aria-expanded`.
- Language buttons changed state visually, but did not expose pressed state to assistive technology.

## Changes implemented

- Rebuilt the homepage hero around a sharper positioning line: durable trust infrastructure for assets that cannot expire.
- Added a premium command-panel visual with orbital protocol core, live stats, stack signals and explorer CTA.
- Replaced loose feature pills with a stronger proof rail: post-quantum crypto, deterministic finality and programmable issuance.
- Added a trust architecture section explaining the four core layers: cryptographic durability, finality, programmable issuance and governance continuity.
- Added a launch surface section that routes serious readers to documentation, validator operations and tokenomics.
- Added a stronger closing CTA that restates the long-horizon infrastructure thesis.
- Added a more Web3-native protocol cockpit in the hero: HUD panel, chain lattice, live terminal, stack signals and primitive ticker.
- Added a protocol control-plane section that maps the homepage visuals to real chain layers: accounts, validators, execution, assets and access surfaces.
- Added an SVG favicon and social metadata foundation.
- Improved keyboard focus styles across links, buttons and form fields.
- Added reduced-motion safeguards and disabled the particle canvas for users who request reduced motion.
- Fixed canvas retina resizing by using `ctx.setTransform` instead of repeated scaling.
- Added `aria-expanded` for mobile navigation and `aria-pressed` for language buttons.
- Added a timeout to live status fetches so a slow API does not hang unnecessarily.

## Design direction

The updated homepage follows the existing brand documentation in `docs/branding/05-visual-identity.md`:

- Institutional sci-tech, not retail crypto casino.
- Obsidian and graphite base, cyan for signal, gold for premium emphasis.
- Orbital/lattice visual language instead of generic blockchain cubes.
- Slow, deliberate motion with reduced-motion fallback.
- Clear proof-first messaging instead of vague hype.

## Remaining priorities

- Run Lighthouse against the deployed site and capture scores for performance, accessibility, best practices and SEO.
- Verify all public claims against the current testnet and repository state before a major launch.
- Add Open Graph and social preview assets for higher quality link sharing.
- Consider a dedicated ecosystem or launch page once real partners or applications are available.
- Add a short public status note when live API data is unavailable, instead of silently leaving stats as `--`.
