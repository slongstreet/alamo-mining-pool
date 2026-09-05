//! Protocol state for one miner connection.

use crate::config::VardiffConfig;
use crate::events::{BlockCandidate, PoolEvent};
use crate::job::{SessionJob, EXTRANONCE1_LEN, EXTRANONCE2_LEN};
use crate::protocol::{Notification, Request, Response, StratumError};
use crate::validate::{self, Submit};
use crate::vardiff::Vardiff;
use alamo_core::coinbase::CoinbaseParts;
use alamo_core::job::{JobId, RejectReason, ShareOutcome};
use alamo_core::payout::{Payout, PayoutTable};
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
    payout: Option<Payout>,
    workers: Vec<String>,
    difficulty: f64,
    next_job: u64,
    jobs: VecDeque<SessionJob>,
    work: Option<Arc<WorkTemplate>>,
    vardiff: Vardiff,
    payouts: Arc<PayoutTable>,
}

impl Session {
    /// Create a session with a fresh extranonce1.
    pub fn new(
        id: u64,
        extranonce1: [u8; EXTRANONCE1_LEN],
        vardiff_cfg: VardiffConfig,
        payouts: Arc<PayoutTable>,
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
        }
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
            "mining.configure" => {
                // No extensions (version rolling is not used by scrypt miners).
                fx.respond(Response::ok(id, json!({ "version-rolling": false })));
            }
            "mining.extranonce.subscribe" => fx.respond(Response::ok(id, json!(true))),
            "mining.suggest_difficulty" => {
                if let Some(d) = params.first().and_then(Value::as_f64).filter(|d| *d > 0.0) {
                    let d = self.vardiff.clamp(d);
                    if (d - self.difficulty).abs() > f64::EPSILON {
                        self.set_difficulty(d, &mut fx);
                        self.resend_current_job(&mut fx);
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
        let payout = self.payouts.resolve(username);
        if let Some(existing) = &self.payout {
            if existing.script != payout.script {
                fx.respond(Response::err(
                    id,
                    StratumError::other("This connection already pays a different address"),
                ));
                return;
            }
        }
        if payout.fallback {
            tracing::warn!(session = self.id, username, fallback = %payout.address, "username is not a valid address; paying fallback");
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
            self.resend_current_job(fx);
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
        let (Ok(extranonce2), Ok(ntime), Ok(nonce)) = (
            hex::decode(en2),
            u32::from_str_radix(ntime, 16),
            u32::from_str_radix(nonce, 16),
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
        };
        let job_difficulty = job.difficulty;
        let coin = job.work.coin;
        let (outcome, block) =
            validate::validate(job, &self.extranonce1, &submit, &payout.address, now_unix);
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
        self.send_job(clean, &mut fx);
        fx
    }

    fn resend_current_job(&mut self, fx: &mut Effects) {
        self.send_job(false, fx);
    }

    /// Send the current template as a new job, if there is a template and a payout.
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
    use alamo_core::address::{encode_segwit, AddressParams};

    const PARAMS: AddressParams = AddressParams {
        p2pkh_prefix: 111,
        p2sh_prefixes: &[58, 196],
        bech32_hrp: Some("rltc"),
    };

    fn addr(byte: u8) -> String {
        encode_segwit("rltc", 0, &[byte; 20]).unwrap()
    }

    fn work(clean: bool) -> Arc<WorkTemplate> {
        let mut w = WorkTemplate::regtest_sample(5, None);
        w.clean_jobs = clean;
        Arc::new(w)
    }

    fn session() -> Session {
        let payouts = Arc::new(PayoutTable::new(PARAMS, &addr(9)).unwrap());
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
            matches!(&fx.events[0], PoolEvent::Authorized { fallback: true, address, .. } if *address == addr(9))
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
}
