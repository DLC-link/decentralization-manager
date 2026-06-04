DELETE FROM chain_audit_cache
WHERE event_type IN ('create', 'other') OR governance_type = 'unknown';
