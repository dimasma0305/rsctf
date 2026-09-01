use super::super::*;

#[test]
fn asset_routes_have_distinct_source_work_gate_and_byte_budgets() {
    assert!(matches!(
        Policy::AssetRequestSource.kind(),
        Kind::Bucket {
            capacity: 512.0,
            refill_per_sec: 128.0,
        }
    ));
    assert!(matches!(
        Policy::AssetRequestIdentity.kind(),
        Kind::Bucket {
            capacity: 512.0,
            refill_per_sec: 64.0,
        }
    ));
    assert!(matches!(
        Policy::AssetRequestWork.kind(),
        Kind::Bucket {
            capacity: 2_048.0,
            refill_per_sec: 256.0,
        }
    ));
    assert!(matches!(
        Policy::AssetGateMiss.kind(),
        Kind::Bucket {
            capacity: 256.0,
            refill_per_sec: 128.0,
        }
    ));
    assert!(matches!(
        Policy::AssetResponseBytes.kind(),
        Kind::Bucket {
            capacity: 8_192.0,
            refill_per_sec: 2_048.0,
        }
    ));
    assert!(redis_key(Policy::AssetRequestSource, "source").starts_with("rl:tb:26:"));
    assert!(redis_key(Policy::AssetRequestIdentity, "account").starts_with("rl:tb:27:"));
    assert!(redis_key(Policy::AssetRequestWork, "deployment").starts_with("rl:tb:28:"));
    assert!(redis_key(Policy::AssetResponseBytes, "deployment").starts_with("rl:tb:29:"));
    assert!(redis_key(Policy::AssetGateMiss, "deployment").starts_with("rl:tb:30:"));
}
