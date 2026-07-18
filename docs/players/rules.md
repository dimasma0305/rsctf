# Rules and fair play

This page is a safe starting template. The event organizer's published rules always take precedence.

## Competition boundary

Only attack systems and addresses explicitly identified as challenge targets. The rsctf website, API, database, reverse proxy, VPN hub, checker, monitoring systems, organizer accounts, and other infrastructure are out of scope unless a challenge says otherwise.

## Prohibited behavior

- Denial-of-service attacks or deliberate resource exhaustion
- Attacking non-game services, other players' devices, or organizer infrastructure
- Sharing flags, control tokens, private VPN profiles, or live solutions across teams
- Brute-forcing or flooding flag-submission endpoints
- Bypassing roster, division, eligibility, or account restrictions
- Intentionally destroying a shared KotH hill so no team can play
- Publishing writeups before the organizer's allowed time

## Team conduct

- Use only accounts assigned to your team.
- Keep invite codes, A&D Bearer tokens, SSH keys, and VPN configurations private.
- Report accidental access to out-of-scope data immediately and stop investigating it.
- Follow organizer instructions when a challenge is paused or reset.

## A&D-specific rules

- Attack only the current target list.
- Keep your own service compatible with the checker.
- Do not block the checker, VPN hub, or scoring traffic.
- Submit only flags obtained through allowed game activity.
- A flag's validity follows the event's configured lifetime window.

## KotH-specific rules

- Use the control token issued to your team.
- Preserve the hill's expected functionality.
- Do not interfere with the checker or control plane.

## Reporting a problem

Give the organizer the game, challenge, team, approximate UTC time, and a short reproduction. Send sensitive evidence privately. Never include a live flag, token, password, private key, or VPN configuration in a public ticket.
