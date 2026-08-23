use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{FixedOffset, NaiveDateTime, TimeZone, Utc};
use reqwest::header::{HeaderValue, ACCEPT};
use serde::Deserialize;

use super::{DonationFeed, DonationLeaderboardEntry, DonationMessage, DonationProvider};

const SUPPORTS_URL: &str = "https://api.trakteer.id/v1/public/supports?include=reply_message";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_SUPPORTS: usize = 200;
const MAX_LEADERBOARD: usize = 10;
const MAX_MESSAGES: usize = 20;

static CLIENT: LazyLock<Result<reqwest::Client, String>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("rsctf-donations/1.0")
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| error.to_string())
});

#[derive(Debug, Deserialize)]
struct Envelope {
    status: String,
    status_code: u16,
    result: ResultBody,
}

#[derive(Debug, Deserialize)]
struct ResultBody {
    #[serde(default)]
    data: Vec<Support>,
}

#[derive(Debug, Deserialize)]
struct Support {
    #[serde(default, alias = "creator_name")]
    supporter_name: String,
    #[serde(default)]
    support_message: String,
    #[serde(default)]
    quantity: i64,
    #[serde(default)]
    amount: i64,
    #[serde(default)]
    unit_name: String,
    #[serde(default)]
    status: String,
    updated_at: String,
    #[serde(default)]
    reply_message: Option<String>,
}

#[derive(Debug)]
struct Aggregate {
    name: String,
    amount: i64,
    quantity: i64,
    count: usize,
}

pub(super) async fn fetch(api_key: &str) -> Result<DonationFeed, String> {
    let client = CLIENT
        .as_ref()
        .map_err(|error| format!("HTTP client unavailable: {error}"))?;
    let key = HeaderValue::from_str(api_key).map_err(|_| "invalid API key header".to_owned())?;
    let response = client
        .get(SUPPORTS_URL)
        .header("key", key)
        .header(ACCEPT, "application/json")
        .header("X-Requested-With", "XMLHttpRequest")
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("provider returned HTTP {}", response.status()));
    }
    let envelope: Envelope = bounded_json(response).await?;
    build_feed(envelope)
}

async fn bounded_json(mut response: reqwest::Response) -> Result<Envelope, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("provider response exceeded the size limit".to_owned());
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(MAX_RESPONSE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("provider response read failed: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("provider response exceeded the size limit".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|error| format!("provider response was invalid: {error}"))
}

fn build_feed(envelope: Envelope) -> Result<DonationFeed, String> {
    if envelope.status_code != 200 || !envelope.status.eq_ignore_ascii_case("success") {
        return Err("provider rejected the request".to_owned());
    }

    let mut aggregates: BTreeMap<String, Aggregate> = BTreeMap::new();
    let mut messages = Vec::new();
    for support in envelope
        .result
        .data
        .into_iter()
        .take(MAX_SUPPORTS)
        .filter(|support| {
            support.status.is_empty() || support.status.eq_ignore_ascii_case("success")
        })
    {
        if !(0..=1_000_000_000_000).contains(&support.amount)
            || !(0..=1_000_000).contains(&support.quantity)
        {
            continue;
        }
        let name = clean_text(&support.supporter_name, 120, false);
        let name = if name.is_empty() {
            "Anonymous".to_owned()
        } else {
            name
        };
        let normalized = name.to_lowercase();
        let aggregate = aggregates.entry(normalized).or_insert_with(|| Aggregate {
            name: name.clone(),
            amount: 0,
            quantity: 0,
            count: 0,
        });
        aggregate.amount = aggregate.amount.saturating_add(support.amount);
        aggregate.quantity = aggregate.quantity.saturating_add(support.quantity);
        aggregate.count += 1;

        let message = clean_text(&support.support_message, 500, true);
        if message.is_empty() {
            continue;
        }
        let Some(updated_at) = parse_trakteer_time(&support.updated_at) else {
            continue;
        };
        messages.push(DonationMessage {
            supporter_name: name,
            message,
            amount: support.amount,
            quantity: support.quantity,
            unit_name: clean_text(&support.unit_name, 64, false),
            updated_at,
            reply_message: support
                .reply_message
                .map(|message| clean_text(&message, 500, true))
                .filter(|message| !message.is_empty()),
        });
    }

    let mut rows: Vec<_> = aggregates.into_values().collect();
    rows.sort_by(|left, right| {
        right
            .amount
            .cmp(&left.amount)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    let leaderboard = rows
        .into_iter()
        .take(MAX_LEADERBOARD)
        .enumerate()
        .map(|(index, row)| DonationLeaderboardEntry {
            rank: index + 1,
            supporter_name: row.name,
            total_amount: row.amount,
            total_quantity: row.quantity,
            support_count: row.count,
        })
        .collect();
    messages.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    messages.truncate(MAX_MESSAGES);

    Ok(DonationFeed {
        provider: DonationProvider::Trakteer,
        currency: "IDR",
        fetched_at: Utc::now(),
        leaderboard,
        messages,
    })
}

fn parse_trakteer_time(value: &str) -> Option<chrono::DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    let jakarta = FixedOffset::east_opt(7 * 60 * 60)?;
    jakarta
        .from_local_datetime(&naive)
        .single()
        .map(|value| value.with_timezone(&Utc))
}

fn clean_text(value: &str, max_chars: usize, allow_newlines: bool) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control() || (allow_newlines && *character == '\n'))
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_is_bounded_aggregated_and_sanitized() {
        let envelope: Envelope = serde_json::from_value(serde_json::json!({
            "status": "success",
            "status_code": 200,
            "result": { "data": [
                {
                    "creator_name": " Alice\u{0000} ",
                    "support_message": "Keep going!",
                    "quantity": 2,
                    "amount": 30000,
                    "unit_name": "Kopi",
                    "status": "success",
                    "updated_at": "2025-03-11 13:44:07",
                    "reply_message": "Thanks!"
                },
                {
                    "supporter_name": "alice",
                    "support_message": "Again",
                    "quantity": 1,
                    "amount": 15000,
                    "unit_name": "Kopi",
                    "updated_at": "2025-03-12 09:00:00"
                },
                {
                    "supporter_name": "Refunded",
                    "support_message": "not public",
                    "quantity": 10,
                    "amount": 999999,
                    "unit_name": "Kopi",
                    "status": "refund",
                    "updated_at": "2025-03-12 10:00:00"
                }
            ]}
        }))
        .unwrap();
        let feed = build_feed(envelope).unwrap();
        assert_eq!(feed.leaderboard.len(), 1);
        assert_eq!(feed.leaderboard[0].supporter_name, "Alice");
        assert_eq!(feed.leaderboard[0].total_amount, 45000);
        assert_eq!(feed.leaderboard[0].support_count, 2);
        assert_eq!(feed.messages.len(), 2);
        assert_eq!(feed.messages[0].message, "Again");
        assert_eq!(
            feed.messages[1].updated_at.timestamp_millis(),
            1_741_675_447_000
        );
    }

    #[test]
    fn text_limits_chars_and_removes_controls() {
        assert_eq!(clean_text(" a\u{0000}b\ncd ", 4, true), "ab\nc");
        assert_eq!(clean_text(" a\u{0000}b\ncd ", 4, false), "abcd");
    }
}
