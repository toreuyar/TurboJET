# Engineering one-task draft-PR live pilot (TJ-038) — BLOCKED

**Status:** NOT RUN — external real-forge/SSH/reviewer prerequisites missing\
**Task:** TJ-038\
**Local stand-in:** `crates/tj-manager/tests/publication_harness.rs` and
the local integration-operation record (TJ-034–TJ-038)

> This scenario is preparation for a real pilot. The local bare-Git + HTTP
> Forgejo mock is not real-forge evidence. Do not check TJ-038 or claim a
> verified remote SHA/PR until this scenario is executed against a disposable
> **real** Forgejo instance over SSH, with a human reviewer and owner-approved
> scope.

## Required prerequisites (none are satisfied here)

- [ ] A disposable real Forgejo (or approved equivalent) instance whose URL,
      owner/repository and base branch are recorded in the candidate evidence.
- [ ] A dedicated disposable repository, not production, with a protected base
      branch and no auto-merge/auto-release.
- [ ] A project SSH key generated through the TurboJET API and its public key
      registered on the forge.
- [ ] A verified host identity written with
      `PUT /v1/projects/{project_id}/ssh-known-hosts`; the value must come from
      the real forge host, not copied from this repository.
- [ ] A provider API token stored through the project secrets API and a git
      provider configuration pointing at that disposable forge.
- [ ] A supported live model/credential approved for paid calls, or an explicit
      recorded decision to use the scripted model only (which does **not** prove
      model quality/cost).
- [ ] A named human reviewer and owner-approved pilot scope (one language/forge,
      one task size, correction-effort collection, cost/time capture).
- [ ] A disposable database/workspace and a copy of logs/artifacts under
      `/tmp/opencode/` (or an access-controlled evidence store).

## Execution

1. Create a full-profile project pointing at the disposable SSH repository.
   Confirm `provisioning_state=ready`; a failed clone must remain `failed` and
   must not contain an empty repository.
2. Run Discovery and approve the exact packet revision.
3. Run Planning, approve the exact plan revision, then select exactly one ready
   task. Confirm the run waits at `engineering_task_selection_required` before
   selection and that no model/repository side effect occurred.
4. Execute Engineering. Record model/tool calls, budget, test/review/acceptance
   artifact IDs/hashes, branch, commit SHA, changed files, and the draft PR URL.
5. Verify on the forge that the remote branch SHA equals the published commit,
   the PR is a **draft**, and its body includes task/requirements/criteria,
   plan/packet hashes, test/review evidence and limitations. Confirm no merge or
   ready-for-review action was performed by TurboJET.
6. Run the failure injections on fresh disposable repositories:
   - cancelling before publication: no remote branch/PR, workspace retained;
   - rejected push (forge hook/policy): publication `push_state=failed`, no PR,
     workspace retained, no success claim;
   - forge PR 5xx: push succeeded, PR failed, workspace retained;
   - lost PR response after remote creation: lookup reconciles to one PR only;
   - restart after a push whose response was lost: startup/retry verifies the
     remote SHA, opens no duplicate PR and never replays model/tool work.
7. Restart the process during a gate wait and during publication. Record that
   the run is `interrupted` unless a durable reconciliation proves the operation
   completed, and that no automatic duplicate publication occurs.
8. Obtain reviewer feedback: correction effort, success/failure, cost/time,
   invented scope or missing evidence. The reviewer and owner sign the decision.

## Acceptance evidence to attach

- Real forge URL/repository, base branch, remote branch and verified SHA.
- Draft PR URL/number and a snapshot of the body/evidence links.
- Run id, selection id, plan/packet revision IDs and hashes.
- Test/review/acceptance artifact IDs/hashes and reproducible commands.
- Local database/publication state and retained-workspace evidence for failures.
- Reviewer correction effort/success/cost and owner-approved pilot scope.
- Explicit limitations: model/provider versions, language/forge, task size,
  excluded work, and anything not proven.

## Blocked result (current)

No real forge, SSH identity, live provider approval or human reviewer is
available in this environment. The local harness proves the protocol logic only.
TJ-038 remains unchecked; TJ-039/TJ-040 must not start until this live evidence
and an explicit CI/CD target decision exist.
