# Bring Your Own Container (BYOC) SSH: the `'E'` (exec-shell) stream

Players SSH to the A&D bastion (`ssh <challengeId>@<host> -p 2222`) and get an
interactive shell **in their service container**. For platform-hosted challenges the
bastion `docker exec`s locally. For **self-hosted (BYOC)** challenges the service runs
on the team's own box, reachable only through the team's `rsctf-byoc-agent` —
so the bastion routes the shell over a new multiplexed tunnel stream, `'E'`, and the
agent must run the shell for it.

**Both halves are now implemented and verified end-to-end** (SSH + the admin console
both open a root shell in a live owasp-portal BYOC service):
- server: `services::ad::ssh` (bastion routing + throttle), `services::byoc_tunnel`
  (the `'E'` stream), `hubs::container` (the admin web terminal);
- agent: `rsctf-byoc-agent`'s `'E'` handler — docker-exec's `/bin/sh` in the service
  container over the mounted socket (raw Engine API, no docker CLI/client dep);
- the generated compose names the service container and includes the
  `/var/run/docker.sock` mount as a commented, explicit opt-in.

The rest of this doc is the reference for the wire format + the agent handler.

## Wire framing (rsctf → agent, over a fresh yamux stream)

The tunnel already types every stream by a leading byte — `'S'` service, `'F'` flag.
Add `'E'`:

```
byte 0        : 0x45  ('E')
bytes 1..3    : u16 BE  columns   (initial PTY width)
bytes 3..5    : u16 BE  rows      (initial PTY height)
bytes 5..     : raw PTY bytes, bidirectional, until either side closes
```

The bastion writes the 5-byte header, then bridges the player's SSH channel ↔ the
stream verbatim. Everything after byte 5 is the shell's stdin/stdout/stderr on a PTY.

## Agent handler (Go, add to the stream-type switch)

```go
func handleStream(s net.Conn, serviceContainer string, docker *client.Client) {
    var t [1]byte
    if _, err := io.ReadFull(s, t[:]); err != nil { return }
    switch t[0] {
    case 'S': /* existing: dial the service, io.Copy both ways */
    case 'F': /* existing: read seq+flag, write it into the service */
    case 'E':
        var wh [4]byte
        if _, err := io.ReadFull(s, wh[:]); err != nil { return }
        cols := binary.BigEndian.Uint16(wh[0:2])
        rows := binary.BigEndian.Uint16(wh[2:4])

        // Exec an interactive shell in the SERVICE container (option 1: the agent
        // has the team's Docker socket — see compose below). A TTY of the requested
        // size; prefer bash, fall back to sh — mirrors the platform-hosted path.
        exec, err := docker.ContainerExecCreate(ctx, serviceContainer, types.ExecConfig{
            AttachStdin: true, AttachStdout: true, AttachStderr: true, Tty: true,
            Env: []string{"TERM=xterm-256color",
                fmt.Sprintf("COLUMNS=%d", cols), fmt.Sprintf("LINES=%d", rows)},
            Cmd: []string{"/bin/sh", "-c",
                "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi"},
        })
        if err != nil { io.WriteString(s, "\r\nshell unavailable\r\n"); return }
        att, err := docker.ContainerExecAttach(ctx, exec.ID, types.ExecStartCheck{Tty: true})
        if err != nil { return }
        defer att.Close()
        _ = docker.ContainerExecResize(ctx, exec.ID, types.ResizeOptions{
            Height: uint(rows), Width: uint(cols)})

        // Pipe both directions; either EOF tears the shell down.
        go func() { io.Copy(att.Conn, s) }()   // player keystrokes -> shell stdin
        io.Copy(s, att.Reader)                  // shell output    -> player
    }
}
```

Notes:
- **No `sshd` in the service image** — the agent execs a shell directly, exactly like
  the platform-hosted bastion. The service container just needs `/bin/sh`.
- **Window resize is v1-deferred** on the rsctf side (only the initial size is sent);
  a later `'R'` stream or an in-band resize frame can add it. Not required to ship.
- **No agent-side throttle needed** — rsctf caps shells per team at the bastion
  (5 concurrent + 30/min), verified under load.

## Compose change (option 1: docker-socket-into-agent)

The generated `docker-compose.yml` runs the service + the relay agent. Give the agent
access to the team's Docker daemon and tell it the service container's name:

```yaml
services:
  service:                     # the platform's built challenge image
    container_name: byoc_service
    # ...team's service...

  agent:                       # rsctf-byoc-agent (byoc_image)
    # Use the digest-pinned image emitted by the server bundle.
    image: ghcr.io/dimasma0305/rsctf-byoc-agent@sha256:<release-digest>
    environment:
      RSCTF_BYOC_AGENT_TOKEN: "<adbyocagent:...>"
      RSCTF_BYOC_SERVICE_CONTAINER: "byoc_service"   # what the 'E' handler execs into
    volumes:
      # Explicit opt-in; required only for exec-shell:
      - /var/run/docker.sock:/var/run/docker.sock
    # ...tunnel dial config...
```

**Security note (state it to the team):** mounting the Docker socket grants the agent
root-equivalent control of the team's host. The generated setup leaves this mount
commented out; teams must review and opt in deliberately. Teams that decline it don't get
BYOC SSH (service + flag streams still work); the alternative is `sshd`-in-the-image +
an agent that dials `service:22` instead of exec'ing.

## End-to-end

1. Player: `ssh <challengeId>@<host> -p 2222`.
2. Bastion auths by SSH key → participation; sees the challenge is `ad_self_hosted` →
   opens an `'E'` stream on that team's live tunnel (`open_exec_stream`).
3. Agent execs `/bin/sh` in `byoc_service`, pipes the PTY.
4. Player has a shell in their own service container.
