//! Protocol state for one miner connection.

use crate::config::VardiffConfig;
use crate::events::{AuxPayoutInfo, BlockCandidate, PoolEvent};
use crate::job::{AuxJob, SessionJob, EXTRANONCE1_LEN, EXTRANONCE2_LEN};
use crate::protocol::{Notification, Request, Response, StratumError};
use crate::validate::{self, Submit};
use crate::vardiff::Vardiff;
use alamo_core::auxpow::{chain_id_of, AuxTree};
use alamo_core::coinbase::CoinbaseParts;
use alamo_core::hash::sha256d;
use alamo_core::header::BlockHeader;
use alamo_core::job::{JobId, RejectReason, ShareOutcome};
use alamo_core::payout::{password_difficulty, PayoutSet, Payouts};
use alamo_core::work::MergedWork;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

/// How many recent jobs a session keeps for late submissions.
const MAX_JOBS: usize = 8;

/// A message to write to the miner.
#[derive(Debug)]
pub enum Outgoing {
    /// Reply to a request.
    Response(Response),
    /// Server-initiated notification.
    Notification(Notification),
}

/// What the connection task should do after a request.
#[derive(Debug, Default)]
pub struct Effects {
    /// Messages to send, in order.
    pub outgoing: Vec<Outgoing>,
    /// Events to report.
    pub events: Vec<PoolEvent>,
    /// Blocks to submit, one per chain whose target was met.
    pub blocks: Vec<BlockCandidate>,
    /// The session cannot continue (for example, no coinbase fits the template).
    pub close: bool,
}

impl Effects {
    fn respond(&mut self, r: Response) {
        self.outgoing.push(Outgoing::Response(r));
    }
    fn notify(&mut self, n: Notification) {
        self.outgoing.push(Outgoing::Notification(n));
    }
}

/// Protocol state for one connection.
pub struct Session {
    id: u64,
    extranonce1: [u8; EXTRANONCE1_LEN],
    payout: Option<Payouts>,
    workers: Vec<String>,
    difficulty: f64,
    next_job: u64,
    jobs: VecDeque<SessionJob>,
    work: Option<Arc<MergedWork>>,
    vardiff: Vardiff,
    payouts: Arc<PayoutSet>,
    /// Header version bits the lane lets miners roll (BIP310); zero when it offers none.
    pool_version_mask: u32,
    /// The mask negotiated with `mining.configure`; zero until then.
    version_mask: u32,
}

impl Session {
    /// Create a session with a fresh extranonce1.
    pub fn new(
        id: u64,
        extranonce1: [u8; EXTRANONCE1_LEN],
        vardiff_cfg: VardiffConfig,
        payouts: Arc<PayoutSet>,
        now: Instant,
    ) -> Self {
        Self {
            id,
            extranonce1,
            payout: None,
            workers: Vec::new(),
            difficulty: vardiff_cfg.initial_difficulty,
            next_job: 1,
            jobs: VecDeque::new(),
            work: None,
            vardiff: Vardiff::new(vardiff_cfg, now),
            payouts,
            pool_version_mask: 0,
            version_mask: 0,
        }
    }

    /// Offer BIP310 version rolling over the header bits in `mask` (the lane coin's
    /// `version_rolling_mask`, BIP320's `0x1fffe000` for Bitcoin). Zero, the default,
    /// keeps `mining.configure` answering `version-rolling: false`.
    pub fn with_version_rolling(mut self, mask: u32) -> Self {
        self.pool_version_mask = mask;
        self
    }

    /// The version-rolling mask negotiated with the miner (zero if none).
    pub fn version_mask(&self) -> u32 {
        self.version_mask
    }

    /// Authorized worker names.
    pub fn workers(&self) -> &[String] {
        &self.workers
    }

    /// Handle one request from the miner.
    pub fn handle(&mut self, req: Request, now: Instant, now_unix: u64) -> Effects {
        let mut fx = Effects::default();
        let Request { id, method, params } = req;
        let params = params.as_array().map(Vec::as_slice).unwrap_or(&[]);
        match method.as_str() {
            "mining.subscribe" => self.subscribe(id, params, &mut fx),
            "mining.authorize" => self.authorize(id, params, &mut fx),
            "mining.submit" => self.submit(id, params, now, now_unix, &mut fx),
            "mining.configure" => self.configure(id, params, &mut fx),
            "mining.extranonce.subscribe" => fx.respond(Response::ok(id, json!(true))),
            "mining.suggest_difficulty" => {
                if let Some(d) = params.first().and_then(Value::as_f64).filter(|d| *d > 0.0) {
                    let d = self.vardiff.clamp(d);
                    if (d - self.difficulty).abs() > f64::EPSILON {
                        self.set_difficulty(d, &mut fx);
                    }
                }
                fx.respond(Response::ok(id, json!(true)));
            }
            "mining.get_transactions" => fx.respond(Response::ok(id, json!([]))),
            "mining.ping" => fx.respond(Response::ok(id, json!("pong"))),
            other => {
                if !id.is_null() {
                    fx.respond(Response::err(id, StratumError::unknown_method(other)));
                }
            }
        }
        fx
    }

    fn subscribe(&mut self, id: Value, params: &[Value], fx: &mut Effects) {
        if let Some(agent) = params.first().and_then(Value::as_str) {
            tracing::debug!(session = self.id, agent, "subscribed");
        }
        let sub_id = format!("{:x}", self.id);
        let result = json!([
            [["mining.set_difficulty", sub_id], ["mining.notify", sub_id]],
            hex::encode(self.extranonce1),
            EXTRANONCE2_LEN,
        ]);
        fx.respond(Response::ok(id, result));
    }

    /// BIP310 `mining.configure`. Answers every requested extension: `version-rolling`
    /// with the mask the miner and the lane share, everything else `false`. A lane that
    /// offers no version rolling answers `{"version-rolling": false}` whatever was asked,
    /// as it always has. It may arrive before or after subscribe and authorize.
    fn configure(&mut self, id: Value, params: &[Value], fx: &mut Effects) {
        if self.pool_version_mask == 0 {
            fx.respond(Response::ok(id, json!({ "version-rolling": false })));
            return;
        }
        let requested = params.first().and_then(Value::as_array);
        let options = params.get(1);
        let mut result = serde_json::Map::new();
        for code in requested.into_iter().flatten().filter_map(Value::as_str) {
            if code == "version-rolling" {
                self.configure_version_rolling(options, &mut result);
            } else {
                result.insert(code.to_owned(), Value::Bool(false));
            }
        }
        fx.respond(Response::ok(id, Value::Object(result)));
    }

    fn configure_version_rolling(
        &mut self,
        options: Option<&Value>,
        result: &mut serde_json::Map<String, Value>,
    ) {
        let option = |name: &str| options.and_then(|o| o.get(name));
        // BIP310: a miner that sends no mask may roll any bit.
        let miner_mask = match option("version-rolling.mask") {
            None | Some(Value::Null) => Some(u32::MAX),
            Some(Value::String(hex)) if !hex.is_empty() && hex.len() <= 8 => {
                u32::from_str_radix(hex, 16).ok()
            }
            Some(_) => None,
        };
        let Some(miner_mask) = miner_mask else {
            self.version_mask = 0;
            result.insert(
                "version-rolling".into(),
                Value::String("version-rolling.mask must be 8 hex digits".into()),
            );
            return;
        };
        let mask = miner_mask & self.pool_version_mask;
        if mask == 0 {
            tracing::info!(
                session = self.id,
                miner_mask = %format_args!("{miner_mask:08x}"),
                pool_mask = %format_args!("{:08x}", self.pool_version_mask),
                "version rolling refused: the miner's mask shares no bits with the pool's"
            );
            self.version_mask = 0;
            result.insert("version-rolling".into(), Value::Bool(false));
            return;
        }
        // BIP310: min-bit-count is diagnostic. A miner offered fewer bits than it wants
        // can still roll them (or not), so the shared mask is granted regardless.
        let min_bits = option("version-rolling.min-bit-count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if u64::from(mask.count_ones()) < min_bits {
            tracing::info!(
                session = self.id,
                mask = %format_args!("{mask:08x}"),
                bits = mask.count_ones(),
                min_bits,
                "version rolling granted with fewer bits than the miner asked for"
            );
        }
        self.version_mask = mask;
        tracing::debug!(session = self.id, mask = %format_args!("{mask:08x}"), "version rolling");
        result.insert("version-rolling".into(), Value::Bool(true));
        result.insert(
            "version-rolling.mask".into(),
            Value::String(format!("{mask:08x}")),
        );
    }

    fn authorize(&mut self, id: Value, params: &[Value], fx: &mut Effects) {
        let username = params
            .first()
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if username.is_empty() {
            fx.respond(Response::err(id, StratumError::other("Missing username")));
            return;
        }
        let password = params.get(1).and_then(Value::as_str).unwrap_or_default();
        let payout = self.payouts.resolve(username, password);
        if let Some(existing) = &self.payout {
            if existing.parent.script != payout.parent.script
                || existing.aux.len() != payout.aux.len()
                || existing
                    .aux
                    .iter()
                    .zip(&payout.aux)
                    .any(|(a, b)| a.payout.script != b.payout.script)
            {
                fx.respond(Response::err(
                    id,
                    StratumError::other("This connection already pays a different address"),
                ));
                return;
            }
        }
        if payout.parent.fallback {
            tracing::warn!(session = self.id, username, fallback = %payout.parent.address, "username is not a valid address; paying fallback");
        }
        for aux in payout.aux.iter().filter(|a| a.payout.fallback) {
            tracing::warn!(session = self.id, username, coin = aux.coin, fallback = %aux.payout.address, "password holds no valid address; paying fallback");
        }
        let first = self.payout.is_none();
        if first {
            if let Some(d) = password_difficulty(password) {
                self.difficulty = self.vardiff.clamp(d);
            }
        }
        if !self.workers.iter().any(|w| w == username) {
            self.workers.push(username.to_owned());
        }
        fx.events.push(PoolEvent::Authorized {
            session: self.id,
            worker: username.to_owned(),
            address: payout.parent.address.clone(),
            fallback: payout.parent.fallback,
            aux: payout
                .aux
                .iter()
                .map(|a| AuxPayoutInfo {
                    coin: a.coin,
                    address: a.payout.address.clone(),
                    fallback: a.payout.fallback,
                })
                .collect(),
        });
        self.payout = Some(payout);
        fx.respond(Response::ok(id, json!(true)));
        if first {
            self.set_difficulty(self.difficulty, fx);
            self.send_current_job(fx);
        }
    }

    fn submit(
        &mut self,
        id: Value,
        params: &[Value],
        now: Instant,
        now_unix: u64,
        fx: &mut Effects,
    ) {
        let Some(payout) = self.payout.as_ref() else {
            fx.respond(Response::err(id, RejectReason::Unauthorized.into()));
            return;
        };
        let str_param = |i: usize| params.get(i).and_then(Value::as_str);
        let (Some(worker), Some(job_id), Some(en2), Some(ntime), Some(nonce)) = (
            str_param(0),
            str_param(1),
            str_param(2),
            str_param(3),
            str_param(4),
        ) else {
            fx.respond(Response::err(id, StratumError::other("Malformed submit")));
            return;
        };
        if !self.workers.iter().any(|w| w == worker) {
            fx.respond(Response::err(id, RejectReason::Unauthorized.into()));
            return;
        }
        // BIP310's sixth parameter. A lane without version rolling ignores it, as it
        // always has, so a scrypt miner that sends one anyway behaves exactly as before.
        let version_bits = match params.get(5) {
            _ if self.pool_version_mask == 0 => Ok(None),
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(hex)) if !hex.is_empty() && hex.len() <= 8 => {
                u32::from_str_radix(hex, 16).map(Some).map_err(drop)
            }
            Some(_) => Err(()),
        };
        let (Ok(extranonce2), Ok(ntime), Ok(nonce), Ok(version_bits)) = (
            hex::decode(en2),
            u32::from_str_radix(ntime, 16),
            u32::from_str_radix(nonce, 16),
            version_bits,
        ) else {
            fx.respond(Response::err(id, StratumError::other("Malformed submit")));
            return;
        };
        let job = job_id
            .parse::<JobId>()
            .ok()
            .and_then(|job_id| self.jobs.iter_mut().find(|j| j.id == job_id));
        let Some(job) = job else {
            fx.respond(Response::err(id, RejectReason::UnknownJob.into()));
            return;
        };

        let submit = Submit {
            worker,
            extranonce2,
            ntime,
            nonce,
            version_bits,
        };
        let job_difficulty = job.difficulty;
        let coin = job.work.coin;
        let (outcome, blocks) = validate::validate(
            job,
            &self.extranonce1,
            &submit,
            self.version_mask,
            &payout.parent.address,
            now_unix,
        );
        let (share_difficulty, rejected) = match &outcome {
            ShareOutcome::Accepted { difficulty } | ShareOutcome::Block { difficulty } => {
                (*difficulty, None)
            }
            ShareOutcome::Rejected(reason) => (0.0, Some(*reason)),
        };
        fx.events.push(PoolEvent::Share {
            session: self.id,
            worker: worker.to_owned(),
            coin,
            job_difficulty,
            share_difficulty,
            rejected,
        });
        match rejected {
            Some(reason) => fx.respond(Response::err(id, reason.into())),
            None => {
                fx.respond(Response::ok(id, json!(true)));
                self.vardiff.on_share(now, job_difficulty);
                fx.blocks = blocks;
            }
        }
        self.maybe_retarget(now, fx);
    }

    /// Called on a timer so idle workers get their difficulty lowered.
    pub fn tick(&mut self, now: Instant) -> Effects {
        let mut fx = Effects::default();
        if self.payout.is_some() {
            self.maybe_retarget(now, &mut fx);
        }
        fx
    }

    fn maybe_retarget(&mut self, now: Instant, fx: &mut Effects) {
        // The new difficulty applies to the next job, as cgminer and ESP-Miner expect.
        // Resending the current work under a new job id would let a miner that restarts
        // its nonce search re-find and resubmit the same solutions past the per-job
        // duplicate check, inflating its share rate right after every retarget.
        if let Some(new) = self.vardiff.evaluate(now, self.difficulty) {
            self.set_difficulty(new, fx);
        }
    }

    fn set_difficulty(&mut self, difficulty: f64, fx: &mut Effects) {
        self.difficulty = difficulty;
        fx.notify(Notification::new(
            "mining.set_difficulty",
            json!([difficulty]),
        ));
        fx.events.push(PoolEvent::DifficultyChanged {
            session: self.id,
            difficulty,
        });
    }

    /// New work arrived: a parent template, aux templates, or both.
    pub fn on_work(&mut self, work: Arc<MergedWork>) -> Effects {
        let mut fx = Effects::default();
        let clean = work.clean_jobs;
        for job in &mut self.jobs {
            if clean {
                job.stale = true;
            }
            // An aux block built on a previous aux tip can no longer be accepted.
            for aux in &mut job.aux {
                let current = work
                    .aux
                    .iter()
                    .any(|w| w.coin == aux.work.coin && w.prev_hash == aux.work.prev_hash);
                aux.stale |= !current;
            }
        }
        self.work = Some(work);
        self.send_job(clean, &mut fx);
        fx
    }

    fn send_current_job(&mut self, fx: &mut Effects) {
        self.send_job(false, fx);
    }

    /// Send the current work as a new job, if there is work and a payout.
    fn send_job(&mut self, clean: bool, fx: &mut Effects) {
        let (Some(work), Some(payout)) = (self.work.clone(), self.payout.as_ref()) else {
            return;
        };
        let (aux, commitment) = match build_aux_jobs(&work, payout) {
            Ok(v) => v,
            Err(err) => {
                tracing::error!(session = self.id, %err, "cannot build aux blocks");
                fx.close = true;
                return;
            }
        };
        let coinbase = match CoinbaseParts::build(
            &work.parent,
            &payout.parent.script,
            &commitment,
            EXTRANONCE1_LEN + EXTRANONCE2_LEN,
        ) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(session = self.id, %err, "cannot build coinbase");
                fx.close = true;
                return;
            }
        };
        let job = SessionJob {
            id: JobId(self.next_job),
            work: work.parent.clone(),
            coinbase,
            aux,
            difficulty: self.difficulty,
            target: work.parent.algorithm.share_target(self.difficulty),
            stale: false,
            seen: Default::default(),
        };
        self.next_job += 1;
        // A new session — including a miner reconnecting after a pool restart — must
        // start with a clean job so the miner drops work from a previous connection.
        let clean_jobs = clean || self.jobs.is_empty();
        fx.notify(Notification::new(
            "mining.notify",
            job.notify_params(clean_jobs),
        ));
        self.jobs.push_back(job);
        while self.jobs.len() > MAX_JOBS {
            self.jobs.pop_front();
        }
    }
}

/// Build one aux block per aux template the session has a payout for, and the commitment
/// to place in the parent coinbase (empty when there are no aux blocks).
fn build_aux_jobs(
    work: &MergedWork,
    payout: &Payouts,
) -> Result<(Vec<AuxJob>, Vec<u8>), alamo_core::coinbase::CoinbaseError> {
    let mut aux = Vec::with_capacity(work.aux.len());
    for template in &work.aux {
        let Some(p) = payout.aux.iter().find(|a| a.coin == template.coin) else {
            continue;
        };
        let coinbase = CoinbaseParts::build(template, &p.payout.script, &[], 0)?.serialize(&[]);
        let header = BlockHeader {
            version: template.version,
            prev_hash: template.prev_hash,
            merkle_root: template.merkle_root(&sha256d(&coinbase)),
            time: template.cur_time,
            bits: template.bits,
            nonce: 0,
        };
        aux.push(AuxJob {
            work: template.clone(),
            address: p.payout.address.clone(),
            coinbase,
            hash: header.block_hash(),
            header,
            chain_index: 0,
            chain_branch: Vec::new(),
            stale: false,
        });
    }
    if aux.is_empty() {
        return Ok((aux, Vec::new()));
    }
    let leaves: Vec<(u32, _)> = aux
        .iter()
        .map(|a| (chain_id_of(a.work.version), a.hash))
        .collect();
    let tree = AuxTree::build(&leaves).expect("non-empty, distinct chain ids");
    let commitment = tree.commitment().to_vec();
    for (job, proof) in aux.iter_mut().zip(tree.proofs) {
        job.chain_index = proof.index;
        job.chain_branch = proof.branch;
    }
    Ok((aux, commitment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::address::{encode_base58, encode_segwit, AddressParams};
    use alamo_core::payout::{AuxPayoutTable, PayoutTable};
    use alamo_core::work::WorkTemplate;

    const PARAMS: AddressParams = AddressParams {
        p2pkh_prefix: 111,
        p2sh_prefixes: &[58, 196],
        bech32_hrp: Some("rltc"),
    };
    const DOGE_PARAMS: AddressParams = AddressParams {
        p2pkh_prefix: 111,
        p2sh_prefixes: &[196],
        bech32_hrp: None,
    };

    fn addr(byte: u8) -> String {
        encode_segwit("rltc", 0, &[byte; 20]).unwrap()
    }

    fn doge_addr(byte: u8) -> String {
        encode_base58(111, &[byte; 20])
    }

    fn work(clean: bool) -> Arc<MergedWork> {
        let mut w = WorkTemplate::regtest_sample(5, None);
        w.clean_jobs = clean;
        Arc::new(MergedWork::solo(Arc::new(w)))
    }

    fn merged(parent_id: u64, doge_prev: u8) -> Arc<MergedWork> {
        let mut parent = WorkTemplate::regtest_sample(5, None);
        parent.id = JobId(parent_id);
        let mut doge = WorkTemplate::regtest_aux_sample(31);
        doge.prev_hash = [doge_prev; 32];
        Arc::new(MergedWork {
            parent: Arc::new(parent),
            aux: vec![Arc::new(doge)],
            clean_jobs: false,
        })
    }

    fn session() -> Session {
        let payouts = Arc::new(PayoutSet {
            parent: PayoutTable::new(PARAMS, &addr(9)).unwrap(),
            aux: vec![AuxPayoutTable {
                coin: "DOGE",
                table: PayoutTable::new(DOGE_PARAMS, &doge_addr(9)).unwrap(),
            }],
        });
        Session::new(
            9,
            [1, 2, 3, 4],
            VardiffConfig::default(),
            payouts,
            Instant::now(),
        )
    }

    fn req(method: &str, params: Value) -> Request {
        Request {
            id: json!(1),
            method: method.into(),
            params,
        }
    }

    #[test]
    fn subscribe_authorize_then_notify_on_work() {
        let mut s = session();
        let fx = s.handle(
            req("mining.subscribe", json!(["cgminer"])),
            Instant::now(),
            0,
        );
        let Outgoing::Response(r) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(r.result[1], "01020304");
        assert_eq!(r.result[2], 4);

        // Authorizing before any template: set_difficulty is sent, no job yet.
        let fx = s.handle(
            req("mining.authorize", json!([format!("{}.rig", addr(1)), "x"])),
            Instant::now(),
            0,
        );
        assert!(matches!(&fx.outgoing[0], Outgoing::Response(r) if r.result == json!(true)));
        assert!(
            matches!(&fx.outgoing[1], Outgoing::Notification(n) if n.method == "mining.set_difficulty")
        );
        assert_eq!(fx.outgoing.len(), 2);

        let fx = s.on_work(work(true));
        let Outgoing::Notification(n) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(n.method, "mining.notify");
        assert_eq!(n.params[0], "1");
        assert_eq!(n.params[8], true);
        assert_eq!(n.params[5], "20000000");
        assert_eq!(n.params[6], "207fffff");
    }

    #[test]
    fn first_job_after_reconnect_is_clean_even_if_the_template_is_not() {
        // Pool restart: the miner reconnects, the current template is a refresh
        // (clean_jobs=false), but the new session has no prior jobs to keep.
        let mut s = session();
        s.handle(
            req("mining.authorize", json!([addr(1), "x"])),
            Instant::now(),
            0,
        );
        let fx = s.on_work(work(false));
        let Outgoing::Notification(n) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(n.method, "mining.notify");
        assert_eq!(n.params[0], "1");
        assert_eq!(n.params[8], true);

        let fx = s.on_work(work(false));
        let Outgoing::Notification(n) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(n.params[0], "2");
        assert_eq!(n.params[8], false);
    }

    #[test]
    fn password_sets_the_doge_payout_and_the_coinbase_commits_to_it() {
        let mut s = session();
        let fx = s.handle(
            req("mining.authorize", json!([addr(1), doge_addr(3)])),
            Instant::now(),
            0,
        );
        let PoolEvent::Authorized { aux, .. } = &fx.events[0] else {
            panic!()
        };
        assert_eq!(aux[0].coin, "DOGE");
        assert_eq!(aux[0].address, doge_addr(3));
        assert!(!aux[0].fallback);

        let fx = s.on_work(merged(1, 0x22));
        let Outgoing::Notification(n) = &fx.outgoing[0] else {
            panic!()
        };
        let coinb1 = hex::decode(n.params[2].as_str().unwrap()).unwrap();
        let job = s.jobs.back().unwrap();
        assert_eq!(job.aux.len(), 1);
        assert_eq!(job.aux[0].address, doge_addr(3));
        let mut committed = job.aux[0].hash;
        committed.reverse();
        assert!(coinb1
            .windows(36)
            .any(|w| w[..4] == [0xfa, 0xbe, 0x6d, 0x6d] && w[4..] == committed));

        // A DOGE tip change: non-clean job, and the old job's aux block goes stale
        // while its parent share stays valid.
        let fx = s.on_work(merged(1, 0x33));
        let Outgoing::Notification(n) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(n.params[8], false);
        assert!(s.jobs[0].aux[0].stale);
        assert!(!s.jobs[0].stale);
        assert!(!s.jobs[1].aux[0].stale);
    }

    #[test]
    fn second_address_on_same_connection_is_refused() {
        let mut s = session();
        s.handle(
            req("mining.authorize", json!([addr(1), "x"])),
            Instant::now(),
            0,
        );
        let fx = s.handle(
            req("mining.authorize", json!([addr(2), "x"])),
            Instant::now(),
            0,
        );
        assert!(matches!(&fx.outgoing[0], Outgoing::Response(r) if r.error.is_some()));
        let fx = s.handle(
            req("mining.authorize", json!([addr(1), doge_addr(2)])),
            Instant::now(),
            0,
        );
        assert!(matches!(&fx.outgoing[0], Outgoing::Response(r) if r.error.is_some()));
    }

    #[test]
    fn invalid_username_pays_fallback_and_is_reported() {
        let mut s = session();
        let fx = s.handle(
            req("mining.authorize", json!(["bogus.rig", "x"])),
            Instant::now(),
            0,
        );
        assert!(
            matches!(&fx.events[0], PoolEvent::Authorized { fallback: true, address, aux, .. } if *address == addr(9) && aux[0].fallback && aux[0].address == doge_addr(9))
        );
    }

    #[test]
    fn submit_before_authorize_is_unauthorized() {
        let mut s = session();
        let fx = s.handle(
            req(
                "mining.submit",
                json!(["w", "1", "00000000", "00000000", "00000000"]),
            ),
            Instant::now(),
            0,
        );
        let Outgoing::Response(r) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(r.error.as_ref().unwrap().code, 24);
    }

    #[test]
    fn unknown_job_is_reported() {
        let mut s = session();
        let a = addr(1);
        s.handle(req("mining.authorize", json!([a, "x"])), Instant::now(), 0);
        s.on_work(work(true));
        let fx = s.handle(
            req(
                "mining.submit",
                json!([a, "ff", "00000001", "65500000", "00000000"]),
            ),
            Instant::now(),
            1_700_000_000,
        );
        let Outgoing::Response(r) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(r.error.as_ref().unwrap().code, 21);
    }

    #[test]
    fn retarget_changes_difficulty_without_resending_the_job() {
        let t0 = Instant::now();
        let cfg = VardiffConfig::default();
        let mut s = session();
        s.handle(req("mining.subscribe", json!(["m"])), t0, 0);
        s.handle(
            req("mining.authorize", json!([format!("{}.rig", addr(1)), "x"])),
            t0,
            0,
        );
        let fx = s.on_work(work(true));
        assert!(
            matches!(&fx.outgoing[0], Outgoing::Notification(n) if n.method == "mining.notify")
        );
        assert_eq!(s.jobs.len(), 1);

        // A quiet worker gets its difficulty lowered on the timer: set_difficulty only.
        let fx = s.tick(t0 + std::time::Duration::from_secs_f64(cfg.retarget_seconds + 1.0));
        assert_eq!(fx.outgoing.len(), 1, "{:?}", fx.outgoing);
        let Outgoing::Notification(n) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(n.method, "mining.set_difficulty");
        let lowered = n.params[0].as_f64().unwrap();
        assert!(lowered < cfg.initial_difficulty);
        assert_eq!(s.jobs.len(), 1, "no new job until new work arrives");

        // The next job carries the new target.
        let fx = s.on_work(work(false));
        assert!(
            matches!(&fx.outgoing[0], Outgoing::Notification(n) if n.method == "mining.notify")
        );
        let newest = s.jobs.iter().max_by_key(|j| j.id.0).unwrap();
        assert_eq!(newest.difficulty, lowered);
    }

    fn btc_session(mask: u32) -> Session {
        session().with_version_rolling(mask)
    }

    fn result(fx: &Effects) -> &Value {
        let Outgoing::Response(r) = &fx.outgoing[0] else {
            panic!("{:?}", fx.outgoing)
        };
        &r.result
    }

    #[test]
    fn scrypt_lanes_still_refuse_version_rolling() {
        let mut s = session();
        let fx = s.handle(
            req(
                "mining.configure",
                json!([["version-rolling", "minimum-difficulty"], {"version-rolling.mask": "1fffe000"}]),
            ),
            Instant::now(),
            0,
        );
        assert_eq!(result(&fx), &json!({ "version-rolling": false }));
        assert_eq!(s.version_mask(), 0);
    }

    #[test]
    fn configure_negotiates_the_shared_mask_and_refuses_other_extensions() {
        let mut s = btc_session(0x1fff_e000);
        let fx = s.handle(
            req(
                "mining.configure",
                json!([
                    ["minimum-difficulty", "version-rolling", "subscribe-extranonce"],
                    {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2,
                     "minimum-difficulty.value": 2048}
                ]),
            ),
            Instant::now(),
            0,
        );
        assert_eq!(
            result(&fx),
            &json!({
                "minimum-difficulty": false,
                "version-rolling": true,
                "version-rolling.mask": "1fffe000",
                "subscribe-extranonce": false,
            })
        );
        assert_eq!(s.version_mask(), 0x1fff_e000);

        // BIP310's own example: the result is the miner's mask AND the pool's.
        let mut s = btc_session(0x1fff_e000);
        let fx = s.handle(
            req(
                "mining.configure",
                json!([["version-rolling"], {"version-rolling.mask": "18000000", "version-rolling.min-bit-count": 2}]),
            ),
            Instant::now(),
            0,
        );
        assert_eq!(
            result(&fx),
            &json!({"version-rolling": true, "version-rolling.mask": "18000000"})
        );

        // No mask means the miner can roll anything (BIP310 default "ffffffff").
        let mut s = btc_session(0x1fff_e000);
        let fx = s.handle(
            req("mining.configure", json!([["version-rolling"], {}])),
            Instant::now(),
            0,
        );
        assert_eq!(result(&fx)["version-rolling.mask"], "1fffe000");

        // Fewer shared bits than the miner's min-bit-count: still granted (BIP310 treats
        // the count as diagnostic).
        let mut s = btc_session(0x1fff_e000);
        let fx = s.handle(
            req(
                "mining.configure",
                json!([["version-rolling"], {"version-rolling.mask": "00006000", "version-rolling.min-bit-count": 3}]),
            ),
            Instant::now(),
            0,
        );
        assert_eq!(
            result(&fx),
            &json!({"version-rolling": true, "version-rolling.mask": "00006000"})
        );
        assert_eq!(s.version_mask(), 0x0000_6000);
        // No shared bits at all: refused.
        let mut s = btc_session(0x1fff_e000);
        let fx = s.handle(
            req(
                "mining.configure",
                json!([["version-rolling"], {"version-rolling.mask": "e0000000", "version-rolling.min-bit-count": 0}]),
            ),
            Instant::now(),
            0,
        );
        assert_eq!(result(&fx), &json!({"version-rolling": false}));
        assert_eq!(s.version_mask(), 0);
        // A malformed mask gets an error string, as BIP310 allows.
        let fx = s.handle(
            req(
                "mining.configure",
                json!([["version-rolling"], {"version-rolling.mask": "xyz"}]),
            ),
            Instant::now(),
            0,
        );
        assert!(result(&fx)["version-rolling"].is_string());
    }

    #[test]
    fn configure_after_authorize_enables_the_sixth_submit_parameter() {
        let mut s = btc_session(0x1fff_e000);
        let a = addr(1);
        s.handle(req("mining.authorize", json!([a, "x"])), Instant::now(), 0);
        s.on_work(work(true));
        // Before configure nothing may be rolled.
        let submit = |bits: &str| {
            req(
                "mining.submit",
                json!([a, "1", "00000001", "6553f100", "00000000", bits]),
            )
        };
        let fx = s.handle(submit("00002000"), Instant::now(), 1_700_000_000);
        assert_eq!(result_error(&fx), (20, "Invalid version bits"));
        s.handle(
            req(
                "mining.configure",
                json!([["version-rolling"], {"version-rolling.mask": "1fffe000"}]),
            ),
            Instant::now(),
            0,
        );
        let fx = s.handle(submit("00002000"), Instant::now(), 1_700_000_000);
        assert_ne!(result_error(&fx).1, "Invalid version bits");
        let fx = s.handle(submit("00001000"), Instant::now(), 1_700_000_000);
        assert_eq!(result_error(&fx), (20, "Invalid version bits"));
        let fx = s.handle(submit("nothex"), Instant::now(), 1_700_000_000);
        assert_eq!(result_error(&fx), (20, "Malformed submit"));
    }

    #[test]
    fn a_scrypt_lane_ignores_a_sixth_submit_parameter() {
        let mut s = session();
        let a = addr(1);
        s.handle(req("mining.authorize", json!([a, "x"])), Instant::now(), 0);
        s.on_work(work(true));
        let fx = s.handle(
            req(
                "mining.submit",
                json!([a, "1", "00000001", "6553f100", "00000000", "ffffffff"]),
            ),
            Instant::now(),
            1_700_000_000,
        );
        let (code, message) = result_error(&fx);
        assert_eq!((code, message), (23, "Low difficulty share"));
    }

    fn result_error(fx: &Effects) -> (i32, &str) {
        let Outgoing::Response(r) = &fx.outgoing[0] else {
            panic!("{:?}", fx.outgoing)
        };
        let e = r.error.as_ref().expect("an error reply");
        (e.code, e.message.as_str())
    }

    #[test]
    fn d_in_the_password_sets_the_starting_difficulty_within_the_lane_limits() {
        let cfg = VardiffConfig::default();
        let start = |password: &str| {
            let mut s = session();
            let fx = s.handle(
                req("mining.authorize", json!([addr(1), password])),
                Instant::now(),
                0,
            );
            let Some(Outgoing::Notification(n)) = fx.outgoing.get(1) else {
                panic!("{:?}", fx.outgoing)
            };
            assert_eq!(n.method, "mining.set_difficulty");
            n.params[0].as_f64().unwrap()
        };
        assert_eq!(start("d=4096"), 4096.0);
        assert_eq!(start(&format!("doge={},d=4096", doge_addr(3))), 4096.0);
        assert_eq!(start(&format!("{} d=4096", doge_addr(3))), 4096.0);
        assert_eq!(start("d=1"), cfg.min_difficulty, "clamped to the minimum");
        assert_eq!(
            start("d=1e12"),
            cfg.max_difficulty,
            "clamped to the maximum"
        );
        assert_eq!(start("d=banana"), cfg.initial_difficulty);
        assert_eq!(start("x"), cfg.initial_difficulty);

        // The aux payout next to d= still resolves.
        let mut s = session();
        let fx = s.handle(
            req(
                "mining.authorize",
                json!([addr(1), format!("{} d=4096", doge_addr(3))]),
            ),
            Instant::now(),
            0,
        );
        assert!(
            matches!(&fx.events[0], PoolEvent::Authorized { aux, .. } if aux[0].address == doge_addr(3) && !aux[0].fallback)
        );
    }

    #[test]
    fn fractional_difficulty_reaches_the_miner_and_the_job_target() {
        // SHA-256d lanes start tiny devices at a thousandth of a share.
        let cfg = VardiffConfig {
            initial_difficulty: 1024.0,
            min_difficulty: 0.001,
            max_difficulty: 4_294_967_296.0,
            ..VardiffConfig::default()
        };
        let mut s = Session::new(
            9,
            [1, 2, 3, 4],
            cfg,
            session().payouts.clone(),
            Instant::now(),
        );
        let fx = s.handle(
            req("mining.authorize", json!([addr(1), "d=0.001"])),
            Instant::now(),
            0,
        );
        let Outgoing::Notification(n) = &fx.outgoing[1] else {
            panic!()
        };
        assert_eq!(serde_json::to_string(&n.params).unwrap(), "[0.001]");
        let mut w = WorkTemplate::regtest_sample(5, None);
        w.algorithm = alamo_core::Algorithm::Sha256d;
        s.on_work(Arc::new(MergedWork::solo(Arc::new(w))));
        let job = s.jobs.back().unwrap();
        assert_eq!(job.difficulty, 0.001);
        assert_eq!(
            job.target,
            alamo_core::target::Target::from_difficulty(0.001)
        );
        // 32 fractional bits: within a part in ten million of the asked difficulty.
        assert!((job.target.difficulty() / 0.001 - 1.0).abs() < 1e-7);
        // A thousandth of a share is a target a thousand times easier than difficulty 1.
        assert!(job.target > alamo_core::Algorithm::Sha256d.share_target(1.0));
        // Below the minimum it clamps to it.
        let fx = s.handle(
            req("mining.suggest_difficulty", json!([0.00001])),
            Instant::now(),
            0,
        );
        assert!(
            fx.outgoing.len() == 1,
            "already at the minimum: no set_difficulty, {:?}",
            fx.outgoing
        );
    }
}
