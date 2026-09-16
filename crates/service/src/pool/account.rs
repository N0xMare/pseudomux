//! Named credential pins. One daemon, many Anthropic accounts.
//!
//! Claude Code does not pick an account from `.claude.json`. The live token is
//! the keychain (or file) slot named by `CLAUDE_SECURESTORAGE_CONFIG_DIR`
//! bytes. Two accounts in one `pmuxd` are therefore two pins, not two binaries
//! (`claude-1` on this host is `CLAUDE_CONFIG_DIR=~/.claude-1 claude` — the
//! same Mach-O).
//!
//! The caller names an account the same way it names a model: a daemon-
//! configured word, never a filesystem path. The pin stays operator config.

use std::fmt;

/// Longest operator account name. Enough for `default` and `claude-1`.
pub const MAX_ACCOUNT_NAME_BYTES: usize = 32;

/// The account used when a request omits `account` and a warm class omits
/// `@name`. Its pin is `--pool-securestorage-dir` (default `empty`).
pub const DEFAULT_ACCOUNT: &str = "default";

/// Copy-sized account name, interned into the class key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountName {
    buf: [u8; MAX_ACCOUNT_NAME_BYTES],
    len: u8,
}

impl AccountName {
    pub const DEFAULT: Self = Self::from_bytes(b"default");

    const fn from_bytes(bytes: &[u8]) -> Self {
        let mut buf = [0u8; MAX_ACCOUNT_NAME_BYTES];
        let mut i = 0;
        while i < bytes.len() {
            buf[i] = bytes[i];
            i += 1;
        }
        Self {
            buf,
            len: bytes.len() as u8,
        }
    }

    /// `^[A-Za-z][A-Za-z0-9_-]{0,31}$`.
    pub fn parse(raw: &str) -> Result<Self, AccountNameError> {
        if raw.is_empty() || raw.len() > MAX_ACCOUNT_NAME_BYTES {
            return Err(AccountNameError::Invalid(raw.to_owned()));
        }
        let mut chars = raw.chars();
        let Some(first) = chars.next() else {
            return Err(AccountNameError::Invalid(raw.to_owned()));
        };
        if !first.is_ascii_alphabetic()
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(AccountNameError::Invalid(raw.to_owned()));
        }
        Ok(Self::from_bytes(raw.as_bytes()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or(DEFAULT_ACCOUNT)
    }

    #[must_use]
    pub fn is_default(self) -> bool {
        self == Self::DEFAULT
    }
}

impl fmt::Display for AccountName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountNameError {
    Invalid(String),
}

impl fmt::Display for AccountNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(raw) => write!(
                formatter,
                "account name {raw:?} is not {MAX_ACCOUNT_NAME_BYTES} or fewer ASCII letters, \
                 digits, `_` or `-`, starting with a letter (e.g. default, claude-1)"
            ),
        }
    }
}

impl std::error::Error for AccountNameError {}

/// One operator-configured pin, after boot validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredAccount {
    pub name: AccountName,
    /// Exact `CLAUDE_SECURESTORAGE_CONFIG_DIR` bytes. Empty = unsuffixed store.
    pub pin: String,
}

impl ConfiguredAccount {
    #[must_use]
    pub fn pin_is_empty(&self) -> bool {
        self.pin.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_the_word_default() {
        assert_eq!(AccountName::DEFAULT.as_str(), "default");
        assert!(AccountName::DEFAULT.is_default());
        assert_eq!(AccountName::parse("default").unwrap(), AccountName::DEFAULT);
    }

    #[test]
    fn claude_1_is_a_legal_account_name() {
        let name = AccountName::parse("claude-1").unwrap();
        assert_eq!(name.as_str(), "claude-1");
        assert!(!name.is_default());
    }

    #[test]
    fn a_path_is_not_an_account_name() {
        assert!(AccountName::parse("/Users/me/.claude-1").is_err());
        assert!(AccountName::parse("~/.claude-1").is_err());
        assert!(AccountName::parse("").is_err());
        assert!(AccountName::parse("1claude").is_err());
    }
}
