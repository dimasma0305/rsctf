use std::time::Duration;

use super::{
    ad_submit_burst_flags, AUTHENTICATED_IP_BACKSTOP_PER_MINUTE, CREDENTIAL_IP_ADMISSION_PER_MINUTE,
};

/// The rate-limit policies, mirroring RSCTF's `RateLimiter.LimitPolicy` plus the
/// always-on `Global` sliding window that every `/api` request passes through.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Policy {
    /// 150 requests / 60s sliding window — all `/api` requests.
    Global,
    /// 50 / 60s on `POST /api/account/login` (per-IP brute-force ceiling).
    Login,
    /// 20 / 300s on the mail-triggering endpoints + oauth start.
    Register,
    /// Token bucket, ~1 token / 5s, small burst — flag submission.
    Submit,
    /// Token bucket, ~1 / 10s — container create/delete/extend.
    Container,
    /// Token bucket, ~1 / 10s with a ~30 burst — heavy DB query routes.
    Query,
    /// One-at-a-time heavy admin routes; modelled as a tight ~1 / 10s bucket.
    Concurrency,
    /// High per-IP abuse backstop for authenticated traffic.
    GlobalIpBackstop,
    /// Cheap source-IP admission before JWT verification or A&D token lookup.
    /// Appended to preserve every shipped Redis policy discriminant.
    CredentialIpAdmission,
    /// Team-scoped A&D batch work budget. The cost is the number of distinct,
    /// plausible flags in the request rather than one token per HTTP request.
    /// Appended to preserve every shipped Redis policy discriminant.
    AdSubmit,
    /// Source-IP admission for privileged hub negotiation and WebSocket upgrade.
    /// Frames inside an established connection are intentionally not charged.
    /// Appended to preserve every shipped Redis policy discriminant.
    PrivilegedHubAdmission,
    /// Source-IP admission for anonymous/public hub negotiation and upgrade.
    /// Long-lived socket counts are bounded separately by `hubs::admission`.
    /// Appended to preserve every shipped Redis policy discriminant.
    PublicHubAdmission,
    /// Per-identity ceiling for player submission-verdict recovery. Appended to
    /// preserve every shipped Redis policy discriminant.
    Verdict,
    /// Fixed-shape A&D bearer digest admission before PostgreSQL.
    AdAuthTokenAdmission,
    /// Source-IP backstop for rotating A&D bearer strings.
    AdAuthSourceAdmission,
    /// Tight anonymous per-source HashPoW issuance budget.
    PowIssuanceSource,
    /// Deployment-wide HashPoW issuance budget; all callers share one key.
    PowIssuanceGlobal,
    /// Tight per-identity defense-in-depth budget for player credential changes.
    /// Appended to preserve every shipped Redis policy discriminant.
    CredentialMutation,
    /// Anonymous source budget for bounded Ed25519 team-token verification.
    TeamSignatureSource,
    /// Deployment-wide CPU/query budget for team-token verification.
    TeamSignatureGlobal,
    /// Trusted solve-verifier issuance work. Appended to preserve every
    /// previously shipped Redis policy discriminant.
    SolveReceipt,
    /// Distributed churn budget for authenticated proxy subjects, workloads,
    /// and participations. Appended to preserve shipped discriminants.
    ProxyOpen,
    /// Higher-capacity source/NAT backstop for proxy-open churn.
    /// Appended to preserve shipped discriminants.
    ProxySourceOpen,
    /// Coarse cross-replica proxy byte credits. Per-frame work remains behind
    /// strict process/session ceilings and never performs a Redis round trip.
    ProxyTraffic,
    /// Per-session Event-VPN challenge/proof mint budget. Appended to preserve
    /// every previously shipped Redis policy discriminant.
    EventVpnMint,
    /// Deployment-wide Event-VPN mint work budget. Appended to preserve every
    /// previously shipped Redis policy discriminant.
    EventVpnMintGlobal,
    /// Cheap source admission before a managed-token digest lookup. Appended
    /// to preserve every previously shipped Redis policy discriminant.
    ManagedApiAuthSourceAdmission,
}

/// The shape of a policy: either a sliding window (log of hit instants) or a
/// token bucket (fractional tokens refilled continuously).
#[derive(Clone, Copy)]
pub(super) enum Kind {
    /// Allow at most `permit` hits within any `window`.
    Sliding { permit: u32, window: Duration },
    /// A bucket of at most `capacity` tokens refilled at `refill_per_sec`; each
    /// request costs one token.
    Bucket { capacity: f64, refill_per_sec: f64 },
}

impl Policy {
    pub(super) fn kind(self) -> Kind {
        match self {
            Policy::Global => Kind::Sliding {
                permit: 150,
                window: Duration::from_secs(60),
            },
            Policy::GlobalIpBackstop => {
                let capacity = *AUTHENTICATED_IP_BACKSTOP_PER_MINUTE as f64;
                Kind::Bucket {
                    capacity,
                    refill_per_sec: capacity / 60.0,
                }
            }
            Policy::CredentialIpAdmission => {
                let capacity = *CREDENTIAL_IP_ADMISSION_PER_MINUTE as f64;
                Kind::Bucket {
                    capacity,
                    refill_per_sec: capacity / 60.0,
                }
            }
            Policy::AdSubmit => Kind::Bucket {
                capacity: ad_submit_burst_flags() as f64,
                refill_per_sec: 10.0,
            },
            Policy::PrivilegedHubAdmission => Kind::Bucket {
                capacity: 120.0,
                refill_per_sec: 10.0,
            },
            Policy::PublicHubAdmission => Kind::Bucket {
                capacity: 512.0,
                refill_per_sec: 10.0,
            },
            Policy::Verdict => Kind::Bucket {
                capacity: 30.0,
                refill_per_sec: 0.5,
            },
            Policy::AdAuthTokenAdmission => Kind::Bucket {
                capacity: 120.0,
                refill_per_sec: 2.0,
            },
            Policy::AdAuthSourceAdmission => Kind::Bucket {
                capacity: 1_200.0,
                refill_per_sec: 20.0,
            },
            Policy::PowIssuanceSource => Kind::Bucket {
                capacity: 8.0,
                refill_per_sec: 0.2,
            },
            Policy::PowIssuanceGlobal => Kind::Bucket {
                capacity: 256.0,
                refill_per_sec: 20.0,
            },
            Policy::CredentialMutation => Kind::Bucket {
                capacity: 6.0,
                refill_per_sec: 0.1,
            },
            Policy::TeamSignatureSource => Kind::Bucket {
                capacity: 20.0,
                refill_per_sec: 1.0,
            },
            Policy::TeamSignatureGlobal => Kind::Bucket {
                capacity: 256.0,
                refill_per_sec: 32.0,
            },
            Policy::SolveReceipt => Kind::Bucket {
                capacity: 128.0,
                refill_per_sec: 16.0,
            },
            Policy::ProxyOpen => Kind::Bucket {
                capacity: 32.0,
                refill_per_sec: 4.0,
            },
            Policy::ProxySourceOpen => Kind::Bucket {
                capacity: 512.0,
                refill_per_sec: 32.0,
            },
            // Units are 64 KiB. This preserves a 1 GiB interactive/bulk burst
            // while bounding sustained deployment-wide work at 64 MiB/s.
            Policy::ProxyTraffic => Kind::Bucket {
                capacity: 16_384.0,
                refill_per_sec: 1_024.0,
            },
            Policy::EventVpnMint => Kind::Bucket {
                capacity: 12.0,
                refill_per_sec: 1.0,
            },
            Policy::EventVpnMintGlobal => Kind::Bucket {
                capacity: 512.0,
                refill_per_sec: 64.0,
            },
            Policy::ManagedApiAuthSourceAdmission => Kind::Bucket {
                capacity: 600.0,
                refill_per_sec: 10.0,
            },
            Policy::Login => Kind::Sliding {
                permit: 50,
                window: Duration::from_secs(60),
            },
            Policy::Register => Kind::Sliding {
                permit: 20,
                window: Duration::from_secs(300),
            },
            Policy::Submit => Kind::Bucket {
                capacity: 12.0,
                refill_per_sec: 1.0 / 5.0,
            },
            Policy::Container => Kind::Bucket {
                capacity: 6.0,
                refill_per_sec: 1.0 / 10.0,
            },
            Policy::Query => Kind::Bucket {
                capacity: 30.0,
                refill_per_sec: 1.0 / 10.0,
            },
            Policy::Concurrency => Kind::Bucket {
                capacity: 1.0,
                refill_per_sec: 1.0 / 10.0,
            },
        }
    }

    /// A fixed-window `(limit, window-in-ms)` representation. Redis uses this for
    /// sliding policies; bucket policies use their native capacity/refill values.
    pub(super) fn fixed_window(self) -> (u32, u64) {
        match self.kind() {
            Kind::Sliding { permit, window } => (permit, window.as_millis() as u64),
            Kind::Bucket {
                capacity,
                refill_per_sec,
            } => (
                capacity as u32,
                ((capacity / refill_per_sec) * 1000.0) as u64,
            ),
        }
    }
}
