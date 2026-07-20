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

The tunnel types every stream by a leading byte — `'S'` service, `'F'` flag, and
`'E'` exec. The flag stream is bidirectional so publication is never inferred
from a successful socket write:

```
rsctf -> agent : 0x46 ('F') | u64 BE sequence | flag bytes | write-half close
agent -> rsctf : 0x41 ('A') | the same u64 BE sequence
```

The agent sends the nine-byte ACK only after the temporary flag file has been
renamed over the live file. A missing or mismatched ACK fails the bounded
publication attempt. The sequence is the positive durable `AdRounds.id`, so it
does not reset with an rsctf process. Identical retries keep the same sequence,
and a relay endpoint retains at most one 4096-byte current value for immediate
reconnect replay. After a server restart, activation loads the latest exact
game/participation/challenge flag directly from durable round data; it does not
require a prior successful-delivery receipt. A lower sequence cannot replace a
newer concurrent push, and equal sequences must contain identical bytes.
Participation/challenge revocation discards the retained value. Server and agent
negotiate the `X-RSCTF-BYOC-Protocol: rsctf-byoc-v2` WebSocket
upgrade header. A v2 server rejects an agent that does not offer that capability with HTTP 426 before it
publishes a service endpoint. A replacement session is not available to service
or exec forwarding until it ACKs the exact retained sequence; a late ACK from a
superseded or revoked session is ignored.

### Agent-first v2 rollout

The transition is deliberately one-way compatible: a v2 agent sends the
capability header but accepts an old server that ignores it. A v2
server does not accept an old ACK-less agent. Roll out in this exact order:

1. Publish the v2 agent image and record its immutable multi-platform digest.
2. Before changing the server, replace the pinned agent digest in every running
   team's Compose bundle and run `docker compose pull rsctf-agent` followed by
   `docker compose up -d rsctf-agent`. Existing bundles are digest-pinned and do
   not update themselves.
3. Verify those agents reconnect and continue serving flags through the old
   server. The old server ignores the offered header; this is expected.
4. Deploy the server image compiled with that same workflow's agent digest.
   Any missed old agent now gets HTTP 426 instead of being published and later
   producing ambiguous delivery evidence. Regenerate its team's bundle with the
   current agent digest.

Official server images receive the matching digest through the image workflow's
`RSCTF_DEFAULT_BYOC_AGENT_IMAGE` build argument. A direct source/local server
build intentionally has no baked-in fallback. Set
`RSCTF_AD_BYOC_AGENT_IMAGE=registry.example/rsctf-byoc-agent@sha256:<digest>` at
runtime or compile with the same immutable build argument; otherwise the BYOC
setup/Compose endpoints fail closed instead of emitting an older incompatible
agent.

The exec stream uses:

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
    case 'F': /* existing: atomically install seq+flag, then ACK 'A'+seq */
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
