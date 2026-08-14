use std::env;

use bitcoin::Network;
use config::Config;
use sqlx::{Sqlite, migrate::MigrateDatabase, SqlitePool};
use anyhow::Result;

/// Config struct storing all StataChain Entity config
pub struct ClientConfig {
    /// Active lockbox server addresses
    pub statechain_entity: String,
    /// Electrum client
    pub electrum_client: electrum_client::Client,
    /// Electrum server url
    pub electrum_server_url: String,
    /// Electrum server type (e.g. electrs, electrumx, etc.)
    pub electrum_type: String,
    /// Bitcoin network name (testnet, regtest, mainnet)
    pub network: Network,
    /// Fee rate tolerance
    pub fee_rate_tolerance: f64,
    /// Confirmation target
    pub confirmation_target: u32,
    /// Database connection pool
    pub pool: sqlx::Pool<Sqlite>,
    /// Tor SOCKS5 proxy address
    pub tor_proxy: Option<String>,
    /// Confirmation target
    pub max_fee_rate: f64,
    /// **[D69] The enclave attestation identity to verify terminality against.**
    ///
    /// Used ONLY where this build has no compiled-in pin for the network
    /// (`mercurylib::tesr::TesrParams::attestation_identity_const`). Where a pin exists it wins and a
    /// mismatching value here is a REFUSAL, not an override — see
    /// `TesrParams::attestation_identity`. Read the value from the enclave's
    /// `GET /attestation_identity`.
    pub attestation_identity: Option<String>,
}

fn check_and_set_settings() -> String {
    if let Ok(value) = env::var("ML_NETWORK") {
        if value == "regtest" {
            return String::from("regtest.Settings");
        }
    }
    "Settings".to_string()
}

impl ClientConfig {
    /// Programmatic constructor for SDK embedders (no Settings.toml). Creates the sqlite
    /// database if missing and runs this crate's migrations.
    ///
    /// `attestation_identity` is the [D69] enclave attestation identity pin. `None` falls back to
    /// `UTEXO_ATTESTATION_IDENTITY`; if neither is set and this build has no compiled-in pin, every
    /// laddering claim REFUSES. That is the correct direction to fail — the alternative is trusting
    /// the key the coordinator serves alongside the signature it is meant to check.
    pub async fn from_params(
        statechain_entity: String,
        electrum_server: String,
        electrum_type: String,
        network: Network,
        database_file: String,
        confirmation_target: u32,
        attestation_identity: Option<String>,
    ) -> Result<Self> {
        if !Sqlite::database_exists(&database_file).await.unwrap_or(false) {
            Sqlite::create_database(&database_file).await?;
        }
        let pool: sqlx::Pool<Sqlite> = SqlitePool::connect(&database_file).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;

        let electrum_client = electrum_client::Client::new(electrum_server.as_str())?;

        Ok(ClientConfig {
            statechain_entity,
            electrum_client,
            electrum_server_url: electrum_server,
            electrum_type,
            network,
            fee_rate_tolerance: 5.0,
            confirmation_target,
            pool,
            tor_proxy: None,
            max_fee_rate: 1.0,
            // [D69] The embedder's explicit pin wins; the environment is the fallback so an
            // existing harness that exports UTEXO_ATTESTATION_IDENTITY keeps working unchanged.
            attestation_identity: attestation_identity
                .or_else(|| std::env::var("UTEXO_ATTESTATION_IDENTITY").ok()),
        })
    }

    pub async fn load() -> Self {

        let settings_filename = check_and_set_settings();

        let settings = Config::builder()
            .add_source(config::File::with_name(&settings_filename))
            .build()
            .unwrap();

        let statechain_entity = settings.get_string("statechain_entity").unwrap();
        let electrum_server = settings.get_string("electrum_server").unwrap();
        let electrum_type = settings.get_string("electrum_type").unwrap();
        let network = settings.get_string("network").unwrap();
        let fee_rate_tolerance = settings.get_int("fee_rate_tolerance").unwrap() as f64;
        let database_file = settings.get_string("database_file").unwrap();
        let confirmation_target = settings.get_int("confirmation_target").unwrap() as u32;
        let max_fee_rate = settings.get_int("max_fee_rate").unwrap() as f64;

        // [D69] Optional in the file; the environment wins so a CI run can point at a freshly
        // seeded lockbox without rewriting Settings.toml. Neither is a fallback for a compiled-in
        // pin — `TesrParams::attestation_identity` refuses a mismatch rather than preferring one.
        let attestation_identity = std::env::var("UTEXO_ATTESTATION_IDENTITY")
            .ok()
            .or_else(|| settings.get_string("attestation_identity").ok());

        let tor_proxy = match settings.get_string("tor_proxy") {
            Ok(proxy) => Some(proxy.to_string()),
            Err(_) => None,
        };
        // Open database connection pool

        if !Sqlite::database_exists(&database_file).await.unwrap_or(false) {
            match Sqlite::create_database(&database_file).await {
                Ok(_) => {},
                Err(error) => panic!("error: {}", error),
            }
        }
    
        let pool: sqlx::Pool<Sqlite> = SqlitePool::connect(&database_file).await.unwrap();
    
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .unwrap();

        // Convert network string to Network enum

        let network = match network.as_str() {
            "signet" => Network::Signet,
            "testnet" => Network::Testnet,
            "regtest" => Network::Regtest,
            "bitcoin" => Network::Bitcoin,
            _ => panic!("Invalid network name"),
        };

        // Create Electrum client

        let electrum_client = electrum_client::Client::new(electrum_server.as_str()).unwrap();

        ClientConfig {
            statechain_entity,
            electrum_client,
            electrum_server_url: electrum_server,
            electrum_type,
            network,
            fee_rate_tolerance,
            confirmation_target,
            pool,
            tor_proxy,
            max_fee_rate,
            attestation_identity,
        }
    }

    pub fn get_reqwest_client(&self) -> Result<reqwest::Client> {

        // Bound every SE request so a hung/black-holing SE cannot pin the caller's task forever
        // (adversarial-log review, DELAY dimension: the core client previously built bare clients
        // with no timeout, unlike the SSP client). 120s is far longer than any single SE JSON
        // round-trip — deposit confirmation etc. are client-side polling loops of separate quick
        // calls, not one long request — so this only fires on a genuine stall, converting an
        // infinite hang into a clean error.
        let timeout = std::time::Duration::from_secs(120);

        match self.tor_proxy {
            Some(ref proxy) => {
                let proxy = reqwest::Proxy::all(proxy)?;
                Ok(reqwest::Client::builder()
                    .proxy(proxy)
                    .timeout(timeout)
                    .build()?)
            },
            None => Ok(reqwest::Client::builder()
                .timeout(timeout)
                .build()?),

        }
    }
}

pub async fn load() -> ClientConfig {
    let client_config = ClientConfig::load().await;
    client_config
}