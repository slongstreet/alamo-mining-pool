//! Protocol state for one miner connection.

use crate::config::VardiffConfig;
use crate::events::{BlockCandidate, PoolEvent};
use crate::job::{SessionJob, EXTRANONCE1_LEN, EXTRANONCE2_LEN};
use crate::protocol::{Notification, Request, Response, StratumError};
use crate::validate::{self, Submit};
use crate::vardiff::Vardiff;
use alamo_core::coinbase::CoinbaseParts;
use alamo_core::job::{JobId, ShareOutcome};
use alamo_core::payout::{Payout, PayoutResolver};
use alamo_core::target::Target;
use alamo_core::work::WorkTemplate;
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
    /// A block to submit.
    pub block: Option<BlockCandidate>,
    /// The miner asked us to close the connection or misbehaved.
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
    subscribed: bool,
    user_agent: Option<String>,
    payout: Option<Payout>,
    workers: Vec<String>,
    difficulty: f64,
    next_job: u64,
    jobs: VecDeque<SessionJob>,
    work: Option<Arc<WorkTemplate>>,
    vardiff: Vardiff,
    resolver: Arc<dyn PayoutResolver>,
}

impl Session {
    /// Create a session with a fresh extranonce1.
    pub fn new(
        id: u64,
        extranonce1: [u8; EXTRANONCE1_LEN],
        vardiff_cfg: VardiffConfig,
        resolver: Arc<dyn PayoutResolver>,
        now: Instant,
    ) -> Self {
        Self {
            id,
            extranonce1,
            subscribed: false,
            user_agent: None,
            payout: None,
            workers: Vec::new(),
            difficulty: vardiff_cfg.initial_difficulty,
            next_job: 1,
            jobs: VecDeque::new(),
            work: None,
            vardiff: Vardiff::new(vardiff_cfg, now),
            resolver,
        }
    }

    /// Session id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Authorized worker names.
    pub fn workers(&self) -> &[String] {
        &self.workers
    }

    /// Reported miner software, if any.
    pub fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }

    /// Current share difficulty.
    pub fn difficulty(&self) -> f64 {
        self.difficulty
    }

    /// Handle one request from the miner.
    pub fn handle(&mut self, req: Request, now: Instant, now_unix: u64) -> Effects {
        let mut fx = Effects::default();
        let id = req.id.clone();
        let params = req.params.as_array().cloned().unwrap_or_default();
        match req.method.as_str() {
            "mining.subscribe" => self.subscribe(id, &params, &mut fx),
            "mining.authorize" => self.authorize(id, &params, &mut fx),
            "mining.submit" => self.submit(id, &params, now, now_unix, &mut fx),
            "mining.configure" => {
                // No extensions (version rolling is not used by scrypt miners).
                fx.respond(Response::ok(id, json!({ "version-rolling": false })));
            }
            "mining.extranonce.subscribe" => fx.respond(Response::ok(id, json!(true))),
            "mining.suggest_difficulty" => {
                if let Some(d) = params.first().and_then(Value::as_f64) {
                    if d > 0.0 {
                        let d = self.vardiff.clamp(d);
                        if (d - self.difficulty).abs() > f64::EPSILON {
                            self.set_difficulty(d, &mut fx);
                            self.resend_current_job(&mut fx);
                        }
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
        self.user_agent = params.first().and_then(Value::as_str).map(str::to_owned);
        self.subscribed = true;
        let sub_id = format!("{:x}", self.id);
        let result = json!([
            [["mining.set_difficulty", sub_id], ["mining.notify", sub_id]],
            hex::encode(self.extranonce1),
            EXTRANONCE2_LEN,
        ]);
        fx.respond(Response::ok(id, result));
    }

    fn authorize(&mut self, id: Value, params: &[Value], fx: &mut Effects) {
        let Some(username) = params.first().and_then(Value::as_str).map(str::trim) else {
            fx.respond(Response::err(id, StratumError::other("Missing username")));
            return;
        };
        if username.is_empty() {
            fx.respond(Response::err(id, StratumError::other("Empty username")));
            return;
        }
        let payout = self.resolver.resolve(username);
        if let Some(existing) = &self.payout {
            if existing.script != payout.script {
                fx.respond(Response::err(
                    id,
                    StratumError::other("This connection already pays a different address"),
                ));
                return;
            }
        }
        let first = self.payout.is_none();
        if !self.workers.iter().any(|w| w == username) {
            self.workers.push(username.to_owned());
        }
        fx.events.push(PoolEvent::Authorized {
            session: self.id,
            worker: username.to_owned(),
            address: payout.address.clone(),
            fallback: payout.fallback,
        });
        self.payout = Some(payout);
        fx.respond(Response::ok(id, json!(true)));
        if first {
            self.set_difficulty(self.difficulty, fx);
            if self.work.is_some() {
                self.send_job(true, fx);
            }
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
        let Some(payout) = self.payout.clone() else {
            fx.respond(Response::err(
                id,
                StratumError {
                    code: 24,
                    message: "Unauthorized worker".into(),
                },
            ));
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
            fx.respond(Response::err(
                id,
                StratumError {
                    code: 24,
                    message: "Unauthorized worker".into(),
                },
            ));
            return;
        }
        let parsed = (|| {
            Some(Submit {
                worker: worker.to_owned(),
                extranonce2: hex::decode(en2).ok()?,
                ntime: u32::from_str_radix(ntime, 16).ok()?,
                nonce: u32::from_str_radix(nonce, 16).ok()?,
            })
        })();
        let Some(submit) = parsed else {
            fx.respond(Response::err(id, StratumError::other("Malformed submit")));
            return;
        };
        let Ok(job_id) = job_id.parse::<JobId>() else {
            fx.respond(Response::err(
                id,
                StratumError {
                    code: 21,
                    message: "Job not found".into(),
                },
            ));
            return;
        };
        let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) else {
            fx.respond(Response::err(
                id,
                StratumError {
                    code: 21,
                    message: "Job not found".into(),
                },
            ));
            return;
        };

        let job_difficulty = job.difficulty;
        let coin = job.work.coin.clone();
        let (outcome, block) =
            validate::validate(job, &self.extranonce1, &submit, &payout.address, now_unix);
        let (share_difficulty, rejected) = match &outcome {
            ShareOutcome::Accepted { difficulty } | ShareOutcome::Block { difficulty, .. } => {
                (*difficulty, None)
            }
            ShareOutcome::Rejected(reason) => (0.0, Some(*reason)),
        };
        fx.events.push(PoolEvent::Share {
            session: self.id,
            worker: submit.worker.clone(),
            coin,
            job_difficulty,
            share_difficulty,
            rejected,
        });
        match outcome {
            ShareOutcome::Rejected(reason) => {
                fx.respond(Response::err(
                    id,
                    StratumError {
                        code: reason.stratum_code(),
                        message: reason.message().into(),
                    },
                ));
            }
            _ => {
                fx.respond(Response::ok(id, json!(true)));
                self.vardiff.on_share();
                fx.block = block;
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
        if let Some(new) = self.vardiff.evaluate(now, self.difficulty) {
            self.set_difficulty(new, fx);
            self.resend_current_job(fx);
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

    /// A new template arrived.
    pub fn on_work(&mut self, work: Arc<WorkTemplate>) -> Effects {
        let mut fx = Effects::default();
        let clean = work.clean_jobs;
        self.work = Some(work);
        if clean {
            for job in &mut self.jobs {
                job.stale = true;
            }
        }
        if self.payout.is_some() {
            self.send_job(clean, &mut fx);
        }
        fx
    }

    fn resend_current_job(&mut self, fx: &mut Effects) {
        if self.work.is_some() && self.payout.is_some() {
            self.send_job(false, fx);
        }
    }

    fn send_job(&mut self, clean: bool, fx: &mut Effects) {
        let (Some(work), Some(payout)) = (self.work.clone(), self.payout.as_ref()) else {
            return;
        };
        let coinbase =
            match CoinbaseParts::build(&work, &payout.script, EXTRANONCE1_LEN + EXTRANONCE2_LEN) {
                Ok(c) => c,
                Err(err) => {
                    tracing::error!(session = self.id, %err, "cannot build coinbase");
                    fx.close = true;
                    return;
                }
            };
        let job = SessionJob {
            id: JobId(self.next_job),
            work,
            coinbase,
            difficulty: self.difficulty,
            target: Target::from_difficulty(self.difficulty),
            stale: false,
            seen: Default::default(),
        };
        self.next_job += 1;
        fx.notify(Notification::new("mining.notify", job.notify_params(clean)));
        self.jobs.push_back(job);
        while self.jobs.len() > MAX_JOBS {
            self.jobs.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::job::JobId;
    use alamo_core::Algorithm;

    struct FixedResolver;
    impl PayoutResolver for FixedResolver {
        fn resolve(&self, username: &str) -> Payout {
            let (address, worker) = alamo_core::payout::split_username(username);
            Payout {
                address: address.into(),
                script: address.as_bytes().to_vec(),
                worker: worker.into(),
                fallback: false,
            }
        }
    }

    fn work(clean: bool) -> Arc<WorkTemplate> {
        let mut w = WorkTemplate {
            id: JobId(1),
            coin: "LTC".into(),
            algorithm: Algorithm::Scrypt,
            height: 5,
            version: 0x2000_0000,
            prev_hash: [0; 32],
            bits: 0x207f_ffff,
            target: Target::from_compact(0x207f_ffff),
            cur_time: 1_700_000_000,
            min_time: 1_699_990_000,
            coinbase_value: 1,
            coinbase_script_prefix: vec![0x55],
            witness_commitment: None,
            transactions: vec![],
            merkle_branch: vec![],
            extra_payload: vec![],
            clean_jobs: clean,
            created_at: 0,
        };
        w.compute_merkle_branch();
        Arc::new(w)
    }

    fn session() -> Session {
        Session::new(
            9,
            [1, 2, 3, 4],
            VardiffConfig::default(),
            Arc::new(FixedResolver),
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
            req("mining.authorize", json!(["addr.rig", "x"])),
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
    fn second_address_on_same_connection_is_refused() {
        let mut s = session();
        s.handle(
            req("mining.authorize", json!(["addr1", "x"])),
            Instant::now(),
            0,
        );
        let fx = s.handle(
            req("mining.authorize", json!(["addr2", "x"])),
            Instant::now(),
            0,
        );
        assert!(matches!(&fx.outgoing[0], Outgoing::Response(r) if r.error.is_some()));
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
        s.handle(
            req("mining.authorize", json!(["addr", "x"])),
            Instant::now(),
            0,
        );
        s.on_work(work(true));
        let fx = s.handle(
            req(
                "mining.submit",
                json!(["addr", "ff", "00000001", "65500000", "00000000"]),
            ),
            Instant::now(),
            1_700_000_000,
        );
        let Outgoing::Response(r) = &fx.outgoing[0] else {
            panic!()
        };
        assert_eq!(r.error.as_ref().unwrap().code, 21);
    }
}
