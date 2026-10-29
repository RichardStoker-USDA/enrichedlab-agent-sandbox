# Setup questionnaire

Ask only what is still unknown after read-only host inspection.

## Required decisions

1. Is this a single-operator local controller, a researcher client for an existing remote broker, or a site-owned
   shared broker?
2. Where will sandboxes run?
   - Apple Silicon macOS with OrbStack or another Docker engine
   - Linux with Docker Engine
   - NVIDIA OpenShell direct VM
   - Kubernetes with the Agent Sandbox controller and a named RuntimeClass
   - WSL2 for an experimental Linux qualification
3. What host folder may hold sandbox work? It must be a dedicated absolute path, never the home directory.
4. Is inference local, remote through an approved private route, or not needed yet?
5. Which outbound domains are needed initially? Default to none beyond the selected policy template.
6. Should the loopback dashboard run as a user service? A remote broker does not expose this operator surface.
7. Should the portable skills be linked into Codex, Claude Code, OpenCode, Pi, Copilot CLI, or another explicit skill
   directory?
8. Is the work ordinary development, sensitive data analysis, or offline? This selects the policy profile.
9. For Kubernetes, which kubeconfig path, context, namespace, and RuntimeClass does the operator approve for read-only
   observation? Peer NetworkPolicy writes are a separate opt-in.
10. Should provenance use local embedded SQLite or an existing PostgreSQL service? Default to SQLite for one user or
   one controller. PostgreSQL needs a tenant ID, separate runtime and migration identities, TLS off loopback, and its
   own live qualification.
11. For a remote researcher, what HTTPS broker origin, research-group key, profile, and owner-only download directory
    did the site operator assign? For a broker operator, what private GitOps overlay and Access application own those
    values?

## Facts to inspect instead of asking

- operating system and architecture;
- whether Docker, OrbStack, Podman, OpenShell, Rust, Git, OpenSSL, `kubectl`, and Helm exist;
- Docker engine health and current context;
- OpenShell version, registered gateways, and advertised compute driver;
- whether the intended workspace exists and whether it is already managed;
- whether old EnrichedLab Agent Sandbox state can be migrated;
- whether the dashboard port is already owned;
- whether a requested skill target already exists.

## Choices that need explicit consent

- installing or updating NVIDIA OpenShell;
- enabling a persistent user service;
- copying existing controller state to a new product path;
- adding any LAN listener, client certificate, public domain, host mount, peer link, or Kubernetes write grant;
- creating or migrating a PostgreSQL schema, changing database grants, or adding a shared provenance tenant;
- creating an Access application or tunnel, publishing a controller image, activating a broker Flux child, or adding
  and removing research-group invitations;
- using a cloud API that incurs separate billing;
- deleting a sandbox workspace or old controller state.

Do not ask for a credential value. Ask for the owner-managed path or provider setup the runtime will reference.
