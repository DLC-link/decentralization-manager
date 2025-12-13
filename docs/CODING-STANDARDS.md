# Coding Standards

## Git

### Commits

Git commits should be written in past tense.

The commit messages should follow the format:

```
<type>(<scope>): <subject>
```

Where:
- `<type>` is the type of change (e.g., `feat`, `fix`, `docs`, `style`, `refact`, `perf`, `test`, `chore`)
- `<scope>` is the scope of the change (e.g., `api`, `ui`, `core`, `utils`)
- `<subject>` is a brief description of the change

### Branches

Similarly to commits, branches should be named using the following format:

```
<type>/<scope>/<subject>
```

Where:
- `<type>` is the type of change (e.g., `feat`, `fix`, `docs`, `style`, `refact`, `perf`, `test`, `chore`)
- `<scope>` is the scope of the change (e.g., `api`, `ui`, `core`, `utils`)
- `<subject>` is a brief description of the change

## Rust Code Organization

### Module and Use Statement Order

Module declarations should come first, followed by use statements:

1. Module declarations (`mod foo;`)
2. Public module declarations (`pub mod bar;`)
3. Use statements (grouped as described below)

```rust
// Good
mod utils;
mod config;

pub mod api;
pub mod error;

use std::path::Path;

use clap::Parser;

use crate::utils::helper;
```

### Use Statements

Use statements should be grouped in the following order:

1. Standard library
2. Third-party crates
3. Local modules

These groups should be separated by a blank line.

#### Multiple Imports from Same Module

When importing multiple items from the same module, use the brace syntax:

```rust
// Good
use std::{collections::HashMap, path::Path};

use tokio::{fs, io};

use crate::{error::Result, utils};

// Bad
use std::path::Path;
use std::collections::HashMap;

use tokio::fs;
use tokio::io;

use crate::error::Result;
use crate::utils;
```

#### Public Re-exports

Within each group, regular imports should come first, followed by a blank line, then public re-exports:

```rust
// Good
use std::path::Path;

use clap::Subcommand;

pub use clap::Parser;

use crate::error::Result;
```

#### Complete Example

```rust
use std::path::Path;

use serde::Deserialize;
use tokio::fs;

pub use clap::Parser;

use crate::{error::Result, utils};
```

### Use Statement Placement

All `use` statements must be placed at the top of the file, never inside functions. This keeps imports organized and makes dependencies visible at a glance.

```rust
// Good
use anyhow::Context;

use crate::consts::{LEDGER_SUBMISSIONS_DIR, PREPARED_DIR};

pub async fn my_function() -> Result {
    let dir = Path::new(LEDGER_SUBMISSIONS_DIR).join(PREPARED_DIR);
    // ...
}

// Bad
pub async fn my_function() -> Result {
    use anyhow::Context;
    use crate::consts::{LEDGER_SUBMISSIONS_DIR, PREPARED_DIR};

    let dir = Path::new(LEDGER_SUBMISSIONS_DIR).join(PREPARED_DIR);
    // ...
}
```

### Namespace Resolution

Always use `use` statements for long namespace paths (3+ segments) instead of resolving them inline. This improves code readability and makes it easier to refactor.

```rust
// Good
use crate::proto::com::digitalasset::canton::crypto::v30::SigningPublicKey;

fn get_key() -> Result<SigningPublicKey> {
    // ...
}

// Bad
fn get_key() -> Result<crate::proto::com::digitalasset::canton::crypto::v30::SigningPublicKey> {
    // ...
}
```

```rust
// Good
use std::collections::HashMap;

let map = HashMap::new();

// Bad
let map = std::collections::HashMap::new();
```

**Exceptions:**

- Short paths (1-2 segments) like `std::env::current_dir()` are acceptable
- Proto-generated enum variants like `store_id::Store::Synchronizer()` can be used inline
- When a full path provides clarity in a specific context

## Rust Code Style

### Format Strings

Use inline values in format strings instead of positional arguments:

```rust
// Good
let message = format!("Error at line {line}: {error}");
tracing::info!("Processing file {path} with {count} items");

// Bad
let message = format!("Error at line {}: {}", line, error);
tracing::info!("Processing file {} with {} items", path, count);
```

**Important**: Do NOT create new variables just to use inline formatting syntax. Only use inline formatting with variables that already exist:

```rust
// Good - using existing variable
let address = format!("{}:{}", host, port);
tracing::info!("Connecting to {}", address);

// Bad - creating variable just for formatting
let address = format!("{}:{}", host, port);
let addr = &address;
tracing::info!("Connecting to {addr}");  // Unnecessary intermediate variable

// Also Bad - creating variable just to inline it
let participant_id = &participant.id;
tracing::info!("Checking {participant_id}");  // Should just use participant.id directly
```

### Line Length

Keep lines under 100 characters when reasonable. Use Rust's trailing comma feature for better diffs:

```rust
let config = Config {
    admin_api_host: "localhost".to_string(),
    admin_api_port: 5001,
    ledger_api_host: "localhost".to_string(),
    ledger_api_port: 5002,
    ledger_api_token: None,  // Trailing comma
};
```

### Documentation

Document public APIs with doc comments:

```rust
/// Load configuration from a TOML file
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed
pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
    // ...
}
```

## Cargo.toml

### Release Profile

The release profile should be configured as follows:

```toml
[profile.release]
opt-level = 3
strip = "debuginfo"
codegen-units = 1
lto = true
panic = "unwind"
```

### Dependencies

All dependencies and their features must be in **strict alphabetical order**:

```toml
[dependencies]
anyhow = { version = "1.0.98" }
bytes = { version = "1.9.0" }
clap = { version = "4.5.43", features = ["derive", "env", "string"] }
tokio = { version = "1.48.0", features = ["fs", "macros", "rt-multi-thread"] }
```

Features within each dependency must also be alphabetical:
- ✓ `features = ["derive", "env", "string"]`
- ✗ `features = ["string", "derive", "env"]`

**Format:**
```toml
[dependencies]
anyhow = { version = "0.69.420", features = ["useless-feature", "tokio"] }
```

**Adding New Dependencies:**

Before adding a new dependency:
1. Explain what it will be used for
2. Ensure no existing dependency provides the same functionality
3. Add it in alphabetical order with features alphabetically ordered

## Standard Crates

The following crates should be used for their respective purposes:

### CLI and Configuration
- **`clap`** - Command-line argument parsing and environment variable handling
  - Use with `derive` feature for declarative CLI definitions
  - Use `env` feature for reading configuration from environment variables

### Logging and Observability
- **`tracing`** - Structured logging and instrumentation
- **`tracing-subscriber`** - Collecting and formatting tracing data

### Error Handling
- **`anyhow`** - General application errors and error propagation
- **`thiserror`** - Defining custom error types with specific variants

### Enums
- **`strum`** - Enum utilities and macros
  - Use `EnumString` for parsing strings into enums
  - Use `Display` for converting enums to strings
  - Use `EnumIter` for iterating over enum variants

### Database
- **`sqlx`** - Async SQL database access
  - Compile-time checked queries
  - Support for PostgreSQL, MySQL, SQLite

### Protocol Buffers and gRPC
- **`tonic`** - gRPC client and server implementation
- **`prost`** - Protocol Buffers encoding/decoding (typically used with tonic)

### Async Runtime
- **`tokio`** - Async runtime for I/O operations
  - Use `rt-multi-thread` feature for multi-threaded runtime
  - Use `macros` feature for `#[tokio::main]` and `#[tokio::test]`

When a new crate is needed, prefer using these standard crates over alternatives to maintain consistency across the codebase.

## Error Handling

- Use `anyhow::Result` for general application errors and error propagation
- Use `thiserror` for custom error types with specific variants

```rust
use crate::error::Result;  // This is anyhow::Result

pub async fn load_file(path: &Path) -> Result<String> {
    let content = tokio::fs::read_to_string(path).await?;
    Ok(content)
}
```

### Custom Errors

When creating custom error types, use `thiserror`:

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Configuration file not found at {path}")]
    NotFound { path: String },

    #[error("Invalid configuration: {reason}")]
    Invalid { reason: String },
}
```

## Logging

Use `tracing` for all logging instead of `println!` or `eprintln!`:

```rust
// Good
tracing::info!("Starting process for user {user_id}");
tracing::warn!("Attempt {attempt}/{max_attempts}: condition not met");
tracing::error!("Failed to connect to {host}:{port}: {error}");

// Bad
println!("Starting process for user {}", user_id);
eprintln!("Error: {}", error);
```

### Log Levels

- **`trace`** - Very detailed debugging information
- **`debug`** - General debugging information
- **`info`** - Important runtime information
- **`warn`** - Warning about potential issues
- **`error`** - Error conditions

## Testing

Write tests for utility functions and important logic:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_name() {
        // Arrange
        let input = create_test_input();

        // Act
        let result = process(input);

        // Assert
        assert_eq!(result, expected);
    }
}
```

### Database Testing

For tests that require database access, use the `#[sqlx::test]` macro with a migrator. This macro automatically:
- Creates a new isolated database for each test
- Runs migrations before each test
- Cleans up the database after the test completes
- Provides connection pooling

**Setup Requirements:**

1. Add `sqlx` to your dependencies in `Cargo.toml`:

```toml
[dependencies]
sqlx = { version = "0.8", features = ["any", "chrono", "postgres", "runtime-tokio-rustls", "uuid"] }
```

2. Create a migrations directory with numbered SQL files:

```
migrations/
├── 01_base.sql
├── 02_add_users_table.sql
└── 03_add_indexes.sql
```

3. Define a static `MIGRATOR` in your `main.rs` or `lib.rs`:

```rust
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
```

**Basic Database Test Pattern:**

```rust
#[cfg(test)]
mod tests {
    use crate::{MIGRATOR, error::Result};

    use super::*;

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_insert_user(pool: PgPool) -> Result {
        // Arrange
        let username = "alice";
        let email = "alice@example.com";

        // Act
        sqlx::query!(
            "INSERT INTO users (username, email) VALUES ($1, $2)",
            username,
            email
        )
        .execute(&pool)
        .await?;

        // Assert
        let user = sqlx::query!("SELECT username, email FROM users WHERE username = $1", username)
            .fetch_one(&pool)
            .await?;

        assert_eq!(user.username, username);
        assert_eq!(user.email, email);

        Ok(())
    }
}
```

**Key Points:**

1. **Always use `migrator = "MIGRATOR"`**: This ensures your migrations run before each test
2. **Import MIGRATOR**: Use `use crate::MIGRATOR;` in your test modules
3. **Use project Result type**: Import and use your project's `Result` type (typically `anyhow::Result`)
4. **Query macros**: Use `sqlx::query!()` and `sqlx::query_as!()` for compile-time checked queries

**Complete Example:**

```rust
// In main.rs or lib.rs
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

// In your module (e.g., src/storage/tables/contracts.rs)
use anyhow::Context;
use sqlx::PgPool;

use crate::error::Result;

#[derive(Debug, sqlx::FromRow)]
pub struct Contract {
    pub contract_id: String,
    pub template_id: String,
    pub created_at: chrono::NaiveDateTime,
}

impl Contract {
    pub async fn insert(&self, pool: &PgPool) -> Result {
        sqlx::query!(
            "INSERT INTO contracts (contract_id, template_id, created_at) VALUES ($1, $2, $3)",
            self.contract_id,
            self.template_id,
            self.created_at
        )
        .execute(pool)
        .await
        .context("Failed to insert contract")?;

        Ok(())
    }

    pub async fn get_all(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        sqlx::query_as!(
            Self,
            "SELECT contract_id, template_id, created_at FROM contracts LIMIT $1 OFFSET $2",
            limit,
            offset
        )
        .fetch_all(pool)
        .await
        .context("Failed to fetch contracts")
    }
}

#[cfg(test)]
mod tests {
    use crate::{MIGRATOR, error::Result};

    use super::*;

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_get_all(pool: PgPool) -> Result {
        // Test with empty database (migrations have run)
        let contracts = Contract::get_all(&pool, 100, 0).await?;
        assert_eq!(contracts.len(), 0);

        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn test_insert_and_retrieve(pool: PgPool) -> Result {
        // Insert test data
        let contract = Contract {
            contract_id: "test-123".to_string(),
            template_id: "template-456".to_string(),
            created_at: chrono::Utc::now().naive_utc(),
        };

        contract.insert(&pool).await?;

        // Retrieve and verify
        let contracts = Contract::get_all(&pool, 100, 0).await?;
        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].contract_id, "test-123");

        Ok(())
    }
}
```

**Best Practices:**

1. **Isolation**: Each test gets its own database, so tests can run in parallel
2. **Clean State**: Always assume a clean database state (only migrations have run)
3. **Use project error types**: Return your project's `Result` type, not `sqlx::Result`
4. **Context for errors**: Use `.context()` to add helpful error messages
5. **Compile-time checking**: Use `query!()` and `query_as!()` macros for type safety

## Code Quality Tools

Run these commands before committing:

```sh
# Format code
cargo fmt

# Check for warnings (strict mode)
cargo clippy --all-targets --all-features -- -D warnings

# Run tests
cargo test
```

### CI Requirements

All pull requests must:
- Pass `cargo fmt -- --check`
- Pass `cargo clippy --all-targets --all-features -- -D warnings`
- Pass `cargo test`
- Have no compiler warnings
