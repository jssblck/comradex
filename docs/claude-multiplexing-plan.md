# Claude account multiplexing

Investigated and implemented September 25, 2026. Local validation only; not deployed.

The selected design is a native Claude Code account router with quota-aware selection and controlled account rollover. It uses Comradex's existing routing model. The review considered a launcher with permanent profile binding, but rejected that product expansion. Comradex does not shim the Claude CLI or manage inference processes.

CLIProxyAPI (CPA) is a useful implementation reference, particularly its distinction between native client state and selected credential identity. Copying its entire executor would add unrelated translation and cloaking behavior, including several behaviors that have caused native-client regressions.

The implementation accepts genuine Claude Code request shapes and subscription OAuth. It does not implement CPA's foreign-harness cloaking, prompt injection, tool renaming, beta reconstruction, or synthetic device identities. Request-shape validation cannot cryptographically attest the caller.

## Implemented behavior and validation limits

- Separate `claude_home` and `claude_inbound` accounts, homogeneous pools, loopback listeners, and an independent HTTP handler. Codex context and WebSocket handling remain separate.
- Official isolated `claude auth login`, native Keychain/file import, atomic private credential persistence, refresh serialization, and failure cooldowns. The relay owns the imported refresh grant; the login profile is not an inference profile.
- Menu-driven browser login uses the same official executable and isolated store, with account admissions paused during sign-in. The CLI remains available. Background renewal shares the request-time refresh lock and rotating grant.
- Managed usage polls the native OAuth `/api/oauth/usage` endpoint, identified as Comradex management traffic. It normalizes shared 5-hour/7-day percentages, handles inactive windows, respects management-endpoint cooldowns, and keeps model-specific limits out of whole-account routing.
- `auto_activate_claude_usage` separately opts into reset warming. A durable ledger tracks owner-scoped cycles and uncertain attempts. The worker invokes genuine Claude Code with Haiku and a tiny prompt, no tools, no saved conversation, and a temporary profile containing the selected account's real identity and access token. It never constructs inference JSON or supplies a refresh grant to that subprocess. No user-session launcher or CLI shim is introduced.
- Installation changes only `env.ANTHROPIC_BASE_URL`, with a secret path and reversible settings record. Pooling still substitutes credentials upstream; BASE_URL-only configuration does not imply untouched upstream credentials.
- Sticky session selection with prefer/preserve, exact model pins, durable credential-identity evidence, and shared-quota rollover before returning response headers. Live sibling streams prevent migration. Count-token success never establishes ownership of generated state.
- Raw request span edits for account/device identity. An existing CCH uses the reference algorithm for the reviewed 2.1.220-2.1.281 range; unknown versions/formats fail rewriting. An absent CCH remains absent. No thinking signatures are repaired.
- Raw responses, errors, and SSE, including unknown events. Only explicit shared 5h/7d rejection/reset headers authorize quota rollover. Model/mode-specific and ambiguous limits pass through. No automatic cross-account recovery from 400/401/403/5xx, transport loss, or partial streams.
- Signed thinking, compaction, opaque resources, and predecessor fields remain pinned because cross-account portability has not been validated. The handler does not erase these fields to force a migration.
- Messages and token counting use session placement. Models honor an explicit `models_account`; otherwise models and the startup probe retain inbound credentials. Unsupported endpoints are rejected. Missing native session identity fails closed.

Local evidence includes byte-exact synthetic forwarding, selected-identity byte diffs, a CPA checksum vector, concurrent refresh and rejected-grant tests, quota rollover/stickiness, stream overlap, durable ownership, changed-identity rejection, provider isolation, and reversible settings tests. A synthetic capture from installed Claude Code 2.1.281 exercises its actual request shape against a local mock without real credentials or upstream inference. No captured native prompt or credential is checked into the repository.

Baseline validation on JessBook: `cargo test --locked` passed 493 enabled tests; the synthetic native-capture replay passed separately; `swift test` passed 38 menubar tests. An isolated CLI smoke test covered `check`, Claude settings install, daemon startup, rejection of foreign requests, status, graceful shutdown, and uninstall restoration. This baseline was committed as `f1f10f2` without changing the existing service or real credentials.

Feature-parity validation passed 502 enabled Rust tests and 39 menubar tests, including browser-login status, preferred/preserved routing, management-endpoint cooldowns, concurrent foreground/background token rotation, and durable reset warming. The installed Claude Code 2.1.281 produced one successful native warming request against a local mock with the exact production CLI flags. Formatting, Clippy with warnings denied, and diff checks passed.

Live validation then used two distinct user-authorized subscription accounts on an isolated daemon. Both official browser sign-ins completed and produced private managed credential files. Native Claude Code completed new and resumed conversations through both accounts. New work followed prefer/preserve changes; existing conversations retained their owner. A second-account conversation also resumed on its original owner after a daemon restart with the first account preferred. Real OAuth refresh preserved account/device identity and persisted the credential; the production native warming function then completed a real Haiku request using it.

Live inference exposed a partial-observation bug: a response reporting only 5-hour usage erased the last known weekly value. A failing regression reproduced it; merging the other shared window after verifying credential ownership fixed it. Both windows then remained visible after live inference. The final Rust suite passed 503 enabled tests, and Clippy, formatting, and diff checks passed. The existing Comradex service was not modified. The separate Codex live-backend test remains ignored.

The opt-in `live_managed_claude_refresh_and_warming` test requires `COMRADEX_LIVE_CLAUDE_HOME` pointing at a Comradex-owned Claude account directory. Running it with `cargo test --locked --lib live_managed_claude_refresh_and_warming -- --ignored` rotates that grant and sends one native Haiku prompt; run it only with explicit authorization for that account. Ordinary test runs skip it. Natural reset timing and real quota-triggered rollover were not exercised: the initially limited account's reset had passed by live validation. Those paths retain their controlled-test evidence and conservative ownership rules.

The implementation uses ordinary native-tls HTTP/1.1 for inference and rustls for OAuth. Header values and application bytes are preserved subject to the stated edits; HTTP framing, header order, and TLS fingerprints are not identical to a direct Claude connection. There is no CPA-style TLS impersonation. Real quota-triggered rollover, all helper and IDE surfaces, direct-only native feature checks, and cross-account signed-context portability remain unverified. These limits must remain visible before release; successful live requests are not a measured ban-risk result.

## Evidence and limits

Reviewed Comradex `795562efe5ccb7b61b013f2c7599c85a6b38a214`, CPA `9bdde54b59d1af70ae0534a0ef61b2c3361a1257`, CPA history and issue reports, and current Anthropic documentation. The reference checkout is `~/projects/scratch/CLIProxyAPI`. The installed Claude Code reports `2.1.281`; CPA's current declared baseline is `2.1.280`.

The audit covers consequential actions in Claude OAuth authentication, request preparation, transport, responses, and routing. It is not an audit of every CPA provider or every generic server function.

Evidence labels below:

- **Code:** verified in the pinned implementation and, where noted, its tests. Tests were inspected, not executed during this investigation.
- **History:** an explicit commit or PR rationale, sometimes with author-reported captures. This is evidence of implementation intent, not our independent reproduction.
- **Report:** an upstream issue's reproduction, paired with the relevant fix where available.
- **Inference:** our engineering judgment; requires validation before becoming a compatibility rule.

CPA's main wire-alignment [PR #4752][wire-pr] reports captures, local tests, and selected successful upstream interactions. It does not measure account suspension rates or prove that every fingerprint adjustment is required. Successful requests and long-term account safety are different claims.

Anthropic documents subscription-authenticated Claude Code using a gateway when only `ANTHROPIC_BASE_URL` is set. That preserves the client's saved login; it does not select another account. A proxy replacing that login's bearer with a pool credential is an additional operation, even if it happens only once per conversation. Permanent binding removes migration concerns, but does not by itself remove credential substitution. The credential policy restricts third-party subscription-token intermediation; the cited sources do not establish a suspension-risk guarantee for pooling. [Gateway authentication][gateway], [credential policy][policy].

We cannot infer Anthropic's private abuse or distillation classifiers from geography, a TLS fingerprint, or CPA's popularity. The engineering objective is coherent, ordinary use of genuine Claude Code, without manufactured tools, identities, conversations, or traffic.

## Rejected alternative: permanent session binding

Permanent binding reduces engineering risk by removing cross-account continuation, migration races, and account-local resource portability from the normal path. The additional reduction in intervention comes from choosing the account before the client constructs its first request.

| Design | What happens on the first request | Remaining intervention |
| --- | --- | --- |
| Only set `ANTHROPIC_BASE_URL`; forward unchanged | Claude Code sends its existing account A login. | A gateway, but no automatic selection of account B. A new conversation still uses the active login. |
| Proxy selects B once and pins the conversation | Claude Code sends A's request; Comradex replaces credentials for B. | Credential storage/refresh and account-metadata consistency still need handling, even without later switching. |
| Select a native profile before launch; then forward unchanged | Claude Code starts already signed into the chosen account and constructs its own request. | Local process/profile selection and optional gateway transport; no proxy-side token substitution or body rewriting. |

The third design would suit permanent binding, but is outside the selected product scope:

1. Sign each account in through official Claude Code using a separate profile directory.
2. Select an eligible profile before launching a new Claude Code process, using Comradex's prefer/preserve policy and observed quota state.
3. Launch with that `CLAUDE_CONFIG_DIR` and, if gateway observation is wanted, `ANTHROPIC_BASE_URL`.
4. Let Claude Code own authentication, refresh, account metadata, and all direct auxiliary requests.
5. Forward its bearer, headers, and body without identity or checksum edits; observe response limits without persisting tokens.
6. Persist the conversation-to-profile binding and reopen resumed conversations in their original profile, including after quota exhaustion or daemon restart.

Anthropic documents that `CLAUDE_CONFIG_DIR` separates the credential file and macOS Keychain entry. It is an additional local configuration setting, so this is not literally an `ANTHROPIC_BASE_URL`-only setup. The lower-intervention property is that native Claude Code, rather than the proxy, selects and uses its own saved credential. [Credential storage][auth-docs].

Starting a new conversation inside an already-running process is not sufficient to select a different profile automatically. The launcher must start another process when choosing another account; `/clear` must not be treated as authorization to replace the live process's credential. Resume and fork behavior, settings/plugins across profiles, and supported IDE launch surfaces need explicit validation. Do not copy an old conversation into a different profile and call it new work.

A quota rejection passes through. The user can wait or launch a fresh conversation on another eligible account. Unknown or stale quota is not proof of available capacity; a newly selected account can still reject its first request. Native same-account retries remain native behavior, but Comradex never silently changes accounts.

With this design, proxy-managed OAuth refresh, account metadata edits, CCH regeneration, synthetic device identities, and cross-account continuation tests are unnecessary. Test exact forwarding, profile isolation, durable binding, resume behavior, and quota-aware new launches instead. A gateway still terminates and recreates transport connections. If proxy observations are unnecessary, direct native connections remove that extra layer, but automatic selection would then need another source of quota information.

Its possible benefit follows from fewer changed components and closer alignment with documented native login/gateway behavior, not measured ban rates. The user chose proxy pooling because launcher/profile management would be a larger departure from Comradex.

## Why CPA performs its actions

The action audit and Comradex decisions in this section concern the selected credential-substituting router.

### Credential and session handling

| Action | Root cause and evidence | Comradex decision |
| --- | --- | --- |
| OAuth authorization, PKCE, state checks, refresh, and profile/roles requests | OAuth establishes and renews access; profile data supplies identity that cannot be recovered by assuming Codex-style JWT claims. The roles lookup mirrors observed native login behavior. **Code/History:** [OAuth implementation][oauth], [wire PR][wire-pr]. | Invoke official Claude Code for interactive login. Add a Claude-specific resolver for managed accounts. Do not copy OpenAI token parsing or its device-login flow. |
| Deduplicate refresh and honor refresh backoff | Concurrent requests can refresh the same grant repeatedly; retrying a refresh 429 creates a loop. Commit [6431cec7][refresh-fix] adds coordination/backoff; `TestRefreshTokens_DeduplicatesConcurrentRefresh` and `TestRefreshTokensWithRetry_429BlocksImmediateReplay` pin it. **Code/History.** | One refresh owner per managed account, cross-process locking, reread after acquiring the lock, bounded timeout, atomic persistence, and provider-directed backoff. |
| Persist account identity and preserve it across refresh | Refresh responses may omit optional profile fields. Clearing those fields changes identity even though authentication succeeded. A failed advisory lookup must not discard newly rotated tokens. **Code:** [OAuth tests][oauth-tests]. | Persist real account/organization identity; keep it when absent from a refresh response. Quota and affinity ownership follow stable identity, not bearer text. |
| Rewrite `metadata.user_id` for the selected credential | Native software can arrive with account A's metadata while CPA selected account B. CPA writes selected-account identity and aligns session metadata with its session header. This is an explicit native-path exception. **Code/History:** [credential metadata][identity], [wire PR][wire-pr]. | Adopt the separation of authorities. Preserve native session and device identity where applicable; replace account identity only when selection requires it. Specify and test each changed field. |
| Use one persisted device ID per credential | CPA reduces legacy pools to one ID, preventing per-request device churn and inconsistent concurrent updates. The code proves stability, not a ban-prevention effect. **Code:** [identity storage][device-id]. | Use real device identity from the native profile/caller. Do not generate a rotating device pool. Keep managed account identity stable across refresh and daemon restart. |
| Generate an account UUID when profile access fails | Setup tokens can lack profile scope. CPA accepts certain profile 403s and derives a stable substitute rather than failing inference. **Code:** [auth preparation][auth-prepare], [fix 8aa6868d][profile-fix]. | Do not fabricate account UUIDs. Managed pooling requires verified account metadata. An unknown-identity credential may remain a same-account inbound pass-through, without becoming a managed replacement. |
| Resolve session IDs and apply session affinity | Unstable IDs lose cache locality and mix continuation state. CPA prefers explicit IDs; its optional affinity selector also has content-derived fallbacks and model-specific keys. **Code:** [session extraction][sessions], [selector][selector]. | Prefer native session/agent lineage. Use explicit persistent routing keys, not prompt hashes or CPA's general-purpose conversation tree machinery. |
| Reject duplicate identity fields and synchronize metadata access | Choosing the first or last duplicate key can route and authenticate different interpretations. Shared metadata writes can race. **Code:** [identity tests][identity-tests], [hardening commit][identity-hardening]. | Reject ambiguous fields needed for routing or rewriting. Use immutable request credentials and generation checks. Do not reject unrelated future body fields. |

### Request bodies and headers

| Action | Root cause and evidence | Comradex decision |
| --- | --- | --- |
| Detect native clients before deciding whether to cloak | CPA serves arbitrary harnesses and tries to keep genuine Claude Code out of its reconstruction path. Exact-version detection misclassified newer native clients. **Report/History:** [#5715][detection-report], [#5820][detection-fix]. | Dedicated native Claude listener; never turn an unfamiliar version into a fabricated older Claude client. Headers are compatibility signals, not authentication. |
| Inject a Claude identity prompt, relocate caller system text, insert dates, and obfuscate configured words | These were introduced to disguise non-Claude callers. Later changes tried to preserve their original instructions while constructing a native-looking top-level prompt. **History:** [00280b6f][cloak-origin], [wire PR][wire-pr]. | Omit. The actual Claude Code client already supplies its system content. No word obfuscation, copied native prompt, or synthetic date. |
| Rename tools to Claude names or synthetic MCP aliases, then reverse the names in history/responses | An older commit reports different billing behavior for third-party tool names; later work generalizes aliases for foreign harnesses. The current executor gates aliases on cloaking, not confirmed native traffic. **History/Code:** [e8d1b79c][tool-origin], [#4670][tool-request], [stream pipeline][stream]. | Omit. Preserve genuine tool declarations, names, IDs, schemas, references, and tool results exactly. Supporting arbitrary harnesses is outside this design. |
| Add context-management instructions, fallback models, thinking-display settings, and diagnostics | The cloaked path must construct features its foreign caller did not supply and approximate newer native behavior. Some fixes undo invalid combinations created during that process. **Code:** [cloaking][cloaking], [stream pipeline][stream]. | Forward native fields. Do not add models, change thinking visibility, or activate context management on the client's behalf. |
| Rebuild beta headers and lift body betas into headers | Translation changes the body, so CPA assembles matching capability headers. A pinned list dropped newer client capabilities and caused 400s. **Report/History:** [#5738][beta-report], [d9b8fdb][beta-fix]. | Forward the complete native beta header, including unknown values and ordering. Use native subscription auth at the client; do not introduce a dummy API-key mode that then requires reconstructed OAuth betas. |
| Add, limit, reorder, or change cache markers and TTLs | Translated/cloaked clients may not supply cache markers or may violate upstream constraints. Applying defaults to native traffic removed explicitly requested subagent TTLs. **Report/History:** [#5629][ttl-report], [6a73f39][ttl-fix]. | Preserve native cache configuration. Routing must accept a cold cache after a real account change rather than rewriting the conversation to recover it. |
| Stabilize billing prompt prefixes | CPA's generated attribution changed with later user messages, invalidating cached prefixes. A report separately measured repeated cache writes on the cloaked path. **Report/History:** [#5730][cache-report], [377c315][cache-fix]. | Preserve native attribution and its position. Do not regenerate its session-wide fields on every turn. |
| Finalize the `cch` request checksum after all edits | CPA treats CCH as dependent on final serialized request bytes. Native custom-base-URL requests omit it, so CPA also inserts it to recreate the direct first-party shape. **Code/History:** [signing][signing], [known-vector tests][signing-tests]. | Separate checksum correctness from checksum synthesis. If a supported incoming checksum exists and credential edits affect it, recompute after the final edit. For native gateway requests without it, preserve absence initially; adding it requires evidence that this supported gateway path needs it. |
| Sanitize foreign thinking signatures and normalize model/thinking/sampling/max-token fields | Translation can carry another provider's signatures or parameter combinations Anthropic rejects. These functions can run even when cloaking is skipped. **Code:** [executor][executor], [request pipeline][stream]. | Omit for native Claude traffic. Preserve signed blocks and parameters; return upstream capability errors so Claude Code can handle them. |
| Restore hidden thinking from cached history | CPA supports Claude-compatible API-key backends whose clients lose thinking between turns. This path explicitly excludes Claude OAuth credentials. **Code:** [thinking replay][thinking-replay]. | Omit. It is unrelated to native subscription multiplexing. |
| Treat `count_tokens` separately | Generation-only fields are not necessarily accepted by token counting. CPA's native token-counting branch avoids the full Messages cloak and has explicit field handling. **Code:** [token counting][tokens]. | Forward the native count request with selected authentication. Do not add Messages metadata, attribution, diagnostics, or generation defaults. Use the same account placement when session evidence is available. |
| Preserve or synthesize User-Agent/Stainless/agent headers; optionally stabilize software profiles | Foreign callers need a synthetic profile under CPA's policy. Optional stabilization prevents profile churn; it is disabled by default. Native headers also carry real subagent and environment state. **Code:** [headers][headers], [device profiles][profiles]. | Preserve genuine caller software and agent headers. No cached older version substituted for a client upgrade, invented OS, or profile-learning dependency. |

### Transport, responses, and scheduling

| Action | Root cause and evidence | Comradex decision |
| --- | --- | --- |
| Match TLS ClientHello, ALPN, HTTP version, header casing, and order | Early history attributes failures/detection to Go's transport shape and first used a browser profile. Later work replaces approximation with a measured Claude profile. This establishes intent and fixture compatibility, not which details Anthropic enforces. **History/Code:** [249f9691][tls-origin], [f3e25ab2][wire-origin], [transport][transport]. | Give Claude a separately measured transport. Do not reuse Comradex's Codex TLS profile or automatically copy CPA's old macOS profile onto every current platform. Preserve what the native client actually supplies and document unavoidable relay differences. |
| Pool connections and enable TLS resumption | Reduces connection overhead; implementation must keep request boundaries correct across partial writes, compression, and connection reuse. **Code/History:** [transport][transport], [70793491][resumption-fix], [wire PR][wire-pr]. | Reuse connections normally. Test cancellation and framing. Do not treat connection isolation or resumption as a claim of account anonymity. |
| Strictly gate first-party behavior by origin | A custom or lookalike upstream must not inherit Anthropic-specific transport or credentials. **Code:** [origin check][origin]. | Bind Claude inference credentials to the canonical upstream. Do not follow credential-bearing redirects to arbitrary origins. Keep OAuth endpoints separately defined. |
| Decode compressed bodies, forward whole SSE events, reverse aliases, and normalize response models | CPA must inspect/translate responses. Earlier event splitting and added newlines broke native streams. **History:** [15981aa4][sse-fix]. Current native-format output still passes through an event scanner. **Code:** [stream][stream]. | Relay raw response bytes, including errors, pings, and unknown events. A separate incremental observer may read usage/terminal state without rebuilding output. Maintain coherent encoding/length headers. |
| Keep cancellation and malformed caller requests from poisoning account health | A stopped request or local validation error is not proof the account is unavailable. A later fix avoids recording a disconnect after completion as failure. **History:** [ce7fcd92][cancel-fix], [7c32971][completion-fix]. | Cancel upstream promptly; do not cool down, refresh, or rotate solely because the user cancelled. Commit completion once. |
| Handle Fast failures separately | Mode entitlement/spend failures should not silently become another mode or account. Current code makes an exception for genuinely credential-scoped quota evidence; the earlier PR's blanket description is stale. **Code:** [Fast errors][fast]. | Preserve requested mode. Ordinary Fast errors are returned unchanged; shared quota evidence follows the explicit migration rules below. Never enable extra usage or alter spending settings. |
| Parse shared quota, model limits, overage, and reset times separately | Treating every rejection or reset timestamp as whole-account exhaustion caused long and incorrectly scoped cooldowns. **Report/Code:** [#5101][quota-report], [quota parser][quota]. | Provider-specific normalized quota evidence, with scope and expiry. No universal interpretation of every 429, 403, or `7d_oi` field. |
| Retry/select another available credential and retain affinity | CPA supports both base selectors and optional session affinity. Its current selector preserves a healthy binding over priority changes and can reselect when the binding becomes unavailable. **Code:** [selector][selector]. | Reuse Comradex's sticky prefer/preserve behavior, with a Claude-specific safe retry decision. Avoid per-request round robin. |
| Maintain diagnostics/request continuity with generation checks | Out-of-order or truncated responses must not overwrite newer session state. CPA scopes generated continuity to stable credential identity and session. **Code:** [diagnostics][diagnostics], [wire PR][wire-pr]. | Preserve client-owned continuity fields. Observe them for migration validation; do not invent replacement predecessor IDs or claim an account switch is a new conversation. |

The practical distinction is between repairing credential consistency, repairing translation damage, and imitating another client. Only the first is intrinsic to native account multiplexing. Transport compatibility needs measurement; translation repair and foreign-client disguise can largely disappear.

## Selected design: proxy-side account substitution and rollover

The design below records the intended boundaries and validation matrix. The implementation status above distinguishes completed local behavior from remaining release evidence.

### 1. Provider boundary and account setup

Add a small Claude implementation boundary, not a general provider framework:

- Infer the provider from each pool's homogeneous account kinds. Managed Claude accounts use `claude_home`; inbound-only forwarding uses `claude_inbound`. Reject mixed pools and Claude pools on Codex Desktop listeners.
- Use the fixed Anthropic upstream, separate from the configurable Codex upstream. Dispatch to Claude before any Responses/WebSocket/context handling.
- Extend `account add --claude`, `login`, `list`, `prefer`, `preserve`, and removal. `connect` remains Codex-only; do not copy an everyday Claude profile's rotating grant into a second owner.
- Launch official `claude auth login` in an isolated account profile. Use its real account metadata. Never share one writable managed credential store with a concurrently running native login/refresh owner.
- Imported everyday client credentials remain inbound-owned initially: Claude Code refreshes them and Comradex forwards the current bearer. Managed alternates use isolated stores and daemon-owned refresh after login exits. Importing a credential must not silently create two refresh owners.
- A managed login operation pauses that account's admissions and coordinates with the daemon before touching its store. Preserve unrelated profiles, permissions, plugins, and session files.

Claude's macOS Keychain storage and file fallback need a dedicated adapter, including the profile-directory identity. This differs from Comradex's current `auth.json` handling. [Native credential storage][auth-docs].

Use local-only listeners with actual access control. A claimed Claude User-Agent does not authorize access to a subscription pool. Keep the current client's subscription login active when setting the gateway URL. Use Comradex's installation secret in the local URL path and strip that path prefix upstream. Do not install custom headers or replace OAuth with a placeholder API key to authenticate the local hop.

### 2. Authority and permitted changes

| Data | Authority | Proposed handling |
| --- | --- | --- |
| OAuth bearer and real account/organization IDs | Selected account | Replace only where selection requires it; remove conflicting alternative authentication headers. |
| Device identity | Real native profile/caller | Preserve the actual originating installation identity where valid; retain imported identity for a managed profile. Verify the mapping with native captures before cross-account release. Do not clone CPA's generated device IDs by default. |
| Session, subagent, parent IDs | Claude Code | Preserve. Internally namespace by provider, pool, client identity, session, and agent where relevant. Never rewrite on bearer refresh. |
| System, tools, messages, thinking, compaction, cache controls, parameters | Claude Code | No semantic edits and no whole-body JSON reserialization. |
| Native attribution and diagnostic chain | Claude Code | Preserve, except a measured checksum dependency caused by an explicitly permitted edit. |
| Routing and quota observations | Comradex | Internal metadata only; never inject them into the model conversation. |
| Response body and upstream errors | Anthropic | Raw forwarding. Local errors use a distinct error path. |

Implement credential changes as bounded raw JSON span edits. Preserve unknown members inside metadata and outside it, original escapes, whitespace, and key order. If no changes are required, the upstream body must equal the inbound body byte for byte. If changes are required, tests must enumerate every changed span. Reject malformed or duplicate identity containers instead of guessing.

CCH is a request checksum, distinct from a model-generated thinking signature. CPA includes metadata in its hash input, so account metadata edits can affect it. Never repair thinking signatures. For CCH, preserve an absent native gateway checksum unless live evidence establishes a need to add one; if present, verify the supported algorithm and finalize it after all edits. Unknown checksum formats must stop cross-account rewriting with a clear local diagnostic rather than silently emitting an invalid request.

### 3. Sticky routing and automatic rollover

Reuse Comradex's preferred/preserved account policy for new work. Keep healthy sessions on their selected account even when another account's quota improves or a preference changes. The soft usage threshold changes new placement, not established sessions.

Initially keep a root session's related native requests on the same account, including model changes, helpers, compaction, and subagents. Track agent-specific completion independently. A persistent root placement prevents accidental account fragmentation; strict model pins and unavailable entitlements must report conflicts rather than silently changing a model. More granular subagent balancing is a later product choice.

For requests without a usable session identifier, use explicit client/endpoint association or retain inbound-account forwarding. Do not choose an arbitrary new account on every helper call or derive identity from prompt content.

Automatic migration is allowed only when all of these hold:

1. The selected account has an explicit pre-generation quota rejection, or is already known unavailable before dispatch.
2. No response content has been committed to the caller, and the failed attempt is known not to have generated a response.
3. The request carries its usable conversation state, with no known account-owned resource preventing migration.
4. Relevant thinking, compaction, attribution, and diagnostic continuation shapes have passed the cross-account validation matrix.
5. The replacement supports the same requested model and mode under the user's existing billing settings.
6. Migration wins an atomic placement-generation update after active related attempts have drained or reached a defined safe boundary.

Freeze new admissions to the migrating session cohort while deciding its replacement. Existing unrelated sessions continue. Do not let concurrent subagents independently move the cohort to different accounts. If the cohort cannot reach a safe boundary within the request's existing deadline, return the original rejection; do not interrupt successful siblings to force migration.

Rebuild an alternate attempt from the original client bytes, applying only the new credential envelope. Try each eligible account at most once within the existing request budget. Keep the replacement sticky after success; do not bounce back as soon as the previous account recovers. Exhausting the pool returns a real upstream failure and useful local status, not synthetic assistant text.

Do not migrate after partial output, ambiguous network loss, a thinking/signature 400, a generic permission 403, account suspension evidence, or an ordinary Fast entitlement failure. Refresh an expired managed token once through the coordinated resolver before deciding it needs login. A cancellation is neither a refresh trigger nor a routing failure. Keep temporary overload separate from account exhaustion and respect the upstream's retry guidance.

This is quota routing among already authorized accounts, not recovery from a provider's account restriction. A generic 403 must never trigger a search for another account that will accept the same request.

### 4. Account-owned resources and auxiliary traffic

Messages being largely self-contained does not prove every continuation is portable. Track ownership of uploaded files, server-side containers, and any opaque resource references actually observed. Pin those requests to an eligible owner unless portability is established. Never re-upload or reconstruct user content silently just to force rollover.

`cc_prev_req`, `diagnostics.previous_message_id`, fallback credits, compaction blocks, and signed thinking are explicit migration test cases. Do not assume that every predecessor ID is hard ownership, or that every such ID is advisory. Test each emitted native shape. If a field is account-bound, preserve the owner; do not clear it to conceal a broken continuation.

Implement Messages, token counting, models, and observed startup endpoints as separate route contracts, preserving the path and query. Profile/usage/control requests need origin-account semantics rather than arbitrary pool selection. Unsupported auxiliary endpoints return a clear response; they must not accidentally enter the Codex handler.

Some native checks bypass `ANTHROPIC_BASE_URL`. Inventory those during capture and distinguish local-client checks from selected-account inference requirements. Fast-mode checks and WebFetch checks are documented examples. Do not add TLS interception or synthetic telemetry to make all traffic appear to belong to the selected account. Keep affected features on a verified owner if the mismatch matters. [Gateway protocol][protocol].

### 5. Streaming, quotas, transport, and observability

Use raw HTTP streaming with backpressure and cancellation. A side observer recognizes Claude terminal events and quota/usage evidence, but does not rewrite SSE. Unknown events, ping/comment lines, compressed payloads, and error bodies remain intact. Do not feed Claude streams through the existing Codex SSE/Responses decoder.

Normalize quota observations into account-wide and model/mode-specific availability with independent reset times. Start from real response headers and the bounded OAuth usage poll. Idle accounts with stale or absent observations remain unknown; an explicit inactive window from the authoritative usage API is distinct from missing data. Claude warming uses the separate opt-in native-executable path above; it never reuses Codex's constructed activation request.

Measure Claude transport on supported client/platform combinations before selecting the Rust implementation. CPA uses separate inference and OAuth transport profiles plus a custom ordered HTTP writer. Comradex currently uses Codex-specific native-tls/rustls configurations; neither is evidence of Claude parity. The transport spike must decide whether a suitable Rust connector can meet the measured requirements or whether an isolated native-runtime transport is necessary. Do not commit to a permanent sidecar without that comparison.

For the supported gateway path, the default is preserving real client headers and normal connection reuse, not asserting a fabricated software baseline. TLS/profile matching should fix measured compatibility requirements; it is not a promise to defeat a private classifier. Record any residual differences explicitly.

Log account aliases, hashed routing keys, placement generations, status classes, usage freshness, and the reason for a rollover. Never log tokens, complete request bodies, thinking, or device/account identifiers. Local fixture capture uses synthetic tasks and keeps sensitive bytes out of checked-in artifacts. Do not fabricate upstream telemetry or disable native safeguards.

## Implementation sequence and acceptance gates

| Step | Work | Completion evidence |
| --- | --- | --- |
| 1. Capture and transport spike | Compare genuine Claude Code direct traffic, its gateway traffic, and CPA's native path on the same synthetic workload. Record versions. Inventory main/helper/subagent/count/auth requests and direct-only checks. | Sanitized structural fixtures and a byte-diff report. Explain each difference, including CCH presence, identity metadata, HTTP/TLS, and compression. Decide the transport implementation and minimum rewrite set. |
| 2. Provider boundary and unchanged relay | Add Claude pool/account typing and an inbound-auth listener. Keep requests out of Codex transforms. | Local mock tests prove raw request/response forwarding, unknown-field preservation, stream pings, cancellation, and unchanged Codex configuration behavior. |
| 3. Managed credentials and routing | Add official login integration, isolated credential stores, stable identity, refresh locking, sticky prefer/preserve placement, and observed quota state. | Mock expiry, concurrent refresh, failed profile lookup, login races, missing metadata, duplicate aliases for one real account, restart persistence, and wrong-provider rejection. |
| 4. Minimal rewrite and rollover | Add span edits and verified checksum handling. Implement one atomic session migration after a qualifying quota failure. | Two-account tests prove continuation without changing system/tools/messages/thinking; concurrent agents do not split placement; partial streams and ambiguous failures are never replayed. |
| 5. Product integration | Add provider-aware install/uninstall, CLI status, and menubar account/usage display. Document actual supported Claude surfaces and client versions. | Isolated daemon/client smoke test; existing user configuration and running service remain untouched during validation. |
| 6. Release validation | Run repository tests, formatting, Clippy, and targeted native smoke scenarios; review actual wire differences. | Passing local/CI evidence for the final implementation. A documented limitation stays a limitation, not a passing test. |

The cross-account matrix must include ordinary turns, tools, parallel subagents, explicit cache TTLs, native compaction, thinking/signature replay, model changes, server resource references, resumed sessions, expiry during concurrency, and quota resets. Verify that preference changes do not move healthy sessions and the preserved account remains the last eligible choice for new work.

Use a mock upstream to force quota and failure conditions. Live rollover tests should move a test session at a controlled boundary; do not exhaust real subscriptions to manufacture a 429. A successful two-account exchange verifies those protocol shapes, not future non-enforcement. Track cache reads/writes and usage before/after a switch so the cost of losing account-local cache is visible.

Release the requested multiplexing behavior only after the relevant continuation shapes pass. If a resource or feature proves account-bound, document and enforce that specific pin. Do not silently strip state or replace the whole design with launch-time account selection.

## Repository change map

- `src/config.rs`: provider-aware pools, Claude account kind, separate upstream configuration, validation.
- `src/auth.rs` and a focused Claude auth module: shared credential interface with provider-specific identity/refresh; retain existing Codex behavior.
- `src/accounts.rs`, `src/main.rs`, `src/install.rs`: native account lifecycle and opt-in Claude gateway setup.
- `src/proxy/mod.rs` plus a Claude handler: dispatch boundary, raw forwarding, minimal edits, Claude completion observation.
- `src/transport.rs` plus a Claude transport module if needed: independently measured transport, separate from Codex connectors.
- `src/routing/{router,metadata,affinity}.rs`: provider-scoped identities, root/agent placement, quota scope, atomic migration and persistence.
- `src/usage.rs`, `src/state.rs`, `src/control.rs`, and `macos/ComradexMenu`: provider-aware snapshots and honest usage/availability display.
- Tests: sanitized native fixtures, mock-upstream protocol tests, credential concurrency, routing races, and an isolated optional live harness.

Do not extend `context_codec`, Codex context storage, or the Responses WebSocket bridge for Claude. The anticipated simplification is forwarding Claude-owned context instead of materializing or translating it. The remaining cost is credential and transport correctness, plus proof of cross-account continuation.

The implementation and live validation do not establish production account safety. Release validation still needs an inventory of helper/IDE/direct-only feature behavior and the quota-triggered continuation matrix. Unverified account-owned shapes remain pinned. The original investigation used synthetic data; the subsequent live tests used the user's explicitly authorized managed accounts and small native prompts. No measured ban rate was established.

[wire-pr]: https://github.com/router-for-me/CLIProxyAPI/pull/4752
[gateway]: https://code.claude.com/docs/en/llm-gateway
[policy]: https://code.claude.com/docs/en/legal-and-compliance
[protocol]: https://code.claude.com/docs/en/llm-gateway-protocol
[auth-docs]: https://code.claude.com/docs/en/authentication
[oauth]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/auth/claude/anthropic_auth.go
[oauth-tests]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/auth/claude/anthropic_auth_test.go
[refresh-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/6431cec7
[identity]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/helps/claude_credential_identity.go
[device-id]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/auth/claude/identity.go
[auth-prepare]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_executor_auth.go
[profile-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/8aa6868d
[sessions]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/helps/claude_code_session.go
[selector]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/sdk/cliproxy/auth/selector.go
[identity-tests]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/helps/claude_credential_identity_test.go
[identity-hardening]: https://github.com/router-for-me/CLIProxyAPI/commit/b3ed702e
[detection-report]: https://github.com/router-for-me/CLIProxyAPI/issues/5715
[detection-fix]: https://github.com/router-for-me/CLIProxyAPI/pull/5820
[cloak-origin]: https://github.com/router-for-me/CLIProxyAPI/commit/00280b6f
[tool-origin]: https://github.com/router-for-me/CLIProxyAPI/commit/e8d1b79c
[tool-request]: https://github.com/router-for-me/CLIProxyAPI/issues/4670
[cloaking]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_executor_cloaking.go
[stream]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_executor_stream.go
[beta-report]: https://github.com/router-for-me/CLIProxyAPI/issues/5738
[beta-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/d9b8fdb
[ttl-report]: https://github.com/router-for-me/CLIProxyAPI/issues/5629
[ttl-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/6a73f39
[cache-report]: https://github.com/router-for-me/CLIProxyAPI/issues/5730
[cache-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/377c315
[signing]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_signing.go
[signing-tests]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_signing_test.go
[executor]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_executor.go
[thinking-replay]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_thinking_replay.go
[tokens]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_executor_tokens.go
[headers]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_executor_request.go
[profiles]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/helps/claude_device_profile.go
[tls-origin]: https://github.com/router-for-me/CLIProxyAPI/commit/249f9691
[wire-origin]: https://github.com/router-for-me/CLIProxyAPI/commit/f3e25ab2
[transport]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/helps/utls_client.go
[resumption-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/70793491
[origin]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/helps/claude_upstream.go
[sse-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/15981aa4
[cancel-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/ce7fcd92
[completion-fix]: https://github.com/router-for-me/CLIProxyAPI/commit/7c32971
[fast]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_executor_fast_error.go
[quota-report]: https://github.com/router-for-me/CLIProxyAPI/issues/5101
[quota]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/helps/claude_ratelimit.go
[diagnostics]: https://github.com/router-for-me/CLIProxyAPI/blob/9bdde54b59d1af70ae0534a0ef61b2c3361a1257/internal/runtime/executor/claude_executor_diagnostics.go
