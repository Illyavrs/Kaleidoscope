//! Validated private-server origins shared by launcher settings and sessions.

use crate::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::{Host, Url};

const MAX_ENDPOINT_BYTES: usize = 2048;
const DEFAULT_ORIGIN: &str = "https://illya.test";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    origin: String,
    domain: String,
    port: u16,
}

impl Endpoint {
    /// Accept an HTTPS DNS origin and normalize case, slash and default port.
    /// Errors never echo the input, which could contain rejected credentials.
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > MAX_ENDPOINT_BYTES
            || !value.is_ascii()
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace() || byte == b'\\')
        {
            return Err(
                "Server endpoint must be 1–2048 ASCII bytes without whitespace or controls".into(),
            );
        }
        let (scheme, remainder) = value
            .split_once("://")
            .ok_or("Server endpoint must be an HTTPS origin")?;
        if !scheme.eq_ignore_ascii_case("https") {
            return Err("Server endpoint must use HTTPS".into());
        }
        let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
        let authority = &remainder[..authority_end];
        let suffix = &remainder[authority_end..];
        if authority.is_empty() || authority.contains(['@', '%']) {
            return Err(
                "Server endpoint authority must not contain credentials or percent encoding".into(),
            );
        }
        // Reject path syntax before URL normalization can erase dot segments.
        if !suffix.is_empty() && suffix != "/" {
            return Err("Server endpoint must not contain a path, query or fragment".into());
        }
        if let Some((_, supplied_port)) = authority.rsplit_once(':') {
            if supplied_port.is_empty()
                || !supplied_port.bytes().all(|byte| byte.is_ascii_digit())
                || supplied_port
                    .parse::<u16>()
                    .ok()
                    .is_none_or(|port| port == 0)
            {
                return Err("Server endpoint port must be between 1 and 65535".into());
            }
        }
        let parsed = Url::parse(value).map_err(|_| "Server endpoint is not a valid URL")?;
        if parsed.scheme() != "https"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("Server endpoint must be an HTTPS origin without credentials, path, query or fragment".into());
        }
        let domain = match parsed.host() {
            Some(Host::Domain(domain)) => domain.to_ascii_lowercase(),
            _ => {
                return Err("Server endpoint must use a DNS name rather than an IP address".into())
            }
        };
        if !domain.is_ascii()
            || domain.len() > 253
            || domain.ends_with('.')
            || domain.split('.').count() < 2
            || domain.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || !label
                        .as_bytes()
                        .first()
                        .is_some_and(|byte| byte.is_ascii_alphanumeric())
                    || !label
                        .as_bytes()
                        .last()
                        .is_some_and(|byte| byte.is_ascii_alphanumeric())
                    || !label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err(
                "Server endpoint must use a valid ASCII DNS name with at least two labels".into(),
            );
        }
        if domain == "ppy.sh" || domain.ends_with(".ppy.sh") {
            return Err("Official osu! domains cannot be used as a private-server endpoint".into());
        }
        Ok(Self {
            origin: parsed.origin().ascii_serialization(),
            domain,
            port: parsed.port().unwrap_or(443),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.origin
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stable's devserver argument cannot express a custom HTTPS port.
    pub fn stable_domain(&self) -> Result<&str> {
        if self.port != 443 {
            return Err("Stable requires a private-server endpoint using HTTPS port 443".into());
        }
        Ok(&self.domain)
    }
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            origin: DEFAULT_ORIGIN.into(),
            domain: "illya.test".into(),
            port: 443,
        }
    }
}

impl TryFrom<&str> for Endpoint {
    type Error = String;

    fn try_from(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl TryFrom<String> for Endpoint {
    type Error = String;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl Serialize for Endpoint {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_dns_origins_canonicalize_without_losing_custom_ports() {
        for (input, origin, domain, port) in [
            (
                "https://illya.test",
                "https://illya.test",
                "illya.test",
                443,
            ),
            (
                "HTTPS://ILLYA.TEST/",
                "https://illya.test",
                "illya.test",
                443,
            ),
            (
                "https://illya.test:443/",
                "https://illya.test",
                "illya.test",
                443,
            ),
            (
                "https://illya.test:00443/",
                "https://illya.test",
                "illya.test",
                443,
            ),
            (
                "https://server.example:8443/",
                "https://server.example:8443",
                "server.example",
                8443,
            ),
            (
                "https://server.example:1",
                "https://server.example:1",
                "server.example",
                1,
            ),
            (
                "https://server.example:65535",
                "https://server.example:65535",
                "server.example",
                65535,
            ),
            (
                "https://xn--bcher-kva.test/",
                "https://xn--bcher-kva.test",
                "xn--bcher-kva.test",
                443,
            ),
            (
                "https://ppy.sh.example/",
                "https://ppy.sh.example",
                "ppy.sh.example",
                443,
            ),
        ] {
            let endpoint = Endpoint::parse(input).unwrap();
            assert_eq!(endpoint.as_str(), origin);
            assert_eq!(endpoint.domain(), domain);
            assert_eq!(endpoint.port(), port);
            assert_eq!(Endpoint::parse(origin).unwrap(), endpoint);
        }
        assert_eq!(
            Endpoint::default(),
            Endpoint::parse(DEFAULT_ORIGIN).unwrap()
        );
        assert_eq!(Endpoint::default().stable_domain().unwrap(), "illya.test");
        assert!(Endpoint::parse("https://server.example:8443")
            .unwrap()
            .stable_domain()
            .is_err());
    }

    #[test]
    fn official_domains_and_ambiguous_origins_are_rejected() {
        for input in [
            "",
            "http://server.example",
            "https:server.example",
            "https:///server.example",
            "https://ppy.sh",
            "HTTPS://OSU.PPY.SH:443/",
            "https://nested.osu.ppy.sh",
            "https://ppy.sh.",
            "https://user:password@server.example",
            "https://@server.example",
            "https://user%40server.example",
            "https://%70py.sh",
            "https://server.example\\path",
            " https://server.example",
            "https://server.example ",
            "https://server.example\n",
            "https://server.example\u{7f}",
            "https://ｐｐｙ.sh",
            "https://bücher.test",
            "https://127.0.0.1",
            "https://127.1",
            "https://2130706433",
            "https://0x7f000001",
            "https://[::1]",
            "https://localhost",
            "https://-server.example",
            "https://server-.example",
            "https://server..example",
            "https://server_name.example",
            "https://server.example.",
            "https://server.example/path",
            "https://server.example/../",
            "https://server.example/%2e",
            "https://server.example//",
            "https://server.example?query",
            "https://server.example?",
            "https://server.example#fragment",
            "https://server.example#",
            "https://server.example:0",
            "https://server.example:65536",
            "https://server.example:",
            "https://server.example:+443",
            "https://server.example:wrong",
        ] {
            assert!(Endpoint::parse(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn dns_and_input_lengths_are_bounded() {
        let longest_domain = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61)
        );
        assert_eq!(longest_domain.len(), 253);
        assert!(Endpoint::parse(&format!("https://{longest_domain}")).is_ok());
        assert!(Endpoint::parse(&format!("https://{}.test", "a".repeat(64))).is_err());
        assert!(Endpoint::parse(&format!("https://{longest_domain}e")).is_err());
        assert!(Endpoint::parse(&"a".repeat(MAX_ENDPOINT_BYTES + 1)).is_err());
    }

    #[test]
    fn serde_uses_a_validated_canonical_string() {
        let endpoint = Endpoint::parse("HTTPS://Server.Example:443/").unwrap();
        let serialized = serde_json::to_string(&endpoint).unwrap();
        assert_eq!(serialized, "\"https://server.example\"");
        assert_eq!(
            serde_json::from_str::<Endpoint>(&serialized).unwrap(),
            endpoint
        );
        assert_eq!(
            serde_json::from_str::<Endpoint>("\"HTTPS://Server.Example:443/\"").unwrap(),
            endpoint
        );
        assert_eq!(
            Endpoint::try_from("https://server.example").unwrap(),
            endpoint
        );
        assert_eq!(
            Endpoint::try_from(String::from("https://server.example")).unwrap(),
            endpoint
        );
        let custom_port = Endpoint::parse("https://server.example:08443/").unwrap();
        let custom_serialized = serde_json::to_string(&custom_port).unwrap();
        assert_eq!(custom_serialized, "\"https://server.example:8443\"");
        assert_eq!(
            serde_json::from_str::<Endpoint>(&custom_serialized).unwrap(),
            custom_port
        );
        for invalid in [
            "\"https://ppy.sh\"",
            "\"https://server.example:0\"",
            "null",
            "443",
            "{}",
            "[]",
        ] {
            assert!(serde_json::from_str::<Endpoint>(invalid).is_err());
        }
    }
}
