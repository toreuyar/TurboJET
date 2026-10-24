//! Shared validation for the verified SSH host identity stored by the API.

pub fn validate_known_hosts(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty(),
        "at least one host/key line is required"
    );
    for line in value
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        let mut fields = line.split_whitespace();
        let host = fields.next().unwrap_or_default();
        let key_type = fields.next().unwrap_or_default();
        let key = fields.next().unwrap_or_default();
        anyhow::ensure!(
            !host.is_empty()
                && matches!(
                    key_type,
                    "ssh-ed25519"
                        | "ssh-rsa"
                        | "ecdsa-sha2-nistp256"
                        | "ecdsa-sha2-nistp384"
                        | "ecdsa-sha2-nistp521"
                )
                && !key.is_empty(),
            "not a recognised host/key line: {line}"
        );
    }
    Ok(())
}
