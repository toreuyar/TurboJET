//! Declared requirements-packet production module.
//!
//! The producer flow and stage come from the frozen contract. The module only
//! persists the newest durable artifact of that exact stage; it never searches
//! another flow by name or falls back to "the first artifact that looks like a
//! packet".

use anyhow::Result;
use tj_config::capability::RequirementsPacketCapability;
use tj_db::Database;

/// Persist the generated packet from the declared producer stage. Returns
/// `None` when the declared stage produced no structured packet object.
pub fn record_generated_packet(
    db: &Database,
    run_id: &str,
    producer: &RequirementsPacketCapability,
) -> Result<Option<tj_db::packets::PacketRevision>> {
    let artifacts = db.list_artifacts(run_id)?;
    let candidate = artifacts
        .into_iter()
        .filter(|artifact| artifact.flow_name == producer.produce_flow)
        .filter(|artifact| artifact.stage_name == producer.producer_stage)
        .filter_map(|artifact| {
            let value: serde_json::Value = serde_json::from_str(&artifact.content).ok()?;
            let packet = value.get("packet")?.clone();
            Some((artifact, packet))
        })
        .max_by(|(left, _), (right, _)| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.version.cmp(&right.version))
        });
    let Some((artifact, packet_value)) = candidate else {
        return Ok(None);
    };
    let packet_json = serde_json::to_string(&packet_value)?;
    let packet = tj_packet::RequirementsPacket::from_json(&packet_json)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let allowed = db.packet_allowed_sources(run_id)?;
    packet
        .validate_known_sources(&allowed)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let draft = tj_db::packets::NewPacketRevision {
        run_id: run_id.to_string(),
        packet_id: None,
        content_json: packet_json,
        parent_revision_id: None,
        source_artifact_id: Some(artifact.id.clone()),
        source_artifact_hash: Some(artifact.content_hash.clone()),
        flow_name: Some(artifact.flow_name.clone()),
        stage_name: Some(artifact.stage_name.clone()),
        change_request: None,
        created_by: "system:packet-generation".to_string(),
        allowed_sources: Some(allowed),
    };
    let (revision, replayed) = db.insert_packet_revision(&draft)?;
    tracing::info!(
        run_id, packet_id = %revision.packet_id, revision = revision.revision,
        hash = %revision.content_hash, completeness = %revision.completeness, replayed,
        flow = %producer.produce_flow, stage = %producer.producer_stage,
        "Requirements packet revision recorded from declared stage output"
    );
    Ok(Some(revision))
}
