use std::borrow::Borrow;
use std::fmt;

use elp_syntax::SmolStr;

use serde::{Deserialize, Serialize};

#[derive(
    Deserialize,
    Serialize,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord
)]
pub struct AtomName(SmolStr);

impl AtomName {
    pub fn new(name: &str) -> Self {
        Self(name.into())
    }

    /// Returns the unquoted name as a `str`.
    ///
    /// The `Display` implementation and `ToString` should be preferred. Only use this function
    /// in cases where atom names like `'Elixir.Foo'` should not be quoted, for example when
    /// building the filename of a module.
    pub fn as_unquoted_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AtomName {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}

impl From<String> for AtomName {
    fn from(s: String) -> Self {
        Self(s.into())
    }
}

impl From<SmolStr> for AtomName {
    fn from(s: SmolStr) -> Self {
        Self(s)
    }
}

impl fmt::Display for AtomName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use std::fmt::Write;

        fn is_unquoted_atom(input: &str) -> bool {
            let mut chars = input.chars();
            chars.next().is_some_and(|c| c.is_lowercase())
                && chars.all(|c| char::is_alphanumeric(c) || c == '_' || c == '@')
        }

        if is_unquoted_atom(&self.0) {
            f.write_str(&self.0)
        } else {
            f.write_char('\'')?;
            f.write_str(&self.0)?;
            f.write_char('\'')
        }
    }
}

impl Borrow<str> for AtomName {
    fn borrow(&self) -> &str {
        // `Borrow<str>` is implemented only to satisfy hashmap trait bounds. See
        // `as_unquoted_str` docs above.
        self.as_unquoted_str()
    }
}

impl PartialEq<&str> for AtomName {
    fn eq(&self, other: &&str) -> bool {
        &self.0 == other
    }
}

impl PartialEq<AtomName> for &str {
    fn eq(&self, other: &AtomName) -> bool {
        other.eq(self)
    }
}

impl AsRef<std::ffi::OsStr> for AtomName {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}
