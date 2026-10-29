---
name: enrichedlab-agent-qualify
description: Qualify an EnrichedLab Agent Sandbox source commit and installed runtime. Use after installation, upgrade, runtime or platform changes, before a release claim, and when checking dashboard, isolation, service forwarding, cleanup, evidence, or disclosure boundaries. Produces scoped evidence without treating a running container as proof.
license: MIT
---

# EnrichedLab agent qualification

Source checks need the repository test toolchain. Live checks also need a configured NVIDIA OpenShell gateway and the
selected runtime.

Read [references/qualification_matrix.md](references/qualification_matrix.md) and run only the rows that apply to the
selected platform and release claim. Unknown and skipped are not passes.

## Source gate

Run from a clean exact commit:

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
cargo audit
scripts/validate_agent_skills.sh
scripts/scan_public_boundary.sh
```

Also parse every shell script, check JavaScript syntax, render both user-service formats, and inspect the final diff.
The disclosure scan covers tracked source and every reachable commit. Put deployment-specific names, usernames, host
labels, addresses, and old product coordinates in an owner-only file outside the repository, then set
`PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE` to that path. Do not print the matching content. The built-in checks also reject
non-placeholder home paths, private-key headers, and common token prefixes.

## Live gate

1. Run `agent-sandbox doctor` and inspect the exact gateway and driver it observed.
2. Run `scripts/qualify_sandbox.sh --profile <profile>` with a route only when model inference is part of the claim.
3. Confirm the disposable workspace, runtime object, service leases, policy additions, and controller record are gone.
4. Check the host listener list. The dashboard and gateway use their declared loopback addresses. No LAN listener may
   exist without a current mTLS lease.
5. Run one allowed action and one denied host/private-network action from the same sandbox.
6. Capture and verify evidence, then verify the host audit chain.
7. For an upgrade, restart the user service and prove it uses the candidate binary and new private path while the old
   state remains available for rollback.
8. For the automatic state claim, run a synthetic research worker. Verify a fresh provider session per segment, no
   continuation argument, machine tool and token events, output-version custody, a checkpoint epoch, and a restricted
   controller-log artifact for Pi and OpenCode. Inject malformed and truncated fixtures in source tests and confirm
   they fail.
9. Run direct Pi and OpenCode fixtures or disposable runs. Confirm each returns a provenance run ID, emitted reasoning
   stays restricted, and a failed run still records a terminal event.
10. When PostgreSQL is selected, use separate migration and runtime roles. Test tenant crossing, event update/delete,
    TLS enforcement, evidence-lease scope and expiry, concurrent append order, hash-chain verification, and backup or
    restore before a shared deployment claim.
11. For a named-agent continuity claim, run
    `scripts/qualify_continuity.sh --profile <profile> --route <private-route> --exercise-harness`. It retains the
    retired qualification identity and evidence but leaves no active runtime. Verify both runtime generations and the
    restored lesson before accepting the pass. If the installed controller uses custom paths, pass the matching
    `--root`, `--state-dir`, `--config-dir`, `--openshell-bin`, and `--docker-bin` values. A mixed path set is a failed
    qualification.

## Dashboard gate

Use the browser the user named. Check initial load, live SSE revision, stable revision when only probe time changes,
stale detection, reconnect, clean shutdown with an open stream, every navigation target, and theme persistence. Check
approval settings, the bounded request queue, research-team state, Agent state trace, global metadata search, paged
Audit search, and Provenance replay. Pan and zoom the graph and confirm lane labels stay attached to their rows. Check
the initial fit, minimap navigation, run selection, filters, time controls, baseline comparison, a purpose-bound
evidence lease, restricted evidence search, one restricted read, narrow viewport containment, and a denied
cross-origin request. Confirm the default state, search, and graph responses contain no collection value, restricted
event content, or artifact byte. Test light and dark themes at phone, tablet, and desktop widths. Do not substitute a
headless or different browser when the requested browser connection is unavailable. Record the browser gate as
blocked and keep the rest of the release evidence separate.

## Claim wording

- Name the host, runtime, gateway, OpenShell version, controller commit, and observed time.
- A Kubernetes pass binds the Sandbox CR, Pod owner, requested RuntimeClass, observed handler, policy objects, and a
  trusted node/runtime receipt. API binding alone is not microVM attestation.
- An isolation sentinel proves only the exact tested paths and addresses at that time.
- A package inventory is observed state, not a byte-for-byte download history.
- A valid hash chain cannot detect clean tail truncation unless its last hash is anchored elsewhere.
- NIST CSF and AI RMF mappings are engineering crosswalks, not certification or authorization.

Return the receipt template from the reference. If any required row is blocked, say what external change is needed.
