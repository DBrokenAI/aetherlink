use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long an acquired lease stays valid before auto-expiring.
pub const LEASE_DURATION: Duration = Duration::from_secs(5 * 60);

/// Outcome of an `acquire_lease` call.
#[derive(Debug)]
pub enum AcquireResult {
    /// The lease is now held by the caller until `expires_in` from now.
    Granted { expires_in: Duration },
    /// Another agent already holds an active lease on this file.
    Denied { remaining: Duration },
}

/// In-memory file lease registry. Single-process only — sufficient because
/// AetherLink runs as one MCP server per project root.
pub struct LeaseManager {
    leases: HashMap<PathBuf, Instant>, // value = expiry instant
}

impl LeaseManager {
    pub fn new() -> Self {
        Self { leases: HashMap::new() }
    }

    /// Try to acquire a lease on `path`. Granted if the file is currently
    /// "Open" (no lease, or the previous lease has expired). Denied otherwise.
    pub fn acquire(&mut self, path: PathBuf) -> AcquireResult {
        self.acquire_at(path, Instant::now())
    }

    /// Same as `acquire`, but lets tests inject the "now" instant.
    pub fn acquire_at(&mut self, path: PathBuf, now: Instant) -> AcquireResult {
        if let Some(&expiry) = self.leases.get(&path) {
            if expiry > now {
                return AcquireResult::Denied { remaining: expiry - now };
            }
            // Expired — fall through and re-acquire.
        }
        let expiry = now + LEASE_DURATION;
        self.leases.insert(path, expiry);
        AcquireResult::Granted { expires_in: LEASE_DURATION }
    }

    /// Release a lease early. Returns the outcome so callers can distinguish
    /// "released an active lease" from "no lease was held" from "lease had
    /// already expired anyway".
    pub fn release(&mut self, path: &Path) -> ReleaseResult {
        self.release_at(path, Instant::now())
    }

    pub fn release_at(&mut self, path: &Path, now: Instant) -> ReleaseResult {
        match self.leases.remove(path) {
            None => ReleaseResult::NotHeld,
            Some(expiry) if expiry <= now => ReleaseResult::AlreadyExpired,
            Some(_) => ReleaseResult::Released,
        }
    }
}

/// Outcome of a `release_lease` call.
#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseResult {
    /// An active lease was released early — the file is now Open again.
    Released,
    /// The stored lease had already passed its expiry time. The slot was
    /// cleaned up, but the file was effectively already free.
    AlreadyExpired,
    /// No lease existed for that file at all.
    NotHeld,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_acquire_is_granted() {
        let mut mgr = LeaseManager::new();
        let result = mgr.acquire(PathBuf::from("src/foo.rs"));
        assert!(matches!(result, AcquireResult::Granted { .. }));
    }

    #[test]
    fn second_acquire_on_same_file_is_denied() {
        let mut mgr = LeaseManager::new();
        let path = PathBuf::from("src/foo.rs");
        let _ = mgr.acquire(path.clone());
        let result = mgr.acquire(path);
        match result {
            AcquireResult::Denied { remaining } => {
                assert!(remaining.as_secs() > 0);
                assert!(remaining <= LEASE_DURATION);
            }
            _ => panic!("expected Denied"),
        }
    }

    #[test]
    fn different_files_dont_block_each_other() {
        let mut mgr = LeaseManager::new();
        let r1 = mgr.acquire(PathBuf::from("a.rs"));
        let r2 = mgr.acquire(PathBuf::from("b.rs"));
        assert!(matches!(r1, AcquireResult::Granted { .. }));
        assert!(matches!(r2, AcquireResult::Granted { .. }));
    }

    #[test]
    fn release_active_lease_unlocks_file() {
        let mut mgr = LeaseManager::new();
        let path = PathBuf::from("a.rs");
        let _ = mgr.acquire(path.clone());
        assert_eq!(mgr.release(&path), ReleaseResult::Released);
        // After release, a different caller can acquire immediately.
        assert!(matches!(mgr.acquire(path), AcquireResult::Granted { .. }));
    }

    #[test]
    fn release_unknown_file_reports_not_held() {
        let mut mgr = LeaseManager::new();
        assert_eq!(mgr.release(Path::new("never_locked.rs")), ReleaseResult::NotHeld);
    }

    #[test]
    fn release_expired_lease_reports_already_expired() {
        let mut mgr = LeaseManager::new();
        let path = PathBuf::from("a.rs");
        let now = Instant::now();
        let _ = mgr.acquire_at(path.clone(), now);
        let later = now + LEASE_DURATION + Duration::from_secs(1);
        assert_eq!(mgr.release_at(&path, later), ReleaseResult::AlreadyExpired);
    }

    #[test]
    fn expired_lease_can_be_reacquired() {
        let mut mgr = LeaseManager::new();
        let path = PathBuf::from("a.rs");
        let now = Instant::now();
        let _ = mgr.acquire_at(path.clone(), now);
        // Jump past the expiry.
        let later = now + LEASE_DURATION + Duration::from_secs(1);
        let result = mgr.acquire_at(path, later);
        assert!(matches!(result, AcquireResult::Granted { .. }));
    }
}
