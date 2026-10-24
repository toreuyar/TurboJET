# Chat Provider Architecture: Abstract Interface + Zulip

See Zettelkasten note: `20260515T173706102680000`

- Abstract `ChatProvider` trait with Zulip as first implementation
- One stream per project, one topic per stage execution
- Deterministic topic format: `{lane_slug} #{run_id} / {stage_name}`
- Bidirectional routing via DB table (session_id ↔ conversation_id)
- Zulip connection: REST API outbound, event queue long-polling inbound
- Crate structure: `tj-chat` (trait) + `tj-chat-zulip` (implementation)
