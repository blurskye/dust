//! Bounded artifact and log downloads (W-23).
//!
//! The contract: "Artifact and log downloads name allowed redirect hosts explicitly, are
//! size-bounded, and are never extracted or executed."
//!
//! Nothing here unpacks, decodes or runs anything. A plan carries one address, the hosts it may be
//! redirected to BY NAME, and a size bound; the only output a download can produce is the
//! transport's bounded body, and the transport already refuses a body one byte past its bound.

use super::MAX_RESPONSE_BYTES;

/// Why a download was refused before it was planned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadRefusal {
    /// A download follows only hosts that were NAMED. Neither an empty list nor a wildcard is a
    /// name, so neither is a plan.
    NoAllowedHosts,
    /// A wildcard claims a right to follow anywhere, which is the opposite of naming explicitly.
    WildcardHost(String),
    /// The bound asked for is larger than the transport's own bound.
    BoundTooLarge,
    /// The address could not be read as a bounded HTTPS address.
    UnreadableAddress(String),
}

/// A download: one address, explicitly named redirect hosts, and a size bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadPlan {
    pub address: String,
    allowed_hosts: Vec<String>,
    pub max_bytes: usize,
}

impl DownloadPlan {
    pub fn new(
        address: impl Into<String>,
        allowed_hosts: &[&str],
        max_bytes: usize,
    ) -> Result<Self, DownloadRefusal> {
        let address = address.into();
        super::ApiOrigin::parse(&address)
            .map_err(|_| DownloadRefusal::UnreadableAddress(address.clone()))?;
        if allowed_hosts.is_empty() {
            return Err(DownloadRefusal::NoAllowedHosts);
        }
        for host in allowed_hosts {
            if host.contains('*') {
                return Err(DownloadRefusal::WildcardHost((*host).to_owned()));
            }
        }
        if max_bytes > MAX_RESPONSE_BYTES {
            return Err(DownloadRefusal::BoundTooLarge);
        }
        Ok(Self {
            address,
            allowed_hosts: allowed_hosts.iter().map(|host| (*host).to_owned()).collect(),
            max_bytes,
        })
    }

    /// The hosts a redirect may be followed to, in the form the transport takes.
    #[must_use]
    pub fn allowed_hosts(&self) -> Vec<&str> {
        self.allowed_hosts.iter().map(String::as_str).collect()
    }
}
