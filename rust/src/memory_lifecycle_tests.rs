#[cfg(test)]
mod tests {
    use crate::memory::{consume_memory_lease_write, revoke_memory_lease, verify_memory_lease, MemoryLease, MemoryLeaseRejection};
    use deepstrike_core::mm::memory::MemoryScope;

    #[test]
    fn lease_rules_cover_scope_expiry_revocation_and_budget() {
        let scope = MemoryScope::new("t", "n");
        let mut lease = MemoryLease { lease_id: "l".into(), agent_id: "a".into(), scope: scope.clone(), issued_at: 0, expires_at: 100, max_writes: 1, writes_used: 0, revoked_at: None };
        assert!(verify_memory_lease(Some(&lease), "a", &scope, 50).is_ok());
        assert_eq!(verify_memory_lease(Some(&lease), "b", &scope, 50), Err(MemoryLeaseRejection::LeaseScopeMismatch));
        assert_eq!(verify_memory_lease(Some(&lease), "a", &scope, 100), Err(MemoryLeaseRejection::LeaseExpired));
        revoke_memory_lease(&mut lease, 40);
        assert_eq!(verify_memory_lease(Some(&lease), "a", &scope, 50), Err(MemoryLeaseRejection::LeaseRevoked));
        lease.revoked_at = None;
        consume_memory_lease_write(&mut lease);
        assert_eq!(verify_memory_lease(Some(&lease), "a", &scope, 50), Err(MemoryLeaseRejection::MemoryWriteBudgetExhausted));
    }
}
