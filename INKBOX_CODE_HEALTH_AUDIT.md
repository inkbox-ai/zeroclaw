# Inkbox Integration — Code-Health Audit

Audit of the **added code** on the `inkbox` fork branch (our 11 commits on top of
upstream `master`), measured against the repo's own bar:
`docs/book/src/foundations/fnd-006-zero-compromise-in-practice.md` ("The Seven
Disciplines") and `AGENTS.md` ("Anti-Patterns" + the `fmt`/`clippy`/`test` gates).

**Scope** (~2,839 lines):
`crates/zeroclaw-channels/src/inkbox/{inbound,mod,realtime,voice}.rs`,
`crates/zeroclaw-tools/src/inkbox.rs`, plus the wiring in
`orchestrator/mod.rs`, `zeroclaw-config/src/schema.rs`, `attribution.rs`,
`zeroclaw-runtime/src/tools/mod.rs`, the two `lib.rs` files, and the Cargo manifests.

> This is a **findings document**, not a changeset. Nothing below has been fixed yet.

Every finding is tagged with a **repo scope**:
- **`[fork]`** — fix lives entirely in `inkbox-ai/zeroclaw` (this fork). No other repo needed.
- **`[fork + <repo>]`** — the fix *also* requires a change in another repo.
- **`↪ propagate:`** — the *same* bug exists in a sibling integration; fixing it there is
  optional (not required for this fork), listed so it isn't forgotten.

Repos referenced: **inkbox (rust sdk)** = `inkbox-ai/inkbox` `sdk/rust` (the published
`inkbox` crate) · **servers** = `inkbox-ai/servers` (the backend) ·
**hermes-agent-plugin**, **claude-code-plugin** = sibling Inkbox integrations.

---

## Repo scope — at a glance

**~98% of this is fork-only.** Verified facts that keep it that way:
- The server **already signs** the `/incoming-call` webhook with the V2 `X-Inkbox-*`
  scheme our `verify_webhook` checks (`servers` `phone_subapp.py:922-963`), so **S1 can
  fail closed fork-side** — no `servers` change needed (assumes the org has a signing
  key, which our channel config does).
- The SDK already returns `Result` from every call we make and exposes constructible
  `Contact`/types, so all error-handling (E*) and test (T*) fixes are fork-only.

**The only cross-repo item:**
- **S3** has a fork-only *tactical* fix (`0o600` temp file) **and** a cleaner *proper*
  fix that would span **inkbox (rust sdk) + servers** (carry call context natively
  instead of via a local temp file). See S3.

**Optional propagation** (same defects live in siblings, not required here):
- The **`/incoming-call` fail-open** (S1) pattern exists in **hermes-agent-plugin** and
  **claude-code-plugin**.
- The **`{"event":"hangup"}` ignored-frame** hangup bug (already fixed in this fork) is
  still present in **hermes-agent-plugin** and **claude-code-plugin** — they only drop
  the call via WS close. Worth fixing there too.

---

## TL;DR — where we stand

| Area | Status |
|------|--------|
| `cargo fmt --all -- --check` | ❌ **FAILS** (inbound.rs, mod.rs, realtime.rs) |
| `cargo clippy --all-targets -- -D warnings` | ❌ **FAILS** (`collapsible_if` at `tools/inkbox.rs:417`) |
| Unit tests in the Inkbox code | ❌ **ZERO** across 2,839 lines |
| `unwrap()`/`expect()`/`panic!` in prod paths | ✅ none |
| Secrets leaked into logs / error strings | ✅ none (one latent `Debug`-derive risk, see S4) |
| `#[allow(dead_code)]` / underscore-suppressed dead code | ✅ none |
| Dependency hygiene (`inkbox 0.4.9`, no default features) | ✅ clean |

**Two CI gates are red right now** — the branch would not pass `./dev/ci.sh all`.
Counts: **10 MUST**, **18 SHOULD**, **17 NIT**. Cross-repo: **1** (S3, optional proper fix).

---

## 0. Blocking CI gates (fix first — these are mechanical)

- **[MUST] G1 · `[fork]`** — `cargo fmt` fails. Diffs in `inkbox/inbound.rs`,
  `inkbox/mod.rs`, `inkbox/realtime.rs`. **Fix:** `cargo fmt --all`.
- **[MUST] G2 · `[fork]`** — `cargo clippy -D warnings` fails: `clippy::collapsible_if`
  at `crates/zeroclaw-tools/src/inkbox.rs:417`. **Fix:** collapse the nested `if`.

---

## 1. Discipline §4.5 — Security at the Application Layer (highest priority)

- **[MUST] S1 · `[fork]`** — `/incoming-call` is **fail-open** at a trust boundary.
  `inbound.rs:67-92`: on anything other than `Ok(true)` from `verify_webhook` (bad sig
  **or** a verifier `Err`) it logs a WARN and **answers the call anyway** — asymmetric
  with the `/webhook` handler (which 401s). A forged request (if the tunnel host is
  guessed) can make the agent answer a call, spin up the paid OpenAI bridge (cost/DoS),
  and inject an attacker-chosen `call_id`.
  **Fix (fork-only):** reject non-`Ok(true)` with `401`, like `/webhook`. Safe because
  **servers already signs this webhook** (`phone_subapp.py:922-963`, V2 `X-Inkbox-*`).
  ↪ propagate: **hermes-agent-plugin**, **claude-code-plugin** have the same fail-open
  `_on_incoming_call`.
- **[MUST] S2 · `[fork]`** — `call_id` from the unverified payload used unvalidated.
  `inbound.rs:81-86` (`.unwrap_or("")`, no UUID parse / length bound), then interpolated
  into the WS URL and used as the contact-resolution key. **Fix:** parse/validate as UUID;
  decline if absent/malformed.
- **[SHOULD] S3 · tactical `[fork]` / proper `[fork + inkbox (rust sdk) + servers]`** —
  outbound-call context written to a world-readable temp file. `tools/inkbox.rs:151-162`
  writes `purpose`/`opening_message` to `temp_dir()/inkbox_call_contexts/{uuid}.json`
  with default perms (it *is* unlinked on read at `realtime.rs:144`).
  **Tactical fix (fork):** create with `0o600`.
  **Proper fix (cross-repo):** eliminate the temp file entirely — have **inkbox (rust
  sdk)** `place_call` accept a context payload, **servers** persist it on the call record
  and hand it back on the incoming call WS (e.g. via the `?call_id` lookup or a header).
  That removes the local-file trust surface for good.
- **[NIT] S4 · `[fork]`** — `RealtimeConfig` derives `Debug` over `api_key`
  (`realtime.rs:38`); one stray `{:?}` would print the OpenAI key. **Fix:** hand-write a
  redacting `Debug`. *(Separately, if the SDK's own client/config derives `Debug` over its
  api_key, that's an optional **inkbox (rust sdk)** hardening — out of scope here.)*

---

## 2. Disciplines §4.1 / §4.6 — Error Handling & Observability (silent failures)

*All `[fork]` — the SDK already returns the `Result`s; we just need to handle/log them.*

- **[MUST] E1 · `[fork]`** — `inbound.rs:129` `let _ = state.tx.try_send(msg)`: verified
  inbound email/SMS/iMessage dropped on backpressure, no log. **Fix:** `WARN` on `Full`;
  treat `Closed` as shutdown.
- **[MUST] E2 · `[fork]`** — `voice.rs:222` `let _ = state.tx.try_send(cm)`: live-call
  transcript dropped on backpressure, silent. **Fix:** `WARN` with `conn_id`/`turn_id`.
- **[MUST] E3 · `[fork]`** — `realtime.rs:641`: initial `session.update` send failure
  returns silently — the call never configures OpenAI and vanishes. **Fix:** `WARN` before `return`.
- **[MUST] E4 · `[fork]`** — `realtime.rs:613`: `spawn_blocking` `JoinError` dropped by
  `if let Ok(...)`; a panic in caller resolution leaves the model with no identity/contact,
  no log. **Fix:** handle the `Err` arm with a `WARN`.
- **[SHOULD] E5 · `[fork]`** — `realtime.rs:594,598`: `calls().get()` / `contacts().lookup()`
  errors swallowed by `if let Ok` → network error silently looks like "unknown caller."
  **Fix:** `WARN` on the `Err` branches.
- **[SHOULD] E6 · `[fork]`** — `tools/inkbox.rs:900` `_ => return Vec::new()`:
  `build_inkbox_tools` returns zero tools on client-build failure with no diagnostic
  (the *channel* build at `orchestrator/mod.rs:~7090` already WARNs — inconsistent).
  **Fix:** `WARN` with error + identity.
- **[SHOULD] E7 · `[fork]`** — `tools/inkbox.rs:154,160` (`.ok()?`): `write_call_context`
  swallows write failures → context token silently dropped. **Fix:** log the failure.
- **[SHOULD] E8 · `[fork]`** — `realtime.rs:124`: corrupt context-file JSON silently
  becomes empty meta. **Fix:** `WARN` with the token before fallback.
- **[SHOULD] E9 · `[fork]`** — `realtime.rs:918,1027` `let _ = tx.try_send(cm)`:
  consult / post-call dispatch dropped on backpressure → lost post-call actions/reflection.
  **Fix:** `WARN` on `Err`.
- **[SHOULD] E10 · `[fork]`** — `inbound.rs:79`: malformed `/incoming-call` body →
  `Null` → `call_id=""` → answers with a dud URL. **Fix:** `WARN` + decline.
- **[NIT] E11 · `[fork]`** — best-effort calls with no trace: `mod.rs:170` (typing),
  `mod.rs:292` (empty `public_host`), `voice.rs:56` (`speak_to_call` false ignored),
  `realtime.rs:931` (consult timeout). **Fix:** log at `DEBUG`/`WARN`.

---

## 3. Discipline §4.3 — Tests as Design Feedback

*All `[fork]` — pure logic over our own code (T3 builds an SDK `Contact`, which already
has public fields; no SDK change needed).* **Zero tests today.**

- **[MUST] T1 · `[fork]`** — `map_event` (`inbound.rs:188-286`): webhook→`ChannelMessage`,
  `sms:`/`smsto:` selection, message-id fallback. Highest-value untested logic.
- **[MUST] T2 · `[fork]`** — `send()` reply-target routing (`mod.rs:214-245`): the
  `split_once(':')` dispatch, bare→`sms` fallback, unknown→`bail!`. Extract a
  `route_reply_target` helper to test.
- **[SHOULD] T3 · `[fork]`** — realtime pure renderers (`build_instructions`,
  `build_greeting`, `render_contact_card`, `contact_display_name`).
- **[SHOULD] T4 · `[fork]`** — config: `RealtimeConfig::usable`, `InkboxConfig::default()`
  (hand-rolled defaults can drift), `[channels.inkbox.<alias>]` deser, `HasReplyPacing`.
- **[SHOULD] T5 · `[fork]`** — tool arg helpers (`str_arg`/`int_arg`/… + recipients-vs-
  conversation XOR validation).
- **[SHOULD] T6 · `[fork]`** — `resolve_party` / `with_party_marker` (`inbound.rs:150-182`).

---

## 4. Disciplines §4.4 / §4.7 — Technical Debt & Working Above the Floor

*All `[fork]`.*

- **[SHOULD] D1 · `[fork]`** — `#[allow(clippy::too_many_arguments)]` on
  `run_realtime_bridge` (`realtime.rs:557`, 9 args). **Fix:** `BridgeContext` params struct.
- **[NIT] D2 · `[fork]`** — `now_secs()` duplicated in `inbound.rs`, `voice.rs`,
  `realtime.rs`. Hoist to a shared helper.
- **[NIT] D3 · `[fork]`** — 16 near-identical tool `execute` bodies; the
  `Ok(serde_json::to_value(x)?)` tail repeats ~12×. Optional: fold serialization into `run`.
- **[NIT] D4 · `[fork]`** — `zeroclaw-runtime/src/tools/mod.rs:693` binds tools to the
  *first* enabled `channels.inkbox` identity via nondeterministic `HashMap` order.
  **Fix:** sort by alias, or document the single-identity assumption.
- **[NIT] D5 · `[fork]`** — `InkboxConfig.excluded_tools` not threaded into
  `build_inkbox_tools`. Matches sibling pattern (downstream policy filter) — **verify**
  Inkbox tools flow through that filter; else the field is dead for Inkbox.
- **[NIT] D6 · `[fork]`** — hardcoded greeting literal `voice.rs:196`
  (`"Hi there, how can I help?"`). Extract to a named `const`.

---

## 5. Discipline §4.2 — Public API Surface as a Promise

*All `[fork]`.*

- **[NIT] A1 · `[fork]`** — `CallMeta` public fields partly undocumented (`realtime.rs:60-77`).
- **[NIT] A2 · `[fork]`** — `RealtimeConfig` is `pub` but module-internal (`realtime.rs:39`)
  → `pub(super)`/`pub(crate)`.
- **[NIT] A3 · `[fork]`** — the 16 tool structs are `pub` but constructed only in
  `build_inkbox_tools` → `pub(crate)`.
- **[NIT] A4 · `[fork]`** — `AppState.identity` missing `///` (`inbound.rs:36`).
- **[NIT] A5 · `[fork]`** — `router` / `ws_handler` broader than needed (`inbound.rs:45`,
  `voice.rs:63`) → `pub(crate)`.
- **[NIT] A6 · `[fork]`** — reply-pacing field docs thinner than the `IMessageConfig`
  sibling (`schema.rs:~11960`).
- **[NIT] A7 · `[fork]`** — bloated `# Arguments`/`# Returns` docstring on the private
  `InkboxCtx::run` (`tools/inkbox.rs:~35`).

---

## Recommended order of attack

1. **Unblock CI** `[fork]` — G1 (`cargo fmt --all`), G2 (collapse the `if`). Minutes.
2. **Close the security holes** `[fork]` — S1 (fail-closed), S2 (validate `call_id`),
   S3 tactical (`0o600`). All fork-side.
3. **Make failures visible** `[fork]` — E1–E4 (MUSTs), then E5–E10.
4. **Add the test floor** `[fork]` — T1 + T2 first, then T3–T6.
5. **Debt & polish** `[fork]` — D1, then the NITs.
6. **Cross-repo, when you want it** — S3 proper fix in **inkbox (rust sdk)** + **servers**;
   propagate S1 (and the hangup fix) to **hermes-agent-plugin** + **claude-code-plugin**.

## What's already good (don't regress)

- No `unwrap`/`expect`/`panic`/`todo`/`unimplemented` in production paths.
- No secrets in logs or error strings; no `#[allow(dead_code)]` / underscore-suppressed code.
- The **channel** client build (`orchestrator/mod.rs`) handles build-error *and*
  thread-panic with a WARN-and-skip — matches the repo's existing pattern.
- Dependency hygiene: `inkbox = "0.4.9"`, `default-features = false`, only
  `tunnels-runtime` where needed; the ~19 transitive crates are the TLS/crypto stack the
  tunnel data plane genuinely needs.
- `InkboxConfig` matches sibling-channel shape (derives, serde attrs, `#[secret]`,
  `impl_reply_pacing!`, presence counting, registration); all realtime fields are
  actually consumed (no speculative flags).
