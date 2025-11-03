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

## Rust

### Cargo.toml

The release profile should be configured as follows:

```toml
[profile.release]
opt-level = 3
strip = "debuginfo"
codegen-units = 1
lto = true
panic = "unwind"
```

Dependencies should be listed in alphabetical order in the following format:

```toml
[dependencies]
anyhow = { version = "0.69.420", features = ["useless-feature", "tokio"] }
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

#### Adding Dependencies

Before adding a new dependency:
1. Explain what it will be used for
2. Ensure no existing dependency provides the same functionality
3. Add it in alphabetical order with features alphabetically ordered

### Error Handling

- Use `anyhow::Result` for general application errors and error propagation
- Use `thiserror` for custom error types with specific variants

```rust
use crate::error::Result;  // This is anyhow::Result

pub async fn load_file(path: &Path) -> Result<String> {
    let content = tokio::fs::read_to_string(path).await?;
    Ok(content)
}
```

#### Custom Errors

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

### Logging

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

#### Log Levels

- **`trace`** - Very detailed debugging information
- **`debug`** - General debugging information
- **`info`** - Important runtime information
- **`warn`** - Warning about potential issues
- **`error`** - Error conditions

### Code Style

#### Line Length

Keep lines under 100 characters when reasonable. Use Rust's trailing comma feature for better diffs:

```rust
let config = Config {
    admin_api_host: "localhost".to_string(),
    admin_api_port: 5001,
    ledger_api_host: "localhost".to_string(),
    ledger_api_port: 5002,  // Trailing comma
};
```

#### Documentation

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

#### Testing

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

### Code Quality Tools

Run these commands before committing:

```sh
# Format code
cargo fmt

# Check for warnings (strict mode)
cargo clippy --all-targets --all-features -- -D warnings

# Run tests
cargo test
```

#### CI Requirements

All pull requests must:
- Pass `cargo fmt -- --check`
- Pass `cargo clippy --all-targets --all-features -- -D warnings`
- Pass `cargo test`
- Have no compiler warnings
