//! Prometheus text exposition of the current snapshot at `/metrics`.
//!
//! Everything comes from the status document the dashboard already receives, so scraping
//! costs no database reads. The format is written by hand: a handful of gauges does not
//! justify a metrics crate.

use crate::snapshot::{CoinStatus, PoolSnapshot, WorkerStatus};
use crate::AppState;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use std::fmt::Write;

/// Content type Prometheus expects for the text format.
pub const CONTENT_TYPE_TEXT: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `GET /metrics`.
pub async fn metrics(State(state): State<AppState>) -> Response {
    let body = render(&state.snapshot());
    ([(CONTENT_TYPE, CONTENT_TYPE_TEXT)], body).into_response()
}

/// Render a snapshot in the Prometheus text format.
pub fn render(s: &PoolSnapshot) -> String {
    let mut out = String::with_capacity(4096);
    let mut w = Writer(&mut out);

    w.gauge("alamo_info", "Daemon build information.");
    w.sample(
        "alamo_info",
        &[("version", &s.version), ("pool", &s.pool_name)],
        1.0,
    );
    w.gauge("alamo_uptime_seconds", "Seconds since the daemon started.");
    w.sample("alamo_uptime_seconds", &[], s.uptime_seconds as f64);
    w.gauge(
        "alamo_hashrate_hashes_per_second",
        "Pool hashrate estimate over the recent window.",
    );
    w.sample("alamo_hashrate_hashes_per_second", &[], s.hashrate);
    w.counter(
        "alamo_shares_total",
        "Shares recorded since the pool was created.",
    );
    w.sample(
        "alamo_shares_total",
        &[("result", "accepted")],
        s.shares_accepted as f64,
    );
    w.sample(
        "alamo_shares_total",
        &[("result", "rejected")],
        s.shares_rejected as f64,
    );
    w.counter(
        "alamo_work_total",
        "Accepted work in difficulty units since the pool was created.",
    );
    w.sample("alamo_work_total", &[], s.total_work);
    w.gauge(
        "alamo_best_share_difficulty",
        "Highest share difficulty any worker has found.",
    );
    w.sample("alamo_best_share_difficulty", &[], s.best_share_difficulty);
    w.gauge("alamo_workers", "Workers by state.");
    let connected = s.workers.iter().filter(|w| w.connections > 0).count();
    w.sample("alamo_workers", &[("state", "connected")], connected as f64);
    w.sample(
        "alamo_workers",
        &[("state", "seen")],
        s.workers.len() as f64,
    );

    w.gauge(
        "alamo_worker_hashrate_hashes_per_second",
        "Per-worker hashrate estimate.",
    );
    for worker in &s.workers {
        w.sample(
            "alamo_worker_hashrate_hashes_per_second",
            &[("worker", &worker.name)],
            worker.hashrate,
        );
    }
    w.gauge(
        "alamo_worker_connections",
        "Live stratum sessions per worker.",
    );
    for worker in &s.workers {
        w.sample(
            "alamo_worker_connections",
            &[("worker", &worker.name)],
            worker.connections as f64,
        );
    }
    w.gauge(
        "alamo_worker_difficulty",
        "Current share difficulty per worker.",
    );
    for worker in &s.workers {
        w.sample(
            "alamo_worker_difficulty",
            &[("worker", &worker.name)],
            worker.difficulty,
        );
    }
    w.counter(
        "alamo_worker_shares_total",
        "Shares per worker since first seen.",
    );
    for worker in &s.workers {
        worker_shares(&mut w, worker);
    }

    w.gauge("alamo_coin_height", "Height of the block being mined.");
    w.gauge(
        "alamo_coin_network_difficulty",
        "Network difficulty of the current template.",
    );
    w.gauge(
        "alamo_coin_template_age_seconds",
        "Seconds since the current template was fetched.",
    );
    w.gauge(
        "alamo_coin_coinbase_value",
        "Reward of the current template in base units.",
    );
    w.counter("alamo_coin_blocks_found_total", "Blocks the node accepted.");
    w.gauge(
        "alamo_coin_round_work",
        "Accepted work since the last block, in difficulty units.",
    );
    w.gauge(
        "alamo_coin_round_progress",
        "Round work over the network difficulty; above 1 means running long.",
    );
    w.gauge(
        "alamo_coin_luck_percent",
        "Lifetime luck: expected work over actual work, 100 is average.",
    );
    w.gauge(
        "alamo_coin_expected_blocks",
        "Blocks the pool's lifetime work would find on average at the current difficulty.",
    );
    w.gauge(
        "alamo_coin_block_probability",
        "Probability of at least one block within the horizon at the current hashrate.",
    );
    w.gauge(
        "alamo_coin_expected_seconds_to_block",
        "Expected time to a block at the current hashrate.",
    );
    w.gauge(
        "alamo_node_up",
        "1 when the coin's node answered its last RPC call.",
    );
    w.gauge(
        "alamo_node_stale",
        "1 when the template was withdrawn because the node stayed unreachable.",
    );
    w.gauge("alamo_node_failures", "Consecutive failed node polls.");
    w.gauge(
        "alamo_node_zmq_connected",
        "1 when subscribed to the node's ZMQ hashblock notifications.",
    );
    for coin in &s.coins {
        coin_samples(&mut w, coin);
    }
    out
}

fn worker_shares(w: &mut Writer<'_>, worker: &WorkerStatus) {
    w.sample(
        "alamo_worker_shares_total",
        &[("worker", &worker.name), ("result", "accepted")],
        worker.shares_accepted as f64,
    );
    w.sample(
        "alamo_worker_shares_total",
        &[("worker", &worker.name), ("result", "rejected")],
        worker.shares_rejected as f64,
    );
}

fn coin_samples(w: &mut Writer<'_>, coin: &CoinStatus) {
    let labels: &[(&str, &str)] = &[("coin", &coin.symbol)];
    w.sample("alamo_coin_height", labels, coin.height as f64);
    w.sample(
        "alamo_coin_network_difficulty",
        labels,
        coin.network_difficulty,
    );
    w.sample(
        "alamo_coin_template_age_seconds",
        labels,
        coin.template_age_seconds as f64,
    );
    w.sample(
        "alamo_coin_coinbase_value",
        labels,
        coin.coinbase_value as f64,
    );
    w.sample(
        "alamo_coin_blocks_found_total",
        labels,
        coin.round.blocks_found as f64,
    );
    w.sample("alamo_coin_round_work", labels, coin.round.work);
    w.sample("alamo_coin_round_progress", labels, coin.round.progress);
    if let Some(luck) = coin.round.luck_percent {
        w.sample("alamo_coin_luck_percent", labels, luck);
    }
    w.sample(
        "alamo_coin_expected_blocks",
        labels,
        coin.round.expected_blocks,
    );
    for (horizon, p) in [
        ("hour", coin.odds.p_hour),
        ("day", coin.odds.p_day),
        ("week", coin.odds.p_week),
        ("month", coin.odds.p_month),
        ("year", coin.odds.p_year),
    ] {
        w.sample(
            "alamo_coin_block_probability",
            &[("coin", &coin.symbol), ("horizon", horizon)],
            p,
        );
    }
    if let Some(secs) = coin.odds.expected_seconds {
        w.sample("alamo_coin_expected_seconds_to_block", labels, secs);
    }
    w.sample("alamo_node_up", labels, f64::from(coin.node.connected));
    w.sample("alamo_node_stale", labels, f64::from(coin.node.stale));
    w.sample("alamo_node_failures", labels, f64::from(coin.node.failures));
    if let Some(zmq) = coin.node.zmq {
        w.sample("alamo_node_zmq_connected", labels, f64::from(zmq));
    }
}

struct Writer<'a>(&'a mut String);

impl Writer<'_> {
    fn gauge(&mut self, name: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP {name} {help}\n# TYPE {name} gauge");
    }

    fn counter(&mut self, name: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP {name} {help}\n# TYPE {name} counter");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.0.push_str(name);
        if !labels.is_empty() {
            self.0.push('{');
            for (i, (key, value)) in labels.iter().enumerate() {
                if i > 0 {
                    self.0.push(',');
                }
                self.0.push_str(key);
                self.0.push_str("=\"");
                escape_label(self.0, value);
                self.0.push('"');
            }
            self.0.push('}');
        }
        let _ = writeln!(self.0, " {}", Value(value));
    }
}

/// Label values escape backslash, double quote, and newline.
fn escape_label(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

/// Prometheus number formatting: integers without a fraction, specials by name.
struct Value(f64);

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let v = self.0;
        if v.is_nan() {
            f.write_str("NaN")
        } else if v.is_infinite() {
            f.write_str(if v > 0.0 { "+Inf" } else { "-Inf" })
        } else if v.fract() == 0.0 && v.abs() < 1e15 {
            write!(f, "{}", v as i64)
        } else {
            write!(f, "{v}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{NodeStatus, RoundStatus};
    use alamo_core::odds::OddsSummary;

    #[test]
    fn renders_gauges_with_escaped_labels() {
        let snapshot = PoolSnapshot {
            pool_name: "Alamo \"Test\"".into(),
            version: "0.1.0".into(),
            uptime_seconds: 42,
            hashrate: 1.5e6,
            shares_accepted: 10,
            shares_rejected: 1,
            total_work: 12345.5,
            workers: vec![WorkerStatus {
                name: "rig\\1".into(),
                address: "x".into(),
                fallback: false,
                aux_payouts: vec![],
                connections: 1,
                difficulty: 8.0,
                hashrate: 1.5e6,
                shares_accepted: 10,
                shares_rejected: 1,
                best_difficulty: 100.0,
                work_accepted: 80.0,
                last_share_seconds: Some(1),
            }],
            coins: vec![CoinStatus {
                symbol: "LTC".into(),
                chain: "main".into(),
                height: 2_900_000,
                network_difficulty: 3.0e7,
                template_age_seconds: 4,
                coinbase_value: 625_000_000,
                odds: OddsSummary::compute(1.5e6, 3.0e7),
                round: RoundStatus {
                    blocks_found: 2,
                    luck_percent: Some(95.5),
                    ..Default::default()
                },
                node: NodeStatus {
                    connected: true,
                    zmq: Some(false),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let text = render(&snapshot);
        assert!(text.contains("# TYPE alamo_hashrate_hashes_per_second gauge\n"));
        assert!(text.contains("alamo_hashrate_hashes_per_second 1500000\n"));
        assert!(text.contains("alamo_info{version=\"0.1.0\",pool=\"Alamo \\\"Test\\\"\"} 1\n"));
        assert!(text.contains("alamo_shares_total{result=\"rejected\"} 1\n"));
        assert!(text.contains("alamo_work_total 12345.5\n"));
        assert!(text
            .contains("alamo_worker_shares_total{worker=\"rig\\\\1\",result=\"accepted\"} 10\n"));
        assert!(text.contains("alamo_coin_height{coin=\"LTC\"} 2900000\n"));
        assert!(text.contains("alamo_coin_luck_percent{coin=\"LTC\"} 95.5\n"));
        assert!(text.contains("alamo_coin_block_probability{coin=\"LTC\",horizon=\"day\"} "));
        assert!(text.contains("alamo_node_up{coin=\"LTC\"} 1\n"));
        assert!(text.contains("alamo_node_zmq_connected{coin=\"LTC\"} 0\n"));
        assert!(text.contains("alamo_workers{state=\"connected\"} 1\n"));
    }

    #[test]
    fn optional_values_are_omitted_rather_than_faked() {
        let snapshot = PoolSnapshot {
            coins: vec![CoinStatus {
                symbol: "DOGE".into(),
                chain: "main".into(),
                height: 1,
                network_difficulty: 0.0,
                template_age_seconds: 0,
                coinbase_value: 0,
                odds: OddsSummary::compute(0.0, 0.0),
                round: RoundStatus::default(),
                node: NodeStatus::default(),
            }],
            ..Default::default()
        };
        let text = render(&snapshot);
        assert!(!text.contains("alamo_coin_luck_percent{"));
        assert!(!text.contains("alamo_node_zmq_connected{"));
        assert!(text.contains("alamo_node_up{coin=\"DOGE\"} 0\n"));
    }
}
