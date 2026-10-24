use crate::smg::{FlowType, RouteTarget, StateMachineGraph};
use anyhow::{Context, Result, bail, ensure};
use std::collections::{HashMap, HashSet, VecDeque};

/// Validate the executable structure shared by startup, workflow import, run
/// admission, and runtime resolution. Experimental maturity is informational:
/// it never permits an invalid route or unsupported route form.
pub fn validate_graph(smg: &StateMachineGraph) -> Result<()> {
    ensure!(
        smg.version == 1,
        "workflow '{}' has unsupported version {} (expected 1)",
        smg.name,
        smg.version
    );
    ensure!(!smg.name.trim().is_empty(), "workflow name is required");
    ensure!(
        smg.limits.max_iterations > 0,
        "workflow '{}': limits.maxIterations must be positive",
        smg.name
    );
    ensure!(
        smg.limits.max_steps > 0,
        "workflow '{}': limits.maxSteps must be positive",
        smg.name
    );
    ensure!(
        smg.limits.max_depth > 0,
        "workflow '{}': limits.maxDepth must be positive",
        smg.name
    );
    ensure!(
        !smg.flow_order.is_empty(),
        "workflow '{}': flow-order must not be empty",
        smg.name
    );
    ensure!(
        smg.flows.contains_key(&smg.entry),
        "workflow '{}': entry flow '{}' does not exist",
        smg.name,
        smg.entry
    );
    ensure!(
        smg.flows.contains_key(&smg.exit),
        "workflow '{}': exit flow '{}' does not exist",
        smg.name,
        smg.exit
    );
    ensure!(
        smg.flow_order.first() == Some(&smg.entry),
        "workflow '{}': entry '{}' must be first in flow-order",
        smg.name,
        smg.entry
    );
    ensure!(
        smg.flow_order.last() == Some(&smg.exit),
        "workflow '{}': exit '{}' must be last in flow-order",
        smg.name,
        smg.exit
    );

    let mut order = HashSet::new();
    for name in &smg.flow_order {
        ensure!(
            order.insert(name),
            "workflow '{}': flow-order repeats flow '{}'",
            smg.name,
            name
        );
        ensure!(
            smg.flows.contains_key(name),
            "workflow '{}': flow-order references unknown flow '{}'",
            smg.name,
            name
        );
    }
    for name in smg.flows.keys() {
        ensure!(
            order.contains(name),
            "workflow '{}': flow '{}' is missing from flow-order",
            smg.name,
            name
        );
    }

    for (index, flow_name) in smg.flow_order.iter().enumerate() {
        let flow = smg.flows.get(flow_name).expect("flow-order checked above");
        match flow.flow_type {
            FlowType::Gate => validate_gate(smg, flow_name, flow, index)?,
            FlowType::Flow => validate_flow(smg, flow_name, flow, index)?,
        }
    }
    ensure!(
        smg.flows
            .get(&smg.entry)
            .is_some_and(|f| f.flow_type == FlowType::Flow),
        "workflow '{}': entry must be an executable flow, not a gate",
        smg.name
    );
    // A supported repository-free Discovery profile ends at its final approval
    // gate. Keeping the gate as `exit` makes that terminal behaviour explicit
    // in the frozen graph instead of relying on "there happens to be no next
    // flow" after a gate.
    let exit_is_terminal_gate = smg
        .flows
        .get(&smg.exit)
        .is_some_and(|f| f.flow_type == FlowType::Gate)
        && smg.flow_order.last() == Some(&smg.exit);
    ensure!(
        exit_is_terminal_gate
            || smg
                .flows
                .get(&smg.exit)
                .is_some_and(|f| f.flow_type == FlowType::Flow),
        "workflow '{}': exit must be an executable flow or the final approval gate",
        smg.name
    );
    Ok(())
}

fn validate_gate(
    smg: &StateMachineGraph,
    name: &str,
    flow: &crate::smg::FlowDef,
    index: usize,
) -> Result<()> {
    ensure!(
        index > 0,
        "workflow '{}': gate '{}' cannot be the entry",
        smg.name,
        name
    );
    ensure!(
        smg.flow_order.get(index - 1).is_some_and(|prev| smg
            .flows
            .get(prev)
            .is_some_and(|f| f.flow_type == FlowType::Flow)),
        "workflow '{}': gate '{}' must follow an executable flow",
        smg.name,
        name
    );
    ensure!(
        flow.entry.is_none() && flow.exit.is_none() && flow.stages.is_empty(),
        "workflow '{}': gate '{}' is a manager-owned checkpoint; gate entry/exit/stages are unsupported and must be removed",
        smg.name,
        name
    );
    ensure!(
        flow.routes.len() == 2
            && flow.routes.contains_key("approved")
            && flow.routes.contains_key("rejected"),
        "workflow '{}': gate '{}' must declare exactly approved and rejected routes",
        smg.name,
        name
    );
    ensure!(
        flow.routes.get("approved") == Some(&RouteTarget::Stage("_next-flow".into())),
        "workflow '{}': gate '{}' approved route must target _next-flow",
        smg.name,
        name
    );
    ensure!(
        flow.routes.get("rejected") == Some(&RouteTarget::Stage("_rework".into())),
        "workflow '{}': gate '{}' rejected route must target _rework",
        smg.name,
        name
    );
    if let Some(next) = smg.flow_order.get(index + 1) {
        ensure!(
            smg.flows
                .get(next)
                .is_some_and(|f| f.flow_type == FlowType::Flow),
            "workflow '{}': gate '{}' must be followed by an executable flow",
            smg.name,
            name
        );
    }
    Ok(())
}

fn validate_flow(
    smg: &StateMachineGraph,
    name: &str,
    flow: &crate::smg::FlowDef,
    index: usize,
) -> Result<()> {
    let entry = flow
        .entry
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| {
            format!(
                "workflow '{}': flow '{}' requires an entry stage",
                smg.name, name
            )
        })?;
    let exit = flow
        .exit
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| {
            format!(
                "workflow '{}': flow '{}' requires an exit stage",
                smg.name, name
            )
        })?;
    ensure!(
        !flow.stages.is_empty(),
        "workflow '{}': flow '{}' must declare at least one stage",
        smg.name,
        name
    );
    ensure!(
        flow.stages.contains_key(entry),
        "workflow '{}': flow '{}' entry stage '{}' does not exist",
        smg.name,
        name,
        entry
    );
    ensure!(
        flow.stages.contains_key(exit),
        "workflow '{}': flow '{}' exit stage '{}' does not exist",
        smg.name,
        name,
        exit
    );
    ensure!(
        flow.routes.is_empty(),
        "workflow '{}': flow '{}' declares flow-level routes, which are not executable; declare routes on stages",
        smg.name,
        name
    );

    let has_later_flow = smg.flow_order.get(index + 1).is_some();
    let is_graph_exit = name == smg.exit;
    let mut stage_graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for (stage_name, stage) in &flow.stages {
        ensure!(
            !stage.agent.trim().is_empty(),
            "workflow '{}': flow '{}' stage '{}' requires an agent/ADC reference",
            smg.name,
            name,
            stage_name
        );
        ensure!(
            stage.timeout > 0,
            "workflow '{}': flow '{}' stage '{}' timeout must be positive",
            smg.name,
            name,
            stage_name
        );
        ensure!(
            stage.runner_type.as_deref().is_none_or(str::is_empty),
            "workflow '{}': flow '{}' stage '{}' declares unsupported runner_type {:?}",
            smg.name,
            name,
            stage_name,
            stage.runner_type
        );
        ensure!(
            !stage.routes.is_empty(),
            "workflow '{}': flow '{}' stage '{}' has no routes",
            smg.name,
            name,
            stage_name
        );
        let mut edges = Vec::new();
        for (verdict, target) in &stage.routes {
            ensure!(
                !verdict.trim().is_empty(),
                "workflow '{}': flow '{}' stage '{}' has an empty verdict key",
                smg.name,
                name,
                stage_name
            );
            match target {
                RouteTarget::FlowStage { flow, stage } => bail!(
                    "workflow '{}': flow '{}' stage '{}' uses unsupported cross-flow target {{flow: '{}', stage: '{}'}}; route flows through flow-order instead",
                    smg.name,
                    name,
                    stage_name,
                    flow,
                    stage
                ),
                RouteTarget::Stage(target) if target.starts_with('_') => match target.as_str() {
                    "_escalate" => {}
                    "_next-flow" => {
                        ensure!(
                            *stage_name == exit,
                            "workflow '{}': flow '{}' stage '{}' routes _next-flow but is not its declared exit stage '{}'",
                            smg.name,
                            name,
                            stage_name,
                            exit
                        );
                        ensure!(
                            has_later_flow,
                            "workflow '{}': flow '{}' routes _next-flow but is already the final flow",
                            smg.name,
                            name
                        );
                    }
                    "_exit" | "_end" => {
                        ensure!(
                            is_graph_exit && *stage_name == exit,
                            "workflow '{}': only the exit stage '{}' of final flow '{}' may route to {}",
                            smg.name,
                            exit,
                            smg.exit,
                            target
                        );
                    }
                    other => bail!(
                        "workflow '{}': flow '{}' stage '{}' uses unknown reserved route target '{}'",
                        smg.name,
                        name,
                        stage_name,
                        other
                    ),
                },
                RouteTarget::Stage(target) => {
                    ensure!(
                        flow.stages.contains_key(target),
                        "workflow '{}': flow '{}' stage '{}' targets unknown stage '{}'",
                        smg.name,
                        name,
                        stage_name,
                        target
                    );
                    edges.push(target.as_str());
                }
            }
        }
        stage_graph.insert(stage_name.as_str(), edges);
    }

    let exit_routes = &flow.stages.get(exit).expect("exit checked above").routes;
    if is_graph_exit {
        ensure!(
            exit_routes.values().any(
                |target| matches!(target, RouteTarget::Stage(v) if v == "_exit" || v == "_end")
            ),
            "workflow '{}': final exit stage '{} / {}' must declare _exit (or legacy _end)",
            smg.name,
            name,
            exit
        );
    } else {
        ensure!(
            exit_routes
                .values()
                .any(|target| target == &RouteTarget::Stage("_next-flow".into())),
            "workflow '{}': exit stage '{} / {}' must declare _next-flow",
            smg.name,
            name,
            exit
        );
    }

    let mut reached = HashSet::new();
    let mut queue = VecDeque::from([entry]);
    while let Some(stage) = queue.pop_front() {
        if reached.insert(stage) {
            queue.extend(stage_graph.get(stage).into_iter().flatten().copied());
        }
    }
    ensure!(
        reached.contains(exit),
        "workflow '{}': declared exit stage '{}' in flow '{}' is unreachable from entry '{}'",
        smg.name,
        exit,
        name,
        entry
    );
    for stage in flow.stages.keys() {
        ensure!(
            reached.contains(stage.as_str()),
            "workflow '{}': stage '{}' in flow '{}' is unreachable from entry '{}'",
            smg.name,
            stage,
            name,
            entry
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smg::StateMachineGraph;
    use serde_json::json;

    fn graph() -> StateMachineGraph {
        serde_json::from_value(json!({
            "version":1,"name":"test","entry":"work","exit":"work",
            "limits":{"maxIterations":2,"maxSteps":5,"maxDepth":2},
            "flow-order":["work"],
            "flows":{"work":{"type":"flow","entry":"start","exit":"start","stages":{"start":{"agent":"agent.json","routes":{"success":"_exit"}}}}}
        })).unwrap()
    }

    #[test]
    fn graph_accepts_a_single_valid_terminal_flow() {
        validate_graph(&graph()).unwrap();
    }

    #[test]
    fn graph_rejects_unknown_route_targets_and_unreachable_stages() {
        let mut smg = graph();
        smg.flows
            .get_mut("work")
            .unwrap()
            .stages
            .get_mut("start")
            .unwrap()
            .routes
            .insert("success".into(), RouteTarget::Stage("typo".into()));
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("unknown stage 'typo'")
        );

        let mut smg = graph();
        smg.flows.get_mut("work").unwrap().stages.insert(
            "orphan".into(),
            serde_json::from_value(json!({"agent":"orphan.json","routes":{"success":"_escalate"}}))
                .unwrap(),
        );
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("unreachable")
        );
    }

    #[test]
    fn graph_rejects_unsupported_cross_flow_and_invalid_gate_contracts() {
        let mut smg = graph();
        smg.flows
            .get_mut("work")
            .unwrap()
            .stages
            .get_mut("start")
            .unwrap()
            .routes
            .insert(
                "success".into(),
                RouteTarget::FlowStage {
                    flow: "next".into(),
                    stage: "start".into(),
                },
            );
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("unsupported cross-flow target")
        );
    }

    fn gated_graph() -> StateMachineGraph {
        serde_json::from_value(json!({
            "version":1,"name":"gated","entry":"work","exit":"work2",
            "limits":{"maxIterations":2,"maxSteps":5,"maxDepth":2},
            "flow-order":["work","gate","work2"],
            "flows":{
                "work":{"type":"flow","entry":"start","exit":"start","stages":{"start":{"agent":"agent.json","routes":{"success":"_next-flow"}}}},
                "gate":{"type":"gate","routes":{"approved":"_next-flow","rejected":"_rework"},"stages":{}},
                "work2":{"type":"flow","entry":"end","exit":"end","stages":{"end":{"agent":"agent.json","routes":{"success":"_exit"}}}}
            }
        })).unwrap()
    }

    #[test]
    fn graph_rejects_invalid_limits_order_and_entry_exit_contracts() {
        let mut smg = graph();
        smg.limits.max_iterations = 0;
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("maxIterations")
        );
        let mut smg = graph();
        smg.limits.max_steps = 0;
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("maxSteps")
        );
        let mut smg = graph();
        smg.limits.max_depth = 0;
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("maxDepth")
        );

        let mut smg = graph();
        smg.flow_order = vec!["work".into(), "missing".into(), "work".into()];
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("unknown flow")
        );
        let mut smg = graph();
        smg.flow_order.push("work".into());
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("repeats")
        );
        let mut smg = graph();
        smg.entry = "missing".into();
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("entry flow")
        );
    }

    #[test]
    fn graph_rejects_gate_stages_and_wrong_gate_routes() {
        let mut smg = gated_graph();
        smg.flows.get_mut("gate").unwrap().stages.insert(
            "checkpoint".into(),
            serde_json::from_value(json!({"agent":"agent.json","routes":{"success":"_next-flow"}}))
                .unwrap(),
        );
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("gate entry/exit/stages are unsupported")
        );

        let mut smg = gated_graph();
        smg.flows.get_mut("gate").unwrap().routes.remove("rejected");
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("exactly approved and rejected")
        );

        let mut smg = gated_graph();
        smg.flows
            .get_mut("gate")
            .unwrap()
            .routes
            .insert("approved".into(), RouteTarget::Stage("work2".into()));
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("approved route must target _next-flow")
        );
    }

    #[test]
    fn graph_rejects_flow_without_later_flow_for_next_flow_and_requires_explicit_terminal_route() {
        let mut smg = gated_graph();
        smg.flows
            .get_mut("work2")
            .unwrap()
            .stages
            .get_mut("end")
            .unwrap()
            .routes
            .insert("success".into(), RouteTarget::Stage("_next-flow".into()));
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("already the final flow")
        );

        let mut smg = gated_graph();
        smg.flows
            .get_mut("work2")
            .unwrap()
            .stages
            .get_mut("end")
            .unwrap()
            .routes
            .insert("success".into(), RouteTarget::Stage("end".into()));
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("must declare _exit")
        );

        let mut smg = gated_graph();
        smg.flows
            .get_mut("work")
            .unwrap()
            .stages
            .get_mut("start")
            .unwrap()
            .routes
            .insert("success".into(), RouteTarget::Stage("_exit".into()));
        assert!(
            validate_graph(&smg)
                .unwrap_err()
                .to_string()
                .contains("only the exit stage")
        );
    }

    #[test]
    fn graph_rejects_flow_level_routes_that_runtime_would_ignore() {
        let mut smg = gated_graph();
        smg.flows
            .get_mut("work")
            .unwrap()
            .routes
            .insert("success".into(), RouteTarget::Stage("work2".into()));
        let error = validate_graph(&smg).unwrap_err().to_string();
        assert!(
            error.contains("flow-level routes") && error.contains("flow 'work'"),
            "{error}"
        );
    }
}
