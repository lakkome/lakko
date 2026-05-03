// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! OPAQUE-PAKE (RFC draft `opaque`) registration flow для auth service.
//!
//! Сервер никогда не видит plaintext password — клиент и сервер
//! обмениваются OPRF-blinded сообщениями, в результате на сервере остаётся
//! "envelope" из которого нельзя выкачать пароль без MITM на момент
//! регистрации (защищено OPAQUE-математикой).
//!
//! Phase 1.1b ships только flow регистрации (signup). Login (`/login/start`
//! + `/login/finish`) — следующий sub-sprint, использует те же примитивы.
//!
//! Серверный static-state (OPRF + AKE keypairs) генерится один раз на
//! первом старте и persisted в `opaque_server_setup` таблице. Per-account
//! registration material — в `opaque_registrations`, key-ed по
//! `account_id` (UUIDv7 raw bytes), с FK на `accounts(id)`.

use anyhow::{anyhow, Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use opaque_ke::key_exchange::tripledh::TripleDh;
use opaque_ke::{
    CipherSuite, RegistrationRequest, RegistrationUpload, Ristretto255, ServerRegistration,
    ServerSetup,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

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
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use opaque_ke::{ClientRegistration, ClientRegistrationFinishParameters};

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
}
