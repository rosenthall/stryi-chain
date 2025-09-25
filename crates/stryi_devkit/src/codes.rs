//! Exit codes for the CLI tools for CI/CD use.
//! These codes are returned by the end of tools' execution
//! to indicate success or failure type.

/// OK is OK
/// Program ran to completion with no errors.
pub const EXIT_OK: i32 = 0;

/// IO or TOML parse errors
/// e.g. cannot read file, or cannot parse TOML.
/// See logs for full details.
pub const EXIT_IO_OR_PARSE: i32 = 1;

/// reserved for configuration errors.
/// config was read and parsed, but had invalid values.
/// See logs for full details.
pub const EXIT_CONFIG: i32 = 2;

/// reserved for unexpected fatal errors
pub const EXIT_UNKNOWN: i32 = 4;
