# Run your first event

This tutorial builds a small Jeopardy event first. It gives you a complete, testable path before you add A&D, KotH, repository sync, or complex infrastructure.

## Before creating the game

Confirm that:

- You can open **Admin** and see the dashboard.
- The public URL, registration policy, and site identity are correct.
- You have a separate test-player account and test team.
- PostgreSQL and uploaded files are backed up.
- Dynamic challenge infrastructure is enabled if you plan to use containers.

## 1. Create the game

Open **Admin → Games**, select the create action, and enter a title, start time, and end time. Use a short private schedule for the first test.

After creation, open the game's **Info** page and review:

- Summary and full description
- Team-size and container limits
- Invite code and visibility
- Participation review policy
- Practice behavior outside the event window
- Scoreboard freeze time
- Submission and writeup settings

Save after each logical group of changes, then reload to confirm the values persisted.

## 2. Add a challenge

Open **Challenges** inside the game and create a simple static challenge. Give it:

- A clear title and category
- A description with the exact goal
- An initial score and optional decay settings
- One known flag
- A small attachment only if it is needed

Keep it disabled while testing. See [Create challenges](./challenges) for dynamic containers, A&D, KotH, review, and repository-backed challenges.

## 3. Test as a player

Use the separate test account to:

1. Join the game with the test team.
2. Open the challenge and download any attachment.
3. Submit a wrong flag.
4. Submit the correct flag.
5. Confirm the solve, event feed, and scoreboard.

Never use a production flag in a screenshot, issue, or public support message.

## 4. Prepare participation

If the event uses divisions, create them before teams join. Decide whether participation is automatically accepted or reviewed in **Pending**. An accepted participation can lock the team roster and trigger challenge provisioning, so tell players when their team must be final.

## 5. Publish the event

Before making the game visible:

- Enable only reviewed challenges.
- Verify start, end, freeze, and writeup times in the server's displayed timezone.
- Publish a welcome notice with rules, support contact, and schedule.
- Confirm container capacity and public firewall rules.
- Assign another trusted organizer or monitor.
- Take a fresh backup.

Use [Operate a live event](./live-event) as the event-day checklist.
