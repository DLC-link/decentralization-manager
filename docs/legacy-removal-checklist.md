# Legacy File-Based Config — Removal Checklist

Code to remove after all deployments have successfully migrated to the DB + env var solution.
The seeding logic and TOML/CSV reading exists only as a transitional bridge.

## Seeding Module

- [ ] `src/db/seed.rs` — entire file (seeds DB from CSV/TOML on startup)
- [ ] `src/db/mod.rs` — `pub mod seed;` declaration
- [ ] `src/main.rs` — `db::seed::seed_from_config(&pool, &config).await?;` call

## CSV (peers.csv) Loading/Saving

- [ ] `src/config.rs` — `NetworkConfig::from_file()` method
- [ ] `src/config.rs` — `NetworkConfig::save_to_file()` method
- [ ] `src/config.rs` — `NodeConfig::load_network_config()` method
- [ ] `src/config.rs` — `NodeConfig::save_network_config()` method
- [ ] `src/config.rs` — `NodeConfig::peers_csv_path()` method
- [ ] `Cargo.toml` — `csv` dependency (if no other usage remains)

## TOML (node.toml) Party Credentials

- [ ] `src/config.rs` — `NodeConfig::parties` field on the struct
- [ ] `src/config.rs` — `NodeConfig::get_party_credentials()` method
- [ ] `src/config.rs` — `NodeConfig::get_packages()` method
- [ ] `src/config.rs` — `NodeConfig::upsert_party_credentials()` method
- [ ] `src/config.rs` — `NodeConfig::save_config()` method
- [ ] `src/config.rs` — `NodeConfig::set_and_save_participant_id()` method
- [ ] `src/config.rs` — `PartyCredentials`, `KeycloakConfig`, `PackageConfig` serde derives (Deserialize/Serialize for TOML — keep if needed for DB row conversion)
- [ ] `src/server/mod.rs` — fallback to `config.parties` in `start_server()` when DB load fails
- [ ] `src/server/handlers/parties.rs` — `config.get_packages()` call (should read from DB instead)

## TOML (node.toml) Node Config Loading

- [ ] `src/config.rs` — `NodeConfig::from_dir()` TOML fallback path (the `if config_path.exists()` branch)
- [ ] `src/config.rs` — `NodeConfig` serde Deserialize derive (only needed for TOML parsing)
- [ ] `Cargo.toml` — `toml` dependency (if no other usage remains)

## Config Files

- [ ] `development/remote/participant-{1,2,3}/config/node.toml` — kept only for seeding
- [ ] `development/remote/participant-{1,2,3}/config/peers.csv` — kept only for seeding

## Deployment Files

- [ ] `zarf/deployments/devnet/participant{1,2,3}/deployment.yaml` — remove ConfigMap + init container if still present for seeding
