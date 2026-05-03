// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! OPAQUE-PAKE (RFC draft `opaque`) auth flows для auth service.
//!
//! Сервер никогда не видит plaintext password — клиент и сервер
//! обмениваются OPRF-blinded сообщениями, в результате на сервере остаётся
//! "envelope" из которого нельзя выкачать пароль без MITM на момент
//! регистрации (защищено OPAQUE-математикой).
//!
//! Phase 1.1b: registration (`/signup/start` + `/signup/finish`) — server
//! stateless между start и finish.
//!
//! Phase 1.1c: login (`/login/start` + `/login/finish`) — server **statefull**
//! между start и finish. `ServerLogin<CS>` сериализуется и хранится в
//! `opaque_login_states` таблице под session_id (UUIDv7) с TTL 2 минуты.
//! Background pruner периодически удаляет просроченные записи.
//! DB-backed (не in-memory) чтобы пережить рестарт сервиса и работать
//! корректно при horizontal scale-апе.
//!
//! Серверный static-state (OPRF + AKE keypairs) генерится один раз на
//! первом старте и persisted в `opaque_server_setup` таблице. Per-account
//! registration material — в `opaque_registrations`, key-ed по
//! `account_id` (UUIDv7 raw bytes), с FK на `accounts(id)`.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use opaque_ke::key_exchange::tripledh::TripleDh;
use opaque_ke::{
    CipherSuite, CredentialFinalization, CredentialRequest, RegistrationRequest,
    RegistrationUpload, Ristretto255, ServerLogin, ServerLoginStartParameters, ServerRegistration,
    ServerSetup,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;
use uuid::Uuid;

/// Lakko OPAQUE-PAKE ciphersuite.
///
/// - **OPRF**: Ristretto255 (constant-time, software, no hardware-deps)
/// - **KE group**: Ristretto255 (same — TripleDH)
/// - **KE**: TripleDH (3-message authenticated key exchange)
/// - **KSF**: Argon2id (key-stretching на client side; защищает password
///   против brute-force даже если server-state утёк)
pub(crate) struct LakkoCipherSuite;

impl CipherSuite for LakkoCipherSuite {
    type OprfCs = Ristretto255;
    type KeGroup = Ristretto255;
    type KeyExchange = TripleDh;
    type Ksf = argon2::Argon2<'static>;
}

/// Длина account_id в байтах (UUIDv7 raw form).
const ACCOUNT_ID_LEN: usize = 16;

/// Загрузить server-static OPAQUE setup из DB; на первом старте сгенерить
/// и persisted-сохранить.
///
/// Singleton-row pattern — таблица `opaque_server_setup` имеет CHECK(id=1)
/// constraint, физически не может содержать больше одной записи.
///
/// # Errors
///
/// Любая DB-ошибка или OPAQUE-deserialization failure пробрасывается
/// наверх через `anyhow::Error`. На критических путях fail-fast — service
/// не должен стартовать с broken setup.
pub(crate) async fn load_or_init_server_setup(
    pool: &MySqlPool,
) -> Result<ServerSetup<LakkoCipherSuite>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT setup_bytes FROM opaque_server_setup WHERE id = 1")
            .fetch_optional(pool)
            .await
            .context("query opaque_server_setup")?;

    if let Some((bytes,)) = row {
        let setup = ServerSetup::<LakkoCipherSuite>::deserialize(&bytes)
            .map_err(|e| anyhow!("deserialize ServerSetup from DB: {e:?}"))?;
        tracing::info!("loaded existing OPAQUE ServerSetup from opaque_server_setup");
        return Ok(setup);
    }

    let setup = ServerSetup::<LakkoCipherSuite>::new(&mut OsRng);
    let bytes = setup.serialize().to_vec();
    sqlx::query("INSERT INTO opaque_server_setup (id, setup_bytes) VALUES (1, ?)")
        .bind(&bytes)
        .execute(pool)
        .await
        .context("insert fresh ServerSetup")?;
    tracing::info!("generated and persisted fresh OPAQUE ServerSetup (first run)");
    Ok(setup)
}

/// Декодировать base64 `account_id` и валидировать длину.
fn decode_account_id(b64: &str) -> Result<Vec<u8>, (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid account_id base64: {e}"),
        )
    })?;
    if bytes.len() != ACCOUNT_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("account_id must be {ACCOUNT_ID_LEN} bytes after base64-decode"),
        ));
    }
    Ok(bytes)
}

// ============================================================================
// /signup/start
// ============================================================================

/// Запрос на начало регистрации. Клиент уже сгенерил `account_id`
/// (typically UUIDv7) и `RegistrationRequest` через `ClientRegistration::start`.
#[derive(Deserialize)]
pub(crate) struct SignupStartRequest {
    /// Base64-encoded raw bytes of `account_id` (16 bytes UUIDv7).
    pub(crate) account_id: String,
    /// Base64-encoded `RegistrationRequest` from `opaque-ke` client.
    pub(crate) request: String,
}

/// Ответ на `/signup/start` — `RegistrationResponse` для передачи в
/// `ClientRegistration::finish`.
#[derive(Serialize)]
pub(crate) struct SignupStartResponse {
    /// Base64-encoded `RegistrationResponse` от opaque-ke сервера.
    pub(crate) response: String,
}

/// `POST /signup/start` — первый шаг регистрации.
///
/// Server берёт client-у `RegistrationRequest`, прогоняет через
/// `ServerRegistration::start` со server-static setup-ом и
/// credential_id=account_id, возвращает `RegistrationResponse`.
///
/// **Server остаётся stateless** между start и finish — OPAQUE-математика
/// биндит upload (приходит в /signup/finish) к response через сам
/// account_id и password, server-state не нужен.
pub(crate) async fn signup_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<SignupStartRequest>,
) -> Result<Json<SignupStartResponse>, (StatusCode, String)> {
    let account_id_bytes = decode_account_id(&payload.account_id)?;

    let request_bytes = B64.decode(&payload.request).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid request base64: {e}"),
        )
    })?;
    let request_message = RegistrationRequest::<LakkoCipherSuite>::deserialize(&request_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid RegistrationRequest: {e:?}"),
            )
        })?;

    let server_start = ServerRegistration::<LakkoCipherSuite>::start(
        &state.opaque_setup,
        request_message,
        &account_id_bytes,
    )
    .map_err(|e| {
        tracing::error!(error = ?e, "ServerRegistration::start failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal opaque error".to_string(),
        )
    })?;

    Ok(Json(SignupStartResponse {
        response: B64.encode(server_start.message.serialize()),
    }))
}

// ============================================================================
// /signup/finish
// ============================================================================

/// Запрос на финализацию регистрации. Клиент завершил `ClientRegistration`
/// математикой, получил `RegistrationUpload` — отправляет нам.
#[derive(Deserialize)]
pub(crate) struct SignupFinishRequest {
    pub(crate) account_id: String,
    /// Base64-encoded `RegistrationUpload` (envelope + masking_key + client AKE pubkey).
    pub(crate) upload: String,
}

#[derive(Serialize)]
pub(crate) struct SignupFinishResponse {
    pub(crate) ok: bool,
}

/// `POST /signup/finish` — финализация регистрации.
///
/// В одной DB-транзакции: создать `accounts(id=account_id)` row + сохранить
/// сериализованный `ServerRegistration` в `opaque_registrations`. Если
/// account_id уже зарегистрирован — `409 Conflict`.
pub(crate) async fn signup_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<SignupFinishRequest>,
) -> Result<Json<SignupFinishResponse>, (StatusCode, String)> {
    let account_id_bytes = decode_account_id(&payload.account_id)?;

    let upload_bytes = B64.decode(&payload.upload).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid upload base64: {e}"),
        )
    })?;
    let upload_message = RegistrationUpload::<LakkoCipherSuite>::deserialize(&upload_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid RegistrationUpload: {e:?}"),
            )
        })?;

    let registration = ServerRegistration::<LakkoCipherSuite>::finish(upload_message);
    let registration_bytes = registration.serialize().to_vec();

    let mut tx = state.db.begin().await.map_err(|e| {
        tracing::error!(error = %e, "begin tx");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    // Создать account row. Дубликат по PK = account_id уже занят.
    let account_insert = sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&account_id_bytes)
        .execute(&mut *tx)
        .await;

    match account_insert {
        Ok(_) => {},
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            return Err((
                StatusCode::CONFLICT,
                "account_id already registered".to_string(),
            ));
        },
        Err(e) => {
            tracing::error!(error = %e, "insert account");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            ));
        },
    }

    sqlx::query("INSERT INTO opaque_registrations (account_id, registration_bytes) VALUES (?, ?)")
        .bind(&account_id_bytes)
        .bind(&registration_bytes)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "insert opaque_registrations");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    tx.commit().await.map_err(|e| {
        tracing::error!(error = %e, "commit tx");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    tracing::info!(account_id = %B64.encode(&account_id_bytes), "signup completed");
    Ok(Json(SignupFinishResponse { ok: true }))
}

// ============================================================================
// Login state — DB-backed
// ============================================================================

/// TTL для in-flight login state. Клиент должен завершить /login/finish в
/// течение этого окна. Защита от утечек памяти + replay-атак.
/// `i64` чтобы напрямую пробрасывать в SQL `INTERVAL ? SECOND` без cast.
const LOGIN_STATE_TTL_SECS: i64 = 120;

/// Период между прогонами pruner-а — раз в 30 секунд DELETE просроченные.
const LOGIN_STATE_PRUNE_INTERVAL: Duration = Duration::from_secs(30);

/// Положить ServerLogin state в `opaque_login_states`, вернуть свежий
/// session_id (UUIDv7).
async fn insert_login_state(
    pool: &MySqlPool,
    state: &ServerLogin<LakkoCipherSuite>,
    account_id: &[u8; ACCOUNT_ID_LEN],
) -> Result<Uuid, sqlx::Error> {
    let session_id = Uuid::now_v7();
    let state_bytes = state.serialize().to_vec();
    sqlx::query(
        "INSERT INTO opaque_login_states (session_id, account_id, state_bytes) \
         VALUES (?, ?, ?)",
    )
    .bind(session_id.as_bytes().as_slice())
    .bind(&account_id[..])
    .bind(&state_bytes)
    .execute(pool)
    .await?;
    Ok(session_id)
}

/// Достать ServerLogin state по session_id и **сразу удалить запись**
/// (one-shot — повторный /login/finish с тем же session_id даст 401).
/// Возвращает `None` если session_id неизвестен или просрочен (TTL истёк).
async fn take_login_state(
    pool: &MySqlPool,
    session_id: Uuid,
) -> Result<Option<(ServerLogin<LakkoCipherSuite>, [u8; ACCOUNT_ID_LEN])>, sqlx::Error> {
    // Атомарно: достать запись (с проверкой TTL) и удалить. В MySQL/MariaDB
    // нет нативного DELETE ... RETURNING, поэтому делаем в транзакции:
    //   1. SELECT ... FOR UPDATE
    //   2. DELETE
    let mut tx = pool.begin().await?;

    let row: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT state_bytes, account_id FROM opaque_login_states \
         WHERE session_id = ? \
           AND created_at >= NOW(3) - INTERVAL ? SECOND \
         FOR UPDATE",
    )
    .bind(session_id.as_bytes().as_slice())
    .bind(LOGIN_STATE_TTL_SECS)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((state_bytes, account_id_vec)) = row else {
        // Нет записи (или просрочена) — но всё равно делаем DELETE на случай
        // если запись есть, но просрочена: pruner уберёт, мы тоже уберём.
        sqlx::query("DELETE FROM opaque_login_states WHERE session_id = ?")
            .bind(session_id.as_bytes().as_slice())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    };

    sqlx::query("DELETE FROM opaque_login_states WHERE session_id = ?")
        .bind(session_id.as_bytes().as_slice())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let server_login = ServerLogin::<LakkoCipherSuite>::deserialize(&state_bytes)
        .expect("ServerLogin deserialize failed — DB row corrupted");
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    if account_id_vec.len() != ACCOUNT_ID_LEN {
        return Ok(None);
    }
    acct.copy_from_slice(&account_id_vec);
    Ok(Some((server_login, acct)))
}

/// Запустить background-task который раз в `LOGIN_STATE_PRUNE_INTERVAL`
/// удаляет просроченные login-state-ы из DB.
pub(crate) fn spawn_login_state_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(LOGIN_STATE_PRUNE_INTERVAL);
        // Первый tick срабатывает сразу — пропускаем (на старте таблица
        // пустая или почти пустая).
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = sqlx::query(
                "DELETE FROM opaque_login_states \
                 WHERE created_at < NOW(3) - INTERVAL ? SECOND",
            )
            .bind(LOGIN_STATE_TTL_SECS)
            .execute(&pool)
            .await;
            match result {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(
                        rows = r.rows_affected(),
                        "pruned expired opaque_login_states"
                    );
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "opaque_login_states prune failed"),
            }
        }
    })
}

// ============================================================================
// /login/start
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct LoginStartRequest {
    pub(crate) account_id: String,
    /// Base64-encoded `CredentialRequest` от `ClientLogin::start`.
    pub(crate) request: String,
}

#[derive(Serialize)]
pub(crate) struct LoginStartResponse {
    /// Session ID для `/login/finish` (UUIDv7, время + случайные биты).
    pub(crate) session_id: String,
    /// Base64-encoded `CredentialResponse` от `ServerLogin::start`.
    pub(crate) response: String,
}

/// `POST /login/start` — первый шаг login.
///
/// Server берёт `CredentialRequest`, ищет `ServerRegistration` по account_id
/// в `opaque_registrations`, прогоняет через `ServerLogin::start`,
/// возвращает `CredentialResponse` + session_id. State кэшируется in-memory.
///
/// Если account_id неизвестен — возвращаем 401 Unauthorized без раскрытия
/// "это пользователь существует, но пароль не подошёл" vs "пользователь
/// не существует". OPAQUE сам по себе одинаково отвечает в обоих случаях
/// через "fake credentials" path, но мы добавляем early-return до того как
/// дойдём до реальной криптографии — это нормально, потому что timing-канал
/// здесь не помогает атакующему (он всё равно не может различить).
pub(crate) async fn login_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginStartRequest>,
) -> Result<Json<LoginStartResponse>, (StatusCode, String)> {
    let account_id_bytes = decode_account_id(&payload.account_id)?;

    let request_bytes = B64.decode(&payload.request).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid request base64: {e}"),
        )
    })?;
    let request_message = CredentialRequest::<LakkoCipherSuite>::deserialize(&request_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid CredentialRequest: {e:?}"),
            )
        })?;

    // Достать сохранённый ServerRegistration. Если row нет — fake credentials
    // path: opaque-ke 3.0 ServerLogin::start с password_file = None даёт
    // криптографически правдоподобный ответ (нельзя отличить от валидного
    // login для существующего user-а с неправильным паролем).
    let stored: Option<ServerRegistration<LakkoCipherSuite>> = sqlx::query_as::<_, (Vec<u8>,)>(
        "SELECT registration_bytes FROM opaque_registrations WHERE account_id = ?",
    )
    .bind(&account_id_bytes[..])
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "query opaque_registrations");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?
    .map(|(bytes,)| {
        ServerRegistration::<LakkoCipherSuite>::deserialize(&bytes)
            .expect("ServerRegistration deserialize failed — DB row corrupted")
    });

    let mut rng = OsRng;
    let server_start = ServerLogin::<LakkoCipherSuite>::start(
        &mut rng,
        &state.opaque_setup,
        stored,
        request_message,
        &account_id_bytes,
        ServerLoginStartParameters::default(),
    )
    .map_err(|e| {
        tracing::error!(error = ?e, "ServerLogin::start failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal opaque error".to_string(),
        )
    })?;

    // Конвертируем Vec<u8> в [u8; 16] для DB (validated to be 16 above).
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    acct.copy_from_slice(&account_id_bytes);

    let session_id = insert_login_state(&state.db, &server_start.state, &acct)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "insert opaque_login_states");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    Ok(Json(LoginStartResponse {
        session_id: session_id.to_string(),
        response: B64.encode(server_start.message.serialize()),
    }))
}

// ============================================================================
// /login/finish
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct LoginFinishRequest {
    pub(crate) session_id: String,
    /// Base64-encoded `CredentialFinalization` от `ClientLogin::finish`.
    pub(crate) finalize: String,
}

#[derive(Serialize)]
pub(crate) struct LoginFinishResponse {
    pub(crate) ok: bool,
    /// Base64-encoded account_id (16 raw UUIDv7 bytes) — клиент пинит для
    /// последующих запросов. Реальный session token (JWT / DB-backed
    /// session) — Phase 1.1e+.
    pub(crate) account_id: String,
}

/// `POST /login/finish` — финализация login.
///
/// Берёт `CredentialFinalization` от клиента, прогоняет через
/// `ServerLogin::finish`. Если математика сходится — пароль верный, server
/// и client получили matching session_key. Возвращаем account_id; реальный
/// session token (JWT с подписью / row в `sessions` таблице) — отдельный
/// под-sprint после anti-spam.
///
/// Если session_id неизвестен или просрочен (TTL 2 минуты) — 401.
/// Если CredentialFinalization не валидируется — 401 (неверный пароль).
pub(crate) async fn login_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginFinishRequest>,
) -> Result<Json<LoginFinishResponse>, (StatusCode, String)> {
    let session_id = Uuid::parse_str(&payload.session_id).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid session_id (not a UUID): {e}"),
        )
    })?;

    let cached = take_login_state(&state.db, session_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "take opaque_login_states");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "session_id unknown or expired".to_string(),
        ))?;

    let (server_login, account_id) = cached;

    let finalize_bytes = B64.decode(&payload.finalize).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid finalize base64: {e}"),
        )
    })?;
    let finalize_message = CredentialFinalization::<LakkoCipherSuite>::deserialize(&finalize_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid CredentialFinalization: {e:?}"),
            )
        })?;

    // ServerLogin::finish валидирует AKE proof клиента — если пароль
    // неверный, finish вернёт error.
    let _result = server_login.finish(finalize_message).map_err(|e| {
        // Логируем только internally — клиенту не палим был ли пользователь
        // зарегистрирован vs пароль неверный.
        tracing::debug!(error = ?e, account_id = %B64.encode(account_id), "login finalization failed");
        (
            StatusCode::UNAUTHORIZED,
            "invalid credentials".to_string(),
        )
    })?;

    // На этом этапе server и client получили совпадающий session_key
    // (оба производят одинаковый key из AKE-handshake). Phase 1.1c НЕ
    // выпускает session token — это deferred. Просто отвечаем что login OK.
    tracing::info!(account_id = %B64.encode(account_id), "login completed");

    Ok(Json(LoginFinishResponse {
        ok: true,
        account_id: B64.encode(account_id),
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use opaque_ke::{
        ClientLogin, ClientLoginFinishParameters, ClientRegistration,
        ClientRegistrationFinishParameters,
    };

    /// End-to-end OPAQUE registration math, без HTTP / DB.
    /// Гарантирует что наш CipherSuite консистентен между client и server.
    #[test]
    fn registration_roundtrip_in_memory() {
        let mut rng = OsRng;
        let server_setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let credential_id = b"0123456789abcdef"; // 16 bytes — pretend UUIDv7
        let password = b"correct horse battery staple";

        // Client: start.
        let client_start = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, password)
            .expect("client registration start");

        // Server: start.
        let server_start = ServerRegistration::<LakkoCipherSuite>::start(
            &server_setup,
            client_start.message,
            credential_id,
        )
        .expect("server registration start");

        // Client: finish.
        let client_finish = client_start
            .state
            .finish(
                &mut rng,
                password,
                server_start.message,
                ClientRegistrationFinishParameters::default(),
            )
            .expect("client registration finish");

        // Server: finish — store the resulting registration.
        let server_registration =
            ServerRegistration::<LakkoCipherSuite>::finish(client_finish.message);

        // Sanity: serialize + deserialize round-trip the stored registration.
        let bytes = server_registration.serialize();
        let restored = ServerRegistration::<LakkoCipherSuite>::deserialize(&bytes)
            .expect("deserialize stored registration");
        assert_eq!(bytes.as_slice(), restored.serialize().as_slice());
    }

    #[test]
    fn decode_account_id_validates_length() {
        // Too short (8 bytes encoded).
        let short = B64.encode([0u8; 8]);
        let err = decode_account_id(&short).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Right length (16 bytes encoded).
        let ok = B64.encode([0u8; 16]);
        let bytes = decode_account_id(&ok).unwrap();
        assert_eq!(bytes.len(), 16);

        // Invalid base64.
        let err = decode_account_id("not-valid-base64!!!").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    /// Полный flow: register → login с правильным паролем. Должны получить
    /// matching session_keys на обеих сторонах.
    #[test]
    fn login_with_correct_password_succeeds() {
        let mut rng = OsRng;
        let server_setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let credential_id = b"0123456789abcdef";
        let password = b"correct horse battery staple";

        // ----- Регистрация (получаем stored ServerRegistration) -----
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, password).unwrap();
        let ss =
            ServerRegistration::<LakkoCipherSuite>::start(&server_setup, cs.message, credential_id)
                .unwrap();
        let cf = cs
            .state
            .finish(
                &mut rng,
                password,
                ss.message,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();
        let stored = ServerRegistration::<LakkoCipherSuite>::finish(cf.message);

        // ----- Login -----
        // Client: start.
        let cl_start = ClientLogin::<LakkoCipherSuite>::start(&mut rng, password).unwrap();

        // Server: start (с найденным registration).
        let sl_start = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &server_setup,
            Some(stored),
            cl_start.message,
            credential_id,
            ServerLoginStartParameters::default(),
        )
        .unwrap();

        // Client: finish — выдаёт client session_key.
        let cl_finish = cl_start
            .state
            .finish(
                password,
                sl_start.message,
                ClientLoginFinishParameters::default(),
            )
            .unwrap();

        // Server: finish — выдаёт server session_key.
        let sl_finish = sl_start.state.finish(cl_finish.message).unwrap();

        // Главная инвариант: клиент и сервер получили один и тот же
        // session_key. Если эта проверка проходит — auth удался.
        assert_eq!(
            cl_finish.session_key.as_slice(),
            sl_finish.session_key.as_slice(),
        );
    }

    /// Login с неверным паролем должен фейлится на ClientLogin::finish (или
    /// на ServerLogin::finish — зависит от того где AKE-proof развалился).
    #[test]
    fn login_with_wrong_password_fails() {
        let mut rng = OsRng;
        let server_setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let credential_id = b"0123456789abcdef";
        let real_password = b"correct horse battery staple";
        let wrong_password = b"hunter2";

        // ----- Регистрация на real_password -----
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, real_password).unwrap();
        let ss =
            ServerRegistration::<LakkoCipherSuite>::start(&server_setup, cs.message, credential_id)
                .unwrap();
        let cf = cs
            .state
            .finish(
                &mut rng,
                real_password,
                ss.message,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();
        let stored = ServerRegistration::<LakkoCipherSuite>::finish(cf.message);

        // ----- Login с wrong_password -----
        let cl_start = ClientLogin::<LakkoCipherSuite>::start(&mut rng, wrong_password).unwrap();
        let sl_start = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &server_setup,
            Some(stored),
            cl_start.message,
            credential_id,
            ServerLoginStartParameters::default(),
        )
        .unwrap();

        // ClientLogin::finish обычно фейлится здесь когда server's
        // CredentialResponse не совпадает с тем что клиент ожидал — envelope
        // не расшифровывается без правильного пароля.
        let result = cl_start.state.finish(
            wrong_password,
            sl_start.message,
            ClientLoginFinishParameters::default(),
        );
        assert!(result.is_err(), "login with wrong password must fail");
    }

    /// ServerLogin сериализуется + десериализуется в byte-identical виде —
    /// proxy для того что DB-roundtrip (`insert_login_state` →
    /// `take_login_state`) не теряет state. Реальный DB-roundtrip
    /// проверяется в integration test-ах против настоящей MariaDB.
    #[test]
    fn server_login_state_serializes_roundtrip() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let cred = b"0123456789abcdef";
        let pw = b"x";

        // Mini-registration чтобы получить stored ServerRegistration.
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, pw).unwrap();
        let ss = ServerRegistration::<LakkoCipherSuite>::start(&setup, cs.message, cred).unwrap();
        let cf = cs
            .state
            .finish(
                &mut rng,
                pw,
                ss.message,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();
        let stored = ServerRegistration::<LakkoCipherSuite>::finish(cf.message);

        // Login start, получаем ServerLogin state.
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut rng, pw).unwrap();
        let sl = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &setup,
            Some(stored),
            cl.message,
            cred,
            ServerLoginStartParameters::default(),
        )
        .unwrap();

        // Serialize + deserialize.
        let bytes = sl.state.serialize().to_vec();
        let restored =
            ServerLogin::<LakkoCipherSuite>::deserialize(&bytes).expect("ServerLogin deserialize");
        assert_eq!(bytes.as_slice(), restored.serialize().as_slice());
    }
}
