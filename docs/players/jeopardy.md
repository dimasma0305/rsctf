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

## Solver uploads

Some events let teams upload the solver they used so organizers can verify a
solve. When it is on, a challenge your team has solved shows a **Solver**
section on its card. Uploading is optional.

1. Open the section with **Upload** and choose the file in **Solver file**
   (up to 1 MiB, any type; a script, notebook, or notes).
2. Choose **Upload solver**.

Every upload is kept as a new version; you cannot delete or replace an earlier
one. Your team can upload up to 10 versions per challenge and 16 MiB in total
for the event, until the later of the event end and the writeup deadline.
Organizers see every version with its uploader, upload time, and how long after
the solve it was uploaded. Do not include other teams' information.

## AI chat links

Some events ask teams to disclose the AI chats they used. When the organizer
has turned this on, a challenge your team has solved shows an **AI chat links**
section on its card.

1. In the AI service, create a public share link for the chat and copy it.
2. Open the section with **Manage**, paste the link into **Share link**, and
   choose **Add**. The field shows at once which accepted provider the link
   matches, or why it is rejected. The accepted providers are listed below it.
3. Add up to five links for the challenge, then choose **Save links**. Links
   are not stored until you save.

If your team did not use AI for the challenge, choose **No AI used** instead of
adding links.

Some events **require** a disclosure after every solve. The section then opens
with a "Disclosure required" notice right after you solve, the challenge card
stays open until you save links or declare No AI used, and the challenge page
lists every solved challenge that still needs one. Your points are never
withheld, but organizers see which solves have no disclosure.

Any member of the team can add, replace, or remove the team's links until the
later of the event end and the writeup deadline. If a teammate saved changes
while you were editing, reload the section and apply your change again. A link
must use `https`, must not contain a username, password, or custom port, and is
saved without its `#` fragment.

Every save, edit, and removal is recorded with the time, the member who made
it, how long after the solve it happened, which links were added or removed,
and a keyed hash of your network address; organizers review this history for
cheat detection. Organizers with monitoring access can open and read every chat
you link. Share
only chats you are willing to disclose, and do not include flags or other
teams' information. rsctf stores the link but never fetches the chat. To stop
sharing a chat, remove the link here and also delete or unshare it in the AI
service; removing it from rsctf does not revoke the public link, and anyone who
already has it can still open it until you revoke it at the provider.
