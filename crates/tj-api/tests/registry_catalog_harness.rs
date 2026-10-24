//! Batch 2 acceptance: empty-registry startup and registry-derived project/lane
//! admission, with arbitrary synthetic workflow names and no repo assets.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, request::Builder};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tj_api::handlers::build_app;
use tj_config::ConfigLoader;
use tj_manager::{ManagerHandle, spawn_manager};
use tj_test_support::{SyntheticTree, SyntheticWorkflow};
use tower::ServiceExt;

struct AppHarness {
    manager: ManagerHandle,
    app: axum::Router,
    token: String,
}

async fn call(
    app: &axum::Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder: Builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn spawn_app(tree: &SyntheticTree, register: bool) -> AppHarness {
    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    let spawn = spawn_manager(loader).await.unwrap();
    let manager = spawn.handle.clone();
    let app = build_app(spawn).await.unwrap();
    let mut token = String::new();
    if register {
        let (status, auth) = call(
            &app,
            Method::POST,
            "/v1/auth/register",
            None,
            Some(json!({"username": "admin", "password": "registry-pass"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "register: {auth}");
        token = auth["token"].as_str().unwrap().to_string();
    }
    AppHarness {
        manager,
        app,
        token,
    }
}

fn empty_registry_tree() -> (TempDir, SyntheticTree) {
    let temp = tempfile::tempdir().unwrap();
    let tree = SyntheticTree::at(temp.path().join("fixture"));
    tree.update_config(|config| {
        config["workflow_sources"] = json!([]);
        config.as_object_mut().unwrap().remove("smg_path");
    })
    .unwrap();
    // A local provider credential keeps readiness about workflows, not a missing
    // test key. No live call is made.
    unsafe {
        std::env::set_var("TJ_SYNTHETIC_API_KEY", "unused-local-key");
    }
    (temp, tree)
}

#[tokio::test]
async fn empty_registry_serves_http_and_create_project_fails_before_workspace() {
    let (_temp, tree) = empty_registry_tree();
    let harness = spawn_app(&tree, true).await;

    let (status, health) = call(&harness.app, Method::GET, "/api/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "healthy");

    let (status, ready) = call(&harness.app, Method::GET, "/api/ready", None, None).await;
    assert_eq!(status, StatusCode::OK, "ready: {ready}");
    assert_ne!(ready["status"], "ready");
    let workflows = ready["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "workflows")
        .expect("workflows readiness check");
    assert_eq!(workflows["ok"], false);
    assert!(
        workflows["detail"]
            .as_str()
            .unwrap()
            .contains("No workflow installed")
    );
    assert!(
        ready["first_run"]["next_step"]
            .as_str()
            .unwrap()
            .contains("No workflow installed")
    );

    let (status, list) = call(
        &harness.app,
        Method::GET,
        "/v1/workflows",
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "workflows: {list}");
    assert_eq!(list["workflows"], json!([]));
    assert!(list["configured"].is_null());

    let (status, profiles) = call(
        &harness.app,
        Method::GET,
        "/v1/profiles",
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "profiles: {profiles}");
    assert_eq!(profiles["profiles"], json!([]));
    assert_eq!(profiles["catalog_format_version"], 1);

    let (status, project) = call(
        &harness.app,
        Method::POST,
        "/v1/projects",
        Some(&harness.token),
        Some(json!({"name": "Should not exist"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "project: {project}");
    assert!(
        project["error"]
            .as_str()
            .unwrap()
            .contains("No workflow installed")
    );
    assert!(harness.manager.list_projects().unwrap().is_empty());
    assert!(harness.manager.list_lanes().unwrap().is_empty());
    let workspace = std::path::Path::new(harness.manager.workspace_base());
    if workspace.exists() {
        assert_eq!(std::fs::read_dir(workspace).unwrap().count(), 0);
    }

    let _ = harness.manager.shutdown().await;
}

#[tokio::test]
async fn arbitrary_catalog_import_pin_and_start_survive_source_removal_and_enforce_access() {
    unsafe {
        std::env::set_var("TJ_SYNTHETIC_API_KEY", "unused-local-key");
    }
    let temp = tempfile::tempdir().unwrap();
    let workflow = SyntheticWorkflow::new()
        .named("Arbitrary review pipeline")
        .with_catalog("team-review")
        .catalog_label("Team review")
        .with_gate("approval");
    let tree = SyntheticTree::at_with_workflow(temp.path().join("fixture"), workflow);
    let source_dir = tree.root().join("sources");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::copy(tree.graph_path(), source_dir.join("review.smg.json")).unwrap();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([
            {"id": "local-review", "path": "sources", "kind": "directory"}
        ]);
        config.as_object_mut().unwrap().remove("smg_path");
    })
    .unwrap();

    let first = spawn_app(&tree, true).await;
    let token = first.token.clone();

    let (status, profiles) =
        call(&first.app, Method::GET, "/v1/profiles", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "profiles: {profiles}");
    assert_eq!(profiles["profiles"][0]["catalog_id"], "team-review");
    assert_eq!(profiles["profiles"][0]["label"], "Team review");
    assert_eq!(profiles["profiles"][0]["default"], false);

    let (status, created) = call(
        &first.app,
        Method::POST,
        "/v1/projects",
        Some(&token),
        Some(json!({"name": "Review project", "catalog_id": "team-review"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "project: {created}");
    let project_id = created["project"]["id"].as_str().unwrap().to_string();
    let lane_id = created["lane"]["id"].as_str().unwrap().to_string();
    let workflow_id = created["workflow_id"].as_str().unwrap().to_string();
    assert_eq!(created["project"]["catalog_id"], "team-review");
    assert_eq!(created["project"]["workspace_kind"], "none");
    assert_eq!(created["lane"]["workflow_id"], workflow_id);
    assert_eq!(created["project"]["provisioning_state"], "not_required");
    let project_workspace = created["project"]["workspace_path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !std::path::Path::new(&project_workspace)
            .join(".git")
            .exists()
    );

    let (status, exact) = call(
        &first.app,
        Method::POST,
        "/v1/projects",
        Some(&token),
        Some(json!({"name": "Exact version project", "workflow_id": workflow_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "exact project: {exact}");
    assert_eq!(exact["lane"]["workflow_id"], workflow_id);
    assert_eq!(exact["project"]["catalog_id"], "team-review");

    // A repository URL is refused rather than silently ignored for a workflow
    // with no declared repository capability.
    let (status, refused) = call(
        &first.app,
        Method::POST,
        "/v1/projects",
        Some(&token),
        Some(json!({
            "name": "Repo project", "catalog_id": "team-review",
            "repo_url": "https://example.invalid/repo.git"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "repo: {refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("declares no repository workspace")
    );

    // Create a second lane that is intentionally unpinned to prove admission
    // fails with operator guidance instead of selecting a graph implicitly.
    let unpinned = tj_db::Lane {
        id: "lane_unpinned".into(),
        project_id: tj_db::ProjectId(project_id.clone()),
        name: "Unpinned".into(),
        smg_path: "sources/review.smg.json".into(),
        description: None,
        workflow_id: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    first.manager.db().save_lane(&unpinned).unwrap();
    let (status, missing) = call(
        &first.app,
        Method::POST,
        "/v1/pipeline/runs",
        Some(&token),
        Some(json!({
            "description": "must fail", "project_id": project_id, "lane_id": "lane_unpinned"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "missing pin: {missing}");
    assert!(
        missing["error"]
            .as_str()
            .unwrap()
            .contains("no pinned workflow")
    );
    assert!(missing["error"].as_str().unwrap().contains("pin"));

    // A non-admin member cannot repin another user's project lane.
    let (status, user) = call(
        &first.app,
        Method::POST,
        "/v1/users",
        Some(&token),
        Some(json!({"username": "outsider", "password": "outsider-pass"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create user: {user}");
    let (status, auth) = call(
        &first.app,
        Method::POST,
        "/v1/auth/login",
        None,
        Some(json!({"username": "outsider", "password": "outsider-pass"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login: {auth}");
    let outsider = auth["token"].as_str().unwrap().to_string();
    let (status, denied) = call(
        &first.app,
        Method::PUT,
        &format!("/v1/lanes/{lane_id}/workflow"),
        Some(&outsider),
        Some(json!({"workflow_id": workflow_id})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pin auth: {denied}");
    assert!(
        denied["error"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("access denied")
    );

    let _ = first.manager.shutdown().await;

    // Remove the source file and restart. The immutable registry row remains
    // and admits a run without any configured graph file.
    std::fs::remove_file(source_dir.join("review.smg.json")).unwrap();
    let restarted = spawn_app(&tree, false).await;
    let (status, workflows) = call(
        &restarted.app,
        Method::GET,
        "/v1/workflows",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "workflows: {workflows}");
    assert_eq!(workflows["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(workflows["workflows"][0]["catalog_id"], "team-review");
    assert!(
        workflows["configured"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["id"] == "local-review")
    );

    let (status, run) = call(
        &restarted.app,
        Method::POST,
        "/v1/pipeline/runs",
        Some(&token),
        Some(json!({
            "description": "registry-only admission",
            "project_id": project_id,
            "lane_id": lane_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "run: {run}");
    assert_eq!(run["workflow_id"], workflow_id);
    assert!(
        !std::path::Path::new(&project_workspace)
            .join(".git")
            .exists()
    );
    let _ = restarted
        .manager
        .cancel_run(run["id"].as_str().unwrap())
        .await;
    let _ = restarted.manager.shutdown().await;
}

#[tokio::test]
async fn frozen_capabilities_survive_source_edits_and_restart_and_tampered_pin_is_refused() {
    unsafe {
        std::env::set_var("TJ_SYNTHETIC_API_KEY", "unused-local-key");
    }
    let temp = tempfile::tempdir().unwrap();
    let mut capabilities = json!({
        "schema_version": 1,
        "workspace": {"kind": "none"},
    });
    let workflow = SyntheticWorkflow::new()
        .named("Arbitrary frozen capability pipeline")
        .with_catalog("frozen-capabilities")
        .with_capabilities(capabilities.clone());
    let tree = SyntheticTree::at_with_workflow(temp.path().join("fixture"), workflow);
    let source_dir = tree.root().join("sources");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source_path = source_dir.join("frozen.smg.json");
    std::fs::copy(tree.graph_path(), &source_path).unwrap();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([
            {"id": "frozen-source", "path": "sources/frozen.smg.json", "kind": "file"}
        ]);
        config.as_object_mut().unwrap().remove("smg_path");
    })
    .unwrap();

    let first = spawn_app(&tree, true).await;
    let token = first.token.clone();
    let (status, profiles) =
        call(&first.app, Method::GET, "/v1/profiles", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "profiles: {profiles}");
    assert_eq!(profiles["profiles"][0]["repository_free"], true);
    assert_eq!(profiles["profiles"][0]["gate_binding"], "artifact");

    let (status, created) = call(
        &first.app,
        Method::POST,
        "/v1/projects",
        Some(&token),
        Some(json!({"name": "Frozen capability project", "catalog_id": "frozen-capabilities"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "project: {created}");
    let project_id = created["project"]["id"].as_str().unwrap().to_string();
    let lane_id = created["lane"]["id"].as_str().unwrap().to_string();
    let old_workflow_id = created["workflow_id"].as_str().unwrap().to_string();
    assert_eq!(created["project"]["workspace_kind"], "none");

    let (status, run) = call(
        &first.app,
        Method::POST,
        "/v1/pipeline/runs",
        Some(&token),
        Some(json!({
            "description": "frozen capability admission",
            "project_id": project_id,
            "lane_id": lane_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "run: {run}");
    let run_id = run["id"].as_str().unwrap().to_string();
    let (status, first_bundle) = call(
        &first.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run_id}/bundle"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bundle: {first_bundle}");
    assert_eq!(first_bundle["schema_version"], 2);
    assert_eq!(first_bundle["capability"]["workspace"]["kind"], "none");
    let capability_hash = first_bundle["capability_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = first.manager.cancel_run(&run_id).await;
    let _ = first.manager.shutdown().await;

    // Edit the source to declare a different workspace policy. This creates a
    // new immutable registry version; it must not rewrite the existing bundle
    // or let the pinned lane silently inherit the new contract.
    capabilities["workspace"] = json!({"kind": "repository", "provisioning": "optional"});
    let changed = SyntheticWorkflow::new()
        .named("Arbitrary frozen capability pipeline")
        .with_catalog("frozen-capabilities")
        .with_capabilities(capabilities);
    std::fs::write(&source_path, changed.to_json().to_string()).unwrap();

    let restarted = spawn_app(&tree, false).await;
    let (status, workflows) = call(
        &restarted.app,
        Method::GET,
        "/v1/workflows",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "workflows: {workflows}");
    assert_eq!(workflows["workflows"].as_array().unwrap().len(), 2);
    for workflow in workflows["workflows"].as_array().unwrap() {
        assert_eq!(
            workflow["capability_hash"].as_str().unwrap().len(),
            64,
            "registry rows store the validated declaration hash: {workflow}"
        );
    }
    let new_workflow_id = workflows["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workflow| workflow["id"] != old_workflow_id)
        .and_then(|workflow| workflow["id"].as_str())
        .unwrap()
        .to_string();

    let (status, second_bundle) = call(
        &restarted.app,
        Method::GET,
        &format!("/v1/pipeline/runs/{run_id}/bundle"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bundle after restart: {second_bundle}"
    );
    assert_eq!(second_bundle["capability"]["workspace"]["kind"], "none");
    assert_eq!(second_bundle["capability_hash"], capability_hash);

    // The same lane admits again from its original frozen workflow id, not the
    // edited source revision.
    let (status, second_run) = call(
        &restarted.app,
        Method::POST,
        "/v1/pipeline/runs",
        Some(&token),
        Some(json!({
            "description": "still the frozen contract",
            "project_id": project_id,
            "lane_id": lane_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second run: {second_run}");
    assert_eq!(second_run["workflow_id"], old_workflow_id);
    let _ = restarted
        .manager
        .cancel_run(second_run["id"].as_str().unwrap())
        .await;

    // Pinning the edited repository workspace into a non-Git project fails
    // before any clone or workspace change.
    let (status, refused) = call(
        &restarted.app,
        Method::PUT,
        &format!("/v1/lanes/{lane_id}/workflow"),
        Some(&token),
        Some(json!({"workflow_id": new_workflow_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "pin: {refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("workspace policy is incompatible")
    );
    let _ = restarted.manager.shutdown().await;
}

#[tokio::test]
async fn declared_input_capability_is_admitted_for_an_arbitrary_workflow() {
    unsafe {
        std::env::set_var("TJ_SYNTHETIC_API_KEY", "unused-local-key");
    }
    let temp = tempfile::tempdir().unwrap();
    let workflow = SyntheticWorkflow::new()
        .named("Arbitrary input-sync pipeline")
        .with_catalog("input-sync-admitted")
        .with_capabilities(json!({
            "schema_version": 1,
            "workspace": {"kind": "none"},
            "inputs": {"copy_project_inputs_to": "supplied"}
        }));
    let tree = SyntheticTree::at_with_workflow(temp.path().join("fixture"), workflow);
    let harness = spawn_app(&tree, true).await;

    let (status, created) = call(
        &harness.app,
        Method::POST,
        "/v1/projects",
        Some(&harness.token),
        Some(json!({"name": "Input module project", "catalog_id": "input-sync-admitted"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "project: {created}");
    let project_id = created["project"]["id"].as_str().unwrap();
    let lane_id = created["lane"]["id"].as_str().unwrap();

    let (status, admitted) = call(
        &harness.app,
        Method::POST,
        "/v1/pipeline/runs",
        Some(&harness.token),
        Some(json!({
            "description": "declared inputs are implemented",
            "project_id": project_id,
            "lane_id": lane_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "run: {admitted}");
    assert_eq!(harness.manager.list_runs().unwrap().len(), 1);
    let _ = harness.manager.shutdown().await;
}
