//! Coarse distributed proxy byte credits and trusted-receipt work admission.

use super::{
    check_weighted_async, log_redis_fallback, redis_key, redis_with_timeout, DistributedLimiter,
    Kind, Policy, DISTRIBUTED,
};

const PROXY_TRAFFIC_CREDIT_UNIT_BYTES: usize = 64 * 1024;

impl DistributedLimiter {
    /// Lease one coarse byte-credit chunk from every hierarchy dimension in a
    /// single Redis transaction. A denial does not partially charge any key.
    async fn check_proxy_traffic(&self, partitions: [&str; 5], costs: [u32; 5]) -> Result<(), u64> {
        const SCRIPT: &str = r#"
            local capacity = tonumber(ARGV[1])
            local refill_per_sec = tonumber(ARGV[2])
            local clock = redis.call('TIME')
            local now_ms = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
            local next_tokens = {}
            local next_last_ms = {}
            local retry_ms = 0

            for index, key in ipairs(KEYS) do
                local cost = tonumber(ARGV[index + 2])
                local tokens = tonumber(redis.call('HGET', key, 'tokens'))
                local last_ms = tonumber(redis.call('HGET', key, 'last_ms'))
                if not tokens or not last_ms then
                    tokens = capacity
                    last_ms = now_ms
                else
                    tokens = math.max(0, math.min(capacity, tokens))
                    if now_ms > last_ms then
                        tokens = math.min(capacity, tokens + ((now_ms - last_ms) * refill_per_sec / 1000))
                        last_ms = now_ms
                    end
                end
                next_tokens[index] = tokens
                next_last_ms[index] = last_ms
                if cost > capacity then
                    retry_ms = math.max(retry_ms, math.ceil(capacity * 1000 / refill_per_sec))
                elseif tokens < cost then
                    retry_ms = math.max(retry_ms, math.ceil((cost - tokens) * 1000 / refill_per_sec))
                end
            end

            if retry_ms > 0 then return math.max(1, retry_ms) end
            for index, key in ipairs(KEYS) do
                local tokens = next_tokens[index] - tonumber(ARGV[index + 2])
                redis.call('HSET', key, 'tokens', tokens, 'last_ms', next_last_ms[index])
                local full_in_ms = math.ceil((capacity - tokens) * 1000 / refill_per_sec)
                redis.call('PEXPIRE', key, math.max(1, full_in_ms))
            end
            return 0
        "#;
        let Kind::Bucket {
            capacity,
            refill_per_sec,
        } = Policy::ProxyTraffic.kind()
        else {
            return Err(1);
        };
        let keys = partitions.map(|partition| redis_key(Policy::ProxyTraffic, partition));
        let mut conn = self.conn.clone();
        let result = redis_with_timeout(async {
            let script = redis::Script::new(SCRIPT);
            let mut script = script.prepare_invoke();
            for key in &keys {
                script.key(key);
            }
            script.arg(capacity).arg(refill_per_sec);
            for cost in costs {
                script.arg(cost.max(1));
            }
            script.invoke_async(&mut conn).await
        })
        .await;
        match result {
            Ok(retry_ms) if retry_ms > 0 => Err((retry_ms as u64).div_ceil(1_000).max(1)),
            Ok(_) => Ok(()),
            Err(error) => {
                log_redis_fallback(&error);
                // Every socket still holds strict process/session byte and frame
                // ceilings. Redis loss may reduce deployment-wide precision but
                // can never turn the hot path into unbounded work.
                Ok(())
            }
        }
    }
}

/// Bound trusted solve-receipt signing and retained-row growth across replicas.
/// Higher weights give an issuer and participation tighter budgets than the
/// deployment-wide envelope without introducing more policy discriminants.
pub(crate) async fn admit_solve_receipt_issuance(
    issuer: &str,
    participation_id: i32,
) -> Result<(), u64> {
    use sha2::Digest;

    let issuer = hex::encode(sha2::Sha256::digest(issuer.as_bytes()));
    let global = check_weighted_async(Policy::SolveReceipt, "global".to_owned(), 1);
    let issuer = check_weighted_async(Policy::SolveReceipt, format!("issuer:{issuer}"), 4);
    let participation = check_weighted_async(
        Policy::SolveReceipt,
        format!("participation:{participation_id}"),
        8,
    );
    let (global, issuer, participation) = tokio::join!(global, issuer, participation);
    [global, issuer, participation]
        .into_iter()
        .filter_map(Result::err)
        .max()
        .map_or(Ok(()), Err)
}

/// Lease a multi-dimensional byte-credit chunk. Redis is consulted only once
/// per coarse chunk; frames within that chunk stay entirely process-local.
pub(crate) async fn admit_proxy_traffic_credit(
    subject: uuid::Uuid,
    scope: &str,
    source: std::net::IpAddr,
    workload: uuid::Uuid,
    bytes: usize,
) -> Result<(), u64> {
    let Some(distributed) = DISTRIBUTED.get() else {
        return Ok(());
    };
    let units = bytes
        .div_ceil(PROXY_TRAFFIC_CREDIT_UNIT_BYTES)
        .try_into()
        .unwrap_or(u32::MAX)
        .max(1);
    let source_units = units.div_ceil(8).max(1);
    let subject = format!("subject:{subject}");
    let scope = format!("scope:{scope}");
    let source = format!("source:{source}");
    let workload = format!("workload:{workload}");
    distributed
        .check_proxy_traffic(
            ["global", &subject, &scope, &source, &workload],
            [units, units, units, source_units, units],
        )
        .await
}
