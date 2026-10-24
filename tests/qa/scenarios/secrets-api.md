# Secrets REST API + SSH Key Generation

## Summary

Validates the full secrets management lifecycle: REST API CRUD for project secrets (set, get masked, list keys, delete), secret masking correctness, SSH key pair generation with multiple algorithms, SSH key retrieval and deletion, reserved key prefix enforcement, LLM access blocking verification, and master key configuration. Tests the complete user journey from secret creation through git provider integration.

**Scope:** REST API only (secrets endpoints, SSH key endpoints, health check)
**Prerequisites:** Project "test-project" must exist. `TURBOJET_MASTER_KEY` must be set in `config/.env`.
**Mode:** api-only

---

## Test 1: Server Starts with Secrets Store Initialized

Verify the server starts correctly with the master encryption key configured.

**Steps:**

1. Check that `config/.env` contains a `TURBOJET_MASTER_KEY` entry with a 64-character hex string:
   ```bash
   grep -E '^TURBOJET_MASTER_KEY=[0-9a-fA-F]{64}$' config/.env
   ```
2. Start the server via setup.md.
3. Hit the health endpoint:
   ```bash
   curl -sf http://localhost:8100/api/health
   ```

**Expected:**
- Step 1: The grep matches exactly one line. The key is 64 hex characters.
- Step 2: Server starts without error. Setup reports "SERVER UP".
- Step 3: Health check returns a valid JSON response with status indicating healthy.

---

## Test 2: Set Secret — Happy Path (Create New)

Store a new secret for the test project.

**Steps:**

1. Get the project ID for "test-project":
   ```bash
   PROJECT_ID=$(curl -sf http://localhost:8100/v1/projects | jq -r '.[] | select(.name == "test-project") | .id')
   echo "Project ID: $PROJECT_ID"
   ```
2. Create a new secret:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "forgejo_api_token", "value": "d_actual_secret_value_12345"}'
   ```

**Expected:**
- Step 1: A valid project ID is returned (non-empty string).
- Step 2: HTTP status is 201. Response body contains JSON with:
  - `"key": "forgejo_api_token"`
  - `"project_id"` matching `$PROJECT_ID`
  - `"created": true`

---

## Test 3: Set Secret — Update Existing (Upsert)

Update the same key with a new value.

**Steps:**

1. Set the same key again with a different value:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "forgejo_api_token", "value": "d_updated_token_value_67890"}'
   ```

**Expected:**
- Step 1: HTTP status is 201. Response body contains:
  - `"key": "forgejo_api_token"`
  - `"project_id"` matching `$PROJECT_ID`
  - `"created": false` (indicating update, not creation)

---

## Test 4: Get Secret — Masked Value

Retrieve the secret and verify masking.

**Steps:**

1. Get the masked secret:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/forgejo_api_token"
   ```

**Expected:**
- Step 1: HTTP status is 200. Response body contains JSON with:
  - `"key": "forgejo_api_token"`
  - `"masked": true`
  - `"value": "d_u***"` (first 3 chars "d_u" followed by "***")
  - The full plaintext "d_updated_token_value_67890" does NOT appear anywhere in the response

---

## Test 5: Get Secret — Short Value Masking

Verify that secrets shorter than 6 characters are fully masked.

**Steps:**

1. Set a short secret (5 characters):
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "short_key", "value": "abcde"}'
   ```
2. Get the masked value:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/short_key"
   ```

**Expected:**
- Step 1: HTTP status 201, `"created": true`.
- Step 2: HTTP status 200. Response contains:
  - `"masked": true`
  - `"value": "***"` (entirely masked because "abcde" is 5 chars, under the 6-char threshold)
  - The plaintext "abcde" does NOT appear anywhere in the response

---

## Test 6: List Secrets — Keys Only

List all secrets for the project and verify only key names are returned.

**Steps:**

1. List secrets:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/secrets"
   ```

**Expected:**
- Step 1: HTTP status is 200. Response body contains JSON with:
  - `"project_id"` matching `$PROJECT_ID`
  - `"secrets"` array containing `"forgejo_api_token"` and `"short_key"`
  - Array is sorted alphabetically: `["forgejo_api_token", "short_key"]`
  - No secret values appear anywhere in the response
  - No reserved keys (no `__ssh_*` entries)

---

## Test 7: Delete Secret — Existing Key

Delete a secret that exists.

**Steps:**

1. Delete the short_key secret:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/short_key"
   ```

**Expected:**
- Step 1: HTTP status is 200. Response body contains JSON with:
  - `"key": "short_key"`
  - `"deleted": true`

---

## Test 8: Delete Secret — Non-Existent Key (Idempotent)

Delete a key that does not exist.

**Steps:**

1. Delete the same key again:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/short_key"
   ```

**Expected:**
- Step 1: HTTP status is 200. Response body contains JSON with:
  - `"key": "short_key"`
  - `"deleted": false`

---

## Test 9: Get Secret — Non-Existent Key

Attempt to get a secret that does not exist.

**Steps:**

1. Get a key that was never created:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/nonexistent_key"
   ```

**Expected:**
- Step 1: HTTP status is 404. Response body contains a human-readable error message (not raw JSON stack trace). The message indicates the key was not found.

---

## Test 10: Set Secret — Reserved Key Prefix Rejected

Attempt to create a secret with the reserved `__` prefix.

**Steps:**

1. Try to set a secret with reserved prefix:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "__system_key", "value": "should_fail"}'
   ```

**Expected:**
- Step 1: HTTP status is 403. Response body contains a human-readable error message explaining that keys starting with `__` are reserved for system use.

---

## Test 11: Set Secret — Invalid Key Format

Attempt to create secrets with invalid key names.

**Steps:**

1. Try a key with special characters:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "bad key!", "value": "test"}'
   ```
2. Try an empty key:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "", "value": "test"}'
   ```
3. Try a key that is too long (129 characters):
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d "{\"key\": \"$(printf 'a%.0s' {1..129})\", \"value\": \"test\"}"
   ```

**Expected:**
- Step 1: HTTP status is 400. Error message is human-readable.
- Step 2: HTTP status is 400. Error message is human-readable.
- Step 3: HTTP status is 400. Error message is human-readable.

---

## Test 12: Set Secret — Invalid Value

Attempt to create secrets with invalid values.

**Steps:**

1. Try an empty value:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "empty_val", "value": ""}'
   ```
2. Try a value exceeding 8192 characters:
   ```bash
   LONG_VALUE=$(python3 -c "print('x' * 8193)")
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d "{\"key\": \"too_long_val\", \"value\": \"$LONG_VALUE\"}"
   ```

**Expected:**
- Step 1: HTTP status is 400. Error message is human-readable.
- Step 2: HTTP status is 400. Error message is human-readable.

---

## Test 13: Secrets on Non-Existent Project

All secrets endpoints return 404 for a project that does not exist.

**Steps:**

1. Set secret on non-existent project:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/nonexistent_project/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "test_key", "value": "test_value"}'
   ```
2. List secrets on non-existent project:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/nonexistent_project/secrets"
   ```
3. Get secret on non-existent project:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/nonexistent_project/secrets/test_key"
   ```
4. Delete secret on non-existent project:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/nonexistent_project/secrets/test_key"
   ```

**Expected:**
- Step 1: HTTP status is 404.
- Step 2: HTTP status is 404.
- Step 3: HTTP status is 404.
- Step 4: HTTP status is 404.
- All responses contain human-readable error messages, not raw JSON stack traces.

---

## Test 14: SSH Key Generation — Ed25519

Generate an SSH key pair with the default/recommended algorithm.

**Steps:**

1. Generate an Ed25519 key pair:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys" \
     -H "Content-Type: application/json" \
     -d '{"algorithm": "ed25519"}'
   ```
2. Extract the public key from the response for verification:
   ```bash
   PUB_KEY=$(echo "$RESPONSE" | jq -r '.public_key')
   echo "Public key: $PUB_KEY"
   ```

**Expected:**
- Step 1: HTTP status is 201. Response body contains JSON with:
  - `"algorithm": "ed25519"`
  - `"project_id"` matching `$PROJECT_ID`
  - `"public_key"` starting with `"ssh-ed25519 "` and containing the comment `turbojet@` followed by the project ID
- Step 2: The public key is a valid OpenSSH format string.

---

## Test 15: SSH Key Retrieval — Get Public Key

Retrieve the previously generated public key.

**Steps:**

1. Get the SSH key:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys"
   ```

**Expected:**
- Step 1: HTTP status is 200. Response body contains JSON with:
  - `"algorithm": "ed25519"`
  - `"public_key"` matching the key returned in Test 14
  - `"project_id"` matching `$PROJECT_ID`
  - No private key field present in the response

---

## Test 16: SSH Key Regeneration — Overwrite Existing

Generate a new SSH key pair, overwriting the existing one.

**Steps:**

1. Generate a new Ed25519 key pair (regeneration):
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys" \
     -H "Content-Type: application/json" \
     -d '{"algorithm": "ed25519"}'
   ```
2. Retrieve the key again:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys"
   ```

**Expected:**
- Step 1: HTTP status is 201. A new public key is returned (different from Test 14).
- Step 2: HTTP status is 200. The public key matches the one from Step 1 (not the old one from Test 14).

---

## Test 17: SSH Key Generation — RSA 4096

Generate an SSH key pair with RSA 4096-bit.

**Steps:**

1. Delete the existing SSH key first:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys"
   ```
2. Generate an RSA key pair:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys" \
     -H "Content-Type: application/json" \
     -d '{"algorithm": "rsa_4096"}'
   ```

**Expected:**
- Step 1: HTTP status is 200, `"deleted": true`.
- Step 2: HTTP status is 201. Response body contains:
  - `"algorithm": "rsa_4096"`
  - `"public_key"` starting with `"ssh-rsa "`
  - `"project_id"` matching `$PROJECT_ID`

---

## Test 18: SSH Key Generation — ECDSA P-256

Generate an SSH key pair with ECDSA P-256.

**Steps:**

1. Delete the existing SSH key:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys"
   ```
2. Generate an ECDSA P-256 key pair:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys" \
     -H "Content-Type: application/json" \
     -d '{"algorithm": "ecdsa_256"}'
   ```

**Expected:**
- Step 1: HTTP status is 200.
- Step 2: HTTP status is 201. Response body contains:
  - `"algorithm": "ecdsa_256"`
  - `"public_key"` starting with `"ecdsa-sha2-nistp256 "`

---

## Test 19: SSH Key Generation — Unsupported Algorithm

Attempt to generate an SSH key with an invalid algorithm.

**Steps:**

1. Generate with unsupported algorithm:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys" \
     -H "Content-Type: application/json" \
     -d '{"algorithm": "invalid_algo"}'
   ```

**Expected:**
- Step 1: HTTP status is 400. Response body contains a human-readable error message listing the supported algorithms.

---

## Test 20: SSH Key Generation — Non-Existent Project

Attempt to generate SSH keys for a project that does not exist.

**Steps:**

1. Generate SSH key for non-existent project:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/nonexistent_project/ssh-keys" \
     -H "Content-Type: application/json" \
     -d '{"algorithm": "ed25519"}'
   ```
2. Get SSH key for non-existent project:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/nonexistent_project/ssh-keys"
   ```
3. Delete SSH key for non-existent project:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/nonexistent_project/ssh-keys"
   ```

**Expected:**
- Step 1: HTTP status is 404.
- Step 2: HTTP status is 404.
- Step 3: HTTP status is 404.

---

## Test 21: SSH Key Deletion

Delete the SSH keys and verify they are gone.

**Steps:**

1. Delete the SSH keys:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys"
   ```
2. Try to retrieve the deleted key:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys"
   ```
3. Delete again (idempotent):
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys"
   ```

**Expected:**
- Step 1: HTTP status is 200. Response contains `"deleted": true`.
- Step 2: HTTP status is 404. Error message indicates no SSH key is configured for the project.
- Step 3: HTTP status is 200. Response contains `"deleted": false` (idempotent).

---

## Test 22: Reserved Keys Excluded from List

Verify that SSH key generation stores reserved keys but they are excluded from the user-facing list.

**Steps:**

1. Generate SSH keys:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys" \
     -H "Content-Type: application/json" \
     -d '{"algorithm": "ed25519"}'
   ```
2. List secrets:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/secrets"
   ```
3. Attempt to directly get the reserved private key:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/__ssh_private_key"
   ```

**Expected:**
- Step 1: HTTP status is 201. SSH keys generated.
- Step 2: HTTP status is 200. The `"secrets"` array does NOT contain `"__ssh_private_key"` or `"__ssh_public_key"`. It should contain `"forgejo_api_token"` (from Test 2) but not the reserved keys.
- Step 3: HTTP status is 404 (reserved keys are not accessible through the user-facing GET endpoint).

---

## Test 23: Delete Secret — Reserved Key Rejected

Attempt to delete a reserved key via the user-facing delete endpoint.

**Steps:**

1. Try to delete the SSH private key:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/__ssh_private_key"
   ```

**Expected:**
- Step 1: HTTP status is 403. Response contains a human-readable error explaining that keys starting with `__` are reserved.

---

## Test 24: List Secrets — Empty Project

A project with no user secrets (after cleanup) returns an empty array.

**Steps:**

1. Create a second project for isolation testing:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects" \
     -H "Content-Type: application/json" \
     -d '{"name": "empty-secrets-test", "repo_url": "https://example.com/repo.git", "base_branch": "main"}'
   ```
2. Capture the project ID:
   ```bash
   EMPTY_PROJECT_ID=$(curl -sf http://localhost:8100/v1/projects | jq -r '.[] | select(.name == "empty-secrets-test") | .id')
   ```
3. List secrets for the empty project:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$EMPTY_PROJECT_ID/secrets"
   ```

**Expected:**
- Step 1: Project created successfully.
- Step 2: Valid project ID returned.
- Step 3: HTTP status is 200. Response contains `"secrets": []` (empty array).

---

## Test 25: Secret Masking Boundary — Exactly 6 Characters

Verify masking behavior at the boundary (6 chars: show first 3 + ***).

**Steps:**

1. Set a secret with exactly 6 characters:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "boundary_six", "value": "abcdef"}'
   ```
2. Get the masked value:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/boundary_six"
   ```

**Expected:**
- Step 1: HTTP status 201.
- Step 2: HTTP status 200. Response contains:
  - `"masked": true`
  - `"value": "abc***"` (first 3 chars visible, rest masked)
  - The plaintext "abcdef" does NOT appear in the response

---

## Test 26: Secret Masking Boundary — Exactly 5 Characters

Verify full masking for values just under the threshold.

**Steps:**

1. Set a secret with exactly 5 characters:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
     -H "Content-Type: application/json" \
     -d '{"key": "boundary_five", "value": "abcde"}'
   ```
2. Get the masked value:
   ```bash
   curl -s -w "\nHTTP_STATUS: %{http_code}\n" \
     "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/boundary_five"
   ```

**Expected:**
- Step 1: HTTP status 201.
- Step 2: HTTP status 200. Response contains:
  - `"masked": true`
  - `"value": "***"` (fully masked, under 6-char threshold)
  - The plaintext "abcde" does NOT appear in the response

---

## Cleanup

Remove all test artifacts:

1. Delete all secrets for test-project:
   ```bash
   PROJECT_ID=$(curl -sf http://localhost:8100/v1/projects | jq -r '.[] | select(.name == "test-project") | .id')
   curl -sf "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" | jq -r '.secrets[]' | while read key; do
     curl -sf -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/secrets/$key" > /dev/null
   done
   ```
2. Delete SSH keys for test-project:
   ```bash
   curl -sf -X DELETE "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-keys" > /dev/null
   ```
3. Delete the empty-secrets-test project:
   ```bash
   EMPTY_PROJECT_ID=$(curl -sf http://localhost:8100/v1/projects | jq -r '.[] | select(.name == "empty-secrets-test") | .id')
   if [ -n "$EMPTY_PROJECT_ID" ]; then
     curl -sf -X DELETE "http://localhost:8100/v1/projects/$EMPTY_PROJECT_ID" > /dev/null
   fi
   ```
4. Execute teardown.md.
