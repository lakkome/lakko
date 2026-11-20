// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-canary run`: the loop, and one run's steps.
//!
//! A run signs both accounts in, then has A send a message B decrypts, an
//! attachment B downloads, and (every `call_every`-th run, once both
//! accounts are old enough) a call whose SFrame-sealed audio reaches B. Each
//! step records its outcome; a failing step logs one WARN line with the step
//! name and a short error class and never names an account, group, blob,
//! room or peer. Without sessions every later step is recorded as failed.
//!
//! State discipline: A's MLS snapshot is written after the message is
//! sealed and before it is published (so a crash can never make A seal two
//! messages at one ratchet generation), and B's snapshot and journal cursor
//! are written together after B processed a batch. Storage passes are
//! written when bought.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lakko_anon_token::TokenContext;
use rand::RngCore as _;
use tracing::{info, warn};

use crate::account::Account;
use crate::api::{now_secs, Api, Session, LONG_POLL_WAIT_SECS};
use crate::call::{self, CallInputs};
use crate::endpoints::ds_group_path;
use crate::error::{Error, Result};
use crate::metrics::{self, Outcome, Step};
use crate::schedule::{call_plan, CallPlan};
use crate::state::{Pass, State};

/// How long B waits for the message.
const MESSAGE_DEADLINE: Duration = Duration::from_secs(60);
/// Bytes of the attachment.
const ATTACHMENT_LEN: usize = 1024;

/// Options of `run`.
pub(crate) struct RunOptions {
    /// The state file.
    pub(crate) state_path: PathBuf,
    /// Seconds between run starts.
    pub(crate) interval: Duration,
    /// The call step runs every this many runs (0 = never).
    pub(crate) call_every: u64,
    /// One run, then exit with its result.
    pub(crate) once: bool,
    /// Ignore the young-account rule (local testing only).
    pub(crate) ignore_account_age: bool,
}

struct Ctx<'a> {
    api: &'a Api,
    state: State,
    path: &'a Path,
    a: Account,
    b: Account,
    group_id: [u8; 32],
}

impl Ctx<'_> {
    fn save(&self) -> Result<()> {
        self.state.save(self.path)
    }
}

/// Run the loop. Returns `Ok(true)` when the last run (the only one with
/// `--once`) had no failed step.
///
/// # Errors
///
/// A state file that cannot be loaded, or whose service bases fail
/// [`crate::endpoints::Endpoints::check`].
pub(crate) async fn run(opts: RunOptions) -> Result<bool> {
    let state = State::load(&opts.state_path)?;
    // The same scheme rule as init: a hand-edited state file must not
    // send credentials in the clear either.
    state.endpoints.check()?;
    let api = Api::new(state.endpoints.clone())?;
    drop(state);
    loop {
        let started = Instant::now();
        let ok = match Box::pin(run_once(&api, &opts)).await {
            Ok(ok) => ok,
            Err(e) => {
                warn!(class = %e.class(), "canary run could not start");
                false
            },
        };
        metrics::run_finished(now_secs());
        if opts.once {
            return Ok(ok);
        }
        let next = started + opts.interval;
        tokio::select! {
            () = tokio::time::sleep_until(next.into()) => {},
            () = shutdown() => {
                info!("canary stopping");
                return Ok(ok);
            }
        }
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = term.recv() => {},
                _ = tokio::signal::ctrl_c() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// One run. `Ok(true)` when no step failed.
async fn run_once(api: &Api, opts: &RunOptions) -> Result<bool> {
    let state = State::load(&opts.state_path)?;
    let group_id = state.group_id()?;
    let a = Account::load(&state.a, &group_id)?;
    let b = Account::load(&state.b, &group_id)?;
    let mut ctx = Ctx {
        api,
        state,
        path: &opts.state_path,
        a,
        b,
        group_id,
    };
    let youngest = ctx.state.a.created_at_unix.max(ctx.state.b.created_at_unix);
    let plan = call_plan(
        ctx.state.runs,
        opts.call_every,
        youngest,
        now_secs(),
        opts.ignore_account_age,
    );
    let mut all_ok = true;

    let started = Instant::now();
    let sessions = sign_in(&ctx).await;
    let sessions = match sessions {
        Ok(s) => {
            finish(&mut ctx, Step::SignIn, Ok(started.elapsed()));
            Some(s)
        },
        Err(e) => {
            all_ok = false;
            finish(&mut ctx, Step::SignIn, Err(e));
            None
        },
    };

    let Some((sa, sb)) = sessions else {
        for step in [Step::Message, Step::Attachment] {
            finish(&mut ctx, step, Err(Error::Protocol("no-session")));
        }
        match plan {
            CallPlan::Run => finish(&mut ctx, Step::Call, Err(Error::Protocol("no-session"))),
            CallPlan::SkipYoung => metrics::record(Step::Call, Outcome::Skipped, 0),
            CallPlan::NotDue => {},
        }
        end_run(&mut ctx);
        return Ok(false);
    };

    let r = message_step(&mut ctx).await;
    all_ok &= r.is_ok();
    finish(&mut ctx, Step::Message, r);

    let started = Instant::now();
    let r = attachment_step(&mut ctx, &sa, &sb)
        .await
        .map(|()| started.elapsed());
    all_ok &= r.is_ok();
    finish(&mut ctx, Step::Attachment, r);

    match plan {
        CallPlan::Run => {
            let r = Box::pin(call_step(&ctx, &sa, &sb)).await;
            all_ok &= r.is_ok();
            finish(&mut ctx, Step::Call, r);
        },
        CallPlan::SkipYoung => metrics::record(Step::Call, Outcome::Skipped, 0),
        CallPlan::NotDue => {},
    }
    end_run(&mut ctx);
    Ok(all_ok)
}

fn end_run(ctx: &mut Ctx<'_>) {
    ctx.state.runs = ctx.state.runs.saturating_add(1);
    if let Err(e) = ctx.save() {
        warn!(class = %e.class(), "canary state not saved");
    }
}

/// Record a step's result and update its persisted failure counter.
fn finish(ctx: &mut Ctx<'_>, step: Step, result: Result<Duration>) {
    let counter = ctx
        .state
        .counters
        .consecutive_failures
        .entry(step.name().to_owned())
        .or_insert(0);
    match result {
        Ok(d) => {
            *counter = 0;
            metrics::record(step, Outcome::Ok(d.as_secs_f64()), 0);
            info!(
                step = step.name(),
                seconds = d.as_secs_f64(),
                "canary step ok"
            );
        },
        Err(e) => {
            *counter = counter.saturating_add(1);
            metrics::record(step, Outcome::Failed, *counter);
            warn!(step = step.name(), class = %e.class(), "canary step failed");
        },
    }
}

async fn sign_in(ctx: &Ctx<'_>) -> Result<(Session, Session)> {
    let handle = |name: &str| {
        lakko_contacts::username::login_handle(name).map_err(|_| Error::State("username".into()))
    };
    let sa = ctx
        .api
        .login(
            &handle(&ctx.state.a.username)?,
            ctx.state.a.password.as_bytes(),
        )
        .await?;
    let sb = ctx
        .api
        .login(
            &handle(&ctx.state.b.username)?,
            ctx.state.b.password.as_bytes(),
        )
        .await?;
    Ok((sa, sb))
}

/// A seals and publishes a nonce; B reads the journal from its cursor,
/// processes every row in order and finds it. Returns publish-to-decrypt.
async fn message_step(ctx: &mut Ctx<'_>) -> Result<Duration> {
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    let text = format!("lakko-canary {}", hex::encode(nonce));
    let path = ds_group_path(&ctx.group_id, "messages");

    let msg = ctx
        .a
        .group
        .create_application_message(&ctx.a.device, text.as_bytes())
        .map_err(|_| Error::Mls("seal"))?;
    let wire = lakko_mls::group::encode_message(&msg).map_err(|_| Error::Mls("encode"))?;
    // Persist the advanced ratchet before the message can leave.
    ctx.state.a.mls_snapshot_b64 = ctx.a.snapshot_b64()?;
    ctx.save()?;

    let signer_a = ctx.a.ds_signer()?;
    let started = Instant::now();
    ctx.api.ds_publish(&signer_a, &path, wire).await?;

    let deadline = started + MESSAGE_DEADLINE;
    let signer_b = ctx.b.ds_signer()?;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Timeout("message"));
        }
        let wait = u32::try_from(left.as_secs())
            .unwrap_or(u32::MAX)
            .min(LONG_POLL_WAIT_SECS);
        let page = ctx
            .api
            .ds_fetch(&signer_b, &path, ctx.state.b_cursor, wait)
            .await?;
        let mut found = false;
        for row in &page.messages {
            if row.id <= ctx.state.b_cursor {
                continue;
            }
            if let Ok(bytes) = crate::state::unb64(&row.payload_b64) {
                if let Ok(lakko_mls::group::Incoming::Application { bytes, .. }) =
                    ctx.b.group.process_incoming(&ctx.b.device, &bytes)
                {
                    if bytes == text.as_bytes() {
                        found = true;
                    }
                }
            }
            ctx.state.b_cursor = row.id;
        }
        if !page.messages.is_empty() {
            ctx.state.b.mls_snapshot_b64 = ctx.b.snapshot_b64()?;
            ctx.save()?;
        }
        if found {
            return Ok(started.elapsed());
        }
    }
}

/// A pass from the state when it has time left, else a fresh one bought
/// with one token.
async fn pass_for(
    api: &Api,
    session: &Session,
    held: &mut Option<Pass>,
    kind: &'static str,
    context: TokenContext,
) -> Result<(String, bool)> {
    if let Some(p) = held.as_ref().filter(|p| p.usable_at(now_secs())) {
        return Ok((p.value.clone(), false));
    }
    let token = api.anon_token(session, context).await?;
    let pass = api.blob_pass(kind, &token).await?;
    let value = pass.value.clone();
    *held = Some(pass);
    Ok((value, true))
}

/// Whether a storage refusal means the pass itself is spent or unknown.
const fn pass_refused(e: &Error) -> bool {
    matches!(
        e,
        Error::Http {
            status: 401 | 429,
            ..
        }
    )
}

/// Upload with A's pass, download with B's, compare, delete.
async fn attachment_step(ctx: &mut Ctx<'_>, sa: &Session, sb: &Session) -> Result<()> {
    let mut plain = vec![0u8; ATTACHMENT_LEN];
    rand::thread_rng().fill_bytes(&mut plain);
    let enc = lakko_attachments::encrypt_attachment(&plain).map_err(|_| Error::Crypto("seal"))?;
    let vault = lakko_vault::VaultRoot::from_account_seed(&ctx.a.identity.ed25519_seed_bytes());
    let delete_seed = *vault.blob_delete_seed(&enc.blob_id).as_bytes();
    let delete_pk = lakko_vault::proof::public_key(&delete_seed);

    // Upload, renewing the pass once if the service refuses it.
    let mut attempt = 0;
    loop {
        let (pass, bought) = pass_for(
            ctx.api,
            sa,
            &mut ctx.state.a.upload_pass,
            "upload",
            TokenContext::BlobUpload,
        )
        .await?;
        if bought {
            ctx.save()?;
        }
        match ctx
            .api
            .blob_put(&pass, &enc.blob_id, &delete_pk, enc.ciphertext.clone())
            .await
        {
            Ok(()) => break,
            Err(e) if attempt == 0 && pass_refused(&e) => {
                ctx.state.a.upload_pass = None;
                attempt += 1;
            },
            Err(e) => return Err(e),
        }
    }

    let fetched = download(ctx, sb, &enc.blob_id).await;
    // Delete whatever happened to the download.
    let deleted = ctx.api.blob_delete(&delete_seed, &enc.blob_id).await;
    let ciphertext = fetched?;
    let opened = lakko_attachments::decrypt_attachment(&ciphertext, &enc.key, &enc.nonce)
        .map_err(|_| Error::Crypto("open"))?;
    if opened != plain {
        return Err(Error::Mismatch("attachment"));
    }
    deleted
}

async fn download(ctx: &mut Ctx<'_>, sb: &Session, blob_id: &[u8; 32]) -> Result<Vec<u8>> {
    let mut attempt = 0;
    loop {
        let (pass, bought) = pass_for(
            ctx.api,
            sb,
            &mut ctx.state.b.read_pass,
            "read",
            TokenContext::BlobRead,
        )
        .await?;
        if bought {
            ctx.save()?;
        }
        match ctx.api.blob_get(&pass, blob_id).await {
            Ok(bytes) => return Ok(bytes),
            Err(e) if attempt == 0 && pass_refused(&e) => {
                ctx.state.b.read_pass = None;
                attempt += 1;
            },
            Err(e) => return Err(e),
        }
    }
}

/// Both accounts join a fresh room; A's sealed audio must reach B.
async fn call_step(ctx: &Ctx<'_>, sa: &Session, sb: &Session) -> Result<Duration> {
    let call_id = call::fresh_call_id();
    let room = lakko_sfu_proto::room_hex(&call_id);
    let token_a = ctx.api.anon_token(sa, TokenContext::CallJoin).await?;
    let jwt_a = ctx.api.call_jwt(&token_a, &room).await?;
    let token_b = ctx.api.anon_token(sb, TokenContext::CallJoin).await?;
    let jwt_b = ctx.api.call_jwt(&token_b, &room).await?;
    let inputs = CallInputs {
        connect_url: ctx.api.endpoints.sfu_connect_url(),
        jwt_a,
        jwt_b,
        seed_a: call::call_seed(&ctx.a.group, &ctx.a.device, &call_id)?,
        seed_b: call::call_seed(&ctx.b.group, &ctx.b.device, &call_id)?,
        leaf_a: ctx.a.group.own_leaf_index().u32(),
    };
    let result = Box::pin(call::run(inputs)).await?;
    info!(frames = result.matched, "canary call frames opened");
    Ok(result.first_frame_latency)
}
