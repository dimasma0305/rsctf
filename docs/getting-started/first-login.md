# First login and setup

Use this checklist before inviting players. It takes a few minutes and prevents the most common first-event problems.

## 1. Register the administrator

Open `/account/register?bootstrap=1` and choose a unique password. For Docker
Compose, retrieve the private setup token on the deployment host without
putting it in shell history or a shared log:

```bash
sed -n 's/^RSCTF_BOOTSTRAP_TOKEN=//p' deploy/.env
```

For Kubernetes, use the Secret retrieval command in the Helm notes. Only a
token-authorized registration can become the first **Admin**; public requests
without it fail closed.

::: warning Existing database
“First account” means the first account in the connected PostgreSQL database,
not the first registration after a container restart or reinstall. If you
deliberately wipe the database, rotate the setup token before exposing it again.
:::

## 2. Review the public identity

Open **Admin → Settings** and check the site name, slogan, footer, registration policy, and account activation policy. Save, refresh the public home page, and confirm the changes are visible.

## 3. Decide how people will register

For a private event, disable open registration after creating or importing the required users. For a public event, leave registration enabled and configure the confirmation and anti-abuse controls your event needs.

Environment-backed services such as OAuth, SMTP, and CAPTCHA need server-side credentials. See [Configuration](../reference/configuration); saving similarly named settings in the Admin UI does not configure every startup integration.

## 4. Create a test team and game

Before publishing your real event:

1. Create a normal player account in a separate browser profile.
2. Create a team from **Teams**.
3. As the administrator, create a short test game and one challenge.
4. Join the game with the test team.
5. Submit one wrong flag and one correct flag.
6. Confirm that the solve and scoreboard appear as expected.

If you enabled dynamic challenges, also create and destroy a test container. If you enabled A&D, download a test WireGuard profile and verify the target route from a separate client.

## 5. Make a backup

Create the first known-good backup before importing a large event. See [Back up and update](../deploy/operations).

## You are ready when

- The public URL uses HTTPS for an Internet-facing deployment.
- The first administrator is secured and open registration has the intended policy.
- A player can create or join a team and enter a test game.
- Flag submission and the scoreboard work.
- Container/VPN tests pass if those features are enabled.
- PostgreSQL and uploaded files are included in a backup.

Next, follow [Run your first event](../organizers/).
