# Migrate reqwest from native-tls to rustls-tls

## Summary

Validates that all reqwest dependencies across the five affected crates (tj-chat-zulip, tj-git, tj-runner, tj-manager, tj-provider) use rustls-tls instead of native-tls/OpenSSL, and that the dead `openssl` dependency is removed from tj-provider. Tests verify build succeeds, dependency tree is clean, unit tests pass, and runtime HTTPS connections work with the new TLS backend.

**Scope:** Build system, dependency tree, unit tests, and runtime HTTPS connectivity (API health + Zulip DM)
**Prerequisites:** None. This is a dependency swap with no data requirements.
**Mode:** zulip-only (build verification + unit tests + server runtime with Zulip interaction)

---

## Test 1: Dependency Tree Contains rustls, Not OpenSSL

Verify that the migration from native-tls to rustls-tls is reflected in the resolved dependency tree. This is the primary static analysis gate -- if OpenSSL is still present, the migration is incomplete.

**Run:**
```bash
cargo tree -i openssl-sys 2>&1
```

**Expect:**
- Command output is empty or states no matching packages found.
- No crate in the tree depends on `openssl-sys`, `openssl`, or `native-tls`.

**Run:**
```bash
cargo tree -i rustls 2>&1
```

**Expect:**
- Output shows `rustls` as a dependency of at least one crate.
- The chain goes through `reqwest` (i.e., `reqwest -> rustls` or `reqwest -> rustls-tls -> rustls`).

**Run:**
```bash
cargo tree -i native-tls 2>&1
```

**Expect:**
- Command output is empty or states no matching packages found.
- No crate depends on `native-tls`.

---

## Test 2: Clean Build with Zero Warnings

Build the entire workspace from scratch. The migration must not introduce compilation errors or warnings. This also proves that the build succeeds without OpenSSL system libraries.

**Run:**
```bash
cargo clean
```

**Expect:**
- Build completes successfully (exit code 0).
- No linker errors referencing `-lssl`, `-lcrypto`, or `openssl`.
- No compiler warnings introduced by the dependency change.
- No errors about missing `openssl-sys` or `native-tls` features.

---

## Test 3: All Unit Tests Pass

Run the full test suite. Since reqwest's API is identical regardless of TLS backend, all existing tests must pass unchanged.

**Run:**
```bash
cargo test 2>&1
```

**Expect:**
- All tests pass (exit code 0).
- No test failures related to TLS, HTTPS connections, certificate handling, or reqwest client creation.
- Test output shows the expected number of test results with zero failures.

---

## Test 4: Five Crates Declare reqwest with rustls-tls

Verify each of the five affected crates declares reqwest with `default-features = false` and includes `"rustls-tls"` in its features.

**Run:**
```bash
grep -A2 'reqwest' crates/tj-chat-zulip/Cargo.toml
```

**Expect:**
- Output contains `default-features = false`.
- Output contains `"rustls-tls"` in the features list.
- Output contains `"json"` in the features list.

**Run:**
```bash
grep -A2 'reqwest' crates/tj-git/Cargo.toml
```

**Expect:**
- Output contains `default-features = false`.
- Output contains `"rustls-tls"` and `"json"` in the features list.

**Run:**
```bash
grep -A2 'reqwest' crates/tj-runner/Cargo.toml
```

**Expect:**
- Output contains `default-features = false`.
- Output contains `"rustls-tls"` and `"json"` in the features list.

**Run:**
```bash
grep -A2 'reqwest' crates/tj-manager/Cargo.toml
```

**Expect:**
- Output contains `default-features = false`.
- Output contains `"rustls-tls"` and `"json"` in the features list.

**Run:**
```bash
grep -A2 'reqwest' crates/tj-provider/Cargo.toml
```

**Expect:**
- Output contains `default-features = false`.
- Output contains `"rustls-tls"`, `"json"`, and `"stream"` in the features list.

---

## Test 5: Dead openssl Dependency Removed from tj-provider

Verify that the unused `openssl` dependency has been removed from tj-provider's Cargo.toml.

**Run:**
```bash
grep 'openssl' crates/tj-provider/Cargo.toml
```

**Expect:**
- No output (grep returns exit code 1, meaning no match found).
- The string `openssl` does not appear anywhere in the file.

---

## Test 6: Zero .rs Source Files Modified

Verify that no Rust source files were changed as part of this migration. The reqwest API is backend-agnostic, so no source modifications are needed.

**Run:**
```bash
git diff --name-only HEAD~1 -- '*.rs'
```

**Expect:**
- No `.rs` files listed in the output.
- Only `Cargo.toml` and `Cargo.lock` files appear in the diff.

---

## Test 7: Cargo.lock Reflects rustls, Not OpenSSL

Verify the lockfile contains rustls dependencies and no OpenSSL dependencies.

**Run:**
```bash
grep -c 'name = "rustls"' Cargo.lock
```

**Expect:**
- Count is greater than 0 (rustls is present in the lockfile).

**Run:**
```bash
grep -c 'name = "openssl-sys"' Cargo.lock
```

**Expect:**
- Count is 0 (openssl-sys is absent from the lockfile).

**Run:**
```bash
grep -c 'name = "native-tls"' Cargo.lock
```

**Expect:**
- Count is 0 (native-tls is absent from the lockfile).

**Run:**
```bash
grep -c 'name = "ring"' Cargo.lock
```

**Expect:**
- Count is greater than 0 (ring is present -- rustls uses ring as its default crypto provider).

---

## Test 8: Zulip HTTPS Connection Works with rustls

Verify that the reqwest client in tj-chat-zulip successfully makes an HTTPS connection to the Zulip API using rustls. This is the critical runtime test -- the TLS handshake with a real external service must succeed.

**Send a DM to Concierge:**
```bash
./scripts/zulip_send_dm.sh "Hello, testing TLS connectivity."
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```

**Expect:**
- The DM send script succeeds (the TurboJET bot's outbound HTTPS request to Zulip API completes -- this is also a reqwest HTTPS call via rustls).
- Concierge responds with a human-readable welcome message within 30 seconds.
- Response does NOT contain raw JSON, TLS errors, certificate validation errors, or connection refused messages.
- The successful response proves that both inbound (Zulip webhook) and outbound (reqwest to Zulip API) HTTPS work with rustls.

---

## Cleanup

No server cleanup needed — the orchestrator manages the server lifecycle.
