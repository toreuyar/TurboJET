# Anonymization and retention (TJ-028)

- Replace participant names, emails, employers, customer names, product URLs
  and proprietary identifiers with stable anonymous IDs.
- Keep the mapping/key in the coordinator's access-controlled store, outside
  the repository, until the retention period ends.
- Do not paraphrase requests in a way that changes requirements. Remove only
  identifying material; record each removal in the pair record.
- Formatting normalization (branding, timestamps, internal IDs) is separate
  from anonymization and must be logged.
- Raw identifiable records: retention `____` days, then delete or irreversibly
  anonymize.
- Published artifacts: anonymized rows, outputs, reviewer scores, negative
  results, deviations and limitations.
- Accidental disclosure: stop collection, notify the owner, remove the affected
  artifact, record the incident, and do not publish until resolved.
