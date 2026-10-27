//! The operating system and CPU an engine build is for, and the keys
//! manifests use for them (`linux-x86_64`, `macos-aarch64`, `windows-x86_64`).

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Os::Macos
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Macos => "macos",
            Os::Windows => "windows",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Host {
    pub os: Os,
    /// `x86_64` or `aarch64`.
    pub arch: &'static str,
}

impl Host {
    pub fn current() -> Self {
        Self {
            os: Os::current(),
            arch: if cfg!(target_arch = "aarch64") {
                "aarch64"
            } else {
                "x86_64"
            },
        }
    }

    /// The manifest key of this host.
    pub fn key(self) -> String {
        format!("{}-{}", self.os.as_str(), self.arch)
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key())
    }
}
