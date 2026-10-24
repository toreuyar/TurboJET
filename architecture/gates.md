# Gates

Gates are separate flow definitions in the State Machine Graph (SMG) with `type: "gate"`. Gate flows have no stages. Routes reference cross-flow stages using `{flow, stage}` objects instead of string stage names.

## Gate Flow Lifecycle

1. Manager dispatches gate flow -> Flow executes (no stages) -> returns `Completed`
2. Manager halts on gate flows, marks flow as waiting
3. External trigger (Concierge, project management webhook) calls `SystemOperations::approve_gate(flow_id)`
4. Manager resumes: creates new Flow for the next (non-gate) flow in the pipeline

## SMG Example

```json
{
  "discovery-gate": {
    "type": "gate",
    "routes": {
      "proceed": "_next-flow",
      "reject": { "flow": "discovery", "stage": "intake" }
    }
  }
}
```

Gate routes use `{ "flow": "...", "stage": "..." }` objects for cross-flow references. String values for special keywords (`_next-flow`, `_exit`, `_escalate`) and same-flow stage references.
