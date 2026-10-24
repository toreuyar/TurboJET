# Worktree Workflow Redesign — Branch + PR Lifecycle

## Summary

Validates the redesigned worktree lifecycle: Manager creates a single worktree per task with branch `tj/{run_id}-{slug}`, all flows share it, per-flow commits are pushed, a draft PR is created via Forgejo API and marked ready after the final flow, worktree is cleaned up on both success and failure, and Zulip run topic receives progress messages. No merge ever occurs.

**Scope:** Submit task via Zulip DM, observe worktree/branch creation, interactive discovery flow, per-flow commits, draft PR on Forgejo, cleanup verification.
**Prerequisites:** Project `test-project` must exist in the database with Forgejo repo URL, GitProviderConfig, PAT secret (`forgejo_api_token`), SSH key registered as deploy key on Forgejo, and a Lane. See "Infrastructure Setup" below.
**Mode:** zulip-only

## Forgejo Connection Details

- **URL:** `https://<forge-host>`
- **Owner:** `<owner>`
- **Repo:** `<repository>`
- **SSH URL:** `ssh://git@<forge-host>:<ssh-port>/<owner>/<repository>.git`
- **SSH Port:** `<ssh-port>`
- **Token:** `FORGEJO_TOKEN` from the ignored local environment or `config/.env`; never commit it.

Export matching values before running the commands below. The angle-bracket values are placeholders, not real defaults.

```bash
: "${FORGEJO_HOST:?set FORGEJO_HOST}"
: "${FORGEJO_URL:?set FORGEJO_URL}"
: "${FORGEJO_OWNER:?set FORGEJO_OWNER}"
: "${FORGEJO_REPO:?set FORGEJO_REPO}"
: "${FORGEJO_SSH_PORT:?set FORGEJO_SSH_PORT}"
: "${FORGEJO_TOKEN:?set FORGEJO_TOKEN}"
```

## Infrastructure Setup (ONE-TIME, not per-run)

This must be done once before the first QA run. Subsequent runs skip this.

```bash
# 1. Ensure project exists in DB
PROJECT_ID=$(sqlite3 $TURBOJET_HOME/state.db "SELECT id FROM projects WHERE name='test-project';")
if [ -z "$PROJECT_ID" ]; then
  PROJECT_ID="proj_test_$(openssl rand -hex 4)"
  REPO_URL="ssh://git@${FORGEJO_HOST}:${FORGEJO_SSH_PORT}/${FORGEJO_OWNER}/${FORGEJO_REPO}.git"
  GIT_PROVIDER="{\"provider_type\": \"forgejo\", \"base_url\": \"${FORGEJO_URL}\", \"owner\": \"${FORGEJO_OWNER}\", \"repo\": \"${FORGEJO_REPO}\", \"api_token_key\": \"forgejo_api_token\"}"
  sqlite3 $TURBOJET_HOME/state.db "INSERT INTO projects (id, name, repo_url, base_branch, workspace_path, git_provider, created_at, updated_at) VALUES ('$PROJECT_ID', 'test-project', '$REPO_URL', 'main', '.turbojet/$PROJECT_ID', '$GIT_PROVIDER', datetime('now'), datetime('now'));"
  echo "Created project: $PROJECT_ID"
fi

# 2. Ensure PAT secret exists
PAT_EXISTS=$(sqlite3 $TURBOJET_HOME/state.db "SELECT COUNT(*) FROM project_secrets WHERE project_id='$PROJECT_ID' AND key='forgejo_api_token';")
if [ "$PAT_EXISTS" -eq 0 ]; then
  FORGEJO_PAT="${FORGEJO_TOKEN:?set FORGEJO_TOKEN}"
  curl -sf -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/secrets" \
    -H "Content-Type: application/json" \
    -d "{\"key\": \"forgejo_api_token\", \"value\": \"$FORGEJO_PAT\"}"
  echo "PAT secret stored"
fi

# 3. Ensure SSH key exists
SSH_EXISTS=$(sqlite3 $TURBOJET_HOME/state.db "SELECT COUNT(*) FROM project_secrets WHERE project_id='$PROJECT_ID' AND key='__ssh_private_key';")
if [ "$SSH_EXISTS" -eq 0 ]; then
  SSH_RESPONSE=$(curl -sf -X POST "http://localhost:8100/v1/projects/$PROJECT_ID/ssh-key" \
    -H "Content-Type: application/json" -d '{"algorithm": "ed25519"}')
  PUBLIC_KEY=$(echo "$SSH_RESPONSE" | jq -r '.public_key')
  echo "SSH key generated"
  # Register deploy key on Forgejo
  FORGEJO_PAT="${FORGEJO_TOKEN:?set FORGEJO_TOKEN}"
  curl -sf -X POST -H "Authorization: token $FORGEJO_PAT" \
    -H "Content-Type: application/json" \
    "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/keys" \
    -d "{\"title\": \"turbojet-test\", \"key\": \"$PUBLIC_KEY\", \"read_only\": false}"
  echo "Deploy key registered"
fi

# 4. Ensure Lane exists
LANE_EXISTS=$(sqlite3 $TURBOJET_HOME/state.db "SELECT COUNT(*) FROM lanes WHERE project_id='$PROJECT_ID';")
if [ "$LANE_EXISTS" -eq 0 ]; then
  # smg_path is legacy display metadata; current lanes are pinned by workflow_id.
  sqlite3 $TURBOJET_HOME/state.db "INSERT OR IGNORE INTO lanes (id, project_id, name, smg_path, description, created_at) VALUES ('lane-default', '$PROJECT_ID', 'Default', 'workflows/discovery/workflow.smg.json', 'Default pipeline lane', datetime('now'));"
  echo "Lane seeded"
fi

# 5. Ensure SSH host key in known_hosts
mkdir -p ~/.ssh
grep -q "${FORGEJO_HOST}.*${FORGEJO_SSH_PORT}" ~/.ssh/known_hosts 2>/dev/null || ssh-keyscan -p "${FORGEJO_SSH_PORT}" "${FORGEJO_HOST}" >> ~/.ssh/known_hosts 2>/dev/null
```

---

**IMPORTANT: Stream Topic Rules (NON-NEGOTIABLE)**
- The stream topic is for STAGE AGENT communication ONLY. You are an observer who replies when asked.
- **NEVER** send unsolicited messages to the stream topic. NEVER request features, ask questions, or give instructions to TurboJET via the stream.
- You may ONLY send to the stream topic (`zulip_send.sh`) when DIRECTLY replying to a Stage agent's question. One reply per question. Do NOT send multiple messages while waiting for a response.
- All user commands (create project, start job, status checks, etc.) go to DM (`zulip_send_dm.sh`) ONLY.
- After sending a reply to a stage agent, WAIT for the agent's response before sending anything else. Do NOT send follow-ups while the agent is processing.
- Before sending to a stream topic, verify the topic belongs to the CURRENT run by checking the topic number.

---

**IMPORTANT: Answering Stage Agents (NON-NEGOTIABLE)**
- When a Stage agent asks you questions, you MUST answer EVERY question thoroughly and honestly.
- Provide detailed, complete answers. Do NOT give lazy one-liners like "Looks good" or "Proceed."
- If the agent asks about platform, features, tech stack, users, constraints — answer each one specifically.
- You are a cooperative user who wants the project to succeed. Never argue with or hold back information from the agent.
- Your answers should be detailed enough that the agent can proceed without re-asking the same question.

---

## Test 1: Health Check and Baseline

Verify the server is running and the persistent project is properly configured.

1. Run setup from `tests/qa/setup.md` (build, start server, clean Zulip, record baseline).
2. Confirm `SETUP COMPLETE` report shows server up.
3. Verify project infrastructure:
   ```bash
   PROJECT_ID=$(sqlite3 $TURBOJET_HOME/state.db "SELECT id FROM projects WHERE name='test-project';")
   if [ -z "$PROJECT_ID" ]; then echo "FATAL: test-project not found"; fi
   REPO_URL=$(sqlite3 $TURBOJET_HOME/state.db "SELECT repo_url FROM projects WHERE id='$PROJECT_ID';")
   GIT_PROVIDER=$(sqlite3 $TURBOJET_HOME/state.db "SELECT git_provider FROM projects WHERE id='$PROJECT_ID';")
   PAT_SECRET=$(sqlite3 $TURBOJET_HOME/state.db "SELECT COUNT(*) FROM project_secrets WHERE project_id='$PROJECT_ID' AND key='forgejo_api_token';")
   SSH_KEY=$(sqlite3 $TURBOJET_HOME/state.db "SELECT COUNT(*) FROM project_secrets WHERE project_id='$PROJECT_ID' AND key='__ssh_private_key';")
   LANE=$(sqlite3 $TURBOJET_HOME/state.db "SELECT COUNT(*) FROM lanes WHERE project_id='$PROJECT_ID';")
   echo "Project: $PROJECT_ID"
   echo "Repo: $REPO_URL"
   echo "GitProvider: ${GIT_PROVIDER:0:50}..."
   echo "PAT secret: $PAT_SECRET (expect 1)"
   echo "SSH key: $SSH_KEY (expect 1)"
   echo "Lane: $LANE (expect 1)"
   ```
4. Clean up any stale `tj/*` branches from previous runs:
   ```bash
   FORGEJO_PAT="${FORGEJO_TOKEN:?set FORGEJO_TOKEN}"
   BRANCHES=$(curl -sf -H "Authorization: token $FORGEJO_PAT" \
     "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/branches" | jq -r '.[].name' | grep '^tj/')
   for BRANCH in $BRANCHES; do
     curl -sf -X DELETE -H "Authorization: token $FORGEJO_PAT" \
       "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/branches/$(python3 -c 'import urllib.parse; print(urllib.parse.quote("'"$BRANCH"'", safe=""))')"
     echo "Deleted stale branch: $BRANCH"
   done
   ```
5. Record baseline:
   - Pipeline run count: `sqlite3 $TURBOJET_HOME/state.db "SELECT COUNT(*) FROM pipeline_runs;"`
   - Latest Zulip stream message ID: `./scripts/zulip_get_stream.sh --stream test-project`

**Expected:** Server healthy. Project configured with repo URL, GitProviderConfig, PAT secret, SSH key, and Lane. Stale branches cleaned. Baselines recorded.

---

## Test 2: Submit Task via Zulip — Verify Branch Naming and Worktree Creation

Submit a task with a descriptive title. Verify TurboJET creates the correct branch name and reports it via Zulip.

1. Send a DM to Concierge:
   ```bash
   MSG_ID=$(./scripts/zulip_send_dm.sh "Start a job on the test-project project using the Default lane. The task is: fix the authentication timeout issue in the login module")
   echo "Sent DM, message ID: $MSG_ID"
   ```
2. Monitor DM responses for up to 60 seconds:
   ```bash
   timeout 60 ./scripts/zulip_monitor_dm.sh --after "$MSG_ID" --timeout 60
   ```
3. Look for a response confirming the task was received. Capture the run ID.
4. Monitor the `test-project` stream for "Task started" with "Branch: tj/":
   ```bash
   timeout 60 ./scripts/zulip_monitor.sh --stream test-project --after "$baseline_msg_id" --timeout 60
   ```

**Expected:**
- Concierge responds with human-readable confirmation containing a run ID.
- A Zulip stream message on `test-project` with topic = run ID.
- "Task started" message contains branch name matching `tj/{run_id}-fix-the-authentication-timeout-issue-in`.
- Branch name slug: lowercase, non-alphanumeric → `-`, no consecutive `-`, truncated to 40 chars.

**Record the exact topic name** (the run ID) for subsequent tests.

---

## Test 3: Intake Stage Observation and Interaction

Observe the Discovery flow starting and Intake stage agent engaging. Then reply with detailed requirements.

**Monitor the test-project stream (wait up to 120 seconds):**
```bash
timeout 120 ./scripts/zulip_monitor.sh --stream test-project --after "$baseline_msg_id" --timeout 120
```

**Precondition: Only send AFTER confirming the Intake agent posted a question.**

Reply to the Intake agent with detailed requirements:
```bash
./scripts/zulip_send.sh "Platform: Web application running in modern browsers (Chrome, Firefox, Safari, Edge). Tech stack: React frontend with TypeScript, Express.js backend, PostgreSQL database. The login module uses JWT tokens with a configurable expiration time. Features needed: The authentication timeout is currently hardcoded to 15 minutes. We need it configurable per-user-session, with a max session time of 24 hours. Idle timeout should be 30 minutes by default but admin-configurable. When a session times out, the user should be redirected to the login page with a clear message explaining why. Target users: Internal employees accessing a company dashboard. All users are authenticated — no anonymous access. The system runs behind a corporate VPN so network security is handled externally." "{run_topic}" "test-project"
```

Monitor for response (wait up to 300 seconds):
```bash
timeout 300 ./scripts/zulip_monitor.sh --stream test-project --topic "{run_topic}" --after "$baseline_msg_id" --timeout 300
```

If the Intake agent asks a follow-up question, answer it:
```bash
./scripts/zulip_send.sh "Additional details: The current timeout logic is in src/auth/session.ts — it uses a hardcoded constant SESSION_TIMEOUT_MS = 15 * 60 * 1000. The fix should replace this with a config lookup. We want three timeout tiers: standard users get 30 minutes idle timeout, power users get 2 hours, admins get 8 hours. The max absolute session age should be 24 hours for all tiers — after that, re-login is mandatory regardless of activity. The timeout should be checked on every API request via middleware, not client-side only. That covers what I know — please proceed." "{run_topic}" "test-project"
```

Then monitor again for up to 300 seconds:
```bash
timeout 300 ./scripts/zulip_monitor.sh --stream test-project --topic "{run_topic}" --after "$baseline_msg_id" --timeout 300
```

**Expect:**
- Intake agent responds with acknowledgment or requirements summary.
- Eventually "Intake stage completed" notification appears.

---

## Test 4: Evaluate Stage Interaction

After Intake completes, Evaluate starts. Answer every question with specific, concrete details.

**Monitor (wait up to 120 seconds):**
```bash
timeout 120 ./scripts/zulip_monitor.sh --stream test-project --topic "{run_topic}" --after "$baseline_msg_id" --timeout 120
```

Answer Evaluate's questions thoroughly. Continue monitoring and answering until Evaluate completes (up to 3 rounds of questions). After each answer, monitor for up to 300 seconds.

**Expect:**
- Evaluate asks probing questions about gaps, risks, and ambiguities.
- Each round of answers leads to deeper questions or a verdict.
- Eventually Evaluate completes with a verdict (proceed or reject).

---

## Test 5: Verify Branch Exists on Forgejo

After at least one flow completes and pushes, verify the branch exists on Forgejo.

```bash
FORGEJO_PAT="${FORGEJO_TOKEN:?set FORGEJO_TOKEN}"
curl -sf -H "Authorization: token $FORGEJO_PAT" \
  "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/branches" | jq -r '.[].name' | grep "tj/$RUN_ID"
```

**Expected:** The branch `tj/{run_id}-{slug}` exists on Forgejo. At least one commit has been pushed.

---

## Test 6: Verify Draft PR on Forgejo

Check if a draft PR was created on Forgejo.

```bash
FORGEJO_PAT="${FORGEJO_TOKEN:?set FORGEJO_TOKEN}"
curl -sf -H "Authorization: token $FORGEJO_PAT" \
  "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/pulls?state=open" | jq '.[0] | {number, title, draft, head: .head.ref, base: .base.ref}'
```

**Expected:**
- An open PR exists on `<owner>/<repository>`.
- `draft: true`.
- `head.ref` matches `tj/{run_id}-{slug}`.
- `base.ref` is `main`.
- Title starts with "TurboJet".

---

## Test 7: No Merge Occurs

Verify no merge activity happened.

```bash
# Check that no merge-related messages appeared
grep -i "merge" /tmp/qa_messages.log 2>/dev/null && echo "FAIL: merge detected" || echo "PASS: no merge"
```

**Expected:** Zero messages contain merge-related language.

---

## Test 8: Worktree Cleaned Up

Verify worktree cleanup message appears in the run topic. This happens regardless of success or failure.

**Expected:** A message containing "Workspace cleaned up" appears in the run topic. The worktree directory no longer exists on disk.

---

## Teardown

Clean up only run-specific artifacts. Do NOT delete project infrastructure.

```bash
# 1. Close any open PRs on Forgejo
FORGEJO_PAT="${FORGEJO_TOKEN:?set FORGEJO_TOKEN}"
PR_NUMBERS=$(curl -sf -H "Authorization: token $FORGEJO_PAT" \
  "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/pulls?state=open" | jq -r '.[].number')
for PR_NUM in $PR_NUMBERS; do
  curl -sf -X PATCH -H "Authorization: token $FORGEJO_PAT" \
    -H "Content-Type: application/json" \
    "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/pulls/$PR_NUM" \
    -d '{"state":"closed"}'
  echo "Closed PR #$PR_NUM"
done

# 2. Delete tj/* branches from Forgejo
BRANCHES=$(curl -sf -H "Authorization: token $FORGEJO_PAT" \
  "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/branches" | jq -r '.[].name' | grep '^tj/')
for BRANCH in $BRANCHES; do
  curl -sf -X DELETE -H "Authorization: token $FORGEJO_PAT" \
    "${FORGEJO_URL}/api/v1/repos/${FORGEJO_OWNER}/${FORGEJO_REPO}/branches/$(python3 -c 'import urllib.parse; print(urllib.parse.quote("'"$BRANCH"'", safe=""))')"
  echo "Deleted branch: $BRANCH"
done

# 3. Cancel any active runs
PROJECT_ID=$(sqlite3 $TURBOJET_HOME/state.db "SELECT id FROM projects WHERE name='test-project';")
ACTIVE=$(sqlite3 $TURBOJET_HOME/state.db "SELECT id FROM pipeline_runs WHERE project_id='$PROJECT_ID' AND status IN ('running','pending');")
for RUN_ID in $ACTIVE; do
  curl -sf -X POST "http://localhost:8100/v1/pipeline/runs/$RUN_ID/cancel"
  echo "Cancelled run: $RUN_ID"
done

# 4. Stop server
./scripts/stop_server.sh

echo "TEARDOWN COMPLETE"
echo "PRs closed, branches deleted, runs cancelled"
echo "Project infrastructure preserved for next run"
```

**Do NOT delete:**
- Project `test-project`
- Secrets (PAT, SSH key)
- Forgejo deploy key
- Lanes
- GitProviderConfig
