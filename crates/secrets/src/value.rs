use std::fmt;

use zeroize::Zeroizing;

const REDACTED: &str = "<redacted>";

/// A secret value. Reading it requires an explicit call to [`SecretValue::expose`].
///
/// The buffer is zeroed on drop. `Clone` and `Serialize` are deliberately absent.
/// The first example is a control, showing that the bound compiles for a name:
///
/// ```
/// fn needs_serialize<T: serde::Serialize>(_: T) {}
/// needs_serialize(secrets::SecretName::new("A").unwrap());
/// ```
///
/// ```compile_fail
/// fn needs_serialize<T: serde::Serialize>(_: T) {}
/// needs_serialize(secrets::SecretValue::new("x"));
/// ```
///
/// ```compile_fail
/// let a = secrets::SecretValue::new("x");
/// let _b = a.clone();
/// ```
pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    /// Takes ownership of the value. Passing a `String` moves its buffer, so no
    /// second copy is made. Passing a `&str` copies it, and the caller remains
    /// responsible for the original.
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub(crate) fn from_zeroizing(value: Zeroizing<String>) -> Self {
        Self(value)
    }

    /// Returns the plaintext. Every call site is a place the value can leak, so
    /// keep them few and never pass the result to logs, errors or process args.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretValue({REDACTED})")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}
