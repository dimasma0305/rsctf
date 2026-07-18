---
layout: home

hero:
  name: "rsctf"
  text: "Run a CTF without fighting the platform"
  tagline: Clear guides for players, organizers, and operators—from the first login to Jeopardy, Attack & Defense, and King of the Hill events.
  image:
    src: /logo.svg
    alt: rsctf continuous RS monogram
  actions:
    - theme: brand
      text: Install rsctf
      link: /getting-started/install-wizard
    - theme: alt
      text: Read the player guide
      link: /players/

features:
  - icon: "01"
    title: Guided installation
    details: Answer a few questions and let the wizard generate secrets, validate Docker or Kubernetes, and start the platform.
    link: /getting-started/install-wizard
    linkText: Open installation guide
  - icon: "02"
    title: Built for real events
    details: Learn the complete flow for accounts, teams, games, challenges, scoring, live operations, and recovery.
    link: /organizers/
    linkText: Run an event
  - icon: "03"
    title: Deploy anywhere
    details: Use Docker Compose on one server or the Helm chart in Kubernetes, pulling ready-to-run multi-architecture images from Docker Hub.
    link: /deploy/
    linkText: Compare deployments
  - icon: "04"
    title: Searchable and accessible
    details: Fast local search, mobile navigation, dark mode, visible keyboard focus, semantic pages, and no documentation server to maintain.
    link: /reference/troubleshooting
    linkText: Find help
---

## Find the right guide

<div class="quick-paths">
  <a href="./players/">
    <strong>I am playing</strong>
    <span>Create an account, join a team, submit flags, and understand each game mode.</span>
  </a>
  <a href="./organizers/">
    <strong>I am organizing</strong>
    <span>Create games and challenges, prepare teams, and operate a live event.</span>
  </a>
  <a href="./deploy/">
    <strong>I run the server</strong>
    <span>Install, secure, back up, update, and troubleshoot rsctf.</span>
  </a>
</div>

::: tip New installation?
Start with the [installation wizard](./getting-started/install-wizard). It creates unique secrets and checks the deployment before it starts any services.
:::
