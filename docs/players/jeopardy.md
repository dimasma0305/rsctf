# Jeopardy games

In a Jeopardy-style game, each challenge is an independent problem. Solving it reveals a flag; submitting that flag awards points to your team.

## Open a challenge

From the game page, choose a challenge card. The panel can contain:

- A description and downloadable attachments
- Hints, which may have a point cost or release time
- A button to create a temporary challenge container
- A flag submission field
- Your team's attempts and solve state

Challenge types include static or dynamic attachments and static or dynamic containers. “Dynamic” generally means your team receives its own flag or runtime instance.

## Use a challenge container

If the challenge has a container action:

1. Create the instance.
2. Wait for the endpoint to appear.
3. Connect only to the displayed host and port.
4. Extend the instance before its deadline if the event allows it.
5. Destroy it when you are finished.

The endpoint may use a high, dynamically selected port. If it is unreachable, first check the game notice; then report the exact challenge name, displayed endpoint, and time to the organizer. Do not post your flag.

## Submit a flag

Paste the complete flag exactly as found. Preserve capitalization, braces, punctuation, and any prefix. The response distinguishes an accepted answer from a wrong answer or an already completed challenge.

Repeatedly guessing the submission endpoint is not a productive strategy and may trigger rate limits.

## Understand scoring

Depending on the challenge settings, its value may stay fixed or decay as more
eligible teams solve it. For initial score `O`, minimum rate `m`, difficulty
`d`, and eligible solve count `n`, the value remains `O` through the first
solve. After that, RSCTF floors `O` times the selected factor:

```text
Linear:      max(m, 1 - (1-m)(n-1)/d)
Logarithmic: m + (1-m)/(1 + ln(n)/d)
Standard:    m + (1-m)exp((1-n)/d)
```

First-, second-, and third-blood bonuses multiply the current value by their
configured factors and use round-half-to-even. A challenge may disable blood.
The same snapshot value applies to every eligible solver of that challenge.

The scoreboard orders teams by points, then by the earlier last
score-eligible solve, then stable team ID. Jeopardy ranks are ordinal. An
ineligible solve cannot consume a blood slot, affect dynamic decay, or change
the scoring tie-break.

During a scoreboard freeze, your submission is still graded. The public board may hide recent changes until the organizer reveals or unfreezes it.

Once competition scoring begins or a durable solve exists, organizers cannot
change score-affecting event, challenge, flag/template, or division settings.
This prevents a repository sync or operator edit from reinterpreting existing
solves.

## Writeups

Some games require a writeup after play. When enabled, upload it from the game writeup area before the deadline. The current server accepts a non-empty lowercase `.pdf` file with the `application/pdf` type, up to 20 MiB. Uploading again replaces your previous file.
